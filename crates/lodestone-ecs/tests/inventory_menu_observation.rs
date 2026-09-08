//! Production-schedule coverage for the typed inventory/menu plugin view.
//!
//! The observer is installed on the same `GameTick` and `GameEventBusPlugin`
//! path native plugins use. It receives a server-style open/close sequence and
//! reads the borrowed typed values without reaching into the session aggregate.

use bevy_ecs::prelude::{MessageReader, ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_ecs::app::App;
use lodestone_ecs::events::{GameEvent, GameEventBusPlugin, InventoryMenuEvent};
use lodestone_ecs::{GameTick, TickSet};
use lodestone_model::{ClientEvent, ContainerStateId, ItemComponents, ItemStack, Text};

#[derive(Debug, Default, PartialEq, Eq, Resource)]
struct Seen {
    opened: Vec<(i32, String, String)>,
    closed: Vec<i32>,
    contents: Vec<(i32, ContainerStateId, usize, Option<u32>, bool)>,
    slots: Vec<(i32, ContainerStateId, i32, Option<u32>, bool)>,
    data: Vec<(i32, i32, i32)>,
    cursor: Vec<Option<u32>>,
    held: Vec<i32>,
    inventory: Vec<(i32, Option<u32>, bool)>,
    mounts: Vec<(i32, i32, i32)>,
}

fn observe_menu_lifecycle(
    mut events: MessageReader<GameEvent>,
    mut seen: ResMut<Seen>,
) {
    for event in events.read() {
        match event.inventory_menu() {
            Some(InventoryMenuEvent::ScreenOpened {
                window_id,
                menu_type,
                title,
            }) => seen.opened.push((
                window_id,
                menu_type.to_string(),
                title.to_plain_string(),
            )),
            Some(InventoryMenuEvent::ScreenClosed { window_id }) => {
                seen.closed.push(window_id)
            }
            Some(InventoryMenuEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            }) => seen.contents.push((
                window_id,
                state_id,
                items.len(),
                carried_item.map(|item| item.count),
                carried_item.is_some_and(|item| item.components.has_unmodeled),
            )),
            Some(InventoryMenuEvent::ContainerSlot {
                window_id,
                state_id,
                slot,
                item,
            }) => seen.slots.push((
                window_id,
                state_id,
                slot,
                item.map(|item| item.count),
                item.is_some_and(|item| item.components.has_unmodeled),
            )),
            Some(InventoryMenuEvent::ContainerData {
                window_id,
                property,
                value,
            }) => seen.data.push((window_id, property, value)),
            Some(InventoryMenuEvent::CursorItemChanged { item }) => {
                seen.cursor.push(item.map(|item| item.count))
            }
            Some(InventoryMenuEvent::HeldSlotChanged { slot }) => seen.held.push(slot),
            Some(InventoryMenuEvent::InventorySlotChanged { slot, item }) => seen
                .inventory
                .push((
                    slot,
                    item.map(|item| item.count),
                    item.is_some_and(|item| item.components.has_unmodeled),
                )),
            Some(InventoryMenuEvent::MountScreenOpened {
                container_id,
                inventory_columns,
                entity_id,
            }) => seen
                .mounts
                .push((container_id, inventory_columns, entity_id)),
            _ => {}
        }
    }
}

fn componentful_item(count: u32) -> ItemStack {
    ItemStack {
        item: "minecraft:diamond".parse().expect("valid item key"),
        count,
        components: ItemComponents {
            damage: Some(17),
            has_unmodeled: true,
            ..Default::default()
        },
    }
}

/// A native plugin observer sees the complete menu lifecycle in the real
/// event-bus schedule. The values are deliberately not reconstructed from
/// `SessionMenus`; they are the event payload received by the observer. The
/// component marker and count prove that the typed view borrows the complete
/// native stack rather than reducing it to an item key/count pair.
#[test]
fn game_tick_delivers_typed_menu_lifecycle_and_full_stack_observations() {
    let mut app = App::new();
    app.add_plugins(GameEventBusPlugin);
    app.init_resource::<Seen>();
    app.add_systems(
        GameTick,
        observe_menu_lifecycle.in_set(TickSet::Intent),
    );

    let item = componentful_item(3);
    let cursor_item = componentful_item(5);
    let inventory_item = componentful_item(9);
    let menu_type = "minecraft:chest".parse().expect("valid menu key");
    app.world_mut().write_message(GameEvent(ClientEvent::ScreenOpened {
        window_id: 7,
        menu_type,
        title: Text::literal("Treasure"),
    }));
    app.world_mut().write_message(GameEvent(ClientEvent::ContainerContent {
        window_id: 7,
        state_id: ContainerStateId::new(11),
        items: vec![Some(item.clone()), None],
        carried_item: Some(cursor_item.clone()),
    }));
    app.world_mut().write_message(GameEvent(ClientEvent::ContainerSlot {
        window_id: 7,
        state_id: ContainerStateId::new(12),
        slot: 4,
        item: Some(item),
    }));
    app.world_mut().write_message(GameEvent(ClientEvent::ContainerData {
        window_id: 7,
        property: 2,
        value: 37,
    }));
    app.world_mut().write_message(GameEvent(ClientEvent::CursorItemChanged {
        item: Some(cursor_item),
    }));
    app.world_mut()
        .write_message(GameEvent(ClientEvent::HeldSlotChanged { slot: 3 }));
    app.world_mut()
        .write_message(GameEvent(ClientEvent::InventorySlotChanged {
            slot: 36,
            item: Some(inventory_item),
        }));
    app.world_mut()
        .write_message(GameEvent(ClientEvent::MountScreenOpened {
            container_id: 8,
            inventory_columns: 5,
            entity_id: 42,
        }));
    app.world_mut()
        .write_message(GameEvent(ClientEvent::ScreenClosed { window_id: 7 }));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world().resource::<Seen>(),
        &Seen {
            opened: vec![(7, "minecraft:chest".to_owned(), "Treasure".to_owned())],
            closed: vec![7],
            contents: vec![(7, ContainerStateId::new(11), 2, Some(5), true)],
            slots: vec![(7, ContainerStateId::new(12), 4, Some(3), true)],
            data: vec![(7, 2, 37)],
            cursor: vec![Some(5)],
            held: vec![3],
            inventory: vec![(36, Some(9), true)],
            mounts: vec![(8, 5, 42)],
        }
    );
}
