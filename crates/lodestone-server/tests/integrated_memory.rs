//! Hermetic proof that the integrated server serves worldgen chunks to a client
//! over the **same** in-memory [`Connection`]/[`Transport`] path used for TCP.
//!
//! A `memory_pair()` duplex connects a client end to a server end. The server
//! runs [`serve_connection`] with a [`WorldgenChunkSource`] (backed by the
//! density-function router) and a small stand-in [`ServerProtocol`]. The client
//! drives handshake + login start + the login/configuration acknowledgements
//! through [`Connection::write_packet`], then reads packets back through
//! [`Connection::read_packet`] — the real framing and codec — and verifies
//! every received chunk's solidity matches the terrain the source generates.
//!
//! The stand-in protocol here uses a trivial wire format, not vanilla 26.2's:
//! the version-correct client-bound encoders (`join_game`, registry data,
//! `level_chunk_with_light`) live in the version crate and are a reported seam
//! (the client stack is decode-only today). What this test *does* prove is the
//! full server plumbing: transport → codec → login/configuration/play state
//! machine → worldgen → client decode, with zero framing errors, over the
//! shared seam, including the ack-driven state transitions and single
//! flow-controlled chunk batch (`CHUNK_BATCH_START`/`CHUNK_BATCH_FINISHED`)
//! real `ServerProtocol` implementors must also drive.

use lodestone_core::{Reader, State, Writer};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, EntitySnapshot, EntitySource, IntegratedServer,
    MobHandle, NoEntities, ServerBound, ServerDirective, ServerProtocol, WorldgenChunkSource,
    serve_connection,
};
use lodestone_worldgen::density::{Builder, Density, NoiseParams, Resolver};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const LOGIN_SUCCESS: i32 = 2;
const FINISH_CONFIGURATION: i32 = 3;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;
const ADD_ENTITY: i32 = 20;
const ENTITY_UPDATE: i32 = 21;
const REMOVE_ENTITIES: i32 = 22;
/// A packet the client sends during Play (a keep-alive stand-in). `FakeProtocol`
/// decodes it to `Ignored`, which is enough to drive an entity streaming pass.
const CLIENT_TICK: i32 = 99;

/// A stand-in protocol with a trivial, self-describing wire format.
struct FakeProtocol;

impl FakeProtocol {
    fn encode_column(cx: i32, cz: i32, col: &ChunkColumn) -> Vec<u8> {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        w.var_i32(col.min_y);
        w.var_i32(col.height);
        for y in col.min_y..col.min_y + col.height {
            for z in 0..16 {
                for x in 0..16 {
                    w.u8(u8::from(col.is_solid(x, y, z)));
                }
            }
        }
        w.as_slice().to_vec()
    }
}

impl ServerProtocol for FakeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut r = Reader::new(payload);
                let username = r.string(16).expect("username");
                ServerBound::LoginStart {
                    username,
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        let mut w = Writer::default();
        w.string(username);
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: w.as_slice().to_vec(),
        }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: Self::encode_column(cx, cz, column),
        }
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(batch_size);
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_FINISHED,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(entity.id);
        w.string(&entity.entity_type.to_string());
        w.f64(entity.position.x);
        w.f64(entity.position.y);
        w.f64(entity.position.z);
        ServerDirective::Send {
            packet_id: ADD_ENTITY,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_entity_update(
        &self,
        _prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        let mut w = Writer::default();
        w.var_i32(current.id);
        w.f64(current.position.x);
        w.f64(current.position.y);
        w.f64(current.position.z);
        vec![ServerDirective::Send {
            packet_id: ENTITY_UPDATE,
            payload: w.as_slice().to_vec(),
        }]
    }

    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(ids.len() as i32);
        for id in ids {
            w.var_i32(*id);
        }
        ServerDirective::Send {
            packet_id: REMOVE_ENTITIES,
            payload: w.as_slice().to_vec(),
        }
    }
}

/// Filesystem resolver over the worldgen crate's checked-in vanilla JSON.
struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).expect("read worldgen json");
        serde_json::from_str(&text).expect("parse worldgen json")
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().unwrap() as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_f64().unwrap())
                .collect(),
        }
    }
}

