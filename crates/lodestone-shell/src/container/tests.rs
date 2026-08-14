//! `container.rs`'s own test module, split out verbatim. Still
//! `container::tests`, so every test path is unchanged.
//!
//! The `use` block below is what the inline `mod tests`'s own `use super::*`
//! used to reach through the old file's crate-level imports; `use super::*`
//! now only sees the module root.

use lodestone_game::click::{Click, ContainerInput, drag_header, drag_type};
use lodestone_game::menu::{Menu, MenuKind, OUTSIDE_SLOT, SpecialLayout};
use lodestone_game::recipe::RecipeBook;

use crate::hud::VanillaFont;
use crate::hud::item_icon::IconAssets;

use super::background::{BackgroundKind, background_kind};

use super::*;
use lodestone_game::item::ItemStack;

const VIEW: (u32, u32) = (1280, 720);

/// A stand-in for `en_us.json`, holding only the `container.*` keys these
/// tests need. Deliberately narrow: `lodestone_model`'s built-in stub table
/// (`text::default_translation`) carries *no* `container.*` key at all, so a
/// title path that ignored this closure could not accidentally pass.
fn lang(key: &str) -> Option<String> {
    match key {
        "container.crafting" => Some("Crafting".to_owned()),
        "container.chest" => Some("Chest".to_owned()),
        _ => None,
    }
}

#[test]
fn a_translate_menu_title_renders_words_not_the_raw_key() {
    // Issue #52, exactly as the server sends it: `ClientboundOpenScreen`
    // carries `translate("container.crafting")`, never the word "Crafting".
    let title = lodestone_model::Text::translate("container.crafting", vec![]);
    assert_eq!(menu_title(&title, &lang), "Crafting");

    // -- negative control -------------------------------------------------
    // The call this replaced. If this ever stops producing the raw key, the
    // assertion above has stopped proving anything — either the model grew a
    // `container.*` entry into its stub table, or something upstream is
    // resolving the component before we see it.
    assert_eq!(
        title.to_plain_string(),
        "container.crafting",
        "the translator-free flatten must still leak the key, or the test above is vacuous"
    );

    // …and the resolved title is what the panel actually draws, uppercased
    // by `build_chrome` the way vanilla's container titles are not — the
    // point here is only that it is words. A chest proves the key is read
    // rather than one hard-coded answer.
    let chest = lodestone_model::Text::translate("container.chest", vec![]);
    assert_eq!(menu_title(&chest, &lang), "Chest");
}

#[test]
fn a_menu_title_survives_a_missing_language_table() {
    // The demo palette loads no `en_us.json`, and a server may send a key we
    // have no entry for. Neither may cost the title: `fallback` first, then
    // the key. A literal title (a renamed chest) is untouched either way.
    let with_fallback = lodestone_model::Text {
        content: lodestone_model::TextContent::Translate {
            key: "container.barrel".to_owned(),
            with: vec![],
            fallback: Some("Barrel".to_owned()),
        },
        ..lodestone_model::Text::default()
    };
    assert_eq!(menu_title(&with_fallback, &|_| None), "Barrel");

    let bare = lodestone_model::Text::translate("container.shulker_box", vec![]);
    assert_eq!(menu_title(&bare, &|_| None), "container.shulker_box");

    let named = lodestone_model::Text::literal("Bob's Loot");
    assert_eq!(menu_title(&named, &|_| None), "Bob's Loot");
}

fn survival() -> MenuContext {
    MenuContext {
        cursor_loaded: false,
        creative: false,
    }
}

fn loaded() -> MenuContext {
    MenuContext {
        cursor_loaded: true,
        creative: false,
    }
}

/// A plain player-inventory menu, for the many `press`/`release` tests
/// below that need *a* [`Menu`] to satisfy the signature but do not care
/// about its contents or its result slot. Tests that do care (the
/// `canTakeItemForPickAll` gate, the shift+double-click gather) build
/// their own.
fn blank_menu() -> Menu {
    Menu::player()
}

/// Centre of a slot's hit rect, in **physical** viewport pixels — the same
/// space [`hit_test`] takes, and `VIEW` (1280x720) is deliberately *not* the
/// identity-scale case: `calculate_gui_scale(AUTO, 1280, 720) == 3` (see
/// `config::tests::auto_scale_at_1280x720`), so this genuinely exercises the
/// scale-conversion round trip rather than being inert at scale 1.
/// `panel_origin`/`slot_layout` work in the *logical* canvas, so their
/// result is scaled back up to physical pixels before returning — the
/// inverse of what `hit_test` does to its incoming `x`/`y`.
fn slot_point(menu: &Menu, menu_index: usize) -> (f32, f32) {
    let layout = slot_layout(menu);
    let (px, py) = panel_origin(&layout, VIEW.0, VIEW.1);
    let rect = layout
        .slots
        .iter()
        .find(|r| r.menu_index == menu_index)
        .unwrap_or_else(|| panic!("menu index {menu_index} has no rect"));
    let scale =
        crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1)
            .max(1) as f32;
    (
        (px + rect.x + rect.w * 0.5) * scale,
        (py + rect.y + rect.h * 0.5) * scale,
    )
}

// ---------------------------------------------------------------------
// Layout: the transposition class of bug.
//
// These are the cheap checks that catch what is genuinely hard to see by
// eye: a plausible, fully populated inventory whose slots are all shifted
// by a constant. Every `SlotRect` carries a real menu index, so the gate is
// that hit-testing a rect's own centre returns that same index — round-trip,
// for every slot, in both layouts.
// ---------------------------------------------------------------------

#[test]
fn every_slot_rect_hit_tests_back_to_its_own_menu_index() {
    for menu in [Menu::player(), Menu::crafting(3, 3), Menu::generic(27)] {
        let layout = slot_layout(&menu);
        assert_eq!(
            layout.slots.len(),
            menu.slot_count(),
            "every menu slot must be laid out exactly once"
        );
        let mut seen = vec![false; menu.slot_count()];
        for rect in &layout.slots {
            assert!(
                !std::mem::replace(&mut seen[rect.menu_index], true),
                "menu index {} laid out twice",
                rect.menu_index
            );
            let (x, y) = slot_point(&menu, rect.menu_index);
            assert_eq!(
                hit_test(&menu, VIEW.0, VIEW.1, x, y),
                MenuHit::Slot(rect.menu_index),
                "hit test disagreed with the rect it came from"
            );
        }
        assert!(seen.into_iter().all(|s| s), "a menu slot was never drawn");
    }
}

/// The `MenuKind` trap, stated as an assertion rather than a comment: the
/// player screen's hotbar starts at 36 and it owns armour and an off-hand;
/// a crafting table is a `Generic { container_size: 10 }` whose hotbar
/// starts at **37** and which has neither. A single shared offset cannot
/// satisfy both.
#[test]
fn crafting_and_player_hotbars_are_not_at_the_same_index() {
    let player = Menu::player();
    assert_eq!(player.kind(), MenuKind::Player);
    assert_eq!(player.slot_count(), 46);

    let table = Menu::crafting(3, 3);
    assert_eq!(table.kind(), MenuKind::Generic { container_size: 10 });
    assert_eq!(table.slot_count(), 46);

    // Same slot count, different meaning at the same index. Menu index 36 is
    // the player screen's first hotbar cell and the crafting screen's *last*
    // main-storage cell; the crafting hotbar begins one later.
    let layout = slot_layout(&table);
    let hotbar_first = layout
        .slots
        .iter()
        .find(|r| r.menu_index == 37)
        .expect("crafting hotbar starts at 37");
    let main_last = layout
        .slots
        .iter()
        .find(|r| r.menu_index == 36)
        .expect("crafting main storage ends at 36");
    assert!(
        hotbar_first.y > main_last.y,
        "the crafting hotbar row must sit below main storage; got hotbar y={} main y={}",
        hotbar_first.y,
        main_last.y
    );
    // And the player screen has slots the crafting screen does not.
    assert!(slot_layout(&player).slots.iter().any(|r| r.menu_index == 45));
    assert_eq!(
        player.craft_layout().map(|c| (c.width, c.height)),
        Some((2, 2))
    );
    assert_eq!(
        table.craft_layout().map(|c| (c.width, c.height)),
        Some((3, 3))
    );
}

/// The crafting screen must not lay the result slot on top of a grid cell —
/// which is exactly what the plain 9-wide generic run would do with a
/// container size of 10.
#[test]
fn the_result_slot_is_not_inside_the_grid() {
    let table = Menu::crafting(3, 3);
    let (rx, ry) = slot_point(&table, 0);
    assert_eq!(hit_test(&table, VIEW.0, VIEW.1, rx, ry), MenuHit::Slot(0));
    for cell in 1..=9 {
        let (cx, cy) = slot_point(&table, cell);
        assert!(
            (cx - rx).abs() > 1.0 || (cy - ry).abs() > 1.0,
            "grid cell {cell} landed on top of the result slot"
        );
    }
}

// ---------------------------------------------------------------------
// The press/drag/release protocol.
// ---------------------------------------------------------------------

#[test]
fn an_empty_cursor_sends_on_press_and_nothing_on_release() {
    let menu = blank_menu();
    let mut input = MenuInput::new();
    let clicks = input.press(
        MenuHit::Slot(37),
        MenuButton::Left,
        false,
        survival(),
        false,
        &menu,
    );
    assert_eq!(clicks, vec![Click::left(37)]);
    // `skipNextRelease`: the release must not send a second packet.
    assert!(
        input
            .release(MenuHit::Slot(37), MenuButton::Left, false, survival(), &menu)
            .is_empty()
    );
}

#[test]
fn shift_press_is_a_quick_move() {
    let menu = blank_menu();
    let mut input = MenuInput::new();
    assert_eq!(
        input.press(
            MenuHit::Slot(0),
            MenuButton::Left,
            true,
            survival(),
            false,
            &menu
        ),
        vec![Click::shift(0)],
        "shift-clicking the result slot must be QUICK_MOVE — the repeat-craft gesture"
    );
}

/// The reason this is a state machine: with a loaded cursor the press sends
/// **nothing**, and the ordinary click is emitted by the release. A
/// press-to-`PICKUP` mapper passes every other test here and loses the drag.
#[test]
fn a_loaded_cursor_sends_the_click_on_release_not_on_press() {
    let menu = blank_menu();
    let mut input = MenuInput::new();
    assert!(
        input
            .press(MenuHit::Slot(1), MenuButton::Right, false, loaded(), false, &menu)
            .is_empty(),
        "vanilla only arms isQuickCrafting on press"
    );
    assert!(input.is_dragging());
    assert_eq!(
        input.release(MenuHit::Slot(1), MenuButton::Right, false, loaded(), &menu),
        vec![Click::right(1)],
        "no slot was painted, so it degrades to a plain place-one"
    );
    assert!(!input.is_dragging());
}

#[test]
fn painting_slots_emits_the_full_quick_craft_sequence() {
    // Two things changed here with issue #378 part 1, both because `dragged`
    // now reads the menu instead of recording blindly:
    //
    // * the cursor really has to hold something. This used to run against a
    //   menu whose cursor was empty while `loaded()` claimed otherwise — the
    //   two were free to disagree because nothing consulted the menu.
    // * the **menu** has to be one where cells 1/2/4/5 are all paintable.
    //   On `Menu::player()` (the old `blank_menu()`) slot 5 is an *armour*
    //   slot, whose `may_place` needs a `minecraft:equippable` component, so
    //   a plank genuinely cannot be painted there and vanilla would not
    //   paint it either. A crafting table's 1..=9 are all grid cells.
    let mut menu = Menu::crafting(3, 3);
    menu.set_carried(Some(ItemStack::new(
        "minecraft:oak_planks".parse().unwrap(),
        8,
    )));
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(1), MenuButton::Right, false, loaded(), false, &menu);
    for cell in [1usize, 2, 4, 5] {
        input.dragged(MenuHit::Slot(cell), &menu);
    }
    input.dragged(MenuHit::Slot(5), &menu); // a repeat must not be painted twice
    let clicks = input.release(MenuHit::Slot(5), MenuButton::Right, false, loaded(), &menu);
    assert_eq!(clicks.len(), 6, "start + 4 slots + end, got {clicks:?}");
    assert_eq!(clicks[0].slot, OUTSIDE_SLOT);
    assert_eq!(clicks[5].slot, OUTSIDE_SLOT);
    assert!(
        clicks
            .iter()
            .all(|c| c.input == ContainerInput::QuickCraft)
    );
    assert_eq!(
        clicks[1..5].iter().map(|c| c.slot).collect::<Vec<_>>(),
        vec![1, 2, 4, 5]
    );
    // Right-drag distributes one item per slot.
    for c in &clicks {
        assert_eq!(
            lodestone_game::click::quick_craft_type(c.button),
            drag_type::ONE
        );
    }
}

/// The whole point of the sequence: driven into a real menu it distributes
/// exactly as vanilla does, filling a 2×2 of the crafting grid one plank per
/// cell. Nothing here fills the result slot — that is the server's.
#[test]
fn the_emitted_sequence_fills_a_crafting_grid_one_per_cell() {
    let mut menu = Menu::crafting(3, 3);
    menu.set_carried(Some(ItemStack::new(
        "minecraft:oak_planks".parse().unwrap(),
        8,
    )));
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(1), MenuButton::Right, false, loaded(), false, &menu);
    for cell in [1usize, 2, 4, 5] {
        input.dragged(MenuHit::Slot(cell), &menu);
    }
    for click in input.release(MenuHit::Slot(5), MenuButton::Right, false, loaded(), &menu) {
        click.apply(&mut menu, lodestone_game::click::PlayerCtx::survival());
    }
    for cell in [1usize, 2, 4, 5] {
        assert_eq!(
            menu.slot_item(cell).map(ItemStack::count),
            Some(1),
            "cell {cell} must hold exactly one plank"
        );
    }
    assert_eq!(menu.carried().map(ItemStack::count), Some(4));
    assert_eq!(
        menu.slot_item(0),
        None,
        "the client must never put anything in the result slot"
    );
}

#[test]
fn a_click_inside_the_panel_but_off_a_slot_does_nothing() {
    let menu = blank_menu();
    let mut input = MenuInput::new();
    assert!(
        input
            .press(MenuHit::Panel, MenuButton::Left, false, survival(), false, &menu)
            .is_empty()
    );
    assert!(!input.is_dragging());
    assert!(
        input
            .release(MenuHit::Panel, MenuButton::Left, false, loaded(), &menu)
            .is_empty()
    );
}

#[test]
fn releasing_a_loaded_cursor_outside_drops_it() {
    let menu = blank_menu();
    let mut input = MenuInput::new();
    input.press(MenuHit::Outside, MenuButton::Left, false, loaded(), false, &menu);
    assert_eq!(
        input.release(MenuHit::Outside, MenuButton::Left, false, loaded(), &menu),
        vec![Click::drop_cursor()]
    );
}

#[test]
fn a_second_press_on_the_same_slot_gathers_on_release() {
    let menu = blank_menu();
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(9), MenuButton::Left, false, loaded(), false, &menu);
    input.release(MenuHit::Slot(9), MenuButton::Left, false, loaded(), &menu);
    input.press(MenuHit::Slot(9), MenuButton::Left, false, loaded(), true, &menu);
    assert_eq!(
        input.release(MenuHit::Slot(9), MenuButton::Left, false, loaded(), &menu),
        vec![Click::double(9)]
    );
}

#[test]
fn pick_block_only_clones_with_infinite_materials() {
    let menu = blank_menu();
    let creative = MenuContext {
        cursor_loaded: false,
        creative: true,
    };
    let mut input = MenuInput::new();
    assert_eq!(
        input.press(MenuHit::Slot(3), MenuButton::Pick, false, creative, false, &menu),
        vec![Click::clone_slot(3)]
    );
    let mut survival_input = MenuInput::new();
    assert!(
        survival_input
            .press(MenuHit::Slot(3), MenuButton::Pick, false, survival(), false, &menu)
            .is_empty(),
        "middle-click in survival is a hotbar rebind, not a container click"
    );
}

// ---------------------------------------------------------------------
// The keyboard verbs: `AbstractContainerScreen.keyPressed` (`:495-501`).
//
// These close an island rather than adding a branch — see
// `MenuInput::key_pressed`'s doc comment. Before it, `Click::drop_one` and
// `Click::drop_stack` had **no producer** anywhere in the tree, so
// `ContainerInput::Throw` could only reach `Menu::do_click` at
// `OUTSIDE_SLOT`, where its own `slotIndex >= 0` guard drops it.
// ---------------------------------------------------------------------

/// A player inventory with one stack in the main storage, for the keyboard
/// verbs — which are gated on the *slot* holding something, never on the
/// cursor.
fn menu_with_a_stack(slot: usize, count: i32) -> Menu {
    let mut menu = Menu::player();
    menu.set_slot_item(
        slot,
        Some(ItemStack::new("minecraft:stick".parse().unwrap(), count)),
    );
    menu
}

/// `Q` and `Ctrl+Q` differ **only** in the wire button number (`0` vs `1`,
/// `AbstractContainerScreen.java:499`'s `event.hasControlDown() ? 1 : 0`),
/// which is what `do_throw` reads to pick drop-one from drop-stack.
#[test]
fn the_drop_key_throws_from_the_hovered_slot_and_control_makes_it_a_stack() {
    let menu = menu_with_a_stack(9, 12);
    let input = MenuInput::new();
    assert_eq!(
        input.key_pressed(MenuHit::Slot(9), MenuKey::Drop { ctrl: false }, survival(), &menu),
        vec![Click::drop_one(9)],
    );
    assert_eq!(
        input.key_pressed(MenuHit::Slot(9), MenuKey::Drop { ctrl: true }, survival(), &menu),
        vec![Click::drop_stack(9)],
    );
    // The two really are distinct on the wire, not two names for one packet
    // — the whole point of threading `ctrl` through.
    assert_ne!(Click::drop_one(9).button, Click::drop_stack(9).button);
}

/// The gate is `hoveredSlot.hasItem()` — and *nothing else*. Two controls
/// here, because each one alone would pass against a wrong guard: an empty
/// slot must send nothing (so the guard exists at all), and a **loaded
/// cursor** must still send (so the guard is not `checkHotbarKeyPressed`'s
/// `getCarried().isEmpty()`, copied one method too far). Vanilla leaves the
/// carried check to `AbstractContainerMenu.java:513`, so suppressing it here
/// would withhold a packet the server expects.
#[test]
fn the_drop_key_needs_an_item_in_the_slot_but_not_an_empty_cursor() {
    let menu = menu_with_a_stack(9, 12);
    let input = MenuInput::new();
    assert!(
        input
            .key_pressed(MenuHit::Slot(10), MenuKey::Drop { ctrl: false }, survival(), &menu)
            .is_empty(),
        "slot 10 is empty, so `hoveredSlot.hasItem()` fails",
    );
    assert!(
        input
            .key_pressed(MenuHit::Panel, MenuKey::Drop { ctrl: false }, survival(), &menu)
            .is_empty(),
        "no hovered slot at all is vanilla's `hoveredSlot == null`",
    );
    assert_eq!(
        input.key_pressed(MenuHit::Slot(9), MenuKey::Drop { ctrl: false }, loaded(), &menu),
        vec![Click::drop_one(9)],
        "a loaded cursor does not suppress the packet — `doClick` drops it, we do not",
    );
}

/// Unlike middle-click, the pick-block **key** is not gated on infinite
/// materials at the screen layer: `keyPressed` (`:496-497`) sends CLONE
/// unconditionally and `doClick` (`AbstractContainerMenu.java:508`) is where
/// `hasInfiniteMaterials()` lives. The control is the mouse path in the same
/// state, which *is* gated — so this asserts a real difference between the
/// two entry points rather than restating one of them.
#[test]
fn the_pick_block_key_clones_even_in_survival_where_the_mouse_does_not() {
    let menu = menu_with_a_stack(9, 12);
    let input = MenuInput::new();
    assert_eq!(
        input.key_pressed(MenuHit::Slot(9), MenuKey::PickItem, survival(), &menu),
        vec![Click::clone_slot(9)],
    );
    let mut mouse = MenuInput::new();
    assert!(
        mouse
            .press(MenuHit::Slot(9), MenuButton::Pick, false, survival(), false, &menu)
            .is_empty(),
        "middle-click in survival is a hotbar rebind; the key is not — the \
         permission check is one layer down, in `doClick`'s CLONE arm",
    );
}

// ---------------------------------------------------------------------
// Gap (a): `canTakeItemForPickAll` — AbstractContainerScreen.java:387.
// ---------------------------------------------------------------------

/// Vanilla `AbstractContainerScreen.java:387` gates the whole double-click
/// gather branch on `menu.canTakeItemForPickAll(ItemStack.EMPTY, slot)`,
/// which every result-bearing menu overrides to exclude its own result
/// slot. So double-clicking a crafting result must send **nothing** — not
/// a desync fix (a real server honours the packet fine; `Menu::do_click`
/// has no such gate), just non-vanilla UX otherwise.
#[test]
fn double_clicking_the_crafting_result_slot_sends_nothing() {
    let menu = Menu::crafting(3, 3);
    let craft = menu.craft_layout().expect("a crafting table has a grid");
    let result = MenuHit::Slot(craft.result_slot);
    let mut input = MenuInput::new();
    input.press(result, MenuButton::Left, false, survival(), false, &menu);
    input.release(result, MenuButton::Left, false, survival(), &menu);
    input.press(result, MenuButton::Left, false, survival(), true, &menu);
    assert_eq!(
        input.release(result, MenuButton::Left, false, survival(), &menu),
        Vec::new(),
        "canTakeItemForPickAll excludes the result slot from double-click gather"
    );
}

