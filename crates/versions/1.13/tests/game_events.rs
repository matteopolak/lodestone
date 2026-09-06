//! Independent protocol-404 fixtures for state packets that feed live consumers.
//!
//! The bodies below are literal clientbound bytes, not this crate's encoder.
//! Their pairwise-distinct coordinates and event parameters make a field-order
//! mistake observable.  The route assertions prove the emitted events enter
//! the existing ECS equipment fold and shell block-event forwarder rather than
//! becoming adapter-local islands.

use lodestone_model::{
    route, BlockPos, ClientEvent, ConnectionState, Directive, EquipmentSlot, VersionAdapter,
};
use lodestone_v1_13::{packet_ids::play, V404Adapter};
use lodestone_world::World;

fn dispatch(id: i32, body: &[u8]) -> Vec<Directive> {
    V404Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, id, body)
        .expect("literal protocol-404 packet is accepted")
}

#[test]
fn literal_entity_equipment_reaches_the_ecs_equipment_fold() {
    // entity=7, chest slot=4, present, item=493 (diamond_sword), count=1,
    // TAG_End (no legacy NBT). 493 is encoded as VarInt ed 03.
    let body = [0x07, 0x04, 0x01, 0xed, 0x03, 0x01, 0x00];
    let directives = dispatch(play::clientbound::ENTITY_EQUIPMENT, &body);
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one equipment event, got {directives:?}");
    };
    let ClientEvent::EntityEquipmentUpdated {
        entity_id,
        equipment,
    } = event
    else {
        panic!("expected EntityEquipmentUpdated, got {event:?}");
    };
    assert_eq!(*entity_id, 7);
    assert_eq!(equipment.len(), 1);
    assert_eq!(equipment[0].slot, EquipmentSlot::Chest);
    let item = equipment[0].item.as_ref().expect("present wire stack stays present");
    assert_eq!(item.item.to_string(), "minecraft:diamond_sword");
    assert_eq!(item.count, 1);
    assert!(route(event).ingest, "equipment must enter the ECS fold");
}

#[test]
fn literal_entity_equipment_marks_legacy_nbt_as_unmodeled() {
    // The same stack followed by an empty named compound (TAG_Compound,
    // zero-length name, TAG_End). The model cannot translate legacy NBT
    // fields, but it must retain the fact that they were present.
    let body = [
        0x07, 0x04, 0x01, 0xed, 0x03, 0x01, 0x0a, 0x00, 0x00, 0x00,
    ];
    let directives = dispatch(play::clientbound::ENTITY_EQUIPMENT, &body);
    let [Directive::Emit(ClientEvent::EntityEquipmentUpdated { equipment, .. })] = directives.as_slice() else {
        panic!("expected one equipment event, got {directives:?}");
    };
    assert!(equipment[0]
        .item
        .as_ref()
        .expect("present stack")
        .components
        .has_unmodeled);
}

#[test]
fn literal_block_action_reaches_the_visible_block_event_forwarder() {
    // Packed pre-1.14 position (1, 64, -3), b0=1, b1=2, block type=142
    // (chest). 142 is encoded as VarInt 8e 01; x/y/z and b0/b1 are all
    // deliberately non-interchangeable.
    let body = [
        0x00, 0x00, 0x00, 0x41, 0x03, 0xff, 0xff, 0xfd, 0x01, 0x02, 0x8e, 0x01,
    ];
    let directives = dispatch(play::clientbound::BLOCK_ACTION, &body);
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one block event, got {directives:?}");
    };
    let ClientEvent::BlockEvent { pos, b0, b1, block } = event else {
        panic!("expected BlockEvent, got {event:?}");
    };
    assert_eq!(*pos, BlockPos::new(1, 64, -3));
    assert_eq!((*b0, *b1), (1, 2));
    assert_eq!(block.to_string(), "minecraft:chest");
    assert!(route(event).must_forward(), "block event must reach shell animation state");
}

#[test]
fn unsupported_protocol_404_event_registry_ids_fail_loudly() {
    // A present stack with an id outside the complete protocol-404 registry.
    let equipment = [0x01, 0x00, 0x01, 0x96, 0x06, 0x01, 0x00];
    let error = V404Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::ENTITY_EQUIPMENT,
            &equipment,
        )
        .expect_err("unmapped registry ids must not become a wrong item");
    assert!(error.to_string().contains("unsupported protocol-404 equipment item id 790"));

    // Position zero, distinct parameters, and a block type outside the
    // complete protocol-404 block registry (598 as VarInt d6 04).
    let block = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xd6, 0x04];
    let error = V404Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::BLOCK_ACTION,
            &block,
        )
        .expect_err("unmapped block-event ids must not become a wrong block");
    assert!(error.to_string().contains("unsupported protocol-404 block_action type id 598"));
}
