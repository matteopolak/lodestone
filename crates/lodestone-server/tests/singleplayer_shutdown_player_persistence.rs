//! **The gate for the singleplayer shutdown-cancellation data-loss bug**: a
//! player who changes game mode and moves, then leaves the world the way a
//! real player does — closing it, never sending a clean disconnect — must
//! rejoin where they left off.
//!
//! # The defect this reproduces
//!
//! `IntegratedServer::shutdown` fires `ShutdownSignal::trigger` and then
//! races the connection task's whole serving future against it in a
//! `tokio::select!` (`crate::integrated`'s connection task). On an ordinary
//! "leave world" the signal wins — nothing in singleplayer's in-process
//! `DuplexStream` ever sends a real socket close, so `crate::server`'s own
//! disconnect-save arm (the `conn.read_packet()` returning `Ok(None)`
//! branch) is structurally unreachable on that path. The serving future,
//! including its own stack-local `player_pos`/`player_rot`/`game_mode`, is
//! simply dropped mid-`.await`.
//!
//! Every existing player-persistence gate in this crate
//! (`entity_persistence_round_trip.rs`'s
//! `player_inventory_and_position_survive_a_disconnect`) drives
//! [`PlayerDataStore`] directly — it never opens a real connection at all,
//! so it cannot see this bug: it would pass identically whether `serve_play`
//! ever called `persist_player` or not. This file is the fixture that is
//! not blind to it: a real login, a real `PLAYER_MOVED` and a real
//! `CHANGE_GAME_MODE` over the wire, then `shutdown()` called on a
//! `current_thread` runtime with nothing else pending — the same
//! discriminating input `tests/shutdown_signal.rs` uses to make "the signal
//! fires before anything else is ready" deterministic rather than a coin
//! flip, applied here to a session that has already made real progress
//! rather than to one that has made none.
//!
//! # Why a `ping_request`/`pong_response` round trip, not a sleep
//!
//! `write_packet` returning only proves the bytes reached the duplex
//! buffer, not that the server's `serve_play` loop has read and dispatched
//! them — packets are processed one `conn.read_packet()` at a time, in
//! order, so a reply to a packet sent *after* the move and the game-mode
//! change is proof both were already dispatched (mutating the same
//! stack-locals this bug drops) by the time this test calls `shutdown()`.
//! A `sleep` would be a guess at how long that takes; the echo is exact.

use std::time::Duration;

use lodestone_core::{Nbt, Reader, State, Writer};
use lodestone_model::{GameMode, Vec3};
use lodestone_net::Connection;
use lodestone_server::player_data::{PlayerData, PlayerDataStore};
use lodestone_server::world_storage::{
    Error as WorldStorageError, NativePlayerData, NativePlayerRecord, PlayerRecordError,
    WorldStorage, WorldStorageBackend,
};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use lodestone_storage::{ExtensionRegistration, RecordKey, RecordWrite};
use lodestone_storage_schema::{
    BuiltinDimension, ExtensionValue, GeneralRecord, PlayerRecord, StorageRecord,
    generated::{general_record, storage_record},
};
use tokio::io::DuplexStream;
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const LOGIN_SUCCESS: i32 = 2;
const FINISH_CONFIGURATION: i32 = 3;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;
const SET_TIME_S2C: i32 = 43;
const PLAYER_MOVED_C2S: i32 = 42;
const CHANGE_GAME_MODE_C2S: i32 = 58;
const PING_REQUEST_C2S: i32 = 91;
const PONG_RESPONSE_S2C: i32 = 92;
const GAME_MODE_S2C: i32 = 93;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// Flat, cheap, deterministic terrain — this gate is about the player-save
/// mirror surviving a cancellation, not about worldgen.
#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 60, z, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage needed; nothing in this gate reads terrain back.
    }
}

/// A minimal stand-in wire format — own vocabulary, not vanilla's, matching
/// `tests/serve_play.rs`'s `FakeProtocol` for the subset this gate needs:
/// login/join, one movement packet, one game-mode change, and a ping/pong
/// round trip used only as an exact "the server has processed everything
/// sent before this" signal.
#[derive(Debug)]
struct FakeProtocol;

