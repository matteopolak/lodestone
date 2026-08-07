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
use lodestone_model::{Difficulty, ItemStack};
use lodestone_net::{Connection, NetError, memory_pair};
use lodestone_server::{
    BlockEntityHandle, BlockTickFeed, ChunkColumn, ChunkSource, ChunkWorld, ExplosionFeed,
    MobHandle, MobSim, NoEntities, ServerBound, ServerDirective, ServerError, ServerProtocol,
    WeatherEvent, WeatherFeed, serve_connection, serve_connection_with_mob_events,
};
use std::str::FromStr;
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
// Issue #270's four newly-connected packets (creative-slot writes reuse
// #266's existing `PlayerInventory`/`apply_menu_slot_change` path and so need
// no new wire id of their own beyond the slot write itself).
const SET_CREATIVE_MODE_SLOT_C2S: i32 = 50;
const CLIENT_COMMAND_C2S: i32 = 51;
const CLIENT_INFORMATION_C2S: i32 = 52;
const CHUNK_BATCH_RECEIVED_C2S: i32 = 53;
const GAME_RULE_VALUES_S2C: i32 = 54;
const GAME_EVENT_S2C: i32 = 55;

/// A [`ChunkSource`] that hands out an all-air column instantly — these
/// tests are about packet scheduling, not terrain, so real worldgen would
/// only add cost and noise.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
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

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
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
                    // Likewise: this stand-in format carries no angles, and
                    // `None` is the honest lowering of that — the same value
                    // the real `move_player_pos` arm produces. Player-facing
                    // is gated in `tests/player_rotation.rs` against the real
                    // v770 decoder instead.
                    rotation: None,
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
            // Issue #266/#270: minimal stand-in wire formats for the four
            // newly-connected packets — same "test scheduling, not wire
            // fidelity" rationale as `CHANGE_DIFFICULTY_C2S` above.
            State::Play if packet_id == SET_CREATIVE_MODE_SLOT_C2S => {
                let mut r = Reader::new(payload);
                let slot = r.i16().expect("slot");
                let item = if r.bool().expect("present") {
                    let key = r.string(64).expect("item key");
                    let count = r.var_i32().expect("count");
                    Some(ItemStack::new(key.parse().expect("valid resource key"), count as u32))
                } else {
                    None
                };
                ServerBound::CreativeModeSlotSet { slot, item }
            }
            State::Play if packet_id == CLIENT_COMMAND_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::ClientCommand {
                    action: r.var_i32().expect("action"),
                }
            }
            State::Play if packet_id == CLIENT_INFORMATION_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::ClientInformationChanged {
                    view_distance: r.i8().expect("view distance"),
                }
            }
            State::Play if packet_id == CHUNK_BATCH_RECEIVED_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::ChunkBatchAcknowledged {
                    desired_chunks_per_tick: r.f32().expect("desired rate"),
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

    /// Issue #270's `REQUEST_GAMERULE_VALUES` confirmation — encodes only the
    /// entry count (the tests below only need to distinguish "a reply
    /// arrived, with N entries" from "no reply", not round-trip the actual
    /// key/value strings).
    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(entries.len() as i32);
        ServerDirective::Send {
            packet_id: GAME_RULE_VALUES_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    /// Issue #324. Mirrors the real v770 wire shape (`writeByte` event +
    /// `writeFloat` param) so the weather-drain gate can assert on actual
    /// bytes, not just on "a packet arrived".
    fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.u8(kind);
        w.f32(value);
        ServerDirective::Send {
            packet_id: GAME_EVENT_S2C,
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

/// Sends a `SET_CREATIVE_MODE_SLOT`-equivalent write. `item` mirrors the real
/// packet's `None` = clear-the-slot case.
async fn send_creative_slot(client: &mut Connection<DuplexStream>, slot: i16, item: Option<&ItemStack>) {
    let mut w = Writer::default();
    w.i16(slot);
    w.bool(item.is_some());
    if let Some(item) = item {
        w.string(&item.item.to_string());
        w.var_i32(item.count as i32);
    }
    client
        .write_packet(SET_CREATIVE_MODE_SLOT_C2S, w.as_slice())
        .await
        .expect("send creative slot");
}

/// Sends a `CLIENT_COMMAND`-equivalent request (`0` = respawn, `2` = request
/// current game-rule values — the two ordinals issue #270's consumer acts on).
async fn send_client_command(client: &mut Connection<DuplexStream>, action: i32) {
    let mut w = Writer::default();
    w.var_i32(action);
    client
        .write_packet(CLIENT_COMMAND_C2S, w.as_slice())
        .await
        .expect("send client command");
}

/// Sends a `CLIENT_INFORMATION`-equivalent settings change carrying only the
/// one field this crate's consumer reads.
async fn send_client_information(client: &mut Connection<DuplexStream>, view_distance: i8) {
    let mut w = Writer::default();
    w.i8(view_distance);
    client
        .write_packet(CLIENT_INFORMATION_C2S, w.as_slice())
        .await
        .expect("send client information");
}

/// Sends a `CHUNK_BATCH_RECEIVED`-equivalent acknowledgement — the flow-
/// control gate every one of the recentring tests below now has to satisfy to
/// see a *second* batch, matching vanilla's real "one batch in flight" wire
/// contract (see `ViewTracker`/`send_view_update`'s own doc comments).
async fn send_chunk_batch_received(client: &mut Connection<DuplexStream>, desired_chunks_per_tick: f32) {
    let mut w = Writer::default();
    w.f32(desired_chunks_per_tick);
    client
        .write_packet(CHUNK_BATCH_RECEIVED_C2S, w.as_slice())
        .await
        .expect("send chunk batch received");
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
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
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
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
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
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
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
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Walker", 9).await;

    // Issue #270's chunk-batch flow-control gate (`ServerBound::
    // ChunkBatchAcknowledged`) now holds a *second* batch until the first is
    // acknowledged — see `chunk_batch_is_held_until_acknowledged_then_flushed`
    // below for a test of that gate itself. This test is about the view diff
    // shape, not flow control, so it acks promptly after every batch,
    // exactly like a real client's automatic reply does — without this ack
    // the jump/shift batches below would be silently queued instead of sent.
    send_chunk_batch_received(&mut client, 10.0).await;

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
    send_chunk_batch_received(&mut client, 10.0).await;

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
/// Reads packets, discarding anything whose id is not `target_id`, until one
/// matches — the same "skip keep-alive/time-sync noise" tolerance
/// [`read_until_health_update`] already establishes, generalised to any
/// single target id. Needed because the two-directive respawn confirmation
/// (`SET_HEALTH_S2C` then `AIR_SUPPLY_S2C`) can have a stray periodic
/// broadcast queued immediately ahead of it: `tokio::select!` may pick the
/// drowning tick that reaches exactly `0.0` health and the 1-second
/// time-sync tick as *separate* loop iterations at the same virtual instant,
/// so a `SET_TIME_S2C` the server already queued before the client ever
/// sends the respawn command can still be sitting unread in the pipe —
/// asserting on the very next packet without skipping past it is exactly the
/// kind of interleaving-noise race `read_until_health_update`'s own doc
/// comment already accounts for.
async fn read_until(client: &mut Connection<DuplexStream>, target_id: i32) -> Vec<u8> {
    loop {
        let (id, payload) = client
            .read_packet_timeout(Duration::from_secs(5))
            .await
            .expect("read")
            .expect("packet");
        if id == target_id {
            return payload;
        }
    }
}

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
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
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
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
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
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
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

/// **The flow-control gate itself** (issue #270's real fix): a second chunk
/// batch must not be sent while the first is still unacknowledged — it is
/// queued instead — and must flush the moment the acknowledgement arrives.
/// Before this landing, `crate::server` started a fresh batch on every
/// `recenter` regardless of any outstanding ack (the issue's own "never reads
/// this reply" gap); this is the test that would fail against that old
/// behaviour, since nothing would ever be queued at all.
#[tokio::test(start_paused = true)]
async fn chunk_batch_is_held_until_acknowledged_then_flushed() {
    let view_radius = 1; // 3x3 = 9 columns
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

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
    });

    let mut client = Connection::new(client_end);
    // Deliberately does NOT ack the initial join batch — that unacknowledged
    // batch is exactly what should gate the next one.
    drive_login_and_join(&mut client, "Queued", 9).await;

    send_player_moved(&mut client, 160.0, 64.0, 0.0).await;
    let held = drain_available(&mut client).await;
    let (center, forgotten, added) = split_view_directives(&held);
    assert_eq!(center, Some((10, 0)), "the cache-center update is never gated");
    assert_eq!(forgotten, square(0, 0, view_radius), "forgets are never gated either");
    assert!(
        added.is_empty(),
        "the new columns must be queued, not sent, while the join batch is unacknowledged: {added:?}"
    );

    // Acknowledge the (still-outstanding) join batch. This is also the
    // signal that flushes the queued jump batch.
    send_chunk_batch_received(&mut client, 10.0).await;
    let flushed = drain_available(&mut client).await;
    let (center2, forgotten2, added2) = split_view_directives(&flushed);
    assert!(center2.is_none(), "the cache-center update already went out; must not repeat");
    assert!(forgotten2.is_empty(), "the forgets already went out; must not repeat");
    assert_eq!(
        added2,
        square(10, 0, view_radius),
        "acknowledging must flush exactly the queued batch"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// Issue #266's actual consumer for `SET_CREATIVE_MODE_SLOT`: a write to menu
/// slot 9 (main storage — native index 9 too, see
/// `PlayerInventory::apply_menu_slot_change`'s own table) must land in the
/// real `PlayerInventory` the connection closes with, not just decode
/// cleanly. No confirmation packet is expected either — see
/// `ServerBound::CreativeModeSlotSet`'s own doc comment for why (vanilla's
/// `handleSetCreativeModeSlot` sends none either, once the shift into
/// `AbstractContainerMenu::setRemoteSlot`/`broadcastChanges` is accounted
/// for — the client already predicted this write locally).
#[tokio::test(start_paused = true)]
async fn creative_mode_slot_write_lands_in_the_real_inventory() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Creator", 1).await;

    let stack = ItemStack::new("minecraft:diamond_block".parse().expect("valid resource key"), 12);
    send_creative_slot(&mut client, 9, Some(&stack)).await;
    let stray = drain_available(&mut client).await;
    assert!(
        stray.is_empty(),
        "a creative-slot write must not itself produce a reply: {stray:?}"
    );

    drop(client);
    let summary = server.await.unwrap().expect("clean close");
    assert_eq!(
        summary.inventory.native(9),
        Some(&stack),
        "menu slot 9 (main storage) must land at native index 9"
    );
}

/// **Control**: menu slot 0 (the crafting-result slot) has no native index
/// at all — `PlayerInventory::apply_menu_slot_change`'s own table drops it,
/// exactly as it already does for a real `CONTAINER_CLICK`. Proves the
/// creative-slot consumer really does route through that table rather than
/// writing every wire slot verbatim into some parallel array.
#[tokio::test(start_paused = true)]
async fn creative_mode_slot_write_to_the_crafting_output_is_dropped() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Crafter", 1).await;

    let stack = ItemStack::new("minecraft:diamond_block".parse().expect("valid resource key"), 1);
    send_creative_slot(&mut client, 0, Some(&stack)).await;
    let _ = drain_available(&mut client).await;

    drop(client);
    let summary = server.await.unwrap().expect("clean close");
    for i in 0..lodestone_server::PLAYER_NATIVE_SIZE {
        assert!(
            summary.inventory.native(i).is_none(),
            "slot 0 must not land anywhere in the native inventory, but native {i} is occupied"
        );
    }
}

/// Issue #270's `PERFORM_RESPAWN` consumer: once a player has actually died
/// (drowned to exactly `0.0` health, the same cadence
/// `submerged_player_loses_air_and_takes_drowning_damage_on_vanilla_cadence`
/// pins — 10 hits of 2.0 damage from 20.0, at tick 320 then every 20 ticks
/// after), a respawn request must refill both health and air on the real
/// connection, matching `PlayerVitals::respawn`'s own unit test.
#[tokio::test(start_paused = true)]
async fn respawn_after_death_refills_health_and_air() {
    let (client_end, server_end) = memory_pair();
    let source = WaterSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Drowned", 1).await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    // Drive the exact same cadence the drowning test above pins, until
    // health actually reaches zero (10 hits of 2.0 from 20.0).
    let health_after_death = loop {
        let (_air, health) = read_until_health_update(&mut client).await;
        if health <= 0.0 {
            break health;
        }
    };
    assert_eq!(health_after_death, 0.0, "expected exactly 10 hits to reach 0.0 health");

    send_client_command(&mut client, 0).await; // PERFORM_RESPAWN

    let payload = read_until(&mut client, SET_HEALTH_S2C).await;
    let mut r = Reader::new(&payload);
    assert_eq!(r.f32().expect("health"), 20.0, "respawn must restore full health");

    let payload2 = read_until(&mut client, AIR_SUPPLY_S2C).await;
    let mut r2 = Reader::new(&payload2);
    assert_eq!(r2.var_i32().expect("air"), 300, "respawn must restore full air");

    drop(client);
    let _ = server.await.unwrap();
}

/// **Control**: a respawn request from a player who is not dead must be a
/// no-op, mirroring vanilla's own `getHealth() > 0.0F` early return — proof
/// the health/air refill above is gated on death, not unconditional.
#[tokio::test(start_paused = true)]
async fn respawn_request_while_alive_is_a_no_op() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Alive", 1).await;

    send_client_command(&mut client, 0).await;
    let stray: Vec<_> = drain_available(&mut client)
        .await
        .into_iter()
        .filter(|(id, _)| *id == SET_HEALTH_S2C || *id == AIR_SUPPLY_S2C)
        .collect();
    assert!(
        stray.is_empty(),
        "a live player's respawn request must produce no health/air packet: {stray:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// Issue #270's other `client_command` ordinal: requesting current game-rule
/// values must reply — even with zero rules ever set — proving
/// `apply_client_command` actually calls through to
/// `ServerProtocol::encode_game_rule_values` rather than only doing so when
/// there happens to be something to report.
#[tokio::test(start_paused = true)]
async fn request_game_rule_values_replies_even_with_no_rules_set() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Curious", 1).await;

    send_client_command(&mut client, 2).await; // REQUEST_GAMERULE_VALUES

    let (id, payload) = client
        .read_packet_timeout(Duration::from_secs(5))
        .await
        .expect("read")
        .expect("game rule values reply");
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().expect("entry count"), 0, "no game rule was ever set");

    drop(client);
    let _ = server.await.unwrap();
}

/// Issue #270's `CLIENT_INFORMATION` consumer: a settings change mid-session
/// must resize the streamed view around the connection's own tracked
/// center — shrinking forgets exactly the outer ring, and growing back
/// (clamped at the server's own configured cap, not the client's raw
/// request) re-sends exactly that same ring.
#[tokio::test(start_paused = true)]
async fn client_information_view_distance_resizes_the_streamed_view() {
    let view_radius = 2; // 5x5 = 25 columns — this connection's configured cap
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

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
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Settings", 25).await;
    // Clear the join batch's own outstanding ack first — this test is about
    // the resize diff, not the flow-control gate (see
    // `chunk_batch_is_held_until_acknowledged_then_flushed` for that).
    send_chunk_batch_received(&mut client, 10.0).await;

    let full = square(0, 0, 2);
    let inner = square(0, 0, 1);
    let ring: HashSet<(i32, i32)> = full.difference(&inner).copied().collect();
    assert_eq!(ring.len(), 16, "sanity: the 5x5 minus 3x3 ring is 16 columns");

    // Shrink to 1: never gated at all (there is nothing to add, only
    // forgets — see `send_view_update`'s own doc comment for why forgets
    // bypass the ack gate entirely).
    send_client_information(&mut client, 1).await;
    let shrunk = drain_available(&mut client).await;
    let (center, forgotten, added) = split_view_directives(&shrunk);
    assert!(center.is_none(), "a settings change never moves the tracked center");
    assert_eq!(forgotten, ring, "shrinking must forget exactly the outer ring");
    assert!(added.is_empty(), "shrinking must never add a column");

    // Grow back past the server's own cap (10 requested, 2 configured) —
    // must clamp to the cap, not the raw requested value.
    send_client_information(&mut client, 10).await;
    let grown = drain_available(&mut client).await;
    let (center2, forgotten2, added2) = split_view_directives(&grown);
    assert!(center2.is_none());
    assert!(forgotten2.is_empty(), "growing must never forget a column");
    assert_eq!(
        added2, ring,
        "clamped growth must re-send exactly the same ring, not the raw requested radius"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The island gate for issue #441's player feed.**
///
/// Every other gate for the perception feed calls `MobSim::set_players`
/// *itself*, so all of them would pass with no producer anywhere — which is
/// exactly the state `nearest_player`/`temptation` were in before this: seam
/// present, feed present, nothing calling it. This test never touches
/// `set_players`. It drives a real `PLAYER_MOVED` packet through
/// `serve_connection` and asserts the perception arrived, so it fails if the one
/// line in `dispatch_play_packet`'s `PlayerMoved` arm is ever removed.
///
/// Note the `MobHandle` is a real one over a real `ChunkWorld` holding a real
/// mob, not the `MobHandle::default()` every other test in this file uses — the
/// default is an empty sim, which cannot show a mob's perception changing.
#[tokio::test(start_paused = true)]
async fn a_player_moved_packet_feeds_mob_perception_through_the_real_connection() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    // Flat floor with its surface at y=0, and one cow standing on it.
    let mut world = ChunkWorld::new(-4, 24);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    let mobs = MobHandle::new(world);
    let cow_id = mobs.with(|sim| {
        sim.spawn_species(
            lodestone_model::ResourceKey::from_str("minecraft:cow").expect("valid key"),
            lodestone_model::Vec3::new(0.0, 0.0, 0.0),
        )
        .id()
    });

    // Control, before any movement packet: the sim knows of no players, so the
    // cow's perception is empty. Without this the assertions below could be
    // satisfied by a value that was always there.
    mobs.with(MobSim::tick);
    assert_eq!(
        mobs.with(|sim| sim.players().len()),
        0,
        "precondition: no players known before a PLAYER_MOVED arrives"
    );
    assert_eq!(
        mobs.with(|sim| sim.get(cow_id).expect("alive").nearest_player()),
        None,
        "precondition: the cow must perceive no player yet — this is the state \
         LookAtPlayerGoal and TemptGoal were permanently stuck in"
    );

    let conn_mobs = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &conn_mobs,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Perceived", 1).await;

    // Put wheat in the selected hotbar slot, then move. Wheat because
    // `cow_food` is exactly `[wheat]`
    // (`.cache/mc/26.2/src/data/minecraft/tags/item/cow_food.json`), so this
    // proves the *held item* crossed the connection too, not just the position.
    // Slot 36 is the first hotbar slot in the player's own menu indexing.
    let wheat = ItemStack::new(
        lodestone_model::ResourceKey::from_str("minecraft:wheat").expect("valid key"),
        1,
    );
    send_creative_slot(&mut client, 36, Some(&wheat)).await;
    send_player_moved(&mut client, 4.0, 0.0, 0.0).await;
    let _ = drain_available(&mut client).await;

    // The producer runs inside the packet handler, so the value is present
    // without needing a tick; a tick is what pushes it into the mob.
    assert_eq!(
        mobs.with(|sim| sim.players().len()),
        1,
        "a PLAYER_MOVED packet must register the player with the mob sim"
    );
    mobs.with(MobSim::tick);

    let cow_sees = mobs.with(|sim| {
        let cow = sim.get(cow_id).expect("alive");
        (cow.nearest_player(), cow.temptation())
    });
    assert_eq!(
        cow_sees.0,
        Some(lodestone_model::Vec3::new(4.0, 0.0, 0.0)),
        "the cow must perceive the player at the position the packet carried"
    );
    assert_eq!(
        cow_sees.1,
        Some(lodestone_model::Vec3::new(4.0, 0.0, 0.0)),
        "and must be tempted, because the held wheat crossed the wire into \
         PlayerPerception::held_item — if this is None but nearest_player is \
         Some, the position is being fed and the inventory read is not"
    );

    drop(client);
    let _ = server.await;
}

// ---------------------------------------------------------------------------
// Issue #453: the join view must arrive nearest-first, encoded as it is
// generated — not raster-order from the far corner after all of it exists.
// ---------------------------------------------------------------------------

/// A column source that counts how many columns have been generated so far, over
/// terrain a world spawn can actually be found in.
///
/// The count is the load-bearing half of this gate. Ordering alone is not
/// enough: generating all 361 columns and *then* encoding them nearest-first
/// would satisfy every ordering assertion while leaving time-to-first-chunk
/// exactly as bad as before. So [`ProbeProto`] stamps this counter into each
/// chunk packet, and the assertion is about **how much had been generated when
/// the player's own column reached the wire**.
///
/// # Why there is a floor, and why that is not a convenience
///
/// This served bare air until issue #329 gave `serve_connection` a real world
/// spawn *search* ahead of the ring loop. Against an air column
/// `get_level_respawn_pos` finds no solid block anywhere, so every one of the
/// spiral's 121 candidates is invalid and the search walks the whole ±5-chunk box
/// before a single chunk is encoded — measured at **123** columns before the first
/// encode (1 fallback query + 121 spiral + ring 0), which reads exactly like the
/// pre-#453 "generate everything first" defect and is not it.
///
/// So the air fixture was a *world*-species vacuity in the making: it exercised
/// the pathological invalid-origin path, not the one a joining player takes. A
/// solid layer at `y = 8` makes the origin chunk a valid spawn candidate, which is
/// what every real world presents, and the bound in
/// [`check_proximity_stream`] is then about the ring loop again rather than about
/// the spawn search.
struct CountingAirSource {
    generated: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// The Y of [`CountingAirSource`]'s floor. Inside the column's `0..16` extent,
/// and clear of the top so `get_level_respawn_pos`'s downward scan reaches it
/// through air rather than being aborted by a fluid.
const FLOOR_Y: i32 = 8;

impl ChunkSource for CountingAirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.generated
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut column = ChunkColumn::new(0, 16);
        for lx in 0..16 {
            for lz in 0..16 {
                column.set_block(lx, FLOOR_Y, lz, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this fixture
        // counts generations, not reads.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// [`FakeProtocol`] with one method changed: `encode_chunk` appends the
/// generation counter's current value to the packet, so the test can read
/// "columns generated by the time this chunk was encoded" straight off the wire
/// rather than inferring it.
///
/// Every other method forwards to `FakeProtocol` explicitly rather than relying
/// on the trait defaults — fifteen of them have defaults that silently answer
/// `ServerDirective::None`, which is exactly the failure mode
/// `protocol.rs`'s own boxed-protocol test exists to catch.
struct ProbeProto {
    generated: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ServerProtocol for ProbeProto {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        FakeProtocol.decode(state, packet_id, payload)
    }
    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        FakeProtocol.login_success(username, uuid)
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        FakeProtocol.begin_configuration()
    }
    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        FakeProtocol.begin_play(view_radius)
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        FakeProtocol.begin_chunk_batch()
    }
    fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        w.var_i32(
            self.generated
                .load(std::sync::atomic::Ordering::SeqCst)
                .try_into()
                .expect("generation count fits in i32"),
        );
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: w.as_slice().to_vec(),
        }
    }
    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        FakeProtocol.end_chunk_batch(batch_size)
    }
    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        FakeProtocol.encode_keep_alive(id)
    }
    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        FakeProtocol.encode_set_time(game_time, day_time)
    }
    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        FakeProtocol.encode_chunk_cache_center(cx, cz)
    }
    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        FakeProtocol.encode_forget_chunk(cx, cz)
    }
    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        FakeProtocol.encode_air_supply_update(air)
    }
    fn encode_set_health(&self, health: f32) -> ServerDirective {
        FakeProtocol.encode_set_health(health)
    }
    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        FakeProtocol.encode_change_difficulty(difficulty, locked)
    }
    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        FakeProtocol.encode_game_rule_values(entries)
    }
}

