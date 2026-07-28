//! Crafting-table menu shape: slot kinds, slot *order*, and the
//! `Menus`-level routing that picks the layout from the server's `open_screen`.
//!
//! Hermetic — no jar cache, no server. The values asserted here are vanilla's
//! `CraftingMenu` layout, not our own output.

use lodestone_game::container::SlotKind;
use lodestone_game::item::ItemStack;
use lodestone_game::menu::{Menu, MenuKind};
use lodestone_game::menus::Menus;
use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe};
use lodestone_model::ids::Identifier;
use lodestone_model::{ClientEvent, ItemComponents, ItemStack as ModelItemStack, Text};

fn id(s: &str) -> Identifier {
    s.parse().expect("valid identifier")
}

fn stack(name: &str, count: i32) -> ItemStack {
    ItemStack::new(id(name), count)
}

fn model_stack(name: &str, count: u32) -> ModelItemStack {
    ModelItemStack {
        item: id(name),
        count,
        components: ItemComponents::default(),
    }
}

#[test]
fn crafting_menu_has_vanilla_slot_count_and_kinds() {
    let menu = Menu::crafting(3, 3);
    // Vanilla CraftingMenu: 1 result + 9 grid + 27 main + 9 hotbar.
    assert_eq!(menu.slot_count(), 46);
    assert_eq!(menu.kind(), MenuKind::Generic { container_size: 10 });

    assert_eq!(menu.slot(0).unwrap().kind, SlotKind::Output);
    for i in 1..=9 {
        assert_eq!(
            menu.slot(i).unwrap().kind,
            SlotKind::CraftingInput,
            "menu slot {i} should be a crafting input"
        );
    }
    for i in 10..46 {
        assert_eq!(
            menu.slot(i).unwrap().kind,
            SlotKind::Normal,
            "menu slot {i} should be plain storage"
        );
    }
}

#[test]
fn the_result_slot_never_accepts_a_placed_item() {
    let menu = Menu::crafting(3, 3);
    assert!(!menu.may_place(0, &stack("minecraft:diamond", 1)));
    assert!(menu.may_place(1, &stack("minecraft:diamond", 1)));
    assert!(menu.may_pickup(0), "the result must still be takeable");
}

#[test]
fn the_hotbar_is_at_37_not_36() {
    // The trap: in a *generic* menu the player portion starts after the
    // container slots, so a crafting table's hotbar begins at 10 + 27 = 37.
    // Only the player's own inventory screen puts it at 36.
    let mut menu = Menu::crafting(3, 3);
    menu.set_player_native(0, Some(stack("minecraft:torch", 5)));
    assert_eq!(menu.slot_item(37), Some(&stack("minecraft:torch", 5)));
    assert_eq!(menu.slot_item(36), None);

    // First main-storage slot is native 9 at menu index 10.
    menu.set_player_native(9, Some(stack("minecraft:apple", 1)));
    assert_eq!(menu.slot_item(10), Some(&stack("minecraft:apple", 1)));

    // ...and the player's own screen really is different.
    let mut player = Menu::player();
    player.set_player_native(0, Some(stack("minecraft:torch", 5)));
    assert_eq!(player.slot_item(36), Some(&stack("minecraft:torch", 5)));
}

#[test]
fn crafting_grid_reads_row_major_from_the_input_slots() {
    let mut menu = Menu::crafting(3, 3);
    let layout = menu.craft_layout().expect("crafting table has a grid");
    assert_eq!((layout.width, layout.height), (3, 3));
    assert_eq!(layout.result_slot, 0);
    assert_eq!(layout.first_input, 1);

    // Top row only.
    menu.set_slot_item(1, Some(stack("minecraft:oak_planks", 1)));
    menu.set_slot_item(2, Some(stack("minecraft:oak_planks", 1)));
    menu.set_slot_item(3, Some(stack("minecraft:oak_planks", 1)));

    let grid = menu.crafting_grid().expect("grid snapshot");
    assert_eq!(grid.width(), 3);
    assert_eq!(grid.height(), 3);
    assert_eq!(grid.get(0, 0), Some(&id("minecraft:oak_planks")));
    assert_eq!(grid.get(2, 0), Some(&id("minecraft:oak_planks")));
    // Row 1 is empty — a transposed read would report planks down column 0.
    assert_eq!(grid.get(0, 1), None);
}

#[test]
fn the_player_screen_exposes_its_2x2_grid() {
    let menu = Menu::player();
    let layout = menu.craft_layout().expect("player screen has a 2x2");
    assert_eq!((layout.width, layout.height), (2, 2));
    assert_eq!(layout.result_slot, 0);
    assert_eq!(layout.first_input, 1);
    assert!(Menu::generic(27).craft_layout().is_none());
}

#[test]
fn shift_clicking_from_the_inventory_targets_the_grid_not_the_result() {
    let mut menu = Menu::crafting(3, 3);
    // A stack in the hotbar.
    menu.set_slot_item(37, Some(stack("minecraft:oak_planks", 4)));
    menu.quick_move(37).expect("something moved");

    assert_eq!(menu.slot_item(0), None, "result slot must stay empty");
    assert_eq!(
        menu.slot_item(1),
        Some(&stack("minecraft:oak_planks", 4)),
        "planks should land in the first grid cell"
    );
    assert_eq!(menu.slot_item(37), None);
}