impl ServerProtocol for FakeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut r = Reader::new(payload);
                let username = r.string(16).expect("username");
                // `crate::server`'s `LoginStart` arm takes this uuid verbatim
                // (`login_uuid = Some(uuid)`) rather than deriving one itself,
                // so this stand-in supplies a deterministic per-username uuid
                // and the test looks the saved file up under the identical
                // derivation (`test_uuid_for` below) — internally consistent,
                // and not a claim about vanilla's own offline-uuid algorithm.
                let uuid = test_uuid_for(&username);
                ServerBound::LoginStart { username, uuid }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == PLAYER_MOVED_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::PlayerMoved {
                    x: r.f64().expect("x"),
                    y: r.f64().expect("y"),
                    z: r.f64().expect("z"),
                    on_ground: true,
                    rotation: None,
                }
            }
            State::Play if packet_id == CHANGE_GAME_MODE_C2S => {
                let mut r = Reader::new(payload);
                let mode = match r.u8().expect("game mode ordinal") {
                    1 => GameMode::Creative,
                    2 => GameMode::Adventure,
                    3 => GameMode::Spectator,
                    _ => GameMode::Survival,
                };
                ServerBound::ChangeGameMode { mode }
            }
            State::Play if packet_id == PING_REQUEST_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::PingRequest {
                    time: r.i64().expect("ping time"),
                }
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

    fn begin_play_at(
        &self,
        _view_radius: i32,
        _spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        let mut w = Writer::default();
        w.u8(mode as u8);
        vec![ServerDirective::Send {
            packet_id: GAME_MODE_S2C,
            payload: w.as_slice().to_vec(),
        }]
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        let mut w = Writer::default();
        w.i64(game_time);
        match day_time {
            Some(anchor) => {
                w.bool(true);
                w.i64(anchor);
            }
            None => w.bool(false),
        }
        ServerDirective::Send {
            packet_id: SET_TIME_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: w.as_slice().to_vec(),
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

    fn encode_pong_response(&self, time: i64) -> ServerDirective {
        let mut w = Writer::default();
        w.i64(time);
        ServerDirective::Send {
            packet_id: PONG_RESPONSE_S2C,
            payload: w.as_slice().to_vec(),
        }
    }
}

/// Drives handshake → login → configuration → the initial (one-column) join
/// view, leaving the connection parked in `State::Play`. Trimmed to exactly
/// what this gate needs from `tests/serve_play.rs`'s identically-named
/// helper — no chunk-batch accounting beyond draining the one column a
/// `view_radius: 0` join produces.
async fn drive_login_and_join(client: &mut Connection<DuplexStream>, username: &str) -> (i32, i32) {
    drive_login_and_join_with_mode(client, username).await.0
}

/// Drives the normal join and returns both the streamed column and the mode
/// the protocol received at the join boundary.
async fn drive_login_and_join_with_mode(
    client: &mut Connection<DuplexStream>,
    username: &str,
) -> ((i32, i32), GameMode) {
    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut w = Writer::default();
    w.string(username);
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS);
    let mut r = Reader::new(&payload);
    assert_eq!(r.string(16).unwrap(), username);

    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, GAME_MODE_S2C, "join receives its resolved game mode first");
    let mode = match Reader::new(&payload).u8().expect("game mode") {
        0 => GameMode::Survival,
        1 => GameMode::Creative,
        2 => GameMode::Adventure,
        3 => GameMode::Spectator,
        other => panic!("unexpected game mode {other}"),
    };

    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, SET_TIME_S2C, "join sends the full time sync before any chunk");

    // `view_radius: 0` — exactly one column, batched.
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_START);
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK);
    let mut chunk = Reader::new(&payload);
    let cx = chunk.var_i32().expect("chunk x");
    let cz = chunk.var_i32().expect("chunk z");
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_FINISHED);
    ((cx, cz), mode)
}

async fn send_player_moved(client: &mut Connection<DuplexStream>, x: f64, y: f64, z: f64) {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    client
        .write_packet(PLAYER_MOVED_C2S, w.as_slice())
        .await
        .expect("send move");
}

async fn send_game_mode(client: &mut Connection<DuplexStream>, ordinal: u8) {
    let mut w = Writer::default();
    w.u8(ordinal);
    client
        .write_packet(CHANGE_GAME_MODE_C2S, w.as_slice())
        .await
        .expect("send game mode");
}

