//! Crafting-table menu shape: slot kinds, slot *order*, and the
//! `Menus`-level routing that picks the layout from the server's `open_screen`.
//!
//! Hermetic — no jar cache, no server. The values asserted here are vanilla's
//! `CraftingMenu` layout, not our own output.

use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::container::SlotKind;
use lodestone_game::item::ItemStack;
use lodestone_game::menu::{Menu, MenuKind};
use lodestone_game::menus::Menus;
use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe};
use lodestone_model::ids::Identifier;
use lodestone_model::{
    ClientAction, ClientEvent, ContainerClickType, ItemComponents, ItemStack as ModelItemStack,
    Text,
};

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

// ---------------------------------------------------------------------------
// Taking the result: `ResultSlot.onTake`.
//
// `Slot::may_place` returning false for the output slot is only half of
// "take-only". The other half is that taking must *cost* something: vanilla's
// `ResultSlot.onTake` removes one item from every occupied grid cell. Without
// it the ingredients are never consumed, so the client's very next prediction
// contradicts the server on every grid cell at once.
//
// The result stack itself is always seeded here the way it arrives in real
// life — as a value the *server* put in slot 0 — never matched locally.
// ---------------------------------------------------------------------------

/// A 3×3 menu holding a 2×2 of planks `per_cell` deep, plus the result the
/// server computed for it. Nothing here matches a recipe.
fn table_with_planks(per_cell: i32) -> Menu {
    let mut menu = Menu::crafting(3, 3);
    for cell in [1, 2, 4, 5] {
        menu.set_slot_item(cell, Some(stack("minecraft:oak_planks", per_cell)));
    }
    menu.set_slot_item(0, Some(stack("minecraft:crafting_table", 1)));
    menu
}

fn grid_counts(menu: &Menu) -> Vec<Option<i32>> {
    (1..=9)
        .map(|i| menu.slot_item(i).map(ItemStack::count))
        .collect()
}

#[test]
fn picking_up_the_result_consumes_one_from_every_occupied_grid_cell() {
    let mut menu = table_with_planks(2);
    Click::left(0).apply(&mut menu, PlayerCtx::survival());

    assert_eq!(
        menu.carried(),
        Some(&stack("minecraft:crafting_table", 1)),
        "the result must end up on the cursor"
    );
    assert_eq!(menu.slot_item(0), None, "the result slot empties");
    assert_eq!(
        grid_counts(&menu),
        vec![
            Some(1),
            Some(1),
            None,
            Some(1),
            Some(1),
            None,
            None,
            None,
            None
        ],
        "exactly one plank leaves each occupied cell; empty cells stay empty"
    );
}

/// The detector's negative control: the same click on a **grid** cell is an
/// ordinary take and must consume nothing else. If `on_take` fired on any slot
/// rather than the result slot, this is what would catch it.
#[test]
fn picking_up_a_grid_cell_is_not_a_craft() {
    let mut menu = table_with_planks(2);
    Click::left(1).apply(&mut menu, PlayerCtx::survival());

    assert_eq!(menu.carried(), Some(&stack("minecraft:oak_planks", 2)));
    assert_eq!(
        grid_counts(&menu),
        vec![
            None,
            Some(2),
            None,
            Some(2),
            Some(2),
            None,
            None,
            None,
            None
        ],
        "taking a cell must not decrement its neighbours"
    );
    assert_eq!(
        menu.slot_item(0),
        Some(&stack("minecraft:crafting_table", 1)),
        "and must not consume the server's result"
    );
}

/// A chest has no crafting grid, so slot 0 is plain storage and taking from it
/// is just a take. Proves the hook is gated on the craft layout, not on the
/// index.
#[test]
fn taking_slot_zero_of_a_chest_consumes_nothing() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:crafting_table", 1)));
    menu.set_slot_item(1, Some(stack("minecraft:oak_planks", 2)));
    Click::left(0).apply(&mut menu, PlayerCtx::survival());
    assert_eq!(menu.slot_item(1), Some(&stack("minecraft:oak_planks", 2)));
}

/// Vanilla's `doClick` "slot rejects placement but same item → pull into
/// cursor" branch *is* the result-slot path when you already hold a stack of
/// the result, and it takes too — so it must craft too.
#[test]
fn pulling_the_result_onto_a_matching_cursor_also_crafts() {
    let mut menu = table_with_planks(2);
    menu.set_carried(Some(stack("minecraft:crafting_table", 3)));
    Click::left(0).apply(&mut menu, PlayerCtx::survival());

    assert_eq!(
        menu.carried(),
        Some(&stack("minecraft:crafting_table", 4)),
        "the result merges onto the held stack"
    );
    assert_eq!(grid_counts(&menu)[0], Some(1), "and the grid pays for it");
}

#[test]
fn dropping_the_result_with_q_also_crafts() {
    let mut menu = table_with_planks(2);
    let outcome = Click::drop_one(0).apply(&mut menu, PlayerCtx::survival());
    assert_eq!(outcome.dropped, vec![stack("minecraft:crafting_table", 1)]);
    assert_eq!(grid_counts(&menu)[0], Some(1));
}

#[test]
fn number_key_swapping_the_result_out_also_crafts() {
    let mut menu = table_with_planks(2);
    // Hotbar key 3 -> native 3, which in a crafting menu is menu slot 37 + 3.
    Click::hotbar_swap(0, 3).apply(&mut menu, PlayerCtx::survival());
    assert_eq!(
        menu.slot_item(40),
        Some(&stack("minecraft:crafting_table", 1))
    );
    assert_eq!(grid_counts(&menu)[0], Some(1));
}

