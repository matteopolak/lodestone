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
use lodestone_entity::item_entity::ItemLifecycle;
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, Difficulty, ItemStack, ResourceKey, Vec3,
};
use lodestone_net::{Connection, NetError, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, BlockTickFeed, ChunkColumn, ChunkSource, ChunkWorld, EntitySnapshot,
    ExplosionFeed, MetadataField, MobHandle, MobSim, NoEntities, ServerBound, ServerDirective,
    ServerError, ServerProtocol,
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
/// Issue #336's refusal, so the disconnect reason is readable from the client side.
const DISCONNECT_S2C: i32 = 90;
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
/// Issue #337: this stand-in format's block-break packet (a destroy ordinal
/// plus x/y/z) and the server-initiated window-slot write a pickup produces.
const BLOCK_ACTION_C2S: i32 = 56;
const CONTAINER_SET_SLOT_S2C: i32 = 57;
/// The three ids the pickup-animation gate reads. `TAKE_ITEM_ENTITY_S2C` has to be
/// observable *relative to* `REMOVE_ENTITIES_S2C`, which is the whole point of the
/// gate, so the entity stream needs wire ids of its own in this stand-in vocabulary.
const ADD_ENTITY_S2C: i32 = 58;
/// `lodestone_server`'s own `LOCAL_PLAYER_ENTITY_ID`, which is `pub(crate)`. The
/// collector id a singleplayer connection reports is this, and the client lerps the
/// item toward whatever id the take names.
const LOCAL_PLAYER_ENTITY_ID: i32 = 1;
const REMOVE_ENTITIES_S2C: i32 = 59;
const TAKE_ITEM_ENTITY_S2C: i32 = 60;
/// Stand-in `set_passengers`: a VarInt vehicle id then a VarInt-prefixed
/// VarInt array, matching the real wire shape `encode_set_passengers`'s own
/// doc comment describes. Dismounting is this packet with an empty array.
const SET_PASSENGERS_S2C: i32 = 61;
/// A stand-in `change_game_mode` (one byte ordinal). Needed because
/// `SET_CREATIVE_MODE_SLOT` is gated on creative mode server-side — vanilla's own
/// `hasInfiniteMaterials()` check — so a test that gives itself an item has to be
/// in creative for the write, exactly as a real client would be.
const CHANGE_GAME_MODE_C2S: i32 = 58;
/// A stand-in `set_game_rule` (issue #327): one VarInt entry count, then a
/// key/value string pair each.
const SET_GAME_RULE_C2S: i32 = 59;

/// A stand-in `player_input`: one byte, non-zero meaning sprinting. The real
/// v770 packet is a bitfield of movement flags; this file tests
/// `lodestone-server`'s own consumer logic, and the sprint bit is the only one it
/// reads (hunger's per-block exhaustion, `crate::food`).
const PLAYER_INPUT_C2S: i32 = 60;

/// A stand-in `ping_request`: one big-endian `i64`, matching the real
/// packet's only field. `dispatch_play_packet`'s `PingRequest` arm calls
/// `encode_pong_response`, so this exercises the dispatch-and-consumer half
/// of that wiring — the wire-shape half is covered separately, against the
/// real `V770ServerProtocol`, in `crates/protocol/v770/src/server_protocol.rs`'s
/// own `play_ping_request_tests`.
const PING_REQUEST_C2S: i32 = 91;
/// `ClientboundPongResponsePacket`'s stand-in — the `time` echoed back
/// unchanged.
const PONG_RESPONSE_S2C: i32 = 92;

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

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
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

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
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
            State::Play if packet_id == PLAYER_INPUT_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::PlayerInput {
                    sprint: r.u8().expect("sprint flag") != 0,
                    shift: r.u8().expect("shift flag") != 0,
                    jump: r.u8().expect("jump flag") != 0,
                }
            }
            State::Play if packet_id == SET_GAME_RULE_C2S => {
                let mut r = Reader::new(payload);
                let count = r.var_i32().expect("entry count");
                let mut entries = Vec::new();
                for _ in 0..count {
                    let key = r.string(64).expect("rule key");
                    let value = r.string(64).expect("rule value");
                    entries.push((key, value));
                }
                ServerBound::GameRuleChanged { entries }
            }
            State::Play if packet_id == CHANGE_GAME_MODE_C2S => {
                let mut r = Reader::new(payload);
                let mode = match r.u8().expect("game mode ordinal") {
                    1 => lodestone_model::GameMode::Creative,
                    2 => lodestone_model::GameMode::Adventure,
                    3 => lodestone_model::GameMode::Spectator,
                    _ => lodestone_model::GameMode::Survival,
                };
                ServerBound::ChangeGameMode { mode }
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
            // Issue #337: a minimal stand-in for `PLAYER_ACTION`'s three
            // destroy ordinals — same "test the server's own consumer logic,
            // not wire fidelity" rationale as `CHANGE_DIFFICULTY_C2S`. The
            // ordinals match vanilla's (`START_DESTROY_BLOCK` 0,
            // `ABORT_DESTROY_BLOCK` 1, `STOP_DESTROY_BLOCK` 2) because
            // `apply_block_action`'s behaviour is defined in terms of them.
            State::Play if packet_id == BLOCK_ACTION_C2S => {
                let mut r = Reader::new(payload);
                let action = match r.u8().expect("destroy ordinal") {
                    0 => BlockActionKind::StartDestroy,
                    1 => BlockActionKind::AbortDestroy,
                    _ => BlockActionKind::StopDestroy,
                };
                ServerBound::BlockAction {
                    action,
                    pos: BlockPos::new(
                        r.i32().expect("x"),
                        r.i32().expect("y"),
                        r.i32().expect("z"),
                    ),
                    face: BlockFace::Up,
                    sequence: 0,
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

    /// Issue #336: the login refusal has to reach the wire for its reason to be
    /// assertable at all. The stand-in's own id, like every other here.
    fn encode_disconnect(&self, _state: State, reason: &lodestone_model::Text) -> ServerDirective {
        let mut w = Writer::default();
        w.string(&reason.to_plain_string());
        ServerDirective::Send {
            packet_id: DISCONNECT_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    /// The default trait method emits `ServerDirective::None` (see its own
    /// doc comment) — overridden here so `ping_request_gets_a_pong_response`
    /// below has an actual wire reply to observe, exactly like
    /// `encode_keep_alive` above.
    fn encode_pong_response(&self, time: i64) -> ServerDirective {
        let mut w = Writer::default();
        w.i64(time);
        ServerDirective::Send {
            packet_id: PONG_RESPONSE_S2C,
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

    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.f32(health);
        w.var_i32(food);
        w.f32(saturation);
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
        // Issue #327: the entries themselves, not just the count. The count alone
        // cannot tell a rule that was *validated and stored* from one echoed back
        // verbatim, which is the whole distinction the gate below tests.
        let mut w = Writer::default();
        w.var_i32(entries.len() as i32);
        for (key, value) in entries {
            w.string(key);
            w.string(value);
        }
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

    /// Issue #337: a pickup tells the client which window-0 slot changed. This
    /// stand-in carries `(slot, present, item key, count)` — enough for the
    /// pickup gate to assert *which* slot was announced and what landed in it,
    /// which is the part `lodestone-server` decides. The real wire layout is
    /// `v770`'s concern.
    fn encode_container_slot(
        &self,
        window_id: i32,
        _state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(window_id);
        w.var_i32(slot);
        w.bool(item.is_some());
        if let Some(item) = item {
            w.string(&item.item.to_string());
            w.var_i32(i32::try_from(item.count).unwrap_or(i32::MAX));
        }
        ServerDirective::Send {
            packet_id: CONTAINER_SET_SLOT_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    // The three entity encoders below exist so a pickup's *ordering* is observable
    // from the wire. They are inert for every other test in this file: all of them
    // pass `&NoEntities`, which yields no snapshots, so `stream_pass` produces
    // nothing and these are never called.
    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(entity.id);
        ServerDirective::Send {
            packet_id: ADD_ENTITY_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(i32::try_from(ids.len()).unwrap_or(i32::MAX));
        for &id in ids {
            w.var_i32(id);
        }
        ServerDirective::Send {
            packet_id: REMOVE_ENTITIES_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_take_item_entity(
        &self,
        item_entity_id: i32,
        collector_entity_id: i32,
        amount: i32,
    ) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(item_entity_id);
        w.var_i32(collector_entity_id);
        w.var_i32(amount);
        ServerDirective::Send {
            packet_id: TAKE_ITEM_ENTITY_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(vehicle_id);
        w.var_i32(i32::try_from(passenger_ids.len()).unwrap_or(i32::MAX));
        for &id in passenger_ids {
            w.var_i32(id);
        }
        ServerDirective::Send {
            packet_id: SET_PASSENGERS_S2C,
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

    // The view arrives across **one or more** batches: the innermost
    // `JOIN_PRESTREAM_RADIUS` rings go out before the play loop starts and the
    // rest streams from it in `JOIN_STREAM_BATCH_COLUMNS`-sized batches, so a
    // single begin/…/end pair only happens for a view small enough to fit in the
    // pre-stream. What every caller of this helper needs is unchanged: the whole
    // view has arrived, accounted for by markers, before the test proper begins.
    let batches = drain_join_view(client, expected_chunks).await;
    assert_eq!(
        batches.iter().sum::<i32>(),
        expected_chunks as i32,
        "the join view's batch markers must account for exactly {expected_chunks} columns"
    );
}

/// Reads `expected_chunks` `CHUNK` packets and the batch markers around them,
/// returning each marker's reported size. Skips anything else the server sends
/// while the view is streaming.
///
/// The payload-blind counterpart of [`collect_join_chunks`], for the `FakeProtocol`
/// clients whose chunk packets carry no generation counter to read.
async fn drain_join_view<T: Transport>(
    client: &mut Connection<T>,
    expected_chunks: usize,
) -> Vec<i32> {
    let mut batches = Vec::new();
    let mut seen = 0usize;
    let mut in_batch = 0usize;
    while seen < expected_chunks {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        if id == CHUNK_BATCH_START {
            assert!(payload.is_empty());
            in_batch = 0;
        } else if id == CHUNK {
            seen += 1;
            in_batch += 1;
        } else if id == CHUNK_BATCH_FINISHED {
            let reported = Reader::new(&payload).var_i32().unwrap();
            assert_eq!(reported as usize, in_batch, "batch marker/packet mismatch");
            batches.push(reported);
        }
    }
    // The marker closing the batch the last column landed in.
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_FINISHED, "the last batch must be closed");
    let reported = Reader::new(&payload).var_i32().unwrap();
    assert_eq!(reported as usize, in_batch);
    batches.push(reported);
    batches
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

/// Sends one block-break phase (issue #337). `ordinal` is vanilla's destroy
/// ordinal — `0` start, `1` abort, `2` stop.
async fn send_block_action(
    client: &mut Connection<DuplexStream>,
    ordinal: u8,
    pos: BlockPos,
) {
    let mut w = Writer::default();
    w.u8(ordinal);
    w.i32(pos.x);
    w.i32(pos.y);
    w.i32(pos.z);
    client
        .write_packet(BLOCK_ACTION_C2S, w.as_slice())
        .await
        .expect("send block action");
}

/// Every `(slot, item key, count)` a `CONTAINER_SET_SLOT_S2C` in `packets`
/// announced, decoded back out of this file's stand-in layout.
fn container_slot_writes(packets: &[(i32, Vec<u8>)]) -> Vec<(i32, String, i32)> {
    packets
        .iter()
        .filter(|(id, _)| *id == CONTAINER_SET_SLOT_S2C)
        .filter_map(|(_, payload)| {
            let mut r = Reader::new(payload);
            let _window = r.var_i32().ok()?;
            let slot = r.var_i32().ok()?;
            if !r.bool().ok()? {
                return None;
            }
            let key = r.string(64).ok()?;
            let count = r.var_i32().ok()?;
            Some((slot, key, count))
        })
        .collect()
}

/// Sends a `SET_CREATIVE_MODE_SLOT`-equivalent write. `item` mirrors the real
/// packet's `None` = clear-the-slot case.
async fn send_creative_slot(client: &mut Connection<DuplexStream>, slot: i16, item: Option<&ItemStack>) {
    // `SET_CREATIVE_MODE_SLOT` is creative-only server-side (vanilla's
    // `hasInfiniteMaterials()`), so the write is bracketed by a switch into
    // creative and straight back out. Back out matters: creative also changes
    // block breaking and damage immunity, and every caller of this helper is
    // testing survival behaviour.
    send_game_mode(client, 1).await;
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
    send_game_mode(client, 0).await;
}

/// Sends the stand-in `player_input` sprint, shift and jump flags.
async fn send_player_input(client: &mut Connection<DuplexStream>, sprint: bool, shift: bool, jump: bool) {
    let mut w = Writer::default();
    w.u8(u8::from(sprint));
    w.u8(u8::from(shift));
    w.u8(u8::from(jump));
    client
        .write_packet(PLAYER_INPUT_C2S, w.as_slice())
        .await
        .expect("send player input");
}

/// Sends the stand-in `ping_request` (one big-endian `i64`).
async fn send_ping_request(client: &mut Connection<DuplexStream>, time: i64) {
    let mut w = Writer::default();
    w.i64(time);
    client
        .write_packet(PING_REQUEST_C2S, w.as_slice())
        .await
        .expect("send ping request");
}

/// Sends the stand-in `set_game_rule` with one `(key, value)` entry.
async fn send_game_rule(client: &mut Connection<DuplexStream>, key: &str, value: &str) {
    let mut w = Writer::default();
    w.var_i32(1);
    w.string(key);
    w.string(value);
    client
        .write_packet(SET_GAME_RULE_C2S, w.as_slice())
        .await
        .expect("send game rule");
}

/// Sends the stand-in `change_game_mode` (`0` survival, `1` creative).
async fn send_game_mode(client: &mut Connection<DuplexStream>, ordinal: u8) {
    let mut w = Writer::default();
    w.u8(ordinal);
    client
        .write_packet(CHANGE_GAME_MODE_C2S, w.as_slice())
        .await
        .expect("send game mode");
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

    // Now in `serve_play`: collect the periodic broadcasts. **Issue #323**: each
    // one reports the *world's* clock, and this server has no tick loop, so that
    // clock is zero and stays zero. Three broadcasts arrive, proving the 1-second
    // `TIME_SYNC_INTERVAL` timer fires repeatedly, and all three carry the same
    // value.
    //
    // A *rising* value here was the bug. The old broadcast sent
    // `ticks_since(play_start)` — wall-clock elapsed since this connection joined —
    // with no anchor, and this test asserted exactly that, which is why every link
    // in the chain read green while the number on the wire was not the world's
    // time. So the assertion is inverted on purpose: if someone reintroduces an
    // elapsed-time source, `game_time` climbs and this fails.
    for broadcast in 0..3 {
        let (id, payload) = client.read_packet().await.unwrap().unwrap();
        assert_eq!(id, SET_TIME_S2C);
        let mut r = Reader::new(&payload);
        assert_eq!(
            r.i64().unwrap(),
            0,
            "broadcast {broadcast}: no tick loop means no world ticks, so game_time \
             stays 0 — a climbing value is elapsed-since-join, issue #323's bug"
        );
        assert!(
            r.bool().unwrap(),
            "the day/night anchor is now always sent: an empty clock map means \
             'keep your own anchor', and a client keeps advancing that, so a frozen \
             server clock would still show a moving sun"
        );
        assert_eq!(r.i64().unwrap(), 0, "and the anchor is the world's day_time");
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

/// **"If the player moves it should properly generate the closer chunks first."**
///
/// A move's newly-visible columns must reach the wire ordered by distance from the
/// player's *new* column, not lexicographically by `(cx, cz)`.
///
/// # The two hypotheses, and why this jump was chosen
///
/// The subject is a diagonal jump from chunk `(0, 0)` to `(2, 2)` at
/// `view_radius = 3`, which is the smallest move whose added set contains columns
/// at *different* distances — a straight one-axis step adds a single strip, all of
/// it equidistant, and could not tell the two orderings apart. The added set is
/// every column of `[-1, 5]²` outside `[-3, 3]²`, whose distances from `(2, 2)`
/// are 2 and 3.
///
/// | ordering | first column sent |
/// |---|---|
/// | lexicographic (`sort_unstable`, the old behaviour) | `(4, -1)` — distance **3**, a corner behind the player |
/// | distance-first (`join_scheduler::view_order_key`) | distance **2** |
///
/// So the assertion is on the first column's distance *and* on monotonicity: the
/// first alone would be satisfied by a shuffle, and monotonicity alone would be
/// satisfied by a set that happened to be equidistant.
#[tokio::test]
async fn a_move_streams_the_new_columns_nearest_first() {
    let view_radius = 3;
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
    drive_login_and_join(&mut client, "Diagonal", 49).await;
    send_chunk_batch_received(&mut client, 10.0).await;

    // Block (40, 64, 40) is chunk (2, 2).
    send_player_moved(&mut client, 40.0, 64.0, 40.0).await;
    let moved = drain_available(&mut client).await;
    let added: Vec<(i32, i32)> = moved
        .iter()
        .filter(|(id, _)| *id == CHUNK)
        .map(|(_, payload)| {
            let mut r = Reader::new(payload);
            (r.var_i32().unwrap(), r.var_i32().unwrap())
        })
        .collect();
    assert!(
        !added.is_empty(),
        "a two-chunk diagonal jump must send new columns"
    );

    let distance = |(cx, cz): (i32, i32)| (cx - 2).abs().max((cz - 2).abs());
    assert_eq!(
        distance(added[0]),
        2,
        "the first column of a move must be one of the nearest ones; got {:?} at distance {}. \
         Lexicographic order would send (4, -1) at distance 3 first",
        added[0],
        distance(added[0])
    );
    let mut previous = 0;
    for &coord in &added {
        let d = distance(coord);
        assert!(
            d >= previous,
            "{coord:?} at distance {d} follows a column at distance {previous}: a move's batch \
             must be non-decreasing in distance from the player's new column"
        );
        previous = d;
    }

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

    // **Natural regeneration off, and this is load-bearing rather than tidying.**
    // Once hunger landed, a hurt player with a full food bar heals on the fast
    // regeneration arm every 10 ticks — so the very next `SetHealth` after the first
    // drowning hit is a *heal*, not the second hit, and this gate's "the next health
    // update is the second hit" premise stopped holding. Turning the rule off keeps
    // the test measuring the drowning cadence rather than the race between drowning
    // and regeneration, which `crate::food`'s own gates cover.
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;

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

/// The play-state `ServerBound::PingRequest` arm added to
/// `dispatch_play_packet`: previously this variant sat in the "unreachable
/// here by construction" catch-all alongside `Handshake`/`LoginStart`/etc,
/// so even after the decode arm stopped discarding it, the reply would
/// still never have been sent. This is the dispatch-and-consumer half of
/// that fix (`encode_pong_response` actually gets called); the wire-shape
/// half is `crates/protocol/v770/src/server_protocol.rs`'s own
/// `play_ping_request_tests`, against the real `V770ServerProtocol`, per
/// `CLAUDE.md`'s note that a `FakeProtocol` test proves dispatch and
/// consumer but never decode.
///
/// The time value is echoed unchanged (`ClientboundPongResponsePacket`
/// carries the same field, un-transformed) — asserted by value, not just by
/// "a reply arrived", so a consumer that answered with the wrong field (or a
/// constant) cannot pass.
#[tokio::test(start_paused = true)]
async fn ping_request_gets_a_pong_response_echoing_the_time() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Pinger", 1).await;

    send_ping_request(&mut client, 0x0102_0304_0506_0708).await;
    let reply = drain_available(&mut client).await;
    let pongs: Vec<i64> = reply
        .iter()
        .filter(|(id, _)| *id == PONG_RESPONSE_S2C)
        .map(|(_, payload)| {
            let mut r = Reader::new(payload);
            r.i64().expect("pong time")
        })
        .collect();
    assert_eq!(
        pongs,
        vec![0x0102_0304_0506_0708],
        "exactly one pong, echoing the ping's own time unchanged: {reply:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
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

/// **Issue #327 end to end**: `SET_GAME_RULE` writes into the world's typed store,
/// and an unknown key is *rejected* rather than stored.
///
/// The camelCase name is the load-bearing half. 26.2 renamed every rule
/// (`GameRules.java:24-92`), so `randomTickSpeed` is not a rule any more — and the
/// old store kept every `(String, String)` verbatim, so it was accepted, echoed
/// back to the client, and then never read by anything, because the reader asks for
/// `random_tick_speed`. The player saw their rule confirmed and no behaviour
/// change, with nothing reporting a problem.
///
/// Both directions are asserted from the same connection: the reply to the valid
/// rule carries it, and the reply to the invalid one is empty.
#[tokio::test(start_paused = true)]
async fn a_set_game_rule_is_validated_and_a_renamed_key_is_refused() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "RuleSetter", 1).await;

    // The real 26.2 identifier: accepted, and confirmed back.
    send_game_rule(&mut client, "random_tick_speed", "7").await;
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().expect("entry count"), 1);
    assert_eq!(r.string(64).expect("key"), "random_tick_speed");
    assert_eq!(r.string(64).expect("value"), "7");

    // The pre-26.2 spelling: refused, so the confirmation is empty.
    send_game_rule(&mut client, "randomTickSpeed", "9").await;
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(
        r.var_i32().expect("entry count"),
        0,
        "a renamed key is not a rule this server knows, and storing it would be \
         confirmed-and-never-read"
    );

    // And the store still holds only the valid one.
    send_client_command(&mut client, 2).await;
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().expect("entry count"), 1);
    assert_eq!(r.string(64).expect("key"), "random_tick_speed");
    assert_eq!(r.string(64).expect("value"), "7");

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

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this fixture
        // counts generations, not reads.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
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
    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        FakeProtocol.encode_set_health(health, food, saturation)
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

/// Reads the whole join view off the wire, in wire order, tolerating everything
/// else the server sends while it is doing so.
///
/// Returns `(observed, batch_sizes)` — `observed` is
/// `(cx, cz, columns_generated_when_encoded)` for [`check_proximity_stream`], and
/// `batch_sizes` is what each `CHUNK_BATCH_FINISHED` marker *reported*.
///
/// # Why this replaced a positional read
///
/// The join no longer finishes before the play loop starts: the innermost rings go
/// out inline and the rest streams from `serve_play` beside everything else it
/// sends, in batches of `JOIN_STREAM_BATCH_COLUMNS`. So the two things the old
/// loop assumed — that chunk packets are contiguous, and that one begin/end pair
/// wraps the lot — are gone *by design*, and a gate asserting them would be
/// asserting the absence of the fix.
///
/// What still has to hold is asserted here rather than dropped:
///
/// * every chunk arrives **inside** an open batch (a stray chunk outside a
///   begin/end pair would break a real client's flow-control accounting);
/// * each marker's reported size equals the columns actually inside that batch;
/// * the chunk *order* is untouched, which the caller checks with the same
///   [`check_proximity_stream`] the pre-fix control is judged by.
async fn collect_join_chunks<T: Transport>(
    client: &mut Connection<T>,
    expected: usize,
) -> (Vec<(i32, i32, usize)>, Vec<i32>) {
    let mut observed = Vec::with_capacity(expected);
    let mut batch_sizes = Vec::new();
    let mut in_batch = false;
    let mut counted = 0usize;
    while observed.len() < expected {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-view");
        if id == CHUNK_BATCH_START {
            assert!(!in_batch, "a batch opened inside another batch");
            in_batch = true;
            counted = 0;
        } else if id == CHUNK {
            assert!(in_batch, "a chunk arrived outside a begin/end batch pair");
            let mut r = Reader::new(&payload);
            let cx = r.var_i32().unwrap();
            let cz = r.var_i32().unwrap();
            let at = r.var_i32().unwrap() as usize;
            observed.push((cx, cz, at));
            counted += 1;
        } else if id == CHUNK_BATCH_FINISHED {
            assert!(in_batch, "a batch closed without opening");
            let reported = Reader::new(&payload).var_i32().unwrap();
            assert_eq!(
                reported as usize, counted,
                "the batch marker reported {reported} columns but {counted} chunk packets \
                 arrived in it"
            );
            batch_sizes.push(reported);
            in_batch = false;
        }
    }
    // The tail marker for the batch the last chunk landed in.
    if in_batch {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        assert_eq!(
            id, CHUNK_BATCH_FINISHED,
            "the last chunk's batch must be closed"
        );
        let reported = Reader::new(&payload).var_i32().unwrap();
        assert_eq!(reported as usize, counted);
        batch_sizes.push(reported);
    }
    (observed, batch_sizes)
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

    let (observed, batch_sizes) = collect_join_chunks(&mut client, expected_chunks).await;
    assert_eq!(
        batch_sizes.iter().sum::<i32>(),
        expected_chunks as i32,
        "the batch markers must account for exactly the whole view — no column may be sent \
         outside one, and none counted twice"
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

    let (observed, batch_sizes) = collect_join_chunks(&mut client, expected_chunks).await;
    assert_eq!(
        batch_sizes.iter().sum::<i32>(),
        expected_chunks as i32,
        "the batch markers must account for exactly the whole view on this arm too"
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

/// **The owner's report, as a counter: "I can't break blocks, take damage, etc.
/// until it finishes."**
///
/// A play packet sent the instant the client finishes configuration must be
/// *serviced* — replied to — while the join view is still streaming. Measured on
/// the production arm (`SourceRef::Shared`, over a real loopback socket) because
/// the arm is the thing that changed.
///
/// # Why a counter and not a stopwatch
///
/// This is a latency claim, and a wall-clock one would be attributed to the wrong
/// cause: this machine's timings reproduce to ~10.8% and several agents run
/// concurrently. So the instrument is **how many of the view's 361 chunk packets
/// had arrived when the reply did**, which is a property of the ordering rather
/// than of the machine:
///
/// | hypothesis | count |
/// |---|---|
/// | the join burst blocks the play loop (the defect) | **361** — the reply cannot precede the last chunk, because the loop that produces it has not started |
/// | the burst is deferred past `JOIN_PRESTREAM_RADIUS` (the fix) | **12–24**, measured over three runs — the nine pre-streamed columns plus however many of the deferred stream `select!` emitted before it happened to poll the socket read first |
///
/// The bound is 40 — comfortably above the second and nowhere near the first, so
/// it cannot be satisfied by a scheduler that merely reordered the burst. It is a
/// *range* rather than a single number because `select!` picks between a ready
/// column and a ready packet at random, which is exactly the property that stops
/// either starving the other; the floor of 9 is the deterministic part. And the
/// view still has to arrive **whole and in order** afterwards, which the tail of
/// this test asserts with the same [`check_proximity_stream`] the two ordering
/// gates use: a "fix" that dropped the rest of the view would otherwise pass.
#[tokio::test]
async fn a_play_packet_is_serviced_before_the_last_join_chunk() {
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
    w.string("Impatient");
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
    // The interaction, written before a single chunk has been read: a difficulty
    // change, whose reply is a `change_difficulty` broadcast
    // (`apply_difficulty_change`). Any play packet with an observable reply would
    // do — this one is the cheapest in this file's stand-in vocabulary and needs
    // no world state to succeed.
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, &[3])
        .await
        .expect("difficulty change");

    let mut chunks_before_reply = None;
    let mut observed = Vec::with_capacity(expected_chunks);
    while observed.len() < expected_chunks {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-view");
        if id == CHUNK {
            let mut r = Reader::new(&payload);
            let cx = r.var_i32().unwrap();
            let cz = r.var_i32().unwrap();
            let at = r.var_i32().unwrap() as usize;
            observed.push((cx, cz, at));
        } else if id == CHANGE_DIFFICULTY_S2C && chunks_before_reply.is_none() {
            chunks_before_reply = Some(observed.len());
        }
    }

    let at = chunks_before_reply.expect(
        "the difficulty change was never answered: the play loop either never ran or the \
         packet was consumed without a reply",
    );
    assert!(
        at < 40,
        "the play packet was answered only after {at} of {expected_chunks} join chunks. \
         Under the defect this is exactly {expected_chunks} (the play loop cannot run until the \
         burst finishes); with the burst deferred it measured 12-24"
    );

    // …and the deferred remainder still arrives, whole and in order.
    check_proximity_stream(&observed, view_radius)
        .expect("deferring the burst must not change what the client receives, or in what order");
    let sent: HashSet<(i32, i32)> = observed.iter().map(|&(cx, cz, _)| (cx, cz)).collect();
    assert_eq!(
        sent,
        square(0, 0, view_radius),
        "every column of the view must still arrive exactly once"
    );

    drop(client);
    server.shutdown().await;
}

/// **The same instrument, pointed at a *move* instead of a join — the half the
/// join fix did not cover.**
///
/// [`a_play_packet_is_serviced_before_the_last_join_chunk`] above proves the join
/// burst no longer stands in front of the play loop. It says nothing about the
/// steady state, and the steady state had the identical defect for a different
/// reason: `ViewTracker::recenter` used to `await` the generation *and* encode of
/// every newly-visible column inside `dispatch_play_packet`, so one movement packet
/// occupied the connection task for the whole strip. **That is a `world`-species
/// blind spot in the join gate rather than a missing assertion in it** — the gate is
/// exemplary and simply cannot reach a code path that only runs after the join has
/// drained.
///
/// # The counter, and why this jump
///
/// The subject is a jump far enough that the new window shares nothing with the old,
/// so the added set is a whole `(2r + 1)²` square — the same 361 columns the join
/// sends, which is what makes the two hypotheses as far apart as they can be:
///
/// | hypothesis | chunks of the strip that precede the reply |
/// |---|---|
/// | the move generates the strip inline (the defect) | **361** — the loop cannot read the next packet until the last column is encoded |
/// | the strip is enqueued and streamed (the fix) | a handful — the socket read and the column stream interleave |
///
/// The bound is 40, matching the join gate's, and it is nowhere near 361. A
/// one-axis step would not do: its added set is 19 columns, close enough to the
/// bound that a passing run would not distinguish the two orderings.
///
/// Ordering is `select!`'s random choice between a ready column and a ready packet,
/// which is the property that stops either starving the other — so this asserts a
/// *bound*, and separately asserts the strip still arrives whole, because a "fix"
/// that dropped the newly-visible columns would otherwise sail through.
#[tokio::test]
async fn a_play_packet_is_serviced_before_the_last_chunk_of_a_move() {
    let view_radius = 9;
    let square_columns = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
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
    w.string("Strider");
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

    // The join first: this gate is about what happens *after* it, so the strip
    // counted below cannot be contaminated by columns the join still owed.
    let (join_observed, _) = collect_join_chunks(&mut client, square_columns).await;
    assert_eq!(
        join_observed.len(),
        square_columns,
        "precondition: the join view must be fully drained before the move, or the \
         count below mixes join columns into the strip"
    );
    let mut ack = Writer::default();
    ack.f32(10.0);
    client
        .write_packet(CHUNK_BATCH_RECEIVED_C2S, ack.as_slice())
        .await
        .expect("ack the join");

    // The jump — chunk (60, 60), whose radius-9 window shares no column with the
    // one centred on the spawn chunk — followed immediately by an interaction whose
    // reply is observable. Written back to back, before a single byte of the reply
    // or the strip is read, so the server sees both in its buffer at once and the
    // ordering it produces is the thing under test.
    let mut moved = Writer::default();
    moved.f64(60.0 * 16.0 + 8.0);
    moved.f64(64.0);
    moved.f64(60.0 * 16.0 + 8.0);
    client
        .write_packet(PLAYER_MOVED_C2S, moved.as_slice())
        .await
        .expect("send move");
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, &[3])
        .await
        .expect("difficulty change");

    let mut chunks_before_reply = None;
    let mut strip: Vec<(i32, i32)> = Vec::with_capacity(square_columns);
    while strip.len() < square_columns {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-strip");
        if id == CHUNK {
            let mut r = Reader::new(&payload);
            strip.push((r.var_i32().unwrap(), r.var_i32().unwrap()));
        } else if id == CHANGE_DIFFICULTY_S2C && chunks_before_reply.is_none() {
            chunks_before_reply = Some(strip.len());
        }
    }

    let at = chunks_before_reply.expect(
        "the difficulty change was never answered: the move consumed the connection task \
         for the whole strip, or the packet was dropped",
    );
    assert!(
        at < 40,
        "the play packet sent behind a chunk-boundary crossing was answered only after \
         {at} of {square_columns} newly-visible columns. Under the defect this is exactly \
         {square_columns}: `ViewTracker::recenter` awaited the whole strip inside \
         `dispatch_play_packet`, so the loop could not read the next packet at all"
    );

    let sent: HashSet<(i32, i32)> = strip.into_iter().collect();
    assert_eq!(
        sent,
        square(60, 60, view_radius),
        "streaming the strip must not change *which* columns the client receives: \
         exactly the new window, once each"
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

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
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

/// A [`ChunkSource`] of solid `minecraft:stone` — the block-drop tests' subject
/// world, chosen because `assets/loot_table/blocks/stone.json` is one of the five
/// bundled block tables and its `alternatives`/`match_tool` shape is the
/// non-trivial one (a bare hand must fall through the silk-touch branch to
/// cobblestone, so a fixture of stone proves the fall-through actually happens
/// rather than that "an item dropped").
struct StoneSource;

impl ChunkSource for StoneSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col
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
        // No storage; edits are discarded by design. Harmless here: the drop is
        // rolled from the state read *before* `set_block`, which is exactly the
        // ordering `apply_block_action` guarantees.
    }
}

/// The block every block-drop test below breaks. Inside the served view, and
/// inside `StoneSource`'s 0..16 stone band.
const BREAK_POS: BlockPos = BlockPos::new(4, 9, 4);

/// How long a bare-handed dig is held for, between `StartDestroy` and
/// `StopDestroy`, in tests that break without a tool.
///
/// Comfortably over the slowest dig any of them performs: bare-handed stone is
/// `1.0 / 1.5 / 100` progress per server tick, so with
/// `block_breaking::UNTRACKED_SPEED_HEADROOM` it needs 14 ticks (700 ms) to reach
/// `STOP_DESTROY_PROGRESS`. Kept under `TIME_SYNC_INTERVAL`'s 1 s so the wait does
/// not also inject a time broadcast into the drained stream.
const BARE_HANDED_DIG: std::time::Duration = std::time::Duration::from_millis(800);

/// Puts a plain diamond pickaxe in the selected hand, breaks `pos`, and then
/// **empties the hand again**.
///
/// The pickaxe is required since issue #539: stone `requiresCorrectToolForDrops`,
/// so `Player.hasCorrectToolForDrops` is false bare-handed and vanilla never
/// rolls the table at all (asserted by
/// `bare_handed_stone_drops_nothing_while_bare_handed_dirt_still_drops`). It is
/// deliberately *unenchanted*, so the silk-touch `match_tool` branch of stone's
/// `alternatives` still fails and the expected drop is `cobblestone` exactly as
/// before.
///
/// Emptying the hand afterwards is not tidiness: `Inventory.add` searches
/// selected → off-hand → `0..36` and `getFreeSlot` scans `items` in order, so a
/// pickaxe left in native slot 0 would send the collected cobblestone to native
/// slot 1 (menu slot 10) and change *which slot the pickup announces*. Clearing
/// it keeps the pickup gates asserting menu slot 36, the property they exist to
/// pin.
async fn break_with_a_pickaxe(client: &mut Connection<DuplexStream>, ordinal: u8, pos: BlockPos) {
    send_creative_slot(
        client,
        36,
        Some(&ItemStack::new(
            "minecraft:diamond_pickaxe".parse().expect("valid key"),
            1,
        )),
    )
    .await;
    send_block_action(client, 0, pos).await;
    send_block_action(client, ordinal, pos).await;
    send_creative_slot(client, 36, None).await;
}

/// **Issue #337's acceptance gate, first half: a broken block drops.**
///
/// Before this, `apply_block_action`'s `StopDestroy` arm set the block to air and
/// nothing else — `crate::loot`'s 1,551 lines had zero production callers, which
/// is why #337 was reopened as a confirmed island. This drives the *real*
/// `serve_connection` path (not `drop_block_loot` directly, which would pass
/// whether or not the server ever called it) and asserts the exact drop.
///
/// Three separate predictions, each of which a plausible-but-wrong
/// implementation fails:
///
/// 1. **exactly one** item entity — not "at least one", which a table rolling
///    its pool twice would also satisfy;
/// 2. the item is **`minecraft:cobblestone`**, which is stone's table falling
///    through its silk-touch `alternatives` branch under the empty loot context.
///    A port that took the *first* alternative would produce `minecraft:stone`
///    here, and "a stone-ish item dropped" reads as success;
/// 3. the entity streams with entity type **`minecraft:item`**. This is the
///    regression guard for a real shipped bug: `MobSim::snapshots` used to set
///    `entity_type` to the *item's* key, so a dropped `minecraft:cobblestone`
///    streamed as entity type `minecraft:cobblestone` — which is not in the
///    entity-type registry, and `v770`'s `entity_type_id(name).unwrap_or(0)`
///    resolves a miss to network id `0`, `minecraft:acacia_boat`. Every wire on
///    that path read green while the value travelling it was a boat.
#[tokio::test(start_paused = true)]
async fn breaking_stone_drops_exactly_one_cobblestone_item_entity() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Driller", 1).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "precondition: nothing has dropped anything yet"
    );
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "one break of stone rolls one pool once, so exactly one item entity"
    );
    assert_eq!(
        snapshots.len(),
        1,
        "the drop must be the only entity in the sim, so the assertions below \
         cannot be reading some other entity: {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].entity_type.to_string(),
        "minecraft:item",
        "a dropped item is entity type `minecraft:item`; the item's own key here \
         means `entity_type_id` misses and the client draws `minecraft:acacia_boat`"
    );
    // Issue #537: and the *stack* travels as metadata, which is what decides
    // whether the drop draws at all. `entity_type` alone gets a correctly
    // positioned, correctly typed, completely **invisible** item entity onto
    // the client — vanilla's `ItemEntityRenderer.submit` returns early on
    // `state.item.isEmpty()` and this project's client does the same. Asserted
    // as the exact field list, not `!is_empty()`: a `MetadataField::Item`
    // carrying the wrong key (`minecraft:stone`, had the silk-touch branch
    // won) or the wrong count would satisfy a non-emptiness check.
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:cobblestone".parse().expect("valid key"),
            count: 1,
        }],
        "a dropped item's whole visible identity is ItemEntity.DATA_ITEM"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The wire assertion every other drop gate in this file omits.**
///
/// Every drop/pickup test above (and `a_dropped_item_is_collected_into_the_hotbar_and_announced`
/// and friends below) passes `entities: &NoEntities` — this file's own
/// `encode_add_entity` doc comment names that directly: "inert for every other
/// test in this file... `stream_pass` produces nothing and these are never
/// called." So the whole corpus proves a break rolls the right loot into the
/// right `MobSim`, and proves pickup/inventory side effects, but **none of it
/// proves a connected client is ever told the drop exists.** That is exactly
/// the browser's own production gap: `IntegratedServer::open_in_memory` (the
/// `wasm32` singleplayer entry — see `crates/lodestone-shell/src/net.rs`'s
/// `#[cfg(target_arch = "wasm32")]` arm) passes `NoEntities` too, so a block
/// break there rolls loot, spawns a real item entity into a real `MobHandle`,
/// and never once reaches the wire — zero pixels, no error anywhere.
///
/// This gate closes the gap the cheap way: pass the **same** `MobHandle` as
/// both `entities` and `mobs`, exactly what
/// [`lodestone_server::IntegratedServer::open_in_memory_with_items`] now does
/// for the browser build. `MobHandle` is already a legitimate `EntitySource`
/// on its own (see that impl's own doc comment) for a caller that mutates the
/// sim directly and needs no ticked republish — which is exactly
/// `destroy_block`'s access pattern, no tick loop involved.
#[tokio::test(start_paused = true)]
async fn breaking_stone_streams_add_entity_when_the_mob_handle_is_its_own_source() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            // The fix: the same handle as `mobs` below, not `&NoEntities`.
            &mobs_for_server,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Streamer", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "precondition: the break must actually roll a drop"
    );
    assert!(
        packets.iter().any(|(id, _)| *id == ADD_ENTITY_S2C),
        "the dropped item's MobHandle doubles as its own EntitySource, so an \
         ADD_ENTITY must reach the client on the very next streaming pass; got \
         packet ids {:?}",
        packets.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// A [`ChunkSource`] like [`StoneSource`] but with **one persisted cell**: every
/// other block is stone (discarded on write, exactly as [`StoneSource`]), while
/// [`BREAK_POS`] alone remembers whatever it was last set to. Issue #550's
/// fixture needs a real write to survive the break so the test can read back
/// *what* replaced the broken block, not merely that something did.
#[derive(Clone)]
struct SingleBlockSource {
    at_break_pos: std::sync::Arc<std::sync::Mutex<String>>,
}

impl SingleBlockSource {
    fn new(initial: &str) -> Self {
        Self {
            at_break_pos: std::sync::Arc::new(std::sync::Mutex::new(initial.to_string())),
        }
    }

    fn current(&self) -> String {
        self.at_break_pos.lock().expect("lock").clone()
    }
}

impl ChunkSource for SingleBlockSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col.set_block(BREAK_POS.x, BREAK_POS.y, BREAK_POS.z, &self.current());
        col
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if (x, y, z) == (BREAK_POS.x, BREAK_POS.y, BREAK_POS.z) {
            return self.current();
        }
        "minecraft:stone".to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        if (x, y, z) == (BREAK_POS.x, BREAK_POS.y, BREAK_POS.z) {
            *self.at_break_pos.lock().expect("lock") = name.to_string();
        }
        // Everything else discarded, matching `StoneSource`.
    }
}

/// **Issue #550's discriminating pair, through the real `serve_connection`
/// path**: breaking a waterlogged block leaves its water source behind, and
/// breaking the identical block *without* `waterlogged` leaves air.
///
/// Either arm alone passes under a wrong rule — the dry arm passes under
/// "always keep the fluid" too (a dry block's fluid state is `None`, so both
/// rules answer air), and the wet arm alone would pass under a bug that always
/// wrote a water source. Only running both against the same break sequence
/// separates `Level.removeBlock`'s real rule
/// (`fluidState.createLegacyBlock()`) from "write air unconditionally".
///
/// `oak_slab` rather than a plain waterlogged block because a slab drops loot
/// (exercising the same `destroy_block` path the cobblestone gate above does)
/// and is a real vanilla `waterlogged`-carrying block, not a synthetic one.
#[tokio::test(start_paused = true)]
async fn breaking_a_waterlogged_block_leaves_water_and_a_dry_one_leaves_air() {
    async fn break_and_read(initial_state: &str) -> String {
        let source = SingleBlockSource::new(initial_state);
        let (client_end, server_end) = memory_pair();
        let mobs = MobHandle::default();
        let mobs_for_server = mobs.clone();
        let source_for_server = source.clone();
        let server = tokio::spawn(async move {
            let mut conn = Connection::new(server_end);
            serve_connection(
                &mut conn,
                &FakeProtocol,
                &source_for_server,
                &NoEntities,
                0,
                &BlockEntityHandle::default(),
                &mobs_for_server,
            )
            .await
        });

        let mut client = Connection::new(client_end);
        drive_login_and_join(&mut client, "WaterlogBreaker", 1).await;
        // Creative rather than `break_with_a_pickaxe`: a pickaxe is the
        // *wrong* tool for a wood slab (an axe is correct), so a survival dig
        // needs `divider = 100` and ~140 ticks to clear `STOP_DESTROY_PROGRESS`
        // — far more than `drain_available`'s single 50ms idle window lets
        // through. Creative's `StartDestroy` breaks synchronously
        // (`apply_block_action`'s `creative` branch), and this fix is about
        // *what replaces the cell*, not about drops or dig timing, so
        // creative exercises the exact same `destroy_block` write path with
        // none of that noise.
        send_game_mode(&mut client, 1).await;
        send_block_action(&mut client, 0, BREAK_POS).await;
        let _ = drain_available(&mut client).await;

        drop(client);
        let _ = server.await.expect("server task panicked");
        source.current()
    }

    let dry = break_and_read("minecraft:oak_slab[type=bottom,waterlogged=false]").await;
    assert_eq!(dry, "minecraft:air", "a dry block's cell must become air");

    let wet = break_and_read("minecraft:oak_slab[type=bottom,waterlogged=true]").await;
    assert_eq!(
        wet, "minecraft:water[level=0]",
        "a waterlogged block's cell must keep its water source, not go to air \
         — `level=0` is `FlowingFluid.getLegacyLevel`'s own encoding for a \
         source, matching `fluidState.createLegacyBlock()`"
    );
}

/// Stone everywhere except one column of dirt at [`DIRT_POS`], for issue #539's
/// correct-tool gate.
///
/// The *world*-species guard for that gate: `Player.hasCorrectToolForDrops` is
/// `!state.requiresCorrectToolForDrops() || tool.isCorrectToolForDrops(state)`,
/// and a fixture of **only stone** exercises exactly one side of that `||`.
/// Every block in [`StoneSource`] requires a correct tool, so a gate
/// mis-implemented as "you need a tool" would pass every stone assertion and
/// fail only here.
struct StoneWithDirtSource;

/// The dirt column in [`StoneWithDirtSource`]: same chunk and height band as
/// [`BREAK_POS`], different x.
const DIRT_POS: BlockPos = BlockPos::new(6, 9, 4);

impl ChunkSource for StoneWithDirtSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col.set_block(DIRT_POS.x, DIRT_POS.y, DIRT_POS.z, "minecraft:dirt");
        col
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
        // As `StoneSource`: no storage, and the drop is rolled from the state
        // read before `set_block`.
    }
}

/// **Issue #539's correct-tool gate, through the real `serve_connection` path:
/// stone broken bare-handed drops nothing, and dirt still does.**
///
/// Vanilla's `ServerPlayerGameMode.destroyBlock` (`:295`) consults
/// `Player.hasCorrectToolForDrops` and, when it is false, never calls
/// `playerDestroy` → `dropResources` at all. Stone
/// `requiresCorrectToolForDrops`, so a bare hand breaks it and yields nothing —
/// before #539 it dropped a cobblestone, the most visible wrong behaviour in the
/// whole block-drop chain.
///
/// The stone half is an **absence**, so it is only worth what the evidence that
/// the detector fires is worth. Three things supply that here rather than a
/// description of it:
///
/// 1. the *same* packets with a pickaxe in slot 36 do produce a drop — that is
///    `breaking_stone_drops_exactly_one_cobblestone_item_entity` above, which had
///    to have the pickaxe added to keep passing;
/// 2. **dirt, bare-handed, in the same session, still drops dirt.** A gate that
///    swallowed the whole `StopDestroy` arm, or that read "you need a tool",
///    would report "no drop" for stone too and fail here;
/// 3. the two assertions run against one connection in one order, so neither can
///    be explained by the session never having reached the break path.
#[tokio::test(start_paused = true)]
async fn bare_handed_stone_drops_nothing_while_bare_handed_dirt_still_drops() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source = StoneWithDirtSource;
    assert_eq!(
        source.block_state(BREAK_POS.x, BREAK_POS.y, BREAK_POS.z),
        "minecraft:stone",
        "precondition: BREAK_POS is stone, which requires a correct tool"
    );
    assert_eq!(
        source.block_state(DIRT_POS.x, DIRT_POS.y, DIRT_POS.z),
        "minecraft:dirt",
        "precondition: DIRT_POS is dirt, which requires none — without this row \
         the fixture cannot exercise the other side of vanilla's `||`"
    );
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneWithDirtSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "BareHands", 1).await;

    // No creative-slot write at all: the selected slot is empty, which is the
    // bare hand.
    //
    // A held dig between the two ordinals, and only in this test, because it is
    // the only one that breaks **bare-handed**: since issue #531 the server
    // prices the dig (`crate::block_breaking`) and refuses a `StopDestroy` that
    // arrives too early, and a bare hand on stone is the slowest dig in the
    // file. Without the advance both packets land on one server tick, the break
    // is *refused*, and this test's first assertion — an absence — would pass
    // for the wrong reason while the dirt half failed. `break_with_a_pickaxe`
    // needs none of this: a diamond pickaxe on stone clears the threshold in a
    // single tick, which is why every other break gate here still holds.
    //
    // `sleep`, **not** `tokio::time::advance`: `advance` jumps the clock before
    // yielding, so the server had not yet read the `StartDestroy` and stamped it
    // with the *old* tick — both packets then landed on one tick anyway and the
    // break was still refused. A paused-clock `sleep` lets the runtime drain the
    // start packet first and only auto-advances once everything is idle.
    send_block_action(&mut client, 0, BREAK_POS).await;
    tokio::time::sleep(BARE_HANDED_DIG).await;
    send_block_action(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "a bare hand on stone is `hasCorrectToolForDrops == false`, so vanilla \
         never rolls the table; before #539 this dropped a cobblestone"
    );

    send_block_action(&mut client, 0, DIRT_POS).await;
    tokio::time::sleep(BARE_HANDED_DIG).await;
    send_block_action(&mut client, 2, DIRT_POS).await;
    let _ = drain_available(&mut client).await;
    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        snapshots.len(),
        1,
        "dirt does not require a correct tool, so the same bare hand drops it: \
         {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:dirt".parse().expect("valid key"),
            count: 1,
        }],
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// Stone everywhere except one dandelion at [`FLOWER_POS`], for the one-shot
/// gate below.
struct StoneWithFlowerSource;

/// The dandelion in [`StoneWithFlowerSource`] — a zero-hardness block, so
/// `progress_per_tick` is `+inf` and vanilla's `"insta mine"` branch fires on the
/// `StartDestroy`.
const FLOWER_POS: BlockPos = BlockPos::new(8, 9, 4);

impl ChunkSource for StoneWithFlowerSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col.set_block(FLOWER_POS.x, FLOWER_POS.y, FLOWER_POS.z, "minecraft:dandelion");
        col
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
        // As `StoneSource`: no storage, and the drop is rolled from the state
        // read before `set_block`.
    }
}

/// **The one-shot-block gate, end to end: a `StartDestroy` with no `StopDestroy`
/// after it pops a flower.**
///
/// This is the behaviour issue #531's commit introduced and whose only test was a
/// unit assertion on `progress_per_tick >= 1.0` — a closed loop that says nothing
/// about whether `apply_block_action` reaches `destroy_block` on the start
/// ordinal. A real client that knows a block is instant sends *only* the start
/// action, so this is the whole packet sequence for pulling grass.
///
/// The control is the pair below it: the same single start action on **stone**
/// drops nothing, so this cannot pass by breaking on every `StartDestroy`.
#[tokio::test(start_paused = true)]
async fn a_start_action_alone_pops_a_one_shot_flower_but_not_stone() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source = StoneWithFlowerSource;
    assert_eq!(
        source.block_state(FLOWER_POS.x, FLOWER_POS.y, FLOWER_POS.z),
        "minecraft:dandelion",
        "precondition: FLOWER_POS is the zero-hardness block under test"
    );
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneWithFlowerSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Picker", 1).await;

    // The control first, so a failure cannot be blamed on session ordering: one
    // start action on stone, bare-handed, and *nothing* after it.
    send_block_action(&mut client, 0, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "a bare-handed start action on stone must not break it — if this is \
         non-zero the flower assertion below proves nothing"
    );

    send_block_action(&mut client, 0, FLOWER_POS).await;
    let _ = drain_available(&mut client).await;
    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        snapshots.len(),
        1,
        "a start action alone must pop a zero-hardness block: {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:dandelion".parse().expect("valid key"),
            count: 1,
        }],
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The regression gate for ordinary block breaking: a `StopDestroy` that
/// arrives on the same tick as its `StartDestroy` still breaks the block, a
/// tick or two later.**
///
/// This is what #531 broke. That commit refused a shortfall outright, but
/// vanilla's shortfall branch arms `hasDelayedDestroy` and keeps accruing
/// progress in `ServerPlayerGameMode.tick` until the block is fully earned
/// (`ServerPlayerGameMode.java:229-234`). A local integrated server reads both
/// packets off one buffer, so *every* non-instant block took the shortfall path
/// and nothing but flowers could be broken at all.
///
/// Dirt bare-handed, because it is the one block in this file that both takes a
/// real dig and drops without a tool: `1.0 / 0.5 / 100` per tick, so with
/// `UNTRACKED_SPEED_HEADROOM` the *deferred* target of a whole block needs 7
/// ticks. The wait below is deliberately longer than that and shorter than the
/// slower blocks around it.
///
/// Two controls, and the first is the one that matters: **the drop is absent
/// immediately after the pair is sent** and present only after the wait, so this
/// is a gate on the deferred continuation rather than on the pair being accepted
/// outright. The second replaces the stop with an abort, which must never break
/// however long you wait.
#[tokio::test(start_paused = true)]
async fn a_same_tick_stop_breaks_the_block_a_few_ticks_later() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneWithDirtSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Releaser", 1).await;

    // Control: start then abort, back to back, then the same wait. An abort
    // clears the dig, so no deferred continuation may exist to finish it.
    send_block_action(&mut client, 0, DIRT_POS).await;
    send_block_action(&mut client, 1, DIRT_POS).await;
    tokio::time::sleep(BARE_HANDED_DIG).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "an aborted dig must not be finished by the deferred-destroy tick pass"
    );

    // The subject: start then stop, back to back, with no wait between them —
    // both land on one server tick, which is the case #531 refused.
    send_block_action(&mut client, 0, DIRT_POS).await;
    send_block_action(&mut client, 2, DIRT_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "a same-tick stop has not earned the block yet — if it drops here the \
         wait below is not what this gate is measuring"
    );

    // `sleep`, **not** `tokio::time::advance`: see
    // `bare_handed_stone_drops_nothing_while_bare_handed_dirt_still_drops`'s own
    // note. Only a paused-clock `sleep` lets the server's 50ms timer arm run.
    tokio::time::sleep(BARE_HANDED_DIG).await;
    let _ = drain_available(&mut client).await;
    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        snapshots.len(),
        1,
        "the deferred dig must finish on the server's own clock: {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:dirt".parse().expect("valid key"),
            count: 1,
        }],
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The control for the gate above, and it must fail the same assertion.**
///
/// Same world, same block, same packets — except the second phase is
/// `AbortDestroy` (ordinal `1`) instead of `StopDestroy`. Vanilla's
/// `pos.equals(this.destroyPos)` bookkeeping means an aborted dig breaks nothing,
/// so nothing may drop.
///
/// Without this, `breaking_stone_drops_exactly_one_cobblestone_item_entity` would
/// pass just as well against a server that dropped an item on *every* block
/// packet, including the `StartDestroy` that precedes every break. The two tests
/// differ in exactly one byte.
#[tokio::test(start_paused = true)]
async fn an_aborted_dig_drops_nothing() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Aborter", 1).await;
    // A *correct* tool, so the only difference from the positive gate is the
    // second ordinal — the correct-tool gate cannot be what makes this pass.
    break_with_a_pickaxe(&mut client, 1, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "an aborted dig breaks no block, so it must drop nothing — if this is \
         non-zero the drop is firing on the wrong packet and the positive gate \
         proves nothing"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **Issue #337's acceptance gate, second half: the drop can be picked up, and
/// the pickup reaches the client.**
///
/// The chain under test is `Player.aiStep` → `ItemEntity.playerTouch` →
/// `Inventory.add` → a window-0 `container_set_slot`. Two assertions that a
/// weaker gate would omit:
///
/// * the item entity is **gone** from the sim afterwards — a pickup that credits
///   the inventory without removing the entity duplicates items forever, and an
///   inventory-only assertion cannot see it;
/// * the client was told **which slot** changed, and the announced slot is `36`
///   — window-0 menu coordinates for native hotbar slot `0`, the selected one.
///   A pickup that wrote the right item into the right native slot but announced
///   a native index instead of a menu index puts cobblestone in the player's
///   *main storage* row on screen while the server thinks it is in the hand.
///
/// `tick_for(10)` before the walk is not incidental: `popResource` calls
/// `setDefaultPickUpDelay()` (10 ticks), so the drop is deliberately
/// uncollectable when it spawns. See the control below.
#[tokio::test(start_paused = true)]
async fn a_dropped_item_is_collected_into_the_hotbar_and_announced() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Collector", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(mobs.with(|sim| sim.item_count()), 1, "precondition: a drop exists");

    // Let the 10-tick pickup delay elapse. `MobSim::tick` is what production's
    // `run_tick_loop` calls every 50 ms; driving it here rather than
    // `ItemLifecycle::tick` keeps the delay's advancement on the real path.
    mobs.with(|sim| sim.tick_for(12));

    // Stand on the block that was broken. `popResource` scatters the drop within
    // ±0.25 of the block centre, and the pickup volume reaches 1.425 blocks
    // horizontally, so the centre is comfortably inside it from any roll.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "the collected item entity must be removed from the world, not merely \
         copied into the inventory — otherwise it is an infinite item source"
    );

    let writes = container_slot_writes(&packets);
    assert_eq!(
        writes,
        vec![(36, "minecraft:cobblestone".to_owned(), 1)],
        "exactly one window-0 slot write, announcing menu slot 36 (native hotbar \
         slot 0, the selected one) holding one cobblestone; got {writes:?}"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

// ---------------------------------------------------------------------------
// The pickup animation (`TAKE_ITEM_ENTITY`) and its ordering.
// ---------------------------------------------------------------------------

/// Fills every one of the player's 36 native inventory slots with `filler`, then
/// overwrites `partial_slot` with `partial` — so a subsequent pickup of a stackable
/// item can only partly fit.
///
/// Creative writes, bracketed into and out of creative by [`send_creative_slot`],
/// which is how a real client gives itself an item. Menu coordinates: native `0..9`
/// is the hotbar at menu `36..45`, native `9..36` is storage at menu `9..36`.
async fn fill_inventory(
    client: &mut Connection<DuplexStream>,
    filler: &ItemStack,
    partial_slot: i16,
    partial: &ItemStack,
) {
    for native in 0..36_i16 {
        let menu = if native < 9 { native + 36 } else { native };
        send_creative_slot(client, menu, Some(filler)).await;
    }
    send_creative_slot(client, partial_slot, Some(partial)).await;
}

/// Every `TAKE_ITEM_ENTITY_S2C` in `packets`, as `(index, item_entity_id,
/// collector_id, amount)`, plus the index of the first `REMOVE_ENTITIES_S2C`
/// mentioning `item_entity_id`.
fn take_and_remove_indices(
    packets: &[(i32, Vec<u8>)],
    item_entity_id: i32,
) -> (Vec<(usize, i32, i32, i32)>, Option<usize>) {
    let mut takes = Vec::new();
    let mut remove_at = None;
    for (i, (id, payload)) in packets.iter().enumerate() {
        if *id == TAKE_ITEM_ENTITY_S2C {
            let mut r = Reader::new(payload);
            let item = r.var_i32().expect("item entity id");
            let collector = r.var_i32().expect("collector id");
            let amount = r.var_i32().expect("amount");
            takes.push((i, item, collector, amount));
        } else if *id == REMOVE_ENTITIES_S2C && remove_at.is_none() {
            let mut r = Reader::new(payload);
            let count = r.var_i32().expect("count");
            for _ in 0..count {
                if r.var_i32().expect("removed id") == item_entity_id {
                    remove_at = Some(i);
                    break;
                }
            }
        }
    }
    (takes, remove_at)
}

/// **Matthew's report: "the pickup animation for items is missing on the integrated
/// server".** The client half was already complete — the v770 adapter decodes
/// `TAKE_ITEM_ENTITY` into `ClientEvent::ItemPickup` and `lodestone-shell`'s
/// `entities.rs` carries the lerp — and our server never sent the packet. An island
/// in the **outbound** direction, the mirror of `ClientAction::SetFlying`.
///
/// # The claim, and why it needs a *full* pickup
///
/// **The take must precede the `REMOVE_ENTITIES` for the same entity.** Vanilla's
/// `ItemEntity.playerTouch` calls `player.take(this, orgCount)` and only then
/// `this.discard()`, because the client deliberately keeps the entity alive to
/// interpolate it toward the collector and removes it when the animation ends. A
/// server that removes first produces **no animation at all** with the packet present
/// and correct on the wire — precisely the way this lands looking fixed and is not.
/// Asserted as an index comparison, which is a counter.
///
/// The pickup is therefore **full**: a partial one leaves the entity alive by
/// construction, so there is no removal to order against and the claim would be
/// unobservable. `amount`'s own discriminating case is the partial one and lives in
/// [`a_partial_pickup_announces_the_original_stack_count_not_the_amount_banked`].
///
/// The rig passes the `MobHandle` as the **entity source** as well, which no other
/// test in this file does, because `NoEntities` streams nothing and there would be no
/// `REMOVE_ENTITIES` at all.
#[tokio::test(start_paused = true)]
async fn a_pickup_announces_the_take_before_removing_the_item_entity() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let entities = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &entities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Magpie", 1).await;

    // Count 5 into an empty inventory: all of it fits, so the entity is removed.
    // Spawned directly because the block-break path pops exactly one cobblestone, and
    // a count of 1 could not tell `amount` apart from a hardcoded `1`.
    let item_entity_id = mobs.with(|sim| {
        sim.spawn_item(
            ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
            Vec3::new(
                f64::from(BREAK_POS.x) + 0.5,
                f64::from(BREAK_POS.y),
                f64::from(BREAK_POS.z) + 0.5,
            ),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle {
                age: 20,
                pickup_delay: 0,
                count: 5,
                max_stack_size: 64,
            },
        )
    });

    // Stand above it first so the entity is streamed to the client *before* the
    // pickup. Without this the spawn and the removal could land in one pass and the
    // ordering claim would be about a different pair of packets.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y) + 3.0,
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let _ = drain_available(&mut client).await;

    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    let (takes, remove_at) = take_and_remove_indices(&packets, item_entity_id);
    let mut problems: Vec<String> = Vec::new();

    match takes.as_slice() {
        [] => problems.push(
            "no TAKE_ITEM_ENTITY was sent at all: the pickup animation packet has no \
             producer, which is the reported bug"
                .to_owned(),
        ),
        [(take_at, item, collector, amount)] => {
            if *item != item_entity_id {
                problems.push(format!(
                    "the take names item entity {item}, but the drop is {item_entity_id}"
                ));
            }
            if *collector != LOCAL_PLAYER_ENTITY_ID {
                problems.push(format!(
                    "the take names collector {collector}, but the player is \
                     {LOCAL_PLAYER_ENTITY_ID}; the client lerps the item toward this id, \
                     so a wrong one sends the item to the wrong place"
                ));
            }
            if *amount != 5 {
                problems.push(format!(
                    "amount is {amount}, but the stack held 5; a hardcoded 1 is audible, \
                     because the amount drives the pickup sound's pitch"
                ));
            }
            match remove_at {
                None => problems.push(
                    "the item entity was never removed, so the pickup is an infinite \
                     item source and the ordering claim is moot"
                        .to_owned(),
                ),
                Some(remove_at) if remove_at <= *take_at => problems.push(format!(
                    "REMOVE_ENTITIES (packet {remove_at}) reached the client at or before \
                     the take (packet {take_at}). The client keeps the entity alive to \
                     interpolate it and removes it when the animation ends, so it has \
                     nothing left to animate: no pickup animation, with the packet \
                     present and correct"
                )),
                Some(_) => {}
            }
        }
        many => problems.push(format!(
            "expected exactly one take, got {}: {many:?}",
            many.len()
        )),
    }

    assert!(
        problems.is_empty(),
        "the pickup animation is wrong in {} way(s):\n  {}",
        problems.len(),
        problems.join("\n  "),
    );

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "precondition: the whole stack fitted, so the entity must be gone — otherwise \
         there was no removal for the ordering assertion to be about"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **`amount` is `orgCount`, not the amount banked — and only a partial pickup can
/// tell them apart.**
///
/// Vanilla captures `int orgCount = itemStack.getCount()` *before*
/// `player.getInventory().add(itemStack)`, which shrinks the stack in place, and then
/// passes `orgCount` to `player.take`. On a **full** pickup the two coincide, so that
/// case measures only that the code runs — the same corollary that made `oak_leaves`
/// the wrong choice for the item-collision gates.
///
/// The inventory is filled so exactly 2 of a 5-stack fit: `orgCount` is `5`, banked is
/// `2`, and the assertion is `5`. The surviving entity's count is checked too, because
/// that is what establishes the pickup really was partial — without it a full pickup
/// would satisfy the `amount == 5` assertion and prove nothing.
#[tokio::test(start_paused = true)]
async fn a_partial_pickup_announces_the_original_stack_count_not_the_amount_banked() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let entities = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &entities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Fussy", 1).await;

    // 35 slots of a *different* item plus one holding 62 cobblestone, so only 2 of a
    // 5-cobblestone pickup can be placed.
    let filler = ItemStack::new(
        ResourceKey::from_str("minecraft:stone").expect("valid key"),
        64,
    );
    let nearly_full = ItemStack::new(
        ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
        62,
    );
    fill_inventory(&mut client, &filler, 36, &nearly_full).await;
    let _ = drain_available(&mut client).await;

    let item_entity_id = mobs.with(|sim| {
        sim.spawn_item(
            ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
            Vec3::new(
                f64::from(BREAK_POS.x) + 0.5,
                f64::from(BREAK_POS.y),
                f64::from(BREAK_POS.z) + 0.5,
            ),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle {
                age: 20,
                pickup_delay: 0,
                count: 5,
                max_stack_size: 64,
            },
        )
    });

    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    let (takes, _) = take_and_remove_indices(&packets, item_entity_id);
    let amounts: Vec<i32> = takes.iter().map(|&(_, _, _, amount)| amount).collect();
    assert_eq!(
        amounts,
        vec![5],
        "the take must report orgCount = 5, the stack's count before the inventory \
         took any of it. `2` means the banked amount was sent instead — which is \
         indistinguishable from correct on a full pickup, and is the reason this one \
         is partial"
    );

    assert_eq!(
        mobs.with(|sim| sim.item_position(item_entity_id)).is_some(),
        true,
        "precondition: the pickup must really have been partial, so the entity \
         survives holding the remainder. If it is gone the whole stack was banked and \
         orgCount could not be told apart from the banked amount"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The control for the `banked > 0` gate: a pickup that fits nowhere announces
/// nothing.**
///
/// Vanilla's guard is `player.getInventory().add(itemStack)` returning true —
/// `playerTouch` only calls `take` when something actually went in. With every slot
/// full of a different item, a cobblestone drop fits nowhere, so there is no take and
/// the entity stays. Without this, an implementation that announced a take on every
/// overlap would pass the gate above.
#[tokio::test(start_paused = true)]
async fn a_pickup_into_a_full_inventory_announces_no_take() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let entities = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &entities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Hoarder", 1).await;

    // Every slot holds a full stack of something else, so nothing can be added.
    let filler = ItemStack::new(
        ResourceKey::from_str("minecraft:stone").expect("valid key"),
        64,
    );
    fill_inventory(&mut client, &filler, 36, &filler).await;
    let _ = drain_available(&mut client).await;

    let item_entity_id = mobs.with(|sim| {
        sim.spawn_item(
            ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
            Vec3::new(
                f64::from(BREAK_POS.x) + 0.5,
                f64::from(BREAK_POS.y),
                f64::from(BREAK_POS.z) + 0.5,
            ),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle {
                age: 20,
                pickup_delay: 0,
                count: 1,
                max_stack_size: 64,
            },
        )
    });

    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    let (takes, _) = take_and_remove_indices(&packets, item_entity_id);
    assert!(
        takes.is_empty(),
        "nothing fitted, so vanilla's `add(...)` would have returned false and no \
         take is sent; got {takes:?}"
    );
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "precondition: the drop must still be in the world, or this measured a \
         successful pickup that merely failed to announce itself"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The control for the pickup gate: the delay is real.**
///
/// Identical to the test above but with **no** `tick_for`, so the drop still
/// carries its full 10-tick `setDefaultPickUpDelay`. The player stands exactly on
/// it and must collect nothing.
///
/// This is the control that matters most here, because its absence is invisible:
/// a server that ignored `pickup_delay` entirely would pass the positive gate
/// (the item is collected — just a few ticks early), and the only symptom would
/// be that a player mining in survival never sees a drop bounce, because they
/// re-absorb it on the spawning tick. It also proves the pickup sweep is
/// genuinely gated rather than firing on every movement packet regardless.
#[tokio::test(start_paused = true)]
async fn a_freshly_popped_drop_is_not_collectable_before_its_delay_elapses() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Impatient", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(mobs.with(|sim| sim.item_count()), 1);

    // No `tick_for`: the pickup delay is still 10.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "the drop still carries its 10-tick pickup delay, so standing on it must \
         collect nothing"
    );
    assert!(
        container_slot_writes(&packets).is_empty(),
        "and no slot write may be announced: {:?}",
        container_slot_writes(&packets)
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The other control on the pickup volume: distance is real.**
///
/// The delay has elapsed and the drop is collectable, but the player walks to a
/// position well outside `Player.aiStep`'s inflated box (10 blocks away). Nothing
/// may be collected.
///
/// Without this, the positive pickup gate is satisfied by a server that collects
/// every drop in the world on any movement packet — which would look completely
/// correct in the one scene where the only drop is the one at your feet. That is
/// this repo's *world* species: the flaw would live in the fixture, not the
/// assertion.
#[tokio::test(start_paused = true)]
async fn a_drop_outside_the_pickup_volume_is_not_collected() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Distant", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    mobs.with(|sim| sim.tick_for(12));

    // Ten blocks away — far outside the 1.425-block horizontal reach, and inside
    // the same served column so the connection is not disconnected for a view
    // change mid-test.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 10.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "a collectable drop ten blocks away must stay in the world; if this is 0 \
         the pickup sweep ignores position and the positive gate proves nothing"
    );
    assert!(
        container_slot_writes(&packets).is_empty(),
        "and nothing may be credited: {:?}",
        container_slot_writes(&packets)
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}


/// **Issue #336: a banned uuid is refused at login**, before `login_success`, with
/// vanilla's own translation key on the wire — and the identical connection is
/// admitted once the ban is lifted.
///
/// The lifted-ban arm is the control: without it, a test that only asserts the
/// refusal cannot tell "the ban was enforced" from "this fixture never joins".
#[tokio::test]
async fn a_banned_uuid_is_refused_at_login_and_admitted_once_pardoned() {
    use lodestone_server::access::{AccessHandle, BanEntry};

    // `FakeProtocol` decodes every login as `Uuid::nil()`, so that is the identity
    // to ban.
    let access = AccessHandle::default();
    access.with(|lists| {
        lists.ban(
            Uuid::nil(),
            BanEntry::permanent("tester", "gate", "no reason at all"),
        );
    });

    let (client_end, server_end) = memory_pair();
    let mut client = Connection::new(client_end);
    let serving = {
        let access = access.clone();
        tokio::spawn(async move {
            let mut conn = Connection::new(server_end);
            lodestone_server::serve_connection_with_access(
                &mut conn,
                &FakeProtocol,
                &AirSource,
                &NoEntities,
                0,
                &access,
                None,
            )
            .await
        })
    };

    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("tester");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(
        id, DISCONNECT_S2C,
        "a banned uuid must be disconnected, not sent login_success"
    );
    let mut r = Reader::new(&payload);
    let reason = r.string(256).expect("reason");
    assert!(
        reason.starts_with("multiplayer.disconnect.banned.reason"),
        "the refusal must carry vanilla's own translation key; got {reason:?}"
    );
    assert!(reason.contains("no reason at all"), "and the ban's reason: {reason:?}");

    let outcome = serving.await.expect("server task panicked");
    assert!(
        matches!(outcome, Err(ServerError::AccessDenied(_))),
        "the refusal must be reported as AccessDenied; got {outcome:?}"
    );

    // Control: pardon and the same sequence joins.
    access.with(|lists| assert!(lists.pardon(Uuid::nil())));
    let (client_end, server_end) = memory_pair();
    let mut client = Connection::new(client_end);
    let serving = {
        let access = access.clone();
        tokio::spawn(async move {
            let mut conn = Connection::new(server_end);
            lodestone_server::serve_connection_with_access(
                &mut conn,
                &FakeProtocol,
                &AirSource,
                &NoEntities,
                0,
                &access,
                None,
            )
            .await
        })
    };
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("tester");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(
        id, LOGIN_SUCCESS,
        "a pardoned uuid must reach login_success"
    );
    drop(client);
    let _ = serving.await.expect("server task panicked");
}

// ---------------------------------------------------------------------------
// Protocol encode must not run on the connection task (`ChunkEncoder`).
// ---------------------------------------------------------------------------

/// Set on any thread that has run [`ServerProtocol::decode`] — i.e. on the
/// connection task's thread, and nowhere else.
///
/// This is the discriminator [`EncodeSiteProto`] is built on, and it is exact
/// rather than heuristic. `decode` is only ever called from
/// `serve_connection_inner`/`dispatch_play_packet`, both of which run on the
/// connection task; the tests below run on `#[tokio::test]`'s **current-thread**
/// runtime — the same flavour production builds
/// (`crates/lodestone-shell/src/net.rs`'s `new_current_thread`) — so that task
/// cannot migrate, and `tokio`'s blocking pool is a disjoint set of threads that
/// never decodes anything.
///
/// Deliberately not a thread-id comparison: a thread id would have to be captured
/// somewhere and compared somewhere else, and the capture site is exactly what a
/// future refactor would move. A flag set by `decode` itself cannot be wrong about
/// what a connection thread is.
thread_local! {
    static HAS_DECODED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Where column encode actually happened, counted by site.
#[derive(Debug, Default)]
struct EncodeSites {
    /// Encodes that ran on a thread which has served inbound packets — the
    /// defect. Every one of these is ≈2.4 ms / 62 M instructions the player's
    /// next interaction waits behind.
    on_connection_thread: std::sync::atomic::AtomicUsize,
    /// Encodes that ran on a blocking-pool thread. The fix.
    off_connection_thread: std::sync::atomic::AtomicUsize,
}

impl EncodeSites {
    fn record(&self) {
        let counter = if HAS_DECODED.with(std::cell::Cell::get) {
            &self.on_connection_thread
        } else {
            &self.off_connection_thread
        };
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn on_task(&self) -> usize {
        self.on_connection_thread
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn off_task(&self) -> usize {
        self.off_connection_thread
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The `ChunkEncoder` half of [`EncodeSiteProto`], sharing its counters.
struct SiteEncoder {
    sites: std::sync::Arc<EncodeSites>,
}

impl lodestone_server::ChunkEncoder for SiteEncoder {
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.sites.record();
        encode_site_chunk(cx, cz, column)
    }
}

/// The one column-encode body both arms use, so the arms differ in **where** the
/// encode runs and in nothing else. Payload is just the coordinate pair, which is
/// what [`collect_join_chunks`] reads.
fn encode_site_chunk(cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
    let mut w = Writer::default();
    w.var_i32(cx);
    w.var_i32(cz);
    w.var_i32(0);
    ServerDirective::Send {
        packet_id: CHUNK,
        payload: w.as_slice().to_vec(),
    }
}

/// [`FakeProtocol`] plus an encode-site census, and a switch for whether it
/// offers an off-task [`lodestone_server::ChunkEncoder`] at all.
///
/// `off_task: false` is the **live negative control**: it is not a neutered copy
/// of the fixed arm, it is the shape every protocol without a `ChunkEncoder`
/// still has (the trait method defaults to `None`), driven through the same
/// `serve_connection` body. So the control cannot rot, and it must show the
/// defect.
struct EncodeSiteProto {
    sites: std::sync::Arc<EncodeSites>,
    off_task: bool,
}

impl ServerProtocol for EncodeSiteProto {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        HAS_DECODED.with(|flag| flag.set(true));
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
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.sites.record();
        encode_site_chunk(cx, cz, column)
    }
    fn chunk_encoder(&self) -> Option<std::sync::Arc<dyn lodestone_server::ChunkEncoder>> {
        self.off_task.then(|| {
            std::sync::Arc::new(SiteEncoder {
                sites: std::sync::Arc::clone(&self.sites),
            }) as std::sync::Arc<dyn lodestone_server::ChunkEncoder>
        })
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
    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        FakeProtocol.encode_change_difficulty(difficulty, locked)
    }
}

/// Joins a loopback server built on `off_task`, sends one play packet before
/// reading a single chunk, and reports `(encodes on the connection thread when
/// the reply arrived, sites over the whole view, chunks observed)`.
async fn measure_encode_sites(
    off_task: bool,
    view_radius: i32,
) -> (usize, std::sync::Arc<EncodeSites>, usize) {
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let sites = std::sync::Arc::new(EncodeSites::default());
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let server = lodestone_server::IntegratedServer::bind(
        "127.0.0.1:0",
        EncodeSiteProto {
            sites: std::sync::Arc::clone(&sites),
            off_task,
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
    w.string(&format!("EncodeSite{}", u8::from(off_task)));
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
    // The interaction, written before a single chunk has been read — exactly as
    // `a_play_packet_is_serviced_before_the_last_join_chunk` does, and for the
    // same reason: it is the cheapest packet in this file's vocabulary with an
    // observable reply.
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, &[3])
        .await
        .expect("difficulty change");

    let mut on_task_at_reply = None;
    let mut observed = 0usize;
    while observed < expected_chunks {
        let (id, _payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-view");
        if id == CHUNK {
            observed += 1;
        } else if id == CHANGE_DIFFICULTY_S2C && on_task_at_reply.is_none() {
            on_task_at_reply = Some(sites.on_task());
        }
    }

    drop(client);
    server.shutdown().await;

    let at = on_task_at_reply.expect(
        "the difficulty change was never answered: the play loop either never ran or the \
         packet was consumed without a reply",
    );
    // Returned as the `Arc`: the serving task still holds the protocol at this
    // point (`shutdown` does not join it synchronously), so a `try_unwrap` here
    // panics on a live second reference and says nothing about the counters.
    (at, sites, observed)
}

/// **The latency claim as a counter: how many columns' worth of encode work can
/// sit between a play packet and its reply.**
///
/// `crate::protocol::ChunkEncoder`'s measurement is 62 M instructions / ≈2.4 ms
/// per column, and until this landed every one of those ran on the connection
/// task — the task that owes the acting player an answer. A wall clock cannot
/// state that here (this machine's timings reproduce to ~10.8% and several agents
/// compile concurrently), so the instrument is a **count of encodes that ran on a
/// thread which has served an inbound packet**; see [`HAS_DECODED`] for why that
/// is exact rather than a proxy.
///
/// | arm | encodes on the connection thread |
/// |---|---|
/// | no `ChunkEncoder` (the defect, and the live control below) | **every column of the view** — 361 at `view_radius = 9` |
/// | encode moved into the generating worker (the fix) | **0** |
///
/// The two arms differ in one thing: whether the protocol answers
/// `chunk_encoder()` with `Some`. Same server body, same source, same wire.
///
/// The exact-zero is what makes this a magnitude assertion rather than a
/// direction one: it is not "fewer", and no reordering of the burst can satisfy
/// it. And the view still has to arrive whole, which the last assertion checks —
/// a "fix" that dropped columns would otherwise trivially pass.
#[tokio::test]
async fn column_encode_never_runs_on_the_connection_task() {
    let view_radius = 9;
    let expected = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;

    let (at_reply, sites, observed) = measure_encode_sites(true, view_radius).await;
    assert_eq!(
        at_reply, 0,
        "{at_reply} columns had been encoded on the connection task by the time the play \
         packet was answered; with encode moved into the generating worker this is 0, and \
         under the defect it is however many chunks had gone out"
    );
    assert_eq!(
        sites.on_task(),
        0,
        "not one of the view's {expected} columns may be encoded on a thread that serves \
         inbound packets — that is the whole of `ChunkEncoder`"
    );
    assert_eq!(
        sites.off_task(),
        expected,
        "…and every column must still be encoded exactly once, on the blocking pool"
    );
    assert_eq!(observed, expected, "the whole view must still arrive");
}

/// **The live control, and it must fail the assertion above.**
///
/// A protocol that answers [`ServerProtocol::chunk_encoder`] with `None` — the
/// trait default, and what every legacy family and every other test protocol in
/// this workspace does — drives the identical `serve_connection` body down the
/// on-task encode path. If this arm ever reported zero on-task encodes, the gate
/// beside it would be measuring nothing: an encode site that is *never* on the
/// connection task regardless of the fix.
///
/// Asserted as "every column", not "some": the fallback path encodes the whole
/// view on the connection task, so a partial count would mean the two paths had
/// silently merged.
#[tokio::test]
async fn control_without_an_off_task_encoder_every_column_is_encoded_on_the_connection_task() {
    let view_radius = 9;
    let expected = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;

    let (at_reply, sites, observed) = measure_encode_sites(false, view_radius).await;
    assert_eq!(
        sites.on_task(),
        expected,
        "the control must show the defect: with no off-task encoder all {expected} columns \
         are encoded on the connection task"
    );
    assert_eq!(
        sites.off_task(),
        0,
        "and none off it — a non-zero here means the two arms are not actually different"
    );
    assert!(
        at_reply > 0,
        "the control's reply must arrive with on-task encode work already behind it, or the \
         `at_reply == 0` assertion in the fixed arm is vacuous"
    );
    assert_eq!(observed, expected, "the whole view must still arrive");
}

/// **The island gate for hunger** (issue #258): a sprinting player's exhaustion
/// reaches the wire as a falling `saturation`, then a falling `food`, on the real
/// `serve_connection` path — not just inside `crate::food`'s own unit tests, which
/// would be entirely green with nothing calling them.
///
/// # The prediction, derived rather than observed
///
/// One 250-block sprinting step charges
/// `EXHAUSTION_SPRINT_PER_BLOCK * round(250 * 100) * 0.01 = 0.1 * 25000 * 0.01 =
/// 25.0` exhaustion. The tick then spends `4.0` per tick while exhaustion is
/// **strictly** above `4.0`, taking one saturation point each time and then one food
/// point once saturation is gone:
///
/// | tick | exhaustion after | saturation | food |
/// |---|---|---|---|
/// | 1 | 21.0 | 4.0 | 20 |
/// | 5 | 5.0 | 0.0 | 20 |
/// | 6 | 1.0 | 0.0 | **19** |
/// | 7 | 1.0 — not above 4.0 | 0.0 | 19 |
///
/// So the final wire state is exactly `food = 19, saturation = 0.0`, and it settles
/// there. Six drops, not seven: `1.0` is not greater than `4.0`.
///
/// Natural regeneration is turned off, because the player is at full health here and
/// a regeneration arm would otherwise start competing for the same exhaustion as
/// soon as anything hurt them — the same reason the drowning gate above turns it off.
#[tokio::test(start_paused = true)]
async fn a_sprinting_player_loses_saturation_then_food_on_the_wire() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Sprinter", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;

    // Establish a starting position, then sprint 250 blocks in z from it. The
    // exhaustion charge needs the *previous* position, so the first move is the
    // baseline and the second is the one that costs.
    send_player_input(&mut client, true, false, false).await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;
    send_player_moved(&mut client, 8.0, 8.0, 258.0).await;

    // Collect `(food, saturation)` from every `SetHealth` until the predicted final
    // state arrives, bounded so a failure reports what it saw rather than hanging.
    let mut seen: Vec<(i32, f32)> = Vec::new();
    for _ in 0..4_000 {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        let health = r.f32().expect("health");
        let food = r.var_i32().expect("food");
        let saturation = r.f32().expect("saturation");
        assert_eq!(health, 20.0, "nothing here should damage the player");
        seen.push((food, saturation));
        if (food, saturation) == (19, 0.0) {
            break;
        }
    }

    assert!(
        seen.contains(&(20, 4.0)),
        "the first drop must cost *saturation*, not food — if the first update is \
         (19, …) then exhaustion is decrementing food directly and hunger depletes \
         five times too fast. Saw: {seen:?}"
    );
    assert_eq!(
        seen.last().copied(),
        Some((19, 0.0)),
        "25.0 exhaustion is exactly six 4.0 drops: five of saturation then one of \
         food. Saw: {seen:?}"
    );
    assert!(
        !seen.contains(&(18, 0.0)),
        "a seventh drop would mean the threshold test is `>=` rather than `>`, since \
         1.0 exhaustion is left over. Saw: {seen:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The control**, and the reason the gate above is about *sprinting* rather than
/// about "moving costs food": the identical 250-block step with the sprint flag off
/// must cost **nothing at all**. Vanilla's walking branch is a literal `0.0F`
/// multiply.
///
/// Asserted as an absence, so it needs the detector to be known-working — and it is,
/// by construction: the gate above uses the same harness, the same source and the
/// same read loop, and does see updates. Here a bounded read must see none.
#[tokio::test(start_paused = true)]
async fn the_same_step_while_walking_costs_no_hunger_at_all() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Walker", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;

    // No `send_player_input`, so `sprinting` stays at its `false` default.
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;
    send_player_moved(&mut client, 8.0, 8.0, 258.0).await;

    // The window is **measured, not assumed**. This reads until the server closes the
    // connection (it eventually does: nothing here answers a keep-alive, so the
    // keep-alive timeout fires), and counts the packets that did arrive. A control
    // asserting an absence is only as good as the evidence something *would* have
    // shown up, so the packet count is asserted too — an empty stream would
    // otherwise pass this vacuously.
    let mut updates: Vec<(i32, f32)> = Vec::new();
    let mut observed = 0usize;
    for _ in 0..4_000 {
        let Ok(Some((id, payload))) = client.read_packet().await else {
            break;
        };
        observed += 1;
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        let _health = r.f32().expect("health");
        updates.push((
            r.var_i32().expect("food"),
            r.f32().expect("saturation"),
        ));
    }
    assert!(
        observed > 20,
        "the window must actually span some server traffic, or this control measures \
         nothing; saw {observed} packets"
    );
    assert!(
        updates.is_empty(),
        "walking is a 0.0F multiply in vanilla, so nothing may change: {updates:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// A world of lava, for the burning gate. Mirrors [`WaterSource`]'s shape exactly.
struct LavaSource;

impl ChunkSource for LavaSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:lava");
                }
            }
        }
        col
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

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// **The island gate for burning** (issue #269): a player standing in lava takes
/// lava's damage on the real `serve_connection` path, and dies to it.
///
/// # The prediction, derived from the constants
///
/// `Entity.lavaHurt` is **`4.0` per tick**, and the burn tick's own `1.0` is suppressed
/// by `baseTick`'s `!isInLava()` guard. So from full health the sequence on the wire is
/// `16.0, 12.0, 8.0, 4.0, 0.0` — **five ticks to death**, and every value a multiple
/// of 4.
///
/// The wrong hypotheses, both separated:
///
/// | hypothesis | first health value |
/// |---|---|
/// | lava alone (correct) | **16.0** |
/// | lava plus the unguarded burn tick | 15.0 |
/// | burn tick alone | 19.0 |
///
/// `natural_health_regeneration` is off so nothing heals between hits, which would
/// otherwise make the sequence depend on the race rather than on the damage.
#[tokio::test(start_paused = true)]
async fn a_player_standing_in_lava_burns_at_four_damage_per_tick() {
    let (client_end, server_end) = memory_pair();
    let source = LavaSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Diver", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    let mut healths: Vec<f32> = Vec::new();
    for _ in 0..2_000 {
        let Ok(Some((id, payload))) = client.read_packet().await else {
            break;
        };
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        let health = r.f32().expect("health");
        if healths.last().copied() != Some(health) {
            healths.push(health);
        }
        if health <= 0.0 {
            break;
        }
    }

    assert_eq!(
        healths.first().copied(),
        Some(16.0),
        "lava is 4.0 per tick and the burn tick's own 1.0 is suppressed while in it — \
         15.0 would mean the !isInLava guard is missing, 19.0 that lava's contact \
         damage is. Saw: {healths:?}"
    );
    assert_eq!(
        healths,
        vec![16.0, 12.0, 8.0, 4.0, 0.0],
        "five ticks of 4.0 from full health, every value a multiple of 4: {healths:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The control**: the identical run in an all-air world must produce no health
/// update at all. Without it, the gate above is satisfied by anything that damages a
/// player on a timer.
///
/// The window is measured by packet count for the reason the walking control gives.
#[tokio::test(start_paused = true)]
async fn a_player_standing_in_air_never_burns() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Bystander", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    let mut updates: Vec<f32> = Vec::new();
    let mut observed = 0usize;
    for _ in 0..2_000 {
        let Ok(Some((id, payload))) = client.read_packet().await else {
            break;
        };
        observed += 1;
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        updates.push(r.f32().expect("health"));
    }
    assert!(
        observed > 20,
        "the window must span real traffic or this control measures nothing; saw \
         {observed} packets"
    );
    assert!(
        updates.is_empty(),
        "an air world must not burn anyone: {updates:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The boat-dismount gate — the last open symptom tracked by issue #11.**
///
/// The chain that was broken: the client already sent the sneak bit every
/// tick, `server_protocol.rs`'s `PLAYER_INPUT` decode arm only read bit
/// `0x40` (sprint) and never `0x20` (shift), `ServerBound::PlayerInput` had
/// no field to carry it, and `MobSim::dismount_rider` — the mechanism that
/// actually removes a rider — had zero callers anywhere in the tree. This
/// drives the real `dispatch_play_packet` path (not `dismount_rider`
/// directly, which would pass whether or not the server ever called it) and
/// asserts both the sim state and the wire.
///
/// Mounting goes through the real `MobSim` API rather than a wire-level
/// `INTERACT_ENTITY` (this file's stand-in protocol has no decode arm for
/// that packet, and boarding wiring is pre-existing, not part of this fix) —
/// only the dismount half is under test.
///
/// `Player.rideTick`'s `wantsToStopRiding()` is exactly `isShiftKeyDown()`, a
/// **level** check run every tick a passenger is aboard, not an edge. That is
/// safe here (does not lock a sneaking player out of re-boarding) only
/// because `AbstractBoat.interact`/`mount_vehicle`'s own
/// `using_secondary_action` gate already refuses to board while sneaking —
/// ported and checked in `mount_vehicle`'s own tests. This gate's own
/// precondition assertion (`boarded` while *not* sneaking) is the other half
/// of that argument holding.
#[tokio::test(start_paused = true)]
async fn sneaking_dismounts_a_boat_on_the_wire() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Sailor", 1).await;

    let boat = mobs.with(|sim| {
        sim.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("valid key"),
            Vec3::new(8.0, 8.0, 8.0),
            0.0,
        )
    });
    let boarded = mobs.with(|sim| sim.mount_vehicle(boat, LOCAL_PLAYER_ENTITY_ID, false));
    assert!(
        boarded,
        "precondition: mounting must succeed while not sneaking"
    );
    assert_eq!(
        mobs.with(|sim| sim.vehicle_ridden_by(LOCAL_PLAYER_ENTITY_ID)),
        Some(boat),
        "precondition: the player is aboard before the sneak packet is sent"
    );
    let _ = drain_available(&mut client).await;

    // The fix under test: a real `PLAYER_INPUT` with `shift: true`, through
    // the real dispatch path.
    send_player_input(&mut client, false, true, false).await;

    let packets = drain_available(&mut client).await;
    let payload = packets
        .iter()
        .find(|(id, _)| *id == SET_PASSENGERS_S2C)
        .map(|(_, payload)| payload.clone())
        .unwrap_or_else(|| {
            panic!(
                "sneaking while aboard must send SET_PASSENGERS — the only channel a \
                 client learns it dismounted through; got packet ids {:?}",
                packets.iter().map(|(id, _)| *id).collect::<Vec<_>>()
            )
        });
    let mut r = Reader::new(&payload);
    let vehicle_id = r.var_i32().expect("vehicle id");
    let count = r.var_i32().expect("passenger count");
    assert_eq!(vehicle_id, boat, "must name the boat the player just left");
    assert_eq!(
        count, 0,
        "dismounting sends the vehicle's whole (now empty) passenger list, not a delta"
    );

    assert_eq!(
        mobs.with(|sim| sim.vehicle_ridden_by(LOCAL_PLAYER_ENTITY_ID)),
        None,
        "the sim itself must actually vacate the boat, not just announce it on the wire"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}