#[test]
fn shift_clicking_the_result_sends_it_to_the_player_inventory() {
    let mut menu = Menu::crafting(3, 3);
    menu.set_slot_item(0, Some(stack("minecraft:crafting_table", 1)));
    menu.quick_move(0).expect("result moved");
    assert_eq!(menu.slot_item(0), None);
    // Vanilla fills the player region from the back, i.e. the last hotbar slot.
    assert_eq!(
        menu.slot_item(45),
        Some(&stack("minecraft:crafting_table", 1))
    );
}

/// The end-to-end routing: a server `open_screen` for `minecraft:crafting`
/// followed by a 46-slot `container_set_content` must produce a *crafting*
/// menu, not a 10-slot chest.
#[test]
fn menus_builds_a_crafting_menu_from_open_screen_plus_content() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 1,
        menu_type: id("minecraft:crafting"),
        title: Text::literal("Crafting"),
    });
    let mut items = vec![None; 46];
    items[1] = Some(model_stack("minecraft:oak_planks", 1));
    items[2] = Some(model_stack("minecraft:oak_planks", 1));
    items[4] = Some(model_stack("minecraft:oak_planks", 1));
    items[5] = Some(model_stack("minecraft:oak_planks", 1));
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 1,
        state_id: 7,
        items,
        carried_item: None,
    });

    let open = menus.opened().expect("a container is open");
    assert_eq!(open.slot_count(), 46);
    assert_eq!(open.slot(0).unwrap().kind, SlotKind::Output);
    let layout = open.craft_layout().expect("routed to a crafting menu");
    assert_eq!((layout.width, layout.height), (3, 3));

    // The 2x2 of planks the server sent lands in the top-left of the grid.
    let grid = menus.crafting_grid().expect("active menu has a grid");
    assert_eq!(grid.get(0, 0), Some(&id("minecraft:oak_planks")));
    assert_eq!(grid.get(1, 0), Some(&id("minecraft:oak_planks")));
    assert_eq!(grid.get(0, 1), Some(&id("minecraft:oak_planks")));
    assert_eq!(grid.get(1, 1), Some(&id("minecraft:oak_planks")));
    assert_eq!(grid.get(2, 2), None);
}

#[test]
fn a_chest_is_still_a_plain_generic_menu() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 2,
        menu_type: id("minecraft:generic_9x3"),
        title: Text::literal("Chest"),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 2,
        state_id: 1,
        items: vec![None; 27 + 36],
        carried_item: None,
    });
    let open = menus.opened().expect("chest open");
    assert_eq!(open.kind(), MenuKind::Generic { container_size: 27 });
    assert!(open.craft_layout().is_none());
    assert!(menus.crafting_grid().is_none());
}

/// A hand-built book: 2×2 planks → a crafting table, vanilla's shape.
fn tiny_book() -> RecipeBook {
    let planks = Ingredient::Item(id("minecraft:oak_planks"));
    let recipe = ShapedRecipe::new(
        2,
        2,
        vec![
            Some(planks.clone()),
            Some(planks.clone()),
            Some(planks.clone()),
            Some(planks),
        ],
        stack("minecraft:crafting_table", 1),
    );
    let mut book = RecipeBook::new();
    book.insert(id("minecraft:crafting_table"), Recipe::Shaped(recipe));
    book
}

#[test]
fn a_grid_the_book_knows_predicts_a_result() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 1,
        menu_type: id("minecraft:crafting"),
        title: Text::literal("Crafting"),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 1,
        state_id: 1,
        items: vec![None; 46],
        carried_item: None,
    });
    let book = tiny_book();

    // Empty grid predicts nothing.
    assert_eq!(menus.predicted_craft_result(&book), None);

    // Fill the top-left 2x2 of the 3x3 grid: menu slots 1, 2, 4, 5.
    for slot in [1, 2, 4, 5] {
        menus.apply(&ClientEvent::ContainerSlot {
            window_id: 1,
            state_id: 2,
            slot,
            item: Some(model_stack("minecraft:oak_planks", 1)),
        });
    }
    assert_eq!(
        menus.predicted_craft_result(&book),
        Some(stack("minecraft:crafting_table", 1))
    );

    // A stray fifth plank in an uncovered cell breaks the match, as vanilla does.
    menus.apply(&ClientEvent::ContainerSlot {
        window_id: 1,
        state_id: 3,
        slot: 9,
        item: Some(model_stack("minecraft:oak_planks", 1)),
    });
    assert_eq!(menus.predicted_craft_result(&book), None);
}

#[test]
fn a_chest_predicts_nothing_because_it_has_no_grid() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 4,
        menu_type: id("minecraft:generic_9x3"),
        title: Text::literal("Chest"),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 4,
        state_id: 1,
        items: vec![None; 27 + 36],
        carried_item: None,
    });
    assert_eq!(menus.predicted_craft_result(&tiny_book()), None);
}

/// Server truth wins over the menu-type hint: if a server ever advertises
/// `minecraft:crafting` with a content length that is not 46, we must size the
/// menu from the packet rather than build a 46-slot menu the packet cannot fill.
#[test]
fn a_crafting_menu_type_with_an_unexpected_size_falls_back_to_generic() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 3,
        menu_type: id("minecraft:crafting"),
        title: Text::literal("Crafting"),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 3,
        state_id: 1,
        items: vec![None; 5 + 36],
        carried_item: None,
    });
    let open = menus.opened().expect("open");
    assert_eq!(open.slot_count(), 41);
    assert!(open.craft_layout().is_none());
}
