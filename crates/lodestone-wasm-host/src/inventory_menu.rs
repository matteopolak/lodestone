//! Capability-gated copies of the native inventory/menu event family.
//!
//! The native event bus retains the full decoded `ClientEvent`; this module is
//! only the WASM copy boundary. It deliberately flattens the menu title and
//! canonical menu key to strings and keeps item stacks at the existing
//! key/count projection. No menu cache or mutable inventory handle crosses the
//! boundary.

use lodestone_model::ClientEvent;

use crate::capability::{Capability, CapabilitySet};
use crate::host::{
    Event, InventoryContainerContent, InventoryContainerDataChanged,
    InventoryContainerSlotChanged, InventoryCursorItemChanged, InventoryHeldSlotChanged,
    InventoryMountScreenOpened, InventoryScreenClosed, InventoryScreenOpened, ItemStack,
};

fn item_stack(item: &lodestone_model::ItemStack) -> ItemStack {
    ItemStack {
        item: item.item.to_string(),
        count: item.count,
    }
}

fn optional_item(item: &Option<lodestone_model::ItemStack>) -> Option<ItemStack> {
    item.as_ref().map(item_stack)
}

/// Lift the inventory/menu events not covered by the existing native-inventory
/// slot arm. The caller remains responsible for routing this result through the
/// production `drive_wasm_plugins` conductor.
pub(crate) fn lift_event(
    event: &ClientEvent,
    granted: &CapabilitySet,
) -> Option<Event> {
    if !granted.contains(Capability::ObserveInventory) {
        return None;
    }

    Some(match event {
        ClientEvent::ContainerContent {
            window_id,
            state_id,
            items,
            carried_item,
        } => Event::InventoryContainerContent(InventoryContainerContent {
            window_id: *window_id,
            state_id: state_id.raw(),
            items: items.iter().map(|item| item.as_ref().map(item_stack)).collect(),
            carried_item: optional_item(carried_item),
        }),
        ClientEvent::ContainerSlot {
            window_id,
            state_id,
            slot,
            item,
        } => Event::InventoryContainerSlotChanged(InventoryContainerSlotChanged {
            window_id: *window_id,
            state_id: state_id.raw(),
            slot: *slot,
            item: optional_item(item),
        }),
        ClientEvent::ContainerData {
            window_id,
            property,
            value,
        } => Event::InventoryContainerDataChanged(InventoryContainerDataChanged {
            window_id: *window_id,
            property: *property,
            value: *value,
        }),
        ClientEvent::ScreenOpened {
            window_id,
            menu_type,
            title,
        } => Event::InventoryScreenOpened(InventoryScreenOpened {
            window_id: *window_id,
            menu_type: menu_type.to_string(),
            title: title.to_plain_string(),
        }),
        ClientEvent::ScreenClosed { window_id } => {
            Event::InventoryScreenClosed(InventoryScreenClosed { window_id: *window_id })
        }
        ClientEvent::MountScreenOpened {
            container_id,
            inventory_columns,
            entity_id,
        } => Event::InventoryMountScreenOpened(InventoryMountScreenOpened {
            container_id: *container_id,
            inventory_columns: *inventory_columns,
            entity_id: *entity_id,
        }),
        ClientEvent::HeldSlotChanged { slot } => {
            Event::InventoryHeldSlotChanged(InventoryHeldSlotChanged { slot: *slot })
        }
        ClientEvent::CursorItemChanged { item } => {
            Event::InventoryCursorItemChanged(InventoryCursorItemChanged {
                item: optional_item(item),
            })
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(key: &str, count: u32) -> lodestone_model::ItemStack {
        lodestone_model::ItemStack::new(key.parse().expect("valid item key"), count)
    }

    #[test]
    fn content_lift_copies_container_identity_items_cursor_and_title_surface() {
        let item = stack("minecraft:diamond", 7);
        let event = ClientEvent::ContainerContent {
            window_id: 4,
            state_id: lodestone_model::ContainerStateId::new(31),
            items: vec![None, Some(item.clone())],
            carried_item: Some(item),
        };
        let granted = CapabilitySet::from_iter([Capability::ObserveInventory]);

        let Some(Event::InventoryContainerContent(content)) = lift_event(&event, &granted) else {
            panic!("container content must reach a granted guest");
        };
        assert_eq!(content.window_id, 4);
        assert_eq!(content.state_id, 31);
        assert_eq!(content.items[0], None);
        assert_eq!(
            content.items[1].as_ref().map(|item| item.item.as_str()),
            Some("minecraft:diamond")
        );
        assert_eq!(content.items[1].as_ref().map(|item| item.count), Some(7));
        assert_eq!(content.carried_item.as_ref().map(|item| item.count), Some(7));
    }

    #[test]
    fn menu_lift_is_denied_without_observe_inventory_and_ignores_unrelated_events() {
        let event = ClientEvent::ScreenClosed { window_id: 3 };
        assert_eq!(lift_event(&event, &CapabilitySet::empty()), None);
        assert_eq!(
            lift_event(
                &ClientEvent::HealthChanged {
                    health: 20.0,
                    food: 20,
                    saturation: 5.0,
                },
                &CapabilitySet::from_iter([Capability::ObserveInventory]),
            ),
            None
        );
    }
}
