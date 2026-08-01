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
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use lodestone_core::State;
use lodestone_model::{BlockActionKind, BlockFace, BlockPos};
use lodestone_net::{Connection, NetError, Transport};

use crate::chunk::{AIR, ChunkSource, STONE, is_air_or_fluid, is_water};
use crate::protocol::{EntitySnapshot, ServerBound, ServerDirective, ServerProtocol};
use crate::vitals::{EYE_HEIGHT, PlayerVitals};

/// Server-initiated keep-alive interval, and the width of the window in
/// which an echo must arrive before the connection is treated as dead.
///
/// Vanilla's `LATENCY_CHECK_INTERVAL` and `CLOSED_LISTENER_TIMEOUT`
/// (`ServerCommonPacketListenerImpl.java:35-36`) are both the literal
/// constant `15000` (milliseconds) — **not** two different numbers.
/// `keepConnectionAlive` (`ServerCommonPacketListenerImpl.java:118-133`)
/// sends a fresh challenge once `now - keepAliveTime >= 15000`, and
/// disconnects immediately if the *previous* challenge is still pending at
/// that point — so an unanswered challenge is caught within one more
/// interval of being sent (up to ~15s later), not two intervals (~30s).
#[cfg(not(target_arch = "wasm32"))]
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(15_000);

/// Cadence of the periodic time-of-day broadcast.
///
/// Vanilla re-broadcasts the world's monotonic game time every 20 ticks
/// (`MinecraftServer::forceGameTimeSynchronization`,
/// `MinecraftServer.java:1095-1099`: `if (this.tickCount % 20 == 0)`) —
/// carrying an *empty* clock-update map, which is what tells a client to keep
/// its held day/night anchor rather than resetting it (see
/// `packets::time::SetTime::day_clock`'s doc comment in the `v770` crate).
/// This crate has no fixed server tick loop (see the module docs), so a
/// 1-second wall-clock interval stands in for "every 20 ticks" at vanilla's
/// normal 20 TPS.
#[cfg(not(target_arch = "wasm32"))]
const TIME_SYNC_INTERVAL: Duration = Duration::from_millis(1_000);

/// Milliseconds per tick at vanilla's normal 20 TPS, used to convert
/// wall-clock elapsed time into the tick-based `game_time`
/// [`ServerProtocol::encode_set_time`] carries, in the absence of a real
/// per-tick server loop.
#[cfg(not(target_arch = "wasm32"))]
const MILLIS_PER_TICK: u128 = 50;

/// Cadence of the air-supply/drowning-damage tick ([`crate::vitals`]).
/// Vanilla ticks `LivingEntity.baseTick`'s water-breath block once per real
/// server tick (20 TPS); this crate has no fixed tick loop (see the module
/// docs), so — exactly like [`TIME_SYNC_INTERVAL`] standing in for "every 20
/// ticks" — a wall-clock interval of [`MILLIS_PER_TICK`] stands in for "every
/// tick". Getting this cadence right matters more here than for time-of-day:
/// the drowning countdown's exact tick counts (300 to empty, +20 to the first
/// hit, then every 20 thereafter — see `crate::vitals`'s module doc comment)
/// are the whole point, not an approximation, so this must fire at the real
/// 20 TPS rate rather than some coarser stand-in.
#[cfg(not(target_arch = "wasm32"))]
const VITALS_TICK_INTERVAL: Duration = Duration::from_millis(50);

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

/// Per-connection view-streaming bookkeeping: which chunk columns has this
/// connection been sent, and around which chunk column.
///
/// Mirrors vanilla's `ChunkMap`/`ChunkTrackingView`
/// (`ChunkMap.java:1110-1132`'s `updateChunkTracking`/`applyChunkTrackingView`,
/// `ChunkTrackingView.java`'s `difference`), simplified to the same square
/// window `serve_connection`'s own initial view already uses
/// (`[-view_radius, view_radius]²`) rather than vanilla's rounded
/// `ChunkTrackingView.Positioned::contains` (a buffered Euclidean-distance
/// test). Keeping the join-time and move-time shapes identical is what stops
/// a live connection from immediately forgetting chunks it only just
/// finished sending at join; matching vanilla's exact circular shape is not
/// otherwise load-bearing for "the world keeps up as the player walks".
#[derive(Debug)]
struct ViewTracker {
    center: (i32, i32),
    loaded: HashSet<(i32, i32)>,
}

