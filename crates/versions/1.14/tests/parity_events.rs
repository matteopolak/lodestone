//! Independent wire fixtures for the formerly-disconnected clientbound play
//! packets. These bodies are assembled field-by-field, never through the
//! packet encoder being exercised.

use lodestone_core::Writer;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v1_14::{
    PROTOCOL_1_14_4, PROTOCOL_1_15_2, PROTOCOL_1_16_5, adapter_for, packet_ids,
    packet_ids_498, packet_ids_578,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};
use uuid::Uuid;

fn dispatch(protocol: i32, packet_id: i32, payload: Vec<u8>) -> Vec<Directive> {
    adapter_for(protocol)
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &payload)
        .expect("fixture decodes")
}

fn clientbound_id(protocol: i32, name: &str) -> i32 {
    let entries = match protocol {
        PROTOCOL_1_14_4 => packet_ids_498::play::clientbound::ENTRIES,
        PROTOCOL_1_15_2 => packet_ids_578::play::clientbound::ENTRIES,
        PROTOCOL_1_16_5 => packet_ids::play::clientbound::ENTRIES,
        _ => unreachable!("test protocols only"),
    };
    entries
        .iter()
        .find_map(|(entry, id)| (*entry == name).then_some(*id))
        .expect("generated table names the fixture packet")
}

#[test]
fn game_state_weather_and_mode_fixture_reaches_every_protocol() {
    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2, PROTOCOL_1_16_5] {
        let mut rain = Writer::default();
        rain.u8(7);
        rain.f32(0.625);
        let directives = dispatch(
            protocol,
            clientbound_id(protocol, "minecraft:game_state_change"),
            rain.into_vec(),
        );
        assert!(matches!(
            directives.as_slice(),
            [Directive::Emit(ClientEvent::WeatherChanged {
                raining: None,
                rain_level: Some(level),
                thunder_level: None,
            })] if *level == 0.625
        ));

        let mut mode = Writer::default();
        mode.u8(3);
        mode.f32(3.0);
        let directives = dispatch(
            protocol,
            clientbound_id(protocol, "minecraft:game_state_change"),
            mode.into_vec(),
        );
        assert!(matches!(
            directives.as_slice(),
            [Directive::Emit(ClientEvent::GameModeChanged { game_mode })]
                if *game_mode == lodestone_model::GameMode::Spectator
        ));
    }
}

#[test]
fn game_state_rejects_fractional_and_non_finite_modes() {
    for value in [3.5, f32::NAN] {
        let mut wire = Writer::default();
        wire.u8(3);
        wire.f32(value);
        let result = adapter_for(PROTOCOL_1_16_5).handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            clientbound_id(PROTOCOL_1_16_5, "minecraft:game_state_change"),
            &wire.into_vec(),
        );
        assert!(result.is_err(), "mode value {value:?} must be rejected");
    }
}

#[test]
fn explosion_fixture_preserves_offsets_and_zero_impulse() {
    let mut wire = Writer::default();
    wire.f32(1.5);
    wire.f32(64.0);
    wire.f32(-2.25);
    wire.f32(4.0);
    wire.i32(2);
    wire.i8(-1);
    wire.i8(0);
    wire.i8(1);
    wire.i8(2);
    wire.i8(-3);
    wire.i8(4);
    wire.f32(0.0);
    wire.f32(-0.5);
    wire.f32(0.25);
    let directives = dispatch(
        PROTOCOL_1_16_5,
        clientbound_id(PROTOCOL_1_16_5, "minecraft:explosion"),
        wire.into_vec(),
    );
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Explosion {
            pos,
            radius,
            affected_blocks,
            knockback: Some(knockback),
        })] => {
            assert_eq!(*pos, lodestone_model::Vec3::new(1.5, 64.0, -2.25));
            assert_eq!(*radius, 4.0);
            assert_eq!(affected_blocks, &vec![[-1, 0, 1], [2, -3, 4]]);
            assert_eq!(*knockback, lodestone_model::Vec3::new(0.0, -0.5, 0.25));
        }
        other => panic!("unexpected explosion directives: {other:?}"),
    }
}

