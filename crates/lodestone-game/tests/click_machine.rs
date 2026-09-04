//! Behavioural tests for the container click state machine, hand-computed
//! against vanilla `AbstractContainerMenu.doClick` semantics.

use lodestone_game::click::{Click, PlayerCtx, drag_type};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;

fn id(name: &str) -> lodestone_model::Identifier {
    name.parse().unwrap()
}

fn stack(name: &str, count: i32) -> ItemStack {
    ItemStack::new(id(name), count)
}

fn stack16(name: &str, count: i32) -> ItemStack {
    ItemStack::new(id(name), count).with_max_stack_size(16)
}

fn survival() -> PlayerCtx {
    PlayerCtx::survival()
}

// --- Pickup / place ---

#[test]
fn left_click_places_whole_cursor_into_empty_slot() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 64)));
    Click::left(0).apply(&mut menu, survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(64));
    assert!(menu.carried().is_none());
}

#[test]
fn right_click_places_one() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 64)));
    Click::right(0).apply(&mut menu, survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(1));
    assert_eq!(menu.carried().map(ItemStack::count), Some(63));
}

#[test]
fn left_click_full_slot_empty_cursor_picks_up_whole() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 40)));
    Click::left(0).apply(&mut menu, survival());
    assert!(menu.slot_item(0).is_none());
    assert_eq!(menu.carried().map(ItemStack::count), Some(40));
}

#[test]
fn right_click_full_slot_empty_cursor_takes_half_rounding_up() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 41)));
    Click::right(0).apply(&mut menu, survival());
    assert_eq!(menu.carried().map(ItemStack::count), Some(21));
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(20));
}

#[test]
fn left_click_same_item_merges_up_to_cap() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 60)));
    menu.set_carried(Some(stack("minecraft:stone", 10)));
    Click::left(0).apply(&mut menu, survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(64));
    assert_eq!(menu.carried().map(ItemStack::count), Some(6));
}

#[test]
fn left_click_different_items_swaps_cursor_and_slot() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:dirt", 5)));
    menu.set_carried(Some(stack("minecraft:stone", 10)));
    Click::left(0).apply(&mut menu, survival());
    assert_eq!(
        menu.slot_item(0)
            .map(|s| (s.item().path().to_string(), s.count())),
        Some(("stone".into(), 10))
    );
    assert_eq!(
        menu.carried()
            .map(|s| (s.item().path().to_string(), s.count())),
        Some(("dirt".into(), 5))
    );
}

#[test]
fn placing_into_smaller_max_stack_respects_item_cap() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack16("minecraft:egg", 16)));
    Click::left(0).apply(&mut menu, survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(16));
    assert!(menu.carried().is_none());
}

// --- Drop ---

#[test]
fn drop_cursor_outside_left_drops_all() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 12)));
    let outcome = Click::drop_cursor().apply(&mut menu, survival());
    assert!(menu.carried().is_none());
    assert_eq!(outcome.dropped.len(), 1);
    assert_eq!(outcome.dropped[0].count(), 12);
}

#[test]
fn drop_cursor_outside_right_drops_one() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 12)));
    let outcome = Click::drop_cursor_one().apply(&mut menu, survival());
    assert_eq!(menu.carried().map(ItemStack::count), Some(11));
    assert_eq!(outcome.dropped[0].count(), 1);
}

#[test]
fn throw_q_drops_one_from_slot() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 5)));
    let outcome = Click::drop_one(0).apply(&mut menu, survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(4));
    assert_eq!(outcome.dropped[0].count(), 1);
}

#[test]
fn throw_ctrl_q_drops_whole_slot() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 5)));
    let outcome = Click::drop_stack(0).apply(&mut menu, survival());
    assert!(menu.slot_item(0).is_none());
    assert_eq!(outcome.dropped[0].count(), 5);
}

/// Vanilla's own click-handler THROW step: `THROW` bails out entirely when
/// `!player.canDropItems()`, before taking anything from the slot. Vanilla
/// gates it *inside* the `THROW` arm, unlike the outside-cursor drop (`PICKUP`
/// with `slotIndex == -999`, `:404-412`), which drops unconditionally — so this
/// is a control specific to `Throw`, not a general "can't drop" gate.
#[test]
fn throw_is_a_noop_when_the_player_cannot_drop_items() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 5)));
    let ctx = PlayerCtx {
        infinite_materials: false,
        can_drop: false,
        selected_hotbar_slot: 0,
        furnace_input_items: None,
    };
    let outcome = Click::drop_one(0).apply(&mut menu, ctx);
    assert!(outcome.dropped.is_empty(), "nothing may be thrown");
    assert_eq!(
        menu.slot_item(0).map(ItemStack::count),
        Some(5),
        "the slot must be untouched"
    );
}