fn overworld_final_density(seed: i64, root: &Path) -> Density {
    let resolver = FsResolver {
        root: root.to_path_buf(),
    };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    // Build first, then drop the borrow before returning the owned Density.
    let builder = Builder::new(seed, &resolver);
    builder.build(&settings["noise_router"]["final_density"])
}

#[tokio::test]
async fn integrated_server_streams_worldgen_chunks_over_memory_transport() {
    // The worldgen crate owns the staged vanilla data (plan §3 puts it in the
    // version crate; it is staged as fixtures this session).
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-worldgen/tests/support/worldgen_data");
    let seed = 42_i64;
    let view_radius = 0; // single chunk keeps this hermetic test fast

    let final_density = overworld_final_density(seed, &root);
    // The router math is per-point, so a shorter column still exercises the full
    // density tree; we cap the height to keep debug-mode sampling quick.
    let sample_min_y = -64;
    let sample_height = 96;
    let source = WorldgenChunkSource::new(final_density.clone(), sample_min_y, sample_height);

    // An independent reference source the client checks received chunks against.
    let reference = WorldgenChunkSource::new(final_density, sample_min_y, sample_height);

    let (client_end, server_end) = memory_pair();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .expect("serve")
    });

    let mut client = Connection::new(client_end);

    // Handshake: one byte selecting the Login next-state.
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    // Login start: the username.
    let mut w = Writer::default();
    w.string("SinglePlayer");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");

    // First play packet is login success carrying the echoed username.
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS);
    let mut r = Reader::new(&payload);
    assert_eq!(r.string(16).unwrap(), "SinglePlayer");

    // Acknowledge login, then finish configuration — mirroring vanilla's
    // ack-driven handshake: neither transition happens until the client says
    // so, exactly as the real `V770Adapter` drives it from the other side.
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    // The join sequence opens with a chunk batch marker.
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_START);
    assert!(payload.is_empty());

    // Then 9 chunk packets, each verified block-for-block against the reference.
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let mut received = 0;
    let mut total_solid = 0usize;
    for _ in 0..expected_chunks {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        assert_eq!(id, CHUNK);
        let mut r = Reader::new(&payload);
        let cx = r.var_i32().unwrap();
        let cz = r.var_i32().unwrap();
        let min_y = r.var_i32().unwrap();
        let height = r.var_i32().unwrap();
        assert_eq!((min_y, height), (sample_min_y, sample_height));

        let expected = reference.column(cx, cz);
        for y in min_y..min_y + height {
            for z in 0..16 {
                for x in 0..16 {
                    let solid = r.u8().unwrap() == 1;
                    assert_eq!(
                        solid,
                        expected.is_solid(x, y, z),
                        "mismatch at chunk ({cx},{cz}) block ({x},{y},{z})"
                    );
                    if solid {
                        total_solid += 1;
                    }
                }
            }
        }
        assert_eq!(r.remaining(), 0, "trailing bytes in chunk ({cx},{cz})");
        received += 1;
    }

    // The batch closes with a finished marker carrying the batch size.
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_FINISHED);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().unwrap(), expected_chunks as i32);

    // Close the client end so the server's read loop sees EOF and returns the
    // summary — a real connection stays open past the join sequence, so the
    // loop no longer returns as soon as the view is delivered.
    drop(client);

    let summary = server.await.expect("join");
    assert_eq!(summary.chunks_sent, expected_chunks);
    assert_eq!(received, expected_chunks);
    assert_eq!(summary.username, "SinglePlayer");
    // The seeded overworld router must actually produce terrain, not empty air.
    assert!(total_solid > 0, "worldgen produced no solid blocks");
    println!("served {received} chunks over in-memory transport; {total_solid} solid blocks total");
}

