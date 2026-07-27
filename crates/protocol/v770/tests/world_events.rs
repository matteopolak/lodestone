//! Hermetic tests for the protocol 776 world-event packets `game_event`,
//! `set_default_spawn_position`, `player_abilities`, and `level_event`.
//!
//! Clientbound golden byte vectors are hand-built from the wire specification
//! (behavioural reference only), so a symmetric encode/decode bug cannot pass
//! silently. Packed block positions are built with an independent bit-packing
//! helper so the adapter's unpacking is pinned against a separate implementation.

use lodestone_model::{
    BlockPos, ClientEvent, ConnectionState, Directive, GameMode, VersionAdapter,
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
fn game_event_unhandled_code_consumes_bytes_without_directive() {
    let adapter = V770Adapter::new();
    // WIN_GAME (4) is fully decoded but produces no canonical event.
    let directives = handle(
        &adapter,
        play::clientbound::GAME_EVENT,
        &game_event_bytes(4, 0.0),
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
