//! The generic integrated-server driver.
//!
//! [`serve_connection`] runs the server side of a single client connection over
//! any [`Transport`]: it reads packets through the shared
//! [`Connection`](lodestone_net::Connection) codec, lifts them with a
//! [`ServerProtocol`], plays the login sequence, and streams the initial view's
//! chunks from a [`ChunkSource`]. The identical loop serves an in-memory
//! [`memory_pair`](lodestone_net::memory_pair) client (singleplayer) or a
//! `TcpStream` client (open-to-LAN).

use std::collections::{HashMap, HashSet};

use lodestone_core::State;
use lodestone_net::{Connection, NetError, Transport};

use crate::chunk::ChunkSource;
use crate::protocol::{EntitySnapshot, ServerBound, ServerDirective, ServerProtocol};

/// A read-only view of the entities in the world right now, supplied by the
/// caller that owns the simulation and its tick.
///
/// [`serve_connection`] reads snapshots each streaming pass and diffs them
/// against what *this* connection was last sent; it never ticks the simulation
/// itself, so one shared world can feed many connections without double-ticking.
pub trait EntitySource: Send + Sync {
    /// The entities that should currently be visible to the client.
    fn snapshots(&self) -> Vec<EntitySnapshot>;
}

/// An [`EntitySource`] carrying no entities — for callers that only stream
/// terrain (the existing chunk-only behaviour).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEntities;

impl EntitySource for NoEntities {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        Vec::new()
    }
}

/// Per-connection bookkeeping that turns "the entities right now" into the
/// spawn / update / remove directives this client still needs, given what it was
/// already sent.
///
/// This is the diff the integrated server owns (plan: the server drives the
/// lifecycle; the [`ServerProtocol`] only encodes individual packets). It holds
/// the last snapshot sent per entity id so it can decide, each pass, which ids
/// are new (spawn), which changed (update, handing the protocol the previous
/// snapshot so it may choose a relative encoding), and which vanished (remove,
/// batched into one packet as `REMOVE_ENTITIES` is on the wire).
#[derive(Debug, Default)]
struct EntityStreamer {
    last_sent: HashMap<i32, EntitySnapshot>,
}

impl EntityStreamer {
    /// Produces the directives that bring the client from its last-sent state to
    /// `current`, updating the bookkeeping to match.
    fn sync<P: ServerProtocol>(
        &mut self,
        proto: &P,
        current: &[EntitySnapshot],
    ) -> Vec<ServerDirective> {
        let mut directives = Vec::new();

        // Removals first, batched: any id we sent that is no longer present.
        let live: HashSet<i32> = current.iter().map(|e| e.id).collect();
        let removed: Vec<i32> = self
            .last_sent
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        if !removed.is_empty() {
            for id in &removed {
                self.last_sent.remove(id);
            }
            directives.push(proto.encode_remove_entity(&removed));
        }

        // Spawns and updates, in the source's iteration order.
        for entity in current {
            match self.last_sent.get(&entity.id) {
                None => {
                    directives.push(proto.encode_add_entity(entity));
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(prev) if prev != entity => {
                    directives.extend(proto.encode_entity_update(Some(prev), entity));
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(_) => {}
            }
        }

        directives
    }
}

/// Outcome of serving a connection's initial view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeSummary {
    /// The username the client logged in as.
    pub username: String,
    /// Number of chunk columns sent for the initial view.
    pub chunks_sent: usize,
}

/// Errors from the integrated-server driver.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The underlying transport/codec failed.
    #[error("network error: {0}")]
    Net(#[from] NetError),
    /// The client disconnected before completing login.
    #[error("client closed before login completed")]
    ClosedBeforeLogin,
}

async fn apply<T: Transport>(
    conn: &mut Connection<T>,
    state: &mut State,
    directive: ServerDirective,
) -> Result<(), ServerError> {
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload).await?;
        }
        ServerDirective::SetState(next) => *state = next,
        ServerDirective::SetCompression(threshold) => conn.set_compression(threshold),
        ServerDirective::None => {}
    }
    Ok(())
}