/// The entity-encoder trait methods land as no-op defaults so the trait shell
/// compiles and existing `ServerProtocol` implementors are unaffected until a
/// version crate fills them in. This pins that contract: a protocol that does
/// **not** override them emits nothing — no bogus packet, no panic — for spawn,
/// update (with and without a previous snapshot), and batched removal.
#[test]
fn entity_encoder_defaults_are_harmless_noops() {
    use lodestone_model::{Rotation, Vec3};

    // A protocol that overrides only the required methods, leaving the entity
    // encoders at their trait defaults. (`FakeProtocol` overrides them, so it
    // can't stand in for "a protocol that hasn't implemented entities yet".)
    struct DefaultsProtocol;
    impl ServerProtocol for DefaultsProtocol {
        fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
            unimplemented!()
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
    }

    let proto = DefaultsProtocol;
    let snap = EntitySnapshot {
        id: 7,
        uuid: Uuid::nil(),
        entity_type: "minecraft:zombie".parse().unwrap(),
        position: Vec3::new(1.0, 2.0, 3.0),
        rotation: Rotation::new(90.0, 0.0),
        head_yaw: 45.0,
        velocity: Vec3::new(0.1, 0.0, 0.0),
        metadata: Vec::new(),
    };

    assert_eq!(proto.encode_add_entity(&snap), ServerDirective::None);
    assert!(proto.encode_entity_update(None, &snap).is_empty());
    assert!(proto.encode_entity_update(Some(&snap), &snap).is_empty());
    assert_eq!(
        proto.encode_remove_entity(&[7, 8, 9]),
        ServerDirective::None
    );
}

/// A shared, mutable view of the world's entities. The test holds one clone and
/// mutates it between client packets; the server task holds another and reads it
/// each streaming pass — mirroring the real seam where the simulation loop owns
/// the entities and `serve_connection` only reads a snapshot of them.
#[derive(Clone, Default)]
struct SharedEntities(std::sync::Arc<std::sync::Mutex<Vec<EntitySnapshot>>>);

impl SharedEntities {
    fn set(&self, entities: Vec<EntitySnapshot>) {
        *self.0.lock().unwrap() = entities;
    }
}

impl EntitySource for SharedEntities {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.0.lock().unwrap().clone()
    }
}

/// End-to-end proof that `serve_connection` actually *streams* entities: an
/// entity present at join reaches the client as a real `ADD_ENTITY` packet over
/// the memory transport, a mutation to the shared world produces an
/// `ENTITY_UPDATE`, and clearing it produces a batched `REMOVE_ENTITIES` — all
/// decoded back through the same framing/codec a live client uses, and asserted
/// against known values (id, type string, position). This exercises the whole
/// diff/spawn/despawn loop the integrated server owns, not just the pure core.
#[tokio::test]
async fn integrated_server_streams_entity_lifecycle_over_memory_transport() {
    use lodestone_model::{Rotation, Vec3};

    let view_radius = 0;

    // The world starts with one pig; a `WorldgenChunkSource` is overkill here, so
    // reuse an all-air flat column source via a trivial ChunkSource.
    struct FlatAir;
    impl ChunkSource for FlatAir {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // The column-regenerating form (correct, just not cheap); this
            // fixture is tiny and this path is not hot.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // No storage: this fixture serves fresh columns and edits are
        // discarded by design. Explicit rather than inherited — issue #440.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design.
        }
    }

    let entities = SharedEntities::default();
    let pig = |x: f64| EntitySnapshot {
        id: 42,
        uuid: Uuid::from_u128(0x1234),
        entity_type: "minecraft:pig".parse().unwrap(),
        position: Vec3::new(x, 64.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        head_yaw: 0.0,
        velocity: Vec3::new(0.0, 0.0, 0.0),
        metadata: Vec::new(),
    };
    entities.set(vec![pig(0.0)]);

    let (client_end, server_end) = memory_pair();
    let server_entities = entities.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &FlatAir,
            &server_entities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .expect("serve")
    });

    let mut client = Connection::new(client_end);

    // Drive the join sequence (identical to the chunk test).
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("SinglePlayer");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    let (id, _) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS);
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    // The join view: batch start, one chunk, batch finished.
    let (id, _) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_START);
    let (id, _) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK);
    let (id, _) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_FINISHED);

    // Immediately after the view, the join-time entities are streamed: our pig
    // spawns exactly once, with its real type and position.
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, ADD_ENTITY, "pig present at join must spawn");
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().unwrap(), 42);
    assert_eq!(r.string(64).unwrap(), "minecraft:pig");
    assert_eq!(r.f64().unwrap(), 0.0);
    assert_eq!(r.f64().unwrap(), 64.0);
    assert_eq!(r.f64().unwrap(), 0.0);
    assert_eq!(r.remaining(), 0, "trailing bytes after ADD_ENTITY");

    // Move the pig, then poke the server with a client-tick packet. The next
    // streaming pass sees the changed snapshot and emits a single update.
    entities.set(vec![pig(5.0)]);
    client
        .write_packet(CLIENT_TICK, &[])
        .await
        .expect("client tick");
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, ENTITY_UPDATE, "moved pig must update");
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().unwrap(), 42);
    assert_eq!(r.f64().unwrap(), 5.0);
    let _ = r.f64().unwrap();
    let _ = r.f64().unwrap();
    assert_eq!(r.remaining(), 0, "trailing bytes after ENTITY_UPDATE");

    // An unchanged world must be silent: poke again, then remove the pig and poke
    // once more. The client should read exactly the REMOVE next — proving the
    // no-change pass emitted nothing between the update and the removal.
    client
        .write_packet(CLIENT_TICK, &[])
        .await
        .expect("client tick");
    entities.set(vec![]);
    client
        .write_packet(CLIENT_TICK, &[])
        .await
        .expect("client tick");
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(
        id, REMOVE_ENTITIES,
        "an unchanged pass is silent, so the next packet is the removal"
    );
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().unwrap(), 1, "one id removed");
    assert_eq!(r.var_i32().unwrap(), 42);
    assert_eq!(r.remaining(), 0, "trailing bytes after REMOVE_ENTITIES");

    drop(client);
    let summary = server.await.expect("join");
    assert_eq!(summary.username, "SinglePlayer");
    println!("streamed pig lifecycle: spawn -> update -> remove over memory transport");
}