impl ViewTracker {
    /// Seeds the tracker with the square already sent for the initial join
    /// view (`center`, `[-view_radius, view_radius]²` around it), so the
    /// first [`recenter`](Self::recenter) diffs against what the client
    /// actually has rather than an empty set.
    fn new(center: (i32, i32), view_radius: i32) -> Self {
        let mut loaded = HashSet::new();
        for dz in -view_radius..=view_radius {
            for dx in -view_radius..=view_radius {
                loaded.insert((center.0 + dx, center.1 + dz));
            }
        }
        Self { center, loaded }
    }

    /// Recomputes the view for a new player chunk position `(cx, cz)`,
    /// returning the directives that bring the client's tracked chunks back
    /// in sync — and returning nothing at all if `(cx, cz)` is still the
    /// tracked center (the same "did the 2D chunk position actually change"
    /// guard `ChunkMap::updateChunkTracking` applies before touching the
    /// view at all).
    ///
    /// Order mirrors vanilla's `applyChunkTrackingView`
    /// (`ChunkMap.java:1122-1132`): the cache-center update is sent first
    /// (unconditionally, since by this point the center *did* change —
    /// vanilla additionally guards this send on the center changing, which
    /// is already implied here), then every column that left the window is
    /// forgotten, then every column that entered it is sent as one chunk
    /// batch.
    fn recenter<P, S>(
        &mut self,
        proto: &P,
        source: &S,
        cx: i32,
        cz: i32,
        view_radius: i32,
    ) -> Vec<ServerDirective>
    where
        P: ServerProtocol,
        S: ChunkSource,
    {
        if (cx, cz) == self.center {
            return Vec::new();
        }

        let mut next = HashSet::new();
        for dz in -view_radius..=view_radius {
            for dx in -view_radius..=view_radius {
                next.insert((cx + dx, cz + dz));
            }
        }

        let mut directives = vec![proto.encode_chunk_cache_center(cx, cz)];

        for &(x, z) in self.loaded.difference(&next) {
            directives.push(proto.encode_forget_chunk(x, z));
        }

        let added: Vec<(i32, i32)> = next.difference(&self.loaded).copied().collect();
        if !added.is_empty() {
            directives.push(proto.begin_chunk_batch());
            for &(x, z) in &added {
                let column = source.column(x, z);
                directives.push(proto.encode_chunk(x, z, &column));
            }
            directives.push(proto.end_chunk_batch(added.len() as i32));
        }

        self.center = (cx, cz);
        self.loaded = next;
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
    /// The client did not echo the server's keep-alive challenge before the
    /// next one was due (a fixed 15-second interval, matching vanilla's
    /// `TIMEOUT_DISCONNECTION_MESSAGE` disconnect path —
    /// `ServerCommonPacketListenerImpl.java:121-129`). Native-only in
    /// practice: nothing constructs this on `wasm32`, since that build never
    /// starts the keep-alive timer in the first place (see
    /// `serve_play`'s doc comment).
    #[error("keep-alive timeout: client did not echo the server's challenge in time")]
    KeepAliveTimeout,
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
/// [`State::Play`] itself is served by [`serve_play`], which adds the parts
/// that have no place before a client has a world to live in: a
/// server-initiated keep-alive (with vanilla's disconnect-on-timeout),
/// periodic time-of-day, and view streaming as the player's chunk column
/// changes. See that function's doc comment for the scheduling.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure,
/// [`ServerError::ClosedBeforeLogin`] if the client hangs up before it ever
/// reaches [`ServerBound::LoginStart`], or whatever [`serve_play`] returns
/// once [`State::Play`] is reached.
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

                // Full clock sync at join, mirroring vanilla's
                // `ServerClockManager::createFullSyncPacket`, sent by
                // `PlayerList.sendLevelInfo` before chunk streaming starts
                // (`PlayerList.java:648-651`): anchor a fresh session's
                // day/night clock at tick 0. This crate has no persisted
                // world age, so "tick 0" is the session's own join moment,
                // not a restored save.
                apply(conn, &mut state, proto.encode_set_time(0, Some(0))).await?;

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
                let chunks_sent = batch_size as usize;

                for directive in proto.welcome_message() {
                    apply(conn, &mut state, directive).await?;
                }

                // Initial entity sync — the same pass the old single-loop
                // version ran on this same iteration via its trailing
                // `if state == State::Play` check, now made explicit because
                // `serve_play` below takes over the loop entirely.
                for directive in streamer.sync(proto, &entities.snapshots()) {
                    apply(conn, &mut state, directive).await?;
                }

                // `ConfigurationFinished` cannot be reached without an
                // earlier `LoginStart` in any correct `ServerProtocol` (the
                // documented ack-driven state machine above), so `username`
                // is always `Some` here; falling back to an empty string
                // rather than panicking keeps a protocol that violates that
                // contract merely wrong, not a crash.
                let username = username.clone().unwrap_or_default();
                let view = ViewTracker::new((0, 0), view_radius);
                return serve_play(
                    conn,
                    proto,
                    source,
                    entities,
                    view_radius,
                    state,
                    streamer,
                    view,
                    username,
                    chunks_sent,
                )
                .await;
            }
            ServerBound::KeepAlive { .. }
            | ServerBound::PlayerMoved { .. }
            | ServerBound::BlockAction { .. }
            | ServerBound::UseItemOn { .. }
            | ServerBound::Ignored => {}
        }
    }

    match username {
        Some(username) => Ok(ServeSummary {
            username,
            chunks_sent: 0,
        }),
        None => Err(ServerError::ClosedBeforeLogin),
    }
}

