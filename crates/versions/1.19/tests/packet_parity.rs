//! Literal protocol-762 packet bodies for bounded adapter paths that are not
//! represented in the small flat-world join capture.
//!
//! These are deliberately decoder fixtures, not `Encode`/`Decode` round
//! trips: their count widths, offsets and text keys remain independent of the
//! codecs under test.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, VersionAdapter,
};
use lodestone_v1_19::{PROTOCOL_1_19_4, adapter_for, packet_ids};
use lodestone_v1_19::packets::metadata::EntityMetadata;
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

fn hex(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 2, 0, "fixture has an odd number of hex digits");
    (0..input.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&input[index..index + 2], 16).expect("fixture is hex"))
        .collect()
}

fn decode(packet_id: i32, body: &[u8]) -> Vec<Directive> {
    adapter_for(PROTOCOL_1_19_4)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_id,
            body,
        )
        .expect("literal protocol-762 fixture decodes")
}

#[test]
fn explosion_uses_a_varint_offset_count_and_preserves_its_tail() {
    // f64 (1.5, -2.25, 3.75), f32 radius 4.5, VarInt count 2, two signed
    // three-byte offsets, then the unconditional f32 knockback tail.
    let body = hex(
        "3ff8000000000000c002000000000000400e00000000000040900000\
         0201fe03fc05fa3e800000bf0000003f800000",
    );
    let directives = decode(packet_ids::play::clientbound::EXPLOSION, &body);
    let [Directive::Emit(ClientEvent::Explosion {
        pos,
        radius,
        affected_blocks,
        knockback,
    })] = directives.as_slice()
    else {
        panic!("expected one explosion directive, got {directives:?}");
    };
    assert_eq!(*pos, lodestone_model::Vec3::new(1.5, -2.25, 3.75));
    assert_eq!(*radius, 4.5);
    assert_eq!(affected_blocks, &vec![[1, -2, 3], [-4, 5, -6]]);
    assert_eq!(
        *knockback,
        Some(lodestone_model::Vec3::new(0.25, -0.5, 1.0)),
        "the three legacy knockback floats are always present, including zeroes"
    );
}

#[test]
fn explosion_count_cannot_consume_the_knockback_tail() {
    // The VarInt claims five offsets, but only one fits before the twelve-byte
    // tail. A decoder must reject this rather than consuming motion as offsets.
    let body = hex(
        "3ff8000000000000c002000000000000400e00000000000040900000\
         0501fe033e800000bf0000003f800000",
    );
    let err = adapter_for(PROTOCOL_1_19_4)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::EXPLOSION,
            &body,
        )
        .expect_err("count must not reach the knockback tail");
    assert!(err.to_string().contains("affected-block count"), "{err}");
}

#[test]
fn explosion_removes_loaded_blocks_and_their_block_entities_before_emitting() {
    fn loaded_chunk() -> LoadedChunk {
        LoadedChunk::new(
            ChunkColumn::new(
                -64,
                24,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                0,
                0,
            ),
            ColumnLight::new(24),
            Heightmaps::new(),
            Vec::new(),
        )
    }

    // The center floors to (1, -3, 3), so the two offsets target (2, -5, 6)
    // and (-3, 2, -3), spanning two loaded chunks.  Both must be air before
    // the event is returned; the first also starts with a block entity.
    let mut world = World::new();
    world.load(ChunkPos::new(0, 0), loaded_chunk());
    world.load(ChunkPos::new(-1, -1), loaded_chunk());
    world.set_block(2, -5, 6, 91);
    world.set_block(-3, 2, -3, 92);
    world.sync_block_entity(2, -5, 6, Some(1));
    assert_eq!(world.get(ChunkPos::new(0, 0)).unwrap().block_entities.len(), 1);

    let body = hex(
        "3ff8000000000000c002000000000000400e00000000000040900000\
         0201fe03fc05fa3e800000bf0000003f800000",
    );
    let directives = adapter_for(PROTOCOL_1_19_4)
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            packet_ids::play::clientbound::EXPLOSION,
            &body,
        )
        .expect("literal explosion decodes into the loaded world");
    assert!(matches!(directives.as_slice(), [Directive::Emit(ClientEvent::Explosion { .. })]));
    assert_eq!(world.block_state_at(2, -5, 6), Some(0));
    assert_eq!(world.block_state_at(-3, 2, -3), Some(0));
    assert!(
        world.get(ChunkPos::new(0, 0)).unwrap().block_entities.is_empty(),
        "a removed block must not retain its block entity"
    );
}

