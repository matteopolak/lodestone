//! Hermetic proof of the three things that make a served [`State::Play`]
//! connection survive, keep time, and follow the player, over the same
//! in-memory `Connection`/`Transport` path `integrated_memory.rs` already
//! exercises for the join sequence.
//!
//! This uses a small stand-in [`ServerProtocol`] (own wire format, not
//! vanilla 26.2's — the real-protocol counterpart of these same three things
//! lives in `crates/protocol/v770/tests/server_liveness.rs`) so the
//! assertions here are about `lodestone-server`'s own scheduling logic
//! (`ViewTracker`, `serve_play`'s keep-alive/time-sync timers), not about
//! wire-layout fidelity.
//!
//! # Controls
//!
//! Per `CLAUDE.md`'s evidence standard, an assertion of an absence needs a
//! control proving the detector actually fires:
//!
//! * [`silent_client_is_disconnected_after_keep_alive_timeout`] is the
//!   **positive** control — the keep-alive mechanism actually firing and
//!   disconnecting a genuinely unresponsive peer.
//! * [`responsive_client_survives_multiple_keep_alive_intervals`] is the
//!   **negative** control run against the *same* mechanism — a peer that
//!   answers every challenge must **not** be disconnected, across several
//!   intervals, not just one.
//!
//! Both run under `#[tokio::test(start_paused = true)]`: the 15-second
//! keep-alive interval and 1-second time-sync interval are real
//! `tokio::time` intervals, but with the clock paused and auto-advancing
//! whenever the runtime is otherwise idle, both tests resolve in a fraction
//! of a second of wall-clock time — the same pattern already established in
//! `crates/lodestone-net/src/connection.rs`'s own
//! `read_packet_timeout_fires_when_peer_is_silent` test.

use std::collections::HashSet;
use std::time::Duration;

use lodestone_core::{Reader, State, Writer};
use lodestone_model::Difficulty;
use lodestone_net::{Connection, NetError, memory_pair};
use lodestone_server::{
    ChunkColumn, ChunkSource, NoEntities, ServerBound, ServerDirective, ServerError,
    ServerProtocol, serve_connection,
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

// Play-state wire ids this stand-in protocol adds on top of the join
// sequence above — a private vocabulary distinct from any real protocol's,
// exactly like `integrated_memory.rs`'s `FakeProtocol`.
const KEEP_ALIVE_S2C: i32 = 40;
const KEEP_ALIVE_C2S: i32 = 41;
const PLAYER_MOVED_C2S: i32 = 42;
const SET_TIME_S2C: i32 = 43;
const SET_CHUNK_CACHE_CENTER_S2C: i32 = 44;
const FORGET_LEVEL_CHUNK_S2C: i32 = 45;
const AIR_SUPPLY_S2C: i32 = 46;
const SET_HEALTH_S2C: i32 = 47;
const CHANGE_DIFFICULTY_C2S: i32 = 48;
const CHANGE_DIFFICULTY_S2C: i32 = 49;

/// A [`ChunkSource`] that hands out an all-air column instantly — these
/// tests are about packet scheduling, not terrain, so real worldgen would
/// only add cost and noise.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }
}

/// A [`ChunkSource`] whose every block is `minecraft:water` — the drowning
/// tests' subject world. Filling the *entire* column (not just a shallow
/// pool) means any in-range player position is submerged regardless of
/// exactly where `y` lands, which is what lets the drowning tests below
/// place the player with a plain [`send_player_moved`] and not also have to
/// reason about a precise pool depth.
struct WaterSource;

impl ChunkSource for WaterSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:water");
                }
            }
        }
        col
    }
}

