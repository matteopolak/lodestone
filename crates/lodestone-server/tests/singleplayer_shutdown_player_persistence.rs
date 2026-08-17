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

use lodestone_core::{Reader, State, Writer};
use lodestone_model::{GameMode, Vec3};
use lodestone_net::Connection;
use lodestone_server::player_data::PlayerDataStore;
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol};
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
async fn drive_login_and_join(client: &mut Connection<DuplexStream>, username: &str) {
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

    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, SET_TIME_S2C, "join sends the full time sync before any chunk");

    // `view_radius: 0` — exactly one column, batched.
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_START);
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK);
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_FINISHED);
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

/// A deterministic per-username uuid for this file's own stand-in
/// `LoginStart` decode — see that arm's comment for why matching a real
/// offline-mode derivation is not the point: `crate::server`'s `LoginStart`
/// handling takes whatever uuid the decode reports and persists under it
/// verbatim, so this only has to be internally consistent between the
/// decode above and the store lookup below.
fn test_uuid_for(username: &str) -> Uuid {
    Uuid::new_v3(&Uuid::NAMESPACE_OID, username.as_bytes())
}