/// Control for the test above, proving the detector actually fires rather
/// than every double-click silently sending nothing: the identical
/// press/release sequence on an ordinary slot of the same menu must still
/// gather.
#[test]
fn double_clicking_an_ordinary_slot_still_gathers() {
    let menu = Menu::crafting(3, 3);
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(10), MenuButton::Left, false, survival(), false, &menu);
    input.release(MenuHit::Slot(10), MenuButton::Left, false, survival(), &menu);
    input.press(MenuHit::Slot(10), MenuButton::Left, false, survival(), true, &menu);
    assert_eq!(
        input.release(MenuHit::Slot(10), MenuButton::Left, false, survival(), &menu),
        vec![Click::double(10)]
    );
}

// ---------------------------------------------------------------------
// Gap (b): shift+double-click "move all matching" —
// AbstractContainerScreen.java:388-398.
// ---------------------------------------------------------------------

/// The gather-by-shift branch sends one `QUICK_MOVE` per slot that shares
/// the double-clicked slot's **backing container**, holds an item, and
/// matches `last_quick_moved` — not a single `PICKUP_ALL`. Exercises the
/// exact set and order of emitted slots, plus two controls: a
/// wrong-item chest slot (must not appear) and a matching player-inventory
/// slot in a *different* backing container (must not appear either — the
/// `target.container == slot.container` restriction, not just an item
/// match).
#[test]
fn shift_double_click_gathers_only_matching_slots_in_the_same_backing_container() {
    let mut menu = Menu::generic(9);
    let diamond = |count: i32| ItemStack::new("minecraft:diamond".parse().unwrap(), count);
    // Chest slots (container 0): three matching diamonds and one
    // non-matching dirt stack, at varied counts to show the match is
    // item-identity, not size.
    menu.set_slot_item(0, Some(diamond(1)));
    menu.set_slot_item(1, Some(ItemStack::new("minecraft:dirt".parse().unwrap(), 64)));
    menu.set_slot_item(2, Some(diamond(3)));
    menu.set_slot_item(4, Some(diamond(5)));
    // Player main storage (container 1): a matching diamond stack that
    // must NOT be swept — it is a different backing container than the
    // chest, even though it lives in the same `Menu`.
    menu.set_slot_item(20, Some(diamond(1)));

    let mut input = MenuInput::new();
    // A first shift-click on chest slot 0 is what populates
    // `last_quick_moved` in real play; reproduce it before the
    // shift+double-click.
    input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), false, &menu);
    input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu);
    input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), true, &menu);
    let clicks = input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu);

    assert!(
        clicks.iter().all(|c| c.input == ContainerInput::QuickMove),
        "shift+double-click gathers via QUICK_MOVE, not PICKUP_ALL: {clicks:?}"
    );
    assert_eq!(
        clicks.iter().map(|c| c.slot).collect::<Vec<_>>(),
        vec![0, 2, 4],
        "must sweep exactly the matching chest slots, in ascending slot order, and not \
         the wrong-item slot 1 or the different-container slot 20"
    );
}

/// Control for the test above: `last_quick_moved` is captured off the
/// double-clicked slot's *own* contents at press time
/// (`AbstractContainerScreen.java:312`), so shift+double-clicking an
/// **empty** slot records vanilla's `ItemStack.EMPTY` — and
/// `!this.lastQuickMoved.isEmpty()` then suppresses the gather entirely,
/// sending nothing. This proves the emitted clicks in the test above come
/// from a real match against a captured stack, not from the double-click
/// alone.
#[test]
fn shift_double_click_on_an_empty_slot_sends_nothing() {
    let menu = Menu::generic(9); // slot 0 starts empty
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), false, &menu);
    input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu);
    input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), true, &menu);
    assert_eq!(
        input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu),
        Vec::new(),
        "an empty slot's captured stack is vanilla's ItemStack.EMPTY, which suppresses \
         the shift+double-click gather"
    );
}

// ---------------------------------------------------------------------
// Issue #378 part 1: taking from a crafting result onto a matching cursor.
//
// The machine's arm for this (`click.rs::do_pickup`'s "slot rejects
// placement but same item" branch, vanilla
// `AbstractContainerMenu.java:459-465`) was present, correct and tested.
// What was broken is *which packet the release sends*: an unfiltered paint
// set turned a click-with-a-jiggle on the result slot into a
// `QUICK_CRAFT` sequence the machine then dropped on the floor. These tests
// are at the shell layer for that reason — the click audit in
// `docs/container-clicks.md` covers `doClick` and structurally could not
// see this.
// ---------------------------------------------------------------------

/// A crafting table whose result slot holds `result_count` sticks and whose
/// cursor holds `carried_count` of the same, i.e. the state a player is in
/// halfway through emptying a stack of crafted sticks onto the cursor.
fn result_and_cursor(result_count: i32, carried_count: i32) -> Menu {
    let mut menu = Menu::crafting(3, 3);
    let stick = |n: i32| ItemStack::new("minecraft:stick".parse().unwrap(), n);
    let craft = menu.craft_layout().expect("a crafting table has a grid");
    menu.set_slot_item(craft.result_slot, Some(stick(result_count)));
    // A loaded grid, so `on_take` has something to charge and the take is a
    // real craft rather than a free pull.
    menu.set_slot_item(craft.first_input, Some(ItemStack::new(
        "minecraft:oak_planks".parse().unwrap(),
        8,
    )));
    menu.set_carried(Some(stick(carried_count)));
    menu
}

/// **The reproduction.** A drag that crossed the result slot must not paint
/// it, so the release falls through to the plain `PICKUP` vanilla sends —
/// which is the only click that reaches the cursor-merge arm.
///
/// Hand-derived from `AbstractContainerScreen.java:554-561`: the result
/// slot's `mayPlace` is `false` (`ResultSlot.java:24-27`), so
/// `shouldAddSlotToQuickCraft` is `false`, `quickCraftSlots` stays empty, and
/// `mouseReleased`'s `isQuickCrafting && !quickCraftSlots.isEmpty()` test
/// fails into the `else if (!carried.isEmpty())` branch at `:420-430`.
#[test]
fn dragging_across_a_crafting_result_sends_a_pickup_not_a_dead_drag() {
    let menu = result_and_cursor(1, 4);
    let craft = menu.craft_layout().expect("a crafting table has a grid");
    let result = MenuHit::Slot(craft.result_slot);
    let mut input = MenuInput::new();
    input.press(result, MenuButton::Left, false, loaded(), false, &menu);
    input.dragged(result, &menu);
    assert_eq!(
        input.drag_paint().map(|(_, slots)| slots.len()),
        Some(0),
        "a result slot fails `mayPlace`, so vanilla never paints it"
    );
    assert_eq!(
        input.release(result, MenuButton::Left, false, loaded(), &menu),
        vec![Click::left(craft.result_slot)],
        "an unpaintable slot must degrade to the plain PICKUP, which is what \
         carries the cursor merge"
    );
}

/// …and driven into the real machine, that `PICKUP` **merges**: the cursor
/// grows by the result's count and the grid is charged one item per cell.
///
/// Expected value hand-derived from `AbstractContainerMenu.java:459-465`,
/// which on `!slot.mayPlace(carried) && isSameItemSameComponents` does
/// `tryRemove(clicked.getCount(), carried.getMaxStackSize() - carried.getCount())`
/// then `carried.grow(taken)` — 4 + 1 = 5 — plus `ResultSlot.onTake`
/// consuming one plank from the loaded cell (8 → 7).
#[test]
fn the_resulting_pickup_merges_the_result_onto_the_matching_cursor() {
    let mut menu = result_and_cursor(1, 4);
    let craft = menu.craft_layout().expect("a crafting table has a grid");
    let result = MenuHit::Slot(craft.result_slot);
    let mut input = MenuInput::new();
    input.press(result, MenuButton::Left, false, loaded(), false, &menu);
    input.dragged(result, &menu);
    let clicks = input.release(result, MenuButton::Left, false, loaded(), &menu);
    for click in clicks {
        click.apply(&mut menu, lodestone_game::click::PlayerCtx::survival());
    }
    assert_eq!(
        menu.carried().map(ItemStack::count),
        Some(5),
        "the cursor must grow by the result's count"
    );
    assert_eq!(menu.slot_item(craft.result_slot), None, "the result was taken");
    assert_eq!(
        menu.slot_item(craft.first_input).map(ItemStack::count),
        Some(7),
        "`ResultSlot.onTake` charges the grid one item per occupied cell"
    );
}

/// Control: the filter must be *selective*, not a blanket refusal to paint.
/// The same press/jiggle/release on an ordinary empty grid cell — which does
/// pass `mayPlace` — still paints and still emits the drag sequence. Without
/// this, `dragged` returning early unconditionally would satisfy both tests
/// above and delete the entire paint gesture.
#[test]
fn control_dragging_across_a_placeable_cell_still_paints_it() {
    let menu = result_and_cursor(1, 4);
    let craft = menu.craft_layout().expect("a crafting table has a grid");
    // The second grid cell: empty, `CraftingInput`, so `mayPlace` is true.
    let cell = MenuHit::Slot(craft.first_input + 1);
    let mut input = MenuInput::new();
    input.press(cell, MenuButton::Left, false, loaded(), false, &menu);
    input.dragged(cell, &menu);
    assert_eq!(
        input.drag_paint().map(|(_, slots)| slots.to_vec()),
        Some(vec![craft.first_input + 1])
    );
    let clicks = input.release(cell, MenuButton::Left, false, loaded(), &menu);
    assert!(
        clicks.iter().all(|c| c.input == ContainerInput::QuickCraft),
        "a paintable cell must still produce the drag sequence: {clicks:?}"
    );
}

/// The other two arms of `shouldAddSlotToQuickCraft`, each with the same
/// slot flipped to the passing case so neither is measuring a slot that was
/// unpaintable for an unrelated reason.
#[test]
fn the_paint_gate_also_honours_item_identity_and_the_cursor_count() {
    let stick = |n: i32| ItemStack::new("minecraft:stick".parse().unwrap(), n);
    let dirt = |n: i32| ItemStack::new("minecraft:dirt".parse().unwrap(), n);

    // `canItemQuickReplace`: an occupied cell holding a *different* item is
    // never painted (`AbstractContainerMenu.java:726-731`).
    let mut mismatched = Menu::generic(9);
    mismatched.set_slot_item(0, Some(dirt(1)));
    mismatched.set_carried(Some(stick(8)));
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(0), MenuButton::Left, false, loaded(), false, &mismatched);
    input.dragged(MenuHit::Slot(0), &mismatched);
    assert_eq!(input.drag_paint().map(|(_, s)| s.len()), Some(0));

    // Control for it: the same slot holding the *same* item is painted.
    let mut matched = Menu::generic(9);
    matched.set_slot_item(0, Some(stick(1)));
    matched.set_carried(Some(stick(8)));
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(0), MenuButton::Left, false, loaded(), false, &matched);
    input.dragged(MenuHit::Slot(0), &matched);
    assert_eq!(input.drag_paint().map(|(_, s)| s.to_vec()), Some(vec![0]));

    // `carried.getCount() > quickCraftSlots.size()`: a cursor of two cannot
    // paint a third cell, so the paint stops at two.
    let mut small = Menu::generic(9);
    small.set_carried(Some(stick(2)));
    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(0), MenuButton::Left, false, loaded(), false, &small);
    for cell in [0usize, 1, 2, 3] {
        input.dragged(MenuHit::Slot(cell), &small);
    }
    assert_eq!(
        input.drag_paint().map(|(_, s)| s.to_vec()),
        Some(vec![0, 1]),
        "vanilla stops painting once the painted count reaches the cursor's"
    );
}

// ---------------------------------------------------------------------
// Issue #378 part 2: the live drag preview.
//
// The arithmetic itself is proved in `lodestone-game`'s
// `tests/drag_preview_agreement.rs`, which compares the plan against the
// real release path. What is proved *here* is the precondition that makes
// that comparison mean anything on screen: the set the screen previews and
// the set the machine distributes over must be the same set.
// ---------------------------------------------------------------------

/// `Menu::quick_craft_plan`'s divisor is `painted.len()`, and the screen and
/// the machine each keep their own paint set — the screen's grown by
/// `dragged`, the machine's by the `ADD` packets `release` emits. If they
/// ever differed in size, the previewed split would divide by a different
/// number than the distribution and every cell would show the wrong count.
///
/// They cannot differ, because both call `Menu::can_drag_place_at`. This
/// asserts it end to end over a paint that crosses cells of three kinds —
/// takeable, refused for item mismatch, refused for `may_place` — rather
/// than leaving it as an argument in a doc comment.
#[test]
fn the_screens_paint_set_and_the_machines_stay_identical() {
    let stick = |n: i32| ItemStack::new("minecraft:stick".parse().unwrap(), n);
    let mut menu = Menu::crafting(3, 3);
    menu.set_carried(Some(stick(8)));
    // Grid cell 2 holds a different item; the result slot (0) refuses
    // placement outright. Cells 1, 3 and 4 are takeable.
    menu.set_slot_item(2, Some(ItemStack::new("minecraft:dirt".parse().unwrap(), 1)));
    menu.set_slot_item(0, Some(stick(1)));

    let mut input = MenuInput::new();
    input.press(MenuHit::Slot(1), MenuButton::Left, false, loaded(), false, &menu);
    for cell in [1usize, 2, 0, 3, 4] {
        input.dragged(MenuHit::Slot(cell), &menu);
    }
    let (kind, screen_set) = input.drag_paint().expect("a drag is armed");
    let screen_set = screen_set.to_vec();
    assert_eq!(
        screen_set,
        vec![1, 3, 4],
        "the mismatched cell and the result slot must both be refused, or the \
         comparison below is between two unfiltered sets and proves nothing"
    );

    // Now drive the emitted sequence into the machine and read back the set
    // it accumulated from the ADD packets.
    let clicks = input.release(MenuHit::Slot(4), MenuButton::Left, false, loaded(), &menu);
    let mut machine = menu.clone();
    let ctx = lodestone_game::click::PlayerCtx::survival();
    let mut machine_set: Vec<usize> = Vec::new();
    for click in &clicks {
        // Snapshot the machine's own set just before END consumes it.
        if click.input == ContainerInput::QuickCraft
            && lodestone_game::click::quick_craft_header(click.button) == drag_header::END
        {
            machine_set = machine.quick_craft_slots().to_vec();
        }
        click.apply(&mut machine, ctx);
    }
    assert_eq!(
        machine_set, screen_set,
        "the screen previews against its own paint set and the machine distributes \
         over its own; `Menu::can_drag_place_at` is the single predicate that keeps \
         them equal"
    );

    // …and the counts the screen would draw are the counts that landed.
    let source = stick(8);
    let plan = menu.quick_craft_plan(&screen_set, kind, &source);
    for cell in &plan {
        assert_eq!(
            Some(cell.count),
            machine.slot_item(cell.menu_index).map(ItemStack::count),
            "cell {} previewed {} ",
            cell.menu_index,
            cell.count
        );
    }
}

/// The preview must reach the *geometry*, not merely be computable — the
/// island defect this project has hit repeatedly. A frame with a drag armed
/// has to emit more colour vertices than the identical frame without one
/// (the 50%-white wash plus the provisional stacks' counts), and the pixel
/// gate in `tests/container_drag_preview_pixels.rs` then proves *where*.
#[test]
fn a_painted_drag_changes_the_geometry_it_would_not_otherwise() {
    let stick = |n: i32| ItemStack::new("minecraft:stick".parse().unwrap(), n);
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stick(9)));
    let painted = [0usize, 1, 2];

    let plain = ContainerFrame::new(Some(&menu), "Chest");
    let dragging = ContainerFrame::new(Some(&menu), "Chest").with_drag(Some((
        drag_type::EVEN,
        &painted,
    )));
    let without = ContainerGeometry::build(&plain, 1280, 720);
    let with = ContainerGeometry::build(&dragging, 1280, 720);
    assert!(
        with.chrome_vertex_count > without.chrome_vertex_count,
        "the painted-cell wash belongs to the chrome run, under the icon it backs: \
         {} vs {}",
        with.chrome_vertex_count,
        without.chrome_vertex_count
    );
    assert!(
        with.vertex_count() > without.vertex_count(),
        "and the provisional counts land past the chrome split"
    );

    // Control: an *empty* paint set is `None`, so nothing changes. Without
    // this, a `with_drag` that unconditionally emitted something would
    // satisfy the assertions above.
    let empty: [usize; 0] = [];
    let armed_but_unpainted = ContainerFrame::new(Some(&menu), "Chest")
        .with_drag(Some((drag_type::EVEN, &empty)));
    assert_eq!(
        ContainerGeometry::build(&armed_but_unpainted, 1280, 720).vertex_count(),
        without.vertex_count(),
        "a drag that has painted nothing draws nothing"
    );
}

/// Vanilla's `extractSlot` **returns before drawing anything** when exactly
/// one cell is painted (`AbstractContainerScreen.java:203-205`), so that cell
/// blanks — including whatever it already held. Easy to miss, and visible:
/// the drag is about to be re-dispatched as an ordinary click.
#[test]
fn a_one_cell_paint_blanks_that_cell_rather_than_previewing_it() {
    let stick = |n: i32| ItemStack::new("minecraft:stick".parse().unwrap(), n);
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stick(9)));
    menu.set_slot_item(0, Some(stick(5)));
    let one = [0usize];

    let plain = ContainerFrame::new(Some(&menu), "Chest");
    let single = ContainerFrame::new(Some(&menu), "Chest")
        .with_drag(Some((drag_type::EVEN, &one)));
    let without = ContainerGeometry::build(&plain, 1280, 720);
    let with = ContainerGeometry::build(&single, 1280, 720);
    assert!(
        with.vertex_count() < without.vertex_count(),
        "the occupied cell's swatch and count must disappear, not gain a wash: \
         {} vs {}",
        with.vertex_count(),
        without.vertex_count()
    );

    // Control: with a *second* cell painted the same slot draws again, so the
    // blanking above is the `size() == 1` rule and not "a painted cell never
    // draws".
    let two = [0usize, 1];
    let pair = ContainerFrame::new(Some(&menu), "Chest")
        .with_drag(Some((drag_type::EVEN, &two)));
    assert!(
        ContainerGeometry::build(&pair, 1280, 720).vertex_count() > with.vertex_count(),
        "a two-cell paint previews both cells"
    );
}

// ---------------------------------------------------------------------
// Container background art (issue #51) and the hotbar dim (issue #61's
// leftover). GPU-free: `ContainerBackground` is deliberately a pure
// producer/consumer split (see its own doc comment) so this needs no
// device. The GPU pixel proof lives in
// `tests/container_background_pixels.rs`.
// ---------------------------------------------------------------------

#[test]
fn background_kind_mirrors_slot_layouts_own_dispatch() {
    assert_eq!(background_kind(&Menu::player()), BackgroundKind::Inventory);
    assert_eq!(
        background_kind(&Menu::crafting(3, 3)),
        BackgroundKind::Crafting
    );
    // A single chest: one row.
    assert_eq!(
        background_kind(&Menu::generic(9)),
        BackgroundKind::Generic { rows: 1 }
    );
    // A double chest: six rows, `generic_54`'s own native row count.
    assert_eq!(
        background_kind(&Menu::generic(54)),
        BackgroundKind::Generic { rows: 6 }
    );
    // A hopper-sized (5-slot) container still rounds up to a whole row
    // rather than drawing a fractional one.
    assert_eq!(
        background_kind(&Menu::generic(5)),
        BackgroundKind::Generic { rows: 1 }
    );
}

/// The four `special_layout` menus (#253-#255) get their own real
/// background, not the plain generic-chest fallback a same-sized
/// container without one draws — the control at the end proves it is the
/// `special_layout`, not the size, that changed the result.
#[test]
fn background_kind_recognises_the_four_special_layout_menus() {
    assert_eq!(
        background_kind(&Menu::item_combiner(3, 2, SpecialLayout::Anvil)),
        BackgroundKind::Anvil
    );
    assert_eq!(
        background_kind(&Menu::item_combiner(3, 2, SpecialLayout::Grindstone)),
        BackgroundKind::Grindstone
    );
    assert_eq!(
        background_kind(&Menu::item_combiner(4, 3, SpecialLayout::Smithing)),
        BackgroundKind::Smithing
    );
    assert_eq!(
        background_kind(&Menu::enchanting_table()),
        BackgroundKind::Enchantment
    );
    // Control: a plain 3-slot generic container (same size as the anvil
    // and grindstone, no `special_layout`) still draws the ordinary chest
    // background — proving the dispatch above keyed on `special_layout`,
    // not merely on `container_size == 3`.
    assert_eq!(
        background_kind(&Menu::generic(3)),
        BackgroundKind::Generic { rows: 1 }
    );
}