#[test]
fn explosion_removes_signed_offsets_from_a_loaded_world_only() {
    let mut world = World::new();
    let column = ChunkColumn::new(
        0,
        16,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    world.load(
        ChunkPos::new(0, 0),
        LoadedChunk::new(column, ColumnLight::new(16), Heightmaps::new(), Vec::new()),
    );
    // floor(5.5, 64.25, 5.75) + (-1, -2, 3) = (4, 62, 8).
    world.set_block(4, 62, 8, 1);
    world.set_block(5, 62, 8, 1);

    let mut wire = Writer::default();
    wire.f32(5.5);
    wire.f32(64.25);
    wire.f32(5.75);
    wire.f32(2.0);
    wire.i32(1);
    wire.i8(-1);
    wire.i8(-2);
    wire.i8(3);
    wire.f32(0.0);
    wire.f32(0.0);
    wire.f32(0.0);
    let adapter = adapter_for(PROTOCOL_1_16_5);
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            clientbound_id(PROTOCOL_1_16_5, "minecraft:explosion"),
            &wire.into_vec(),
        )
        .expect("explosion fixture decodes");

    assert_eq!(world.block_state_at(4, 62, 8), Some(0));
    assert_eq!(world.block_state_at(5, 62, 8), Some(1));
}

#[test]
fn bulk_block_change_has_legacy_chunk_and_754_section_fixtures() {
    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2] {
        let mut wire = Writer::default();
        wire.i32(-2);
        wire.i32(3);
        wire.var_i32(1);
        wire.u8(0xa4); // local x=10, z=4
        wire.u8(70);
        wire.var_i32(1);
        let directives = dispatch(
            protocol,
            clientbound_id(protocol, "minecraft:multi_block_change"),
            wire.into_vec(),
        );
        assert!(matches!(
            directives.as_slice(),
            [Directive::Emit(ClientEvent::SectionBlocksChanged { section, blocks })]
                if (section.x, section.y, section.z) == (-2, 4, 3)
                    && blocks.as_slice() == &[[10, 6, 4]]
        ));
    }

    let mut wire = Writer::default();
    let packed_section = ((-1_i64 & 0x3f_ffff) << 42) | ((2_i64 & 0x3f_ffff) << 20) | 5;
    wire.i64(packed_section);
    wire.bool(false);
    wire.var_i32(1);
    wire.var_i64((1_i64 << 12) | (3_i64 << 8) | (9_i64 << 4) | 7);
    let directives = dispatch(
        PROTOCOL_1_16_5,
        clientbound_id(PROTOCOL_1_16_5, "minecraft:multi_block_change"),
        wire.into_vec(),
    );
    assert!(matches!(
        directives.as_slice(),
        [Directive::Emit(ClientEvent::SectionBlocksChanged { section, blocks })]
            if (section.x, section.y, section.z) == (-1, 5, 2)
                && blocks.as_slice() == &[[3, 7, 9]]
    ));
}

