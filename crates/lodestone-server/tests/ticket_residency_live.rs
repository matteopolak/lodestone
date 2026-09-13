//! **The ticket-residency gate**: a real join grants a chunk-residency
//! ticket, a real move carries it, and a real disconnect withdraws it — all
//! observed through [`IntegratedServer::tickets`], the same
//! [`lodestone_server::TicketStoreHandle`] the connection itself grants
//! against, never by calling ticket-graph internals to fake the state up.
//!
//! # Why a saved, reopened world
//!
//! The fixture closes the world after its initial setup, reopens it, and only
//! then checks that a *fresh* connection into the reopened world still grants
//! and moves a real ticket. This keeps the residency assertions meaningful
//! across the persistence boundary rather than limiting them to one live
//! `IntegratedServer` instance.
//!
//! # What each step proves
//!
//! 1. Join grants a real `PLAYER_LOADING`/`PLAYER_SIMULATION` pair at the
//!    join column, **and** a real world-spawn `PLAYER_SPAWN` ticket
//!    (radius [`PLAYER_SPAWN_RADIUS`]) independent of the player's ticket.
//! 2. A real `PlayerMoved` packet moves the player's own ticket pair off the
//!    join column onto the new one, while the spawn ticket — a **separate**
//!    ticket under a separate owner — stays exactly where it was.
//! 3. A second real move carries the player ticket again, and the *first*
//!    destination stops being ticket-resident once the player ticket that
//!    was the only thing holding it up has moved away — proving unload is
//!    reachable from real traffic, not just from a hand-driven
//!    `remove_ticket` call.
//! 4. A real, clean disconnect (dropping the client's own transport half, the
//!    same path `serve_connection_inner`'s `Ok(None)` disconnect arm reaches)
//!    drops [`PlayerTicketGuard`], withdrawing the player's own ticket pair —
//!    while the world-spawn ticket, which nothing in this connection's own
//!    lifetime withdraws, persists.
//!
//! # The control
//!
//! Step 2's "the spawn column is still resident once the player has moved
//! away" assertion needs proof the residency check can read `false` at all —
//! otherwise a `TicketStoreHandle` bug that reports every column resident
//! would pass this file trivially. Step 3's "the first destination is no
//! longer resident" is exactly that control, and it fires before the
//! positive assertions that lean on the same check reading `true`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Reader, State, Writer};
use lodestone_net::Connection;
use lodestone_server::ticket::PLAYER_SPAWN_RADIUS;
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol};
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
const PING_REQUEST_C2S: i32 = 91;
const PONG_RESPONSE_S2C: i32 = 92;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// Flat, cheap, deterministic terrain, matching
/// `singleplayer_shutdown_player_persistence.rs`'s own fixture: this gate is
/// about the ticket graph, not worldgen, and a flat stone floor gives
/// `find_initial_spawn` an unambiguous surface at every column so the spawn
/// search this test relies on is not itself a variable.
#[derive(Debug, Clone, Default)]
struct FlatWorld {
    unloaded: Arc<Mutex<Vec<(i32, i32)>>>,
}

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

    fn unload(&self, cx: i32, cz: i32) {
        self.unloaded
            .lock()
            .expect("unload log poisoned")
            .push((cx, cz));
    }
}

/// A minimal stand-in wire format — own vocabulary, trimmed from
/// `singleplayer_shutdown_player_persistence.rs`'s identically-named type to
/// exactly what this gate needs: login/join, movement, and a ping/pong round
/// trip as an exact "the server has already dispatched everything sent
/// before this" signal.
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

/// Drives handshake → login → configuration → the initial (one-column, since
/// this gate opens with `view_radius: 0`) join view, leaving the connection
/// parked in `State::Play`.
async fn drive_login_and_join<T: lodestone_net::Transport>(client: &mut Connection<T>, username: &str) {
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

    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_START);
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK);
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_FINISHED);
}

async fn send_player_moved<T: lodestone_net::Transport>(client: &mut Connection<T>, x: f64, y: f64, z: f64) {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    client
        .write_packet(PLAYER_MOVED_C2S, w.as_slice())
        .await
        .expect("send move");
}

/// Sends a ping and blocks until the matching pong — proof the server has
/// already dispatched every packet written before this one (packets are
/// processed one `conn.read_packet()` at a time, in order), used instead of a
/// sleep for the same reason `singleplayer_shutdown_player_persistence.rs`
/// uses it.
async fn ping_pong<T: lodestone_net::Transport>(client: &mut Connection<T>, time: i64) {
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
    }
}