/// The six more `special_layout` menus issue #28 added — same proof as
/// [`background_kind_recognises_the_four_special_layout_menus`], plus a
/// control that the two size-9 special layouts (dispenser/dropper vs. a
/// plain 9-slot chest) are told apart by `special_layout`, not size.
#[test]
fn background_kind_recognises_the_six_menus_issue_28_added() {
    assert_eq!(
        background_kind(&Menu::furnace(SpecialLayout::Furnace)),
        BackgroundKind::Furnace
    );
    assert_eq!(
        background_kind(&Menu::furnace(SpecialLayout::BlastFurnace)),
        BackgroundKind::BlastFurnace
    );
    assert_eq!(
        background_kind(&Menu::furnace(SpecialLayout::Smoker)),
        BackgroundKind::Smoker
    );
    assert_eq!(
        background_kind(&Menu::brewing_stand()),
        BackgroundKind::Brewing
    );
    assert_eq!(background_kind(&Menu::loom()), BackgroundKind::Loom);
    assert_eq!(
        background_kind(&Menu::stonecutter()),
        BackgroundKind::Stonecutter
    );
    assert_eq!(
        background_kind(&Menu::cartography_table()),
        BackgroundKind::Cartography
    );
    assert_eq!(
        background_kind(&Menu::dispenser()),
        BackgroundKind::Dispenser
    );
    // Control: a plain 9-slot generic container (same size as the
    // dispenser/dropper, no `special_layout`) still draws one row of the
    // ordinary chest background.
    assert_eq!(
        background_kind(&Menu::generic(9)),
        BackgroundKind::Generic { rows: 1 }
    );
}

/// The hopper — not one of issue #28's own named containers, but found
/// while documenting the family it was almost mistaken for already
/// covering (see `SpecialLayout::Hopper`'s doc comment). The control
/// proves it is keyed on `special_layout`, not merely on size: a
/// same-sized (`5`) generic container is also what the brewing stand
/// happens to be, and the two must not collide.
#[test]
fn background_kind_recognises_the_hopper_as_a_shorter_panel_not_a_generic_row() {
    assert_eq!(background_kind(&Menu::hopper()), BackgroundKind::Hopper);
    assert_eq!(
        background_kind(&Menu::brewing_stand()),
        BackgroundKind::Brewing,
        "a hopper and a brewing stand are both container_size 5 — \
         special_layout, not size, must tell them apart"
    );
    assert_eq!(
        background_kind(&Menu::generic(5)),
        BackgroundKind::Generic { rows: 1 }
    );
}

/// The hopper's own panel is genuinely shorter than every other special
/// layout — `176×133`, not `166` (`HopperScreen.java:15`) — because its
/// `addStandardInventorySlots` call uses `main_y = 51`, not `84`
/// (`HopperMenu.java:27`). The wrong hypothesis this rules out: reusing
/// the other special layouts' fixed `84.0` would still produce a
/// plausible-looking layout, just one 33px taller than the real panel,
/// with the hopper's own five slots overlapping the top of the main
/// inventory rows.
#[test]
fn hopper_slots_land_at_vanillas_real_positions_on_a_shorter_panel() {
    let hopper = Menu::hopper();
    let layout = slot_layout(&hopper);
    assert_eq!(layout.width, 176.0);
    assert_eq!(
        layout.height, 133.0,
        "expected vanilla's real 133 — the wrong hypothesis (reusing \
         main_y = 84 like every other special layout) would give 166"
    );
    let at = |i: usize| {
        layout
            .slots
            .iter()
            .find(|s| s.menu_index == i)
            .map(|s| (s.x, s.y))
    };
    for i in 0..5 {
        assert_eq!(at(i), Some((44.0 + i as f32 * SLOT, 20.0)), "slot {i}");
    }
    // Main storage starts at y=51, not y=84 — the first main-storage
    // slot (menu index 5) is the clearest witness.
    assert_eq!(at(5), Some((8.0, 51.0)));
}

/// The anvil's three slots land at vanilla's real positions
/// (`AnvilMenu.java:42-45`), not [`generic_layout`]'s plain left-to-right
/// row — and the panel is the real `176x166`, not whatever height a
/// 3-slot generic container's single row would compute.
#[test]
fn anvil_slots_land_at_vanillas_real_positions_not_a_generic_row() {
    let anvil = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
    let layout = slot_layout(&anvil);
    assert_eq!(layout.width, 176.0);
    assert_eq!(layout.height, 166.0);
    let at = |i: usize| {
        layout
            .slots
            .iter()
            .find(|s| s.menu_index == i)
            .map(|s| (s.x, s.y))
    };
    assert_eq!(at(0), Some((27.0, 47.0)));
    assert_eq!(at(1), Some((76.0, 47.0)));
    assert_eq!(at(2), Some((134.0, 47.0)));
    // The player's main storage starts at vanilla's fixed `(8, 84)`, same
    // as every other `addStandardInventorySlots(inventory, 8, 84)` screen.
    assert_eq!(at(3), Some((8.0, 84.0)));

    // Control: an *ordinary* 3-slot generic container (no `special_layout`)
    // draws slot 0 at the plain grid's `(8, 18)` — proving the anvil
    // positions above come from `special_layout`, not from `container_size
    // == 3` alone.
    let plain = slot_layout(&Menu::generic(3));
    assert_eq!(
        plain.slots.iter().find(|s| s.menu_index == 0).map(|s| (s.x, s.y)),
        Some((8.0, 18.0)),
        "control: a plain 3-slot container must use the generic grid, not the anvil layout"
    );
}

/// Counts vertices in `verts` (the `[x_ndc, y_ndc, r, g, b, a]` colour
/// stream) whose colour is within `tol` of `want` — the same
/// approximate-match approach `held_item_name_pixels.rs` uses for real
/// glyph ink, adapted to this crate's hermetic (font-less) debug glyphs.
fn ink_near(verts: &[f32], want: [f32; 3], tol: f32) -> usize {
    verts
        .chunks_exact(FLOATS_PER_VERTEX)
        .filter(|v| {
            (v[2] - want[0]).abs() < tol
                && (v[3] - want[1]).abs() < tol
                && (v[4] - want[2]).abs() < tol
        })
        .count()
}

/// The anvil's XP cost (`docs/container-cost-screens.md`'s "What is not
/// yet wired" gap): reaches real ink, in the colour vanilla's own
/// `AnvilMenu.mayPickup`/`AnvilScreen.extractLabels` predicts — and, per
/// CLAUDE.md's *magnitude* evidence rule, this checks the actual colour
/// drawn, not merely "something changed".
#[test]
fn anvil_cost_reaches_pixels_and_colours_by_affordability() {
    let anvil = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
    let base = ContainerFrame::new(Some(&anvil), "Repair & Name");

    // Controls: no `cost_data` at all (every pre-existing caller) must
    // draw no green or red ink whatsoever.
    let none_geo = ContainerGeometry::build(&base, VIEW.0, VIEW.1);
    assert_eq!(
        ink_near(&none_geo.verts, [128.0 / 255.0, 1.0, 32.0 / 255.0], 0.05)
            + ink_near(&none_geo.verts, [1.0, 96.0 / 255.0, 96.0 / 255.0], 0.05),
        0,
        "control: no cost_data must draw neither cost colour"
    );

    // Control: cost > 0 but the result slot is empty — `AnvilScreen.java:102-103`
    // draws nothing in this case either.
    let data = [(0, 17)];
    let frame_no_result = base.with_cost_context(&data, false, 20);
    let empty_geo = ContainerGeometry::build(&frame_no_result, VIEW.0, VIEW.1);
    assert_eq!(
        ink_near(&empty_geo.verts, [128.0 / 255.0, 1.0, 32.0 / 255.0], 0.05)
            + ink_near(&empty_geo.verts, [1.0, 96.0 / 255.0, 96.0 / 255.0], 0.05),
        0,
        "control: cost > 0 with an empty result slot must draw nothing \
         (AnvilScreen.java:102-103)"
    );

    // Subject: a result item present and enough levels — green ink.
    let mut with_result = anvil.clone();
    with_result.set_slot_item(
        2,
        Some(lodestone_game::item::ItemStack::new(
            "minecraft:diamond_pickaxe".parse().unwrap(),
            1,
        )),
    );
    let frame_afford = ContainerFrame::new(Some(&with_result), "Repair & Name")
        .with_cost_context(&data, false, 20);
    let afford_geo = ContainerGeometry::build(&frame_afford, VIEW.0, VIEW.1);
    assert!(
        ink_near(&afford_geo.verts, [128.0 / 255.0, 1.0, 32.0 / 255.0], 0.05) > 0,
        "an affordable cost (xp_level 20 >= cost 17) must draw in vanilla's \
         green (AnvilMenu.mayPickup true) — the pixel this feature draws"
    );

    // Subject: same result item, but too few levels — red ink, no green.
    let frame_unafford = ContainerFrame::new(Some(&with_result), "Repair & Name")
        .with_cost_context(&data, false, 3);
    let unafford_geo = ContainerGeometry::build(&frame_unafford, VIEW.0, VIEW.1);
    assert!(
        ink_near(&unafford_geo.verts, [1.0, 96.0 / 255.0, 96.0 / 255.0], 0.05) > 0,
        "an unaffordable cost (xp_level 3 < cost 17) must draw in vanilla's \
         red (AnvilMenu.mayPickup false)"
    );
    assert_eq!(
        ink_near(&unafford_geo.verts, [128.0 / 255.0, 1.0, 32.0 / 255.0], 0.05),
        0,
        "an unaffordable cost must not also draw green"
    );

    // Subject: cost >= 40 without infinite materials draws "Too
    // Expensive!" in red regardless of the result slot or level.
    let expensive = [(0, 40)];
    let frame_expensive =
        ContainerFrame::new(Some(&anvil), "Repair & Name").with_cost_context(&expensive, false, 99);
    let expensive_geo = ContainerGeometry::build(&frame_expensive, VIEW.0, VIEW.1);
    assert!(
        ink_near(&expensive_geo.verts, [1.0, 96.0 / 255.0, 96.0 / 255.0], 0.05) > 0,
        "cost >= 40 without infinite materials must draw red \"Too Expensive!\" \
         (AnvilScreen.java:99-101) even with an empty result slot and a high level"
    );

    // Control: the same >= 40 cost, but with infinite materials, must
    // fall through to the ordinary (green, since the result slot still
    // has an item) path rather than "Too Expensive!".
    let frame_creative = ContainerFrame::new(Some(&with_result), "Repair & Name")
        .with_cost_context(&expensive, true, 99);
    let creative_geo = ContainerGeometry::build(&frame_creative, VIEW.0, VIEW.1);
    assert!(
        ink_near(&creative_geo.verts, [128.0 / 255.0, 1.0, 32.0 / 255.0], 0.05) > 0,
        "infinite materials must bypass the >= 40 \"Too Expensive!\" branch \
         (AnvilScreen.java:99: `!hasInfiniteMaterials()`)"
    );
}

/// The enchanting table's three per-row level costs: reach pixels, in
/// vanilla's own affordable/disabled colours
/// (`EnchantmentScreen.java:110-129`). Deliberately does not check the
/// enchantment-name text — this build has no Standard Galactic Alphabet
/// glyphs, see [`draw_enchanting_costs`]'s doc.
#[test]
fn enchanting_costs_reach_pixels_and_colour_by_affordability() {
    let enchanting = Menu::enchanting_table();

    // Control: no cost_data draws nothing.
    let base = ContainerFrame::new(Some(&enchanting), "Enchant");
    let none_geo = ContainerGeometry::build(&base, VIEW.0, VIEW.1);
    assert_eq!(
        ink_near(&none_geo.verts, [128.0 / 255.0, 1.0, 32.0 / 255.0], 0.05)
            + ink_near(&none_geo.verts, [64.0 / 255.0, 127.0 / 255.0, 16.0 / 255.0], 0.05),
        0,
        "control: no cost_data must draw neither enchanting cost colour"
    );

    // One lapis in the lapis-only slot (menu index 1) — enough for row 0
    // (`goldCount >= i + 1`) but not row 1 or row 2.
    let mut with_lapis = enchanting.clone();
    with_lapis.set_slot_item(
        1,
        Some(lodestone_game::item::ItemStack::new(
            "minecraft:lapis_lazuli".parse().unwrap(),
            1,
        )),
    );
    // Row 0: cost 1, affordable (1 lapis, level 5 >= 1) -> green.
    // Row 1: cost 5, unaffordable (needs 2 lapis, has 1) -> disabled green.
    // Row 2: cost 0 -> vanilla draws nothing for this row at all.
    let data = [(0, 1), (1, 5), (2, 0)];
    let frame = ContainerFrame::new(Some(&with_lapis), "Enchant").with_cost_context(&data, false, 5);
    let geo = ContainerGeometry::build(&frame, VIEW.0, VIEW.1);
    assert!(
        ink_near(&geo.verts, [128.0 / 255.0, 1.0, 32.0 / 255.0], 0.05) > 0,
        "row 0 (affordable) must draw green"
    );
    assert!(
        ink_near(&geo.verts, [64.0 / 255.0, 127.0 / 255.0, 16.0 / 255.0], 0.05) > 0,
        "row 1 (not enough lapis) must draw the disabled half-brightness green \
         (EnchantmentScreen.java:115's col = -12550384)"
    );

    // Control: row 2's cost is 0, so removing it must not change the
    // geometry at all — proving cost == 0 draws nothing, not just "less".
    let data_row2_dropped = [(0, 1), (1, 5)];
    let frame_dropped = ContainerFrame::new(Some(&with_lapis), "Enchant")
        .with_cost_context(&data_row2_dropped, false, 5);
    let geo_dropped = ContainerGeometry::build(&frame_dropped, VIEW.0, VIEW.1);
    assert_eq!(
        geo.verts, geo_dropped.verts,
        "control: a row already at cost 0 must be identical whether or not \
         property 2 is even present in cost_data"
    );
}

/// The other three `special_layout` screens, spot-checked against the
/// same `SmithingMenu.java`/`GrindstoneMenu.java`/`EnchantmentMenu.java`
/// constants [`special_layout_positions`] cites.
#[test]
fn the_other_three_special_layouts_match_their_menu_constructors() {
    let smithing = slot_layout(&Menu::item_combiner(4, 3, SpecialLayout::Smithing));
    let at = |layout: &SlotLayout, i: usize| {
        layout
            .slots
            .iter()
            .find(|s| s.menu_index == i)
            .map(|s| (s.x, s.y))
    };
    assert_eq!(at(&smithing, 0), Some((8.0, 48.0)));
    assert_eq!(at(&smithing, 1), Some((26.0, 48.0)));
    assert_eq!(at(&smithing, 2), Some((44.0, 48.0)));
    assert_eq!(at(&smithing, 3), Some((98.0, 48.0)));

    let grindstone = slot_layout(&Menu::item_combiner(3, 2, SpecialLayout::Grindstone));
    assert_eq!(at(&grindstone, 0), Some((49.0, 19.0)));
    assert_eq!(at(&grindstone, 1), Some((49.0, 40.0)));
    assert_eq!(at(&grindstone, 2), Some((129.0, 34.0)));

    let enchanting = slot_layout(&Menu::enchanting_table());
    assert_eq!(at(&enchanting, 0), Some((15.0, 47.0)));
    assert_eq!(at(&enchanting, 1), Some((35.0, 47.0)));
}

/// The six menus issue #28 added, checked against the same vanilla slot
/// constructor arguments cited in `special_layout_positions`'s own doc
/// table (`AbstractFurnaceMenu.java:63-65`, `BrewingStandMenu.java:48-52`,
/// `LoomMenu.java:64-82`, `StonecutterMenu.java:54-55`,
/// `CartographyTableMenu.java:49-61`, `DispenserMenu.java:26,30-37`).
///
/// The wrong hypothesis this rules out: before `special_layout_positions`
/// grew these arms, every one of them fell through to `generic_layout`'s
/// plain left-to-right row — e.g. the furnace's fuel slot would have
/// landed at `(26.0, 18.0)` (index 1 in a 9-wide row starting at `(8,
/// 18)`) instead of `(56.0, 53.0)`, and the panel would size itself to
/// `114 + 1*18 = 132` tall instead of the real `166`. Both wrong values
/// are far enough from the real ones that a transposed-but-plausible
/// layout (the exact trap issue #28 itself warns about) cannot pass this
/// by accident.
#[test]
fn the_six_menus_issue_28_added_match_their_menu_constructors() {
    let at = |layout: &SlotLayout, i: usize| {
        layout
            .slots
            .iter()
            .find(|s| s.menu_index == i)
            .map(|s| (s.x, s.y))
    };

    for layout_kind in [
        SpecialLayout::Furnace,
        SpecialLayout::BlastFurnace,
        SpecialLayout::Smoker,
    ] {
        let furnace = slot_layout(&Menu::furnace(layout_kind));
        assert_eq!(furnace.width, 176.0);
        assert_eq!(furnace.height, 166.0);
        assert_eq!(at(&furnace, 0), Some((56.0, 17.0)), "{layout_kind:?} ingredient");
        assert_eq!(at(&furnace, 1), Some((56.0, 53.0)), "{layout_kind:?} fuel");
        assert_eq!(at(&furnace, 2), Some((116.0, 35.0)), "{layout_kind:?} result");
        // Wrong hypothesis: a plain 9-wide generic row starting at (8,18)
        // would put the fuel slot (index 1) at (26.0, 18.0) — 30px away
        // in x and 35px in y from the real position.
        assert_ne!(at(&furnace, 1), Some((26.0, 18.0)));
    }

    let brewing = slot_layout(&Menu::brewing_stand());
    assert_eq!(brewing.width, 176.0);
    assert_eq!(brewing.height, 166.0);
    assert_eq!(at(&brewing, 0), Some((56.0, 51.0)));
    assert_eq!(at(&brewing, 1), Some((79.0, 58.0)));
    assert_eq!(at(&brewing, 2), Some((102.0, 51.0)));
    assert_eq!(at(&brewing, 3), Some((79.0, 17.0)));
    assert_eq!(at(&brewing, 4), Some((17.0, 17.0)));

    let loom = slot_layout(&Menu::loom());
    assert_eq!(at(&loom, 0), Some((13.0, 26.0)));
    assert_eq!(at(&loom, 1), Some((33.0, 26.0)));
    assert_eq!(at(&loom, 2), Some((23.0, 45.0)));
    assert_eq!(at(&loom, 3), Some((143.0, 57.0)));

    let stonecutter = slot_layout(&Menu::stonecutter());
    assert_eq!(at(&stonecutter, 0), Some((20.0, 33.0)));
    assert_eq!(at(&stonecutter, 1), Some((143.0, 33.0)));

    let cartography = slot_layout(&Menu::cartography_table());
    assert_eq!(at(&cartography, 0), Some((15.0, 15.0)));
    assert_eq!(at(&cartography, 1), Some((15.0, 52.0)));
    assert_eq!(at(&cartography, 2), Some((145.0, 39.0)));

    // The dispenser/dropper's whole point: a 3x3 square, not a 9-wide row.
    let dispenser = slot_layout(&Menu::dispenser());
    for i in 0..9 {
        let want = (62.0 + (i % 3) as f32 * SLOT, 17.0 + (i / 3) as f32 * SLOT);
        assert_eq!(at(&dispenser, i), Some(want), "dispenser slot {i}");
    }
    // Wrong hypothesis: `generic_layout(9)` puts every one of these nine
    // slots on a single row at y=18.0. Slot 3 (the grid's second row,
    // first column) is the clearest divergence: a flat row would put it
    // at (8 + 3*18, 18) = (62.0, 18.0), one row above the real (62.0,
    // 35.0).
    assert_ne!(at(&dispenser, 3), Some((62.0, 18.0)));
}

/// [`slot_layout`] is what both drawing (`build_inner`, above) and
/// [`hit_test`] consult — proven here by calling `hit_test` itself rather
/// than re-deriving the coordinates, so a future refactor that moves the
/// override elsewhere cannot silently desync the two. A click at the
/// anvil's second input slot (`76,47` local, panel origin `(0,0)` at this
/// view size) must resolve to menu index 1, which is nowhere near where
/// a plain 3-slot generic grid would put it.
#[test]
fn hit_test_agrees_with_the_anvil_layout_it_was_never_told_about() {
    let anvil = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
    let layout = slot_layout(&anvil);
    let (px, py) = panel_origin_with_scale(&layout, 1, VIEW.0, VIEW.1);
    // Centre of slot 1's cell, in physical pixels at `gui_scale = 1`.
    let x = px + 76.0 + SLOT * 0.5;
    let y = py + 47.0 + SLOT * 0.5;
    let hit = hit_test_with_scale(&anvil, 1, VIEW.0, VIEW.1, x, y);
    assert_eq!(
        hit,
        MenuHit::Slot(1),
        "a click on the anvil's second input slot must hit menu index 1 — \
         if this fails, hit_test and the draw path have desynced"
    );
}

