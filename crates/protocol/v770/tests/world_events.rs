//! Hermetic tests for the protocol 776 world-event packets `game_event`,
//! `set_default_spawn_position`, `player_abilities`, `level_event`,
//! `block_event`, `block_destruction`, `block_changed_ack`,
//! `set_chunk_cache_center`, `set_chunk_cache_radius`,
//! `set_simulation_distance`, and `change_difficulty`.
//!
//! Clientbound golden byte vectors are hand-built from the wire specification
//! (behavioural reference only), so a symmetric encode/decode bug cannot pass
//! silently. Packed block positions are built with an independent bit-packing
//! helper so the adapter's unpacking is pinned against a separate implementation.

use lodestone_model::{
    BlockPos, ClientEvent, ConnectionState, Difficulty, Directive, GameMode, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

/// Independently packs block coordinates the way vanilla `BlockPos.asLong`
/// does: `x` in the high 26 bits, `z` in the middle 26 bits, `y` in the low 12.
fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    let x = (i64::from(x)) & 0x3FF_FFFF;
    let y = (i64::from(y)) & 0xFFF;
    let z = (i64::from(z)) & 0x3FF_FFFF;
    (x << 38) | (z << 12) | y
}

fn handle(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
}

/// Independent VarInt encoder (not the codec under test).
fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

// ---- game_event -----------------------------------------------------------

fn game_event_bytes(event: u8, param: f32) -> Vec<u8> {
    let mut bytes = vec![event];
    bytes.extend_from_slice(&param.to_be_bytes());
    bytes
}

#[test]
fn game_event_start_raining_emits_weather_started() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(1, 0.0),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::WeatherChanged {
            raining: Some(true),
            rain_level: None,
            thunder_level: None,
        })]
    );
}

#[test]
fn game_event_stop_raining_emits_weather_stopped() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(2, 0.0),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::WeatherChanged {
            raining: Some(false),
            rain_level: None,
            thunder_level: None,
        })]
    );
}

#[test]
fn game_event_change_game_mode_maps_ordinal() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(3, 1.0),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::GameModeChanged {
            game_mode: GameMode::Creative,
        })]
    );
}

#[test]
fn game_event_rain_and_thunder_levels_are_surfaced() {
    let adapter = V770Adapter::new();
    let rain = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(7, 0.5),
    );
    assert_eq!(
        rain,
        vec![Directive::Emit(ClientEvent::WeatherChanged {
            raining: None,
            rain_level: Some(0.5),
            thunder_level: None,
        })]
    );
    let thunder = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(8, 0.25),
    );
    assert_eq!(
        thunder,
        vec![Directive::Emit(ClientEvent::WeatherChanged {
            raining: None,
            rain_level: None,
            thunder_level: Some(0.25),
        })]
    );
}

#[test]
fn game_event_win_game_emits_win_game_event() {
    // WIN_GAME is event code 4 (`ClientboundGameEventPacket.java:18`,
    // `WIN_GAME = new ClientboundGameEventPacket.Type(4)`), the packet vanilla
    // sends on exiting the End through the exit portal
    // (`ClientPacketListener.java:1548-1552` always opens `WinScreen(true, ..)`
    // regardless of `param` — see `ClientEvent::WinGame`'s own doc for why the
    // event therefore carries no fields).
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(4, 0.0),
    );
    assert_eq!(directives, vec![Directive::Emit(ClientEvent::WinGame)]);
}

#[test]
fn game_event_unhandled_code_consumes_bytes_without_directive() {
    let adapter = V770Adapter::new();
    // DEMO_EVENT (5, `ClientboundGameEventPacket.java:19`) is fully decoded
    // but produces no canonical event — unlike WIN_GAME (4), which now does
    // (see `game_event_win_game_emits_win_game_event` above).
    let directives = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(5, 0.0),
    );
    assert!(directives.is_empty());
}

#[test]
fn game_event_change_game_mode_with_invalid_ordinal_is_ignored() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(3, -1.0),
    );
    assert!(directives.is_empty());
}

