//! Throwing an item out with `Q` must change the count the hotbar
//! and the inventory screen draw.
//!
//! # The bug, and why it is unrecoverable rather than merely late
//!
//! The shell sent `ClientAction::DropSelectedItem` and touched nothing locally.
//! Vanilla's protocol contract for this action is *client predicts, server
//! trusts*, and the server half is the load-bearing part:
//!
//! * Vanilla's own server-side drop-item packet handler's `case DROP_ITEM:` /
//!   `case DROP_ALL_ITEMS:` call `this.player.drop(false)` / `drop(true)` and
//!   `return`. **No `SET_CONTAINER_SLOT`, no content packet, nothing comes back.**
//! * Vanilla's own local-player drop step: the client does
//!   `ItemStack prediction = this.getInventory().removeFromSelected(all);` and
//!   *then* sends the bare drop-item action packet. Vanilla names the
//!   variable `prediction`.
//!
//! So with no local mutation the count stays wrong **forever**, which is exactly
//! the shape of the report: the item really is dropped and the server is right;
//! only our display disagrees.
//!
//! # Where the expected values come from
//!
//! Vanilla's own selected-item removal step: if the selected stack is empty,
//! return empty; otherwise remove either the whole stack or a single item
//! from it,
//!
//! lowering through vanilla's own inventory-remove step →
//! its own container-helper remove step (which guards
//! `!isEmpty() && count > 0`) → its own stack-split step (`min(amount, count)`
//! then `shrink`).
//!
//! Every count below is that arithmetic, predicted **exactly** — `4`, never
//! "less than 5". Per `CLAUDE.md`'s *magnitude* species, a direction-only
//! predicate ("the count went down") is satisfied identically by a correct port
//! and by one that empties the stack on a plain `Q`.
//!
//! # The *world* species, and why nothing here writes a slot directly
//!
//! Every fixture is seeded through the real `ClientEvent::ContainerContent` /
//! `ScreenOpened` fold — the shape a server actually sends — rather than through
//! `Menu::set_player_native`. A test that seeds with the same setter the code
//! under test uses can agree with itself about a wrong slot mapping. Window-0
//! menu index `36` is native hotbar `0` (`Menu::player`'s slot table:
//! `36..=44` are `Slot::normal(0, 0..9)`), and that mapping is the thing being
//! relied on, so it must come from the fold.
//!
//! # Controls
//!
//! [`unpredicted_drop_leaves_the_stale_count`] is the negative control: the
//! pre-fix code path — send the action, mutate nothing — measured against the
//! same assertion. It **must** read `5`. Without it, "the count is 4" is not
//! evidence the assertion can tell a predicted model from an unpredicted one.

use lodestone_game::{
    click::{Click, PlayerCtx},
    item::ItemStack as GameItemStack,
    menu::Menu,
    menus::Menus,
    reconcile::ClientMenu,
};
use lodestone_model::{
    ClientEvent, ItemStack as ModelItemStack, Text,
    ids::{Identifier, ResourceKey},
};

/// Window-0 menu index of the first hotbar cell. `Menu::player` lays
/// `36..=44` as `Slot::normal(0, native)` for `native` in `0..9`, so this
/// addresses native hotbar slot 0 — the one a fresh join has selected.
const MENU_INDEX_HOTBAR_0: usize = 36;
/// Native index of the first hotbar cell: what `SelectedSlot(0)` means and what
/// `Menus::drop_selected` takes.
const NATIVE_HOTBAR_0: usize = 0;
/// Native index of the second hotbar cell — the untouched-neighbour control.
const NATIVE_HOTBAR_1: usize = 1;
/// A 27-slot chest sends `27 + 36` content slots.
const CHEST_SIZE: usize = 27;

