//! The HUD hotbar and an open container's player rows must be the
//! **same** player inventory.
//!
//! # What was measured, and why these tests look like this
//!
//! `Menus` used to hold two `ClientMenu`s that each owned a full 41-slot player
//! container: `player` (window 0, what `Sim::player_menu` → the HUD reads) and
//! `opened.menu` (what the container screen renders and every click mutates).
//! A quick-move into the hotbar therefore updated the container's copy and left
//! window 0 untouched — the item was usable, because the server had it, and the
//! hotbar cell stayed blank or stale, including after the screen closed, because
//! a vanilla server sends nothing on close (`ServerPlayer.doCloseContainer` only
//! calls `transferState`).
//!
//! Vanilla has one `Inventory` and every menu's player-section slots are
//! references into it. Rust will not lend one `Container` to two owned `Menu`s,
//! so `Menus` models it as ownership that moves — see its type doc. These tests
//! pin the *observable* consequence of that, in both directions:
//!
//! * a mutation made through the **container** (a real click) is visible through
//!   the **HUD's** accessor, and
//! * a server update addressed to **window 0** is visible in the **open
//!   container's** own player rows.
//!
//! # `CLAUDE.md`'s *world* species applies directly here
//!
//! A test whose menu has no shared player section proves nothing about sharing.
//! So every case below builds its container through the real
//! `ScreenOpened` + `ContainerContent` fold (which is what performs the
//! ownership handoff) and mutates it through `Menus::click` — the click path
//! `docs/container-clicks.md` documents — never by writing a slot directly.
//!
//! Expected landing slots are hand-derived from 26.2, not from this port:
//! vanilla's own chest quick-move step moves a container stack with
//! `moveItemStackTo(stack, containerSize, slots.size(), true)`
//! and its own crafting-table quick-move step moves the result with
//! `moveItemStackTo(stack, 10, 46, true)`; the
//! trailing `true` is `reverseDirection`, so both fill from the **last** menu
//! slot backwards. The last slot of either menu is the ninth hotbar cell, native
//! index 8.

use lodestone_game::{
    click::{Click, PlayerCtx},
    item::ItemStack as GameItemStack,
    menus::Menus,
};
use lodestone_model::{
    ClientEvent, ItemStack as ModelItemStack, Text,
    ids::{Identifier, ResourceKey},
};

/// Native index of the ninth hotbar cell — where a reverse-direction quick-move
/// out of a container lands first. See the module doc.
const LAST_HOTBAR_NATIVE: usize = 8;
/// Native index of the first hotbar cell (`Inventory` slot 0, the one the HUD
/// draws leftmost).
const FIRST_HOTBAR_NATIVE: usize = 0;
/// Native index of the helmet slot (`InventoryMenu`'s armour run is `39 - i`).
const HELMET_NATIVE: usize = 39;
/// Native index of the off-hand slot.
const OFFHAND_NATIVE: usize = 40;

/// A 27-slot chest: `27 + 36` content slots.
const CHEST_SIZE: usize = 27;

fn id(s: &str) -> Identifier {
    s.parse().expect("valid id")
}

fn key(s: &str) -> ResourceKey {
    s.parse().expect("valid key")
}

fn wire(name: &str, count: u32) -> ModelItemStack {
    ModelItemStack {
        item: id(name),
        count,
        components: lodestone_model::ItemComponents::default(),
    }
}

fn game(name: &str, count: i32) -> GameItemStack {
    GameItemStack::new(id(name), count)
}

/// Opens `window_id` as a 27-slot chest through the real two-packet fold, with
/// `container` as the contents of container slot 0.
fn open_chest(menus: &mut Menus, window_id: i32, container: Option<ModelItemStack>) {
    menus.apply(&ClientEvent::ScreenOpened {
        window_id,
        menu_type: key("minecraft:generic_9x3"),
        title: Text::literal("Chest"),
    });
    let mut items = vec![None; CHEST_SIZE + 36];
    items[0] = container;
    menus.apply(&ClientEvent::ContainerContent {
        window_id,
        state_id: lodestone_model::ContainerStateId::new(1),
        items,
        carried_item: None,
    });
}