#[test]
fn entity_equipment_literal_fixture_resolves_each_historical_item_registry() {
    for (protocol, item_id) in [
        (PROTOCOL_1_14_4, 575),
        (PROTOCOL_1_15_2, 575),
    ] {
        let mut wire = Writer::default();
        wire.var_i32(17); // entity id
        wire.var_i32(5); // head slot
        wire.bool(true);
        wire.var_i32(item_id);
        wire.i8(1);
        wire.u8(10); // populated legacy NBT: empty named compound
        wire.u16(0);
        wire.u8(0);
        let directives = dispatch(
            protocol,
            clientbound_id(protocol, "minecraft:entity_equipment"),
            wire.into_vec(),
        );
        match directives.as_slice() {
            [Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                entity_id,
                equipment,
            })] => {
                assert_eq!(*entity_id, 17);
                assert_eq!(equipment.len(), 1);
                assert_eq!(equipment[0].slot, lodestone_model::EquipmentSlot::Head);
                let item = equipment[0].item.as_ref().expect("populated item");
                assert_eq!(item.item.to_string(), "minecraft:diamond_helmet");
                assert_eq!(item.count, 1);
                assert!(item.components.has_unmodeled);
            }
            other => panic!("unexpected equipment directives for {protocol}: {other:?}"),
        }
    }

    // Protocol 754 changed the slot field to an i8-shaped byte whose top bit
    // continues the packet. This two-entry body distinguishes it from the
    // single-VarInt shape used above and keeps both entries visible.
    let mut wire = Writer::default();
    wire.var_i32(17); // entity id
    wire.u8(0x80); // main hand, another entry follows
    wire.bool(false); // clear main hand
    wire.u8(0x05); // head, final entry
    wire.bool(true);
    wire.var_i32(634); // diamond helmet in protocol 754
    wire.i8(1);
    wire.u8(0); // no item NBT
    let directives = dispatch(
        PROTOCOL_1_16_5,
        clientbound_id(PROTOCOL_1_16_5, "minecraft:entity_equipment"),
        wire.into_vec(),
    );
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntityEquipmentUpdated {
            entity_id,
            equipment,
        })] => {
            assert_eq!(*entity_id, 17);
            assert_eq!(equipment.len(), 2);
            assert_eq!(equipment[0].slot, lodestone_model::EquipmentSlot::MainHand);
            assert!(equipment[0].item.is_none());
            assert_eq!(equipment[1].slot, lodestone_model::EquipmentSlot::Head);
            let item = equipment[1].item.as_ref().expect("populated item");
            assert_eq!(item.item.to_string(), "minecraft:diamond_helmet");
            assert_eq!(item.count, 1);
            assert!(!item.components.has_unmodeled);
        }
        other => panic!("unexpected 754 equipment directives: {other:?}"),
    }
}

#[test]
fn entity_equipment_rejects_nonpositive_present_counts() {
    for count in [0, -1] {
        let mut wire = Writer::default();
        wire.var_i32(17);
        wire.var_i32(5);
        wire.bool(true);
        wire.var_i32(575);
        wire.i8(count);
        wire.u8(0);
        let result = adapter_for(PROTOCOL_1_14_4).handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            clientbound_id(PROTOCOL_1_14_4, "minecraft:entity_equipment"),
            &wire.into_vec(),
        );
        assert!(result.is_err(), "present count {count} must be rejected");
    }
}

#[test]
fn block_action_literal_fixture_resolves_each_historical_block_registry() {
    let fixtures = [
        (PROTOCOL_1_14_4, 145, "minecraft:chest"),
        (PROTOCOL_1_15_2, 145, "minecraft:chest"),
        (PROTOCOL_1_16_5, 147, "minecraft:chest"),
    ];
    for (protocol, block_id, expected) in fixtures {
        let mut wire = Writer::default();
        // x=-12, y=63, z=34 in the 1.14+ x/z/y packed-position layout.
        wire.i64(-3_298_534_744_001);
        wire.u8(1);
        wire.u8(9);
        wire.var_i32(block_id);
        let directives = dispatch(
            protocol,
            clientbound_id(protocol, "minecraft:block_action"),
            wire.into_vec(),
        );
        match directives.as_slice() {
            [Directive::Emit(ClientEvent::BlockEvent { pos, b0, b1, block })] => {
                assert_eq!((*pos, *b0, *b1), (lodestone_model::BlockPos::new(-12, 63, 34), 1, 9));
                assert_eq!(block.to_string(), expected);
            }
            other => panic!("unexpected block-event directives for {protocol}: {other:?}"),
        }
    }
}