fn id(s: &str) -> Identifier {
    s.parse().expect("valid id")
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

/// A session whose first hotbar cell holds `count` cobblestone, seeded the way a
/// server seeds it: one window-0 `container_set_content`.
///
/// `neighbour` fills the *second* hotbar cell, so every assertion below can also
/// show that the removal is addressed to one slot and not to the container.
fn session_with_hotbar(count: u32, neighbour: Option<u32>) -> Menus {
    let mut menus = Menus::new();
    let mut items = vec![None; 46];
    if count > 0 {
        items[MENU_INDEX_HOTBAR_0] = Some(wire("minecraft:cobblestone", count));
    }
    if let Some(n) = neighbour {
        items[MENU_INDEX_HOTBAR_0 + 1] = Some(wire("minecraft:dirt", n));
    }
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 0,
        state_id: 3,
        items,
        carried_item: None,
    });
    menus
}

/// What the HUD reads. `Menus::player()` is the exact expression
/// `Sim::player_menu()` returns, and `app/redraw.rs` walks it with
/// `player_native(0..9)` to build the `HotbarSlot` records `HudFrame::hotbar_items`
/// carries — so reading through here is the HUD's own accessor, not a proxy for
/// it. The pixel end of that chain is
/// `lodestone-shell/tests/hotbar_drop_prediction_pixels.rs`.
fn hud_count(menus: &Menus, native: usize) -> Option<i32> {
    menus.player().player_native(native).map(GameItemStack::count)
}

/// Plain `Q` from a stack of five: **four**, exactly.
#[test]
fn plain_drop_removes_exactly_one() {
    let mut menus = session_with_hotbar(5, Some(2));

    let dropped = menus.drop_selected(NATIVE_HOTBAR_0, false);

    assert_eq!(
        hud_count(&menus, NATIVE_HOTBAR_0),
        Some(4),
        "plain Q takes `1` (`removeFromSelected`'s `all ? getCount() : 1`), so five \
         cobblestone must read as four"
    );
    assert_eq!(
        dropped.as_ref(),
        Some(&game("minecraft:cobblestone", 1)),
        "the returned stack is what `ContainerHelper.removeItem` split off — one item, \
         same identity"
    );
    assert_eq!(
        hud_count(&menus, NATIVE_HOTBAR_1),
        Some(2),
        "the neighbouring cell must be untouched: this is a one-slot removal, not a \
         container-wide one"
    );
}

/// `Ctrl`+`Q` from a stack of five empties the slot, and the slot must be
/// **`None`** rather than a zero-count stack.
///
/// The distinction is not academic. `app/redraw.rs` maps a present stack to
/// `HotbarSlot { count: st.count().max(0) as u32, .. }` unconditionally, and
/// `hud/item_icon.rs:357` draws the number only `if slot.count > 1` — so a
/// surviving `Some(count: 0)` would draw a cobblestone icon with no number in a
/// slot the player just emptied. `Option` is how this port models vanilla's
/// `isEmpty()`, so the assertion is on the `Option` and not on the count.
#[test]
fn ctrl_drop_empties_the_slot_rather_than_leaving_a_zero_count_stack() {
    let mut menus = session_with_hotbar(5, Some(2));

    let dropped = menus.drop_selected(NATIVE_HOTBAR_0, true);

    assert_eq!(
        menus.player().player_native(NATIVE_HOTBAR_0),
        None,
        "Ctrl+Q takes `getCount()`, so the slot must be empty — and empty means \
         `None`, not `Some(count: 0)`"
    );
    assert_eq!(
        dropped.as_ref(),
        Some(&game("minecraft:cobblestone", 5)),
        "the whole stack comes back, at its full count"
    );
    assert_eq!(
        hud_count(&menus, NATIVE_HOTBAR_1),
        Some(2),
        "still a one-slot removal"
    );
}

/// Plain `Q` on the last item empties the slot — the boundary the
/// `count == 1` case would get wrong if the port decremented without
/// normalising.
#[test]
fn plain_drop_of_the_last_item_empties_the_slot() {
    let mut menus = session_with_hotbar(1, Some(2));

    let dropped = menus.drop_selected(NATIVE_HOTBAR_0, false);

    assert_eq!(
        menus.player().player_native(NATIVE_HOTBAR_0),
        None,
        "one minus one is empty, and empty is `None`"
    );
    assert_eq!(
        dropped.as_ref(),
        Some(&game("minecraft:cobblestone", 1)),
        "the single item still comes back as dropped"
    );
}

