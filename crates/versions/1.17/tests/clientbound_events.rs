//! Independent byte fixtures for the bounded 1.17/1.18 event seams.
//!
//! These bodies are assembled from the protocol field widths, not encoded by
//! the implementation under test. In particular, the multi-block header
//! fixtures differ only in the signedness of section Y, the one 756/758
//! change this test is intended to catch.

use lodestone_model::{ClientEvent, ConnectionState, Directive, SectionPos, VersionAdapter};
use lodestone_v1_17::packet_ids;
use lodestone_v1_17::packet_ids_758;
use lodestone_v1_17::packets::position::pack_position;
use lodestone_v1_17::{V756Adapter, PROTOCOL_1_17_1, PROTOCOL_1_18_2};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut value = value as u32;
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// The 1.14+ position layout written independently from the packet codec:
/// signed x and z are 26-bit fields, and y is the low 12 bits.
fn position_body(x: i32, y: i32, z: i32) -> [u8; 8] {
    (((i64::from(x) & ((1 << 26) - 1)) << 38)
        | ((i64::from(z) & ((1 << 26) - 1)) << 12)
        | (i64::from(y) & ((1 << 12) - 1)))
        .to_be_bytes()
}

fn dispatch(protocol: i32, id: i32, payload: &[u8]) -> Vec<Directive> {
    let adapter = V756Adapter::for_protocol(protocol);
    let mut world = World::new();
    adapter
        .handle_packet(&mut world, ConnectionState::Play, id, payload)
        .expect("literal clientbound body decodes")
}

fn loaded_world() -> World {
    let mut world = World::new();
    let column = ChunkColumn::new(0, 16, PaletteKind::block_states(), PaletteKind::biomes(), 0, 0);
    world.load(
        ChunkPos::new(0, 0),
        LoadedChunk::new(column, ColumnLight::new(16), Heightmaps::new(), Vec::new()),
    );
    world
}

#[test]
fn explosion_uses_varint_offset_count_and_preserves_knockback() {
    let mut body = Vec::new();
    body.extend_from_slice(&1.5f32.to_be_bytes());
    body.extend_from_slice(&(-2.25f32).to_be_bytes());
    body.extend_from_slice(&4.0f32.to_be_bytes());
    body.extend_from_slice(&3.0f32.to_be_bytes());
    body.extend_from_slice(&var_i32(2));
    body.extend_from_slice(&[1, 2, 3, 0xff, 0xfe, 4]);
    body.extend_from_slice(&0.25f32.to_be_bytes());
    body.extend_from_slice(&(-0.5f32).to_be_bytes());
    body.extend_from_slice(&0.75f32.to_be_bytes());
    let id = packet_ids::play::clientbound::EXPLOSION;
    let events = dispatch(PROTOCOL_1_17_1, id, &body);
    let [Directive::Emit(ClientEvent::Explosion { pos, radius, affected_blocks, knockback })] =
        events.as_slice()
    else { panic!("wrong event") };
    assert_eq!((*radius, affected_blocks.clone()), (3.0, vec![[1, 2, 3], [-1, -2, 4]]));
    assert_eq!((*pos, *knockback), (lodestone_model::Vec3::new(1.5, -2.25, 4.0), Some(lodestone_model::Vec3::new(0.25, -0.5, 0.75))));
}

#[test]
fn explosion_applies_signed_offsets_to_loaded_world_only() {
    let mut body = Vec::new();
    body.extend_from_slice(&1.75f32.to_be_bytes());
    body.extend_from_slice(&64.9f32.to_be_bytes());
    body.extend_from_slice(&2.2f32.to_be_bytes());
    body.extend_from_slice(&1.0f32.to_be_bytes());
    body.extend_from_slice(&var_i32(1));
    body.extend_from_slice(&[1, 0xff, 0xfe]);
    body.extend_from_slice(&0.0f32.to_be_bytes());
    body.extend_from_slice(&0.0f32.to_be_bytes());
    body.extend_from_slice(&0.0f32.to_be_bytes());

    let adapter = V756Adapter::for_protocol(PROTOCOL_1_17_1);
    let mut world = loaded_world();
    let target = (2, 63, 0); // floor((1.75, 64.9, 2.2)) + (1, -1, -2)
    let untouched = (1, 64, 2);
    world.set_block(target.0, target.1, target.2, 42);
    world.set_block(untouched.0, untouched.1, untouched.2, 43);
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            packet_ids::play::clientbound::EXPLOSION,
            &body,
        )
        .expect("literal explosion body decodes");
    assert_eq!(world.block_state_at(target.0, target.1, target.2), Some(0));
    assert_eq!(world.block_state_at(untouched.0, untouched.1, untouched.2), Some(43));
}