/// Chebyshev (chess-king) distance from the join centre, `(0, 0)`.
fn chebyshev(cx: i32, cz: i32) -> i32 {
    cx.abs().max(cz.abs())
}

/// The detector, factored out so the **same** code judges the real join and the
/// synthesised pre-fix sequence below.
///
/// `observed` is `(cx, cz, columns_generated_when_this_was_encoded)` in wire
/// order. Returns the first violation as an `Err`, so a failure names *which*
/// entry broke *which* rule rather than reporting a bare fraction.
///
/// Three rules, each aimed at one of issue #453's three compounding orderings:
///
/// 1. the player's own column `(0, 0)` is encoded **first** — it used to be item
///    ~180 of 361;
/// 2. Chebyshev distance from the centre never decreases — terrain grows
///    outward from the player instead of inward from a corner;
/// 3. the first chunk was encoded after **at most two columns** of
///    generation — the "generate everything, then encode" half.
///
/// Rule 3's bound is `2`, not `1`, and the two are itemised rather than rounded:
///
/// | column | why |
/// |---|---|
/// | 1 | the world spawn search (#329/#461) resolves the origin column's surface before the batch opens, and reuses that one column for the spiral's `(0, 0)` candidate — `world_spawn::a_valid_origin_column_is_generated_exactly_once` |
/// | 2 | ring 0 asks the source for the same column again; the fixture has no `ChunkStore`, so it is a second generation |
///
/// So 2 is the player's own column plus one infra query, not the full view, and
/// the wrong hypothesis is 361.
///
/// Two ways this bound has been wrong, both worth keeping because both fail in the
/// *safe*-looking direction — a number just over the bound reads as a mild
/// ordering regression:
///
/// * before the origin-column reuse landed, the search generated `(0, 0)` twice,
///   making the honest figure 3 and this bound unreachable;
/// * with an all-air fixture the search finds no valid spawn anywhere and walks
///   all 121 spiral candidates first, for a figure of **123** — which looks like
///   issue #453 undone and is not. [`CountingAirSource`] has a floor for exactly
///   that reason.
fn check_proximity_stream(observed: &[(i32, i32, usize)], view_radius: i32) -> Result<(), String> {
    let expected_total = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    if observed.len() != expected_total {
        return Err(format!(
            "expected {expected_total} columns, got {}",
            observed.len()
        ));
    }

    let (cx, cz, generated_at_first) = observed[0];
    if (cx, cz) != (0, 0) {
        return Err(format!(
            "the player's own column must be encoded first; got ({cx}, {cz}) at Chebyshev \
             distance {}",
            chebyshev(cx, cz)
        ));
    }
    if generated_at_first > 2 {
        return Err(format!(
            "the first chunk must be encoded after at most 2 columns of generation \
             (1 for the spawn-surface query plus ring 0); \
             {generated_at_first} columns had already been generated, meaning the whole view \
             is generated before anything is encoded"
        ));
    }

    let mut previous = 0;
    for &(cx, cz, _) in observed {
        let distance = chebyshev(cx, cz);
        if distance < previous {
            return Err(format!(
                "wire order must be non-decreasing in Chebyshev distance from the centre; \
                 ({cx}, {cz}) at distance {distance} follows a column at distance {previous}"
            ));
        }
        previous = distance;
    }

    Ok(())
}