/// Sends a ping and blocks until the matching pong — the exact "the server
/// has already dispatched everything written before this" signal this
/// file's header explains.
async fn ping_pong(client: &mut Connection<DuplexStream>, time: i64) {
    let mut w = Writer::default();
    w.i64(time);
    client
        .write_packet(PING_REQUEST_C2S, w.as_slice())
        .await
        .expect("send ping");
    loop {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        if id == PONG_RESPONSE_S2C {
            let mut r = Reader::new(&payload);
            assert_eq!(r.i64().expect("echoed time"), time);
            return;
        }
        // Anything else (a stray keep-alive, a resync) is drained and ignored;
        // this gate's view is one column and nothing else should arrive, but
        // being tolerant here keeps the gate about the save, not the drain.
    }
}

/// A unique scratch directory. Not a pid or a random: the scratchpad is
/// shared between concurrently running agents/tests, and a literal nonce is
/// what keeps a collision from reading as a persistence bug.
fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-shutdown-persist-{tag}-9k2f"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp world dir");
    dir
}

/// **The gate.** Join, change game mode, move, then leave the way a real
/// singleplayer session does: `shutdown()` with no clean disconnect ever
/// sent. All three fields — position, rotation is not separately probed here
/// since this stand-in wire format carries none (see `PlayerMoved`'s
/// `rotation: None` above; `tests/player_rotation.rs` covers the real v770
/// decode) — must read back as what was actually sent, not the join-time
/// default.
#[tokio::test]
async fn a_shutdown_cancelled_session_still_saves_position_and_game_mode() {
    let dir = tempdir("gate");
    // 16-char cap (vanilla's own username limit, enforced by this stand-in's
    // `r.string(16)` decode too): an 8-char prefix plus 8 hex digits of a
    // fresh uuid, unique per run.
    let username = format!("Quit{:08x}", Uuid::new_v4().as_u128() as u32);

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, &username).await;

    const MOVED_TO: (f64, f64, f64) = (-101.0, 71.0, 202.0);
    send_player_moved(&mut client, MOVED_TO.0, MOVED_TO.1, MOVED_TO.2).await;
    send_game_mode(&mut client, 1 /* Creative */).await;
    // Proves the server has already dispatched both packets above (see this
    // file's header) before this test does anything else.
    ping_pong(&mut client, 4242).await;

    // **Load-bearing.** Dropping `client` here would close the duplex
    // stream's client half, and `conn.read_packet()` on the server side
    // would return a clean `Ok(None)` — the *disconnect* path this bug does
    // not affect, and the input this whole file exists to avoid. A real
    // "quit to title" click never sends a disconnect either; `forget` is
    // what makes shutdown's own race the only way this task ends, exactly
    // as `tests/shutdown_signal.rs`'s `open` helper documents for the same
    // reason.
    std::mem::forget(client);

    // No yield, no sleep: on this `current_thread` runtime, with the
    // connection task parked (nothing new to read, no timer due) and this
    // task about to await `shutdown()` directly, the signal is the only
    // branch that can become ready on the next poll. That is what makes this
    // deterministic rather than a coin flip — see `tests/shutdown_signal.rs`'s
    // own header for the general argument.
    server.shutdown().await;

    let store = PlayerDataStore::new(&dir).expect("reopen the player store");
    let uuid = test_uuid_for(&username);
    let saved = store
        .read(uuid)
        .expect("read")
        .expect("the shutdown-cancelled session must still have saved a player file");

    // Collected rather than asserted one at a time inside a loop: see
    // `CLAUDE.md`'s evidence standard on why an `assert!` inside a loop
    // hides every failure but the first.
    let mut mismatches = Vec::new();
    if saved.spawn_state().pos != Vec3::new(MOVED_TO.0, MOVED_TO.1, MOVED_TO.2) {
        mismatches.push(format!(
            "position: expected {MOVED_TO:?}, got {:?}",
            saved.spawn_state().pos
        ));
    }
    if saved.game_mode != Some(GameMode::Creative) {
        mismatches.push(format!(
            "game_mode: expected Some(Creative), got {:?}",
            saved.game_mode
        ));
    }
    assert!(
        mismatches.is_empty(),
        "a session cancelled by shutdown's own race must still save the last known state; \
         mismatches: {mismatches:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The control.** With no `PlayerMoved`/`ChangeGameMode` ever sent, the
/// saved file must carry the join-time defaults — proving the assertions
/// above are reading a real save and not a coincidence of the store's own
/// `Default` (survival, and whatever `world_spawn` the world opened with).
#[tokio::test]
async fn a_session_that_never_moved_or_changed_mode_saves_the_join_time_defaults() {
    let dir = tempdir("control");
    let username = format!("Idle{:08x}", Uuid::new_v4().as_u128() as u32);

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, &username).await;
    ping_pong(&mut client, 99).await;
    std::mem::forget(client);
    server.shutdown().await;

    let store = PlayerDataStore::new(&dir).expect("reopen the player store");
    let uuid = test_uuid_for(&username);
    let saved = store
        .read(uuid)
        .expect("read")
        .expect("even an idle shutdown-cancelled session saves a file, at the join defaults");

    assert_eq!(
        saved.game_mode,
        Some(GameMode::Survival),
        "control: an idle session must save the join-time mode, not the gate's Creative"
    );
    assert_ne!(
        saved.spawn_state().pos,
        Vec3::new(-101.0, 71.0, 202.0),
        "control: an idle session must not coincidentally save the gate's own moved-to position"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A native locator is a live consumer, not just an isolated writer: the first
/// join reads its position, shutdown writes the last packet-driven position,
/// and a second server reads that exact record back into the join view. The
/// established Anvil player file is checked at the same boundary to prove the
/// locator did not become a replacement for complete player persistence.
#[tokio::test]
async fn native_locator_survives_join_restart_and_cancelled_shutdown() {
    let dir = tempdir("native-restart");
    let native_dir = dir.join("native");
    let username = format!("Native{:08x}", Uuid::new_v4().as_u128() as u32);
    let uuid = test_uuid_for(&username);
    let uuid_bytes = *uuid.as_bytes();
    let anvil = PlayerDataStore::new(&dir).expect("create complete player store");
    anvil
        .write(
            uuid,
            &PlayerData {
                pos: Vec3::new(7.25, 63.0, -4.5),
                preserved: vec![(
                    "example:opaque".to_owned(),
                    Nbt::String("keep-me".to_owned()),
                )],
                ..PlayerData::default()
            },
        )
        .expect("seed complete player state");
    let initial_locator = NativePlayerRecord {
        uuid: uuid_bytes,
        dimension: BuiltinDimension::Overworld,
        x_fixed: 32_000,
        y_fixed: 71_000,
        z_fixed: 32_000,
        yaw_millidegrees: 90_000,
        pitch_millidegrees: -1_000,
    };
    WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir.clone(),
    })
    .expect("open native seed store")
    .write_dirty_player(initial_locator)
    .expect("seed typed locator");

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
        WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_dir.clone(),
        })
        .expect("open native runtime store"),
    )
    .expect("open native persistent world");
    let mut client = Connection::new(client_end);
    assert_eq!(
        drive_login_and_join(&mut client, &username).await,
        (2, 2),
        "the first join must consume the native locator rather than the Anvil seed"
    );
    const MOVED_TO: (f64, f64, f64) = (-101.125, 71.875, 202.25);
    send_player_moved(&mut client, MOVED_TO.0, MOVED_TO.1, MOVED_TO.2).await;
    ping_pong(&mut client, 711).await;
    std::mem::forget(client);
    server.shutdown().await;

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir.clone(),
    })
    .expect("reopen native locator after shutdown");
    let expected_locator = NativePlayerRecord {
        uuid: uuid_bytes,
        dimension: BuiltinDimension::Overworld,
        x_fixed: -101_125,
        y_fixed: 71_875,
        z_fixed: 202_250,
        yaw_millidegrees: 90_000,
        pitch_millidegrees: -1_000,
    };
    assert_eq!(
        reopened.load_player(uuid_bytes).expect("read native locator"),
        Some(expected_locator),
        "shutdown must commit the latest typed locator, including its explicit fixed-point conversion"
    );
    let saved = anvil
        .read(uuid)
        .expect("read complete player state")
        .expect("complete player file remains present");
    assert_eq!(saved.pos, Vec3::new(MOVED_TO.0, MOVED_TO.1, MOVED_TO.2));
    assert_eq!(
        saved.preserved,
        vec![(
            "example:opaque".to_owned(),
            Nbt::String("keep-me".to_owned()),
        )],
        "complete Anvil state remains the authority for unsupported fields"
    );

    let (restarted, client_end, _world) =
        IntegratedServer::open_persistent_with_mobs_and_storage(
            FakeProtocol,
            &dir,
            FlatWorld,
            MIN_Y,
            HEIGHT,
            (0..=0, 0..=0),
            (8, 8),
            0,
            0,
            Duration::from_secs(3600),
            reopened,
        )
        .expect("reopen native persistent world");
    let mut client = Connection::new(client_end);
    assert_eq!(
        drive_login_and_join(&mut client, &username).await,
        (-7, 12),
        "a restarted server must stream the chunk containing the native locator"
    );
    std::mem::forget(client);
    restarted.shutdown().await;

    let final_locator = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir,
    })
    .expect("reopen native locator after second shutdown");
    assert_eq!(
        final_locator.load_player(uuid_bytes).expect("read final locator"),
        Some(expected_locator)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The native mode is a live join input and is published into the same
/// cancellation-safe snapshot as the locator. A changed mode therefore
/// survives a singleplayer shutdown even though no clean disconnect occurs.
#[tokio::test]
async fn native_game_mode_survives_join_and_cancelled_shutdown() {
    let dir = tempdir("native-game-mode");
    let native_dir = dir.join("native");
    let username = format!("NMode{:08x}", Uuid::new_v4().as_u128() as u32);
    let uuid = test_uuid_for(&username);
    let uuid_bytes = *uuid.as_bytes();
    let seeded = NativePlayerData {
        locator: NativePlayerRecord {
            uuid: uuid_bytes,
            dimension: BuiltinDimension::Overworld,
            x_fixed: 32_000,
            y_fixed: 71_000,
            z_fixed: 32_000,
            yaw_millidegrees: 90_000,
            pitch_millidegrees: -1_000,
        },
        game_mode: Some(GameMode::Adventure),
    };
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir.clone(),
    })
    .expect("open native game-mode store");
    storage
        .write_dirty_player_data(seeded)
        .expect("seed typed native game mode");

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
        storage,
    )
    .expect("open native persistent world");
    server.world_state().set_default_game_mode(GameMode::Spectator);
    let mut client = Connection::new(client_end);
    let ((chunk_x, chunk_z), mode) = drive_login_and_join_with_mode(&mut client, &username).await;
    assert_eq!((chunk_x, chunk_z), (2, 2), "native locator remains the join-position source");
    assert_eq!(mode, GameMode::Adventure, "native mode fills an absent Anvil mode");
    send_game_mode(&mut client, 1 /* Creative */).await;
    ping_pong(&mut client, 714).await;
    std::mem::forget(client);
    server.shutdown().await;

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir,
    })
    .expect("reopen native game-mode store");
    assert_eq!(
        reopened
            .load_player_data(uuid_bytes)
            .expect("read native game mode")
            .map(|data| data.game_mode),
        Some(Some(GameMode::Creative)),
        "the cancelled session must publish its latest native game mode"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record written before `PlayerRecord.game_mode` existed serializes its
/// default zero without tag 8. Decoding that exact value must keep the host's
/// world default rather than inventing a native mode.
#[tokio::test]
async fn native_locator_without_game_mode_keeps_the_world_default() {
    let dir = tempdir("native-game-mode-absent");
    let native_dir = dir.join("native");
    let username = format!("NAbsent{:08x}", Uuid::new_v4().as_u128() as u32);
    let uuid_bytes = *test_uuid_for(&username).as_bytes();
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir,
    })
    .expect("open native backward-compatible store");
    let key = RecordKey::general(
        i32::from_le_bytes(uuid_bytes[..4].try_into().expect("uuid prefix")),
        i32::from_le_bytes(uuid_bytes[4..8].try_into().expect("uuid prefix")),
        u32::from_le_bytes(uuid_bytes[8..12].try_into().expect("uuid prefix")),
    );
    storage
        .write_dirty([RecordWrite::new(
            key,
            StorageRecord {
                format_version: 1,
                record: Some(storage_record::Record::General(GeneralRecord {
                    extensions: Vec::new(),
                    record: Some(general_record::Record::Player(PlayerRecord {
                        player_uuid: uuid_bytes.to_vec(),
                        dimension: BuiltinDimension::Overworld as i32,
                        x_fixed: 0,
                        y_fixed: 61_000,
                        z_fixed: 0,
                        yaw_millidegrees: 0,
                        pitch_millidegrees: 0,
                        game_mode: 0,
                    })),
                })),
            },
        )])
        .expect("write pre-game-mode player record");
    assert_eq!(
        storage
            .load_player_data(uuid_bytes)
            .expect("decode pre-game-mode player record")
            .map(|data| data.game_mode),
        Some(None),
        "the default zero from a record without tag 8 must stay absent"
    );

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
        storage,
    )
    .expect("open backward-compatible native world");
    server.world_state().set_default_game_mode(GameMode::Creative);
    let mut client = Connection::new(client_end);
    let (_, mode) = drive_login_and_join_with_mode(&mut client, &username).await;
    assert_eq!(
        mode,
        GameMode::Creative,
        "an omitted native mode must not override the world"
    );
    std::mem::forget(client);
    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Complete Anvil state is authoritative when it supplies a game mode, even
