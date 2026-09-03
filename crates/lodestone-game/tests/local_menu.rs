//! A menu a plugin opened, with no server packet behind it.
//!
//! # What this gates
//!
//! Three separate claims, because they fail independently:
//!
//! 1. **It draws.** A local menu fills the same `opened` slot a server-opened
//!    container does and answers the same four accessors
//!    (`opened`/`opened_menu_type`/`opened_title`/`opened_window_id`) that
//!    `Sim::open_menu` reads. All four must be `Some`, because that function is
//!    four chained `?`s — miss one and the screen silently does not draw while
//!    the player's inventory has already been moved into it.
//! 2. **Nothing about it reaches the wire.** `opened_is_local()` is the predicate
//!    the shell's close and click paths consult. If it ever answered `false` for a
//!    plugin menu, a `ContainerClose` and every `ContainerClick` would be
//!    addressed to a window the server has never heard of.
//! 3. **The one player inventory still has one owner.** That single-owner
//!    invariant must survive a local open and close, or the hotbar goes blank.
//!
//! # The route this replaces, and why it was not good enough
//!
//! Before `open_local`, the only way to get a synthetic menu on screen was to
//! push `ScreenOpened` + `ContainerContent` through `IngestQueue`. That route is
//! exercised below (`the_synthetic_event_route_cannot_supply_a_prebuilt_menu`) to
//! pin *why* it is insufficient rather than merely asserting the new API works:
//! the content packet's length sizes the menu and `build_menu` re-derives the
//! layout from the menu-type key, so a plugin could never supply a pre-built
//! `Menu` — and an unknown key silently became `Menu::generic`.

use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::{Menu, SpecialLayout};
use lodestone_game::menus::{LOCAL_MENU_WINDOW_ID, Menus};
use lodestone_model::event::ClientEvent;
use lodestone_model::{Identifier, Text};

fn id(s: &str) -> Identifier {
    s.parse().expect("valid id")
}

/// A plugin's shop screen: a 27-slot generic container with stock in it.
fn shop_menu() -> Menu {
    let mut menu = Menu::generic(27);
    menu.set_slot_item(0, Some(ItemStack::new(id("minecraft:diamond"), 5)));
    menu.set_slot_item(1, Some(ItemStack::new(id("minecraft:emerald"), 3)));
    menu
}

/// A server-opened container, for the tests that need one to contrast against.
fn open_a_server_container(menus: &mut Menus, window_id: i32, container_size: usize) {
    menus.apply(&ClientEvent::ScreenOpened {
        window_id,
        menu_type: id("minecraft:generic_9x3"),
        title: Text::literal("Chest"),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id,
        state_id: 1,
        items: vec![None; container_size + 36],
        carried_item: None,
    });
}

/// Claim 1: every accessor `Sim::open_menu` chains a `?` on is populated.
#[test]
fn a_plugin_opened_menu_satisfies_every_accessor_the_draw_path_requires() {
    let mut menus = Menus::new();
    menus.open_local(
        shop_menu(),
        id("myshop:shop"),
        Text::literal("Bob's Emporium"),
    );

    // The exact four reads `Sim::open_menu` makes, in order.
    assert_eq!(menus.opened_window_id(), Some(LOCAL_MENU_WINDOW_ID));
    assert_eq!(menus.opened_menu_type(), Some(&id("myshop:shop")));
    assert_eq!(
        menus
            .opened_title()
            .map(|title| title.resolve(&|_| None).to_legacy_string()),
        Some("Bob's Emporium".to_owned())
    );
    let opened = menus.opened().expect("a menu is open");

    // And it is the menu the *plugin built*, not one re-derived from a key.
    assert_eq!(
        opened.slot_item(0).map(ItemStack::item),
        Some(&id("minecraft:diamond"))
    );
    assert_eq!(opened.slot_item(1).map(|s| s.count()), Some(3));
}

/// Claim 2, and the control for it in one test: a plugin menu reports local, a
/// server container reports **not** local, through the identical predicate.
///
/// Without the second half, `opened_is_local` returning `true` proves nothing —
/// a function that always returns `true` would pass the first half.
#[test]
fn local_is_true_for_a_plugin_menu_and_false_for_a_server_container() {
    let mut menus = Menus::new();
    assert!(
        !menus.opened_is_local(),
        "nothing open must report not-local, the safe answer"
    );

    menus.open_local(shop_menu(), id("myshop:shop"), Text::literal("Shop"));
    assert!(menus.opened_is_local(), "a plugin menu is local");

    // A real server open supersedes it, and must report not-local.
    open_a_server_container(&mut menus, 3, 27);
    assert!(
        !menus.opened_is_local(),
        "a server container must never report local -- its close and clicks \
         MUST reach the wire"
    );
    assert_eq!(menus.opened_window_id(), Some(3));
}

/// Claim 3: the single-owner one-inventory invariant survives a local open/close.
#[test]
fn the_one_player_inventory_survives_a_local_open_and_close() {
    let mut menus = Menus::new();
    // Put something in the hotbar through the server's own window-0 path, so the
    // starting state is one a real session produces rather than one this test
    // constructed by hand.
    menus.apply(&ClientEvent::ContainerSlot {
        window_id: 0,
        state_id: 1,
        slot: 36, // window 0's first hotbar slot
        item: Some(lodestone_model::ItemStack::new(id("minecraft:bread"), 8)),
    });
    let before = menus.player_native(0).cloned();
    assert!(
        before.is_some(),
        "precondition: the hotbar has bread in it, else this test measures nothing"
    );

    menus.open_local(shop_menu(), id("myshop:shop"), Text::literal("Shop"));
    assert_eq!(
        menus.player_native(0).cloned(),
        before,
        "the inventory moved into the plugin menu and must still read back"
    );

    assert!(menus.close_local(), "the local menu closes");
    assert!(menus.opened().is_none());
    assert_eq!(
        menus.player_native(0).cloned(),
        before,
        "and must be reclaimed on close, or the hotbar goes blank"
    );
}

