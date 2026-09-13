//! Hermetic tests for protocol 776 clientbound `set_equipment`.
//!
//! Wire: `entity id VarInt`, then a continuation-flagged list. Each entry is a
//! slot byte whose low 7 bits are the `EquipmentSlot` ordinal (MainHand=0 …
//! Saddle=7) and whose high bit (`0x80`) signals another entry follows, then an
//! optional item stack (`count VarInt`, `item id VarInt`, empty component patch).
//! Golden bytes are hand-built so the continuation-bit framing cannot regress
//! silently, and every decode asserts zero trailing bytes.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, EquipmentSlot, ItemStack, ResourceKey, VersionAdapter,
};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;

fn handle(payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_EQUIPMENT,
            payload,
        )
        .expect("handle set_equipment")
}

fn key(s: &str) -> ResourceKey {
    s.parse().expect("valid key")
}

#[test]
fn set_equipment_decodes_two_slots_via_continuation_bit() {
    // entity 42; head (ordinal 5) with continuation bit set → 0x85, item empty;
    // main-hand (ordinal 0) terminal → 0x00, item 1 × diamond (id 926 → VarInt
    // 0x9E 0x07), empty patch.
    let payload = vec![
        0x2A, // entity id 42
        0x85, // head, continue
        0x00, // empty stack
        0x00, // main-hand, terminal
        0x01, 0x9E, 0x07, 0x00, 0x00, // 1 × diamond, empty patch
    ];
    match handle(&payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                entity_id,
                equipment,
            }),
        ] => {
            assert_eq!(*entity_id, 42);
            assert_eq!(equipment.len(), 2);
            assert_eq!(equipment[0].slot, EquipmentSlot::Head);
            assert_eq!(equipment[0].item, None);
            assert_eq!(equipment[1].slot, EquipmentSlot::MainHand);
            assert_eq!(
                equipment[1].item,
                Some(ItemStack {
                    item: key("minecraft:diamond"),
                    count: 1,
                    // An empty component *patch* does not mean empty components:
                    // the decoder folds the item's built-in prototype into the
                    // effective fields. `minecraft:diamond` stacks to 64, is not
                    // damageable and is not equippable — from the committed server
                    // dump (`tests/support/item_prototype_jvm.txt`), not from the
                    // census code. See `docs/item-prototypes.md`.
                    components: lodestone_model::ItemComponents {
                        max_stack_size: Some(64),
                        max_damage: None,
                        equippable: None,
                        ..lodestone_model::ItemComponents::default()
                    },
                })
            );
        }
        other => panic!("expected EntityEquipmentUpdated, got {other:?}"),
    }
}

#[test]
fn set_equipment_decodes_a_single_terminal_slot() {
    // entity 1; saddle (ordinal 7) terminal, empty stack.
    let payload = vec![0x01, 0x07, 0x00];
    match handle(&payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                entity_id,
                equipment,
            }),
        ] => {
            assert_eq!(*entity_id, 1);
            assert_eq!(equipment.len(), 1);
            assert_eq!(equipment[0].slot, EquipmentSlot::Saddle);
            assert_eq!(equipment[0].item, None);
        }
        other => panic!("expected EntityEquipmentUpdated, got {other:?}"),
    }
}

#[test]
fn set_equipment_rejects_trailing_bytes() {
    // A terminal entry followed by a stray byte must fail, not silently stop.
    let payload = vec![0x01, 0x07, 0x00, 0xFF];
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_EQUIPMENT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a trailing byte must fail decode, got {result:?}"
    );
}

#[test]
fn set_equipment_rejects_unknown_slot_ordinal() {
    // Ordinal 9 is out of range (only 0..=7 are defined slots); this must be a
    // hard decode error, not silently coerced to a nearby valid slot.
    let payload = vec![0x01, 0x09, 0x00];
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_EQUIPMENT,
        &payload,
    );
    assert!(
        result.is_err(),
        "an unknown equipment slot ordinal must fail decode, got {result:?}"
    );
}

#[test]
fn set_equipment_rejects_truncated_payload() {
    // The continuation bit promises another entry, but the buffer ends before
    // its slot byte arrives.
    let payload = vec![0x01, 0x85, 0x00];
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_EQUIPMENT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated continuation must fail decode, got {result:?}"
    );
}
