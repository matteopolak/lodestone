//! Literal-body seam tests for the 1.9-era explosion, world-state, and
//! textual attribute packets.
//!
//! The fixtures do not use the packet codecs under test to construct their
//! bodies: each byte order, fixed count, and modifier prefix is asserted at
//! the adapter boundary for all four protocol tables.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, VersionAdapter,
};
use lodestone_v1_9::adapter::{
    PROTOCOL_1_10_2, PROTOCOL_1_11_2, PROTOCOL_1_12_2, PROTOCOL_1_9_4, adapter_for,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

fn clientbound_ids() -> [(i32, i32, i32, i32); 4] {
    [
        (
            PROTOCOL_1_9_4,
            lodestone_v1_9::packet_ids_110::play::clientbound::EXPLOSION,
            lodestone_v1_9::packet_ids_110::play::clientbound::GAME_STATE_CHANGE,
            lodestone_v1_9::packet_ids_110::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
        (
            PROTOCOL_1_10_2,
            lodestone_v1_9::packet_ids_210::play::clientbound::EXPLOSION,
            lodestone_v1_9::packet_ids_210::play::clientbound::GAME_STATE_CHANGE,
            lodestone_v1_9::packet_ids_210::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
        (
            PROTOCOL_1_11_2,
            lodestone_v1_9::packet_ids_316::play::clientbound::EXPLOSION,
            lodestone_v1_9::packet_ids_316::play::clientbound::GAME_STATE_CHANGE,
            lodestone_v1_9::packet_ids_316::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
        (
            PROTOCOL_1_12_2,
            lodestone_v1_9::packet_ids::play::clientbound::EXPLOSION,
            lodestone_v1_9::packet_ids::play::clientbound::GAME_STATE_CHANGE,
            lodestone_v1_9::packet_ids::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
    ]
}

fn dispatch(protocol: i32, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    dispatch_into(&mut World::new(), protocol, packet_id, payload)
}

fn dispatch_into(
    world: &mut World,
    protocol: i32,
    packet_id: i32,
    payload: &[u8],
) -> Vec<Directive> {
    adapter_for(protocol)
        .handle_packet(world, ConnectionState::Play, packet_id, payload)
        .expect("known literal packet decodes")
}

fn game_state(reason: u8, value: f32) -> [u8; 5] {
    let mut bytes = [0; 5];
    bytes[0] = reason;
    bytes[1..].copy_from_slice(&value.to_be_bytes());
    bytes
}

#[test]
fn explosion_lifts_legacy_offsets_and_always_present_knockback_in_every_protocol() {
    // centre=(1.5,-2.25,3.75), radius=4, two signed-byte offsets, then a
    // zero local-player impulse. A zero is still `Some`: these revisions put
    // all three floats on the wire unconditionally.
    let mut payload = Vec::new();
    payload.extend(1.5f32.to_be_bytes());
    payload.extend((-2.25f32).to_be_bytes());
    payload.extend(3.75f32.to_be_bytes());
    payload.extend(4.0f32.to_be_bytes());
    payload.extend(2i32.to_be_bytes());
    payload.extend([1, -2i8 as u8, 3]);
    payload.extend([-4i8 as u8, 5, -6i8 as u8]);
    payload.extend(0.0f32.to_be_bytes());
    payload.extend(0.0f32.to_be_bytes());
    payload.extend(0.0f32.to_be_bytes());

    for (protocol, explosion_id, _, _) in clientbound_ids() {
        match dispatch(protocol, explosion_id, &payload).as_slice() {
            [Directive::Emit(ClientEvent::Explosion {
                pos,
                radius,
                affected_blocks,
                knockback,
            })] => {
                assert_eq!((pos.x, pos.y, pos.z), (1.5, -2.25, 3.75));
                assert_eq!(*radius, 4.0);
                assert_eq!(affected_blocks, &vec![[1, -2, 3], [-4, 5, -6]]);
                let knockback = knockback.expect("legacy packet always carries motion");
                assert_eq!((knockback.x, knockback.y, knockback.z), (0.0, 0.0, 0.0));
            }
            other => panic!("protocol {protocol}: expected explosion, got {other:?}"),
        }
    }
}

#[test]
fn explosion_removes_loaded_blocks_at_floor_centre_plus_signed_offset_only() {
    let air = lodestone_data::block_states::air_state_id();
    let mut world = World::new();
    world.load(
        ChunkPos::new(0, 0),
        LoadedChunk::new(
            ChunkColumn::new(0, 16, PaletteKind::block_states(), PaletteKind::biomes(), air, 0),
            ColumnLight::new(16),
            Heightmaps::new(),
            Vec::new(),
        ),
    );
    // floor(10.8, 64.2, 11.6) + (-2, 1, 3) = (8, 65, 14).
    world.set_block(8, 65, 14, air + 1);
    world.set_block(8, 65, 13, air + 1);

    let mut payload = Vec::new();
    payload.extend(10.8f32.to_be_bytes());
    payload.extend(64.2f32.to_be_bytes());
    payload.extend(11.6f32.to_be_bytes());
    payload.extend(2.0f32.to_be_bytes());
    payload.extend(1i32.to_be_bytes());
    payload.extend([-2i8 as u8, 1, 3]);
    payload.extend(0.0f32.to_be_bytes());
    payload.extend(0.0f32.to_be_bytes());
    payload.extend(0.0f32.to_be_bytes());

    let directives = dispatch_into(
        &mut world,
        PROTOCOL_1_12_2,
        lodestone_v1_9::packet_ids::play::clientbound::EXPLOSION,
        &payload,
    );
    assert!(matches!(directives.as_slice(), [Directive::Emit(ClientEvent::Explosion { .. })]));
    assert_eq!(world.block_state_at(8, 65, 14), Some(air));
    assert_eq!(world.block_state_at(8, 65, 13), Some(air + 1));
}

#[test]
fn game_state_reasons_one_two_three_seven_and_eight_reach_model_events() {
    for (protocol, _, game_state_id, _) in clientbound_ids() {
        let cases = [
            (
                game_state(1, 0.0),
                ClientEvent::WeatherChanged {
                    raining: Some(false),
                    rain_level: None,
                    thunder_level: None,
                },
            ),
            (
                game_state(2, 0.0),
                ClientEvent::WeatherChanged {
                    raining: Some(true),
                    rain_level: None,
                    thunder_level: None,
                },
            ),
            (
                game_state(3, 2.0),
                ClientEvent::GameModeChanged {
                    game_mode: GameMode::Adventure,
                },
            ),
            (
                game_state(7, 0.25),
                ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: Some(0.25),
                    thunder_level: None,
                },
            ),
            (
                game_state(8, 0.75),
                ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: None,
                    thunder_level: Some(0.75),
                },
            ),
        ];
        for (payload, expected) in cases {
            assert_eq!(
                dispatch(protocol, game_state_id, &payload),
                vec![Directive::Emit(expected)],
                "protocol {protocol}"
            );
        }
    }
}

#[test]
fn textual_attributes_map_keys_preserve_modifiers_and_skip_extensions() {
    // Entity id 300 as a VarInt, followed by a fixed i32 property count.
    let mut payload = vec![0xac, 0x02];
    payload.extend(2i32.to_be_bytes());
    payload.push(21); // "generic.movementSpeed"
    payload.extend(b"generic.movementSpeed");
    payload.extend(0.25f64.to_be_bytes());
    payload.push(1); // one VarInt-counted modifier
    payload.extend([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
        0xee, 0xff,
    ]);
    payload.extend(0.125f64.to_be_bytes());
    payload.push(2); // multiply total
    payload.push(17); // "unknown.attribute"
    payload.extend(b"unknown.attribute");
    payload.extend(9.0f64.to_be_bytes());
    payload.push(0);

    for (protocol, _, _, attributes_id) in clientbound_ids() {
        match dispatch(protocol, attributes_id, &payload).as_slice() {
            [Directive::Emit(ClientEvent::EntityAttributesUpdated {
                entity_id,
                attributes,
            })] => {
                assert_eq!(*entity_id, 300);
                assert_eq!(attributes.len(), 1, "protocol {protocol}");
                let snapshot = &attributes[0];
                assert_eq!(snapshot.attribute.to_string(), "minecraft:movement_speed");
                assert_eq!(snapshot.base, 0.25);
                assert_eq!(snapshot.modifiers.len(), 1);
                assert_eq!(
                    snapshot.modifiers[0].id.to_string(),
                    "lodestone:legacy_modifier_00112233445566778899aabbccddeeff"
                );
                assert_eq!(snapshot.modifiers[0].amount, 0.125);
                assert_eq!(snapshot.modifiers[0].operation, 2);
            }
            other => panic!("protocol {protocol}: expected attributes, got {other:?}"),
        }
    }
}

#[test]
fn excessive_attribute_count_is_rejected_before_allocation() {
    let mut payload = vec![1]; // entity id
    payload.extend(129i32.to_be_bytes());
    for (protocol, _, _, attributes_id) in clientbound_ids() {
        let error = adapter_for(protocol)
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                attributes_id,
                &payload,
            )
            .expect_err("count over the 128-entry bound must fail");
        assert!(error.to_string().contains("limit"), "protocol {protocol}: {error}");
    }
}

#[test]
fn excessive_modifier_count_is_rejected_before_allocation() {
    let mut payload = vec![1]; // entity id
    payload.extend(1i32.to_be_bytes()); // one property
    payload.extend([1, b'x']); // key
    payload.extend(0.0f64.to_be_bytes());
    payload.extend([0x81, 0x08]); // VarInt 1025, over the 1024-entry bound
    for (protocol, _, _, attributes_id) in clientbound_ids() {
        let error = adapter_for(protocol)
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                attributes_id,
                &payload,
            )
            .expect_err("count over the modifier bound must fail");
        assert!(error.to_string().contains("limit"), "protocol {protocol}: {error}");
    }
}