/// **Issue #453's gate.** A real join must stream the view outward from the
/// player's own column, encoding each ring as it is generated.
///
/// `view_radius = 9` deliberately — the shell's own singleplayer value, so this
/// measures the 361-column configuration a player actually joins with rather
/// than a convenient small one.
#[tokio::test]
async fn join_streams_the_view_outward_from_the_players_own_column() {
    let view_radius = 9;
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let (client_end, server_end) = memory_pair();
    let source = CountingAirSource {
        generated: std::sync::Arc::clone(&generated),
    };
    let proto = ProbeProto {
        generated: std::sync::Arc::clone(&generated),
    };

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &proto,
            &source,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("Spiral");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (id, _payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, SET_TIME_S2C);
    let (id, _payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, CHUNK_BATCH_START);

    let mut observed = Vec::with_capacity(expected_chunks);
    for _ in 0..expected_chunks {
        let (id, payload) = client.read_packet().await.unwrap().unwrap();
        assert_eq!(id, CHUNK);
        let mut r = Reader::new(&payload);
        let cx = r.var_i32().unwrap();
        let cz = r.var_i32().unwrap();
        let at = r.var_i32().unwrap() as usize;
        observed.push((cx, cz, at));
    }
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, CHUNK_BATCH_FINISHED);
    assert_eq!(
        Reader::new(&payload).var_i32().unwrap(),
        expected_chunks as i32,
        "still exactly one batch covering the whole view, one ring at a time"
    );

    check_proximity_stream(&observed, view_radius).expect("join must stream nearest-first");

    // The set is unchanged, not merely reordered: every column of the square
    // exactly once. A ring enumeration that dropped or duplicated a column
    // would still satisfy the ordering rules above.
    let sent: HashSet<(i32, i32)> = observed.iter().map(|&(cx, cz, _)| (cx, cz)).collect();
    assert_eq!(
        sent,
        square(0, 0, view_radius),
        "the ring walk must cover the same square the raster walk did, with no gaps or repeats"
    );

    drop(client);
    let _ = server.await;
}