// --- Number-key swap ---

#[test]
fn hotbar_swap_moves_between_slot_and_hotbar() {
    let mut menu = Menu::player();
    menu.set_slot_item(9, Some(stack("minecraft:diamond", 3)));
    Click::hotbar_swap(9, 0).apply(&mut menu, survival());
    assert!(menu.slot_item(9).is_none());
    assert_eq!(menu.slot_item(36).map(ItemStack::count), Some(3));
    assert_eq!(menu.player_native(0).map(ItemStack::count), Some(3));
}

#[test]
fn hotbar_swap_exchanges_two_stacks() {
    let mut menu = Menu::player();
    menu.set_slot_item(9, Some(stack("minecraft:diamond", 3)));
    menu.set_player_native(0, Some(stack("minecraft:gold_ingot", 7)));
    Click::hotbar_swap(9, 0).apply(&mut menu, survival());
    assert_eq!(
        menu.slot_item(9).map(|s| s.item().path().to_string()),
        Some("gold_ingot".into())
    );
    assert_eq!(
        menu.player_native(0).map(|s| s.item().path().to_string()),
        Some("diamond".into())
    );
}

// --- Off-hand key swap ---

/// Same `Swap` mode, `buttonNum == 40` (vanilla's own click-handler SWAP
/// step's guard: `buttonNum >= 0 && buttonNum < 9 ||
/// buttonNum == 40`) — a distinct wire value from the hotbar keys, addressing
/// [`lodestone_game::menu::OFFHAND_NATIVE`] instead of a hotbar index.
#[test]
fn offhand_swap_moves_between_slot_and_offhand() {
    let mut menu = Menu::player();
    menu.set_slot_item(9, Some(stack("minecraft:diamond", 3)));
    Click::offhand_swap(9).apply(&mut menu, survival());
    assert!(menu.slot_item(9).is_none());
    assert_eq!(menu.slot_item(45).map(ItemStack::count), Some(3));
    assert_eq!(menu.player_native(40).map(ItemStack::count), Some(3));
}

#[test]
fn offhand_swap_exchanges_two_stacks() {
    let mut menu = Menu::player();
    menu.set_slot_item(9, Some(stack("minecraft:diamond", 3)));
    menu.set_player_native(40, Some(stack("minecraft:gold_ingot", 7)));
    Click::offhand_swap(9).apply(&mut menu, survival());
    assert_eq!(
        menu.slot_item(9).map(|s| s.item().path().to_string()),
        Some("gold_ingot".into())
    );
    assert_eq!(
        menu.player_native(40).map(|s| s.item().path().to_string()),
        Some("diamond".into())
    );
}

// --- Shift-click quick move ---

#[test]
fn shift_click_from_hotbar_to_main_in_player_menu() {
    let mut menu = Menu::player();
    menu.set_slot_item(36, Some(stack("minecraft:stone", 10)));
    Click::shift(36).apply(&mut menu, survival());
    assert!(menu.slot_item(36).is_none());
    assert_eq!(menu.slot_item(9).map(ItemStack::count), Some(10));
}

#[test]
fn shift_click_from_container_to_player_inventory() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 10)));
    Click::shift(0).apply(&mut menu, survival());
    assert!(menu.slot_item(0).is_none());
    assert_eq!(menu.slot_item(62).map(ItemStack::count), Some(10));
}

// --- Double-click gather ---

#[test]
fn double_click_gathers_matching_partial_stacks_first() {
    let mut menu = Menu::generic(27);
    // Real flow: first click lifts slot 0 onto the cursor (leaving it empty),
    // then the second click on the now-empty slot triggers PICKUP_ALL.
    menu.set_slot_item(0, Some(stack("minecraft:stone", 4)));
    menu.set_slot_item(1, Some(stack("minecraft:stone", 30)));
    menu.set_slot_item(2, Some(stack("minecraft:stone", 20)));
    menu.set_slot_item(3, Some(stack("minecraft:stone", 64)));
    Click::left(0).apply(&mut menu, survival()); // cursor=4, slot0 empty
    Click::double(0).apply(&mut menu, survival());
    // Partials (30+20) gathered first -> 4+50=54, then 10 from the full stack.
    assert_eq!(menu.carried().map(ItemStack::count), Some(64));
    assert!(menu.slot_item(1).is_none());
    assert!(menu.slot_item(2).is_none());
    assert_eq!(menu.slot_item(3).map(ItemStack::count), Some(54));
}