/// A unique scratch directory. A literal nonce, not a pid or a random value —
/// the scratchpad is shared between concurrently running agents/tests.
fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-ticket-residency-{tag}-7q3m"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp world dir");
    dir
}

/// Chunk column a block position falls in — `floor`, matching
/// `crate::server`'s own `(x / 16.0).floor() as i32` derivation.
fn chunk_of(pos: (f64, f64, f64)) -> (i32, i32) {
    ((pos.0 / 16.0).floor() as i32, (pos.2 / 16.0).floor() as i32)
}

/// **The gate.** See this file's header for what each step proves.
#[tokio::test]
async fn a_real_join_move_and_disconnect_drive_real_ticket_residency() {
    let dir = tempdir("gate");
    let username = format!("Tick{:08x}", Uuid::new_v4().as_u128() as u32);

    // Step 0: open, then immediately close — the "saved, reopened world"
    // fixture this file's header explains, so the connection under test below
    // is not the first one this world directory has ever seen.
    {
        let (server, _client_end, _world) = IntegratedServer::open_persistent_with_mobs(
            FakeProtocol,
            &dir,
            FlatWorld::default(),
            MIN_Y,
            HEIGHT,
            (0..=0, 0..=0),
            (8, 8),
            0,
            0,
            Duration::from_secs(3600),
        )
        .expect("open persistent world (first open)");
        server.shutdown().await;
    }

    let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs(
        FakeProtocol,
        &dir,
        FlatWorld::default(),
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        // `view_radius: 0` — the player's own ticket covers exactly the
        // column it stands on, which is what makes "the previous column
        // stops being resident once the player leaves it" unambiguous: with
        // no view radius, nothing but the spawn ticket or the player's exact
        // position can hold a column up.
        0,
        Duration::from_secs(3600),
    )
    .expect("open persistent world (reopened)");

    let tickets = server
        .tickets()
        .expect("open_persistent_with_mobs always sets a HostCore, so a real ticket handle exists");

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, &username).await;
    tickets.tick();

    // The join column and the world-spawn column coincide on a fresh world
    // (no saved player data, so `join_pos == spawn.pos`) — record it once so
    // later steps can name it without recomputing the spawn search.
    //
    // `find_initial_spawn` on this flat world settles on the origin column
    // for the same reason `singleplayer_shutdown_player_persistence.rs`'s own
    // fixture does: a valid surface exists at (8, 60, 8) and the spiral search
    // never has to move off it.
    let spawn_chunk = (0, 0);

    let mut mismatches = Vec::new();

    // Step 1: the real join must have granted both the player's own ticket
    // and the independent world-spawn ticket at the same column.
    if !tickets.is_resident(spawn_chunk) {
        mismatches.push(format!(
            "step 1: spawn column {spawn_chunk:?} must be resident right after a real join"
        ));
    }

    // Step 2: move far enough that a `view_radius: 0` player ticket at the
    // new column cannot also cover the spawn column (chebyshev distance must
    // exceed `PLAYER_SPAWN_RADIUS`), so if the spawn column is *still*
    // resident afterwards, only the separate world-spawn ticket explains it.
    const MID: (f64, f64, f64) = (328.0, 70.0, 328.0); // chunk (20, 20)
    let mid_chunk = chunk_of(MID);
    assert!(
        mid_chunk.0.max(mid_chunk.1) > PLAYER_SPAWN_RADIUS,
        "test bug: MID must be outside the spawn ticket's own radius"
    );
    send_player_moved(&mut client, MID.0, MID.1, MID.2).await;
    ping_pong(&mut client, 1).await;
    tickets.tick();

    if !tickets.is_resident(mid_chunk) {
        mismatches.push(format!(
            "step 2: {mid_chunk:?} must be resident after a real PlayerMoved packet carried the \
             player ticket there — the positive control for step 3's absence check"
        ));
    }
    if !tickets.is_resident(spawn_chunk) {
        mismatches.push(format!(
            "step 2 (issue #297): spawn column {spawn_chunk:?} must still be resident after the \
             player moved away — it is held up by a separate world-spawn ticket, not by the \
             player's own"
        ));
    }

    // Step 3: move again, further still. `mid_chunk` held nothing but the
    // player's own ticket (step 2 already showed it is outside the spawn
    // ticket's radius), so once that ticket has moved off it, `mid_chunk`
    // must stop being resident — real production traffic causing a real
    // unload, and the control that proves `is_resident` can read `false`.
    const FAR: (f64, f64, f64) = (808.0, 70.0, 808.0); // chunk (50, 50)
    let far_chunk = chunk_of(FAR);
    send_player_moved(&mut client, FAR.0, FAR.1, FAR.2).await;
    ping_pong(&mut client, 2).await;
    tickets.tick();

    if !tickets.is_resident(far_chunk) {
        mismatches.push(format!("step 3: {far_chunk:?} must be resident after the second move"));
    }
    if tickets.is_resident(mid_chunk) {
        mismatches.push(format!(
            "step 3 (the control): {mid_chunk:?} must NOT still be resident — nothing but the \
             player ticket ever covered it, and that ticket has since moved to {far_chunk:?}"
        ));
    }
    if !tickets.is_resident(spawn_chunk) {
        mismatches.push(format!(
            "step 3: spawn column {spawn_chunk:?} must still be resident two moves later"
        ));
    }

    // Step 4: a real, clean disconnect — drop the client's own transport
    // half, which is what makes the server's `conn.read_packet()` observe a
    // real `Ok(None)` rather than the shutdown-race path
    // `singleplayer_shutdown_player_persistence.rs` deliberately exercises
    // instead. `PlayerTicketGuard::drop` is what should withdraw the ticket
    // pair here, on the connection's own natural exit.
    drop(client);
    let far_released = wait_until(|| !tickets.is_resident(far_chunk), &tickets).await;
    if !far_released {
        mismatches.push(format!(
            "step 4: {far_chunk:?} must stop being resident once the connection disconnects and \
             its PlayerTicketGuard drops"
        ));
    }
    // The spawn ticket belongs to the world, not to this one connection, so
    // it must survive the disconnect that just withdrew the player's own
    // ticket pair.
    if !tickets.is_resident(spawn_chunk) {
        mismatches.push(format!(
            "step 4: spawn column {spawn_chunk:?} must still be resident after the connection \
             that joined it has disconnected — issue #297's ticket belongs to the world"
        ));
    }

    assert!(
        mismatches.is_empty(),
        "ticket residency must follow real join/move/disconnect traffic; mismatches: \
         {mismatches:#?}"
    );

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A live server owns the cache lifecycle hand-off as well as the ticket graph:
/// a client crossing negative chunk boundaries first loads `(-1, 0)`, then
/// releases it after crossing into `(-2, 0)`. The source log observes the real
/// `ChunkStore` call, rather than an inspected ticket delta alone.
#[tokio::test]
async fn integrated_server_unloads_a_negative_chunk_through_the_owned_lifecycle_plan() {
    let world = FlatWorld::default();
    let unloads = Arc::clone(&world.unloaded);
    let (server, client_end) = IntegratedServer::open_in_memory_with_mobs(
        FakeProtocol,
        world,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
    );
    let mut client = Connection::new(client_end);
    let username = format!("Life{:08x}", Uuid::new_v4().as_u128() as u32);
    drive_login_and_join(&mut client, &username).await;

    // `floor(-0.5 / 16.0)` is -1; truncating toward zero would leave the
    // intended lifecycle column at the origin and make this assertion vacuous.
    const FIRST: (f64, f64, f64) = (-0.5, 70.0, 8.0);
    const SECOND: (f64, f64, f64) = (-16.5, 70.0, 8.0);
    assert_eq!(chunk_of(FIRST), (-1, 0));
    assert_eq!(chunk_of(SECOND), (-2, 0));

    send_player_moved(&mut client, FIRST.0, FIRST.1, FIRST.2).await;
    ping_pong(&mut client, 10).await;
    send_player_moved(&mut client, SECOND.0, SECOND.1, SECOND.2).await;
    ping_pong(&mut client, 11).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let observed = unloads.lock().expect("unload log poisoned").clone();
        if observed.contains(&(-1, 0)) {
            assert!(
                observed.windows(2).all(|pair| pair[0] < pair[1]),
                "the live cache-release hand-off must retain canonical (cx, cz) order: {observed:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the real integrated server never released chunk (-1, 0) after crossing into (-2, 0); observed unloads: {observed:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    server.shutdown().await;
}

/// Two real connections share one column; the column must survive either player
/// alone leaving it, and only stop being resident once both have. The first
/// connection uses the in-memory duplex returned by
/// `open_in_memory_with_mobs`, while the second uses a real TCP socket via
/// [`IntegratedServer::publish`] so both transport paths are exercised.
#[tokio::test]
async fn a_chunk_near_two_players_stays_resident_when_either_one_alone_moves_away() {
    let (mut server, client_a_end) = IntegratedServer::open_in_memory_with_mobs(
        FakeProtocol,
        FlatWorld::default(),
        (0..=0, 0..=0),
        (8, 8),
        0,
        // `view_radius: 0`, same reasoning as the first gate: each player's
        // own ticket then covers exactly the column they stand on, so
        // "shares a column" and "moves off it" are unambiguous.
        0,
    );
    let tickets = server
        .tickets()
        .expect("open_in_memory_with_mobs always sets a HostCore, so a real ticket handle exists");
    let addr = server
        .publish(("127.0.0.1", 0), None)
        .await
        .expect("publish a second, TCP-backed connection into this same running world");

    let username_a = format!("Alpha{:08x}", Uuid::new_v4().as_u128() as u32);
    let mut client_a = Connection::new(client_a_end);
    drive_login_and_join(&mut client_a, &username_a).await;
    tickets.tick();

    let username_b = format!("Beta{:08x}", Uuid::new_v4().as_u128() as u32);
    let stream_b = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect the second, TCP-backed player");
    let mut client_b = Connection::new(stream_b);
    drive_login_and_join(&mut client_b, &username_b).await;
    tickets.tick();

    // Both players start at the world spawn column on this fresh world (no
    // saved player data for either), so both must first move to a shared,
    // non-spawn column before "shares a column" is a claim about their own
    // tickets rather than about the independent world-spawn ticket.
    const SHARED: (f64, f64, f64) = (328.0, 70.0, 328.0); // chunk (20, 20)
    let shared_chunk = chunk_of(SHARED);
    assert!(
        shared_chunk.0.max(shared_chunk.1) > PLAYER_SPAWN_RADIUS,
        "test bug: SHARED must be outside the spawn ticket's own radius"
    );
    send_player_moved(&mut client_a, SHARED.0, SHARED.1, SHARED.2).await;
    ping_pong(&mut client_a, 1).await;
    send_player_moved(&mut client_b, SHARED.0, SHARED.1, SHARED.2).await;
    ping_pong(&mut client_b, 2).await;
    tickets.tick();

    let mut mismatches = Vec::new();
    if !tickets.is_resident(shared_chunk) {
        mismatches.push(format!(
            "setup: {shared_chunk:?} must be resident once both players stand on it"
        ));
    }

    // A alone leaves. B's own ticket still covers `shared_chunk`, so it must
    // stay resident while B remains there.
    const FAR_A: (f64, f64, f64) = (808.0, 70.0, 8.0); // chunk (50, 0)
    send_player_moved(&mut client_a, FAR_A.0, FAR_A.1, FAR_A.2).await;
    ping_pong(&mut client_a, 3).await;
    tickets.tick();
    if !tickets.is_resident(shared_chunk) {
        mismatches.push(format!(
            "{shared_chunk:?} must stay resident when only one of its two players leaves — \
             player B is still standing on it"
        ));
    }

    // Now B leaves too. Nothing but the two player tickets ever covered
    // `shared_chunk` (it is outside the spawn ticket's radius, asserted
    // above), so it must stop being resident now that both are gone.
    const FAR_B: (f64, f64, f64) = (8.0, 70.0, 808.0); // chunk (0, 50)
    send_player_moved(&mut client_b, FAR_B.0, FAR_B.1, FAR_B.2).await;
    ping_pong(&mut client_b, 4).await;
    tickets.tick();
    if tickets.is_resident(shared_chunk) {
        mismatches.push(format!(
            "{shared_chunk:?} must stop being resident once BOTH players have left it — this is \
             the control proving the earlier \"still resident\" assertion was not simply always \
             true"
        ));
    }

    assert!(
        mismatches.is_empty(),
        "a shared column's residency must be the union of every player ticket covering it; \
         mismatches: {mismatches:#?}"
    );

    server.shutdown().await;
}

/// Polls `predicate`, ticking the store between attempts, until it is `true`
/// or a generous bound is exhausted — the disconnect side of step 4 is
/// asynchronous (the server's connection task has to actually run and
/// observe the closed duplex), so a single immediate check would be racy.
async fn wait_until(
    mut predicate: impl FnMut() -> bool,
    tickets: &lodestone_server::TicketStoreHandle,
) -> bool {
    for _ in 0..200 {
        if predicate() {
            return true;
        }
        tokio::task::yield_now().await;
        tickets.tick();
    }
    predicate()
}

fn test_uuid_for(username: &str) -> Uuid {
    Uuid::new_v3(&Uuid::NAMESPACE_OID, username.as_bytes())
}
