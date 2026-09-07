//! Literal protocol-766 fixtures for registry-backed clientbound packets.
//!
//! These bodies are hand-written known answers rather than codec round trips:
//! they exercise the packet's continuation bit, registry holder ids, packed
//! position, and modifier widths independently from the decoder.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, EquipmentSlot, VersionAdapter,
};
use lodestone_v1_20_6::{PROTOCOL_1_20_6, adapter_for, packet_ids};
use lodestone_world::{ChunkColumn, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

fn hex(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 2, 0, "fixture has an odd number of hex digits");
    (0..input.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&input[index..index + 2], 16).expect("fixture is hex"))
        .collect()
}

fn decode(packet_id: i32, body: &[u8]) -> Vec<Directive> {
    adapter_for(PROTOCOL_1_20_6)
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, body)
        .expect("literal protocol-766 fixture decodes")
}

fn world_with_loaded_section() -> World {
    let mut column = ChunkColumn::new(
        -64,
        24,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    // Materialise the section that the fixtures update; an absent section
    // would make the world's no-op-on-unloaded contract hide a bad dispatch.
    column.set_block(2, 68, 3, 1);
    let mut world = World::new();
    world.load(
        lodestone_world::ChunkPos::new(0, 0),
        LoadedChunk::new(
            column,
            ColumnLight::new(24),
            Heightmaps::new(),
            Vec::new(),
        ),
    );
    world
}

#[test]
fn entity_equipment_continuation_reaches_the_canonical_item_consumer() {
    // Entity 300, a continued main-hand stone stack (count 1, item id 1,
    // empty component patch), then an explicitly empty head slot.
    let directives = decode(
        packet_ids::play::clientbound::ENTITY_EQUIPMENT,
        &hex("ac0280010100000500"),
    );
    let [Directive::Emit(ClientEvent::EntityEquipmentUpdated {
        entity_id,
        equipment,
    })] = directives.as_slice()
    else {
        panic!("expected EntityEquipmentUpdated, got {directives:?}");
    };
    assert_eq!(*entity_id, 300);
    assert_eq!(equipment.len(), 2);
    assert_eq!(equipment[0].slot, EquipmentSlot::MainHand);
    assert_eq!(
        equipment[0].item.as_ref().map(|item| (&item.item, item.count)),
        Some((&"minecraft:stone".parse().expect("key"), 1))
    );
    assert_eq!(equipment[1].slot, EquipmentSlot::Head);
    assert!(equipment[1].item.is_none());
}

#[test]
fn entity_attributes_resolve_registry_id_and_uuid_modifier() {
    // Entity 300, one attribute holder id 17 (movement speed), base 0.25,
    // one UUID modifier with amount -0.5 and multiply-total operation 2.
    let directives = decode(
        packet_ids::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        &hex("ac0201113fd00000000000000100112233445566778899aabbccddeeffbfe000000000000002"),
    );
    let [Directive::Emit(ClientEvent::EntityAttributesUpdated {
        entity_id,
        attributes,
    })] = directives.as_slice()
    else {
        panic!("expected EntityAttributesUpdated, got {directives:?}");
    };
    assert_eq!(*entity_id, 300);
    assert_eq!(attributes.len(), 1);
    let attribute = &attributes[0];
    assert_eq!(attribute.attribute.to_string(), "minecraft:movement_speed");
    assert_eq!(attribute.base, 0.25);
    assert_eq!(attribute.modifiers.len(), 1);
    assert_eq!(
        attribute.modifiers[0].id.to_string(),
        "lodestone:legacy_modifier_00112233445566778899aabbccddeeff"
    );
    assert_eq!(attribute.modifiers[0].amount, -0.5);
    assert_eq!(attribute.modifiers[0].operation, 2);
}

#[test]
fn non_generic_attribute_names_use_canonical_unqualified_paths() {
    // Entity 1 with every non-generic holder: player block-breaking and
    // interaction ranges, followed by zombie reinforcement chance. The four
    // distinct bases make a holder-id shift visible as well as namespace loss.
    let directives = decode(
        packet_ids::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        &hex(
            "0104053ff8000000000000000640040000000000000007400c0000000000000014401200000000000000",
        ),
    );
    let [Directive::Emit(ClientEvent::EntityAttributesUpdated { attributes, .. })] =
        directives.as_slice()
    else {
        panic!("expected EntityAttributesUpdated, got {directives:?}");
    };
    let keys: Vec<_> = attributes
        .iter()
        .map(|attribute| attribute.attribute.to_string())
        .collect();
    assert_eq!(
        keys,
        [
            "minecraft:block_break_speed",
            "minecraft:block_interaction_range",
            "minecraft:entity_interaction_range",
            "minecraft:spawn_reinforcements",
        ]
    );
    assert_eq!(
        attributes.iter().map(|attribute| attribute.base).collect::<Vec<_>>(),
        [1.5, 2.5, 3.5, 4.5]
    );
}

#[test]
fn equipment_component_patch_marks_the_stack_unmodeled() {
    // Entity 1, terminal main-hand stone. It adds component 1 with VarInt
    // payload 42 and removes the payload-free component 14.
    let directives = decode(
        packet_ids::play::clientbound::ENTITY_EQUIPMENT,
        &hex("010001010101012a0e"),
    );
    let [Directive::Emit(ClientEvent::EntityEquipmentUpdated { equipment, .. })] =
        directives.as_slice()
    else {
        panic!("expected EntityEquipmentUpdated, got {directives:?}");
    };
    let item = equipment[0].item.as_ref().expect("fixture has a stack");
    assert_eq!(item.item.to_string(), "minecraft:stone");
    assert!(item.components.has_unmodeled);
}

#[test]
fn block_action_reaches_the_shell_block_event_stream() {
    // Packed (x=1, y=64, z=-3), opaque parameters 1 and 7, and block holder
    // id 177 (chest). The byte ordering makes an x/z swap observably wrong.
    let directives = decode(
        packet_ids::play::clientbound::BLOCK_ACTION,
        &hex("0000007fffffd0400107b101"),
    );
    let [Directive::Emit(ClientEvent::BlockEvent { pos, b0, b1, block })] = directives.as_slice()
    else {
        panic!("expected BlockEvent, got {directives:?}");
    };
    assert_eq!((pos.x, pos.y, pos.z), (1, 64, -3));
    assert_eq!((*b0, *b1), (1, 7));
    assert_eq!(block.to_string(), "minecraft:chest");
}

#[test]
fn block_change_applies_the_canonical_state_to_the_loaded_world() {
    // Packed (x=2, y=68, z=3), followed by source state 4276, the
    // property-free diamond block in the protocol-766 state table. Its
    // canonical id is intentionally obtained from the independent 26.2 data
    // table rather than assuming the two numeric spaces are aligned.
    let mut world = world_with_loaded_section();
    let canonical_diamond = lodestone_data::block_states::state_id("minecraft:diamond_block")
        .expect("canonical diamond block exists");
    assert_ne!(canonical_diamond, 4276, "the fixture must distinguish wire and canonical ids");
    let directives = adapter_for(PROTOCOL_1_20_6)
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            packet_ids::play::clientbound::BLOCK_CHANGE,
            &hex("0000008000003044b421"),
        )
        .expect("literal block_change fixture decodes");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: lodestone_model::SectionPos::new(0, 4, 0),
            blocks: vec![[2, 4, 3]],
        })]
    );
    assert_eq!(
        world.block_state_at(2, 68, 3),
        Some(canonical_diamond),
        "block_change must write canonical state ids, not protocol-766 ids"
    );
}

