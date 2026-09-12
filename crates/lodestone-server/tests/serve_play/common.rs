
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use lodestone_core::{Reader, State, Writer};
use lodestone_entity::item_entity::ItemLifecycle;
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, Difficulty, ItemStack, ResourceKey, Vec3,
};
use lodestone_net::{Connection, NetError, Transport, generate_shared_secret, memory_pair, rsa_encrypt};
use lodestone_server::{
    BlockEntityHandle, BlockTickFeed, ChunkColumn, ChunkSource, ChunkWorld, CommandDispatch,
    EntitySnapshot, ExplosionFeed, MetadataField, MobHandle, MobSim, NoEntities,
    OnlineModeConfig, PluginChannelRegistry, ResourcePackPushFeed, ServerBound, ServerDirective,
    ServerError, ServerProtocol, TicketStoreHandle, WeatherEvent, WeatherFeed, access::AccessHandle,
    serve_connection, serve_connection_with_mob_events, serve_connection_with_online_mode,
    world_state::WorldStateHandle,
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
const ENCRYPTION_REQUEST_S2C: i32 = 70;
const ENCRYPTION_RESPONSE_C2S: i32 = 71;

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
/// A refusal packet whose reason is readable from the client side.
const DISCONNECT_S2C: i32 = 90;
const CHANGE_DIFFICULTY_S2C: i32 = 49;
// The four additional packets (creative-slot writes reuse
// `PlayerInventory`/`apply_menu_slot_change` and so need
// no new wire id of their own beyond the slot write itself).
const SET_CREATIVE_MODE_SLOT_C2S: i32 = 50;
const CLIENT_COMMAND_C2S: i32 = 51;
const CLIENT_INFORMATION_C2S: i32 = 52;
const CHUNK_BATCH_RECEIVED_C2S: i32 = 53;
const GAME_RULE_VALUES_S2C: i32 = 54;
const GAME_EVENT_S2C: i32 = 55;
/// This stand-in format's block-break packet (a destroy ordinal
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
/// VarInt array. Dismounting is this packet with an empty array.
const SET_PASSENGERS_S2C: i32 = 61;
/// Stand-in post-join position sync: `f64` x/y/z then `f32` yaw/pitch.
const PLAYER_POSITION_S2C: i32 = 62;
/// A stand-in `change_game_mode` (one byte ordinal). A creative-mode slot write
/// is accepted only while the connection is in creative mode, so the item-giving
/// test enters that mode first.
const CHANGE_GAME_MODE_C2S: i32 = 58;
/// A stand-in `set_game_rule`: one VarInt entry count, then a
/// key/value string pair each.
const SET_GAME_RULE_C2S: i32 = 59;

/// A stand-in `player_input`: one byte, non-zero meaning sprinting. The real
/// v770 packet is a bitfield of movement flags; this file tests
/// `lodestone-server`'s own consumer logic, and the sprint bit is the only one it
/// reads (hunger's per-block exhaustion, `crate::food`).
const PLAYER_INPUT_C2S: i32 = 60;

/// A stand-in `ping_request`: one big-endian `i64`, matching the packet's only
/// field. `dispatch_play_packet`'s `PingRequest` arm calls
/// `encode_pong_response`, so this exercises the dispatch-and-consumer half
/// without requiring a full protocol implementation.
const PING_REQUEST_C2S: i32 = 91;
/// Stand-in pong response — the `time` value is echoed back unchanged.
const PONG_RESPONSE_S2C: i32 = 92;
/// Stand-in reply to a server-originated control ping. Its only observable
/// server effect is that the Play connection remains usable.
const PONG_C2S: i32 = 93;

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
    // retention). Explicit rather than inherited.
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
    // retention). Explicit rather than inherited.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// Stand-in protocol: the same login/configuration wire format
/// `integrated_memory.rs` uses, plus the four new keep-alive/time/view
/// encoders and the serverbound decodes under test.
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
                    // `None` is the honest lowering of that. Player-facing
                    // rotation has its own protocol-level coverage.
                    rotation: None,
                }
            }
            // A minimal stand-in wire format for the difficulty round trip —
            // a single byte ordinal with the protocol's 0..=3 semantics, but
            // not its VarInt framing. This file tests `lodestone-server`'s
            // scheduling and consumer logic, not wire fidelity.
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
            // Minimal stand-in wire formats for the four
            // additional packets — same "test scheduling, not wire
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
            // A minimal stand-in for the three block-action
            // destroy ordinals — same "test the server's own consumer logic,
            // not wire fidelity" rationale as `CHANGE_DIFFICULTY_C2S`. The
            // ordinals 0, 1, and 2 represent start, abort, and stop because
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
            State::Play if packet_id == PONG_C2S => {
                let mut r = Reader::new(payload);
                ServerBound::Pong {
                    id: r.i32().expect("pong id"),
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

    /// The login refusal has to reach the wire for its reason to be assertable
    /// at all. The stand-in's own id, like every other here.
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

    /// The `REQUEST_GAMERULE_VALUES` confirmation — encodes only the
    /// entry count (the tests below only need to distinguish "a reply
    /// arrived, with N entries" from "no reply", not round-trip the actual
    /// key/value strings).
    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        // The entries themselves, not just the count. The count alone
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

    /// Mirrors the real v770 wire shape (a byte event plus a float parameter)
    /// so the weather-drain gate can assert on actual
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

    /// A pickup tells the client which window-0 slot changed. This
    /// stand-in carries `(slot, present, item key, count)` — enough for the
    /// pickup gate to assert *which* slot was announced and what was stored in it,
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

    fn encode_teleport(&self, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.f64(x);
        w.f64(y);
        w.f64(z);
        w.f32(yaw);
        w.f32(pitch);
        ServerDirective::Send {
            packet_id: PLAYER_POSITION_S2C,
            payload: w.as_slice().to_vec(),
        }
    }
}

/// A pre-configuration-phase protocol reusing the stand-in wire format. Its
/// only distinction is the explicit capability used by the server handshake.
struct LegacyProtocol;

impl ServerProtocol for LegacyProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        FakeProtocol.decode(state, packet_id, payload)
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        FakeProtocol.login_success(username, uuid)
    }

    fn has_configuration_phase(&self) -> bool {
        false
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        FakeProtocol.begin_configuration()
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        FakeProtocol.begin_play(view_radius)
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        FakeProtocol.encode_set_time(game_time, day_time)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        FakeProtocol.begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        FakeProtocol.encode_chunk(cx, cz, column)
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        FakeProtocol.end_chunk_batch(batch_size)
    }
}