/// The neighbour cell one step off `pos` in `face`'s direction — vanilla's
/// `BlockPos.relative(Direction)`, used below to find the placement cell when
/// the directly clicked block cannot be replaced.
fn relative(pos: BlockPos, face: BlockFace) -> BlockPos {
    let (dx, dy, dz) = match face {
        BlockFace::Down => (0, -1, 0),
        BlockFace::Up => (0, 1, 0),
        BlockFace::North => (0, 0, -1),
        BlockFace::South => (0, 0, 1),
        BlockFace::West => (-1, 0, 0),
        BlockFace::East => (1, 0, 0),
    };
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

/// Applies one block-breaking phase, mirroring
/// `ServerPlayerGameMode.handleBlockBreakAction`'s three destroy ordinals —
/// simplified per this crate's documented scope (`docs/block-edit.md`): no
/// hardness/timing validation (the client's own predictor already gates when
/// it sends `StopDestroy` — see `lodestone-shell`'s `drive_mining`), and no
/// interaction-range or spawn-protection checks (this crate does not track
/// player position beyond the view-tracking column).
///
/// `pending_break` is this connection's tracked in-progress dig — the
/// version-free analogue of vanilla's `destroyPos` field. It is what makes
/// `StartDestroy` + `StopDestroy` break a block while `StartDestroy` +
/// `AbortDestroy` does not, and what makes a `StopDestroy` for a position
/// nobody started a no-op, mirroring vanilla's own
/// `pos.equals(this.destroyPos)` guard (`ServerPlayerGameMode.java:217`).
async fn apply_block_action<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pending_break: &mut Option<BlockPos>,
    action: BlockActionKind,
    pos: BlockPos,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
{
    match action {
        BlockActionKind::StartDestroy => {
            *pending_break = Some(pos);
        }
        BlockActionKind::AbortDestroy => {
            if *pending_break == Some(pos) {
                *pending_break = None;
            }
        }
        BlockActionKind::StopDestroy => {
            if *pending_break == Some(pos) {
                *pending_break = None;
                source.set_block(pos.x, pos.y, pos.z, AIR);
                let directive = proto.encode_block_update(pos.x, pos.y, pos.z, AIR);
                apply(conn, state, directive).await?;
            }
        }
    }
    Ok(())
}

/// Applies a right-click placement, mirroring
/// `ServerGamePacketListenerImpl.handleUseItemOn`'s replace-vs-relative
/// choice of placement cell (`BlockPlaceContext`'s constructor: place at the
/// clicked block if it `canBeReplaced`, otherwise at its `face`-neighbour) —
/// simplified per this crate's documented scope (`docs/block-edit.md`):
/// always places `minecraft:stone`, since this crate has no inventory model
/// to resolve a real item from, and no survival/collision validation beyond
/// "is the target cell currently replaceable" (air or a fluid — see
/// [`is_air_or_fluid`]).
///
/// Sends [`ServerProtocol::encode_block_update`] for **both** `pos` and its
/// `face`-neighbour unconditionally, matching vanilla's own
/// `handleUseItemOn` (`ServerGamePacketListenerImpl.java:1397-1398`), which
/// sends both regardless of whether the placement succeeded — this doubles
/// as the correction for a client that predicted a placement the server
/// rejected.
async fn apply_use_item_on<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pos: BlockPos,
    face: BlockFace,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
{
    let neighbour = relative(pos, face);
    let clicked = source.block_state(pos.x, pos.y, pos.z);
    let target = if is_air_or_fluid(&clicked) { pos } else { neighbour };
    let target_state = source.block_state(target.x, target.y, target.z);
    if is_air_or_fluid(&target_state) {
        source.set_block(target.x, target.y, target.z, STONE);
    }
    for p in [pos, neighbour] {
        let current = source.block_state(p.x, p.y, p.z);
        let directive = proto.encode_block_update(p.x, p.y, p.z, &current);
        apply(conn, state, directive).await?;
    }
    Ok(())
}

/// Decodes and applies one inbound packet once the connection is in
/// [`State::Play`]: matches a keep-alive echo against the pending challenge
/// (clearing it, so the next keep-alive tick does not mistake a live client
/// for a dead one), streams the view when the player's chunk column changed,
/// tracks the player's latest position for [`PlayerVitals`]' submersion test,
/// or applies a block break/placement (see [`apply_block_action`]/
/// [`apply_use_item_on`]). Every other packet decodes to
/// [`ServerBound::Ignored`] in `State::Play` under the current protocols (no
/// further state transitions are modeled — no respawn/dimension change yet)
/// and is a no-op here.
#[allow(clippy::too_many_arguments)]
async fn dispatch_play_packet<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    view_radius: i32,
    state: &mut State,
    view: &mut ViewTracker,
    pending_keep_alive: &mut Option<i64>,
    pending_break: &mut Option<BlockPos>,
    player_pos: &mut Option<(f64, f64, f64)>,
    packet_id: i32,
    payload: &[u8],
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
{
    match proto.decode(*state, packet_id, payload) {
        ServerBound::KeepAlive { id } => {
            if *pending_keep_alive == Some(id) {
                *pending_keep_alive = None;
            }
        }
        ServerBound::PlayerMoved { x, y, z } => {
            *player_pos = Some((x, y, z));

            // Chunk coordinate = floor(block / 16), not truncating division —
            // `-1.0_f64 / 16.0` must floor to chunk `-1`, matching vanilla's
            // `SectionPos.blockToSectionCoord` (an arithmetic right shift).
            let cx = (x / 16.0).floor() as i32;
            let cz = (z / 16.0).floor() as i32;
            for directive in view.recenter(proto, source, cx, cz, view_radius) {
                apply(conn, state, directive).await?;
            }
        }
        ServerBound::BlockAction {
            action,
            pos,
            face: _,
            sequence: _,
        } => {
            apply_block_action(conn, proto, source, state, pending_break, action, pos).await?;
        }
        ServerBound::UseItemOn {
            pos,
            face,
            sequence: _,
        } => {
            apply_use_item_on(conn, proto, source, state, pos, face).await?;
        }
        ServerBound::Handshake { .. }
        | ServerBound::LoginStart { .. }
        | ServerBound::LoginAcknowledged
        | ServerBound::ConfigurationFinished
        | ServerBound::Ignored => {}
    }
    Ok(())
}

/// Converts wall-clock elapsed time into a tick count at vanilla's normal 20
/// TPS, for the `game_time` the periodic [`ServerProtocol::encode_set_time`]
/// broadcast carries.
#[cfg(not(target_arch = "wasm32"))]
fn ticks_since(start: tokio::time::Instant) -> i64 {
    (start.elapsed().as_millis() / MILLIS_PER_TICK) as i64
}

/// Serves a connection that has just reached [`State::Play`] until the client
/// disconnects.
///
/// This is where [`serve_connection`] hands off once the join sequence and
/// initial chunk view are out: everything here runs on the connection's own
/// schedule rather than strictly in response to one inbound packet —
/// * a server-initiated keep-alive, matching vanilla's fixed 15-second
///   interval and the same-length disconnect timeout
///   (`ServerCommonPacketListenerImpl.java:35-36,118-133`; see the
///   `KEEP_ALIVE_INTERVAL` doc comment for why that is one interval, not two);
/// * a periodic time-of-day broadcast, matching vanilla's every-20-ticks
///   cadence (`MinecraftServer.java:1095-1099`; see `TIME_SYNC_INTERVAL`);
/// * view streaming (chunk-cache-center, forget, and send) whenever a
///   [`ServerBound::PlayerMoved`] packet crosses into a new chunk column
///   (`ChunkMap::move`/`updateChunkTracking`, `ChunkMap.java:1071-1120`);
///
/// all layered over the same entity-streaming pass the join sequence already
/// ran once, now repeated on every inbound packet exactly as the original
/// single-loop version did.
///
/// # Why this is a separate function, and why it forks on `wasm32`
///
/// Only this phase needs a real timer racing against the socket read, via
/// `tokio::select!` — and `tokio::time`'s timer is unavailable on `wasm32`
/// (see [`Connection::read_packet_timeout`](lodestone_net::Connection::read_packet_timeout)'s
/// own doc comment, the existing precedent for this split in this workspace).
/// The `wasm32` build below degrades to the old packet-driven-only loop
/// through the same [`dispatch_play_packet`] helper: it still answers
/// keep-alive echoes and streams the view reactively, it just never
/// *initiates* a keep-alive challenge or a periodic time broadcast, since
/// nothing can wake it when the client goes quiet. This is a real, documented
/// gap on that target, not a silent one.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure, or
/// [`ServerError::KeepAliveTimeout`] if the client does not echo a challenge
/// in time (native only — see above).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
async fn serve_play<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    mut state: State,
    mut streamer: EntityStreamer,
    mut view: ViewTracker,
    username: String,
    chunks_sent: usize,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<BlockPos> = None;
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut vitals = PlayerVitals::default();
    let mut keep_alive_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + KEEP_ALIVE_INTERVAL,
        KEEP_ALIVE_INTERVAL,
    );
    // `interval_at`, not the bare `interval` constructor: `Interval::tick`'s
    // *first* call resolves immediately for an interval built with
    // `tokio::time::interval`, which would otherwise fire a redundant
    // game-time-only broadcast in the same instant as the join-time full
    // sync `serve_connection` just sent. Anchoring the first tick a full
    // `TIME_SYNC_INTERVAL` out avoids that, and mirrors `keep_alive_tick`
    // above for the same reason.
    let mut time_sync_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + TIME_SYNC_INTERVAL,
        TIME_SYNC_INTERVAL,
    );
    // Same reasoning as `time_sync_tick`: anchored one interval out so the
    // first vitals tick does not fire in the same instant as join.
    let mut vitals_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + VITALS_TICK_INTERVAL,
        VITALS_TICK_INTERVAL,
    );
    let play_start = tokio::time::Instant::now();
    let mut next_keep_alive_id: i64 = 0;

    loop {
        tokio::select! {
            packet = conn.read_packet() => {
                let Some((packet_id, payload)) = packet? else {
                    return Ok(ServeSummary { username, chunks_sent });
                };
                dispatch_play_packet(
                    conn,
                    proto,
                    source,
                    view_radius,
                    &mut state,
                    &mut view,
                    &mut pending_keep_alive,
                    &mut pending_break,
                    &mut player_pos,
                    packet_id,
                    &payload,
                )
                .await?;
                for directive in streamer.sync(proto, &entities.snapshots()) {
                    apply(conn, &mut state, directive).await?;
                }
            }

            _ = keep_alive_tick.tick() => {
                if pending_keep_alive.is_some() {
                    return Err(ServerError::KeepAliveTimeout);
                }
                next_keep_alive_id += 1;
                pending_keep_alive = Some(next_keep_alive_id);
                apply(conn, &mut state, proto.encode_keep_alive(next_keep_alive_id)).await?;
            }

            _ = time_sync_tick.tick() => {
                let game_time = ticks_since(play_start);
                apply(conn, &mut state, proto.encode_set_time(game_time, None)).await?;
            }

            _ = vitals_tick.tick() => {
                // No position yet (client has not sent a single move since
                // join): nothing to test submersion against, so skip rather
                // than guess a spawn position this version-free crate does
                // not otherwise track (see `crate::vitals`'s module docs).
                if let Some((x, y, z)) = player_pos {
                    let eye_state = source.block_state(
                        x.floor() as i32,
                        (y + EYE_HEIGHT).floor() as i32,
                        z.floor() as i32,
                    );
                    let outcome = vitals.tick(is_water(&eye_state));
                    if let Some(air) = outcome.air_changed {
                        apply(conn, &mut state, proto.encode_air_supply_update(air)).await?;
                    }
                    if outcome.damage.is_some() {
                        apply(conn, &mut state, proto.encode_set_health(vitals.health())).await?;
                    }
                }
            }
        }
    }
}