#[test]
fn multi_block_change_batches_world_writes_and_canonicalizes_each_state() {
    // Section (0, 4, 0), two records: protocol state 4276 at local (2, 4, 3)
    // and protocol state 10 at local (15, 1, 0). Both records and the count
    // are literal VarInts; no encoder is involved in this fixture.
    let mut world = world_with_loaded_section();
    let canonical_diamond = lodestone_data::block_states::state_id("minecraft:diamond_block")
        .expect("canonical diamond block exists");
    let canonical_dirt =
        lodestone_data::block_states::state_id("minecraft:dirt").expect("canonical dirt exists");
    assert_ne!(canonical_diamond, 4276, "the fixture must distinguish wire and canonical ids");
    assert_ne!(canonical_dirt, 10, "the fixture must distinguish wire and canonical ids");
    let directives = adapter_for(PROTOCOL_1_20_6)
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            packet_ids::play::clientbound::MULTI_BLOCK_CHANGE,
            &hex("000000000000000402b484ad0881de02"),
        )
        .expect("literal multi_block_change fixture decodes");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: lodestone_model::SectionPos::new(0, 4, 0),
            blocks: vec![[2, 4, 3], [15, 1, 0]],
        })]
    );
    assert_eq!(
        world.block_state_at(2, 68, 3),
        Some(canonical_diamond)
    );
    assert_eq!(
        world.block_state_at(15, 65, 0),
        Some(canonical_dirt)
    );
}

#[test]
fn equipment_rejects_an_unresolved_item_id() {
    // The terminal main-hand slot names item id 1330, one past the 1330-entry
    // jar registry. It must fail rather than display a plausible wrong item.
    let error = adapter_for(PROTOCOL_1_20_6)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::ENTITY_EQUIPMENT,
            &hex("010101b20a0000"),
        )
        .expect_err("unknown item id is a malformed semantic packet");
    assert!(error.to_string().contains("unknown item registry id 1330"));
}

#[test]
fn equipment_rejects_a_continuation_chain_longer_than_all_slots() {
    // Entity 1 followed by eight continued, empty main-hand entries. A ninth
    // entry must follow, but the adapter rejects before accepting it.
    let error = adapter_for(PROTOCOL_1_20_6)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::ENTITY_EQUIPMENT,
            &hex("0180008000800080008000800080008000"),
        )
        .expect_err("continuation cannot exceed the fixed equipment-slot domain");
    assert!(
        error
            .to_string()
            .contains("entity equipment carries more than 8 entries")
    );
}