/// **The control, and it must fail the assertion above.**
///
/// This synthesises the sequence the *pre-fix* code produced — raster order from
/// `(-view_radius, -view_radius)`, with the whole view already generated before
/// the first encode — and requires [`check_proximity_stream`] to reject it.
///
/// It is written out as a literal `cz`-outer/`cx`-inner walk rather than
/// described, because that walk *was* the defect: `serve_connection` built
/// exactly this `Vec`, awaited one `generate` over all of it, and only then
/// began encoding. A detector that passes this is measuring nothing, and the
/// specific violations it must catch are named below so a future change that
/// weakens one rule cannot quietly go green on the others.
#[test]
fn control_the_old_raster_order_fails_the_proximity_assertion() {
    let view_radius = 9;
    let raster: Vec<(i32, i32)> = (-view_radius..=view_radius)
        .flat_map(|cz| (-view_radius..=view_radius).map(move |cx| (cx, cz)))
        .collect();
    let total = raster.len();
    assert_eq!(total, 361, "the shell's own join view is 361 columns");

    // Every column reports the *full* view as already generated, because that
    // is what "generate all 361, then encode" means.
    let observed: Vec<(i32, i32, usize)> = raster
        .iter()
        .map(|&(cx, cz)| (cx, cz, total))
        .collect();

    let verdict = check_proximity_stream(&observed, view_radius);
    let message = verdict.expect_err(
        "the pre-#453 raster order must be rejected; if this passes, the detector in \
         check_proximity_stream is vacuous and the gate beside it proves nothing",
    );
    assert!(
        message.contains("must be encoded first"),
        "the control must be caught on the player's-own-column rule first, got: {message}"
    );

    // The distance rule must fire independently of the first-column rule, so a
    // future relaxation of one cannot silently disarm the other. Rotate the
    // raster walk so it *starts* at (0, 0) and check it is still rejected.
    let centre = raster
        .iter()
        .position(|&c| c == (0, 0))
        .expect("the centre column is in the view");
    assert_eq!(
        centre, 180,
        "the player's own column really was item ~180 of 361 in raster order"
    );
    let mut rotated: Vec<(i32, i32, usize)> = vec![(0, 0, 1)];
    rotated.extend(
        raster
            .iter()
            .filter(|&&c| c != (0, 0))
            .map(|&(cx, cz)| (cx, cz, total)),
    );
    let message = check_proximity_stream(&rotated, view_radius)
        .expect_err("raster order after the centre is still not distance-ordered");
    assert!(
        message.contains("non-decreasing in Chebyshev distance"),
        "the distance rule must be what rejects this one, got: {message}"
    );
}

