//! Tests for the predict-then-reconcile seam.

use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;
use lodestone_game::reconcile::{ClientMenu, Reconciliation, ServerUpdate};

fn stack(name: &str, count: i32) -> ItemStack {
    ItemStack::new(name.parse().unwrap(), count)
}

#[test]
fn prediction_is_applied_locally_and_reports_changed_slots() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 64)));
    let mut client = ClientMenu::new(menu);

    let intent = client.predict(Click::left(0), PlayerCtx::survival());
    // Slot 0 changed from empty to 64 stone; cursor emptied.
    assert_eq!(intent.changed_slots.len(), 1);
    assert_eq!(intent.changed_slots[0].0, 0);
    assert_eq!(
        intent.changed_slots[0].1.as_ref().map(ItemStack::count),
        Some(64)
    );
    assert!(intent.carried.is_none());
    assert_eq!(client.menu().slot_item(0).map(ItemStack::count), Some(64));
}

#[test]
fn matching_server_content_is_a_silent_confirmation() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 64)));
    let mut client = ClientMenu::new(menu);
    let intent = client.predict(Click::left(0), PlayerCtx::survival());

    // Server agrees exactly.
    let mut items = vec![None; client.menu().slot_count()];
    items[0] = Some(stack("minecraft:stone", 64));
    let recon = client.reconcile(ServerUpdate::SetContent {
        state_id: intent.state_id,
        items,
        carried: None,
    });
    assert_eq!(recon, Reconciliation { corrected: false });
    assert_eq!(client.menu().slot_item(0).map(ItemStack::count), Some(64));
}

#[test]
fn disagreeing_server_rolls_back_the_prediction() {
    // Client predicts a successful place, but the server rejects it (e.g. the
    // slot was actually full on the server). The server's content wins.
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 64)));
    let mut client = ClientMenu::new(menu);
    let intent = client.predict(Click::left(0), PlayerCtx::survival());
    assert_eq!(client.menu().slot_item(0).map(ItemStack::count), Some(64));

    // Server says slot 0 is empty and the cursor still holds the stack.
    let items = vec![None; client.menu().slot_count()];
    let recon = client.reconcile(ServerUpdate::SetContent {
        state_id: intent.state_id,
        items,
        carried: Some(stack("minecraft:stone", 64)),
    });
    assert_eq!(recon, Reconciliation { corrected: true });
    assert!(client.menu().slot_item(0).is_none());
    assert_eq!(client.menu().carried().map(ItemStack::count), Some(64));
}

#[test]
fn set_slot_correction_targets_one_slot() {
    let mut client = ClientMenu::new(Menu::generic(27));
    let recon = client.reconcile(ServerUpdate::SetSlot {
        state_id: 5,
        slot: 3,
        item: Some(stack("minecraft:diamond", 2)),
    });
    assert!(recon.corrected);
    assert_eq!(client.menu().slot_item(3).map(ItemStack::count), Some(2));
    assert_eq!(client.menu().state_id(), 5);
}

/// A predicted click must stamp **the server's** state id, never the locally
/// bumped one.
///
/// Vanilla's client sends `containerMenu.getStateId()` and never increments it
/// (`MultiPlayerGameMode.handleContainerInput`); only the server writes that
/// field. Our [`Menu::do_click`](lodestone_game::menu::Menu) *does* bump, because
/// it doubles as the server-side model, so reading the id back off the predicted
/// menu yields `server + 1` — which
/// `ServerGamePacketListenerImpl.handleContainerClick` reads as **stale** and
/// answers with `broadcastFullState()`. Every click would then be a 46-slot
/// resync that discards the prediction, and the reconcile seam could never
/// observe agreement.
#[test]
fn a_predicted_click_stamps_the_servers_state_id_not_the_bumped_one() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 8)));
    let mut client = ClientMenu::new(menu);
    // The server speaks first, as it always does (container_set_content on open).
    client.reconcile(ServerUpdate::SetContent {
        state_id: 17,
        items: vec![None; 63],
        carried: Some(stack("minecraft:stone", 8)),
    });

    let intent = client.predict(Click::left(0), PlayerCtx::survival());
    assert_eq!(
        intent.state_id, 17,
        "the click must carry the server's id; 18 means we sent the bumped one \
         and the server will full-resync"
    );
    // The local menu did bump (it models the server's own incrementStateId), so
    // reading the id off the menu is exactly the mistake this guards.
    assert_eq!(
        client.menu().state_id(),
        18,
        "the predicted menu still bumps, which is why the intent must not read it"
    );

    // Two clicks with no server reply in between both carry the same id, as
    // vanilla's do — the id is the server's, not a local sequence number.
    let second = client.predict(Click::left(1), PlayerCtx::survival());
    assert_eq!(second.state_id, 17);
}

#[test]
fn state_id_aligns_to_server_after_reconcile() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 4)));
    let mut client = ClientMenu::new(menu);
    client.predict(Click::left(0), PlayerCtx::survival());
    client.reconcile(ServerUpdate::SetSlot {
        state_id: 42,
        slot: 0,
        item: Some(stack("minecraft:stone", 4)),
    });
    assert_eq!(client.menu().state_id(), 42);
    assert_eq!(client.confirmed().state_id(), 42);
}