/// Opens `window_id` as a crafting table (10 container slots) with `result` in
/// its result slot, pushed the way a server pushes it: a `container_set_slot`
/// after the content packet, never a direct write.
fn open_crafting_table(menus: &mut Menus, window_id: i32, result: Option<ModelItemStack>) {
    menus.apply(&ClientEvent::ScreenOpened {
        window_id,
        menu_type: key("minecraft:crafting"),
        title: Text::translate("container.crafting", vec![]),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id,
        state_id: lodestone_model::ContainerStateId::new(1),
        items: vec![None; 10 + 36],
        carried_item: None,
    });
    menus.apply(&ClientEvent::ContainerSlot {
        window_id,
        state_id: lodestone_model::ContainerStateId::new(2),
        slot: 0,
        item: result,
    });
}

/// The issue's exact report: craft something, shift-click it out, and the
/// **HUD's own** hotbar record has it.
///
/// `Menus::player()` is what `Sim::player_menu()` returns and what `app.rs`
/// walks with `player_native(0..9)` to build `hotbar_records`, so this is the
/// HUD's accessor and not a proxy for it.
#[test]
fn crafting_result_quick_moved_to_the_hotbar_is_visible_to_the_hud() {
    let mut menus = Menus::new();
    open_crafting_table(&mut menus, 7, Some(wire("minecraft:torch", 4)));

    // Menu slot 0 is the result slot; shift-click is `QuickMove`.
    menus.click(Click::shift(0), PlayerCtx::survival());

    assert_eq!(
        menus.player().player_native(LAST_HOTBAR_NATIVE),
        Some(&game("minecraft:torch", 4)),
        "the HUD's hotbar view must see the crafted stack; before the single-owner \
         inventory fix it saw None"
    );
}

/// The same thing out of a chest, because the two menus take different
/// `quick_move` paths (`quick_move_generic` vs `quick_move_crafting`) and only
/// the storage is shared between them.
#[test]
fn chest_stack_quick_moved_to_the_hotbar_is_visible_to_the_hud() {
    let mut menus = Menus::new();
    open_chest(&mut menus, 4, Some(wire("minecraft:diamond", 3)));

    menus.click(Click::shift(0), PlayerCtx::survival());

    assert_eq!(
        menus.player().player_native(LAST_HOTBAR_NATIVE),
        Some(&game("minecraft:diamond", 3)),
        "the HUD's hotbar view must see the stack pulled out of the chest"
    );
}

/// **The half a server correction cannot paper over.** A vanilla server sends no
/// packets when a container closes, so if the storage left with the menu the row
/// would revert on close — which is the "renders empty after closing the screen"
/// half of the report.
#[test]
fn the_hotbar_still_has_it_after_the_screen_closes() {
    let mut menus = Menus::new();
    open_chest(&mut menus, 4, Some(wire("minecraft:diamond", 3)));
    menus.click(Click::shift(0), PlayerCtx::survival());

    menus.apply(&ClientEvent::ScreenClosed { window_id: 4 });

    assert!(menus.opened().is_none(), "the container really did close");
    assert_eq!(
        menus.player().player_native(LAST_HOTBAR_NATIVE),
        Some(&game("minecraft:diamond", 3)),
        "closing the screen must not take the inventory with it"
    );
}

