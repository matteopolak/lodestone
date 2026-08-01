//! Hermetic tests for protocol 776 death/respawn handling.
//!
//! Golden serverbound/clientbound byte vectors are hand-built from the wire
//! specification so a symmetric encode/decode bug cannot pass silently.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{
    BlockPos, ClientAction, ClientEvent, ConnectionState, DeathLocation, Directive, GameMode,
    VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::game::{ClientCommand, SetHealth};
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 776 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    T::decode(&mut reader, CTX).expect("decode")
}

#[test]
fn set_health_round_trips_against_golden_bytes() {
    // health 20.0 (0x41A00000), food 20 (varint 0x14), saturation 5.0 (0x40A00000).
    let golden = [0x41, 0xA0, 0x00, 0x00, 0x14, 0x40, 0xA0, 0x00, 0x00];
    let body = SetHealth {
        health: 20.0,
        food: 20,
        saturation: 5.0,
    };
    assert_eq!(encode(&body), golden);
    let decoded: SetHealth = decode(&golden);
    assert_eq!(decoded, body);
}

#[test]
fn client_command_perform_respawn_is_single_zero_byte() {
    // Action enum ordinal: PERFORM_RESPAWN = 0, encoded as a VarInt.
    let body = ClientCommand { action: 0 };
    assert_eq!(encode(&body), [0x00]);
    let decoded: ClientCommand = decode(&[0x00]);
    assert_eq!(decoded, body);
}

#[test]
fn handle_play_set_health_emits_health_changed() {
    let adapter = V770Adapter::new();
    let payload = [0x41, 0xA0, 0x00, 0x00, 0x14, 0x40, 0xA0, 0x00, 0x00];
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_HEALTH,
            &payload,
        )
        .expect("handle set_health");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::HealthChanged {
            health: 20.0,
            food: 20,
            saturation: 5.0,
        })]
    );
}

#[test]
fn handle_play_combat_kill_emits_death_with_message() {
    let adapter = V770Adapter::new();
    // VarInt player id 1, then a network-NBT bare-string component "You died".
    let mut payload = vec![0x01u8, 0x08, 0x00, 0x08];
    payload.extend_from_slice(b"You died");
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::PLAYER_COMBAT_KILL,
            &payload,
        )
        .expect("handle combat_kill");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Death { message })] => {
            assert_eq!(message.to_plain_string(), "You died");
        }
        other => panic!("expected a single Death event, got {other:?}"),
    }
}

#[test]
fn encode_action_respawn_targets_client_command() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(ConnectionState::Play, &ClientAction::Respawn)
        .expect("encode respawn");
    assert_eq!(
        encoded,
        Some((play::serverbound::CLIENT_COMMAND, vec![0x00]))
    );
}

#[test]
fn handle_play_respawn_emits_dimension_and_game_mode_without_death_location() {
    // dimension_type=0, dimension="minecraft:the_nether", seed=1,
    // game_type=1 (creative), previous_game_type=-1 (none), is_debug=false,
    // is_flat=false, last_death_location=None, portal_cooldown=0,
    // sea_level=63, data_to_keep=0. Bytes hand-built from
    // `ClientboundRespawnPacket`'s `CommonPlayerSpawnInfo`-derived wire shape.
    let golden: &[u8] = &[
        0x00, 0x14, 0x6D, 0x69, 0x6E, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3A, 0x74, 0x68, 0x65,
        0x5F, 0x6E, 0x65, 0x74, 0x68, 0x65, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x01, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x3F, 0x00,
    ];
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::RESPAWN,
            golden,
        )
        .expect("handle respawn");
    // Since issue #288 a respawn emits the dimension **type** first, then the
    // `Respawned` event. `None` here because this test feeds no `registry_data`,
    // which is also the assertion that the holder id is not defaulted.
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::DimensionTypeChanged {
                dimension_type: None,
                ..
            }),
            Directive::Emit(ClientEvent::Respawned {
                dimension,
                game_mode,
                previous_game_mode,
                last_death_location,
            }),
        ] => {
            assert_eq!(dimension.to_string(), "minecraft:the_nether");
            assert_eq!(*game_mode, GameMode::Creative);
            assert_eq!(*previous_game_mode, None);
            assert_eq!(*last_death_location, None);
        }
        other => panic!("expected DimensionTypeChanged then Respawned, got {other:?}"),
    }
}

#[test]
fn handle_play_respawn_emits_death_location_and_previous_game_mode() {
    // dimension_type=1, dimension="minecraft:overworld", seed=-42,
    // game_type=0 (survival), previous_game_type=0 (survival), is_debug=true,
    // is_flat=false, last_death_location=Some("minecraft:overworld",
    // packed 123456789 -> BlockPos { x: 0, y: -747, z: 30140 }),
    // portal_cooldown=10, sea_level=64, data_to_keep=0.
    let golden: &[u8] = &[
        0x01, 0x13, 0x6D, 0x69, 0x6E, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3A, 0x6F, 0x76, 0x65,
        0x72, 0x77, 0x6F, 0x72, 0x6C, 0x64, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xD6, 0x00,
        0x00, 0x01, 0x00, 0x01, 0x13, 0x6D, 0x69, 0x6E, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3A,
        0x6F, 0x76, 0x65, 0x72, 0x77, 0x6F, 0x72, 0x6C, 0x64, 0x00, 0x00, 0x00, 0x00, 0x07, 0x5B,
        0xCD, 0x15, 0x0A, 0x40, 0x00,
    ];
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::RESPAWN,
            golden,
        )
        .expect("handle respawn");
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::DimensionTypeChanged {
                dimension_type: None,
                ..
            }),
            Directive::Emit(ClientEvent::Respawned {
                dimension,
                game_mode,
                previous_game_mode,
                last_death_location,
            }),
        ] => {
            assert_eq!(dimension.to_string(), "minecraft:overworld");
            assert_eq!(*game_mode, GameMode::Survival);
            assert_eq!(*previous_game_mode, Some(GameMode::Survival));
            assert_eq!(
                *last_death_location,
                Some(DeathLocation {
                    dimension: "minecraft:overworld".parse().unwrap(),
                    pos: BlockPos {
                        x: 0,
                        y: -747,
                        z: 30140,
                    },
                })
            );
        }
        other => panic!("expected DimensionTypeChanged then Respawned, got {other:?}"),
    }
}

#[test]
fn handle_play_respawn_rejects_trailing_bytes() {
    let mut golden = vec![
        0x00, 0x14, 0x6D, 0x69, 0x6E, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3A, 0x74, 0x68, 0x65,
        0x5F, 0x6E, 0x65, 0x74, 0x68, 0x65, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x01, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x3F, 0x00,
    ];
    golden.push(0xAA); // trailing garbage byte
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::RESPAWN,
        &golden,
    );
    assert!(result.is_err(), "trailing bytes must be rejected");
}

#[test]
fn handle_play_respawn_rejects_truncated_payload() {
    // Cut off partway through the dimension identifier string.
    let golden: &[u8] = &[0x00, 0x14, 0x6D, 0x69, 0x6E];
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::RESPAWN,
        golden,
    );
    assert!(result.is_err(), "truncated payload must be rejected");
}
