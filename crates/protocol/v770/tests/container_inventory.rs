//! Hermetic tests for protocol 776 clientbound inventory packets.
//!
//! Golden byte vectors are hand-built from the 26.2 wire spec so a symmetric
//! bug cannot pass silently. An item stack is `count VarInt` (`<= 0` empty),
//! `item id VarInt`, then a data-component patch (`added VarInt`,
//! `removed VarInt`). `container id` and `state id` are VarInts; slot/property/
//! value are big-endian shorts. Every payload is exercised through the public
//! adapter, and every decode asserts zero trailing bytes.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemStack, ResourceKey, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

fn handle(id: i32, payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, id, payload)
        .expect("handle inventory packet")
}

fn key(s: &str) -> ResourceKey {
    s.parse().expect("valid key")
}

/// `64 × minecraft:stone` (item id 1) with an empty component patch.
const STONE_64: [u8; 4] = [0x40, 0x01, 0x00, 0x00];
/// The empty stack.
const EMPTY_STACK: [u8; 1] = [0x00];

#[test]
fn container_set_slot_decodes_a_plain_stack() {
    // window 1, state 5, slot 36, then the stone stack.
    let mut payload = vec![0x01, 0x05, 0x00, 0x24];
    payload.extend_from_slice(&STONE_64);
    match handle(play::clientbound::CONTAINER_SET_SLOT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerSlot {
                window_id,
                state_id,
                slot,
                item,
            }),
        ] => {
            assert_eq!(*window_id, 1);
            assert_eq!(*state_id, 5);
            assert_eq!(*slot, 36);
            assert_eq!(
                *item,
                Some(ItemStack {
                    item: key("minecraft:stone"),
                    count: 64,
                })
            );
        }
        other => panic!("expected ContainerSlot, got {other:?}"),
    }
}

#[test]
fn container_set_slot_decodes_the_empty_stack() {
    let mut payload = vec![0x01, 0x01, 0x00, 0x00];
    payload.extend_from_slice(&EMPTY_STACK);
    match handle(play::clientbound::CONTAINER_SET_SLOT, &payload).as_slice() {
        [Directive::Emit(ClientEvent::ContainerSlot { item, .. })] => assert_eq!(*item, None),
        other => panic!("expected ContainerSlot, got {other:?}"),
    }
}

#[test]
fn container_set_content_decodes_items_and_carried() {
    // window 1, state 2, two items [stone, empty], carried empty.
    let mut payload = vec![0x01, 0x02, 0x02];
    payload.extend_from_slice(&STONE_64);
    payload.extend_from_slice(&EMPTY_STACK); // second slot empty
    payload.extend_from_slice(&EMPTY_STACK); // carried empty
    match handle(play::clientbound::CONTAINER_SET_CONTENT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            }),
        ] => {
            assert_eq!(*window_id, 1);
            assert_eq!(*state_id, 2);
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_ref().map(|s| s.count), Some(64));
            assert_eq!(items[1], None);
            assert_eq!(*carried_item, None);
        }
        other => panic!("expected ContainerContent, got {other:?}"),
    }
}

#[test]
fn container_set_data_decodes_property_channel() {
    // window 1, property 0, value 200 (0x00C8).
    let payload = [0x01, 0x00, 0x00, 0x00, 0xC8];
    match handle(play::clientbound::CONTAINER_SET_DATA, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerData {
                window_id,
                property,
                value,
            }),
        ] => {
            assert_eq!(*window_id, 1);
            assert_eq!(*property, 0);
            assert_eq!(*value, 200);
        }
        other => panic!("expected ContainerData, got {other:?}"),
    }
}

#[test]
fn container_close_decodes_window_id() {
    let payload = [0x07];
    match handle(play::clientbound::CONTAINER_CLOSE, &payload).as_slice() {
        [Directive::Emit(ClientEvent::ScreenClosed { window_id })] => assert_eq!(*window_id, 7),
        other => panic!("expected ScreenClosed, got {other:?}"),
    }
}

#[test]
fn item_stack_with_component_patch_is_refused_loudly() {
    // window 1, state 1, slot 1, then item: count 1, id 1, added=1, removed=0 —
    // a non-empty patch. The decoder must refuse rather than misparse the
    // un-length-prefixed component bytes.
    let payload = vec![0x01, 0x01, 0x00, 0x01, 0x01, 0x01, 0x01, 0x00];
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::CONTAINER_SET_SLOT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a non-empty component patch must be refused, got {result:?}"
    );
}

#[test]
fn container_set_slot_rejects_trailing_bytes() {
    let mut payload = vec![0x01, 0x05, 0x00, 0x24];
    payload.extend_from_slice(&STONE_64);
    payload.push(0xFF); // one stray byte
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::CONTAINER_SET_SLOT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a trailing byte must fail decode, got {result:?}"
    );
}
