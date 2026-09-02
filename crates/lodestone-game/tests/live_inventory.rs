//! Live **inventory round-trip through the real client stack**.
//!
//! `live_click.rs`, `live_container.rs` and `live_reconcile.rs` all prove the
//! click machine agrees with the server — but every one of them *hand-drives the
//! wire* using `lodestone-core`/`lodestone-net` plus locally-declared packet-id
//! constants, deliberately bypassing the v26-2 adapter and `ClientHandle`. That
//! is the §12.24 shape the brief names directly: a click machine proven against
//! the server, but never through the *real client*. This test closes that gap
//! for the **encodable** serverbound inventory actions.
//!
//! The whole path is exercised through the public client:
//!   1. `ClientBuilder::connect()` — the real transport + v26-2 adapter (resolved
//!      through the registry; `lodestone-game` still names no version crate).
//!   2. **Clientbound half.** Seed a known held item over RCON, then read it back
//!      by folding the real `ClientEvent::ContainerContent`/`ContainerSlot`
//!      stream into our version-free [`Menu`]. A misparse in the adapter's
//!      item/slot decode shows up as a wrong or missing stack here.
//!   3. **Serverbound half.** Send `ClientAction::DropSelectedItem` through
//!      `ClientHandle::send_action` — a real `player_action` encode — and require
//!      the server's *authoritative* post-drop count (read over RCON) to match the
//!      click machine's prediction of the same drop.
//!
//! ## Two findings that shape the serverbound observation
//!
//! - **The server does not echo client-initiated inventory mutations.** It trusts
//!   the client's own prediction and only sends *corrections*, so the drop produces
//!   no clientbound `ContainerSlot`. The clientbound stream is therefore the wrong
//!   oracle for our *own* action — the server's own NBT, read over RCON, is the
//!   authority. (This is exactly why the hand-rolled tests force a resync with a
//!   stale-state_id `container_click`, which we cannot use here — `container_click`
//!   is `Unsupported` in v26-2.)
//! - **The load gate.** Player actions are dropped until `hasClientLoaded()`; the
//!   real driver never sends `player_loaded` and no `ClientAction` triggers it, so
//!   the server auto-loads us only after ~60 ticks (~3s). Every drop before that is
//!   a silent no-op, so we retry and stop on the first decrement.
//!
//! ## Why `DropSelectedItem` and not a container click
//!
//! 26.2's `container_click` encodes each slot as a `HashedStack` — a numeric
//! item-registry id plus a CRC32 hash of the component patch — which the
//! count-only canonical `ItemStack` cannot reproduce, so v26-2 returns
//! `Unsupported` for `ContainerClick` (and `SetCreativeModeSlot`). The 10
//! container-click types therefore *cannot* be driven through the real client
//! yet; that is a model+v26-2 blocker documented in the accompanying report. The
//! `player_action` drops carry **no item payload** (the server reads the held
//! slot itself), so they *are* encodable and give a genuine
//! predict→act→server-authority round trip through the real client today.
//!
//! ## Anti-vacuity
//!
//! - A negative control: a slot we never seeded stays empty through the whole
//!   run, so the tracker is proven to discriminate rather than echo.
//! - The mutation must be *observed*: the held count must transition 5 -> 4. If
//!   the action silently failed to encode/send, the count stays 5 and the poll
//!   times out with a loud panic — a no-op cannot pass.
//! - A `checked` floor (`>= EXPECTED_CHECKS`) bites if a future refactor drops
//!   assertions. Proven to bite by temporarily raising it.
//!
//! ## Run it
//!
//! ```text
//! cargo test -p lodestone-game --features live-inventory \
//!     --test live_inventory -- --ignored --nocapture
//! ```
#![cfg(feature = "live-inventory")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use lodestone_game::click::{Click, ContainerInput, PlayerCtx};
use lodestone_game::item::ItemStack as GameItem;
use lodestone_game::menu::Menu;
use lodestone_game::reconcile::{ClickIntent, ClientMenu, ServerUpdate};
use lodestone_model::ItemStack as ModelItem;
use lodestone_model::{ClientAction, ContainerClickType, ContainerSlotChange, ItemComponents};
use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25570;
const RCON_PORT: u16 = 25571;
const RCON_PASSWORD: &str = "lodestone";