/// The other direction, and the second question the single-owner-inventory fix
/// asked: a window-0
/// `container_set_slot` arriving **while another window is open** must reach the
/// one inventory, so the open container's own player rows show it too.
///
/// Vanilla gets this for free — `handleContainerSetSlot` routes container id `0`
/// to `player.inventoryMenu`, whose slots reference the shared `Inventory`. Here
/// it is a forward, so it needs a test.
#[test]
fn window_zero_set_slot_while_a_container_is_open_reaches_both_views() {
    let mut menus = Menus::new();
    open_chest(&mut menus, 4, None);

    // Window-0 menu slot 36 is the first hotbar cell (`InventoryMenu`: 0 result,
    // 1..=4 grid, 5..=8 armour, 9..=35 main, 36..=44 hotbar, 45 off-hand).
    menus.apply(&ClientEvent::ContainerSlot {
        window_id: 0,
        state_id: lodestone_model::ContainerStateId::new(3),
        slot: 36,
        item: Some(wire("minecraft:apple", 1)),
    });

    assert_eq!(
        menus.player().player_native(FIRST_HOTBAR_NATIVE),
        Some(&game("minecraft:apple", 1)),
        "the HUD view must see a window-0 correction"
    );
    // A chest menu is `0..27` container, `27..54` main, `54..63` hotbar, so slot
    // 54 is the same physical cell as window-0's slot 36. This is the
    // discriminator for *one storage* rather than *two that agree*: nothing
    // wrote this slot on the chest menu.
    assert_eq!(
        menus.opened().expect("chest open").slot_item(CHEST_SIZE + 27),
        Some(&game("minecraft:apple", 1)),
        "the open chest's own hotbar row is the same storage the correction hit"
    );
}

/// The native-indexed (`container_id == -2`) form of the same thing, which is
/// the one a real server actually sends mid-container.
#[test]
fn native_inventory_update_while_a_container_is_open_reaches_both_views() {
    let mut menus = Menus::new();
    open_chest(&mut menus, 4, None);

    menus.apply(&ClientEvent::InventorySlotChanged {
        slot: FIRST_HOTBAR_NATIVE as i32,
        item: Some(wire("minecraft:bread", 2)),
    });

    assert_eq!(
        menus.player().player_native(FIRST_HOTBAR_NATIVE),
        Some(&game("minecraft:bread", 2)),
    );
    assert_eq!(
        menus.opened().expect("chest open").slot_item(CHEST_SIZE + 27),
        Some(&game("minecraft:bread", 2)),
    );
}

/// A chest's `container_set_content` carries only **36** player slots (main +
/// hotbar) — armour and the off-hand are not in its slot list at all. So the
/// handoff has to move the whole 41-slot container, not the 36 slots the packet
/// mentions, or opening any chest would wipe the player's armour from the HUD.
#[test]
fn armour_and_offhand_survive_a_container_open_and_close() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::InventorySlotChanged {
        slot: HELMET_NATIVE as i32,
        item: Some(wire("minecraft:diamond_helmet", 1)),
    });
    menus.apply(&ClientEvent::InventorySlotChanged {
        slot: OFFHAND_NATIVE as i32,
        item: Some(wire("minecraft:shield", 1)),
    });

    open_chest(&mut menus, 4, None);
    assert_eq!(
        menus.player().player_native(HELMET_NATIVE),
        Some(&game("minecraft:diamond_helmet", 1)),
        "armour must survive the open handoff"
    );
    assert_eq!(
        menus.player().player_native(OFFHAND_NATIVE),
        Some(&game("minecraft:shield", 1)),
    );

    menus.apply(&ClientEvent::ScreenClosed { window_id: 4 });
    assert_eq!(
        menus.player().player_native(HELMET_NATIVE),
        Some(&game("minecraft:diamond_helmet", 1)),
        "…and the close handoff"
    );
    assert_eq!(
        menus.player().player_native(OFFHAND_NATIVE),
        Some(&game("minecraft:shield", 1)),
    );
}

