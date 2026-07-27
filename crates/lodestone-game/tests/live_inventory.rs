//! Live **inventory round-trip through the real client stack**.
//!
//! `live_click.rs`, `live_container.rs` and `live_reconcile.rs` all prove the
//! click machine agrees with the server — but every one of them *hand-drives the
//! wire* using `lodestone-core`/`lodestone-net` plus locally-declared packet-id
//! constants, deliberately bypassing the v770 adapter and `ClientHandle`. That
//! is the §12.24 shape the brief names directly: a click machine proven against
//! the server, but never through the *real client*. This test closes that gap
//! for the **encodable** serverbound inventory actions.
//!
//! The whole path is exercised through the public client:
//!   1. `ClientBuilder::connect()` — the real transport + v770 adapter (resolved
//!      through the registry; `lodestone-game` still names no version crate).
//!   2. **Clientbound half.** Seed a known held item over RCON, then read it back
//!      by folding the real `ClientEvent::ContainerContent`/`ContainerSlot`
//!      stream into our version-free [`Menu`]. A misparse in the adapter's
//!      item/slot decode shows up as a wrong or missing stack here.
//!   3. **Serverbound half.** Send `ClientAction::DropSelectedItem` through
//!      `ClientHandle::send_action` — a real `player_action` encode — and require
//!      the server's *authoritative* post-drop content (again through the real
//!      stream) to match the click machine's prediction of the same drop.
//!
//! ## Why `DropSelectedItem` and not a container click
//!
//! 26.2's `container_click` encodes each slot as a `HashedStack` — a numeric
//! item-registry id plus a CRC32 hash of the component patch — which the
//! count-only canonical `ItemStack` cannot reproduce, so v770 returns
//! `Unsupported` for `ContainerClick` (and `SetCreativeModeSlot`). The 10
//! container-click types therefore *cannot* be driven through the real client
//! yet; that is a model+v770 blocker documented in the accompanying report. The
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
use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::item::ItemStack as GameItem;
use lodestone_game::menu::Menu;
use lodestone_model::ItemStack as ModelItem;
use lodestone_model::ClientAction;
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

fn count_at(w: &Window0, menu_index: usize) -> Option<u32> {
    w.slots.get(menu_index).and_then(|s| s.as_ref()).map(|s| s.count)
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
        .expect("v770 family compiled into the registry via lodestone-client/live-v770");

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
    let ready = poll_until(Duration::from_secs(30), Duration::from_millis(100), || async {
        handle
            .players()
            .into_iter()
            .find(|p| p.name.as_deref() == Some(user.as_str()))
    })
    .await;
    assert!(
        ready.is_some(),
        "player {user} never appeared in the live tab list — is lodestone-creative on :25570 in Play? (alive={})",
        handle.is_alive()
    );
    println!("player is in-game");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect RCON on 127.0.0.1:25571 (password 'lodestone') — is lodestone-creative up?");

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
    let seeded = poll_until(Duration::from_secs(20), Duration::from_millis(150), || async {
        let w = win.lock().unwrap();
        if !w.saw_content {
            return None;
        }
        match (name_at(&w, MAINHAND_MENU), count_at(&w, MAINHAND_MENU)) {
            (Some(name), Some(5)) if name == "minecraft:diamond" => Some(()),
            _ => None,
        }
    })
    .await;
    assert!(
        seeded.is_some(),
        "seeded diamond x5 never reached our Menu via the real ClientEvent stream within 20s \
         (alive={}, saw_content={})",
        handle.is_alive(),
        win.lock().unwrap().saw_content
    );
    println!("clientbound half: diamond x5 reached our Menu at slot {MAINHAND_MENU} via the real client");

    // Clientbound assertions.
    {
        let w = win.lock().unwrap();
        assert_eq!(
            name_at(&w, MAINHAND_MENU).as_deref(),
            Some("minecraft:diamond"),
            "held item name via real ContainerContent"
        );
        checked += 1;
        assert_eq!(count_at(&w, MAINHAND_MENU), Some(5), "held count via real stream");
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
    //  2. **The server does not echo client-initiated inventory mutations.** It
    //     trusts the client's own prediction and only sends *corrections*, so the
    //     drop produces no clientbound `ContainerSlot` (confirmed live: the seed
    //     arrives, the drop is silent). The clientbound stream is therefore the
    //     wrong oracle for our *own* action; the server's own NBT (read over RCON)
    //     is the authority. `live_click.rs` forces a resync with a stale-state_id
    //     `container_click`, which we cannot do here — that packet is `Unsupported`
    //     in v770 (HashedStack). So we observe server-truth directly.
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
    println!("serverbound half: server authoritative held count = {server_count:?}, matches prediction");

    const EXPECTED_CHECKS: usize = 7;
    assert!(
        checked >= EXPECTED_CHECKS,
        "anti-vacuity floor: only {checked} comparisons ran, expected >= {EXPECTED_CHECKS} — \
         an assertion was skipped, the gate is no longer proving what it claims"
    );

    // Best-effort cleanup (shared --rm server): clear the seeded slot.
    let _ = rcon
        .cmd(&format!("item replace entity {user} container.0 with minecraft:air"))
        .await;

    println!(
        "=== INVENTORY ORACLE PASSED: {checked} comparisons — seeded item reached our Menu via \
         the real ClientEvent stream, DropSelectedItem sent via ClientHandle::send_action, and \
         the server's authoritative 5->4 matched the click machine's prediction ==="
    );
    handle.shutdown();
    drain.abort();
}