/// when a native sidecar has a different independently consumable value.
#[tokio::test]
async fn anvil_game_mode_overrides_native_game_mode_on_join() {
    let dir = tempdir("native-game-mode-anvil");
    let native_dir = dir.join("native");
    let username = format!("NAnvil{:08x}", Uuid::new_v4().as_u128() as u32);
    let uuid = test_uuid_for(&username);
    let uuid_bytes = *uuid.as_bytes();
    PlayerDataStore::new(&dir)
        .expect("open Anvil player store")
        .write(
            uuid,
            &PlayerData {
                game_mode: Some(GameMode::Spectator),
                ..PlayerData::default()
            },
        )
        .expect("seed authoritative Anvil mode");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir,
    })
    .expect("open native mode sidecar");
    storage
        .write_dirty_player_data(NativePlayerData {
            locator: NativePlayerRecord {
                uuid: uuid_bytes,
                dimension: BuiltinDimension::Overworld,
                x_fixed: 0,
                y_fixed: 61_000,
                z_fixed: 0,
                yaw_millidegrees: 0,
                pitch_millidegrees: 0,
            },
            game_mode: Some(GameMode::Adventure),
        })
        .expect("seed native mode sidecar");

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
        storage,
    )
    .expect("open native world with authoritative Anvil mode");
    server.world_state().set_default_game_mode(GameMode::Creative);
    let mut client = Connection::new(client_end);
    let (_, mode) = drive_login_and_join_with_mode(&mut client, &username).await;
    assert_eq!(
        mode,
        GameMode::Spectator,
        "Anvil mode must win over native and world defaults"
    );
    std::mem::forget(client);
    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// The missing-record control proves the runtime consumer is the producer of