/// Player-inventory (window 0) menu index of the first hotbar slot, which is the
/// main hand when the selected slot is 0. RCON `container.0` targets it.
const MAINHAND_MENU: usize = 36;
/// A slot we deliberately never touch — the negative control.
const UNTOUCHED_MENU: usize = 40;

/// The window-0 view we fold the real `ClientEvent` stream into. This is our
/// stand-in for a `ClientHandle::inventory()` accessor (which does not exist —
/// the client keeps no inventory read-model), built only from *public* events.
#[derive(Default)]
struct Window0 {
    slots: Vec<Option<ModelItem>>,
    carried: Option<ModelItem>,
    state_id: i32,
    saw_content: bool,
}

fn folded_menu(w: &Window0) -> Menu {
    let mut menu = Menu::player();
    let items: Vec<Option<GameItem>> = w
        .slots
        .iter()
        .map(|s| s.as_ref().map(model_to_game))
        .collect();
    menu.restore(&items);
    menu.set_carried(w.carried.as_ref().map(model_to_game));
    menu
}

fn model_to_game(m: &ModelItem) -> GameItem {
    // Plain `/give`/`/item replace` items have no component overrides, so a
    // component-free bridge is faithful here (asserted by the counts matching).
    GameItem::new(m.item.clone(), m.count as i32)
}

/// The reverse bridge: a version-free game stack lowered back into the model
/// stack a `ClientAction::ContainerClick` carries. Component-free for the same
/// reason `model_to_game` is.
fn game_to_model(g: &GameItem) -> ModelItem {
    ModelItem {
        item: g.item().clone(),
        count: g.count() as u32,
        components: ItemComponents::default(),
    }
}

/// `ContainerInput` (game) → `ContainerClickType` (model) — a 1:1 mapping the
/// adapter relies on when it lowers a predicted click onto the wire.
fn map_click_type(input: ContainerInput) -> ContainerClickType {
    match input {
        ContainerInput::Pickup => ContainerClickType::Pickup,
        ContainerInput::QuickMove => ContainerClickType::QuickMove,
        ContainerInput::Swap => ContainerClickType::Swap,
        ContainerInput::Clone => ContainerClickType::Clone,
        ContainerInput::Throw => ContainerClickType::Throw,
        ContainerInput::QuickCraft => ContainerClickType::QuickCraft,
        ContainerInput::PickupAll => ContainerClickType::PickupAll,
    }
}

/// Lower a predicted [`ClickIntent`] into the exact `ClientAction::ContainerClick`
/// the client sends. This is the seam the whole gate exercises: our version-free
/// prediction becomes a real serverbound packet through the real adapter.
fn intent_to_action(window_id: i32, intent: &ClickIntent) -> ClientAction {
    ClientAction::ContainerClick {
        window_id,
        state_id: intent.state_id as i32,
        slot: intent.slot,
        button: intent.button,
        click_type: map_click_type(intent.input),
        changed_slots: intent
            .changed_slots
            .iter()
            .map(|(slot, item)| ContainerSlotChange {
                slot: *slot as i32,
                item: item.as_ref().map(game_to_model),
            })
            .collect(),
        carried_item: intent.carried.as_ref().map(game_to_model),
    }
}

fn count_at(w: &Window0, menu_index: usize) -> Option<u32> {
    w.slots
        .get(menu_index)
        .and_then(|s| s.as_ref())
        .map(|s| s.count)
}

fn name_at(w: &Window0, menu_index: usize) -> Option<String> {
    w.slots
        .get(menu_index)
        .and_then(|s| s.as_ref())
        .map(|s| s.item.to_string())
}