/// `close_local` must refuse to close a **server** container. A plugin closing a
/// real container behind the player's back would desynchronise the server's own
/// open container with no packet explaining why.
#[test]
fn close_local_refuses_to_close_a_server_container() {
    let mut menus = Menus::new();
    open_a_server_container(&mut menus, 4, 27);
    assert!(
        !menus.close_local(),
        "close_local must report that it closed nothing"
    );
    assert!(
        menus.opened().is_some(),
        "and the server container must still be open"
    );
    assert_eq!(menus.opened_window_id(), Some(4));
}

/// A server-sourced slot write must not be able to land in a plugin's screen.
///
/// The window id is `i32::MIN`, which no server allocates, so this is testing the
/// `!o.local` guard in `menu_for_mut` rather than a realistic packet — which is
/// the point: the guard exists so that *if* such a packet ever arrives the plugin
/// menu is not silently rewritten.
#[test]
fn a_server_slot_write_cannot_reach_a_plugin_menu() {
    let mut menus = Menus::new();
    menus.open_local(shop_menu(), id("myshop:shop"), Text::literal("Shop"));
    let before = menus.opened().and_then(|m| m.slot_item(0)).cloned();

    menus.apply(&ClientEvent::ContainerSlot {
        window_id: LOCAL_MENU_WINDOW_ID,
        state_id: 9,
        slot: 0,
        item: Some(lodestone_model::ItemStack::new(id("minecraft:dirt"), 64)),
    });

    assert_eq!(
        menus.opened().and_then(|m| m.slot_item(0)).cloned(),
        before,
        "a packet addressed at the local window must be ignored, not applied"
    );
}

/// A plugin menu accepts clicks and predicts them locally — the prediction is
/// authoritative here rather than provisional, because no `container_set_slot` is
/// ever coming to correct it.
#[test]
fn a_click_in_a_plugin_menu_is_predicted_against_the_local_menu() {
    let mut menus = Menus::new();
    menus.open_local(shop_menu(), id("myshop:shop"), Text::literal("Shop"));

    let (window_id, _intent) = menus.click(Click::left(0), PlayerCtx::survival());
    assert_eq!(
        window_id, LOCAL_MENU_WINDOW_ID,
        "the click is addressed to the local sentinel, so a caller that ignored \
         opened_is_local() and sent it anyway names a window no server has"
    );
    // Picking up slot 0 moves the diamonds to the cursor.
    let opened = menus.opened().expect("still open");
    assert!(
        opened.slot_item(0).is_none(),
        "the local prediction actually ran"
    );
    assert_eq!(
        opened.carried().map(ItemStack::item),
        Some(&id("minecraft:diamond"))
    );
}

/// A plugin can open any `SpecialLayout` screen, which the synthetic-event route
/// could only do by *impersonating* the matching vanilla menu-type key and exact
/// container size.
#[test]
fn a_plugin_can_open_a_special_layout_screen_directly() {
    let mut menus = Menus::new();
    menus.open_local(
        Menu::item_combiner(3, 2, SpecialLayout::Anvil),
        id("myplugin:forge"),
        Text::literal("Forge"),
    );
    assert_eq!(
        menus.opened().and_then(Menu::special_layout),
        Some(SpecialLayout::Anvil)
    );
    assert!(menus.opened_is_local());
}

/// **The control that pins why the old route was insufficient**, rather than
/// asserting only that the new one works.
///
/// The synthetic `ScreenOpened` + `ContainerContent` pair does open a screen — but
/// the menu it opens is re-derived by `build_menu` from the menu-type key, so a
/// plugin's own key falls through to `Menu::generic` and the stock it wanted to
/// display is nowhere. That is the gap `open_local` closes.
#[test]
fn the_synthetic_event_route_cannot_supply_a_prebuilt_menu() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 7,
        menu_type: id("myshop:shop"),
        title: Text::literal("Shop"),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 7,
        state_id: 1,
        items: vec![None; 27 + 36],
        carried_item: None,
    });

    let opened = menus.opened().expect("the synthetic route does open something");
    assert!(
        opened.slot_item(0).is_none(),
        "and it is empty -- the plugin's stock could not be supplied through it"
    );
    assert!(
        opened.special_layout().is_none(),
        "an unknown key becomes Menu::generic, losing any layout the plugin wanted"
    );
    assert!(
        !menus.opened_is_local(),
        "worse: it is indistinguishable from a server open, so its close and \
         clicks would be sent to a window the server has never heard of -- the \
         correctness bug open_local exists to prevent"
    );
}

/// `ScreenOpened` alone opens nothing, which is the first trap of the old route:
/// a plugin pushing only the open event gets no screen and no error.
#[test]
fn control_screen_opened_alone_opens_nothing() {
    let mut menus = Menus::new();
    menus.apply(&ClientEvent::ScreenOpened {
        window_id: 7,
        menu_type: id("minecraft:generic_9x3"),
        title: Text::literal("Chest"),
    });
    assert!(
        menus.opened().is_none(),
        "the menu is not built until a content packet sizes it"
    );
    assert!(!menus.opened_is_local());
}