/// Same proof as [`hit_test_agrees_with_the_anvil_layout_it_was_never_told_about`],
/// for the dispenser/dropper's 3×3 grid — the one issue #28 shape most
/// likely to desync, because it is the only new layout that reorders
/// slots into a **square** rather than merely repositioning a handful of
/// them. Slot 4 is the grid's centre cell (row 1, col 1): a click there
/// must not resolve to slot 3 or 5 (its row neighbours) or to whatever
/// index a flat 9-wide row would have put at that pixel.
#[test]
fn hit_test_agrees_with_the_dispenser_grid_it_was_never_told_about() {
    let dispenser = Menu::dispenser();
    let layout = slot_layout(&dispenser);
    let (px, py) = panel_origin_with_scale(&layout, 1, VIEW.0, VIEW.1);
    let x = px + 62.0 + SLOT + SLOT * 0.5;
    let y = py + 17.0 + SLOT + SLOT * 0.5;
    let hit = hit_test_with_scale(&dispenser, 1, VIEW.0, VIEW.1, x, y);
    assert_eq!(
        hit,
        MenuHit::Slot(4),
        "a click on the dispenser's centre cell must hit menu index 4 — \
         if this fails, hit_test and the draw path have desynced"
    );
}

/// A minimal in-memory pack with distinctly-sized solid-colour stand-ins
/// for the three real sheets, so `ContainerBackground::build` succeeds
/// hermetically — no `client.jar` needed for this test.
fn synthetic_background() -> ContainerBackground {
    use lodestone_assets::{MemorySource, ResourceManager, ResourceSource};

    fn solid_png(w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::new();
        let mut encoder = png::Encoder::new(&mut data, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| [10, 20, 30, 255]).collect();
        writer.write_image_data(&pixels).expect("png data");
        drop(writer);
        data
    }

    let mut src = MemorySource::default();
    for name in [
        "generic_54",
        "crafting_table",
        "inventory",
        "anvil",
        "grindstone",
        "smithing",
        "enchanting_table",
        "furnace",
        "blast_furnace",
        "smoker",
        "brewing_stand",
        "loom",
        "stonecutter",
        "cartography_table",
        "dispenser",
        "hopper",
        // The merchant screen (issue #245's UI half) — real `villager.png` is
        // `512x256`, but the stand-in only needs to exist; `whole_panel_sized`
        // grabs a `276x166` sub-rect regardless of the sheet's own dimensions,
        // the same way it already does for every sheet in this list.
        "villager",
        // The creative screen's three sheets (issue #158) — loaded by
        // `ContainerBackground::build` alongside the sixteen above, so a
        // missing stand-in fails every test in this module rather than just
        // the creative ones.
        "creative_inventory/tab_items",
        "creative_inventory/tab_item_search",
        "creative_inventory/tab_inventory",
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/container/{name}.png"),
            solid_png(256, 256),
        );
    }
    // The Advancements screen's loose art (issue #167): the `256 x 256` window
    // sheet and the five `16 x 16` tab backgrounds, keyed by the same ids
    // `ContainerBackground::build` loads.
    src.insert(
        "assets/minecraft/textures/gui/advancements/window.png".to_string(),
        solid_png(256, 256),
    );
    for id in super::background::ADVANCEMENT_TILE_IDS {
        let path = id.strip_prefix("minecraft:").unwrap_or(id);
        src.insert(
            format!("assets/minecraft/textures/{path}.png"),
            solid_png(16, 16),
        );
    }
    // Every id in `GUI_SPRITES`, at its real vanilla size so a test asserting
    // a `dst` rect is asserting the blit and not the stand-in: the highlight
    // pair is natively 24x24 and the five placeholders are 16x16. Built from
    // the same const the loader walks, so a sprite added there cannot be
    // forgotten here — `ContainerBackground::build` would fail instead.
    for id in super::all_gui_sprites() {
        // The furnace/brewing progress sprites (issue #28) are sized at
        // their own real vanilla dimensions rather than folded into the
        // `CELL` default, so a test reading back a sub-region through
        // `sprite_subregion_quad` is exercising the same native size
        // vanilla's own sprite is, not a same-sized stand-in.
        let (w, h) = if id == SLOT_HIGHLIGHT_BACK || id == SLOT_HIGHLIGHT_FRONT {
            (HIGHLIGHT as u32, HIGHLIGHT as u32)
        } else if id == FURNACE_LIT_PROGRESS
            || id == BLAST_FURNACE_LIT_PROGRESS
            || id == SMOKER_LIT_PROGRESS
        {
            (14, 14)
        } else if id == FURNACE_BURN_PROGRESS
            || id == BLAST_FURNACE_BURN_PROGRESS
            || id == SMOKER_BURN_PROGRESS
        {
            (24, 16)
        } else if id == BREWING_FUEL_LENGTH {
            (18, 4)
        } else if id == BREWING_BREW_PROGRESS {
            (9, 28)
        } else if id == BREWING_BUBBLES {
            (12, 29)
        } else if id.starts_with("container/creative_inventory/tab_") {
            // `26 x 32` (`CreativeModeInventoryScreen.java:827`).
            (26, 32)
        } else if id.starts_with("container/creative_inventory/scroller") {
            // `12 x 15` (`:753`).
            (12, 15)
        } else if id.starts_with("advancements/tab_") {
            // `28 x 32` for `AdvancementTabType.ABOVE` (`AdvancementTabType.java:19-20`).
            (28, 32)
        } else if id.ends_with("_frame_obtained") || id.ends_with("_frame_unobtained") {
            // `26 x 26` (`AdvancementWidget.java:164`).
            (26, 26)
        } else if id == "advancements/title_box" {
            // `200 x 26` — `BOX_WIDTH` by `HEIGHT` (`AdvancementWidget.java:26-28`).
            (200, 26)
        } else {
            (CELL as u32, CELL as u32)
        };
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_png(w, h),
        );
    }
    let manager = ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>]);
    ContainerBackground::build(&manager).expect("synthetic background builds")
}

// ---------------------------------------------------------------------
// Title anchors for the screens `label_layout` does not model.
//
// These live here rather than in `tests/container_labels.rs` (where the
// investigation's spec put them) because `menu_type_title_anchor` is pure
// arithmetic over a `SlotLayout`: it needs neither a GPU nor the jar, so
// gating them behind `#[ignore]` alongside that file's pixel gates would
// make them run never instead of always. The font-dependent one degrades to
// a skip on a jar-less run, which is the only reason it could not be
// unconditional.
// ---------------------------------------------------------------------

/// `AnvilScreen.java:30` fixes `titleLabelX = 60` — a literal typed from the
/// decompile, so the expected value originates entirely outside this crate
/// and needs no font.
#[test]
fn an_anvil_titles_at_vanillas_fixed_sixty_not_the_usual_eight() {
    let menu = Menu::generic(3);
    let layout = slot_layout(&menu);
    assert_eq!(
        menu_type_title_anchor(
            Some(&"minecraft:anvil".parse().unwrap()),
            &layout,
            "Repair & Name",
            None,
        ),
        Some([60.0, 6.0]),
    );
    // The three decrement-style screens, resolved to absolutes. Each is
    // `titleLabelY -= n` off an inherited 6 in vanilla, so a wrong *base*
    // would move all three together — which is why all three are asserted
    // rather than one standing in for the family.
    for (path, want) in [
        ("loom", [8.0, 4.0]),
        ("stonecutter", [8.0, 5.0]),
        ("cartography_table", [8.0, 4.0]),
    ] {
        let key: lodestone_model::ResourceKey =
            format!("minecraft:{path}").parse().unwrap();
        assert_eq!(
            menu_type_title_anchor(Some(&key), &layout, "T", None),
            Some(want),
            "{path}"
        );
    }
}

/// The centred family, `(imageWidth - font.width(title)) / 2` floored
/// (`AbstractFurnaceScreen.java:39`). Reusing this crate's own
/// `VanillaFont::width` is not circular: the glyph metrics are validated by
/// the font's own gates, so what this pins is the **centring arithmetic**.
///
/// The magnitude matters, not just the sign — a centred anchor and
/// `label_layout`'s `8.0` are both "a number", so the test additionally
/// asserts the two *differ*, which is the thing a no-op implementation would
/// fail.
#[test]
fn the_furnace_family_centres_its_title_and_a_hopper_is_left_alone() {
    let Some(font) = VanillaFont::shared() else {
        return; // jar-less: nothing to measure against
    };
    let menu = Menu::generic(3);
    let layout = slot_layout(&menu);
    for path in [
        "furnace",
        "blast_furnace",
        "smoker",
        "brewing_stand",
        "generic_3x3",
        "crafter_3x3",
    ] {
        let key: lodestone_model::ResourceKey =
            format!("minecraft:{path}").parse().unwrap();
        let title = "Blast Furnace";
        let want = ((layout.width - font.width(title, 1.0)) / 2.0).floor();
        assert_eq!(
            menu_type_title_anchor(Some(&key), &layout, title, Some(&font)),
            Some([want, 6.0]),
            "{path} must centre"
        );
        assert_ne!(
            want, 8.0,
            "if the centred anchor happened to equal label_layout's own 8.0, \
             this whole test would pass against a no-op override"
        );
    }

    // -- controls -------------------------------------------------------
    // A type vanilla does not override must fall through, or this function
    // would be claiming every screen rather than the nine that moved.
    for path in ["hopper", "grindstone", "shulker_box", "generic_9x3", "crafting"] {
        let key: lodestone_model::ResourceKey =
            format!("minecraft:{path}").parse().unwrap();
        assert_eq!(
            menu_type_title_anchor(Some(&key), &layout, "T", Some(&font)),
            None,
            "{path} already matches label_layout's (8,6) in vanilla"
        );
    }
    assert_eq!(
        menu_type_title_anchor(None, &layout, "T", Some(&font)),
        None,
        "no menu_type at all is the player inventory screen and every \
         pre-existing caller"
    );
    // A non-vanilla namespace must not be matched on `path` alone.
    let modded: lodestone_model::ResourceKey = "mymod:furnace".parse().unwrap();
    assert_eq!(
        menu_type_title_anchor(Some(&modded), &layout, "T", Some(&font)),
        None,
    );
}

/// The override reaches the **draw**, not just the helper — otherwise
/// `menu_type_title_anchor` is an island. Measured through
/// `ContainerGeometry`: attaching a furnace `menu_type` must move the title
/// ink, and attaching a hopper's must not.
#[test]
fn the_menu_type_anchor_reaches_build_inner_and_moves_the_title() {
    let menu = Menu::generic(3);
    let title = "Blast Furnace";
    let geo = |menu_type: Option<&lodestone_model::ResourceKey>| {
        let frame = ContainerFrame::new(Some(&menu), title).with_menu_type(menu_type);
        ContainerGeometry::build(&frame, VIEW.0, VIEW.1).verts
    };
    let furnace: lodestone_model::ResourceKey = "minecraft:furnace".parse().unwrap();
    let hopper: lodestone_model::ResourceKey = "minecraft:hopper".parse().unwrap();
    let plain = geo(None);
    assert_ne!(
        geo(Some(&furnace)),
        plain,
        "a furnace menu_type must change the geometry — if this passes, \
         `menu_type_title_anchor` is computing an anchor nothing consumes"
    );
    assert_eq!(
        geo(Some(&hopper)),
        plain,
        "control: a hopper has no override, so the geometry must be identical \
         — otherwise the test above would pass for any menu_type at all"
    );
}

// ---------------------------------------------------------------------
// Issue #376: the hover highlight and the empty-slot placeholders.
//
// Both were reported from play ("hovering draws no highlight", "the armour
// and off-hand slots show no icons"). Neither was a missing verb and neither
// was an asset gap — `tests/container_slot_sprites.rs` had already measured
// the sprites present in the GUI atlas, and `lodestone-game` already
// declared `Slot::no_item_icon` per slot. The gap was that nothing in this
// module ever asked for a quad.
// ---------------------------------------------------------------------

/// Decode the background stream back into pixel-space `dst` rects, one per
/// quad in emission order — the exact inverse of
/// `item_icon::push_sprite_quad`'s `to_ndc`.
///
/// Asserting on decoded rects rather than on `ContainerBackground::
/// sprite_quad`'s return value is deliberate: `sprite_quad` is the thing
/// under test's *helper*, so a test built on it would agree with a wrong
/// inset. These come out of `bg_verts`, i.e. out of what the GPU is handed.
fn bg_rects(geo: &ContainerGeometry, gui_scale: u32) -> Vec<[f32; 4]> {
    let (vw, vh) = crate::menu::render::logical_canvas(gui_scale, VIEW.0, VIEW.1);
    geo.bg_verts
        .chunks(BG_FLOATS_PER_VERTEX * 6)
        .map(|q| {
            let px = |i: usize| (q[i * BG_FLOATS_PER_VERTEX] + 1.0) * vw * 0.5;
            let py = |i: usize| (1.0 - q[i * BG_FLOATS_PER_VERTEX + 1]) * vh * 0.5;
            // Vertex 0 is the quad's top-left, vertex 2 its bottom-right.
            let (x0, y0, x1, y1) = (px(0), py(0), px(2), py(2));
            [x0, y0, x1 - x0, y1 - y0]
        })
        .collect()
}

/// Panel-local top-left of a slot's 16x16 cell, in the **logical** canvas —
/// the space `bg_rects` decodes into.
fn slot_origin(menu: &Menu, menu_index: usize) -> (f32, f32) {
    let layout = slot_layout(menu);
    let (px, py) = panel_origin(&layout, VIEW.0, VIEW.1);
    let rect = layout
        .slots
        .iter()
        .find(|r| r.menu_index == menu_index)
        .expect("the slot has a rect");
    (px + rect.x, py + rect.y)
}

fn geo_with_background(menu: &Menu, cursor: Option<[f32; 2]>) -> ContainerGeometry {
    let bg = synthetic_background();
    let frame = ContainerFrame::new(Some(menu), "Title").with_cursor(cursor);
    ContainerGeometry::build_inner(
        &frame,
        VIEW.0,
        VIEW.1,
        crate::config::AUTO_GUI_SCALE,
        &IconAssets {
            items: None,
            models: None,
        },
        None,
        Some(&bg),
    )
}

/// As [`geo_with_background`], with `container_set_data` properties
/// attached — the feed the furnace/brewing progress bars (issue #28)
/// read through `frame.cost_data`.
fn geo_with_background_and_data(menu: &Menu, data: &[(i32, i32)]) -> ContainerGeometry {
    let bg = synthetic_background();
    let frame = ContainerFrame::new(Some(menu), "Title").with_cost_context(data, false, 0);
    ContainerGeometry::build_inner(
        &frame,
        VIEW.0,
        VIEW.1,
        crate::config::AUTO_GUI_SCALE,
        &IconAssets {
            items: None,
            models: None,
        },
        None,
        Some(&bg),
    )
}