/// A second container replacing the first without an intervening close (a real
/// sequence: a server opening a new window without closing the old one) must not
/// strand the inventory in the menu being dropped.
///
/// **Asserted on the armour slot, deliberately.** The first draft of this test
/// used a hotbar cell and failed — correctly, and for a reason worth recording:
/// the incoming window's own `container_set_content` carries 36 player slots and
/// overwrites main + hotbar wholesale, so a hotbar cell says nothing about where
/// the storage went. Armour and the off-hand are in **no** chest content packet,
/// so they are the only slots whose survival can only be explained by the
/// reclaim. This is `CLAUDE.md`'s "a control's premise can be false" in
/// miniature: the assertion was fine, the slot it pointed at could not see the
/// thing under test.
#[test]
fn a_second_container_replacing_the_first_keeps_the_inventory() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::InventorySlotChanged {
        slot: HELMET_NATIVE as i32,
        item: Some(wire("minecraft:diamond_helmet", 1)),
    });
    open_chest(&mut menus, 4, None);

    open_chest(&mut menus, 9, None);

    assert_eq!(menus.opened_window_id(), Some(9));
    assert_eq!(
        menus.player().player_native(HELMET_NATIVE),
        Some(&game("minecraft:diamond_helmet", 1)),
        "the inventory must have been reclaimed from window 4 before it was dropped"
    );
}

/// The control for the two quick-move tests above: the click path *itself* is
/// what moved the stack, so a quick-move of an **empty** slot must move nothing.
/// Without this, a hotbar that happened to contain the expected item for any
/// other reason would satisfy them.
#[test]
fn control_quick_moving_an_empty_slot_leaves_the_hotbar_alone() {
    let mut menus = Menus::new();
    open_chest(&mut menus, 4, None);

    menus.click(Click::shift(0), PlayerCtx::survival());

    assert_eq!(
        menus.player().player_native(LAST_HOTBAR_NATIVE),
        None,
        "nothing was in the chest, so nothing may appear in the hotbar"
    );
}

/// The control for the window-0 forward: an update addressed to a window that is
/// **neither** 0 nor the open container must not reach the inventory. Without
/// this, a forward that ignored `window_id` entirely would pass every test
/// above.
#[test]
fn control_a_set_slot_for_an_unknown_window_reaches_nothing() {
    let mut menus = Menus::new();
    open_chest(&mut menus, 4, None);

    menus.apply(&ClientEvent::ContainerSlot {
        window_id: 77,
        state_id: lodestone_model::ContainerStateId::new(3),
        slot: 36,
        item: Some(wire("minecraft:apple", 1)),
    });

    assert_eq!(menus.player().player_native(FIRST_HOTBAR_NATIVE), None);
    assert_eq!(
        menus.opened().expect("chest open").slot_item(CHEST_SIZE + 27),
        None
    );
}

/// The window-0 *content* packet's forward, and the guard that it does not
/// forward slots it must not: menu slots `0..5` are window 0's own 2×2 grid and
/// result, which are not part of the inventory and have no native index.
#[test]
fn window_zero_content_forwards_the_inventory_but_not_the_craft_grid() {
    let mut menus = Menus::new();
    open_chest(&mut menus, 4, None);

    let mut items = vec![None; 46];
    items[0] = Some(wire("minecraft:cake", 1)); // result slot — window 0's own
    items[1] = Some(wire("minecraft:stick", 2)); // 2x2 grid — window 0's own
    items[36] = Some(wire("minecraft:apple", 1)); // first hotbar cell
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 0,
        state_id: lodestone_model::ContainerStateId::new(4),
        items,
        carried_item: None,
    });

    let view = menus.player();
    assert_eq!(
        view.player_native(FIRST_HOTBAR_NATIVE),
        Some(&game("minecraft:apple", 1)),
        "the hotbar cell is part of the inventory and must forward"
    );
    assert_eq!(
        view.slot_item(0),
        Some(&game("minecraft:cake", 1)),
        "window 0's own result slot still belongs to window 0"
    );
    assert_eq!(
        view.slot_item(1),
        Some(&game("minecraft:stick", 2)),
        "…as does its 2x2 grid"
    );
    // And nothing leaked into the chest's container slots, which would be what a
    // forward keyed on menu index rather than on the `Slot` table would do.
    assert_eq!(menus.opened().expect("chest open").slot_item(0), None);
    assert_eq!(menus.opened().expect("chest open").slot_item(1), None);
}
