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
fn literal_block_break_animation_preserves_visible_and_clear_stages() {
    // entity=300, packed Position(-1, 64, 2), visible stage=9. These are
    // literal protocol-404 bytes, so the adapter reader is not paired with
    // this crate's packet encoder.
    let visible = [
        0xac, 0x02, 0xff, 0xff, 0xff, 0xc1, 0x00, 0x00, 0x00, 0x02, 0x09,
    ];
    let directives = dispatch(play::clientbound::BLOCK_BREAK_ANIMATION, &visible);
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one block-destruction event, got {directives:?}");
    };
    let ClientEvent::BlockDestruction {
        entity_id,
        pos,
        progress,
    } = event
    else {
        panic!("expected BlockDestruction, got {event:?}");
    };
    assert_eq!(*entity_id, 300);
    assert_eq!(*pos, BlockPos::new(-1, 64, 2));
    assert_eq!(*progress, 9);
    assert!(route(event).session, "block destruction must reach the session fold");

    // The same position with the signed -1 clear sentinel. Preserving 0xff
    // prevents a reset from becoming a fresh visible crack stage.
    let clear = [
        0xac, 0x02, 0xff, 0xff, 0xff, 0xc1, 0x00, 0x00, 0x00, 0x02, 0xff,
    ];
    let directives = dispatch(play::clientbound::BLOCK_BREAK_ANIMATION, &clear);
    let [Directive::Emit(ClientEvent::BlockDestruction { progress, .. })] = directives.as_slice() else {
        panic!("expected one clear block-destruction event, got {directives:?}");
    };
    assert_eq!(*progress, u8::MAX);
}

#[test]
fn literal_entity_metadata_reaches_the_ingest_route() {
    // Entity 19; shared flags at index 0, an explicit custom-name clear at
    // index 2, name visibility at index 3, and the metadata terminator. These
    // are assembled from the 404 field widths rather than this crate's codec.
    let body = [
        0x13, // entity id
        0x00, 0x00, 0xa4, // index 0, byte serializer, signed flags byte
        0x02, 0x05, 0x00, // index 2, optional chat serializer, absent
        0x03, 0x07, 0x01, // index 3, boolean serializer, visible
        0xff,
    ];
    let directives = dispatch(play::clientbound::ENTITY_METADATA, &body);
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one metadata event, got {directives:?}");
    };
    let ClientEvent::EntityMetadataUpdated {
        entity_id,
        metadata,
    } = event
    else {
        panic!("expected EntityMetadataUpdated, got {event:?}");
    };
    assert_eq!(*entity_id, 19);
    assert_eq!(metadata.flags, Some(0xa4));
    assert_eq!(metadata.custom_name_visible, Some(true));
    assert!(route(event).ingest, "metadata must enter the ECS fold");
}

#[test]
fn literal_entity_attributes_reach_the_ingest_route() {
    // Entity 19, one textual generic attribute with base 20.0, and one UUID
    // modifier. The property count is a big-endian i32; all list counts and
    // the attribute name length use VarInts on protocol 404.
    let mut body = vec![0x13];
    body.extend_from_slice(&1_i32.to_be_bytes());
    body.push(0x11); // "generic.maxHealth" has 17 UTF-8 bytes
    body.extend_from_slice(b"generic.maxHealth");
    body.extend_from_slice(&20.0f64.to_be_bytes());
    body.push(0x01); // one modifier
    body.extend_from_slice(&[
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
        0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    ]);
    body.extend_from_slice(&0.25f64.to_be_bytes());
    body.push(0x02); // multiply-total
    let directives = dispatch(play::clientbound::ENTITY_UPDATE_ATTRIBUTES, &body);
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one attribute event, got {directives:?}");
    };
    let ClientEvent::EntityAttributesUpdated {
        entity_id,
        attributes,
    } = event
    else {
        panic!("expected EntityAttributesUpdated, got {event:?}");
    };
    assert_eq!(*entity_id, 19);
    assert_eq!(attributes.len(), 1);
    assert_eq!(attributes[0].attribute.to_string(), "minecraft:max_health");
    assert_eq!(attributes[0].base, 20.0);
    assert_eq!(attributes[0].modifiers.len(), 1);
    assert_eq!(attributes[0].modifiers[0].amount, 0.25);
    assert_eq!(attributes[0].modifiers[0].operation, 2);
    assert!(route(event).ingest, "attributes must enter the ECS fold");
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