/// the first locator: an empty native segment starts with `None`, then a
/// cancelled session writes the normal spawn fallback and can reopen it.
#[tokio::test]
async fn missing_native_locator_is_created_from_the_join_fallback() {
    let dir = tempdir("native-missing");
    let native_dir = dir.join("native");
    let username = format!("Missing{:08x}", Uuid::new_v4().as_u128() as u32);
    let uuid = test_uuid_for(&username);
    let uuid_bytes = *uuid.as_bytes();
    let empty = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir.clone(),
    })
    .expect("open empty native store");
    assert_eq!(
        empty.load_player(uuid_bytes).expect("read missing locator"),
        None,
        "control must observe absence before the runtime producer runs"
    );

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
        empty,
    )
    .expect("open native persistent world");
    let mut client = Connection::new(client_end);
    assert_eq!(drive_login_and_join(&mut client, &username).await, (0, 0));
    ping_pong(&mut client, 712).await;
    std::mem::forget(client);
    server.shutdown().await;

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir,
    })
    .expect("reopen native store after missing-record control");
    assert_eq!(
        reopened.load_player(uuid_bytes).expect("read created locator"),
        Some(NativePlayerRecord {
            uuid: uuid_bytes,
            dimension: BuiltinDimension::Overworld,
            x_fixed: 0,
            y_fixed: 61_000,
            z_fixed: 0,
            yaw_millidegrees: 0,
            pitch_millidegrees: 0,
        }),
        "missing native state must be created from the explicit world-spawn fallback"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dimension-aware join restore is intentionally outside this slice. A valid
/// Nether locator must therefore remain durable while an overworld connection
/// uses the safe world-spawn fallback; this is the scope control for the
/// non-overworld branch, not just a comment about it.
#[tokio::test]
async fn non_overworld_native_locator_is_not_overwritten_by_overworld_fallback() {
    let dir = tempdir("native-dimension-gap");
    let native_dir = dir.join("native");
    let username = format!("Nether{:08x}", Uuid::new_v4().as_u128() as u32);
    let uuid = test_uuid_for(&username);
    let uuid_bytes = *uuid.as_bytes();
    let locator = NativePlayerRecord {
        uuid: uuid_bytes,
        dimension: BuiltinDimension::Nether,
        x_fixed: -12_345,
        y_fixed: 64_000,
        z_fixed: 98_765,
        yaw_millidegrees: -179_999,
        pitch_millidegrees: 89_999,
    };
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir.clone(),
    })
    .expect("open native dimension-gap store");
    storage
        .write_dirty_player(locator)
        .expect("seed non-overworld locator");

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
        storage,
    )
    .expect("open native persistent world with non-overworld locator");
    let mut client = Connection::new(client_end);
    assert_eq!(
        drive_login_and_join(&mut client, &username).await,
        (0, 0),
        "unsupported dimension restore must use the overworld fallback"
    );
    ping_pong(&mut client, 714).await;
    std::mem::forget(client);
    server.shutdown().await;

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir,
    })
    .expect("reopen native dimension-gap store");
    assert_eq!(
        reopened.load_player(uuid_bytes).expect("read preserved locator"),
        Some(locator),
        "overworld fallback must not overwrite a non-overworld locator"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The corrupt-record control writes a syntactically valid typed record with
/// an extension this bounded runtime does not consume. Login must fail closed,
/// and cancellation must leave the original unsupported record available for
/// recovery instead of replacing it with a partial locator.
#[tokio::test]
async fn corrupt_native_locator_is_not_overwritten_on_cancelled_shutdown() {
    let dir = tempdir("native-corrupt");
    let native_dir = dir.join("native");
    let username = format!("Corrupt{:08x}", Uuid::new_v4().as_u128() as u32);
    let uuid = test_uuid_for(&username);
    let uuid_bytes = *uuid.as_bytes();
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir.clone(),
    })
    .expect("open native corrupt-record store");
    storage
        .register_native_extensions([ExtensionRegistration::new("example", "opaque", 1)])
        .expect("register extension table");
    let key = RecordKey::general(
        i32::from_le_bytes(uuid_bytes[..4].try_into().expect("uuid prefix")),
        i32::from_le_bytes(uuid_bytes[4..8].try_into().expect("uuid prefix")),
        u32::from_le_bytes(uuid_bytes[8..12].try_into().expect("uuid prefix")),
    );
    let record = StorageRecord {
        format_version: 1,
        record: Some(storage_record::Record::General(GeneralRecord {
            extensions: vec![ExtensionValue {
                local_id: 1,
                payload: vec![0xde, 0xad],
            }],
            record: Some(general_record::Record::Player(PlayerRecord {
                player_uuid: uuid_bytes.to_vec(),
                dimension: BuiltinDimension::Overworld as i32,
                x_fixed: 12_000,
                y_fixed: 65_000,
                z_fixed: -4_000,
                yaw_millidegrees: 4_000,
                pitch_millidegrees: -2_000,
                game_mode: 0,
            })),
        })),
    };
    storage
        .write_dirty([RecordWrite::new(key, record)])
        .expect("write corrupt native record");

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
        FakeProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
        Duration::from_secs(3600),
        storage,
    )
    .expect("open native persistent world with corrupt locator");
    let mut client = Connection::new(client_end);
    assert_eq!(
        drive_login_and_join(&mut client, &username).await,
        (0, 0),
        "a corrupt native locator must fall back to world spawn"
    );
    ping_pong(&mut client, 713).await;
    std::mem::forget(client);
    server.shutdown().await;

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_dir,
    })
    .expect("reopen corrupt native store");
    assert!(matches!(
        reopened.load_player(uuid_bytes),
        Err(WorldStorageError::Player(PlayerRecordError::UnsupportedExtensions))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A deterministic per-username uuid for this file's own stand-in
/// `LoginStart` decode — see that arm's comment for why matching a real
/// offline-mode derivation is not the point: `crate::server`'s `LoginStart`
/// handling takes whatever uuid the decode reports and persists under it
/// verbatim, so this only has to be internally consistent between the
/// decode above and the store lookup below.
fn test_uuid_for(username: &str) -> Uuid {
    Uuid::new_v3(&Uuid::NAMESPACE_OID, username.as_bytes())
}