/// The furnace family's lit-flame and burn-progress bars
/// (`AbstractFurnaceScreen.java:53-72`), driven entirely by
/// `frame.cost_data` — the same `container_set_data` feed the
/// anvil/enchanting cost lines already read, so this needs no new
/// wiring to reach `app.rs`. `data[0]` litTime `100`, `data[1]`
/// litDuration `200` gives `litProgress = 0.5`, so
/// `litProgressHeight = ceil(0.5 * 13) + 1 = 8`; `data[2]` cookingProgress
/// `12`, `data[3]` cookingTotalTime `24` gives `burnProgress = 0.5`, so
/// `burnProgressWidth = ceil(0.5 * 24) = 12`.
#[test]
fn furnace_burn_and_lit_bars_draw_from_container_data() {
    let menu = Menu::furnace(SpecialLayout::Furnace);
    let geo =
        geo_with_background_and_data(&menu, &[(0, 100), (1, 200), (2, 12), (3, 24)]);
    let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
    let layout = slot_layout(&menu);
    let (px, py) = panel_origin(&layout, VIEW.0, VIEW.1);

    let has = |want: [f32; 4]| {
        rects
            .iter()
            .any(|r| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
    };
    assert!(
        has([px + 56.0, py + 42.0, 14.0, 8.0]),
        "expected the lit-flame bar at (56, 42, 14, 8) local; background \
         quads were {rects:?}"
    );
    assert!(
        has([px + 79.0, py + 34.0, 12.0, 16.0]),
        "expected the burn-progress bar at (79, 34, 12, 16) local; \
         background quads were {rects:?}"
    );
}

/// Control for the above: with no `container_set_data` at all (the
/// honest all-zero state — an unlit, empty furnace, not a bug), neither
/// bar draws. Both `isLit()` (`data[0] > 0`) and `getBurnProgress`'s
/// `total != 0 && current != 0` guard are false against an all-zero
/// default, so this is also the every-existing-caller path (nothing
/// regressed for anvil/enchanting/chest screens, which never populated
/// these four properties anyway).
#[test]
fn control_an_unlit_furnace_with_no_container_data_draws_neither_bar() {
    let menu = Menu::furnace(SpecialLayout::Furnace);
    let geo = geo_with_background_and_data(&menu, &[]);
    let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
    let layout = slot_layout(&menu);
    let (px, py) = panel_origin(&layout, VIEW.0, VIEW.1);
    let has = |want: [f32; 4]| {
        rects
            .iter()
            .any(|r| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
    };
    assert!(!has([px + 56.0, py + 42.0, 14.0, 8.0]));
    assert!(!has([px + 79.0, py + 34.0, 12.0, 16.0]));
}

/// The brewing stand's three progress widgets
/// (`BrewingStandScreen.java:34-51`). `data[1]` fuel `15` gives
/// `fuelLength = (18*15+19)/20 = 14`; `data[0]` brewingTicks `100` gives
/// `brewLength = floor(28*(1 - 100/400)) = 21` and
/// `bubbleLength = BUBBLELENGTHS[(100/2) % 7] = BUBBLELENGTHS[1] = 24`.
#[test]
fn brewing_stand_fuel_brew_and_bubble_bars_draw_from_container_data() {
    let menu = Menu::brewing_stand();
    let geo = geo_with_background_and_data(&menu, &[(0, 100), (1, 15)]);
    let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
    let layout = slot_layout(&menu);
    let (px, py) = panel_origin(&layout, VIEW.0, VIEW.1);
    let has = |want: [f32; 4]| {
        rects
            .iter()
            .any(|r| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
    };
    assert!(
        has([px + 60.0, py + 44.0, 14.0, 4.0]),
        "expected the fuel-length bar at (60, 44, 14, 4) local; \
         background quads were {rects:?}"
    );
    assert!(
        has([px + 97.0, py + 16.0, 9.0, 21.0]),
        "expected the brew-progress bar at (97, 16, 9, 21) local; \
         background quads were {rects:?}"
    );
    assert!(
        has([px + 63.0, py + 19.0, 12.0, 24.0]),
        "expected the bubbles bar at (63, 19, 12, 24) local; background \
         quads were {rects:?}"
    );
}

/// Control for the above: an empty, unlit brewing stand (no
/// `container_set_data`) draws none of the three bars.
#[test]
fn control_an_idle_brewing_stand_with_no_container_data_draws_no_bars() {
    let menu = Menu::brewing_stand();
    let geo = geo_with_background_and_data(&menu, &[]);
    let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
    let layout = slot_layout(&menu);
    let (px, py) = panel_origin(&layout, VIEW.0, VIEW.1);
    let has = |want: [f32; 4]| {
        rects
            .iter()
            .any(|r| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
    };
    assert!(!has([px + 60.0, py + 44.0, 14.0, 4.0]));
    assert!(!has([px + 97.0, py + 16.0, 9.0, 21.0]));
    assert!(!has([px + 63.0, py + 19.0, 12.0, 24.0]));
}

/// `AbstractContainerScreen.java:155`/`:161` —
/// `blitSprite(SLOT_HIGHLIGHT_{BACK,FRONT}, slot.x - 4, slot.y - 4, 24, 24)`.
/// Both sprites, at the same rect, on opposite sides of the marker.
///
/// The *two-sided* part is what makes this worth a test rather than an
/// eyeball: a single highlight drawn under the item looks almost right, and
/// is what a naive "append it with the panel art" implementation produces.
#[test]
fn hovering_a_slot_blits_both_highlight_sprites_at_vanillas_own_offsets() {
    let menu = Menu::player();
    let (cx, cy) = slot_point(&menu, 9);
    let geo = geo_with_background(&menu, Some([cx, cy]));
    let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
    let (sx, sy) = slot_origin(&menu, 9);
    let want = [sx - 4.0, sy - 4.0, 24.0, 24.0];

    let hits: Vec<usize> = rects
        .iter()
        .enumerate()
        .filter(|(_, r)| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        2,
        "expected the back and front highlight at {want:?}; background quads were {rects:?}"
    );
    // The split: the first is under the items, the second over them.
    assert!(
        hits[0] < geo.bg_slot_vertex_count / 6,
        "the back sprite must fall inside the under-items range"
    );
    assert!(
        hits[1] >= geo.bg_slot_vertex_count / 6,
        "the front sprite must fall past the marker, or it draws beneath the \
         item it exists to frame"
    );
}

/// The control for the above: with no pointer over a slot, neither sprite is
/// emitted and the whole stream is under-items. Two arms, because a
/// highlight keyed on "a background is attached" rather than on the hover
/// would pass the positive test.
#[test]
fn nothing_hovered_blits_no_highlight_at_all() {
    let menu = Menu::player();
    let hovered = geo_with_background(&menu, Some(slot_point(&menu, 9)).map(|(a, b)| [a, b]));
    let none = geo_with_background(&menu, None);
    // Far outside the panel: a cursor that exists but hits nothing.
    let outside = geo_with_background(&menu, Some([0.0, 0.0]));

    let n = |g: &ContainerGeometry| bg_rects(g, crate::config::AUTO_GUI_SCALE).len();
    assert_eq!(
        n(&none) + 2,
        n(&hovered),
        "hovering adds exactly the two highlight quads and nothing else"
    );
    assert_eq!(n(&outside), n(&none), "a cursor over no slot is not a hover");
    for g in [&none, &outside] {
        assert_eq!(
            g.bg_slot_vertex_count,
            g.bg_verts.len() / BG_FLOATS_PER_VERTEX,
            "with no front sprite the marker must cover the whole stream, so \
             the renderer's second range is empty rather than bogus"
        );
    }
}

/// Issue #398's proof requirement: "a base class with one subclass is not a
/// base class." The two tests above only ever exercised `Menu::player()`;
/// this is the identical order proof on a **second** real screen — a chest —
/// through the same `build_inner` path. There is no per-screen branch that
/// could have gotten the ordering right on one screen and wrong on the
/// other, and this is what proves it rather than assumes it.
#[test]
fn a_chest_hovers_the_same_two_part_highlight_in_the_same_order() {
    let menu = Menu::generic(27);
    let (cx, cy) = slot_point(&menu, 0);
    let geo = geo_with_background(&menu, Some([cx, cy]));
    let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
    let (sx, sy) = slot_origin(&menu, 0);
    let want = [sx - 4.0, sy - 4.0, 24.0, 24.0];

    let hits: Vec<usize> = rects
        .iter()
        .enumerate()
        .filter(|(_, r)| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        2,
        "expected the back and front highlight at {want:?}; background quads were {rects:?}"
    );
    assert!(
        hits[0] < geo.bg_slot_vertex_count / 6,
        "a chest's back sprite must fall inside the under-items range too"
    );
    assert!(
        hits[1] >= geo.bg_slot_vertex_count / 6,
        "a chest's front sprite must fall past the marker — this is the same \
         `build_inner` path a player inventory screen uses, so a chest getting \
         the order wrong would mean the ordering had been per-screen after all"
    );
}

/// The control for the test above, mirroring
/// `nothing_hovered_blits_no_highlight_at_all` on the same second screen.
#[test]
fn nothing_hovered_in_a_chest_blits_no_highlight_at_all() {
    let menu = Menu::generic(27);
    let hovered = geo_with_background(&menu, Some(slot_point(&menu, 0)).map(|(a, b)| [a, b]));
    let none = geo_with_background(&menu, None);
    // Far outside the panel: a cursor that exists but hits nothing.
    let outside = geo_with_background(&menu, Some([0.0, 0.0]));

    let n = |g: &ContainerGeometry| bg_rects(g, crate::config::AUTO_GUI_SCALE).len();
    assert_eq!(
        n(&none) + 2,
        n(&hovered),
        "hovering a chest slot adds exactly the two highlight quads and nothing else"
    );
    assert_eq!(
        n(&outside),
        n(&none),
        "a cursor over no slot is not a hover, on a chest either"
    );
}

/// `extractSlot`'s `if (itemStack.isEmpty() && slot.isActive())` arm
/// (`:224-230`), blitting `slot.getNoItemIcon()` at the cell origin, 16x16.
///
/// The ids come off `Slot::no_item_icon`, so this asserts *five* placeholders
/// in a player inventory — the four armour slots and the off-hand.
#[test]
fn the_armour_and_offhand_slots_blit_their_placeholder_icons() {
    let menu = Menu::player();
    let geo = geo_with_background(&menu, None);
    let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
    for slot in [5, 6, 7, 8, 45] {
        let (sx, sy) = slot_origin(&menu, slot);
        let want = [sx, sy, 16.0, 16.0];
        assert!(
            rects
                .iter()
                .any(|r| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01)),
            "slot {slot} declares an empty-slot sprite but nothing blitted it \
             at {want:?}; background quads were {rects:?}"
        );
    }
    // One panel blit plus exactly five placeholders — nothing else, so the
    // crafting result (0) and the 2x2 grid (1..=4) stay bare.
    assert_eq!(rects.len(), 6, "quads were {rects:?}");
}

/// Three controls for the placeholders, each of which alone would let a
/// wrong implementation pass:
///
/// * a **filled** armour slot draws none (the `isEmpty()` half of the gate);
/// * a chest draws none (so this is not keyed on "menu slots 5..=8", which
///   would paint a helmet into the sixth slot of every chest — the exact
///   trap `lodestone-game`'s `no_item_icons` suite names);
/// * a jar-less build draws no background quads at all (the fallback path).
#[test]
fn control_placeholders_are_keyed_on_the_slot_declaring_one_and_being_empty() {
    let mut filled = Menu::player();
    filled.set_slot_item(
        5,
        Some(ItemStack::new("minecraft:iron_helmet".parse().unwrap(), 1)),
    );
    let n = |g: &ContainerGeometry| bg_rects(g, crate::config::AUTO_GUI_SCALE).len();
    assert_eq!(
        n(&geo_with_background(&filled, None)),
        n(&geo_with_background(&Menu::player(), None)) - 1,
        "a filled armour slot must not draw its placeholder under the item"
    );

    for menu in [Menu::generic(27), Menu::crafting(3, 3)] {
        let geo = geo_with_background(&menu, None);
        let rects = bg_rects(&geo, crate::config::AUTO_GUI_SCALE);
        assert!(
            rects.iter().all(|r| r[2] > 16.0),
            "a {:?} declares no empty-slot sprite, so every background quad \
             must be panel art (wider than a 16px cell); got {rects:?}",
            menu.kind()
        );
    }

    // The fallback path: no background attached, so no sprite exists to ask
    // for and the stream is empty — which is also why the two features above
    // are invisible on a jar-less run, by design.
    let bare = ContainerGeometry::build(
        &ContainerFrame::new(Some(&Menu::player()), "Title")
            .with_cursor(Some([100.0, 100.0])),
        VIEW.0,
        VIEW.1,
    );
    assert!(bare.bg_verts.is_empty());
    assert_eq!(bare.bg_slot_vertex_count, 0);
}

#[test]
fn a_single_chest_blits_vanillas_two_part_split_at_the_right_offsets() {
    let bg = synthetic_background();
    let menu = Menu::generic(27); // three rows
    let quads = bg
        .quads(&menu, 10.0, 20.0)
        .expect("every id used by `synthetic_background` is present");
    assert_eq!(quads.len(), 2, "the chest background is vanilla's two blits");
    // Top piece: `ContainerScreen.java:25` — height `rows*18+17`, at the
    // panel's own origin.
    assert_eq!(quads[0].dst, [10.0, 20.0, 176.0, 3.0 * 18.0 + 17.0]);
    // Bottom piece: `:26` — 96 tall, placed immediately below the top one,
    // sampling the sheet's fixed `v=126` row regardless of row count.
    assert_eq!(quads[1].dst, [10.0, 20.0 + (3.0 * 18.0 + 17.0), 176.0, 96.0]);
    assert!(
        quads[1].uv_min[1] > quads[0].uv_max[1],
        "the bottom piece samples further down the sheet (v=126) than the \
         top piece's own bottom edge (v={:.3}) — it must not be sampling \
         the same rows twice",
        quads[0].uv_max[1]
    );
}

#[test]
fn a_double_chest_draws_a_taller_top_piece_than_a_single_one() {
    let bg = synthetic_background();
    let single = bg
        .quads(&Menu::generic(27), 0.0, 0.0)
        .expect("present");
    let double = bg
        .quads(&Menu::generic(54), 0.0, 0.0)
        .expect("present");
    assert_eq!(single[0].dst[3], 3.0 * 18.0 + 17.0);
    assert_eq!(double[0].dst[3], 6.0 * 18.0 + 17.0);
    assert!(
        double[0].dst[3] > single[0].dst[3],
        "a double chest's top blit must be taller than a single chest's"
    );
}

#[test]
fn inventory_and_crafting_each_blit_one_whole_panel() {
    let bg = synthetic_background();
    let inventory = bg
        .quads(&Menu::player(), 4.0, 5.0)
        .expect("present");
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].dst, [4.0, 5.0, 176.0, 166.0]);

    let crafting = bg
        .quads(&Menu::crafting(3, 3), 4.0, 5.0)
        .expect("present");
    assert_eq!(crafting.len(), 1);
    assert_eq!(crafting[0].dst, [4.0, 5.0, 176.0, 166.0]);

    // -- negative control ---------------------------------------------
    // The two must not sample the same sheet: `inventory.png` and
    // `crafting_table.png` are different files, so their UVs must land on
    // different placed regions of the atlas even though both request the
    // identical `(0,0,176,166)` local rect.
    assert_ne!(
        inventory[0].uv_min, crafting[0].uv_min,
        "inventory and crafting table must not sample the same atlas region"
    );
}

#[test]
fn build_inner_without_a_background_falls_back_to_the_flat_fill_and_still_dims() {
    // No background attached: `build`/`build_with_icons` (used by every
    // existing test and gate in this file) must keep drawing something —
    // this is the jar-less path and the pixel gate's negative control.
    let menu = Menu::player();
    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let geo = ContainerGeometry::build(&frame, VIEW.0, VIEW.1);
    assert!(
        geo.dim_vertex_count > 0,
        "the full-canvas dim must draw even with no background attached — \
         it is independent of the panel art"
    );
    assert!(
        geo.bg_verts.is_empty(),
        "with no `ContainerBackground` attached, nothing should land on the \
         background-texture stream"
    );
    assert!(
        geo.chrome_vertex_count > geo.dim_vertex_count,
        "the flat-fill fallback panel must still draw after the dim when \
         there is no real background"
    );
}

// -- Recipe-book panel (issue #163) ----------------------------------

fn id(name: &str) -> lodestone_model::Identifier {
    name.parse().expect("valid identifier")
}

/// Predicts every rect of the recipe-book panel at the standard
/// `VIEW` (`1280x720`, gui scale `3`, per this module's own established
/// convention). The crafting-table's main panel origin
/// (`panel_origin`) at this view is `mx ≈ 125.333`, `my == 37.0`
/// (`176x166` centred in a `426.667x240` logical canvas — see
/// `panel_origin_with_scale`'s own arithmetic).
///
/// # The expected x changed, and the old one was the bug
///
/// This used to assert `x == 4.0` and called that "the clamp actually
/// engaging", with the *unclamped* `mx - 147 - 8 ≈ -29.667` as the
/// rejected hypothesis. Both hypotheses were wrong, because both placed
/// the book relative to the **container panel**. Vanilla's
/// `getXOrigin()` is `(width - 147) / 2 - xOffset`
/// (`RecipeBookComponent.java:167-169`) — screen-centred — and the
/// *panel* is what moves (`updateScreenPosition`).
///
/// So the expected value now comes from that expression, computed here
/// from the logical canvas width rather than from anything the code
/// under test produced:
///
/// * logical canvas `1280 / 3 = 426.67`, floored by
///   `logical_canvas` to `426`
/// * `426 >= 379`, so not too narrow, so `xOffset == 86`
/// * `floor((426 - 147) / 2) - 86 == 139 - 86 == 53`
///
/// The **rejected hypotheses** are therefore the two old answers, `4.0`
/// and `-29.667`, both asserted against below.
#[test]
fn recipe_panel_layout_matches_predicted_vanilla_derived_rects_at_1280x720() {
    let menu = Menu::crafting(3, 3);
    let main = slot_layout(&menu);
    let (mx, my) = panel_origin(&main, VIEW.0, VIEW.1);
    assert!((mx - 125.333_33).abs() < 0.01, "unexpected main panel x: {mx}");
    assert!((my - 37.0).abs() < 0.001, "unexpected main panel y: {my}");
    // The two rejected hypotheses, both from before the layout was inverted.
    let panel_relative_bx = mx - RECIPE_PANEL_W - RECIPE_PANEL_GAP;
    assert!(
        panel_relative_bx < 0.0,
        "the old panel-relative formula must still go negative here, or this \
         test is no longer exercising the case that produced the bug"
    );
    // Hand-computed from the canvas, not from the code under test — see the doc.
    let bx = 53.0;

    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);

    assert_eq!(layout.panel, Rect { x: bx, y: my, w: RECIPE_PANEL_W, h: RECIPE_PANEL_H });
    assert_ne!(layout.panel.x, 4.0, "the old clamp floor must not be the answer");
    assert_ne!(
        layout.panel.x, panel_relative_bx,
        "the old panel-relative formula must not be the answer either"
    );

    // `CraftingScreen.getRecipeBookButtonPosition`: local (5, 34) off the
    // *main* panel's own origin, not the book panel's. `recipe_book_panel_layout`
    // reports the **closed**-book layout (its `book_open` is `false`), so the
    // panel is unshifted and the toggle sits at the plain centred `mx`.
    assert_eq!(layout.toggle, Rect { x: mx + 5.0, y: my + 34.0, w: 20.0, h: 18.0 });

    assert_eq!(layout.search_box, Rect { x: bx + 25.0, y: my + 13.0, w: 81.0, h: 14.0 });
    assert_eq!(layout.magnifier, Rect { x: bx + 8.0, y: my + 13.0, w: 25.0, h: 14.0 });
    assert_eq!(layout.filter_button, Rect { x: bx + 110.0, y: my + 12.0, w: 26.0, h: 16.0 });

    // Tabs are `xo - 30` and **no longer clamped** — the clamp is what stacked
    // all four on the page ("squished into the menu"). `53 - 30 == 23`, on
    // canvas, which is the whole point of placing the book from the screen: at
    // this width the tabs have 23 px of room and at `RECIPE_BOOK_MIN_WIDTH` they
    // have exactly 0.
    assert_eq!(layout.tabs.len(), 4);
    assert_eq!(layout.tabs[0], Rect { x: bx - 30.0, y: my + 3.0, w: 35.0, h: 27.0 });
    assert!(layout.tabs[0].x > 0.0, "every tab must be on canvas");
    assert_ne!(
        layout.tabs[0].x, 4.0,
        "a tab parked on the old clamp floor is the reported bug"
    );
    assert_eq!(
        layout.tabs[3],
        Rect { x: bx - 30.0, y: my + 3.0 + 27.0 * 3.0, w: 35.0, h: 27.0 }
    );
    // And the four tabs are at four *distinct* y positions rather than stacked.
    for i in 1..4 {
        assert_ne!(layout.tabs[i].y, layout.tabs[i - 1].y);
    }

    assert_eq!(layout.recipes.len(), RECIPE_ITEMS_PER_PAGE);
    assert_eq!(layout.recipes[0], Rect { x: bx + 11.0, y: my + 31.0, w: 25.0, h: 25.0 });
    // Cell 19 = column 4, row 3 (20 cells, 5 columns).
    assert_eq!(
        layout.recipes[19],
        Rect { x: bx + 11.0 + 25.0 * 4.0, y: my + 31.0 + 25.0 * 3.0, w: 25.0, h: 25.0 }
    );

    assert!(layout.page_back.is_none(), "has_prev_page was false");
    assert_eq!(
        layout.page_forward,
        Some(Rect { x: bx + 93.0, y: my + 137.0, w: 12.0, h: 17.0 })
    );

    // Composition: the grid must sit *below* the header row it visually
    // reads as being below (search box / filter button), not merely have
    // the right x — a bug that moved only one axis would pass an
    // anchor-only check but still overlap visually.
    assert!(
        layout.recipes[0].y >= layout.search_box.y + layout.search_box.h,
        "recipe grid must start below the search box, not overlap it"
    );
    assert!(
        layout.recipes[0].y >= layout.filter_button.y + layout.filter_button.h,
        "recipe grid must start below the filter button, not overlap it"
    );
}

#[test]
fn recipe_panel_hit_test_toggle_works_even_when_closed() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, false);
    let center = |r: Rect| (r.x + r.w * 0.5, r.y + r.h * 0.5);
    let (tx, ty) = center(layout.toggle);
    assert_eq!(
        recipe_book_panel_hit_test(&layout, false, tx, ty),
        Some(RecipeBookPanelHit::Toggle)
    );
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, tx, ty),
        Some(RecipeBookPanelHit::Toggle)
    );
}

/// Negative control for the toggle-always-live test above: every *other*
/// widget must report nothing while the panel is closed, proving the
/// `open` gate actually gates something rather than the toggle test
/// passing by coincidence (e.g. an early-return bug that always answers
/// `Toggle`).
#[test]
fn recipe_panel_hit_test_reports_nothing_but_toggle_while_closed() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let center = |r: Rect| (r.x + r.w * 0.5, r.y + r.h * 0.5);
    let (sx, sy) = center(layout.search_box);
    assert_eq!(recipe_book_panel_hit_test(&layout, false, sx, sy), None);
    let (rx, ry) = center(layout.recipes[0]);
    assert_eq!(recipe_book_panel_hit_test(&layout, false, rx, ry), None);
    let (fx, fy) = center(layout.page_forward.unwrap());
    assert_eq!(recipe_book_panel_hit_test(&layout, false, fx, fy), None);
}

#[test]
fn recipe_panel_hit_test_resolves_every_widget_kind_while_open() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, true, true);
    let center = |r: Rect| (r.x + r.w * 0.5, r.y + r.h * 0.5);

    let (sx, sy) = center(layout.search_box);
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, sx, sy),
        Some(RecipeBookPanelHit::SearchBox)
    );
    let (fx, fy) = center(layout.filter_button);
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, fx, fy),
        Some(RecipeBookPanelHit::FilterButton)
    );
    let (t2x, t2y) = center(layout.tabs[2]);
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, t2x, t2y),
        Some(RecipeBookPanelHit::Tab(2))
    );
    let (r7x, r7y) = center(layout.recipes[7]);
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, r7x, r7y),
        Some(RecipeBookPanelHit::Recipe(7))
    );
    let (pfx, pfy) = center(layout.page_forward.unwrap());
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, pfx, pfy),
        Some(RecipeBookPanelHit::PageForward)
    );
    let (pbx, pby) = center(layout.page_back.unwrap());
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, pbx, pby),
        Some(RecipeBookPanelHit::PageBack)
    );
    // Inside the panel, between the tabs and the grid, hits nothing more
    // specific than the panel itself.
    assert_eq!(
        recipe_book_panel_hit_test(&layout, true, layout.panel.x + 147.0 - 2.0, layout.panel.y + 2.0),
        Some(RecipeBookPanelHit::Panel)
    );
    // Well outside the panel entirely.
    assert_eq!(recipe_book_panel_hit_test(&layout, true, -1000.0, -1000.0), None);
}

/// The physical-cursor entry point must divide by the same scale
/// [`hit_test_with_scale`] does, or clicks on the book panel and clicks
/// on a slot disagree about scale even though both look correct on
/// screen — this module's own documented failure mode.
#[test]
fn recipe_panel_hit_test_with_scale_matches_the_logical_entry_point() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let scale = crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1) as f32;
    let (lx, ly) = (layout.toggle.x + 1.0, layout.toggle.y + 1.0);
    let (px, py) = (lx * scale, ly * scale);
    assert_eq!(
        recipe_book_panel_hit_test_with_scale(
            &layout,
            true,
            crate::config::AUTO_GUI_SCALE,
            VIEW.0,
            VIEW.1,
            px,
            py
        ),
        Some(RecipeBookPanelHit::Toggle)
    );
}

// -- Recipe-book panel geometry actually reaches the screen ----------
//
// A gate that only checks `vertex_count() > 0` cannot tell a correctly
// placed quad from a degenerate one at NDC (7.0, 7.0) — geometry existing
// and geometry landing where the player can see it are different claims.
// `recipe_book_panel_geometry` originally took `Builder::new(1.0, 1.0,
// None)`, so every pixel-space coordinate was divided by `1.0` instead of
// the real logical-canvas size: the panel had real vertices and drew
// entirely off-screen. The test below asserts every emitted vertex's NDC
// position falls inside the visible `[-1, 1]` clip range — i.e. inside
// the screen rect — and the control directly beneath it reproduces the
// original bug's arithmetic to prove the assertion actually fails for a
// broken conversion rather than passing unconditionally.

#[test]
fn recipe_panel_geometry_open_draws_inside_the_logical_screen_rect() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let geo = recipe_book_panel_geometry(
        &layout,
        true,
        Some(0),
        &[],
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
    );
    assert!(geo.vertex_count() > 0, "an open panel must emit some geometry");
    for chunk in geo.verts.chunks_exact(FLOATS_PER_VERTEX) {
        let (x, y) = (chunk[0], chunk[1]);
        assert!(
            (-1.0..=1.0).contains(&x) && (-1.0..=1.0).contains(&y),
            "vertex at NDC ({x}, {y}) is outside the visible screen rect [-1, 1] — \
             geometry exists but nothing on screen can show it"
        );
    }
}