#[test]
fn game_state_codes_map_to_weather_and_mode() {
    let id = packet_ids_758::play::clientbound::GAME_STATE_CHANGE;
    let mut weather = Vec::new();
    weather.push(7);
    weather.extend_from_slice(&0.4f32.to_be_bytes());
    assert!(matches!(dispatch(PROTOCOL_1_18_2, id, &weather).as_slice(), [Directive::Emit(ClientEvent::WeatherChanged { rain_level: Some(v), .. })] if (*v - 0.4).abs() < f32::EPSILON));
    let mut mode = vec![3];
    mode.extend_from_slice(&1.0f32.to_be_bytes());
    assert!(matches!(dispatch(PROTOCOL_1_18_2, id, &mode).as_slice(), [Directive::Emit(ClientEvent::GameModeChanged { game_mode: lodestone_model::GameMode::Creative })]));
}

#[test]
fn unmodelled_game_state_reason_is_consumed_without_disconnect() {
    let id = packet_ids::play::clientbound::GAME_STATE_CHANGE;
    let mut body = vec![0];
    body.extend_from_slice(&1.0f32.to_be_bytes());
    assert!(dispatch(PROTOCOL_1_17_1, id, &body).is_empty());
}

fn packed_section(x: i32, y: i32, z: i32) -> [u8; 8] {
    let raw = ((x as i64 & ((1 << 22) - 1)) << 42)
        | ((z as i64 & ((1 << 22) - 1)) << 20)
        | (y as i64 & ((1 << 20) - 1));
    raw.to_be_bytes()
}

#[test]
fn multi_block_change_decodes_756_unsigned_and_758_signed_section_y() {
    for &(protocol, id, y) in &[
        (PROTOCOL_1_17_1, packet_ids::play::clientbound::MULTI_BLOCK_CHANGE, 4),
        (PROTOCOL_1_18_2, packet_ids_758::play::clientbound::MULTI_BLOCK_CHANGE, -4),
    ] {
        let mut body = packed_section(-2, y, 3).to_vec();
        body.push(0); // notTrustEdges
        body.extend_from_slice(&var_i32(1));
        body.extend_from_slice(&var_i32((1 << 12) | (2 << 8) | (3 << 4) | 4));
        let events = dispatch(protocol, id, &body);
        assert!(matches!(events.as_slice(), [Directive::Emit(ClientEvent::SectionBlocksChanged { section, blocks })]
            if *section == SectionPos::new(-2, y, 3) && blocks == &vec![[2, 4, 3]]));
    }
}

#[test]
fn world_event_keeps_block_state_numbering_protocol_local() {
    let mut body = 2001i32.to_be_bytes().to_vec();
    body.extend_from_slice(&pack_position(lodestone_model::BlockPos::new(2, 64, -3)).to_be_bytes());
    body.extend_from_slice(&17i32.to_be_bytes());
    body.push(1);
    let events = dispatch(PROTOCOL_1_17_1, packet_ids::play::clientbound::WORLD_EVENT, &body);
    assert!(matches!(events.as_slice(), [Directive::Emit(ClientEvent::LevelEvent { data: lodestone_model::LevelEventData::BlockState(lodestone_model::BlockStateRef::ProtocolLocal(17)), global: true, .. })]));
}

#[test]
fn textual_attributes_decode_with_bounded_modifier_list() {
    let mut body = var_i32(7);
    body.extend_from_slice(&var_i32(1));
    body.extend_from_slice(&var_i32("minecraft:generic.max_health".len() as i32));
    body.extend_from_slice(b"minecraft:generic.max_health");
    body.extend_from_slice(&20.0f64.to_be_bytes());
    body.extend_from_slice(&var_i32(0));
    let events = dispatch(PROTOCOL_1_17_1, packet_ids::play::clientbound::ENTITY_UPDATE_ATTRIBUTES, &body);
    assert!(matches!(events.as_slice(), [Directive::Emit(ClientEvent::EntityAttributesUpdated { entity_id: 7, attributes })]
        if attributes.len() == 1 && attributes[0].attribute.to_string() == "minecraft:max_health"));
}