/// The result slot still refuses everything placed into it — including a stack
/// of the very item it is producing, which the pull-into-cursor branch above
/// handles from the other direction.
#[test]
fn the_result_slot_still_refuses_a_placement() {
    let mut menu = Menu::crafting(3, 3);
    menu.set_carried(Some(stack("minecraft:oak_planks", 4)));
    Click::left(0).apply(&mut menu, PlayerCtx::survival());
    assert_eq!(menu.slot_item(0), None);
    assert_eq!(menu.carried(), Some(&stack("minecraft:oak_planks", 4)));
}

/// **The repeat rule.** Vanilla's `doClick` QUICK_MOVE loop repeats while the
/// slot still holds the same item — but a client's `CraftingMenu` is built with
/// a null level access, so nothing refills the result between
/// iterations and the loop stops after exactly one craft. The repetition is the
/// *server's*: it runs the same loop over a menu that does refill, and pushes
/// the difference back as `container_set_slot`s.
///
/// So this asserts both halves: one craft locally, and a second craft only once
/// server truth has refilled slot 0.
#[test]
fn shift_clicking_the_result_crafts_once_locally_and_again_on_server_refill() {
    let mut menu = table_with_planks(2);
    Click::shift(0).apply(&mut menu, PlayerCtx::survival());

    assert_eq!(menu.slot_item(0), None);
    assert_eq!(
        menu.slot_item(45),
        Some(&stack("minecraft:crafting_table", 1)),
        "the player region fills from the back"
    );
    assert_eq!(
        grid_counts(&menu)[0],
        Some(1),
        "exactly one craft was predicted, not two"
    );

    // The server recomputes the recipe and pushes the next result.
    menu.set_slot_item(0, Some(stack("minecraft:crafting_table", 1)));
    Click::shift(0).apply(&mut menu, PlayerCtx::survival());
    assert_eq!(
        menu.slot_item(45),
        Some(&stack("minecraft:crafting_table", 2))
    );
    assert_eq!(
        grid_counts(&menu),
        vec![None; 9],
        "the second craft empties the grid"
    );
}

// ---------------------------------------------------------------------------
// The serverbound half: predicting a click and addressing it to a window.
// ---------------------------------------------------------------------------

fn open_table(window_id: i32) -> Menus {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id,
        menu_type: id("minecraft:crafting"),
        title: Text::literal("Crafting"),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id,
        state_id: 4,
        items: vec![None; 46],
        carried_item: None,
    });
    menus
}

/// The window-id trap: while a table is open, the player-inventory rows drawn
/// underneath it are **that container's** slots. Addressing them to window 0
/// sends a completely different slot list.
#[test]
fn a_click_in_an_open_table_is_addressed_to_that_window_not_zero() {
    let mut menus = open_table(7);
    menus.apply(&ClientEvent::ContainerSlot {
        window_id: 7,
        state_id: 5,
        slot: 37,
        item: Some(model_stack("minecraft:oak_planks", 4)),
    });

    // Shift-click the first hotbar slot of the *table's* menu.
    let (window, intent) = menus.click(Click::shift(37), PlayerCtx::survival());
    assert_eq!(window, 7, "the open container's window id, not 0");
    assert_eq!(intent.slot, 37);

    // The prediction moved the planks into the grid, not the result.
    let open = menus.opened().expect("still open");
    assert_eq!(open.slot_item(1), Some(&stack("minecraft:oak_planks", 4)));
    assert_eq!(open.slot_item(0), None);
    assert!(
        intent
            .changed_slots
            .iter()
            .any(|(slot, item)| *slot == 1 && item.is_some()),
        "the intent must carry the grid cell it filled: {:?}",
        intent.changed_slots
    );

    // With nothing open, the same click addresses the player's own window.
    let mut player_only = Menus::new();
    let (window, _) = player_only.click(Click::shift(37), PlayerCtx::survival());
    assert_eq!(window, 0);
}

/// The predicted click lowers into the canonical `ContainerClick` action the
/// adapter encodes — state id, mode, and the predicted diff all carried.
#[test]
fn a_predicted_click_lowers_into_a_container_click_action() {
    let mut menus = open_table(9);
    menus.apply(&ClientEvent::ContainerSlot {
        window_id: 9,
        state_id: 5,
        slot: 40,
        item: Some(model_stack("minecraft:oak_planks", 8)),
    });
    let action = menus.click_action(Click::left(40), PlayerCtx::survival());
    match action {
        ClientAction::ContainerClick {
            window_id,
            slot,
            button,
            click_type,
            changed_slots,
            carried_item,
            ..
        } => {
            assert_eq!(window_id, 9);
            assert_eq!(slot, 40);
            assert_eq!(button, 0);
            assert_eq!(click_type, ContainerClickType::Pickup);
            assert_eq!(
                carried_item.map(|i| (i.item.to_string(), i.count)),
                Some(("minecraft:oak_planks".to_owned(), 8)),
                "the cursor picked the stack up"
            );
            assert_eq!(
                changed_slots
                    .iter()
                    .map(|c| c.slot)
                    .collect::<Vec<_>>(),
                vec![40],
                "only the emptied slot changed"
            );
            assert!(changed_slots[0].item.is_none());
        }
        other => panic!("expected a ContainerClick, got {other:?}"),
    }
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