/// Negative control for the test above, pinned to the exact bug this
/// module shipped: converting the panel's own top-left corner with the
/// **wrong** denominator (`w = h = 1.0`, the original argument to
/// `Builder::new` before the fix) rather than the real logical-canvas
/// size. If this control does *not* land outside `[-1, 1]`, the
/// assertion above is vacuous — it would pass regardless of which
/// denominator this function actually used.
#[test]
fn control_the_original_wrong_denominator_fails_the_same_screen_rect_assertion() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let broken_to_ndc = |px: f32, py: f32| (2.0 * px / 1.0 - 1.0, 1.0 - 2.0 * py / 1.0);
    let (x, y) = broken_to_ndc(layout.panel.x, layout.panel.y);
    assert!(
        !(-1.0..=1.0).contains(&x) || !(-1.0..=1.0).contains(&y),
        "the broken w=h=1.0 conversion landed inside [-1, 1] by coincidence — \
         this control no longer distinguishes the fixed function from the broken one"
    );
}

/// Composition check: the panel and the toggle button must both draw —
/// a bug that returned early after the toggle (or drew the toggle but
/// forgot the panel body) would still pass the coverage test above,
/// since "everything is inside the screen rect" is satisfied by an
/// empty set of vertices too.
#[test]
fn recipe_panel_geometry_draws_something_when_closed_and_more_when_open() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let closed = recipe_book_panel_geometry(
        &layout,
        false,
        None,
        &[],
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
    );
    let open = recipe_book_panel_geometry(
        &layout,
        true,
        Some(0),
        &[],
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
    );
    assert!(closed.vertex_count() > 0, "the toggle button alone must still draw while closed");
    assert!(
        open.vertex_count() > closed.vertex_count(),
        "opening the panel must add geometry beyond just the toggle button"
    );
}

fn browse_test_book() -> RecipeBook {
    use lodestone_game::recipe::{
        CookingKind, CookingRecipe, Ingredient, Recipe, RecipeCategory, ShapedRecipe,
    };
    let mut book = RecipeBook::new();
    for (name, cat) in [
        ("aa_dropper", RecipeCategory::Redstone),
        ("bb_chest", RecipeCategory::Building),
        ("cc_torch", RecipeCategory::Misc),
    ] {
        book.insert(
            id(&format!("minecraft:{name}")),
            Recipe::Shaped(
                ShapedRecipe::new(1, 1, vec![Some(Ingredient::Item(id("minecraft:oak_planks")))], ItemStack::new(id(&format!("minecraft:{name}_result")), 1))
                    .with_category(cat),
            ),
        );
    }
    book.insert(
        id("minecraft:dd_smelt"),
        Recipe::Cooking(CookingRecipe {
            kind: CookingKind::Smelting,
            ingredient: Ingredient::Item(id("minecraft:iron_ore")),
            result: ItemStack::new(id("minecraft:iron_ingot"), 1),
            experience: 0.7,
            cooking_time: 200,
            category: RecipeCategory::Blocks,
        }),
    );
    book
}

#[test]
fn recipe_panel_contents_narrows_by_tab_and_paginates() {
    let book = browse_test_book();
    let all = recipe_book_panel_contents(&book, lodestone_model::RecipeBookType::Crafting, None, "", 0);
    assert_eq!(all.tabs, vec![
        lodestone_game::recipe::RecipeCategory::Building,
        lodestone_game::recipe::RecipeCategory::Redstone,
        lodestone_game::recipe::RecipeCategory::Misc,
    ]);
    assert_eq!(all.all_ids.len(), 3, "all three crafting recipes, the smelting one excluded");
    assert_eq!(all.total_pages, 1);
    assert_eq!(all.page, 0);

    // tab index 1 is `Redstone` in the visible-tabs order above.
    let redstone_tab = all.tabs.iter().position(|c| *c == lodestone_game::recipe::RecipeCategory::Redstone).unwrap();
    let narrowed = recipe_book_panel_contents(
        &book,
        lodestone_model::RecipeBookType::Crafting,
        Some(redstone_tab),
        "",
        0,
    );
    assert_eq!(narrowed.all_ids, vec![&id("minecraft:aa_dropper")]);
}

/// A page count of `1` even for **zero** matches (`RecipeBookPage`'s own
/// `ceil` never returns `0`) — the rejected hypothesis is `total_pages ==
/// 0`, which would make `page` clamp against an empty range and panic
/// on the `.min(total_pages - 1)` subtraction.
#[test]
fn recipe_panel_contents_reports_one_page_for_zero_matches_not_zero() {
    let book = browse_test_book();
    let contents = recipe_book_panel_contents(
        &book,
        lodestone_model::RecipeBookType::Crafting,
        None,
        "nonexistent_search_term",
        0,
    );
    assert_eq!(contents.all_ids.len(), 0);
    assert_eq!(contents.total_pages, 1);
    assert_eq!(contents.page, 0);
    assert!(contents.page_ids.is_empty());
}

// ---------------------------------------------------------------------------
// The recipe-book toggle button's per-screen position (owner bug report:
// "the book in my inventory is in the wrong spot")
// ---------------------------------------------------------------------------

/// Vanilla's toggle-button y offset, **local to `topPos`**, recomputed from
/// the two *absolute* jar expressions rather than restating the derived
/// constant — the point being that a test which asserts `y == 61.0` against a
/// draw that also says `61.0` measures nothing but a copy-paste.
///
/// `abs_y` is the screen's own `getRecipeBookButtonPosition().y()` as a
/// function of the logical canvas height, and `topPos` is
/// `AbstractContainerScreen.java:78`'s `(height - imageHeight) / 2`. Both use
/// **integer** division, as Java does; `imageHeight` is the `176x166` default
/// (`AbstractContainerScreen.java:33-34, 57-59`) for all three of these
/// screens.
fn vanilla_toggle_local_y(canvas_h: i32, abs_y: impl Fn(i32) -> i32) -> f32 {
    let top_pos = (canvas_h - 166) / 2;
    (abs_y(canvas_h) - top_pos) as f32
}

/// The **logical** canvas the layout was measured against, which is what
/// vanilla's own `this.height` is — not the physical `VIEW`. Derived through
/// the same call `recipe_book_panel_layout_with_scale` uses, so the two can
/// never disagree about the gui scale.
fn logical_view_h() -> i32 {
    let (_, h) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1);
    h as i32
}

/// The player-inventory screen's toggle sits at local `(104, 61)`, **not** the
/// crafting table's `(5, 34)`.
///
/// This is the owner-reported bug, and the shipped value was the crafting
/// table's, applied to every screen. `getRecipeBookButtonPosition` is
/// `abstract` with no default (`AbstractRecipeBookScreen.java:36`) and each
/// family overrides it:
///
/// | screen | absolute (jar) | local |
/// |---|---|---|
/// | `InventoryScreen.java:64` | `(leftPos + 104, height/2 - 22)` | `(104, 61)` |
/// | `CraftingScreen.java:27` | `(leftPos + 5, height/2 - 49)` | `(5, 34)` |
/// | `AbstractFurnaceScreen.java:44` | `(leftPos + 20, height/2 - 49)` | `(20, 34)` |
///
/// The y values are recomputed from those absolute expressions by
/// [`vanilla_toggle_local_y`], not restated.
#[test]
fn recipe_toggle_uses_each_screens_own_jar_derived_offset() {
    let h = logical_view_h();
    // Derived from the jar's absolute expressions, per the table above.
    let inv_y = vanilla_toggle_local_y(h, |ch| ch / 2 - 22);
    let craft_y = vanilla_toggle_local_y(h, |ch| ch / 2 - 49);
    // A gate that cannot tell the two apart proves nothing about the fix.
    assert_ne!(
        inv_y, craft_y,
        "the two screens' y offsets must differ or this test is vacuous"
    );

    for (label, menu, want_x, want_y) in [
        ("player inventory", Menu::player(), 104.0, inv_y),
        ("crafting table", Menu::crafting(3, 3), 5.0, craft_y),
        ("furnace", Menu::furnace(SpecialLayout::Furnace), 20.0, craft_y),
        (
            "blast furnace",
            Menu::furnace(SpecialLayout::BlastFurnace),
            20.0,
            craft_y,
        ),
        ("smoker", Menu::furnace(SpecialLayout::Smoker), 20.0, craft_y),
    ] {
        let main = slot_layout(&menu);
        let (mx, my) = panel_origin(&main, VIEW.0, VIEW.1);
        let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
        // Derived from the same `panel_origin` expression the draw uses, plus
        // the jar-derived local offset — never a restated absolute pixel.
        assert_eq!(
            layout.toggle,
            Rect { x: mx + want_x, y: my + want_y, w: 20.0, h: 18.0 },
            "{label}: toggle rect (bbox x {}..{} y {}..{}) is not the jar's own \
             position (bbox x {}..{} y {}..{})",
            layout.toggle.x,
            layout.toggle.x + layout.toggle.w,
            layout.toggle.y,
            layout.toggle.y + layout.toggle.h,
            mx + want_x,
            mx + want_x + 20.0,
            my + want_y,
            my + want_y + 18.0,
        );
    }
}

/// Negative control for the test above: the **shipped** behaviour — one
/// offset for every screen — must fail it.
///
/// Rather than describing what the old code would do, this reproduces it:
/// `recipe_toggle_local` is bypassed and `RECIPE_TOGGLE_LOCAL` (the crafting
/// table's own constant, which is what every screen used) is applied to the
/// player inventory. The assertion is that the result is *not* where the jar
/// puts it, and by how much — 99 px in x and 27 px in y, which is why the
/// button landed on the armour column.
#[test]
fn recipe_toggle_control_one_offset_for_every_screen_is_wrong_for_the_inventory() {
    let menu = Menu::player();
    let main = slot_layout(&menu);
    let (mx, my) = panel_origin(&main, VIEW.0, VIEW.1);
    let shipped = Rect {
        x: mx + RECIPE_TOGGLE_LOCAL.x,
        y: my + RECIPE_TOGGLE_LOCAL.y,
        w: RECIPE_TOGGLE_LOCAL.w,
        h: RECIPE_TOGGLE_LOCAL.h,
    };
    let actual = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true).toggle;
    assert_ne!(
        actual, shipped,
        "the fix is inert: the player inventory still uses the crafting table's offset"
    );
    // The magnitude, not merely the direction — a one-pixel nudge would
    // satisfy `assert_ne!` and would not have been visible to a player.
    assert!(
        (actual.x - shipped.x - 99.0).abs() < 0.001,
        "expected the inventory toggle 99 px right of the shipped position, got {}",
        actual.x - shipped.x
    );
    assert!(
        (actual.y - shipped.y - 27.0).abs() < 0.001,
        "expected the inventory toggle 27 px below the shipped position, got {}",
        actual.y - shipped.y
    );
    // And the dispatch itself, so a future refactor that reintroduces one
    // shared constant fails here too.
    assert_eq!(recipe_toggle_local(&Menu::player()), RECIPE_TOGGLE_LOCAL_INVENTORY);
    assert_eq!(recipe_toggle_local(&Menu::crafting(3, 3)), RECIPE_TOGGLE_LOCAL);
    assert_eq!(
        recipe_toggle_local(&Menu::furnace(SpecialLayout::Furnace)),
        RECIPE_TOGGLE_LOCAL_FURNACE
    );
}

// ---------------------------------------------------------------------------
// The recipe panel's colour-stream split (owner bug report: "the item counts
// are behind the items (at least the blocks)")
// ---------------------------------------------------------------------------

/// Indices of every vertex in a colour stream whose RGBA equals `ink`.
///
/// Colour is the only handle a CPU-side test has on *which* vertex is which:
/// the stream is flat `[x, y, r, g, b, a]` with the positions already in NDC,
/// and the fills are the same constants the draw uses (see
/// [`RECIPE_SLOT_COLOUR`] and [`FALLBACK_COUNT_INK`]), so this identifies a
/// vertex by the draw's own value rather than by a restated literal.
fn colour_vertex_indices(verts: &[f32], ink: [f32; 4]) -> Vec<usize> {
    verts
        .chunks_exact(FLOATS_PER_VERTEX)
        .enumerate()
        .filter(|(_, v)| v[2..6] == ink)
        .map(|(i, _)| i)
        .collect()
}

/// One page holding a single stack of `count` oak planks — a 4-output recipe,
/// which is the common real case for a count > 1 in the book, and a *block*
/// item, which is the class the owner singled out.
fn planks(count: i32) -> ItemStack {
    ItemStack::new("minecraft:oak_planks".parse().expect("valid id"), count)
}

fn panel_geo_for(results: &[&ItemStack]) -> (RecipeBookPanelLayout, RecipeBookPanelGeometry) {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let geo = recipe_book_panel_geometry(
        &layout,
        true,
        Some(0),
        results,
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
    );
    (layout, geo)
}

/// **The bug 2 gate.** A recipe result's stack-count digits must be submitted
/// *after* every piece of panel chrome, because this GUI path has no depth
/// compare and submission order alone decides z.
///
/// The load-bearing assertion is the last one: the highest-indexed slot-**well**
/// vertex must come before the lowest-indexed count-**digit** vertex. That is
/// the bug stated exactly. The shipped code emitted well and icon per cell in
/// one interleaved loop, so cell 1's well was submitted after cell 0's digits,
/// and — with the whole colour stream drawn in a single pass before the item
/// passes — every digit ended up under its own icon.
///
/// A test that merely checked both kinds of vertex *exist* passes under the bug,
/// which is why the ordering is asserted and not the presence.
#[test]
fn recipe_panel_count_digits_are_submitted_after_every_piece_of_chrome() {
    let stack = planks(4);
    let (_, geo) = panel_geo_for(&[&stack]);

    let wells = colour_vertex_indices(&geo.verts, super::recipe_book::RECIPE_SLOT_COLOUR);
    let digits = colour_vertex_indices(&geo.verts, super::builder::FALLBACK_COUNT_INK);

    // Preconditions, so a layout change that stops emitting either one turns
    // this into a failure rather than a silent pass over an empty set.
    assert_eq!(
        wells.len(),
        RECIPE_ITEMS_PER_PAGE * 6,
        "expected six vertices per slot well for all {RECIPE_ITEMS_PER_PAGE} cells"
    );
    assert!(!digits.is_empty(), "a count of 4 must emit count-digit geometry");

    let last_well = *wells.iter().max().expect("non-empty");
    let first_digit = *digits.iter().min().expect("non-empty");
    assert!(
        last_well < first_digit,
        "count digits are drawn under the chrome: last well vertex is #{last_well} \
         but the first count digit is #{first_digit} -- a well submitted after a \
         digit paints over it, since this path has no depth compare"
    );

    // And the split the renderer keys off must sit between the two, so the
    // digits land in the range drawn *after* the sprite and model passes.
    assert!(
        geo.chrome_vertex_count > last_well,
        "chrome_vertex_count {} must cover the last well vertex #{last_well}",
        geo.chrome_vertex_count
    );
    assert!(
        geo.chrome_vertex_count <= first_digit,
        "chrome_vertex_count {} must not include the first count digit #{first_digit}",
        geo.chrome_vertex_count
    );
}

/// The split point is exactly the chrome/icon boundary, pinned by an
/// independent route: the chrome a *populated* page emits must be
/// byte-for-byte the same quantity an **empty** page emits in total.
///
/// This is what catches chrome leaking into the icon range (or an icon leaking
/// into the chrome range) without needing to know a single coordinate — and it
/// is exactly what the interleaved loop violated.
#[test]
fn recipe_panel_chrome_range_equals_an_empty_pages_whole_stream() {
    let stack = planks(4);
    let (_, populated) = panel_geo_for(&[&stack]);
    let (_, empty) = panel_geo_for(&[]);

    assert_eq!(
        populated.chrome_vertex_count,
        empty.vertex_count(),
        "the chrome range must be invariant to what the page holds"
    );
    assert_eq!(
        empty.chrome_vertex_count,
        empty.vertex_count(),
        "an empty page is all chrome, so its split is the end of its stream"
    );
    assert!(
        populated.vertex_count() > populated.chrome_vertex_count,
        "a populated page must have a non-empty icon-overlay range or the \
         renderer's second colour pass is dead"
    );
}

/// The count digits, and *only* the count digits, are what the icon range gains
/// from a count > 1 — so the extra vertices really are the digits rather than
/// some other per-stack geometry, and the chrome range does not move.
///
/// `count == 1` draws no number at all (vanilla, and
/// `draw_item_icon_counted`'s own `slot.count > 1` guard), which makes this a
/// clean differential with no atlas and no font.
#[test]
fn recipe_panel_count_digits_land_in_the_icon_range_not_the_chrome_range() {
    let one = planks(1);
    let four = planks(4);
    let (_, geo1) = panel_geo_for(&[&one]);
    let (_, geo4) = panel_geo_for(&[&four]);

    assert_eq!(
        geo1.chrome_vertex_count, geo4.chrome_vertex_count,
        "the count must not change how much chrome is emitted"
    );
    assert!(
        colour_vertex_indices(&geo1.verts, super::builder::FALLBACK_COUNT_INK).is_empty(),
        "a count of 1 draws no number -- the differential is vacuous otherwise"
    );
    let tail1 = geo1.vertex_count() - geo1.chrome_vertex_count;
    let tail4 = geo4.vertex_count() - geo4.chrome_vertex_count;
    assert!(
        tail4 > tail1,
        "the icon-overlay range must grow by the digits: {tail1} -> {tail4}"
    );
    assert_eq!(
        tail4 - tail1,
        colour_vertex_indices(&geo4.verts, super::builder::FALLBACK_COUNT_INK).len(),
        "every vertex the count added must be a count-digit vertex"
    );
}

/// Liveness sweep, reused from the recipe-book agent's own instrument: every
/// vertex the panel submits must land inside the `[-1, 1]` NDC clip range.
///
/// Re-run here because reordering the emission is exactly the kind of change
/// that could drop or duplicate a rect, and this catches a vertex that stopped
/// being on screen at all — which is how two earlier bugs in this panel were
/// found (tabs off-canvas once `bx` clamped, and a placeholder canvas size that
/// put *every* vertex outside the range).
#[test]
fn recipe_panel_split_stream_still_draws_entirely_inside_the_clip_range() {
    let stack = planks(4);
    for results in [&[][..], &[&stack][..]] {
        let (_, geo) = panel_geo_for(results);
        let mut out_of_range: Vec<(usize, f32, f32)> = Vec::new();
        for (i, v) in geo.verts.chunks_exact(FLOATS_PER_VERTEX).enumerate() {
            if !(-1.0..=1.0).contains(&v[0]) || !(-1.0..=1.0).contains(&v[1]) {
                out_of_range.push((i, v[0], v[1]));
            }
        }
        assert!(
            out_of_range.is_empty(),
            "{} of {} vertices fall outside the [-1, 1] clip range, first few: {:?}",
            out_of_range.len(),
            geo.vertex_count(),
            &out_of_range[..out_of_range.len().min(4)]
        );
    }
}

// ---------------------------------------------------------------------------
// The recipe book's real 26.2 art (owner bug report: "the recipe book and its
// menu are completely incorrectly textured")
// ---------------------------------------------------------------------------

/// Native sizes of every recipe-book sprite, read out of `client.jar`'s own
/// IHDR chunks. These are the jar's values, not ours, and the hermetic pack
/// below is built at exactly these sizes so a test asserting a destination rect
/// is asserting vanilla's blit rather than a same-sized stand-in.
///
/// | sprite | jar path | size |
/// |---|---|---|
/// | button | `gui/sprites/recipe_book/button.png` | 20x18 |
/// | tab, tab_selected | `.../tab.png`, `.../tab_selected.png` | 35x27 |
/// | filter_disabled | `.../filter_disabled.png` | 26x16 |
/// | page_forward, page_backward | `.../page_forward.png`, `.../page_backward.png` | 12x17 |
/// | slot_craftable | `.../slot_craftable.png` | 25x25 |
/// | the panel page | `gui/recipe_book.png` (**not** under `sprites/`) | 256x256 |
const RECIPE_SPRITE_SIZES: &[(&str, u32, u32)] = &[
    (RECIPE_SPRITE_BUTTON, 20, 18),
    (RECIPE_SPRITE_TAB, 35, 27),
    (RECIPE_SPRITE_TAB_SELECTED, 35, 27),
    (RECIPE_SPRITE_FILTER, 26, 16),
    (RECIPE_SPRITE_FILTER_FURNACE, 26, 16),
    (RECIPE_SPRITE_PAGE_FORWARD, 12, 17),
    (RECIPE_SPRITE_PAGE_BACK, 12, 17),
    (RECIPE_SPRITE_SLOT, 25, 25),
];

/// A hermetic `GuiAtlas` carrying every recipe-book sprite at its real vanilla
/// size, plus the loose 256x256 panel sheet registered exactly the way
/// `resources::load_gui_atlas` registers it.
fn synthetic_recipe_gui_atlas() -> lodestone_render::GuiAtlas {
    use lodestone_assets::{MemorySource, ResourceManager, ResourceSource};

    fn solid_png(w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::new();
        let mut encoder = png::Encoder::new(&mut data, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| [10, 20, 30, 255]).collect();
        writer.write_image_data(&pixels).expect("png data");
        drop(writer);
        data
    }

    let mut src = MemorySource::default();
    for (id, w, h) in RECIPE_SPRITE_SIZES {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_png(*w, *h),
        );
    }
    // The panel sheet, at the jar's own 256x256 and at the jar's own loose path.
    src.insert(
        crate::resources::RECIPE_BOOK_TEXTURES[0].1.to_string(),
        solid_png(256, 256),
    );
    let manager = ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>]);
    lodestone_render::GuiAtlas::build_with_extras(&manager, crate::resources::RECIPE_BOOK_TEXTURES)
        .expect("synthetic recipe-book gui atlas builds")
}