#[test]
fn game_event_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = game_event_bytes(1, 0.0);
    payload.push(0xAB);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::GAME_EVENT,
        &payload,
    );
    assert!(result.is_err(), "a misaligned game_event must be rejected");
}

// ---- set_default_spawn_position ------------------------------------------

fn spawn_bytes(dimension: &str, packed: i64, yaw: f32, pitch: f32) -> Vec<u8> {
    let mut bytes = vec![dimension.len() as u8];
    bytes.extend_from_slice(dimension.as_bytes());
    bytes.extend_from_slice(&packed.to_be_bytes());
    bytes.extend_from_slice(&yaw.to_be_bytes());
    bytes.extend_from_slice(&pitch.to_be_bytes());
    bytes
}

#[test]
fn spawn_position_emits_unpacked_block_pos_and_yaw() {
    let adapter = V770Adapter::new();
    let packed = pack_block_pos(1, 64, 2);
    let directives = handle(
        &adapter,
        play::clientbound::SET_DEFAULT_SPAWN_POSITION,
        &spawn_bytes("minecraft:overworld", packed, 90.0, 45.0),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
            dimension: "minecraft:overworld".parse().unwrap(),
            pos: BlockPos { x: 1, y: 64, z: 2 },
            angle: 90.0,
            pitch: 45.0,
        })]
    );
}

#[test]
fn spawn_position_unpacks_negative_coordinates() {
    let adapter = V770Adapter::new();
    let packed = pack_block_pos(-1, -5, -3);
    let directives = handle(
        &adapter,
        play::clientbound::SET_DEFAULT_SPAWN_POSITION,
        &spawn_bytes("minecraft:the_nether", packed, 0.0, 0.0),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
            dimension: "minecraft:the_nether".parse().unwrap(),
            pos: BlockPos {
                x: -1,
                y: -5,
                z: -3
            },
            angle: 0.0,
            pitch: 0.0,
        })]
    );
}

#[test]
fn spawn_position_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = spawn_bytes("minecraft:overworld", 0, 0.0, 0.0);
    payload.truncate(payload.len() - 1); // drop a pitch byte
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_DEFAULT_SPAWN_POSITION,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated spawn packet must error, not panic"
    );
}

// ---- player_abilities -----------------------------------------------------

fn abilities_bytes(flags: u8, flying_speed: f32, walking_speed: f32) -> Vec<u8> {
    let mut bytes = vec![flags];
    bytes.extend_from_slice(&flying_speed.to_be_bytes());
    bytes.extend_from_slice(&walking_speed.to_be_bytes());
    bytes
}

#[test]
fn player_abilities_decodes_flags_and_speeds() {
    let adapter = V770Adapter::new();
    // invulnerable | can_fly = 0x01 | 0x04 = 0x05
    let directives = handle(
        &adapter,
        play::clientbound::PLAYER_ABILITIES,
        &abilities_bytes(0x05, 0.05, 0.1),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::AbilitiesChanged {
            invulnerable: true,
            flying: false,
            can_fly: true,
            instabuild: false,
            flying_speed: 0.05,
            walking_speed: 0.1,
        })]
    );
}

#[test]
fn player_abilities_all_flags_set() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::PLAYER_ABILITIES,
        &abilities_bytes(0x0F, 0.05, 0.1),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::AbilitiesChanged {
            invulnerable: true,
            flying: true,
            can_fly: true,
            instabuild: true,
            flying_speed: 0.05,
            walking_speed: 0.1,
        })]
    );
}

#[test]
fn player_abilities_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = abilities_bytes(0x00, 0.05, 0.1);
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::PLAYER_ABILITIES,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned player_abilities must be rejected"
    );
}

// ---- level_event ----------------------------------------------------------

fn level_event_bytes(event: i32, packed: i64, data: i32, global: bool) -> Vec<u8> {
    let mut bytes = event.to_be_bytes().to_vec();
    bytes.extend_from_slice(&packed.to_be_bytes());
    bytes.extend_from_slice(&data.to_be_bytes());
    bytes.push(u8::from(global));
    bytes
}