/// **The same three rules, over the arm production actually runs.**
///
/// [`join_streams_the_view_outward_from_the_players_own_column`] calls
/// `serve_connection`, which resolves to `SourceRef::Borrowed`. Every production
/// caller in `crate::integrated` resolves to `SourceRef::Shared`, and the ring loop
/// **branches on that**: `Borrowed` awaits one `generate_columns_parallel` per ring,
/// while `Shared` spawns each of the ring's columns into the blocking pool
/// individually and awaits the handles in ring order. Two different loop bodies,
/// one of them untested — the `world` species of vacuous test (`DESIGN.md` §12.43),
/// where the source is exemplary and the flaw is which implementation the test's
/// transport resolves to.
///
/// `serve_connection_shared` is `pub(crate)` and deliberately not re-exported, so
/// this reaches the arm the way the shell does: through the public
/// `IntegratedServer::bind`, over a real loopback socket.
///
/// # Two differences from the `Borrowed` gate, both deliberate
///
/// `bind` wraps the source in a [`crate::ChunkStore`], so ring 0 is a **cache hit**
/// on the column the world-spawn search already generated and
/// `generated_at_first` is `1` rather than `2`. [`check_proximity_stream`]'s bound
/// is `<= 2`, which covers both; asserting `1` here would be pinning the store's
/// presence, which `chunk_store.rs` already owns.
///
/// `bind` also spawns `run_tick_loop` over a 5×5 tick area, which would inflate the
/// counter — except that issue #481 defers its first random-tick pass for 40 ticks
/// (2.0 s), and a 361-column air view served from a store completes long before
/// that. Rule 3 reads only `observed[0]`, so even a slow run cannot be corrupted by
/// tick-loop generation that happens after the first encode.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shared_arm_streams_the_view_outward_too() {
    let view_radius = 9;
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let server = lodestone_server::IntegratedServer::bind(
        "127.0.0.1:0",
        ProbeProto {
            generated: std::sync::Arc::clone(&generated),
        },
        CountingAirSource {
            generated: std::sync::Arc::clone(&generated),
        },
        view_radius,
    )
    .await
    .expect("bind loopback");
    let addr = server.local_addr().expect("a bound server has an address");

    let mut client = Connection::new(
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("client connects"),
    );
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("SharedSpiral");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (id, _) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, SET_TIME_S2C);
    let (id, _) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, CHUNK_BATCH_START);

    let mut observed = Vec::with_capacity(expected_chunks);
    for _ in 0..expected_chunks {
        let (id, payload) = client.read_packet().await.unwrap().unwrap();
        assert_eq!(id, CHUNK);
        let mut r = Reader::new(&payload);
        let cx = r.var_i32().unwrap();
        let cz = r.var_i32().unwrap();
        let at = r.var_i32().unwrap() as usize;
        observed.push((cx, cz, at));
    }
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, CHUNK_BATCH_FINISHED);
    assert_eq!(
        Reader::new(&payload).var_i32().unwrap(),
        expected_chunks as i32,
        "still exactly one batch covering the whole view on this arm too"
    );

    check_proximity_stream(&observed, view_radius)
        .expect("the Shared arm must stream nearest-first as well");

    let sent: HashSet<(i32, i32)> = observed.iter().map(|&(cx, cz, _)| (cx, cz)).collect();
    assert_eq!(
        sent,
        square(0, 0, view_radius),
        "the Shared arm's per-column spawn_blocking fan-out must cover the same square, \
         with no gaps or repeats — awaiting the handles out of ring order would show up here"
    );

    drop(client);
    server.shutdown().await;
}

