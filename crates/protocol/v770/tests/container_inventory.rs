//! Hermetic tests for protocol 776 clientbound inventory packets.
//!
//! Golden byte vectors are hand-built from the 26.2 wire spec so a symmetric
//! bug cannot pass silently. An item stack is `count VarInt` (`<= 0` empty),
//! `item id VarInt`, then a data-component patch (`added VarInt`,
//! `removed VarInt`). `container id` and `state id` are VarInts; slot/property/
//! value are big-endian shorts. Every payload is exercised through the public
//! adapter, and every decode asserts zero trailing bytes.
//!
//! Also covers `set_held_slot`, `set_experience`, `set_cursor_item`,
//! `set_player_inventory`, and `cooldown`, which share the same item-stack and
//! VarInt building blocks.

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
                    components: lodestone_model::ItemComponents::default(),
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

// ---- set_held_slot ---------------------------------------------------------

#[test]
fn set_held_slot_emits_slot() {
    let directives = handle(play::clientbound::SET_HELD_SLOT, &[0x04]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::HeldSlotChanged { slot: 4 })]
    );
}

#[test]
fn set_held_slot_rejects_trailing_bytes() {
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_HELD_SLOT,
        &[0x04, 0xFF],
    );
    assert!(
        result.is_err(),
        "a misaligned set_held_slot must be rejected"
    );
}

// ---- set_experience ---------------------------------------------------------

#[test]
fn set_experience_decodes_progress_level_total_wire_order() {
    // Wire order is progress (f32), level (varint), total (varint) — not the
    // constructor's declared field order.
    let mut payload = 0.5f32.to_be_bytes().to_vec();
    payload.push(0x1E); // level 30
    payload.push(0x64); // total 100
    let directives = handle(play::clientbound::SET_EXPERIENCE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ExperienceChanged {
            progress: 0.5,
            level: 30,
            total: 100,
        })]
    );
}

#[test]
fn set_experience_rejects_trailing_bytes() {
    let mut payload = 0.0f32.to_be_bytes().to_vec();
    payload.push(0x00);
    payload.push(0x00);
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_EXPERIENCE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_experience must be rejected"
    );
}

// ---- set_cursor_item ---------------------------------------------------------

#[test]
fn set_cursor_item_decodes_a_plain_stack() {
    let directives = handle(play::clientbound::SET_CURSOR_ITEM, &STONE_64);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::CursorItemChanged {
            item: Some(ItemStack {
                item: key("minecraft:stone"),
                count: 64,
                components: lodestone_model::ItemComponents::default(),
            }),
        })]
    );
}

#[test]
fn set_cursor_item_decodes_the_empty_stack() {
    let directives = handle(play::clientbound::SET_CURSOR_ITEM, &EMPTY_STACK);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::CursorItemChanged {
            item: None
        })]
    );
}

#[test]
fn set_cursor_item_rejects_trailing_bytes() {
    let mut payload = STONE_64.to_vec();
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_CURSOR_ITEM,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_cursor_item must be rejected"
    );
}

// ---- set_player_inventory ---------------------------------------------------

#[test]
fn set_player_inventory_decodes_slot_and_stack() {
    let mut payload = vec![0x08]; // slot 8
    payload.extend_from_slice(&STONE_64);
    let directives = handle(play::clientbound::SET_PLAYER_INVENTORY, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::InventorySlotChanged {
            slot: 8,
            item: Some(ItemStack {
                item: key("minecraft:stone"),
                count: 64,
                components: lodestone_model::ItemComponents::default(),
            }),
        })]
    );
}

#[test]
fn set_player_inventory_rejects_trailing_bytes() {
    let mut payload = vec![0x08];
    payload.extend_from_slice(&STONE_64);
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_PLAYER_INVENTORY,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_player_inventory must be rejected"
    );
}

// ---- cooldown ---------------------------------------------------------------

#[test]
fn cooldown_decodes_combined_namespace_path_string() {
    let group = "minecraft:ender_pearl";
    let mut payload = vec![group.len() as u8];
    payload.extend_from_slice(group.as_bytes());
    payload.push(0xA0); // duration_ticks varint low byte (continuation)
    payload.push(0x01); // duration_ticks varint high byte -> 160
    let directives = handle(play::clientbound::COOLDOWN, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ItemCooldown {
            group: key("minecraft:ender_pearl"),
            duration_ticks: 160,
        })]
    );
}

#[test]
fn cooldown_rejects_trailing_bytes() {
    let group = "minecraft:ender_pearl";
    let mut payload = vec![group.len() as u8];
    payload.extend_from_slice(group.as_bytes());
    payload.push(0x00);
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::COOLDOWN,
        &payload,
    );
    assert!(result.is_err(), "a misaligned cooldown must be rejected");
}