/// An empty selected slot is a no-op in both forms, and reports it.
///
/// `removeFromSelected`'s first line is the `selectedItem.isEmpty()` guard, and
/// `ContainerHelper.removeItem` guards `count > 0` underneath it — so
/// `Ctrl`+`Q` on an empty slot (`all ? getCount() : 1` → `0`) cannot produce a
/// phantom removal either.
#[test]
fn dropping_from_an_empty_slot_changes_nothing() {
    for all in [false, true] {
        let mut menus = session_with_hotbar(0, Some(2));

        let dropped = menus.drop_selected(NATIVE_HOTBAR_0, all);

        assert_eq!(
            dropped, None,
            "an empty slot drops nothing (all = {all}); vanilla returns `ItemStack.EMPTY` here, \
             and `LocalPlayer.drop` turns that into the `false` that suppresses the arm swing"
        );
        assert_eq!(
            menus.player().player_native(NATIVE_HOTBAR_0),
            None,
            "the empty slot stays empty (all = {all})"
        );
        assert_eq!(
            hud_count(&menus, NATIVE_HOTBAR_1),
            Some(2),
            "and nothing else moves either (all = {all})"
        );
    }
}

/// **The negative control.** The pre-fix code path — resolve the action, send it,
/// mutate nothing — measured against [`plain_drop_removes_exactly_one`]'s own
/// assertion. It must read `5`.
///
/// Run it and watch it fail if the `assert_eq!(…, Some(4))` below is swapped in:
/// that is the whole content of the bug report, and it is what proves the
/// assertion can distinguish a predicted model from an unpredicted one rather
/// than passing for some unrelated reason.
#[test]
fn unpredicted_drop_leaves_the_stale_count() {
    let menus = session_with_hotbar(5, Some(2));

    // No `drop_selected` call: this is `send_drop_selected` as it shipped —
    // `net.send_action(action)` and nothing else.

    assert_eq!(
        hud_count(&menus, NATIVE_HOTBAR_0),
        Some(5),
        "control: without the prediction the HUD still reads five, and the server will \
         never correct it (vanilla's own drop-item packet handler sends nothing). \
         If this ever reads `4`, the fixture is predicting behind the test's back and \
         every assertion above is vacuous"
    );
}

/// The removal must reach the copy that is actually drawn while a **container
/// screen is open**.
///
/// The single-owner-inventory hazard applies directly: there is one player inventory and its
/// owner *moves* to the container's menu when a screen opens, leaving window 0's
/// player section an empty husk. A `drop_selected` that wrote `self.player`
/// would land in a menu nothing reads. Vanilla has no such fork — its `Slot`s
/// are references into the one `Inventory` — so this test has no jar counterpart
/// and exists purely to pin our modelling of the aliasing.
#[test]
fn drop_reaches_the_inventory_owner_while_a_container_is_open() {
    let mut menus = session_with_hotbar(5, Some(2));
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 9,
        menu_type: "minecraft:generic_9x3".parse::<ResourceKey>().expect("key"),
        title: Text::literal("Chest"),
    });
    // The content packet is what performs the ownership handoff.
    let mut items = vec![None; CHEST_SIZE + 36];
    // The chest's own player-inventory tail: hotbar sits last, `27 + 27 = 54`.
    items[CHEST_SIZE + 27] = Some(wire("minecraft:cobblestone", 5));
    items[CHEST_SIZE + 28] = Some(wire("minecraft:dirt", 2));
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 9,
        state_id: 4,
        items,
        carried_item: None,
    });
    assert_eq!(
        hud_count(&menus, NATIVE_HOTBAR_0),
        Some(5),
        "precondition: the chest fold must have put five cobblestone where the HUD reads \
         it, or this test proves nothing about ownership"
    );

    menus.drop_selected(NATIVE_HOTBAR_0, false);

    assert_eq!(
        hud_count(&menus, NATIVE_HOTBAR_0),
        Some(4),
        "the removal has to land in whichever menu currently owns the one player \
         inventory; writing window 0's husk would leave this at five"
    );
    assert_eq!(
        menus
            .opened()
            .and_then(|m| m.player_native(NATIVE_HOTBAR_0))
            .map(GameItemStack::count),
        Some(4),
        "and the open container's own player rows are that same storage, so they must \
         read four too"
    );
}