// --- Drag distribute ---

#[test]
fn left_drag_even_split_across_three_slots() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 9)));
    menu.perform_drag(drag_type::EVEN, &[0, 1, 2], survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(3));
    assert_eq!(menu.slot_item(1).map(ItemStack::count), Some(3));
    assert_eq!(menu.slot_item(2).map(ItemStack::count), Some(3));
    assert!(menu.carried().is_none());
}

#[test]
fn left_drag_even_split_leaves_remainder_on_cursor() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 10)));
    menu.perform_drag(drag_type::EVEN, &[0, 1, 2], survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(3));
    assert_eq!(menu.slot_item(1).map(ItemStack::count), Some(3));
    assert_eq!(menu.slot_item(2).map(ItemStack::count), Some(3));
    assert_eq!(menu.carried().map(ItemStack::count), Some(1));
}

#[test]
fn right_drag_one_each() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 5)));
    menu.perform_drag(drag_type::ONE, &[0, 1, 2], survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(1));
    assert_eq!(menu.slot_item(1).map(ItemStack::count), Some(1));
    assert_eq!(menu.slot_item(2).map(ItemStack::count), Some(1));
    assert_eq!(menu.carried().map(ItemStack::count), Some(2));
}

#[test]
fn single_slot_drag_degrades_to_place() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 9)));
    menu.perform_drag(drag_type::EVEN, &[0], survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(9));
    assert!(menu.carried().is_none());
}

#[test]
fn drag_only_fills_matching_or_empty_slots() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(1, Some(stack("minecraft:dirt", 1)));
    menu.set_carried(Some(stack("minecraft:stone", 9)));
    menu.perform_drag(drag_type::EVEN, &[0, 1, 2], survival());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(4));
    assert_eq!(
        menu.slot_item(1).map(|s| s.item().path().to_string()),
        Some("dirt".into())
    );
    assert_eq!(menu.slot_item(2).map(ItemStack::count), Some(4));
    assert_eq!(menu.carried().map(ItemStack::count), Some(1));
}

#[test]
fn creative_middle_drag_places_full_stacks() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 64)));
    menu.perform_drag(drag_type::CLONE, &[0, 1], PlayerCtx::creative());
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(64));
    assert_eq!(menu.slot_item(1).map(ItemStack::count), Some(64));
}

#[test]
fn clone_drag_rejected_in_survival() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 64)));
    menu.perform_drag(drag_type::CLONE, &[0, 1], survival());
    assert!(menu.slot_item(0).is_none());
    assert_eq!(menu.carried().map(ItemStack::count), Some(64));
}

// --- Middle-click clone ---

#[test]
fn middle_click_clone_creative_fills_cursor_full_stack() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 5)));
    Click::clone_slot(0).apply(&mut menu, PlayerCtx::creative());
    assert_eq!(menu.carried().map(ItemStack::count), Some(64));
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(5));
}

#[test]
fn middle_click_clone_noop_in_survival() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 5)));
    Click::clone_slot(0).apply(&mut menu, survival());
    assert!(menu.carried().is_none());
}

/// Vanilla's own click-handler CLONE step additionally requires
/// `this.getCarried().isEmpty()`. A creative middle-click while already
/// holding something must not overwrite the cursor.
#[test]
fn middle_click_clone_refuses_when_cursor_is_occupied() {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(stack("minecraft:stone", 5)));
    menu.set_carried(Some(stack("minecraft:dirt", 1)));
    Click::clone_slot(0).apply(&mut menu, PlayerCtx::creative());
    assert_eq!(
        menu.carried().map(|s| s.item().path().to_string()),
        Some("dirt".into()),
        "the held item must survive untouched"
    );
    assert_eq!(menu.slot_item(0).map(ItemStack::count), Some(5));
}

#[test]
fn every_click_bumps_state_id() {
    let mut menu = Menu::generic(27);
    let before = menu.state_id();
    Click::left(0).apply(&mut menu, survival());
    assert_eq!(menu.state_id(), before.wrapping_add(1));
}