/// Stand-in protocol: the same login/configuration wire format
/// `integrated_memory.rs` uses, plus the four new keep-alive/time/view
/// encoders and the two new serverbound decodes this task adds.
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
                ServerBound::LoginStart {
                    username,
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == KEEP_ALIVE_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::KeepAlive {
                    id: r.i64().expect("keep-alive id"),
                }
            }
            State::Play if packet_id == PLAYER_MOVED_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::PlayerMoved {
                    x: r.f64().expect("x"),
                    y: r.f64().expect("y"),
                    z: r.f64().expect("z"),
                    // This stand-in wire format never carried an on_ground
                    // bit and these tests are about keep-alive/time/view
                    // scheduling, not fall damage — `true` is an arbitrary,
                    // harmless choice (a landing sample every packet, never
                    // producing a multi-tick accumulated fall).
                    on_ground: true,
                }
            }
            // Issue #268: a minimal stand-in wire format for the
            // difficulty round trip — a single byte ordinal, matching the
            // real protocol's semantics (0..=3) but not its VarInt framing,
            // since this file's whole point is testing `lodestone-server`'s
            // own scheduling/consumer logic, not wire fidelity (that lives
            // in `crates/protocol/v770/src/server_protocol.rs`'s own tests).
            State::Play if packet_id == CHANGE_DIFFICULTY_C2S => {
                let mut r = Reader::new(payload);
                let difficulty = match r.u8().expect("difficulty ordinal") {
                    0 => Difficulty::Peaceful,
                    1 => Difficulty::Easy,
                    2 => Difficulty::Normal,
                    _ => Difficulty::Hard,
                };
                ServerBound::DifficultyChanged { difficulty }
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

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        let mut w = Writer::default();
        w.i64(id);
        ServerDirective::Send {
            packet_id: KEEP_ALIVE_S2C,
            payload: w.as_slice().to_vec(),
        }
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

    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        ServerDirective::Send {
            packet_id: SET_CHUNK_CACHE_CENTER_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        ServerDirective::Send {
            packet_id: FORGET_LEVEL_CHUNK_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(air);
        ServerDirective::Send {
            packet_id: AIR_SUPPLY_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_set_health(&self, health: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.f32(health);
        ServerDirective::Send {
            packet_id: SET_HEALTH_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        let mut w = Writer::default();
        w.u8(match difficulty {
            Difficulty::Peaceful => 0,
            Difficulty::Easy => 1,
            Difficulty::Normal => 2,
            Difficulty::Hard => 3,
        });
        w.bool(locked);
        ServerDirective::Send {
            packet_id: CHANGE_DIFFICULTY_S2C,
            payload: w.as_slice().to_vec(),
        }
    }
}

/// Drives the client side of handshake → login → configuration → the
/// initial chunk view, asserting the join-time full time sync arrives
/// (`SET_TIME_S2C`, before any chunk) and that exactly `expected_chunks`
/// columns are batched. Leaves the connection parked in `State::Play`,
/// ready for whatever the caller wants to test next.
async fn drive_login_and_join(
    client: &mut Connection<DuplexStream>,
    username: &str,
    expected_chunks: usize,
) {
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
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

    // The join-time full clock sync precedes chunk streaming, mirroring
    // vanilla's `PlayerList.sendLevelInfo` (`PlayerList.java:648-651`).
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(
        id, SET_TIME_S2C,
        "join sequence must send the full time sync before any chunk"
    );

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_START);
    assert!(payload.is_empty());

    for _ in 0..expected_chunks {
        let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
        assert_eq!(id, CHUNK);
    }

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_FINISHED);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().unwrap(), expected_chunks as i32);
}

/// Reads every packet already available (or that arrives within a short,
/// paused-clock-friendly window) without blocking indefinitely: repeated
/// [`Connection::read_packet_timeout`] calls until one times out. Safe under
/// `start_paused = true` — the same pattern
/// `read_packet_timeout_fires_when_peer_is_silent` in
/// `crates/lodestone-net/src/connection.rs` already proves, and the 50ms
/// budget is well below the 1s time-sync interval, so draining never
/// accidentally waits long enough to pick up a periodic broadcast that was
/// not actually due. `VITALS_TICK_INTERVAL` is *also* 50ms (matching
/// vanilla's own per-tick cadence — see `crate::vitals`'s module docs, not
/// duplicated here since this crate is `lodestone-server`'s *caller*), so
/// under paused-clock auto-advance this races the timeout against the
/// server's own next vitals tick at the same virtual instant; that race
/// still resolves correctly for a control (dry, or at full air) because that
/// tick produces no directive at all to read, so the timeout still fires.
/// The drowning tests below therefore read directly rather than through this
/// helper, since they need to actually wait across many consecutive vitals
/// ticks, not stop at the first one.
async fn drain_available(client: &mut Connection<DuplexStream>) -> Vec<(i32, Vec<u8>)> {
    let mut out = Vec::new();
    loop {
        match client.read_packet_timeout(Duration::from_millis(50)).await {
            Ok(Some(packet)) => out.push(packet),
            Ok(None) => break,
            Err(NetError::Timeout { .. }) => break,
            Err(e) => panic!("unexpected network error while draining: {e}"),
        }
    }
    out
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

/// The square `[-r, r]²` chunk window around `(cx, cz)` — the same shape
/// `ViewTracker` and `serve_connection`'s initial view both use.
fn square(cx: i32, cz: i32, r: i32) -> HashSet<(i32, i32)> {
    let mut s = HashSet::new();
    for dz in -r..=r {
        for dx in -r..=r {
            s.insert((cx + dx, cz + dz));
        }
    }
    s
}

/// Splits a drained packet batch into the cache-center update (at most one),
/// the set of forgotten columns, and the set of newly sent columns —
/// tolerating the chunk-batch markers around the latter.
fn split_view_directives(
    packets: &[(i32, Vec<u8>)],
) -> (Option<(i32, i32)>, HashSet<(i32, i32)>, HashSet<(i32, i32)>) {
    let mut center = None;
    let mut forgotten = HashSet::new();
    let mut added = HashSet::new();
    for (id, payload) in packets {
        let mut r = Reader::new(payload);
        if *id == SET_CHUNK_CACHE_CENTER_S2C {
            let cx = r.var_i32().unwrap();
            let cz = r.var_i32().unwrap();
            assert!(
                center.replace((cx, cz)).is_none(),
                "more than one cache-center update in a single recenter"
            );
        } else if *id == FORGET_LEVEL_CHUNK_S2C {
            let cx = r.var_i32().unwrap();
            let cz = r.var_i32().unwrap();
            assert!(
                forgotten.insert((cx, cz)),
                "duplicate forget for ({cx}, {cz})"
            );
        } else if *id == CHUNK {
            let cx = r.var_i32().unwrap();
            let cz = r.var_i32().unwrap();
            assert!(added.insert((cx, cz)), "duplicate chunk send for ({cx}, {cz})");
        } else if *id != CHUNK_BATCH_START && *id != CHUNK_BATCH_FINISHED {
            panic!("unexpected packet id {id} in a view diff: {payload:?}");
        }
    }
    (center, forgotten, added)
}

/// **Positive control**: a client that stops responding after joining —
/// connected, but never echoing anything back — must actually be
/// disconnected once the keep-alive challenge goes unanswered, not merely
/// "would be" in theory.
#[tokio::test(start_paused = true)]
async fn silent_client_is_disconnected_after_keep_alive_timeout() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0).await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Ghost", 1).await;
    // `client` is deliberately held open (not dropped, not read from, not
    // written to) from here — a genuine stall, not a clean disconnect, which
    // `serve_connection` already handles as a normal `Ok`.

    let result = server.await.expect("server task panicked");
    assert!(
        matches!(result, Err(ServerError::KeepAliveTimeout)),
        "expected KeepAliveTimeout, got {result:?}"
    );

    drop(client);
}