/// The same legacy wire with online-mode encryption enabled. The test keeps
/// the packet layout private because it verifies the server state machine, not
/// a particular release's encryption packet shape.
struct OnlineLegacyProtocol;

impl ServerProtocol for OnlineLegacyProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        if state == State::Login && packet_id == ENCRYPTION_RESPONSE_C2S {
            let mut reader = Reader::new(payload);
            return ServerBound::EncryptionResponse {
                shared_secret: reader
                    .var_bytes(4096)
                    .expect("encrypted shared secret")
                    .to_vec(),
                verify_token: reader
                    .var_bytes(4096)
                    .expect("encrypted verify token")
                    .to_vec(),
            };
        }
        LegacyProtocol.decode(state, packet_id, payload)
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        LegacyProtocol.login_success(username, uuid)
    }

    fn has_configuration_phase(&self) -> bool {
        false
    }

    fn encode_encryption_request(
        &self,
        public_key_der: &[u8],
        verify_token: &[u8],
    ) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_bytes(public_key_der).expect("public key length");
        writer.var_bytes(verify_token).expect("challenge length");
        ServerDirective::Send {
            packet_id: ENCRYPTION_REQUEST_S2C,
            payload: writer.as_slice().to_vec(),
        }
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        LegacyProtocol.begin_configuration()
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        LegacyProtocol.begin_play(view_radius)
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        LegacyProtocol.encode_set_time(game_time, day_time)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        LegacyProtocol.begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        LegacyProtocol.encode_chunk(cx, cz, column)
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        LegacyProtocol.end_chunk_batch(batch_size)
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

    // The join-time full clock sync precedes chunk streaming.
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
    // The marker closing the batch containing the last column.
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
/// not actually due. `VITALS_TICK_INTERVAL` is *also* 50ms (matching the
/// per-tick cadence — see `crate::vitals`'s module docs, not duplicated here
/// since this crate is `lodestone-server`'s *caller*), so
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

/// Sends one block-break phase. `ordinal` is the destroy
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
    // `SET_CREATIVE_MODE_SLOT` is creative-only server-side, so the write is
    // bracketed by a switch into
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

/// Sends the stand-in `pong` acknowledgement (one big-endian `i32`).
async fn send_pong(client: &mut Connection<DuplexStream>, id: i32) {
    let mut w = Writer::default();
    w.i32(id);
    client
        .write_packet(PONG_C2S, w.as_slice())
        .await
        .expect("send pong");
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
/// current game-rule values — the two ordinals the consumer acts on).
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
/// see a *second* batch, matching the "one batch in flight" wire contract
/// (see `ViewTracker`/`send_view_update`'s own doc comments).
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