/// **The bug 1 gate.** The panel page's UV window is the `147x166` sub-rect at
/// `(1, 1)` of the `256x256` sheet, byte-exact.
///
/// `RecipeBookComponent.java:305` is
/// `blit(RenderPipelines.GUI_TEXTURED, RECIPE_BOOK_LOCATION, xo, yo, 1.0F,
/// 1.0F, 147, 166, 256, 256)` — a fixed window, `u = v = 1`. The one-pixel
/// inset is real: decoding the PNG shows its opaque region is exactly
/// `x 1..147, y 1..166`.
///
/// The **rejected hypothesis** is the whole-sheet blit, which is what
/// `GuiAtlas::geometry` would produce for this id and what a naive wiring of a
/// loose texture gets: all 256x256 stretched into a 147x166 rect. The control
/// below asserts that hypothesis yields a *different*, wrong window rather than
/// merely asserting ours is "a" window.
#[test]
fn recipe_panel_page_samples_the_jars_own_147x166_window_at_1_1() {
    let atlas = synthetic_recipe_gui_atlas();
    let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
    let sheet = atlas
        .atlas()
        .sprite(
            &lodestone_assets::ResourceLocation::new(
                "lodestone",
                format!("gui/loose/{RECIPE_SPRITE_PANEL}"),
            )
            .expect("valid location"),
        )
        .expect("the panel sheet is in the atlas");
    assert_eq!(
        (sheet.width, sheet.height),
        (256, 256),
        "the panel sheet must be stitched whole at the jar's own 256x256"
    );

    let dst = [40.0, 50.0, RECIPE_PANEL_W, RECIPE_PANEL_H];
    let q = atlas
        .subregion_quad(RECIPE_SPRITE_PANEL, RECIPE_PANEL_SRC, dst)
        .expect("the panel sheet resolves");

    // Derived from the atlas placement plus the jar's own u/v, never restated.
    let want_min = [(sheet.x as f32 + 1.0) / aw, (sheet.y as f32 + 1.0) / ah];
    let want_max = [
        (sheet.x as f32 + 1.0 + RECIPE_PANEL_W) / aw,
        (sheet.y as f32 + 1.0 + RECIPE_PANEL_H) / ah,
    ];
    assert_eq!(q.dst, dst);
    assert_eq!(q.uv_min, want_min, "panel uv_min");
    assert_eq!(q.uv_max, want_max, "panel uv_max");

    // Control: the whole-sheet hypothesis. `geometry` stretches all 256x256 into
    // the 147x166 rect, which is a *different* UV window -- so a wiring that
    // reached for `geometry` here would sample the wrong region entirely, which
    // is what "completely incorrectly textured" looks like.
    let stretched = atlas.geometry(RECIPE_SPRITE_PANEL, dst[0], dst[1], dst[2], dst[3]);
    assert_eq!(stretched.len(), 1, "a stretched sprite is one quad");
    assert_ne!(
        stretched[0].uv_max, q.uv_max,
        "the whole-sheet control must produce a different window, or this gate \
         cannot tell the right sampling from the wrong one"
    );
    // And by how much: the window is 147/256 and 166/256 of the sheet, so the
    // wrong hypothesis oversamples by these exact factors.
    let (got_u, got_v) = (
        (stretched[0].uv_max[0] - stretched[0].uv_min[0]) / (q.uv_max[0] - q.uv_min[0]),
        (stretched[0].uv_max[1] - stretched[0].uv_min[1]) / (q.uv_max[1] - q.uv_min[1]),
    );
    assert!(
        (got_u - 256.0 / RECIPE_PANEL_W).abs() < 1e-4,
        "expected the wrong hypothesis to oversample u by 256/147, got {got_u}"
    );
    assert!(
        (got_v - 256.0 / RECIPE_PANEL_H).abs() < 1e-4,
        "expected the wrong hypothesis to oversample v by 256/166, got {got_v}"
    );
}

/// Every sprite id the panel emits exists, and is the size the jar says — so a
/// typo cannot silently draw nothing.
///
/// This is the assertion that catches the class of mistake most likely here:
/// vanilla's back-arrow file is `page_backward`, not `page_back`, and the
/// constant this module exposes is named `RECIPE_SPRITE_PAGE_BACK`. An id that
/// does not resolve draws nothing at all, with no error anywhere.
#[test]
fn every_recipe_book_sprite_id_resolves_at_its_jar_native_size() {
    let atlas = synthetic_recipe_gui_atlas();
    for (id, w, h) in RECIPE_SPRITE_SIZES {
        assert!(atlas.contains(id), "sprite id {id} does not resolve");
        assert_eq!(
            atlas.native_size(id),
            Some((*w, *h)),
            "sprite {id} is not the jar's native size"
        );
    }
    assert!(
        atlas.contains(RECIPE_SPRITE_PANEL),
        "the loose panel sheet must resolve under {RECIPE_SPRITE_PANEL}"
    );
}

/// The emitted sprite list is the right art in the right places, in an order
/// that cannot bury a control.
///
/// Every destination rect is asserted against the *layout* the draw uses, never
/// a restated pixel. The two order assertions are the load-bearing ones: the
/// opaque page must be first (anything before it is erased) and the toggle last
/// (the panel is clamped and may overlap the main panel's left edge, so a page
/// drawn over the toggle would bury a live control).
#[test]
fn recipe_panel_emits_vanillas_art_in_an_order_that_cannot_bury_a_control() {
    let stack = planks(4);
    let (layout, geo) = panel_geo_for(&[&stack]);

    let ids: Vec<&str> = geo.sprites.iter().map(|s| s.id).collect();
    assert_eq!(
        ids.first(),
        Some(&RECIPE_SPRITE_PANEL),
        "the opaque page must be submitted first or it erases everything under it"
    );
    assert_eq!(
        ids.last(),
        Some(&RECIPE_SPRITE_BUTTON),
        "the toggle must be submitted last so the page cannot bury a live control"
    );

    let find = |id: &str| -> Vec<RecipeBookSprite> {
        geo.sprites.iter().copied().filter(|s| s.id == id).collect()
    };

    // The page: the layout's own rect, sampling the jar's window.
    let page = find(RECIPE_SPRITE_PANEL);
    assert_eq!(page.len(), 1);
    assert_eq!(
        page[0].dst,
        [layout.panel.x, layout.panel.y, layout.panel.w, layout.panel.h]
    );
    assert_eq!(page[0].src, Some(RECIPE_PANEL_SRC));

    // The filter button, at the layout's rect and with no sub-rect.
    let filter = find(RECIPE_SPRITE_FILTER);
    assert_eq!(filter.len(), 1);
    assert_eq!(
        filter[0].dst,
        [
            layout.filter_button.x,
            layout.filter_button.y,
            layout.filter_button.w,
            layout.filter_button.h
        ]
    );
    assert_eq!(filter[0].src, None);

    // Tabs: `panel_geo_for` selects tab 0, so exactly one is the selected art
    // and it is nudged 2 px left of its own hit rect
    // (`RecipeBookTabButton.java:55-57`) while the rest are not.
    let selected = find(RECIPE_SPRITE_TAB_SELECTED);
    let plain = find(RECIPE_SPRITE_TAB);
    assert_eq!(selected.len(), 1, "one tab is selected");
    assert_eq!(plain.len(), layout.tabs.len() - 1);
    assert_eq!(
        selected[0].dst[0],
        layout.tabs[0].x - 2.0,
        "the selected tab's blit is nudged 2 px left of its widget rect"
    );
    assert_eq!(selected[0].dst[1], layout.tabs[0].y);
    for (s, r) in plain.iter().zip(layout.tabs.iter().skip(1)) {
        assert_eq!(s.dst[0], r.x, "an unselected tab is not nudged");
    }

    // Slot frames for populated cells only -- vanilla hides an unused
    // `RecipeButton`, and the sheet's grid region is uniform white with no
    // frames baked in, so emitting all 20 would draw a grid vanilla lacks.
    let slots = find(RECIPE_SPRITE_SLOT);
    assert_eq!(
        slots.len(),
        1,
        "one result on the page means exactly one slot frame, got {}",
        slots.len()
    );
    assert_eq!(
        slots[0].dst,
        [
            layout.recipes[0].x,
            layout.recipes[0].y,
            layout.recipes[0].w,
            layout.recipes[0].h
        ]
    );

    // Page arrows follow `layout`'s own `Option`s: `panel_geo_for` builds with
    // a next page and no previous one.
    assert_eq!(find(RECIPE_SPRITE_PAGE_FORWARD).len(), 1);
    assert_eq!(find(RECIPE_SPRITE_PAGE_BACK).len(), 0);
}

/// A **closed** panel emits the toggle's art and nothing else — the page, tabs
/// and slots must not be drawn behind a closed book.
#[test]
fn recipe_panel_closed_emits_only_the_toggle_sprite() {
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let geo = recipe_book_panel_geometry(
        &layout,
        false,
        None,
        &[],
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
    );
    assert_eq!(
        geo.sprites.len(),
        1,
        "a closed panel draws one sprite, got {:?}",
        geo.sprites.iter().map(|s| s.id).collect::<Vec<_>>()
    );
    assert_eq!(geo.sprites[0].id, RECIPE_SPRITE_BUTTON);
    assert_eq!(
        geo.sprites[0].dst,
        [layout.toggle.x, layout.toggle.y, layout.toggle.w, layout.toggle.h]
    );
}

/// Every sprite the panel emits resolves against the **real `client.jar`**, at
/// the size the jar carries.
///
/// The hermetic gates above prove the arithmetic and the emission; they cannot
/// prove the ids are the strings vanilla actually ships, because the synthetic
/// pack is built from the same constants it checks. This one closes that loop
/// against the real artefact — the standard this repo holds for an expected
/// value originating outside the code under test.
///
/// `#[ignore]`d because it reads `client.jar`. Run with
/// `cargo test -p lodestone-shell --lib recipe_book_sprites_resolve -- --ignored`.
#[test]
#[ignore = "reads the real client.jar"]
fn recipe_book_sprites_resolve_against_the_real_client_jar() {
    let atlas = crate::resources::load_gui_atlas().expect("client.jar and its GUI atlas");
    for (id, w, h) in RECIPE_SPRITE_SIZES {
        assert!(atlas.contains(id), "the real jar has no sprite {id}");
        assert_eq!(
            atlas.native_size(id),
            Some((*w, *h)),
            "sprite {id} is not the size this module expects"
        );
    }
    // The loose panel sheet, and its jar-derived 256x256.
    assert!(
        atlas.contains(RECIPE_SPRITE_PANEL),
        "the loose panel sheet is not registered -- \
         resources::load_gui_atlas must pass RECIPE_BOOK_TEXTURES as extras"
    );
    assert_eq!(atlas.native_size(RECIPE_SPRITE_PANEL), Some((256, 256)));

    // And every id the geometry actually emits, so a new sprite added to the
    // emission cannot escape this check.
    let stack = planks(4);
    let (_, geo) = panel_geo_for(&[&stack]);
    for s in &geo.sprites {
        assert!(
            atlas.contains(s.id),
            "the geometry emits {} but the real jar has no such sprite",
            s.id
        );
    }
}

// ---------------------------------------------------------------------
// The recipe book vs the hovered slot: two distinct faults, both reported
// as one symptom ("hovering the open recipe book highlights an inventory
// slot").
//
// 1. `build_inner` resolved `hovered` through `hit_test_with_scale`, which
//    is `hit_test_with_book(.., false)`, while the *draw* shifted the whole
//    panel right by `recipe_book_panel_shift`. So the highlight was
//    resolved against slot rects one shift left of where they were drawn.
//    The click path and the tooltip were already book-aware; only the
//    highlight was not.
// 2. Nothing told the frame the pointer was over an overlay, so even a
//    book-aware hover still resolves to whatever slot sits geometrically
//    *beneath* the book — which is what happens at narrow canvases, where
//    `recipe_book_panel_shift` is deliberately zero and the two overlap.
//
// These assert on `bg_verts` — what the GPU is handed — for the reason
// `CLAUDE.md` records twice today: a suite that only ever asserts on frame
// *data* stays green while the draw is wrong.
// ---------------------------------------------------------------------

/// As [`geo_with_background`], with the recipe book's own two flags.
fn geo_with_background_book(
    menu: &Menu,
    cursor: Option<[f32; 2]>,
    book_open: bool,
    hover_blocked: bool,
) -> ContainerGeometry {
    let bg = synthetic_background();
    let frame = ContainerFrame::new(Some(menu), "Title")
        .with_cursor(cursor)
        .with_book_open(book_open)
        .with_hover_blocked(hover_blocked);
    ContainerGeometry::build_inner(
        &frame,
        VIEW.0,
        VIEW.1,
        crate::config::AUTO_GUI_SCALE,
        &IconAssets {
            items: None,
            models: None,
        },
        None,
        Some(&bg),
    )
}

/// How far right the panel moves at `VIEW` with the book open — derived from
/// the same expression `build_inner` and `hit_test_with_book` both add, never
/// restated as a number.
fn book_shift(menu: &Menu) -> f32 {
    let layout = slot_layout(menu);
    let (canvas_w, _) = crate::menu::render::logical_canvas(
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
    );
    super::layout::recipe_book_panel_shift(canvas_w, layout.width, true)
}

/// The physical-pixel cursor over slot `menu_index` **as drawn** with the book
/// open — [`slot_point`] plus the panel shift, scaled the same way.
fn slot_point_book_open(menu: &Menu, menu_index: usize) -> [f32; 2] {
    let (cx, cy) = slot_point(menu, menu_index);
    let scale =
        crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1)
            .max(1) as f32;
    [cx + book_shift(menu) * scale, cy]
}

/// The two highlight quads for slot `menu_index` at its **drawn** origin —
/// `blitSprite(SLOT_HIGHLIGHT_{BACK,FRONT}, slot.x - 4, slot.y - 4, 24, 24)`,
/// where `slot.x` already carries the book shift.
fn highlight_hits(geo: &ContainerGeometry, want: [f32; 4]) -> usize {
    bg_rects(geo, crate::config::AUTO_GUI_SCALE)
        .iter()
        .filter(|r| r.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
        .count()
}

/// Fault 1, positively: with the book open, hovering the slot **where it is
/// drawn** highlights *that* slot.
///
/// The magnitude is what makes this a real assertion rather than a shape
/// check: at `VIEW` the shift is 77 logical pixels, which is more than four
/// whole 18px cells, so the pre-fix code resolved this cursor to a slot four
/// columns away and blitted the highlight there. The shift is asserted
/// non-zero first — at a canvas below `RECIPE_BOOK_MIN_WIDTH` it is
/// deliberately zero and this whole test would be vacuous, measuring the
/// book-closed path twice.
#[test]
fn with_the_book_open_the_highlight_follows_the_shifted_panel() {
    let menu = Menu::player();
    let shift = book_shift(&menu);
    assert!(
        shift > CELL,
        "at {VIEW:?} the panel must actually move, or this test measures the \
         book-closed path twice; shift was {shift}"
    );

    let geo = geo_with_background_book(&menu, Some(slot_point_book_open(&menu, 9)), true, false);
    let (sx, sy) = slot_origin(&menu, 9);
    let drawn = [sx + shift - 4.0, sy - 4.0, 24.0, 24.0];
    assert_eq!(
        highlight_hits(&geo, drawn),
        2,
        "expected the back and front highlight at the *drawn* slot {drawn:?}; \
         background quads were {:?}",
        bg_rects(&geo, crate::config::AUTO_GUI_SCALE)
    );

    // The wrong hypothesis, computed rather than described: the unshifted
    // origin is where a `hit_test_with_scale` hover would have put it if the
    // cursor had been over the unshifted rect. Nothing may be blitted there.
    let unshifted = [sx - 4.0, sy - 4.0, 24.0, 24.0];
    assert_ne!(drawn, unshifted);
    assert_eq!(
        highlight_hits(&geo, unshifted),
        0,
        "a highlight at the unshifted origin means the draw and the hover \
         disagree about where the panel is"
    );
}

/// Fault 1, as the reported symptom: with the book open, a cursor sitting over
/// the **recipe book's own page** — which overlaps the *unshifted* panel — must
/// highlight nothing.
///
/// This is the exact point the pre-fix code lit up. The two `assert!`s below
/// are the control that the point really is in the overlap: inside the book's
/// drawn rect, and inside a slot rect of the unshifted layout. Without them the
/// test could pass by picking a point that was over nothing at all.
#[test]
fn hovering_the_open_recipe_book_highlights_no_slot() {
    let menu = Menu::player();
    let (canvas_w, _) = crate::menu::render::logical_canvas(
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
    );
    let book = recipe_book_panel_layout_with_scale(
        &menu,
        crate::config::AUTO_GUI_SCALE,
        VIEW.0,
        VIEW.1,
        4,
        false,
        false,
        true,
    )
    .panel;
    // Slot 9's unshifted cell, which is what the pre-fix hover measured
    // against. Its right edge is 8 + 16 = 24 local, well inside the book's
    // page at this canvas.
    let (ux, uy) = slot_origin(&menu, 9);
    let point = [ux + 8.0, uy + 8.0];
    assert!(
        point[0] >= book.x
            && point[0] < book.x + book.w
            && point[1] >= book.y
            && point[1] < book.y + book.h,
        "the probe {point:?} must lie inside the book's drawn page {book:?}, \
         or this test is not reproducing the symptom"
    );
    assert!(
        canvas_w >= super::layout::RECIPE_BOOK_MIN_WIDTH,
        "at a narrow canvas the panel does not shift and the probe would be \
         over a real slot; canvas was {canvas_w}"
    );

    let scale =
        crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1)
            .max(1) as f32;
    let geo = geo_with_background_book(
        &menu,
        Some([point[0] * scale, point[1] * scale]),
        true,
        false,
    );
    let closed = geo_with_background_book(&menu, None, true, false);
    assert_eq!(
        bg_rects(&geo, crate::config::AUTO_GUI_SCALE).len(),
        bg_rects(&closed, crate::config::AUTO_GUI_SCALE).len(),
        "a cursor over the recipe book's page added background quads, i.e. it \
         highlighted a slot underneath the book"
    );
}

/// Fault 2, and the pair that is the whole fix: `hover_blocked` suppresses the
/// hovered slot **and the carried stack keeps tracking the pointer**.
///
/// The second half is why this is not simply "withhold the cursor". A carried
/// stack drawn through `Builder::draw_stack` lands in the item/model streams,
/// and it must be byte-identical between the blocked and unblocked frames —
/// the pointer still positions it over the book, exactly as vanilla drags a
/// held item across the page.
#[test]
fn hover_blocked_suppresses_the_highlight_and_keeps_the_carried_stack() {
    let mut menu = Menu::player();
    menu.set_carried(Some(ItemStack::new(
        lodestone_model::Identifier::new("minecraft", "stick").unwrap(),
        7,
    )));
    let cursor = Some(slot_point_book_open(&menu, 9));

    let open = geo_with_background_book(&menu, cursor, true, false);
    let blocked = geo_with_background_book(&menu, cursor, true, true);
    let (sx, sy) = slot_origin(&menu, 9);
    let drawn = [sx + book_shift(&menu) - 4.0, sy - 4.0, 24.0, 24.0];

    // The suppression, and its own control: the unblocked frame really does
    // highlight here, so a `hover_blocked` that did nothing would fail.
    assert_eq!(
        highlight_hits(&open, drawn),
        2,
        "control: the unblocked frame must highlight, or the suppression below \
         is measuring nothing"
    );
    assert_eq!(
        highlight_hits(&blocked, drawn),
        0,
        "hover_blocked must suppress the hovered slot's highlight"
    );
    assert_eq!(
        bg_rects(&blocked, crate::config::AUTO_GUI_SCALE).len() + 2,
        bg_rects(&open, crate::config::AUTO_GUI_SCALE).len(),
        "exactly the two highlight quads, and nothing else, may differ"
    );

    // ...and the carried stack is untouched. With no `ItemAtlas` attached the
    // stack degrades to `Builder::draw_stack`'s hash-derived swatch on the
    // **colour** stream (the same jar-less fallback every other icon here
    // takes), so that is the stream to measure. The `>` against a cursor-less
    // frame is the control: without it, two frames that both drew *no* carried
    // stack would satisfy the equality vacuously.
    let no_cursor = geo_with_background_book(&menu, None, true, false);
    assert!(
        open.verts.len() > no_cursor.verts.len(),
        "the carried stack must actually draw ({} colour floats against a \
         cursor-less frame's {}), or the equality below is vacuous",
        open.verts.len(),
        no_cursor.verts.len()
    );
    assert_eq!(
        blocked.verts, open.verts,
        "hover_blocked must not disturb the carried stack — it keeps following \
         the pointer over the book, which is why the suppression is its own \
         flag and not a withheld cursor"
    );
    assert_eq!(blocked.item_verts, open.item_verts);
    assert_eq!(blocked.model_verts, open.model_verts);
}