/// **Negative control**, run against the exact same mechanism: a client that
/// answers every keep-alive challenge must stay connected across several
/// intervals — not just long enough to look alive once.
#[tokio::test(start_paused = true)]
async fn responsive_client_survives_multiple_keep_alive_intervals() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0).await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Alive", 1).await;

    // Answer four consecutive keep-alive challenges — comfortably more than
    // one interval's worth — echoing each id straight back, exactly as
    // `lodestone-client`'s default automatic `KeepAlivePolicy` does
    // (`crates/lodestone-client/src/driver.rs`'s `ClientEvent::KeepAlive`
    // arm). Anything else received (the periodic time broadcast) is drained
    // and ignored.
    let mut answered = 0;
    while answered < 4 {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        if id == KEEP_ALIVE_S2C {
            client
                .write_packet(KEEP_ALIVE_C2S, &payload)
                .await
                .expect("echo keep-alive");
            answered += 1;
        }
    }

    // Closing now must still be a clean `Ok` — the same mechanism that fired
    // `KeepAliveTimeout` in the sibling test above did not fire here.
    drop(client);
    let result = server.await.expect("server task panicked");
    assert!(
        matches!(result, Ok(_)),
        "expected a clean close after answered keep-alives, got {result:?}"
    );
}

/// The join-time full clock sync anchors at tick 0, and the periodic
/// broadcasts that follow carry a strictly increasing `game_time` with no
/// anchor — proving both halves of `ServerProtocol::encode_set_time`'s
/// contract are actually driven, not just implemented.
#[tokio::test(start_paused = true)]
async fn time_of_day_anchors_at_join_then_broadcasts_periodically() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0).await
    });

    let mut client = Connection::new(client_end);
    // `drive_login_and_join` already asserts the join-time `SET_TIME_S2C`
    // arrives before any chunk; re-derive its payload here to check the
    // anchor's actual value rather than only its position in the sequence.
    client.write_packet(HANDSHAKE, &[2]).await.unwrap();
    let mut w = Writer::default();
    w.string("Clockwatcher");
    client.write_packet(LOGIN_START, w.as_slice()).await.unwrap();
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.unwrap();
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .unwrap();

    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, SET_TIME_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.i64().unwrap(), 0, "join-time game_time must be 0");
    assert!(
        r.bool().unwrap(),
        "join-time sync must carry a day/night anchor"
    );
    assert_eq!(r.i64().unwrap(), 0, "join-time anchor must be tick 0");

    // Drain the initial chunk batch (1 column, view_radius 0) to reach the
    // steady state.
    client.read_packet().await.unwrap().unwrap(); // CHUNK_BATCH_START
    client.read_packet().await.unwrap().unwrap(); // the one CHUNK
    client.read_packet().await.unwrap().unwrap(); // CHUNK_BATCH_FINISHED

    // Now in `serve_play`: collect the periodic broadcasts. Each carries no
    // anchor (an empty clock-update map on the real wire), and `game_time`
    // must strictly increase — proof the 1-second `TIME_SYNC_INTERVAL` timer
    // is actually firing repeatedly, not just once.
    let mut last_game_time = -1i64;
    for _ in 0..3 {
        let (id, payload) = client.read_packet().await.unwrap().unwrap();
        assert_eq!(id, SET_TIME_S2C);
        let mut r = Reader::new(&payload);
        let game_time = r.i64().unwrap();
        assert!(
            !r.bool().unwrap(),
            "periodic sync must not carry a day/night anchor"
        );
        assert!(
            game_time > last_game_time,
            "game_time must strictly increase: {last_game_time} -> {game_time}"
        );
        last_game_time = game_time;
    }

    drop(client);
    let _ = server.await.unwrap();
}