/// A drop writes **both** halves of the prediction pair.
///
/// `ClientMenu` keeps `predicted` (what the UI draws) and `confirmed` (the last
/// server truth). For a container click only `predicted` moves, because the
/// server echoes and `reconcile` decides. A drop has no echo: the server already
/// performed the identical removal silently. So `confirmed` must follow, or the
/// next full `container_set_content` diffs our stale `confirmed` against the
/// server's real contents and reports a **visible correction that never
/// happened**.
///
/// Asserted through `ClientMenu` directly because `Menus` deliberately exposes no
/// `confirmed` accessor.
#[test]
fn drop_moves_both_the_prediction_and_the_confirmation() {
    let mut menu = ClientMenu::new(Menu::player());
    menu.reconcile(lodestone_game::reconcile::ServerUpdate::SetSlot {
        state_id: 3,
        slot: MENU_INDEX_HOTBAR_0,
        item: Some(game("minecraft:cobblestone", 5)),
    });
    assert_eq!(
        menu.confirmed().player_native(NATIVE_HOTBAR_0).map(GameItemStack::count),
        Some(5),
        "precondition: the server update must have reached `confirmed`"
    );

    menu.remove_from_selected(NATIVE_HOTBAR_0, false);

    assert_eq!(
        menu.menu().player_native(NATIVE_HOTBAR_0).map(GameItemStack::count),
        Some(4),
        "the drawn copy"
    );
    assert_eq!(
        menu.confirmed().player_native(NATIVE_HOTBAR_0).map(GameItemStack::count),
        Some(4),
        "and the confirmed copy, because the server did this too without telling us"
    );
}

/// The control for the test above: an ordinary **container click** must move only
/// `predicted`, leaving `confirmed` where the server left it.
///
/// Without this, "a drop writes both" is not evidence of anything — a
/// `ClientMenu` that wrote both on *every* mutation would pass that test too, and
/// would break container reconciliation.
#[test]
fn a_container_click_moves_only_the_prediction() {
    let mut menu = ClientMenu::new(Menu::player());
    menu.reconcile(lodestone_game::reconcile::ServerUpdate::SetSlot {
        state_id: 3,
        slot: MENU_INDEX_HOTBAR_0,
        item: Some(game("minecraft:cobblestone", 5)),
    });

    // Pick the stack up onto the cursor: a plain left click.
    menu.predict(
        Click::left(MENU_INDEX_HOTBAR_0),
        PlayerCtx::survival(),
    );

    assert_eq!(
        menu.menu().player_native(NATIVE_HOTBAR_0),
        None,
        "precondition: the click must actually have emptied the predicted slot, or this \
         control measures nothing"
    );
    assert_eq!(
        menu.confirmed().player_native(NATIVE_HOTBAR_0).map(GameItemStack::count),
        Some(5),
        "control: a click leaves `confirmed` alone — the server will echo and `reconcile` \
         decides. Only a drop, which gets no echo, writes both"
    );
}

/// The container-open drop key already predicted, and this pins that it still
/// does — so the asymmetry recorded in `docs/container-clicks.md` is measured
/// rather than asserted from reading the call sites.
///
/// `App::send_container_drop` produces `ContainerMenuKey::Drop { ctrl }` clicks,
/// which lower to `ContainerInput::Throw` and `Menu::do_throw` through
/// `Menus::click` → `ClientMenu::predict`. That is a real prediction; the
/// gameplay path had none. Both keys, one screen apart, and only one of them was
/// broken.
#[test]
fn the_container_screen_drop_key_predicts_too() {
    let mut menus = session_with_hotbar(5, None);

    // `ClickType::THROW`, button 0 = drop one, on the hovered slot.
    menus.click(
        Click::drop_one(MENU_INDEX_HOTBAR_0),
        PlayerCtx::survival(),
    );

    assert_eq!(
        hud_count(&menus, NATIVE_HOTBAR_0),
        Some(4),
        "the container path predicts through `do_throw`; it was never the broken half"
    );
}