/// The tooltip rides the same single `hovered` resolution as the highlight, so
/// it inherits both halves of the fix. Asserted on the emitted **colour**
/// stream (the tooltip's box and text), not on a flag.
///
/// Skips without the real jar font: `emit_tooltip` requires a `VanillaFont` to
/// measure the box against and draws nothing without one, so a jar-less run has
/// no tooltip to suppress. That is a precondition skip and it is why the
/// `Some(font)` arm below asserts a *non-zero* delta first.
#[test]
fn hover_blocked_suppresses_the_tooltip_too() {
    let Some(font) = VanillaFont::shared() else {
        return; // jar-less: nothing measures, nothing draws
    };
    let mut menu = Menu::player();
    menu.set_slot_item(
        9,
        Some(ItemStack::new(
            lodestone_model::Identifier::new("minecraft", "diamond_sword").unwrap(),
            1,
        )),
    );
    let bg = synthetic_background();
    let build = |hover_blocked: bool| {
        let frame = ContainerFrame::new(Some(&menu), "Title")
            .with_cursor(Some(slot_point_book_open(&menu, 9)))
            .with_tooltips(false)
            .with_book_open(true)
            .with_hover_blocked(hover_blocked);
        ContainerGeometry::build_inner(
            &frame,
            VIEW.0,
            VIEW.1,
            crate::config::AUTO_GUI_SCALE,
            &IconAssets {
                items: None,
                models: None,
            },
            Some(&font),
            Some(&bg),
        )
    };
    let shown = build(false);
    let suppressed = build(true);
    assert!(
        shown.verts.len() > suppressed.verts.len(),
        "control: the unblocked frame must emit a tooltip at all — it drew \
         {} colour floats against the blocked frame's {}",
        shown.verts.len(),
        suppressed.verts.len()
    );
    // And the blocked frame is exactly the no-tooltip frame, not merely a
    // shorter one: nothing else in the colour stream may move.
    let no_tooltip = {
        let frame = ContainerFrame::new(Some(&menu), "Title")
            .with_cursor(Some(slot_point_book_open(&menu, 9)))
            .with_book_open(true);
        ContainerGeometry::build_inner(
            &frame,
            VIEW.0,
            VIEW.1,
            crate::config::AUTO_GUI_SCALE,
            &IconAssets {
                items: None,
                models: None,
            },
            Some(&font),
            Some(&bg),
        )
    };
    assert_eq!(suppressed.verts.len(), no_tooltip.verts.len());
}

// ---------------------------------------------------------------------------
// The inventory avatar's wiring (`ContainerGeometry::player_avatar`)
// ---------------------------------------------------------------------------

/// The player's own inventory carries an avatar; nothing else does.
///
/// `Some`/`None` here is the island question: `PlayerPreview` can be attached,
/// its matrices correct and its pass ready, and reach zero pixels because
/// `build_inner` never produces a placement. Vanilla calls
/// `extractEntityInInventoryFollowsMouse` from `InventoryScreen.extractBackground`
/// and from nowhere else, so a chest drawing one would be a divergence.
#[test]
fn only_the_player_inventory_carries_an_avatar() {
    let geo = |menu: &Menu| {
        ContainerGeometry::build_inner(
            &ContainerFrame::new(Some(menu), "Title"),
            VIEW.0,
            VIEW.1,
            crate::config::AUTO_GUI_SCALE,
            &IconAssets {
                items: None,
                models: None,
            },
            None,
            None,
        )
        .player_avatar
    };
    assert!(
        geo(&Menu::player()).is_some(),
        "the player inventory must place an avatar, or the whole pass is an island"
    );
    assert!(geo(&Menu::generic(27)).is_none(), "a chest has no avatar");
    assert!(
        geo(&Menu::crafting(3, 3)).is_none(),
        "a crafting table has no avatar"
    );
}

/// The avatar rect is derived from the **drawn** panel origin, not from an
/// independent guess at it.
///
/// The expectation comes from `widget_rect` — the rect the panel art is actually
/// blitted into by this same geometry — plus `InventoryScreen.java:101`'s `+26`
/// and `+8`. Restating `panel_origin_with_scale` here instead would be a control
/// that agrees with the draw by coincidence rather than by construction.
#[test]
fn the_avatar_rect_hangs_off_the_drawn_panel_rect() {
    let menu = Menu::player();
    let geo = ContainerGeometry::build_inner(
        &ContainerFrame::new(Some(&menu), "Title"),
        VIEW.0,
        VIEW.1,
        crate::config::AUTO_GUI_SCALE,
        &IconAssets {
            items: None,
            models: None,
        },
        None,
        None,
    );
    let panel = geo.widget_rect.expect("the panel drew");
    let avatar = geo.player_avatar.expect("the avatar placed");
    assert_eq!(avatar.rect.x, panel.x + 26.0);
    assert_eq!(avatar.rect.y, panel.y + 8.0);
    assert_eq!(avatar.rect.w, 49.0);
    assert_eq!(avatar.rect.h, 70.0);
    // …and it is inside the panel, which is the honest statement of "it is in the
    // recess" that does not depend on the panel's own size.
    assert!(
        avatar.rect.x >= panel.x
            && avatar.rect.y >= panel.y
            && avatar.rect.x + avatar.rect.w <= panel.x + panel.w
            && avatar.rect.y + avatar.rect.h <= panel.y + panel.h,
        "the avatar must sit inside the panel: avatar {avatar:?}, panel {panel:?}"
    );
}

/// An open recipe book shifts the whole panel right (`updateScreenPosition`), and
/// the avatar must travel with it by exactly the same delta the panel does.
///
/// This is the concrete failure a restated `panel_origin_with_scale` would ship:
/// the avatar would stay put while its recess slid out from under it, and every
/// hermetic gate with the book closed would still pass.
#[test]
fn an_open_recipe_book_moves_the_avatar_with_the_panel() {
    let menu = Menu::player();
    let geo = |book_open: bool| {
        ContainerGeometry::build_inner(
            &ContainerFrame::new(Some(&menu), "Title").with_book_open(book_open),
            VIEW.0,
            VIEW.1,
            crate::config::AUTO_GUI_SCALE,
            &IconAssets {
                items: None,
                models: None,
            },
            None,
            None,
        )
    };
    let closed = geo(false);
    let open = geo(true);
    let panel_shift =
        open.widget_rect.expect("panel").x - closed.widget_rect.expect("panel").x;
    assert!(
        panel_shift > 0.0,
        "premise-false control: the book must actually shift this panel at {VIEW:?}, \
         measured {panel_shift}"
    );
    let avatar_shift = open.player_avatar.expect("avatar").rect.x
        - closed.player_avatar.expect("avatar").rect.x;
    assert!(
        (avatar_shift - panel_shift).abs() < 1e-4,
        "the avatar must move with its recess: panel moved {panel_shift}, avatar \
         moved {avatar_shift}"
    );
}

/// The cursor arrives in **logical** space, and the head aims at where the
/// pointer visually is. `ContainerFrame::cursor` is physical viewport pixels — the
/// same space `hit_test_with_scale` takes and divides down — so a `gui_scale` of
/// 2 must halve it.
///
/// Both hypotheses are computed from outside the code under test: the correct
/// logical cursor, and the raw physical one. At scale 2 they must land on
/// different look angles, and the drawn one must be the logical one.
#[test]
fn the_avatars_cursor_is_divided_down_to_the_logical_canvas() {
    let menu = Menu::player();
    const SCALE: u32 = 2;
    let geo = |cursor: Option<[f32; 2]>| {
        ContainerGeometry::build_inner(
            &ContainerFrame::new(Some(&menu), "Title").with_cursor(cursor),
            VIEW.0,
            VIEW.1,
            SCALE,
            &IconAssets {
                items: None,
                models: None,
            },
            None,
            None,
        )
        .player_avatar
        .expect("avatar")
    };
    let no_cursor = geo(None);
    // A physical cursor 40 physical px right of the recess centre is 20 *logical*
    // px right of it at scale 2.
    let centre_logical = [
        no_cursor.rect.x + no_cursor.rect.w * 0.5,
        no_cursor.rect.y + no_cursor.rect.h * 0.5,
    ];
    let physical = [
        (centre_logical[0] + 20.0) * SCALE as f32,
        centre_logical[1] * SCALE as f32,
    ];
    let drawn = geo(Some(physical));
    assert!(
        (drawn.mouse[0] - (centre_logical[0] + 20.0)).abs() < 1e-3
            && (drawn.mouse[1] - centre_logical[1]).abs() < 1e-3,
        "the avatar's cursor must be the logical one: expected ~({}, {}), got {:?}",
        centre_logical[0] + 20.0,
        centre_logical[1],
        drawn.mouse
    );
    // The two hypotheses really are separable at this scale.
    let raw = PlayerAvatar::new(no_cursor.rect.x - 26.0, no_cursor.rect.y - 8.0, Some(physical));
    assert!(
        (drawn.look().head_yaw_deg - raw.look().head_yaw_deg).abs() > 1.0,
        "logical and raw-physical must give different look angles or this gate is \
         vacuous: {} vs {}",
        drawn.look().head_yaw_deg,
        raw.look().head_yaw_deg
    );
    // And with no cursor at all the avatar faces the viewer rather than snapping
    // to a corner.
    assert_eq!(
        no_cursor.look(),
        lodestone_render::gui_entity::GuiEntityLook::FORWARD
    );
}

// ---------------------------------------------------------------------------
// The recipe button's hover tooltip (`RecipeBookPage.extractTooltip`)
// ---------------------------------------------------------------------------

/// The tooltip must resolve **per recipe cell**, not "the pointer is somewhere
/// on the panel".
///
/// Three arms, and the two equalities are the load-bearing ones:
///
/// * cursor on cell `0`, which *has* a result stack — must emit extra colour
///   floats;
/// * cursor on cell `1`, which is inside the grid but has **no** result stack
///   (this page carries exactly one) — must be byte-identical to no cursor at
///   all. A tooltip keyed on the grid rect rather than on `page_results.get(i)`
///   fails here, and only here;
/// * cursor on the **search box**, a widget of the panel that is not a recipe —
///   likewise byte-identical.
///
/// The exact float *delta* for the hovered arm is deliberately not predicted:
/// it is a function of the glyph advances of the item's own display name in
/// whichever font pack is installed, so a number here would be a guess wearing
/// a prediction's clothes. What is predicted is that the other two arms move
/// nothing whatsoever.
///
/// Skips without the real jar font, for the same reason
/// [`hover_blocked_suppresses_the_tooltip_too`] does: `emit_tooltip_for_stack`
/// measures its box against a `VanillaFont` and draws nothing without one, so a
/// jar-less run has no tooltip to resolve.
#[test]
fn the_recipe_tooltip_resolves_only_over_a_cell_that_holds_a_result() {
    let Some(font) = VanillaFont::shared() else {
        return; // jar-less: nothing measures, nothing draws
    };
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let stack = ItemStack::new(
        lodestone_model::Identifier::new("minecraft", "torch").unwrap(),
        4,
    );
    let results = [&stack];
    // `RecipeTooltipContext::cursor` is physical viewport pixels while the
    // layout is logical, so the centre has to go back through the *same* scale
    // the geometry derives — never a restated constant.
    let scale = crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1)
        .max(1) as f32;
    let centre = |r: Rect| Some([(r.x + r.w * 0.5) * scale, (r.y + r.h * 0.5) * scale]);
    let build = |tooltip: RecipeTooltipContext| {
        super::recipe_book::recipe_book_panel_geometry_inner(
            &layout,
            true,
            Some(0),
            &results,
            crate::config::AUTO_GUI_SCALE,
            VIEW.0,
            VIEW.1,
            &IconAssets {
                items: None,
                models: None,
            },
            Some(&font),
            tooltip,
        )
    };

    let none = build(RecipeTooltipContext::default());
    let hovered = build(RecipeTooltipContext {
        cursor: centre(layout.recipes[0]),
        advanced: false,
    });
    let empty_cell = build(RecipeTooltipContext {
        cursor: centre(layout.recipes[1]),
        advanced: false,
    });
    let search_box = build(RecipeTooltipContext {
        cursor: centre(layout.search_box),
        advanced: false,
    });

    assert!(
        hovered.verts.len() > none.verts.len(),
        "hovering a populated recipe cell must emit tooltip geometry — {} \
         colour floats against the no-cursor {}",
        hovered.verts.len(),
        none.verts.len()
    );
    assert_eq!(
        empty_cell.verts, none.verts,
        "a recipe cell with no result stack must emit no tooltip at all"
    );
    assert_eq!(
        search_box.verts, none.verts,
        "the search box is not a recipe button and must emit no tooltip"
    );
    // Everything the tooltip adds lands in the *tail* of the colour stream, so
    // the chrome split — and therefore what the caller draws in its first pass —
    // is untouched.
    assert_eq!(hovered.chrome_vertex_count, none.chrome_vertex_count);
    assert_eq!(hovered.sprites, none.sprites);
}

/// F3+H reaches the recipe tooltip too: the advanced flag is forwarded to the
/// same line builder the container's slot tooltip uses, so it adds vanilla's
/// extra id line rather than being accepted and dropped.
#[test]
fn advanced_tooltips_add_lines_to_the_recipe_tooltip() {
    let Some(font) = VanillaFont::shared() else {
        return; // jar-less: see the gate above
    };
    let menu = Menu::crafting(3, 3);
    let layout = recipe_book_panel_layout(&menu, VIEW.0, VIEW.1, 4, false, true);
    let stack = ItemStack::new(
        lodestone_model::Identifier::new("minecraft", "torch").unwrap(),
        4,
    );
    let results = [&stack];
    let scale = crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1)
        .max(1) as f32;
    let cursor = Some([
        (layout.recipes[0].x + layout.recipes[0].w * 0.5) * scale,
        (layout.recipes[0].y + layout.recipes[0].h * 0.5) * scale,
    ]);
    let build = |advanced: bool| {
        super::recipe_book::recipe_book_panel_geometry_inner(
            &layout,
            true,
            Some(0),
            &results,
            crate::config::AUTO_GUI_SCALE,
            VIEW.0,
            VIEW.1,
            &IconAssets {
                items: None,
                models: None,
            },
            Some(&font),
            RecipeTooltipContext { cursor, advanced },
        )
    };
    let plain = build(false);
    let advanced = build(true);
    assert!(
        advanced.verts.len() > plain.verts.len(),
        "advanced tooltips must add at least the id line: {} vs {}",
        advanced.verts.len(),
        plain.verts.len()
    );
}

// -- merchant screen (issue #245's UI half) --------------------------------

/// Builds the merchant `Menu` through the **real** dispatch path — a
/// synthesized `ScreenOpened` + `ContainerContent`, exactly what a live
/// `OPEN_SCREEN`/content packet pair produces — rather than calling
/// `Menu::merchant()` directly. `lodestone_game::menus`'s own test module
/// carries the size-guard control for this dispatch
/// (`build_menu_selects_the_merchant_shape_for_a_real_open`,
/// `control_merchant_menu_type_with_the_wrong_size_falls_back_to_generic`);
/// this helper exists so the render gate below shares the same real path
/// rather than re-deriving it.
fn merchant_menu_via_real_path() -> Menu {
    let mut menus = lodestone_game::menus::Menus::new();
    assert!(menus.apply(&lodestone_model::ClientEvent::ScreenOpened {
        window_id: 9,
        menu_type: "minecraft:merchant".parse().expect("valid key"),
        title: lodestone_model::Text::literal("Villager"),
    }));
    assert!(menus.apply(&lodestone_model::ClientEvent::ContainerContent {
        window_id: 9,
        state_id: 1,
        items: vec![None; 3 + 36],
        carried_item: None,
    }));
    menus.opened().expect("container open").clone()
}

/// One real offer, folded through [`lodestone_game::trades::TradeOffers::apply`]
/// (the exact production fold) rather than hand-built already-populated —
/// registry id `1` is `minecraft:stone` in the 26.2 table
/// (`lodestone_data::items::item_name`).
fn trade_offers_via_real_path() -> lodestone_game::trades::TradeOffers {
    let mut store = lodestone_game::trades::TradeOffers::new();
    assert!(store.apply(&lodestone_model::ClientEvent::MerchantOffersReceived {
        window_id: 9,
        offers: vec![lodestone_model::event::MerchantOffer {
            cost_a: (1, 5),
            cost_b: None,
            result: None,
            out_of_stock: false,
            uses: 0,
            max_uses: 12,
            xp: 1,
            special_price_diff: 0,
            price_multiplier: 0.0,
            demand: 0,
        }],
        villager_level: 1,
        villager_xp: 0,
        show_progress: false,
        can_restock: false,
    }));
    store
}

/// [`ColourStream::rect`]'s own pixel-to-NDC formula, mirrored here (the
/// function itself is private to `builder.rs`) so the probe point below is
/// derived the same way the draw computes it.
fn to_ndc(px: f32, py: f32, w: f32, h: f32) -> (f32, f32) {
    (2.0 * px / w - 1.0, 1.0 - 2.0 * py / h)
}

/// How many colour-stream triangles cover NDC point `(x, y)` — a raster
/// point-test against real triangle geometry (not a vertex-only sample),
/// mirroring `app::recipe_book_wiring::coverage`'s inner loop. Returns a
/// *count*, not a bool: the panel's own flat jar-less background already
/// covers most of the interior, so the subject under test is a **count
/// delta**, not bare presence — see the gate below.
fn triangle_hits(verts: &[f32], (x, y): (f32, f32)) -> usize {
    verts
        .chunks_exact(FLOATS_PER_VERTEX * 3)
        .filter(|tri| {
            let (ax, ay) = (tri[0], tri[1]);
            let (bx, by) = (tri[FLOATS_PER_VERTEX], tri[FLOATS_PER_VERTEX + 1]);
            let (cx, cy) = (tri[FLOATS_PER_VERTEX * 2], tri[FLOATS_PER_VERTEX * 2 + 1]);
            let d = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
            if d.abs() < f32::EPSILON {
                return false;
            }
            let w0 = ((bx - x) * (cy - y) - (cx - x) * (by - y)) / d;
            let w1 = ((cx - x) * (ay - y) - (ax - x) * (cy - y)) / d;
            let w2 = 1.0 - w0 - w1;
            w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
        })
        .count()
}

/// The island control this unit is dispatched against: V4 (server-side trade
/// generation) does not exist yet, so nothing in a real session opens this
/// screen today. This gate is the substitute — it drives the **real**
/// `ScreenOpened`/`ContainerContent`/`MerchantOffersReceived` dispatch
/// (not `Menu::merchant()` or `row_layout()` called directly) all the way
/// through to rendered geometry, and proves the trade row lands at its own
/// expected screen position rather than merely "somewhere".
///
/// The point sampled is the first trade row's cost-A icon centre
/// (`super::merchant::row_layout(0)`, offset by the same
/// `panel_origin_with_scale` the draw uses) — derived from the real layout
/// expressions, never a restated pixel literal, per this repo's own
/// `cluster_top` warning. The control is `ContainerFrame::with_trades`
/// simply not called: **same menu**, same merchant special layout, only the
/// offer data withheld — so a merchant screen with no offers yet (every
/// pre-existing caller) is what proves the detector can fail.
#[test]
fn merchant_screen_opens_and_renders_its_trade_list_through_the_real_path() {
    let menu = merchant_menu_via_real_path();
    assert_eq!(
        menu.special_layout(),
        Some(SpecialLayout::Merchant),
        "sanity: the real ScreenOpened/ContainerContent dispatch must have \
         built the merchant shape"
    );
    let layout = crate::container::slot_layout(&menu);
    assert_eq!(
        layout.width, 276.0,
        "the merchant panel is 276px wide, not the generic 176 — MerchantScreen.java:57"
    );
    assert_eq!(background_kind(&menu), BackgroundKind::Merchant);

    let (w, h) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1);
    let (px, py) =
        crate::container::panel_origin_with_scale(&layout, crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1);
    let row0 = crate::container::merchant::row_layout(0);
    // +8, not +3: the swatch itself is inset 3px into its 16px cell and is
    // 10px wide, so its centre is 3 + 5 = 8px from the cell's own origin
    // (`Builder::draw_stack_counted`'s jar-less fallback).
    let point = to_ndc(px + row0.cost_a[0] + 8.0, py + row0.cost_a[1] + 8.0, w, h);

    let control = ContainerGeometry::build(&ContainerFrame::new(Some(&menu), "Villager"), VIEW.0, VIEW.1);
    let control_hits = triangle_hits(&control.verts, point);

    let trades = trade_offers_via_real_path();
    let frame = ContainerFrame::new(Some(&menu), "Villager").with_trades(Some(&trades), 0);
    let subject = ContainerGeometry::build(&frame, VIEW.0, VIEW.1);
    let subject_hits = triangle_hits(&subject.verts, point);

    assert!(
        subject_hits > control_hits,
        "control: a merchant screen with no TradeOffers attached must not draw \
         a swatch at the first trade row's own cost-A position — got \
         {control_hits} triangles there with no trades and {subject_hits} with \
         a real offer folded through TradeOffers::apply"
    );
}