/// View streaming is vacuous if the player never actually crosses a chunk
/// boundary. This moves through three states — no change, a jump far enough
/// that the old and new windows share nothing, then a one-column shift — and
/// asserts on **which** columns were sent and dropped each time, not just a
/// count.
#[tokio::test(start_paused = true)]
async fn player_moved_streams_view_across_several_chunk_boundaries() {
    let view_radius = 1; // 3x3 = 9 columns
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, view_radius).await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Walker", 9).await;

    // Same-chunk movement: must touch nothing, matching vanilla's own guard
    // (`ChunkMap::updateChunkTracking` only recomputes the view when the 2D
    // chunk position actually changes).
    send_player_moved(&mut client, 1.0, 64.0, 1.0).await;
    let noop = drain_available(&mut client).await;
    assert!(
        noop.is_empty(),
        "same-chunk movement must not touch the view: {noop:?}"
    );

    // A jump to chunk (10, 0): far enough that the old 3x3 (centered (0,0))
    // and new 3x3 (centered (10,0)) windows share no columns at all.
    send_player_moved(&mut client, 160.0, 64.0, 0.0).await;
    let jump = drain_available(&mut client).await;
    let (center, forgotten, added) = split_view_directives(&jump);
    assert_eq!(center, Some((10, 0)));
    assert_eq!(forgotten, square(0, 0, view_radius));
    assert_eq!(added, square(10, 0, view_radius));

    // One more chunk to the right: a partial diff. Exactly the trailing
    // (x = 9) column leaves, exactly the new (x = 12) column enters, and the
    // two shared columns (x = 10, 11) are touched by neither.
    send_player_moved(&mut client, 176.0, 64.0, 0.0).await;
    let shift = drain_available(&mut client).await;
    let (center2, forgotten2, added2) = split_view_directives(&shift);
    assert_eq!(center2, Some((11, 0)));
    assert_eq!(
        forgotten2,
        HashSet::from([(9, -1), (9, 0), (9, 1)]),
        "expected exactly the trailing column to be forgotten"
    );
    assert_eq!(
        added2,
        HashSet::from([(12, -1), (12, 0), (12, 1)]),
        "expected exactly the new column to be sent"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// Reads packets until a [`SET_HEALTH_S2C`] arrives, collecting every
/// [`AIR_SUPPLY_S2C`] value seen along the way (in order) and discarding
/// anything else (keep-alive, time-sync noise interleaved by the same
/// `tokio::select!` loop). Returns `(air_values, health_after)`.
async fn read_until_health_update(client: &mut Connection<DuplexStream>) -> (Vec<i32>, f32) {
    let mut air_values = Vec::new();
    loop {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        let mut r = Reader::new(&payload);
        if id == AIR_SUPPLY_S2C {
            air_values.push(r.var_i32().expect("air value"));
        } else if id == SET_HEALTH_S2C {
            return (air_values, r.f32().expect("health value"));
        }
        // else: keep-alive / time-sync noise, ignored.
    }
}

/// **Subject**: a player whose eye is submerged the whole time must lose air
/// on the exact vanilla cadence (`crate::vitals`'s module doc comment,
/// mirrored from `LivingEntity.baseTick`/`decreaseAirSupply`/
/// `shouldTakeDrowningDamage`) and take the first drowning hit at exactly
/// tick 320 (300 ticks = 15s to empty from full, then 20 more ticks = 1s to
/// cross the `<= -20` threshold) — not some rounder or approximated number.
/// [`WaterSource`] fills the *entire* column, so the player is genuinely
/// submerged throughout (the "world" species of vacuous test this guards
/// against: a player who never actually gets wet would prove nothing).
///
/// This test spans 320 vitals ticks (16s of virtual time) to the first hit,
/// then a further 20 ticks (1s) to the second — 340 real tick-cadence steps
/// in total, all resolved by `tokio`'s paused-clock auto-advance in a
/// fraction of a second of wall time, the same mechanism the keep-alive
/// tests above already rely on for their 15s+ spans. This is deliberately
/// **not** a short window: the "duration" species of vacuous test
/// (`CLAUDE.md`) would pass a test that only ran a handful of ticks even if
/// the real cadence were wrong, since nothing would yet distinguish "1 tick"
/// from "20 ticks" from "300 ticks". Spanning past two full hits is what
/// proves the cadence repeats rather than being a one-off.
#[tokio::test(start_paused = true)]
async fn submerged_player_loses_air_and_takes_drowning_damage_on_vanilla_cadence() {
    let (client_end, server_end) = memory_pair();
    let source = WaterSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0).await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Diver", 1).await;

    // Any position inside chunk (0, 0) with y in [0, 16) is submerged: the
    // entire `WaterSource` column is water, feet at y = 8 puts the eye
    // (8 + 1.62 = 9.62, floored to block y = 9) in water too.
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    let (air_values, health_after_first_hit) = read_until_health_update(&mut client).await;

    // Expected sequence, derived the same way `PlayerVitals::tick` computes
    // it rather than restated as a magic literal: 319 decrements from 300
    // (299, 298, ..., 0, -1, ..., -19), then the 320th tick resets to 0 on
    // crossing the damage threshold.
    let mut expected = Vec::new();
    let mut air = 300;
    for _ in 0..319 {
        air -= 1;
        expected.push(air);
    }
    expected.push(0);

    assert_eq!(
        air_values, expected,
        "air must count down by exactly 1/tick, resetting to 0 only on the hit"
    );
    assert_eq!(
        health_after_first_hit, 18.0,
        "first drowning hit must deal exactly 2.0 damage (20.0 -> 18.0)"
    );

    // The countdown re-arms identically: the second hit must land exactly
    // 20 ticks later, not immediately and not some other interval.
    let (air_values2, health_after_second_hit) = read_until_health_update(&mut client).await;
    let mut expected2 = Vec::new();
    let mut air2 = 0;
    for _ in 0..19 {
        air2 -= 1;
        expected2.push(air2);
    }
    expected2.push(0);

    assert_eq!(air_values2, expected2, "the re-armed countdown must also take exactly 20 ticks");
    assert_eq!(
        health_after_second_hit, 16.0,
        "second drowning hit must also deal exactly 2.0 damage (18.0 -> 16.0)"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **Control**: a player who is never submerged (an all-air world, matching
/// `AirSource`) must receive **zero** air-supply or health updates, even
/// across a window (20s) comfortably longer than the 16s the subject test
/// above takes to reach its first drowning hit. Per `CLAUDE.md`'s evidence
/// standard this is the control that proves the submersion test actually
/// gates the tick — not merely that the subject test happened to show
/// numbers going down, which alone would not rule out air draining
/// regardless of water.
#[tokio::test(start_paused = true)]
async fn dry_player_keeps_full_air_and_takes_no_damage() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0).await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Dry", 1).await;
    send_player_moved(&mut client, 8.0, 64.0, 8.0).await;

    tokio::time::sleep(Duration::from_secs(20)).await;

    let packets = drain_available(&mut client).await;
    let stray: Vec<_> = packets
        .iter()
        .filter(|(id, _)| *id == AIR_SUPPLY_S2C || *id == SET_HEALTH_S2C)
        .collect();
    assert!(
        stray.is_empty(),
        "a dry player must never receive an air-supply or health update: {stray:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// Issue #268's actual consumer, exercised through the real scheduling loop
/// (`dispatch_play_packet`/`apply_difficulty_change`) rather than just at the
/// `V770ServerProtocol` decode/encode layer (which
/// `crates/protocol/v770/src/server_protocol.rs`'s own `world_admin_tests`
/// already pins). A `ServerBound::DifficultyChanged` sent over a real
/// connection must produce exactly one confirmation back, carrying the
/// requested difficulty — proof `WorldAdminState` is real, connected state
/// and not a struct nothing calls into.
#[tokio::test(start_paused = true)]
async fn difficulty_change_is_confirmed_back_to_the_connection() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0).await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Op", 1).await;

    let mut w = Writer::default();
    w.u8(3); // Hard
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, w.as_slice())
        .await
        .expect("send change_difficulty");

    let (id, payload) = client
        .read_packet_timeout(Duration::from_secs(5))
        .await
        .expect("read")
        .expect("confirmation packet");
    assert_eq!(id, CHANGE_DIFFICULTY_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.u8().expect("difficulty"), 3, "confirmed difficulty must be Hard");
    assert!(
        !r.bool().expect("locked"),
        "difficulty was never locked in this test"
    );

    drop(client);
    let _ = server.await.unwrap();
}