#[test]
fn level_event_emits_positioned_event() {
    let adapter = V770Adapter::new();
    let packed = pack_block_pos(10, 70, -20);
    // 2001 = block-break-with-sound, data carries the block state id.
    let directives = handle(
        &adapter,
        play::clientbound::LEVEL_EVENT,
        &level_event_bytes(2001, packed, 42, false),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::LevelEvent {
            event: 2001,
            pos: BlockPos {
                x: 10,
                y: 70,
                z: -20
            },
            data: 42,
            global: false,
        })]
    );
}

#[test]
fn level_event_global_flag_is_surfaced() {
    let adapter = V770Adapter::new();
    // 1023 = wither spawn (global).
    let directives = handle(
        &adapter,
        play::clientbound::LEVEL_EVENT,
        &level_event_bytes(1023, 0, 0, true),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::LevelEvent {
            event: 1023,
            pos: BlockPos { x: 0, y: 0, z: 0 },
            data: 0,
            global: true,
        })]
    );
}

#[test]
fn level_event_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = level_event_bytes(2001, 0, 0, false);
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::LEVEL_EVENT,
        &payload,
    );
    assert!(result.is_err(), "a misaligned level_event must be rejected");
}

// ---- block_event ------------------------------------------------------

/// `block_event`'s trailing VarInt is a `registry(BLOCK)` id — registration
/// order, `air` is 0 — and `minecraft:note_block` is **109** there.
///
/// # This test used to encode 640 and still pass
///
/// 640 is note_block's index in the *alphabetical* `blocks.json` key order, and
/// `block_type_name` used to index that alphabetical table with a registry id.
/// The test and the decoder shared the mistake, so the pair round-tripped
/// perfectly while every real `block_event` on the wire named the wrong block: a
/// server's note block (109) decoded as `minecraft:blue_glazed_terracotta`, and
/// this test's 640 decodes as `minecraft:acacia_fence`. Neither id is a free
/// choice — 109 is fixed by `generated/reports/registries.json`, outside this
/// crate.
#[test]
fn block_event_emits_pos_params_and_block_name() {
    let adapter = V770Adapter::new();
    let mut payload = pack_block_pos(3, 10, -4).to_be_bytes().to_vec();
    payload.push(0); // note-block instrument byte
    payload.push(6); // note pitch byte
    payload.extend_from_slice(&var_i32(109)); // minecraft:note_block, registry id
    let directives = handle(&adapter, play::clientbound::BLOCK_EVENT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BlockEvent {
            pos: BlockPos { x: 3, y: 10, z: -4 },
            b0: 0,
            b1: 6,
            block: "minecraft:note_block".parse().unwrap(),
        })]
    );
}

/// The negative control for the test above: the alphabetical index must *not*
/// resolve to the block it names in that ordering. Without this, reverting
/// `block_type_name` to index the name-sorted table would make the assertion
/// above fail — but only for as long as someone remembers which of the two ids
/// is the wire's. Pinning both directions makes the id spaces impossible to
/// re-conflate silently.
#[test]
fn block_event_does_not_read_the_alphabetical_block_index() {
    let adapter = V770Adapter::new();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&var_i32(640)); // note_block's *alphabetical* index
    let directives = handle(&adapter, play::clientbound::BLOCK_EVENT, &payload);
    let [Directive::Emit(ClientEvent::BlockEvent { block, .. })] = directives.as_slice() else {
        panic!("expected a single block event, got {directives:?}");
    };
    assert_eq!(
        block.to_string(),
        "minecraft:acacia_fence",
        "registry id 640 is acacia_fence; reading it as note_block means the \
         alphabetical block-name table is being indexed with a registry id again"
    );
}

#[test]
fn block_event_rejects_unknown_block_id() {
    let adapter = V770Adapter::new();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&var_i32(1_000_000)); // far out of range
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::BLOCK_EVENT,
        &payload,
    );
    assert!(result.is_err(), "an unknown block id must be rejected");
}