/// Issue #324 / `docs/plans/world-state.md` W1, gate (c)'s serve half:
/// `serve_play`'s `container_sync_tick` arm must actually drain the
/// [`WeatherFeed`] and turn each transition into an `encode_game_event`
/// broadcast — the piece with no inbound packet driving it, exactly like the
/// block-tick and explosion drains it sits beside. The feed is published to
/// directly here (the loop that fills it in production is gated in `crate::tick`'s
/// own module); a failure means the drain is missing or miswired, not the
/// weather machine.
#[tokio::test(start_paused = true)]
async fn weather_feed_transitions_reach_the_client_as_game_event_bytes() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
    let weather = WeatherFeed::default();
    // A second handle for the server task, so this test can keep publishing
    // into the feed after the spawn (a `WeatherFeed` is an `Arc`-backed
    // shared buffer — cloning the handle is not cloning the events).
    let server_weather = weather.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection_with_mob_events(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &server_weather,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Stormwatcher", 1).await;

    // Publish a rain ramp, a thunder ramp, then a rain flip, and wait one
    // `CONTAINER_SYNC_INTERVAL` (50 ms — the timer the drain rides) for the
    // arm to fire. `start_paused` freezes the clock except for explicit
    // advances, so one 50 ms advance is exactly one interval tick and the
    // three events all drain on it, in publish order.
    weather.publish(WeatherEvent::RainLevelChanged(0.5));
    weather.publish(WeatherEvent::ThunderLevelChanged(0.0));
    weather.publish(WeatherEvent::StartRaining);
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    // The three transitions arrive as three `GAME_EVENT_S2C` frames, each
    // carrying the exact `(event, param)` pair the real v770 `GAME_EVENT`
    // packet would carry (ids 7/8/1) — asserted on bytes, not on "a packet
    // arrived".
    let mut seen = Vec::new();
    while seen.len() < 3 {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        assert_eq!(id, GAME_EVENT_S2C, "expected only game events, got id {id}");
        let mut r = Reader::new(&payload);
        seen.push((r.u8().expect("event id"), r.f32().expect("param")));
    }
    assert_eq!(
        seen,
        vec![(7, 0.5), (8, 0.0), (1, 0.0)],
        "the drain must forward each transition in publish order with its wire pair"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// A [`ChunkSource`] that records every coordinate it is asked to generate.
///
/// Exists for [`generation_is_anchored_at_the_player_not_at_the_origin`]: the
/// owner's own hypothesis for the walk-away-from-spawn slowdown was that the
/// server enumerates from `(0, 0)` outward rather than from the player, so
/// walking further makes each recenter do more work. That is a claim about
/// *which coordinates are generated*, and nothing that counts columns can answer
/// it — only something that records the coordinates themselves.
struct RecordingSource {
    seen: std::sync::Arc<std::sync::Mutex<Vec<(i32, i32)>>>,
}

impl ChunkSource for RecordingSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.seen.lock().expect("recording lock").push((cx, cz));
        let mut column = ChunkColumn::new(0, 16);
        for lx in 0..16 {
            for lz in 0..16 {
                column.set_block(lx, 8, lz, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // Deliberately does NOT record: only whole-column generation is the
        // subject, and a spawn-position probe reading single blocks near the
        // origin would otherwise show up as origin-anchored generation and make
        // this test lie in the alarming direction.
        let _ = (x, y, z);
        if y == 8 { "minecraft:stone".to_string() } else { "minecraft:air".to_string() }
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design (issue #440: explicit).
    }
}

/// **Generation is anchored at the player, not at `(0, 0)`** — instrumented on
/// the real `serve_connection` path at a coordinate 80,000 blocks from spawn.
///
/// This is the direct falsification of the owner's hypothesis for the
/// "exponentially slower as I walk away from spawn" report.
/// [`player_moved_streams_view_across_several_chunk_boundaries`] already gates
/// the exact-diff shape, but it does so at chunk 10–11 (160 blocks), which is
/// close enough to the origin that an origin-anchored enumeration and a
/// player-anchored one would produce sets of similar size. At chunk 5,000 the two
/// hypotheses differ by five orders of magnitude in column count, so the
/// measurement is unambiguous.
///
/// The assertion is on the recorded coordinate *set*, and it has two halves,
/// because either alone is satisfiable by a wrong implementation:
///
/// * every column generated for the far recenter lies within the view radius of
///   the player — an origin-anchored spiral would generate columns near `(0, 0)`;
/// * the *count* is exactly the window size — an implementation that generated
///   the whole rectangle between the origin and the player would satisfy the
///   first half for its final columns while doing 5,000× the work.
///
/// The count half is the one that matters, and it is a magnitude assertion, not
/// a direction: the expected value is derived from the view radius (`(2r+1)²`)
/// rather than read off a measurement.
#[tokio::test]
async fn generation_is_anchored_at_the_player_not_at_the_origin() {
    let view_radius = 1; // 3x3 = 9 columns
    let (client_end, server_end) = memory_pair();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let source = RecordingSource {
        seen: std::sync::Arc::clone(&seen),
    };

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
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "FarWalker", 9).await;
    send_chunk_batch_received(&mut client, 10.0).await;

    // Everything generated for the join is not this test's subject. Clear it so
    // the far recenter's own coordinate set is read in isolation.
    seen.lock().expect("recording lock").clear();

    // Chunk (5000, 0) — 80,000 blocks out, the scale the owner's report is about.
    const FAR_CHUNK: i32 = 5000;
    send_player_moved(&mut client, f64::from(FAR_CHUNK) * 16.0, 64.0, 0.0).await;
    let far = drain_available(&mut client).await;

    let (center, _forgotten, added) = split_view_directives(&far);
    assert_eq!(
        center,
        Some((FAR_CHUNK, 0)),
        "the cache centre must follow the player"
    );
    assert_eq!(
        added,
        square(FAR_CHUNK, 0, view_radius),
        "exactly the player's own window must be sent"
    );

    let recorded = seen.lock().expect("recording lock").clone();

    // Half one: nothing outside the player's window was generated at all.
    let window = square(FAR_CHUNK, 0, view_radius);
    let strays: Vec<(i32, i32)> = recorded
        .iter()
        .copied()
        .filter(|pos| !window.contains(pos))
        .collect();
    assert!(
        strays.is_empty(),
        "generation is not anchored at the player: {} of {} generated columns lie outside \
         the player's window, e.g. {:?}",
        strays.len(),
        recorded.len(),
        &strays[..strays.len().min(8)]
    );

    // Half two, the magnitude: the expected count comes from the view radius,
    // not from a measurement. An origin-anchored enumeration would be ~5,000x
    // this; a rectangle-fill between origin and player, ~10,000x.
    let expected = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    assert_eq!(
        recorded.len(),
        expected,
        "a recenter 80,000 blocks out generated {} columns; a player-anchored window \
         is exactly {expected}",
        recorded.len()
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}