#[test]
fn game_state_reasons_surface_weather_and_game_mode() {
    let cases = [
        (
            "013f000000",
            ClientEvent::WeatherChanged {
                raining: Some(true),
                rain_level: None,
                thunder_level: None,
            },
        ),
        (
            "023f400000",
            ClientEvent::WeatherChanged {
                raining: Some(false),
                rain_level: None,
                thunder_level: None,
            },
        ),
        (
            "0340000000",
            ClientEvent::GameModeChanged {
                game_mode: GameMode::Adventure,
            },
        ),
        (
            "073f400000",
            ClientEvent::WeatherChanged {
                raining: None,
                rain_level: Some(0.75),
                thunder_level: None,
            },
        ),
        (
            "083f000000",
            ClientEvent::WeatherChanged {
                raining: None,
                rain_level: None,
                thunder_level: Some(0.5),
            },
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(
            decode(
                packet_ids::play::clientbound::GAME_STATE_CHANGE,
                &hex(body),
            ),
            vec![Directive::Emit(expected)],
            "fixture {body}",
        );
    }
}

#[test]
fn block_destruction_keeps_the_raw_stage_byte() {
    // VarInt entity id 300, packed origin, then signed stage -1.  The event
    // holds the raw byte, so consumers can distinguish a clear from stage 9.
    let directives = decode(
        packet_ids::play::clientbound::BLOCK_BREAK_ANIMATION,
        &hex("ac020000000000000000ff"),
    );
    let [Directive::Emit(ClientEvent::BlockDestruction {
        entity_id,
        pos,
        progress,
    })] = directives.as_slice()
    else {
        panic!("expected one block-destruction directive, got {directives:?}");
    };
    assert_eq!(*entity_id, 300);
    assert_eq!((pos.x, pos.y, pos.z), (0, 0, 0));
    assert_eq!(*progress, 255);
}

#[test]
fn existing_metadata_codec_surfaces_only_the_shared_flags_byte() {
    // VarInt entity id 300; index 0, serializer 0 (byte), value 3; sentinel.
    let directives = decode(
        packet_ids::play::clientbound::ENTITY_METADATA,
        &hex("ac02000003ff"),
    );
    let [Directive::Emit(ClientEvent::EntityMetadataUpdated {
        entity_id,
        metadata,
    })] = directives.as_slice()
    else {
        panic!("expected metadata directive, got {directives:?}");
    };
    assert_eq!(*entity_id, 300);
    assert_eq!(metadata.flags, Some(3));
}

#[test]
fn captured_metadata_serializer_three_is_a_float_not_a_string() {
    // Exact first metadata body from the committed 1.19.4 capture: entity
    // 131, index 9, serializer 3, float 16.0; index 16, serializer 1,
    // VarInt 4; sentinel. It has no index-zero flags, so a successful decode
    // intentionally produces no semantic directive.
    assert!(
        decode(
            packet_ids::play::clientbound::ENTITY_METADATA,
            &hex("8301090341800000100104ff"),
        )
        .is_empty()
    );
}

#[test]
fn metadata_late_serializer_ids_follow_the_complete_762_table() {
    // Literal metadata entries covering the table after block-state id 14:
    // 15 optional block state, 16 compound tag, 18 villager data, 19 optional
    // unsigned int, 20 pose, 21/22 variants, 24/25 variants, 26 vec3, and 27
    // quaternion. Types 17 and 23 deliberately have no fixture here: their
    // payload widths are unresolved and are rejected explicitly below rather
    // than guessed.
    let fixture = hex(
        "010f05021000031201020304130705140806150907160a09180b0a190c0b1a3f800000c000000040400000\
         0c1b4080000040a0000040c0000040e00000ff",
    );
    let mut reader = Reader::new(&fixture);
    let decoded = EntityMetadata::decode(&mut reader, Ctx { version: PROTOCOL_1_19_4 })
        .expect("the literal late-table fixture decodes");
    reader.ensure_empty().expect("metadata fixture has no trailing bytes");
    assert_eq!(decoded.0.len(), 11);

    let mut writer = Writer::default();
    decoded
        .encode(&mut writer, Ctx { version: PROTOCOL_1_19_4 })
        .expect("the supported table entries encode");
    assert_eq!(writer.into_vec(), fixture, "each value retains its 762 type id");
}

#[test]
fn metadata_unsettled_payload_types_are_rejected_without_a_width_guess() {
    for (type_id, what) in [(17_u8, "particle"), (23, "global position")] {
        // Entity id 1; index 1; serializer. Decoding must stop at the type id
        // rather than interpreting an unknown payload as a later entry.
        let err = adapter_for(PROTOCOL_1_19_4)
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                packet_ids::play::clientbound::ENTITY_METADATA,
                &[1, 1, type_id, 0xff],
            )
            .expect_err("unsettled payload remains unmodelled");
        assert!(err.to_string().contains("metadata type"), "{what}: {err}");
    }
}

#[test]
fn textual_attributes_map_to_canonical_keys_and_keep_uuid_modifiers() {
    // Entity 300; two VarInt-counted properties. The first has a UUID
    // modifier, the second proves a non-generic namespace maps too.
    let body = hex(
        "ac0202206d696e6563726166743a67656e657269632e6d6f76656d656e745f7370656564\
         3fc00000000000000100112233445566778899aabbccddeeffbfd000000000000002\
         1d6d696e6563726166743a686f7273652e6a756d705f737472656e677468\
         3fe666666666666600",
    );
    let directives = decode(packet_ids::play::clientbound::ENTITY_UPDATE_ATTRIBUTES, &body);
    let [Directive::Emit(ClientEvent::EntityAttributesUpdated {
        entity_id,
        attributes,
    })] = directives.as_slice()
    else {
        panic!("expected attributes directive, got {directives:?}");
    };
    assert_eq!(*entity_id, 300);
    assert_eq!(attributes.len(), 2);
    assert_eq!(attributes[0].attribute.to_string(), "minecraft:movement_speed");
    assert_eq!(attributes[0].base, 0.125);
    assert_eq!(attributes[0].modifiers.len(), 1);
    assert_eq!(
        attributes[0].modifiers[0].id.to_string(),
        "lodestone:legacy_modifier_00112233445566778899aabbccddeeff"
    );
    assert_eq!(attributes[0].modifiers[0].amount, -0.25);
    assert_eq!(attributes[0].modifiers[0].operation, 2);
    assert_eq!(attributes[1].attribute.to_string(), "minecraft:jump_strength");
    assert_eq!(attributes[1].base, 0.7);
    assert!(attributes[1].modifiers.is_empty());
}