#[test]
fn block_event_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&var_i32(0));
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::BLOCK_EVENT,
        &payload,
    );
    assert!(result.is_err(), "a misaligned block_event must be rejected");
}

#[test]
fn block_event_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.push(0); // missing b1 and block id
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::BLOCK_EVENT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated block_event must be rejected, not panic"
    );
}

// ---- block_destruction --------------------------------------------------

#[test]
fn block_destruction_emits_entity_pos_and_stage() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(7); // breaker entity id
    payload.extend_from_slice(&pack_block_pos(1, 2, 3).to_be_bytes());
    payload.push(5); // break stage
    let directives = handle(&adapter, play::clientbound::BLOCK_DESTRUCTION, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BlockDestruction {
            entity_id: 7,
            pos: BlockPos { x: 1, y: 2, z: 3 },
            progress: 5,
        })]
    );
}

#[test]
fn block_destruction_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&pack_block_pos(0, 0, 0).to_be_bytes());
    payload.push(0);
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::BLOCK_DESTRUCTION,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned block_destruction must be rejected"
    );
}

// ---- block_changed_ack ---------------------------------------------------

#[test]
fn block_changed_ack_emits_sequence() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::BLOCK_CHANGED_ACK, &var_i32(99));
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BlockChangedAck {
            sequence: 99
        })]
    );
}

#[test]
fn block_changed_ack_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::BLOCK_CHANGED_ACK,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned block_changed_ack must be rejected"
    );
}

// ---- set_chunk_cache_center / radius / simulation_distance ---------------

#[test]
fn set_chunk_cache_center_emits_coordinates() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(12);
    payload.extend_from_slice(&var_i32(-7));
    let directives = handle(
        &adapter,
        play::clientbound::SET_CHUNK_CACHE_CENTER,
        &payload,
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ChunkCacheCenterChanged {
            x: 12,
            z: -7,
        })]
    );
}

#[test]
fn set_chunk_cache_center_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(0);
    payload.extend_from_slice(&var_i32(0));
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_CHUNK_CACHE_CENTER,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_chunk_cache_center must be rejected"
    );
}

#[test]
fn set_chunk_cache_radius_emits_radius() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::SET_CHUNK_CACHE_RADIUS,
        &var_i32(16),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ChunkCacheRadiusChanged {
            radius: 16
        })]
    );
}

#[test]
fn set_chunk_cache_radius_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_CHUNK_CACHE_RADIUS,
        &[0x80], // continuation bit set, no following byte
    );
    assert!(
        result.is_err(),
        "a truncated set_chunk_cache_radius must be rejected, not panic"
    );
}

#[test]
fn set_simulation_distance_emits_distance() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::SET_SIMULATION_DISTANCE,
        &var_i32(10),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SimulationDistanceChanged {
            distance: 10
        })]
    );
}

#[test]
fn set_simulation_distance_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(5);
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_SIMULATION_DISTANCE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_simulation_distance must be rejected"
    );
}

// ---- change_difficulty ----------------------------------------------------

#[test]
fn change_difficulty_decodes_each_known_id() {
    let adapter = V770Adapter::new();
    let cases = [
        (0u8, Difficulty::Peaceful),
        (1, Difficulty::Easy),
        (2, Difficulty::Normal),
        (3, Difficulty::Hard),
    ];
    for (id, expected) in cases {
        let payload = [id, 1]; // difficulty id, locked = true
        let directives = handle(&adapter, play::clientbound::CHANGE_DIFFICULTY, &payload);
        assert_eq!(
            directives,
            vec![Directive::Emit(ClientEvent::DifficultyChanged {
                difficulty: expected,
                locked: true,
            })]
        );
    }
}

#[test]
fn change_difficulty_rejects_unknown_id() {
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::CHANGE_DIFFICULTY,
        &[4, 0],
    );
    assert!(result.is_err(), "an unknown difficulty id must be rejected");
}

#[test]
fn change_difficulty_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::CHANGE_DIFFICULTY,
        &[2, 0, 0xFF],
    );
    assert!(
        result.is_err(),
        "a misaligned change_difficulty must be rejected"
    );
}