/// Serves one client connection through login, configuration, the play join
/// sequence, and the initial chunk view — then keeps serving until the client
/// disconnects.
///
/// The loop transitions Handshaking → Login → Configuration → Play driven
/// entirely by the [`ServerProtocol`], acknowledgement by acknowledgement,
/// exactly mirroring the client-side `VersionAdapter`'s choreography:
///
/// 1. [`ServerBound::LoginStart`] → [`ServerProtocol::login_success`] (no
///    state change yet).
/// 2. [`ServerBound::LoginAcknowledged`] → state becomes
///    [`State::Configuration`], then [`ServerProtocol::begin_configuration`].
/// 3. [`ServerBound::ConfigurationFinished`] → state becomes [`State::Play`],
///    then [`ServerProtocol::begin_play`], then every column in
///    `[-view_radius, view_radius]²` (chunk coordinates) from `source` as a
///    single flow-controlled chunk batch
///    ([`ServerProtocol::begin_chunk_batch`]/
///    [`ServerProtocol::encode_chunk`]/[`ServerProtocol::end_chunk_batch`]),
///    then [`ServerProtocol::welcome_message`] (optional; empty by default).
///
/// Unlike the initial version of this loop, it does not return once the view
/// has been delivered: a real client stays connected past the join sequence
/// (keep-alives, movement, chunk-batch acknowledgements), so the loop keeps
/// reading and lifting packets — dispatching to [`ServerBound::Ignored`] for
/// anything not yet acted on — until the client closes the connection. The
/// summary is only available once that happens.
///
/// Once the connection reaches [`State::Play`], every inbound packet also drives
/// an entity streaming pass: the [`EntityStreamer`] diffs `entities.snapshots()`
/// against what this connection was last sent and emits the necessary spawn /
/// update / remove directives. The client's own traffic (keep-alives, movement)
/// provides the cadence for this MVP; a fixed server-side tick is a later
/// refinement that only changes *when* `sync` is called, not the diff it
/// computes. Pass [`NoEntities`] to keep the chunk-only behaviour.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure, or
/// [`ServerError::ClosedBeforeLogin`] if the client hangs up before it ever
/// reaches [`ServerBound::LoginStart`].
pub async fn serve_connection<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
    E: EntitySource,
{
    let mut state = State::Handshaking;
    let mut username: Option<String> = None;
    let mut chunks_sent = 0usize;
    let mut streamer = EntityStreamer::default();

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        match proto.decode(state, packet_id, &payload) {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            ServerBound::LoginStart {
                username: name,
                uuid,
            } => {
                username = Some(name.clone());
                for directive in proto.login_success(&name, uuid) {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::LoginAcknowledged => {
                state = State::Configuration;
                for directive in proto.begin_configuration() {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::ConfigurationFinished => {
                state = State::Play;
                for directive in proto.begin_play(view_radius) {
                    apply(conn, &mut state, directive).await?;
                }

                apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                let mut batch_size = 0;
                for cz in -view_radius..=view_radius {
                    for cx in -view_radius..=view_radius {
                        let column = source.column(cx, cz);
                        apply(conn, &mut state, proto.encode_chunk(cx, cz, &column)).await?;
                        batch_size += 1;
                    }
                }
                apply(conn, &mut state, proto.end_chunk_batch(batch_size)).await?;
                chunks_sent = batch_size as usize;

                for directive in proto.welcome_message() {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::Ignored => {}
        }

        // Stream entity changes to the client once it is in Play. The client's
        // inbound traffic drives the cadence for this MVP; entity directives are
        // only Send/None, so this never changes `state`.
        if state == State::Play {
            for directive in streamer.sync(proto, &entities.snapshots()) {
                apply(conn, &mut state, directive).await?;
            }
        }
    }

    match username {
        Some(username) => Ok(ServeSummary {
            username,
            chunks_sent,
        }),
        None => Err(ServerError::ClosedBeforeLogin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkColumn;
    use lodestone_model::{Rotation, Vec3};
    use uuid::Uuid;

    /// A protocol double whose entity encoders tag each directive with a
    /// distinct packet id and the entity id(s) involved, so a test can read the
    /// streamer's diff *decisions* straight off the returned directives. It does
    /// not implement the chunk/login half — the streamer never calls those.
    struct TagProto;

    const ADD: i32 = 1;
    const UPDATE: i32 = 2;
    const REMOVE: i32 = 3;

    impl ServerProtocol for TagProto {
        fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
            unimplemented!("streamer never decodes")
        }
        fn login_success(&self, _u: &str, _uuid: Uuid) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_configuration(&self) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_play(&self, _r: i32) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_chunk_batch(&self) -> ServerDirective {
            unimplemented!()
        }
        fn encode_chunk(&self, _cx: i32, _cz: i32, _c: &ChunkColumn) -> ServerDirective {
            unimplemented!()
        }
        fn end_chunk_batch(&self, _n: i32) -> ServerDirective {
            unimplemented!()
        }

        fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
            ServerDirective::Send {
                packet_id: ADD,
                payload: vec![entity.id as u8],
            }
        }
        fn encode_entity_update(
            &self,
            _prev: Option<&EntitySnapshot>,
            current: &EntitySnapshot,
        ) -> Vec<ServerDirective> {
            vec![ServerDirective::Send {
                packet_id: UPDATE,
                payload: vec![current.id as u8],
            }]
        }
        fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
            ServerDirective::Send {
                packet_id: REMOVE,
                payload: ids.iter().map(|id| *id as u8).collect(),
            }
        }
    }

    fn snap(id: i32, x: f64) -> EntitySnapshot {
        EntitySnapshot {
            id,
            uuid: Uuid::nil(),
            entity_type: "minecraft:zombie".parse().unwrap(),
            position: Vec3::new(x, 0.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            head_yaw: 0.0,
            velocity: Vec3::new(0.0, 0.0, 0.0),
        }
    }

    /// Extracts `(packet_id, payload)` from a `Send` directive for assertions.
    fn sent(d: &ServerDirective) -> (i32, &[u8]) {
        match d {
            ServerDirective::Send { packet_id, payload } => (*packet_id, payload.as_slice()),
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn first_sync_spawns_every_entity_in_source_order() {
        let mut s = EntityStreamer::default();
        let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
        assert_eq!(out.len(), 2);
        assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
        assert_eq!(sent(&out[1]), (ADD, [20u8].as_slice()));
    }

    #[test]
    fn resync_with_no_change_emits_nothing() {
        let mut s = EntityStreamer::default();
        let world = [snap(10, 0.0), snap(20, 0.0)];
        let _ = s.sync(&TagProto, &world);
        let out = s.sync(&TagProto, &world);
        assert!(out.is_empty(), "unchanged world must not re-send: {out:?}");
    }

    #[test]
    fn moved_entity_emits_a_single_update() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap(10, 0.0)]);
        let out = s.sync(&TagProto, &[snap(10, 5.0)]);
        assert_eq!(out.len(), 1);
        assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    }

    #[test]
    fn vanished_entity_is_removed_and_removals_batch() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0), snap(30, 0.0)]);
        let out = s.sync(&TagProto, &[snap(10, 0.0)]);
        // Both 20 and 30 gone -> one batched REMOVE carrying both ids.
        assert_eq!(out.len(), 1);
        let (id, payload) = sent(&out[0]);
        assert_eq!(id, REMOVE);
        let mut ids: Vec<u8> = payload.to_vec();
        ids.sort_unstable();
        assert_eq!(ids, vec![20, 30]);
    }

    #[test]
    fn readding_a_removed_id_spawns_it_again() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
        let _ = s.sync(&TagProto, &[snap(10, 0.0)]); // 20 removed
        let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]); // 20 back
        assert_eq!(out.len(), 1);
        assert_eq!(sent(&out[0]), (ADD, [20u8].as_slice()));
    }
}