#[test]
fn metadata_flags_and_textual_attribute_keys_are_lifted() {
    let mut metadata = Writer::default();
    metadata.var_i32(19);
    metadata.u8(0);
    metadata.var_i32(0);
    metadata.i8(0x24);
    metadata.u8(0xff);
    let directives = dispatch(
        PROTOCOL_1_16_5,
        clientbound_id(PROTOCOL_1_16_5, "minecraft:entity_metadata"),
        metadata.into_vec(),
    );
    assert!(matches!(
        directives.as_slice(),
        [Directive::Emit(ClientEvent::EntityMetadataUpdated { entity_id: 19, metadata })]
            if metadata.flags == Some(0x24)
    ));

    for (protocol, key) in [
        (PROTOCOL_1_14_4, "generic.maxHealth"),
        (PROTOCOL_1_15_2, "generic.movementSpeed"),
        (PROTOCOL_1_16_5, "minecraft:generic.max_health"),
    ] {
        let mut attributes = Writer::default();
        attributes.var_i32(19);
        attributes.i32(1);
        attributes.string(key);
        attributes.f64(20.0);
        attributes.var_i32(1);
        attributes.uuid(Uuid::from_u128(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef));
        attributes.f64(0.25);
        attributes.i8(2);
        let directives = dispatch(
            protocol,
            clientbound_id(protocol, "minecraft:entity_update_attributes"),
            attributes.into_vec(),
        );
        match directives.as_slice() {
            [Directive::Emit(ClientEvent::EntityAttributesUpdated { entity_id, attributes })] => {
                assert_eq!(*entity_id, 19);
                assert_eq!(attributes.len(), 1);
                assert_eq!(attributes[0].base, 20.0);
                assert_eq!(attributes[0].modifiers[0].operation, 2);
                assert!(attributes[0]
                    .modifiers[0]
                    .id
                    .to_string()
                    .starts_with("lodestone:legacy_modifier_"));
            }
            other => panic!("unexpected attribute directives: {other:?}"),
        }
    }
}

#[test]
fn attributes_reject_excessive_counts_and_unknown_operations() {
    let packet_id = clientbound_id(PROTOCOL_1_16_5, "minecraft:entity_update_attributes");
    let adapter = adapter_for(PROTOCOL_1_16_5);

    let mut properties = Writer::default();
    properties.var_i32(1);
    properties.i32(129);
    assert!(adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &properties.into_vec())
        .is_err());

    let mut modifiers = Writer::default();
    modifiers.var_i32(1);
    modifiers.i32(1);
    modifiers.string("minecraft:generic.max_health");
    modifiers.f64(20.0);
    modifiers.var_i32(1025);
    assert!(adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &modifiers.into_vec())
        .is_err());

    let mut operation = Writer::default();
    operation.var_i32(1);
    operation.i32(1);
    operation.string("minecraft:generic.max_health");
    operation.f64(20.0);
    operation.var_i32(1);
    operation.uuid(Uuid::nil());
    operation.f64(0.0);
    operation.i8(3);
    assert!(adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &operation.into_vec())
        .is_err());
}

#[test]
fn protocol_498_living_spawn_consumes_and_emits_its_trailing_metadata() {
    let mut wire = Writer::default();
    wire.var_i32(44);
    wire.uuid(Uuid::from_u128(44));
    wire.var_i32(11); // protocol 498's generated creeper id
    wire.f64(1.0);
    wire.f64(64.0);
    wire.f64(2.0);
    wire.i8(0);
    wire.i8(0);
    wire.i8(0);
    wire.i16(0);
    wire.i16(0);
    wire.i16(0);
    wire.u8(0); // shared flags index
    wire.var_i32(0); // byte serializer
    wire.i8(0x20);
    wire.u8(0xff); // metadata terminator
    let directives = dispatch(
        PROTOCOL_1_14_4,
        clientbound_id(PROTOCOL_1_14_4, "minecraft:spawn_entity_living"),
        wire.into_vec(),
    );
    assert!(matches!(
        directives.as_slice(),
        [
            Directive::Emit(ClientEvent::EntitySpawned { entity_id: 44, .. }),
            Directive::Emit(ClientEvent::EntityMetadataUpdated { entity_id: 44, metadata }),
        ] if metadata.flags == Some(0x20)
    ));
}

#[test]
fn block_destruction_stage_is_the_raw_wire_byte_in_every_protocol() {
    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2, PROTOCOL_1_16_5] {
        let mut wire = Writer::default();
        wire.var_i32(7);
        wire.i64((4_i64 << 38) | ((-2_i64 & 0x3ffffff) << 12) | 70);
        wire.i8(-1);
        let directives = dispatch(
            protocol,
            clientbound_id(protocol, "minecraft:block_break_animation"),
            wire.into_vec(),
        );
        assert!(matches!(
            directives.as_slice(),
            [Directive::Emit(ClientEvent::BlockDestruction { entity_id: 7, progress: 255, .. })]
        ));
    }
}