/// The server's *own* count for one inventory slot, read over RCON. This is the
/// authority for our own client-initiated mutations, which the server does not
/// echo back over the clientbound stream. Returns `None` when the slot is empty
/// (the `data get` command then reports "Found no elements", which lacks the
/// success marker, so we never mis-parse a digit out of the error text).
async fn held_count_server_truth_slot(rcon: &mut Rcon, user: &str, slot: u8) -> Option<i64> {
    let resp = rcon
        .cmd(&format!(
            "data get entity {user} Inventory[{{Slot:{slot}b}}].count"
        ))
        .await;
    let marker = "entity data: ";
    let idx = resp.find(marker)?;
    let tail = resp[idx + marker.len()..].trim_start();
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<i64>().ok()
}

async fn held_count_server_truth(rcon: &mut Rcon, user: &str) -> Option<i64> {
    held_count_server_truth_slot(rcon, user, 0).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-creative server on 127.0.0.1:25570 (RCON :25571)"]
async fn inventory_mutation_round_trips_through_client() {
    println!("=== LIVE INVENTORY ROUND-TRIP (protocol 776, creative :25570) ===");

    let user = unique_username();
    println!("player = {user}");

    let server = ServerAddress {
        host: HOST.into(),
        port: PORT,
    };
    let profile = LoginProfile {
        username: user.clone(),
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v26-2 family compiled into the registry via lodestone-client/live-v26-2");

    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect(
            "connect to lodestone-creative on 127.0.0.1:25570 — start it with: \
             docker run --rm -d -p 25570:25570 -p 25571:25571 --name lodestone-creative <creative-image>",
        );

    // Fold the *real* client event stream into a window-0 view. Draining also
    // keeps the driver's bounded channel from backpressuring.
    let win: Arc<Mutex<Window0>> = Arc::new(Mutex::new(Window0::default()));
    let win_bg = Arc::clone(&win);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::ContainerContent {
                    window_id: 0,
                    state_id,
                    items,
                    carried_item,
                } => {
                    let mut w = win_bg.lock().unwrap();
                    w.slots = items;
                    w.carried = carried_item;
                    w.state_id = state_id;
                    w.saw_content = true;
                }
                ClientEvent::ContainerSlot {
                    window_id: 0,
                    state_id,
                    slot,
                    item,
                } => {
                    let mut w = win_bg.lock().unwrap();
                    if let Ok(idx) = usize::try_from(slot)
                        && idx < w.slots.len()
                    {
                        w.slots[idx] = item;
                        w.state_id = state_id;
                    }
                }
                ClientEvent::Disconnect { reason } => {
                    eprintln!("driver saw disconnect: {}", reason.to_plain_string());
                    break;
                }
                _ => {}
            }
        }
    });

    // Reach Play: the server must know our player before RCON commands target it.
    let ready = poll_until(
        Duration::from_secs(30),
        Duration::from_millis(100),
        || async {
            handle
                .players()
                .into_iter()
                .find(|p| p.name.as_deref() == Some(user.as_str()))
        },
    )
    .await;
    assert!(
        ready.is_some(),
        "player {user} never appeared in the live tab list — is lodestone-creative on :25570 in Play? (alive={})",
        handle.is_alive()
    );
    println!("player is in-game");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect(
            "connect RCON on 127.0.0.1:25571 (password 'lodestone') — is lodestone-creative up?",
        );

    let mut checked = 0usize;

    // --- Seed a known held stack and read it back through the real stream ---
    let seed = rcon
        .cmd(&format!(
            "item replace entity {user} container.0 with minecraft:diamond 5"
        ))
        .await;
    println!("  RCON seed container.0 -> {seed:?}");

    // Poll, never assert immediately: the seed is tick-published and arrives as a
    // separate packet through the real adapter.
    let seeded = poll_until(
        Duration::from_secs(20),
        Duration::from_millis(150),
        || async {
            let w = win.lock().unwrap();
            if !w.saw_content {
                return None;
            }
            match (name_at(&w, MAINHAND_MENU), count_at(&w, MAINHAND_MENU)) {
                (Some(name), Some(5)) if name == "minecraft:diamond" => Some(()),
                _ => None,
            }
        },
    )
    .await;
    assert!(
        seeded.is_some(),
        "seeded diamond x5 never reached our Menu via the real ClientEvent stream within 20s \
         (alive={}, saw_content={})",
        handle.is_alive(),
        win.lock().unwrap().saw_content
    );
    println!(
        "clientbound half: diamond x5 reached our Menu at slot {MAINHAND_MENU} via the real client"
    );

    // Clientbound assertions.
    {
        let w = win.lock().unwrap();
        assert_eq!(
            name_at(&w, MAINHAND_MENU).as_deref(),
            Some("minecraft:diamond"),
            "held item name via real ContainerContent"
        );
        checked += 1;
        assert_eq!(
            count_at(&w, MAINHAND_MENU),
            Some(5),
            "held count via real stream"
        );
        checked += 1;
        // Negative control: a slot we never seeded must be empty — proves the
        // tracker discriminates rather than echoing a stack into every slot.
        assert_eq!(
            count_at(&w, UNTOUCHED_MENU),
            None,
            "an untouched slot must be empty — proves the window view keys on slot index"
        );
        checked += 1;
    }

    // Cross-check the clientbound view against the server's own NBT: both must
    // agree the held count is 5 before we mutate. This also validates our
    // RCON count reader end to end before it becomes the serverbound oracle.
    let seed_truth = held_count_server_truth(&mut rcon, &user).await;
    assert_eq!(
        seed_truth,
        Some(5),
        "server-side held count must be 5 after seeding (agrees with the clientbound stream)"
    );
    checked += 1;

    // --- Predict the drop on our version-free model ---
    let predicted_count = {
        let w = win.lock().unwrap();
        let mut menu = folded_menu(&w);
        // `DropSelectedItem` (Q) removes one from the held slot; the click
        // machine's throw-drop-one on the same slot has the identical slot effect
        // (verified against vanilla `AbstractContainerMenu.doClick` THROW).
        Click::drop_one(MAINHAND_MENU).apply(&mut menu, PlayerCtx::survival());
        menu.slot_item(MAINHAND_MENU).map(|s| s.count() as u32)
    };
    assert_eq!(
        predicted_count,
        Some(4),
        "click machine predicts held stack 5 -> 4 after a drop-one"
    );
    checked += 1;

    // --- Serverbound half: send a real action through ClientHandle, observed
    //     via RCON server-truth ---
    //
    // Two hazards, both verified against 26.2 `ServerGamePacketListenerImpl`:
    //
    //  1. **Load gate.** Player actions are dropped until `hasClientLoaded()`
    //     (`clientLoadedTimeoutTimer <= 0`). The timer starts at
    //     `CLIENT_LOADED_TIMEOUT_TIME = 60` ticks and only decrements — the real
    //     `ClientHandle`/driver never sends `player_loaded` and no `ClientAction`
    //     triggers it, so the server auto-loads us only after ~60 ticks (~3s). A
    //     drop before that is silently discarded; hence we retry.
    //
    //     DO NOT "harmonise" this retry loop with the container-click test's: the
    //     two differ *deliberately*. `DropSelectedItem` lowers to
    //     `ServerboundPlayerActionPacket(DROP_ITEM)`, routed to `handlePlayerAction`
    //     which **is** `hasClientLoaded()`-gated (verified `:1810`), so it needs
    //     the ~3s retry. `handleContainerClick` (`:1940`) is **not** gated, so the
    //     click test lands on attempt 0. Same-looking loops, different reason —
    //     collapsing them reintroduces a silent flake on whichever side loses its
    //     gate handling.
    //
    //  2. **The server does not echo client-initiated inventory mutations.** It
    //     trusts the client's own prediction and only sends *corrections*, so the
    //     drop produces no clientbound `ContainerSlot` (confirmed live: the seed
    //     arrives, the drop is silent). The clientbound stream is therefore the
    //     wrong oracle for our *own* action; the server's own NBT (read over RCON)
    //     is the authority. (A real `ContainerClick` round-trip is exercised by
    //     the separate `container_click_pickup_round_trips_through_client` test;
    //     `DropSelectedItem` is a `handlePlayerAction`/`DROP_ITEM` packet, which
    //     *is* load-gated, hence the retry here.)
    //
    // The retry reads server-truth after each single send and stops at 4, so
    // exactly one decrement is asserted (not two).
    let mut server_count = None;
    for attempt in 0..15 {
        handle
            .send_action(ClientAction::DropSelectedItem)
            .expect("send DropSelectedItem through the real client");
        tokio::time::sleep(Duration::from_millis(1300)).await;
        let now = held_count_server_truth(&mut rcon, &user).await;
        println!("  drop attempt {attempt}: server-truth held count = {now:?}");
        match now {
            Some(5) => continue, // server not loaded yet — drop was a no-op
            other => {
                server_count = other;
                break;
            }
        }
    }
    println!("serverbound half: DropSelectedItem sent via ClientHandle::send_action");
    println!(
        "  DIAG pos={:?} health={:?} alive={}",
        handle.position(),
        handle.health(),
        handle.is_alive()
    );

    // The mutation must be *observed*: a silent no-op leaves server-truth at 5
    // forever and this fails loudly rather than passing vacuously.
    assert!(
        server_count.is_some() && server_count != Some(5),
        "server-side held count never changed from 5 after repeated DropSelectedItem — the action \
         did not take effect on the server through the real client (alive={}, count={server_count:?})",
        handle.is_alive()
    );
    // Server authority must equal our prediction. This is the anti-vacuity crux:
    // exactly one decrement, not two.
    assert_eq!(
        server_count,
        predicted_count.map(|c| c as i64),
        "server's authoritative held count must match the click machine's single-drop prediction"
    );
    checked += 1;
    // The negative control must *still* hold: a slot we never touched is empty
    // server-side too.
    let untouched = held_count_server_truth_slot(&mut rcon, &user, 4).await;
    assert_eq!(
        untouched, None,
        "an inventory slot we never seeded must be empty server-side after the drop"
    );
    checked += 1;
    println!(
        "serverbound half: server authoritative held count = {server_count:?}, matches prediction"
    );

    const EXPECTED_CHECKS: usize = 7;
    assert!(
        checked >= EXPECTED_CHECKS,
        "anti-vacuity floor: only {checked} comparisons ran, expected >= {EXPECTED_CHECKS} — \
         an assertion was skipped, the gate is no longer proving what it claims"
    );

    // Best-effort cleanup (shared --rm server): clear the seeded slot.
    let _ = rcon
        .cmd(&format!(
            "item replace entity {user} container.0 with minecraft:air"
        ))
        .await;

    println!(
        "=== INVENTORY ORACLE PASSED: {checked} comparisons — seeded item reached our Menu via \
         the real ClientEvent stream, DropSelectedItem sent via ClientHandle::send_action, and \
         the server's authoritative 5->4 matched the click machine's prediction ==="
    );
    handle.shutdown();
    drain.abort();
}