/// `wasm32` counterpart of the native [`serve_play`] above — same signature,
/// same [`dispatch_play_packet`] dispatch, but degraded to the old
/// packet-driven-only loop (no `tokio::select!`, no timers). See the native
/// definition's doc comment for why the two forked instead of sharing one
/// body with an internal `cfg`.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
async fn serve_play<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    mut state: State,
    mut streamer: EntityStreamer,
    mut view: ViewTracker,
    username: String,
    chunks_sent: usize,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<BlockPos> = None;
    // Tracked for parity with the native loop's `dispatch_play_packet` calls
    // (shared function, shared signature), but never ticked here: like
    // keep-alive and time-of-day above, `PlayerVitals` needs a timer
    // independent of inbound packets, and `tokio::time` has none on
    // `wasm32`. Drowning simply does not happen in a `wasm32`-served session
    // today — a real, documented gap, not a silent one (see this function's
    // own doc comment).
    let mut player_pos: Option<(f64, f64, f64)> = None;

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        dispatch_play_packet(
            conn,
            proto,
            source,
            view_radius,
            &mut state,
            &mut view,
            &mut pending_keep_alive,
            &mut pending_break,
            &mut player_pos,
            packet_id,
            &payload,
        )
        .await?;
        for directive in streamer.sync(proto, &entities.snapshots()) {
            apply(conn, &mut state, directive).await?;
        }
    }
    Ok(ServeSummary { username, chunks_sent })
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
