//! Baseline contract coverage for the inventory observation seam.
//!
//! The native event bus carries the complete `ItemStack`; the current WASM
//! record carries the stable item key and count. This fixture keeps that
//! boundary explicit while the ABI/data owner extends the copied record. It
//! also proves that the existing observation capability is a real data-flow
//! gate rather than an unconditional event path.

use lodestone_model::{ClientEvent, ItemComponents, ItemStack, Text};
use lodestone_wasm_host::{lift_event, Capability, CapabilitySet, Event};

fn componentful_stack() -> ItemStack {
    ItemStack {
        item: "minecraft:diamond_sword".parse().expect("valid item key"),
        count: 7,
        components: ItemComponents {
            custom_name: Some(Text::literal("Tagged blade")),
            damage: Some(17),
            custom_data: Some(vec![0x0a, 0x00]),
            has_unmodeled: true,
            ..Default::default()
        },
    }
}

fn inventory_event() -> ClientEvent {
    ClientEvent::InventorySlotChanged {
        slot: 36,
        item: Some(componentful_stack()),
    }
}

/// The currently exposed WASM projection preserves the native slot, key, and
/// count exactly. The native source assertion deliberately checks component
/// data too, making the remaining component gap visible instead of allowing a
/// key/count-only round trip to masquerade as full equivalence.
#[test]
fn inventory_projection_keeps_identity_and_count_against_native_source() {
    let native = inventory_event();
    let ClientEvent::InventorySlotChanged {
        slot: native_slot,
        item: Some(native_item),
    } = &native
    else {
        panic!("fixture must contain one populated inventory slot");
    };
    assert_eq!(*native_slot, 36);
    assert_eq!(native_item.item.to_string(), "minecraft:diamond_sword");
    assert_eq!(native_item.count, 7);
    assert_eq!(native_item.components.damage, Some(17));
    assert_eq!(
        native_item.components.custom_data.as_deref(),
        Some([0x0a, 0x00].as_slice())
    );
    assert!(native_item.components.has_unmodeled);

    let lifted = lift_event(
        &native,
        &CapabilitySet::from_iter([Capability::ObserveInventory]),
    )
    .expect("observe:inventory must lift the source event");
    let Event::InventorySlotChanged(observed) = lifted else {
        panic!("inventory source must lift to inventory-slot-changed");
    };
    let observed_item = observed.item.expect("fixture item must remain populated");
    assert_eq!(observed.slot, *native_slot);
    assert_eq!(observed_item.item, native_item.item.to_string());
    assert_eq!(observed_item.count, native_item.count);
}

/// Data-flow observation is denied without its exact grant, including when a
/// different observation/control capability is present.
#[test]
fn inventory_projection_is_denied_without_its_capability() {
    let event = inventory_event();
    assert_eq!(lift_event(&event, &CapabilitySet::empty()), None);
    assert_eq!(
        lift_event(
            &event,
            &CapabilitySet::from_iter([Capability::ObserveChat]),
        ),
        None,
        "an unrelated observation grant must not open inventory data"
    );
}