/// The definitive serverbound proof for the *container-click machine* (the seam
/// §12.24 kept flagging): a real `ClientAction::ContainerClick`, built by lowering
/// the click machine's own [`ClickIntent`] prediction, sent through the real
/// `ClientHandle`, and confirmed against the server's authoritative NBT.
///
/// This is a strictly stronger gate than `DropSelectedItem` above: it exercises
/// the `ContainerClick` encode path (the v26-2 `HashedStack` + `state_id` seam that
/// was `Unsupported` until impl-v26-2 landed it), not just a player-action drop.
///
/// What it proves: a left-click Pickup on the seeded hotbar slot moves the whole
/// stack into the cursor, so the server empties `Inventory[Slot:0]`, matching the
/// click machine's prediction (slot → empty, cursor → the stack). What it does
/// **not** prove: the cursor contents server-side — the carried stack lives in
/// vanilla's own carried-item getter, not entity NBT, so RCON cannot read it.
/// The slot-emptied assertion plus the untouched-slot control is what is
/// observable, and it is exactly the authoritative half a stubbed encoder cannot
/// fake.
///
/// Unlike the drop test, `handleContainerClick` is **not** gated on
/// `hasClientLoaded()` (verified in 26.2 `ServerGamePacketListenerImpl:1940`), so
/// no ~3s load wait is needed; the short retry only tolerates tick/RCON latency.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-creative server on 127.0.0.1:25570 (RCON :25571)"]
async fn container_click_pickup_round_trips_through_client() {
    println!("=== LIVE CONTAINER-CLICK ROUND-TRIP (protocol 776, creative :25570) ===");

    let user = unique_username();
    println!("player = {user}");

    let server = ServerAddress {
        host: HOST.into(),
        port: PORT,
    };
    let profile = LoginProfile {
        username: user.clone(),
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v26-2 family compiled into the registry via lodestone-client/live-v26-2");

    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to lodestone-creative on 127.0.0.1:25570");

    let win: Arc<Mutex<Window0>> = Arc::new(Mutex::new(Window0::default()));
    let win_bg = Arc::clone(&win);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::ContainerContent {
                    window_id: 0,
                    state_id,
                    items,
                    carried_item,
                } => {
                    let mut w = win_bg.lock().unwrap();
                    w.slots = items;
                    w.carried = carried_item;
                    w.state_id = state_id;
                    w.saw_content = true;
                }
                ClientEvent::ContainerSlot {
                    window_id: 0,
                    state_id,
                    slot,
                    item,
                } => {
                    let mut w = win_bg.lock().unwrap();
                    if let Ok(idx) = usize::try_from(slot)
                        && idx < w.slots.len()
                    {
                        w.slots[idx] = item;
                        w.state_id = state_id;
                    }
                }
                ClientEvent::Disconnect { reason } => {
                    eprintln!("driver saw disconnect: {}", reason.to_plain_string());
                    break;
                }
                _ => {}
            }
        }
    });

    let ready = poll_until(
        Duration::from_secs(30),
        Duration::from_millis(100),
        || async {
            handle
                .players()
                .into_iter()
                .find(|p| p.name.as_deref() == Some(user.as_str()))
        },
    )
    .await;
    assert!(
        ready.is_some(),
        "player {user} never appeared in the live tab list (alive={})",
        handle.is_alive()
    );
    println!("player is in-game");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect RCON on 127.0.0.1:25571 (password 'lodestone')");

    let mut checked = 0usize;

    // Seed a known stack in the hotbar slot the click will pick up.
    let seed = rcon
        .cmd(&format!(
            "item replace entity {user} container.0 with minecraft:diamond 5"
        ))
        .await;
    println!("  RCON seed container.0 -> {seed:?}");

    let seeded = poll_until(
        Duration::from_secs(20),
        Duration::from_millis(150),
        || async {
            let w = win.lock().unwrap();
            if !w.saw_content {
                return None;
            }
            match (name_at(&w, MAINHAND_MENU), count_at(&w, MAINHAND_MENU)) {
                (Some(name), Some(5)) if name == "minecraft:diamond" => Some(()),
                _ => None,
            }
        },
    )
    .await;
    assert!(
        seeded.is_some(),
        "seeded diamond x5 never reached our Menu via the real stream (alive={})",
        handle.is_alive()
    );
    println!("clientbound: diamond x5 reached slot {MAINHAND_MENU} via the real client");

    // Server-truth baseline before the click.
    let seed_truth = held_count_server_truth(&mut rcon, &user).await;
    assert_eq!(
        seed_truth,
        Some(5),
        "server-side held count must be 5 after seeding"
    );
    checked += 1;

    // --- Predict a left-click Pickup on our version-free model, capturing the
    //     server-synchronised state_id from the real stream. ---
    let intent = {
        let w = win.lock().unwrap();
        // Seed the client menu from the real server content + its state_id, so the
        // packet carries a state_id the server recognises (matching → no rollback).
        let items: Vec<Option<GameItem>> = w
            .slots
            .iter()
            .map(|s| s.as_ref().map(model_to_game))
            .collect();
        let mut menu = ClientMenu::new(Menu::player());
        menu.reconcile(ServerUpdate::SetContent {
            state_id: w.state_id.max(0) as u32,
            items,
            carried: w.carried.as_ref().map(model_to_game),
        });
        menu.predict(Click::left(MAINHAND_MENU), PlayerCtx::survival())
    };

    // The prediction itself is a checkable claim: pickup empties the slot and
    // fills the cursor with the whole stack.
    let predicts_slot_emptied = intent
        .changed_slots
        .iter()
        .any(|(s, item)| *s as usize == MAINHAND_MENU && item.is_none());
    assert!(
        predicts_slot_emptied,
        "click machine must predict slot {MAINHAND_MENU} emptied by a left-click pickup, \
         got changed_slots={:?}",
        intent.changed_slots
    );
    checked += 1;
    assert_eq!(
        intent.carried.as_ref().map(|s| s.count()),
        Some(5),
        "click machine must predict the cursor holding the whole diamond x5 after pickup"
    );
    checked += 1;
    assert_eq!(
        intent.slot, MAINHAND_MENU as i32,
        "intent targets the clicked slot"
    );
    checked += 1;

    let action = intent_to_action(0, &intent);

    // --- Send the real ContainerClick and confirm via server-truth ---
    //
    // `handleContainerClick` is not load-gated, so this is usually a no-op retry;
    // the loop only absorbs tick/RCON latency and a first-send that raced the
    // server settling our state_id.
    let mut after = seed_truth;
    for attempt in 0..8 {
        handle
            .send_action(action.clone())
            .expect("send ContainerClick through the real client");
        tokio::time::sleep(Duration::from_millis(600)).await;
        let now = held_count_server_truth(&mut rcon, &user).await;
        println!("  click attempt {attempt}: server-truth slot0 count = {now:?}");
        if now != Some(5) {
            after = now;
            break;
        }
    }
    println!(
        "serverbound: ContainerClick(Pickup, slot {MAINHAND_MENU}) sent via ClientHandle::send_action"
    );

    // The authoritative result: a pickup moves the stack to the cursor, so the
    // slot is empty server-side. A stubbed/no-op encoder would leave it at 5 and
    // this fails loudly rather than passing vacuously.
    assert_eq!(
        after,
        None,
        "after a left-click pickup the server must report slot 0 empty (stack moved to cursor); \
         got {after:?} (alive={})",
        handle.is_alive()
    );
    checked += 1;

    // Negative control: a slot we never seeded or clicked must remain empty — the
    // pickup did not spray the stack elsewhere, and the detector discriminates.
    let untouched = held_count_server_truth_slot(&mut rcon, &user, 4).await;
    assert_eq!(
        untouched, None,
        "an inventory slot we never touched must be empty server-side after the pickup"
    );
    checked += 1;

    const EXPECTED_CHECKS: usize = 6;
    assert!(
        checked >= EXPECTED_CHECKS,
        "anti-vacuity floor: only {checked} comparisons ran, expected >= {EXPECTED_CHECKS}"
    );

    // Best-effort cleanup (shared --rm server).
    let _ = rcon
        .cmd(&format!(
            "item replace entity {user} container.0 with minecraft:air"
        ))
        .await;

    println!(
        "=== CONTAINER-CLICK ORACLE PASSED: {checked} comparisons — a real ContainerClick built \
         from the click machine's prediction went out through ClientHandle::send_action and the \
         server authoritatively emptied the picked-up slot ==="
    );
    handle.shutdown();
    drain.abort();
}