#[test]
fn unknown_attribute_key_is_skipped_after_wire_decode() {
    let mut body = var_i32(7);
    body.extend_from_slice(&var_i32(1));
    body.extend_from_slice(&var_i32("minecraft:future.attribute".len() as i32));
    body.extend_from_slice(b"minecraft:future.attribute");
    body.extend_from_slice(&20.0f64.to_be_bytes());
    body.extend_from_slice(&var_i32(0));
    let events = dispatch(PROTOCOL_1_18_2, packet_ids_758::play::clientbound::ENTITY_UPDATE_ATTRIBUTES, &body);
    assert!(matches!(events.as_slice(), [Directive::Emit(ClientEvent::EntityAttributesUpdated { entity_id: 7, attributes })]
        if attributes.is_empty()));
}

#[test]
fn block_destruction_preserves_clear_stage_byte() {
    let mut body = var_i32(42);
    body.extend_from_slice(&pack_position(lodestone_model::BlockPos::new(1, 2, 3)).to_be_bytes());
    body.push(255);
    let events = dispatch(PROTOCOL_1_18_2, packet_ids_758::play::clientbound::BLOCK_BREAK_ANIMATION, &body);
    assert!(matches!(events.as_slice(), [Directive::Emit(ClientEvent::BlockDestruction { entity_id: 42, progress: 255, .. })]));
}

#[test]
fn block_action_resolves_its_protocol_local_block_and_preserves_visible_parameters() {
    for &(protocol, id) in &[
        (PROTOCOL_1_17_1, packet_ids::play::clientbound::BLOCK_ACTION),
        (PROTOCOL_1_18_2, packet_ids_758::play::clientbound::BLOCK_ACTION),
    ] {
        // `153` is chest in each committed jar report, not a 26.2 block id.
        let mut body = position_body(-12, 64, 35).to_vec();
        body.extend_from_slice(&[1, 0]); // chest opens; these values stay opaque to the adapter
        body.extend_from_slice(&var_i32(153));
        let events = dispatch(protocol, id, &body);
        assert!(matches!(events.as_slice(), [Directive::Emit(ClientEvent::BlockEvent { pos, b0: 1, b1: 0, block })]
            if *pos == lodestone_model::BlockPos::new(-12, 64, 35) && block.to_string() == "minecraft:chest"));
    }
}

#[test]
fn entity_equipment_resolves_continuation_slots_from_literal_slot_bodies() {
    for &(protocol, id) in &[
        (PROTOCOL_1_17_1, packet_ids::play::clientbound::ENTITY_EQUIPMENT),
        (PROTOCOL_1_18_2, packet_ids_758::play::clientbound::ENTITY_EQUIPMENT),
    ] {
        let mut body = var_i32(73);
        body.extend_from_slice(&[0x80, 0]); // MainHand, another entry, empty slot
        body.push(5); // Head, final entry
        body.push(1); // occupied slot
        body.extend_from_slice(&var_i32(746)); // iron_helmet in both jar reports
        body.extend_from_slice(&[1, 0]); // count, TAG_End (no legacy NBT)
        let events = dispatch(protocol, id, &body);
        assert!(matches!(events.as_slice(), [Directive::Emit(ClientEvent::EntityEquipmentUpdated { entity_id: 73, equipment })]
            if equipment.len() == 2
                && equipment[0].slot == lodestone_model::EquipmentSlot::MainHand
                && equipment[0].item.is_none()
                && equipment[1].slot == lodestone_model::EquipmentSlot::Head
                && matches!(&equipment[1].item, Some(item) if item.item.to_string() == "minecraft:iron_helmet" && item.count == 1)));
    }
}

#[test]
fn entity_equipment_rejects_legacy_nbt_instead_of_discarding_it() {
    let mut body = var_i32(73);
    body.push(5); // Head, final entry
    body.push(1); // occupied slot
    body.extend_from_slice(&var_i32(746));
    body.push(1); // count
    // A minimal named compound. Its contents are deliberately not decoded into
    // modern components, so a successful event here would be a false claim.
    body.extend_from_slice(&[10, 0, 0, 0]);
    let adapter = V756Adapter::for_protocol(PROTOCOL_1_17_1);
    let mut world = World::new();
    let error = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            packet_ids::play::clientbound::ENTITY_EQUIPMENT,
            &body,
        )
        .expect_err("legacy NBT must not silently become an empty component set");
    assert!(error.to_string().contains("legacy NBT"));
}