/// End-to-end proof that issue #284/#285's unified tick clock is not an
/// island: `IntegratedServer::open_in_memory_with_mobs` — the exact
/// constructor `lodestone-shell`'s `net.rs` calls to start singleplayer — is
/// driven through its *public* API only (no `#[path]` shortcut into
/// `tick.rs`'s own internals), and its `tick_stats()` accessor must report
/// real, advancing counts.
///
/// `#[tokio::test(start_paused = true)]` makes this deterministic rather than
/// wall-clock-dependent: `tokio::time::advance` drives the *same* virtual
/// clock the spawned tick task's own `sleep_until` calls read, so 5 tick
/// periods (250ms of virtual time) must produce exactly 5 ticks — not 4 (an
/// off-by-one), not 6 (a burst), and `overrun_count` must stay 0 since
/// nothing here ever falls behind. Predicted and measured are compared
/// below, not just "it changed."
#[tokio::test(start_paused = true)]
async fn open_in_memory_with_mobs_advances_the_unified_clock_and_reports_stats() {
    struct EmptyWorld;
    impl ChunkSource for EmptyWorld {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // The column-regenerating form (correct, just not cheap); this
            // fixture is tiny and this path is not hot.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // No storage: this fixture serves fresh columns and edits are
        // discarded by design. Explicit rather than inherited — issue #440.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design.
        }
    }

    let (server, _client_io) = IntegratedServer::open_in_memory_with_mobs(
        FakeProtocol,
        EmptyWorld,
        (0..=0, 0..=0),
        (0, 0),
        0, // mob_count: irrelevant to this test, which only checks the clock
        0, // view_radius
    );

    // Every other constructor reports `None`; this is the one that starts a
    // clock at all (`docs/server-tick-loop.md`).
    let before = server
        .tick_stats()
        .expect("open_in_memory_with_mobs must start a TickClock");
    assert_eq!(before.tick_count, 0, "no tick period has elapsed yet");

    // Let the freshly spawned tick task reach its first `Instant::now()` call
    // before advancing — see `tick.rs`'s own test module for why this is
    // required (a spawned task is never polled synchronously), not
    // defensive.
    tokio::task::yield_now().await;

    const TICK_PERIOD: std::time::Duration = std::time::Duration::from_millis(50);
    for _ in 0..5 {
        tokio::time::advance(TICK_PERIOD).await;
    }
    tokio::task::yield_now().await;

    let after = server.tick_stats().expect("clock persists across ticks");
    assert_eq!(
        after.tick_count, 5,
        "5 real tick periods must advance the public tick_stats() count by exactly 5"
    );
    assert_eq!(
        after.overrun_count, 0,
        "a healthy run observed through the public API must not record an overrun"
    );
    assert!(
        after.tps > 0.0 && after.tps <= 20.0,
        "tps must be a real, bounded figure, got {}",
        after.tps
    );

    server.shutdown().await;
}
