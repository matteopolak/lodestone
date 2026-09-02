//! Live **crafting** oracle: open a real crafting table through the real client,
//! move real planks into its grid with real `container_click` packets, and let the
//! **server** say what comes out.
//!
//! Against the SURVIVAL oracle (`lodestone-survival`, game :25565, RCON :25566).
//!
//! ## Why this test exists in this shape
//!
//! Everything under crafting was built as an island: 1585 recipes, 224 item tags,
//! a `CraftingMenu` layout, `Menu::craft_layout`, a container screen that lays out
//! the grid. None of it had ever been driven against a server, and a hermetic test
//! of a click machine cannot close that gap — a predictor asserted against its own
//! prediction agrees with itself by construction.
//!
//! The load-bearing design rule is that **the client never matches a recipe to
//! fill the result slot**. Vanilla computes the result server-side:
//! `CraftingMenu.slotsChanged` runs `slotChangedCraftingGrid`, which sends a
//! `container_set_slot` for slot 0 — and a *client's* `CraftingMenu` is built with
//! a null level access, so its `slotsChanged` does nothing at all. Our
//! `RecipeBook`/`predicted_craft_result` exist for the recipe book, ghost
//! previews and offline play, and are deliberately not used here. Every
//! result-slot assertion below therefore reads a value that **originated on the
//! server**: either a `ClientEvent::ContainerSlot` push we recorded raw, or the
//! server's own `broadcastFullState()` content packet.
//!
//! ## What it gates
//!
//! 1. **Layout, against server truth.** The server's own 46-slot content packet is
//!    read with distinctive items seeded into three different regions
//!    (`container.0` hotbar, `container.7` hotbar, `container.9` main storage) and
//!    asserted to land at menu indices 37, 44 and 10. A constant-offset
//!    transposition — the trap in `MenuKind`, where a `Generic { n }` has no
//!    armour, no off-hand and a hotbar that is *not* at 36 — draws a plausible,
//!    wrongly-transposed inventory and would be caught here rather than mistaken
//!    for an art bug.
//! 2. **The result slot is take-only, live.** A cursor holding 8 planks is
//!    left-clicked onto slot 0; the server's full state still reports slot 0 empty
//!    and the cursor loaded. Its **control** is the very next step: the same
//!    cursor deposits fine into the grid cells, so the detector is proven to be
//!    able to observe a successful placement.
//! 3. **The server fills the result.** Three planks in an L (cells 1, 2, 4) match
//!    no recipe and the server leaves slot 0 empty — this is the **negative
//!    control, observed firing**, not described. The fourth plank completes the
//!    2×2 and the server pushes `container_set_slot(slot 0) = crafting_table`. The
//!    same detector reports "no result" and then "result" one click apart, and the
//!    click that produced it is asserted to carry **no** slot-0 change of our own.
//! 4. **Taking the result consumes the ingredients** — `ResultSlot.onTake`, and
//!    the reason "slot 0 is take-only" is only half the rule.
//! 5. **Repeat crafting is the server's, not ours.** With 2 planks per cell one
//!    shift-click of the result must yield **two** crafting tables. Our prediction
//!    says one — vanilla's client predicts one too, because its result slot never
//!    refills — and the divergence is asserted in both directions: the intent we
//!    sent carries a count of 1, and the server's authoritative state carries 2.
//!    That is the port of `AbstractContainerMenu.doClick`'s QUICK_MOVE loop
//!    behaving exactly as it does in vanilla on each side of the wire.
//! 6. **An independent server oracle.** `/clear <player> <item> 0` counts without
//!    removing, on a completely different channel from the container packets: the
//!    player must end up with exactly 2 crafting tables and 0 planks (2 crafts ×
//!    4 planks = all 8 consumed).
//!
//! ## Mechanics worth knowing before editing this
//!
//! - **The resync probe is `CLONE`, not `-999 PICKUP`.** The other live container
//!   tests force `broadcastFullState()` with a stale-`state_id` click on slot
//!   `-999`, which is a guaranteed no-op *only with an empty cursor* — with a
//!   loaded one it **drops the cursor into the world**. Half the checks here run
//!   with a loaded cursor, so the probe is mode `CLONE`, whose server branch is
//!   gated on `player.hasInfiniteMaterials()` and therefore a no-op for every slot
//!   and every cursor in survival.
//! - **`stillValid` keeps the table in reach.** `CraftingMenu.stillValid` re-checks
//!   the block is a crafting table *and* `canInteractWithBlock(pos, 4.0)` on every
//!   click, so the table is placed adjacent to the player and the player never
//!   moves.
//! - **`handleUseItemOn` is gated on `hasClientLoaded()`** and the real driver
//!   never sends `player_loaded`, so the server auto-loads us only after ~60 ticks.
//!   Opening the table is retried. `handleContainerClick` has **no** such gate.
//! - **Clicks carry the server's `state_id`.** If they carried a locally bumped
//!   one, every click would arrive stale and the server would answer
//!   `broadcastFullState()` — which would make this whole test vacuous, since the
//!   server's reply would then unconditionally be its own truth rather than a
//!   correction to a prediction. See `reconcile.rs` and the hermetic guard
//!   `a_predicted_click_stamps_the_servers_state_id_not_the_bumped_one`.
//!
//! ## Run it
//!
//! ```text
//! cargo test -p lodestone-game --features live-craft \
//!     --test live_craft -- --ignored --nocapture
//! ```
#![cfg(feature = "live-craft")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestone_client::{ClientBuilder, ClientEvent, ClientHandle, LoginProfile, ServerAddress};
use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::item::ItemStack as GameItem;
use lodestone_game::menu::CraftLayout;
use lodestone_game::menus::Menus;
use lodestone_game::reconcile::ClickIntent;
use lodestone_model::math::{BlockPos, Vec3, Vec3f};
use lodestone_model::{
    BlockFace, ClientAction, ContainerClickType, Hand, ItemStack as ModelItem,
};
use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const RCON_PORT: u16 = 25566;
const RCON_PASSWORD: &str = "lodestone";

/// Independent live assertions this gate must reach. An early return that skipped
/// one would otherwise pass silently.
const EXPECTED_CHECKS: usize = 8;

/// `CraftingMenu`: `0` result, `1..=9` grid, `10..=36` main storage, `37..=45`
/// hotbar. 46 slots total, and **no armour and no off-hand** — a crafting table is
/// a `Generic { container_size: 10 }`, not a player menu, so its hotbar starts at
/// 37 and not at 36.
const CRAFT_SLOTS: usize = 46;
const RESULT: usize = 0;
/// The four grid cells of a 2×2 arrangement inside the 3×3 grid (top-left block).
const CELLS_2X2: [usize; 4] = [1, 2, 4, 5];
/// Menu index of hotbar slot 0 (`/item replace … container.0`).
const HOTBAR_0: usize = 37;
/// Menu index of hotbar slot 7 (`container.7`) — a layout probe.
const HOTBAR_7: usize = 44;
/// Menu index of the **last** hotbar slot, where `CraftingMenu.quickMoveStack`
/// fills first because it moves into `10..46` *backwards*.
const HOTBAR_8: usize = 45;
/// Menu index of main-storage slot 0 (`container.9`) — a layout probe. In a
/// crafting menu main storage begins at 10, immediately after the container's ten
/// slots.
const MAIN_0: usize = 10;

const PLANKS: &str = "minecraft:oak_planks";
const TABLE: &str = "minecraft:crafting_table";
/// Distinctive layout probes: nothing here is craftable with planks, so they can
/// sit in the inventory for the whole run without perturbing a recipe.
const PROBE_MAIN: &str = "minecraft:diamond";
const PROBE_HOTBAR: &str = "minecraft:emerald";

/// A `state_id` the server can never be holding, so `handleContainerClick` takes
/// its `broadcastFullState()` branch. Vanilla ids live in `0..=32767`.
const STALE_STATE_ID: i32 = 30_000;

/// How long to let a click round-trip before reading state.
const SETTLE: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// The shared fold of the real event stream.
// ---------------------------------------------------------------------------

/// One raw server-authored `container_set_slot`. Recorded verbatim so a
/// result-slot assertion can be traced to a packet the *server* sent, rather than
/// to a value that might have come from our own prediction.
#[derive(Debug, Clone)]
struct SlotPush {
    window: i32,
    slot: i32,
    item: Option<ModelItem>,
}

/// The server's last full-state broadcast.
#[derive(Debug, Clone)]
struct FullState {
    window: i32,
    items: Vec<Option<ModelItem>>,
    carried: Option<ModelItem>,
}

struct Session {
    /// The subject: the real `Menus` session, folded from real `ClientEvent`s and
    /// used to predict every click we send.
    menus: Menus,
    opened_windows: Vec<i32>,
    slot_pushes: Vec<SlotPush>,
    full_state: Option<FullState>,
    /// Bumped on every `container_set_content`, so a resync can be *awaited*
    /// rather than slept for.
    content_generation: u64,
    disconnected: Option<String>,
}

impl Session {
    fn new() -> Self {
        Self {
            menus: Menus::new(),
            opened_windows: Vec::new(),
            slot_pushes: Vec::new(),
            full_state: None,
            content_generation: 0,
            disconnected: None,
        }
    }
}

type Shared = Arc<Mutex<Session>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Session> {
    shared.lock().expect("session mutex")
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn block_center(pos: BlockPos) -> Vec3 {
    Vec3 {
        x: f64::from(pos.x) + 0.5,
        y: f64::from(pos.y) + 0.5,
        z: f64::from(pos.z) + 0.5,
    }
}

/// `(id, count)` of a model stack, for readable assertion messages.
fn named(item: Option<&ModelItem>) -> Option<(String, u32)> {
    item.map(|i| (i.item.to_string(), i.count))
}

fn expect_item(name: &str, count: u32) -> Option<(String, u32)> {
    Some((name.to_owned(), count))
}

/// `(id, count)` of one of *our* predicted stacks, in the same shape.
fn named_game(item: Option<&GameItem>) -> Option<(String, u32)> {
    item.map(|i| (i.item().to_string(), i.count().max(0) as u32))
}

/// The server's own count of an item on a player, read with `/clear … 0` (count
/// mode: reports without removing). A totally separate channel from the container
/// packets, so it cannot echo them.
///
/// Vanilla throws `clear.failed.single` when nothing matches, which RCON surfaces
/// as the "No items were found" text rather than a count.
async fn count_items(rcon: &mut Rcon, user: &str, item: &str) -> u32 {
    let resp = rcon.cmd(&format!("clear {user} {item} 0")).await;
    if resp.contains("No items were found") {
        return 0;
    }
    let after_found = resp
        .split_once("Found ")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("cannot read a count out of `clear {user} {item} 0` -> {resp:?}"));
    after_found
        .split_whitespace()
        .next()
        .and_then(|tok| tok.parse::<u32>().ok())
        .unwrap_or_else(|| panic!("cannot parse the count in {resp:?}"))
}

/// Predicts `click` on the active menu and puts the resulting `container_click` on
/// the wire, through `Menus::click` + `ClickIntent::to_action` — the real
/// serverbound path, not a hand-built packet.
async fn send_click(shared: &Shared, handle: &ClientHandle, click: Click) -> (i32, ClickIntent) {
    let (window, intent) = {
        let mut session = lock(shared);
        session.menus.click(click, PlayerCtx::survival())
    };
    handle
        .send_action(intent.to_action(window))
        .expect("send container_click through the real client");
    tokio::time::sleep(SETTLE).await;
    (window, intent)
}

/// Forces the server to re-broadcast its authoritative full menu state and returns
/// it.
///
/// The probe is a stale-`state_id` **`CLONE`** click: in survival
/// `AbstractContainerMenu.doClick`'s clone branch is gated on
/// `player.hasInfiniteMaterials()`, so no branch matches and nothing mutates —
/// unlike the `-999 PICKUP` probe the other live tests use, which throws a loaded
/// cursor into the world. The stale id makes `handleContainerClick` take
/// `broadcastFullState()`.
async fn server_full_state(shared: &Shared, handle: &ClientHandle, window: i32) -> FullState {
    let before = lock(shared).content_generation;
    handle
        .send_action(ClientAction::ContainerClick {
            window_id: window,
            state_id: STALE_STATE_ID,
            slot: 0,
            button: 0,
            click_type: ContainerClickType::Clone,
            changed_slots: Vec::new(),
            carried_item: None,
        })
        .expect("send the resync probe");
    let state = poll_until(
        Duration::from_secs(10),
        Duration::from_millis(100),
        || async {
            let session = lock(shared);
            if session.content_generation > before {
                session.full_state.clone()
            } else {
                None
            }
        },
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "the server never re-broadcast full state for window {window} (disconnected: {:?})",
            lock(shared).disconnected
        )
    });
    // The server also broadcasts window 0 (the player's own menu) in some
    // situations; reading one of those as the crafting menu's state would compare
    // a 46-slot *player* layout against a 46-slot *crafting* layout — same length,
    // different meaning, and every armour/off-hand slot silently shifted.
    assert_eq!(
        state.window, window,
        "the full state we read belongs to window {} , not the open crafting table",
        state.window
    );
    state
}

/// Every server-authored `container_set_slot` for `slot` on `window`, newest last.
fn pushes_for(shared: &Shared, window: i32, slot: usize) -> Vec<Option<ModelItem>> {
    lock(shared)
        .slot_pushes
        .iter()
        .filter(|p| p.window == window && usize::try_from(p.slot) == Ok(slot))
        .map(|p| p.item.clone())
        .collect()
}

/// Asserts the server's full state agrees with our reconciled menu, slot by slot
/// and cursor. This is the anti-transposition assertion: a menu-order/native-order
/// mixup shows up as two swapped slots rather than as a missing item.
fn assert_menu_matches_server(label: &str, shared: &Shared, server: &FullState) {
    let session = lock(shared);
    let menu = session
        .menus
        .opened()
        .expect("the crafting menu is still open");
    let mut diffs = Vec::new();
    for i in 0..CRAFT_SLOTS {
        let ours = named_game(menu.slot_item(i));
        let theirs = named(server.items.get(i).and_then(Option::as_ref));
        if ours != theirs {
            diffs.push(format!("  slot {i}: ours {ours:?} != server {theirs:?}"));
        }
    }
    let our_cursor = named_game(menu.carried());
    let their_cursor = named(server.carried.as_ref());
    if our_cursor != their_cursor {
        diffs.push(format!(
            "  cursor: ours {our_cursor:?} != server {their_cursor:?}"
        ));
    }
    assert!(
        diffs.is_empty(),
        "[{label}] our menu diverged from the server's own full state:\n{}",
        diffs.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The gate.
// ---------------------------------------------------------------------------

/// Hard deadline for the whole scenario.
///
/// A live gate that **hangs** is worse than one that fails: it reports nothing,
/// prints nothing (libtest block-buffers stdout when it is not a terminal) and
/// holds the shared oracle. Every wait inside [`run_gate`] is individually
/// bounded, but two are outside our control — the driver handshake and the RCON
/// client — and with several agents sharing one survival server those are exactly
/// the calls that stall. Observed: a run that sat for 5 minutes with zero output
/// while three other test binaries hammered the same RCON port.
const DEADLINE: Duration = Duration::from_secs(240);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565 (RCON :25566)"]
async fn crafting_round_trips_through_a_real_server() {
    if tokio::time::timeout(DEADLINE, run_gate()).await.is_err() {
        panic!(
            "the live crafting gate exceeded its {}s deadline — the oracle is wedged or contended \
             (another agent's live test may be holding :{RCON_PORT}); rerun alone",
            DEADLINE.as_secs()
        );
    }
}

async fn run_gate() {
    println!("=== LIVE CRAFTING (protocol 776, survival :{PORT}) ===");

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lodestone_client=info")),
        )
        .with_test_writer()
        .try_init();

    // Offline mode derives the account UUID from the *username*, so two runs
    // sharing a name share one persisted player file — and an inherited dead
    // player is held on the death screen, which sends no chunks.
    let user = unique_username();
    println!("player = {user}");

    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v26-2 family compiled into the registry via lodestone-client/live-v26-2");
    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress {
            host: HOST.into(),
            port: PORT,
        },
        LoginProfile {
            username: user.clone(),
            uuid: Uuid::new_v4(),
        },
        adapter,
    )
    // Bounded, so a wedged or paused oracle fails instead of hanging forever.
    .connect_timeout(Some(Duration::from_secs(20)))
    .connect()
    .await
    .expect(
        "connect to lodestone-survival on 127.0.0.1:25565 — recreate it with \
         ./scripts/live-oracles/survival.sh",
    );

    // Fold the real event stream into the real `Menus`, and record the raw
    // container pushes alongside it so every server-sourced claim is traceable.
    let shared: Shared = Arc::new(Mutex::new(Session::new()));
    let writer = Arc::clone(&shared);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let mut session = lock(&writer);
            match &event {
                ClientEvent::ScreenOpened { window_id, .. } => {
                    session.opened_windows.push(*window_id);
                }
                ClientEvent::ContainerSlot {
                    window_id,
                    slot,
                    item,
                    ..
                } => session.slot_pushes.push(SlotPush {
                    window: *window_id,
                    slot: *slot,
                    item: item.clone(),
                }),
                ClientEvent::ContainerContent {
                    window_id,
                    items,
                    carried_item,
                    ..
                } => {
                    session.full_state = Some(FullState {
                        window: *window_id,
                        items: items.clone(),
                        carried: carried_item.clone(),
                    });
                    session.content_generation += 1;
                }
                ClientEvent::Disconnect { reason } => {
                    let text = reason.to_plain_string();
                    eprintln!("!!! driver saw Disconnect: {text}");
                    session.disconnected = Some(text);
                    break;
                }
                _ => {}
            }
            // The subject under test: the version-free menu session.
            session.menus.apply(&event);
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
        "player {user} never appeared in the live tab list — is lodestone-survival on :{PORT} in \
         Play? (alive={})",
        handle.is_alive()
    );
    println!("player is in-game");

    // Vanilla's RCON server handles one client at a time and `AsyncRconClient::cmd`
    // has no timeout of its own, so a contended oracle can block here indefinitely.
    let mut rcon = tokio::time::timeout(
        Duration::from_secs(20),
        Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD),
    )
    .await
    .expect("RCON connect timed out — another live test is probably holding the oracle")
    .expect("connect RCON on 127.0.0.1:25566 (password 'lodestone') — is lodestone-survival up?");

    // op (bypass spawn protection) and keep the player alive: a death teleports it
    // to spawn and holds it on the death screen, which strands every later click.
    let _ = rcon.cmd(&format!("op {user}")).await;
    let _ = rcon.cmd(&format!("gamemode survival {user}")).await;
    for eff in [
        "minecraft:resistance 999999 255 true",
        "minecraft:regeneration 999999 9 true",
        "minecraft:fire_resistance 999999 0 true",
        "minecraft:saturation 999999 9 true",
    ] {
        let _ = rcon.cmd(&format!("effect give {user} {eff}")).await;
    }

    let pos = poll_until(
        Duration::from_secs(15),
        Duration::from_millis(200),
        || async { handle.position() },
    )
    .await
    .expect("client never reported a position");
    #[allow(clippy::cast_possible_truncation)]
    let (bx, by, bz) = (
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    );
    println!("  player feet block = ({bx}, {by}, {bz})");

    // Adjacent, so `CraftingMenu.stillValid`'s `canInteractWithBlock(pos, 4.0)`
    // keeps holding for every click of the run.
    let table = BlockPos::new(bx + 1, by, bz);
    let _ = rcon
        .cmd(&format!(
            "setblock {} {} {} minecraft:crafting_table",
            table.x, table.y, table.z
        ))
        .await;

    // Seed before opening, so the very first `container_set_content` already
    // carries the layout probes. 8 planks = exactly two 2x2 crafts.
    let _ = rcon.cmd(&format!("clear {user}")).await;
    for (container_slot, item, count) in [(0, PLANKS, 8), (7, PROBE_HOTBAR, 1), (9, PROBE_MAIN, 1)] {
        let _ = rcon
            .cmd(&format!(
                "item replace entity {user} container.{container_slot} with {item} {count}"
            ))
            .await;
    }

    let mut checks = 0usize;

    // ---------------------------------------------------------------------
    // Open the table by interacting with the real block.
    // ---------------------------------------------------------------------
    let window = {
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut found = None;
        while Instant::now() < deadline && found.is_none() {
            let _ = handle.look_at(block_center(table));
            let _ = handle.send_action(ClientAction::UseItemOn {
                hand: Hand::Main,
                pos: table,
                face: BlockFace::Up,
                cursor: Vec3f::new(0.5, 1.0, 0.5),
                inside_block: false,
                sequence: 0,
            });
            tokio::time::sleep(Duration::from_millis(500)).await;
            // Wait for the *content* too: the menu is not built until the server's
            // content packet says how big it is.
            let session = lock(&shared);
            if let Some(&wid) = session.opened_windows.last()
                && session.menus.opened_window_id() == Some(wid)
            {
                found = Some(wid);
            }
        }
        assert!(
            found.is_some(),
            "the crafting table never opened — no ScreenOpened + content pair arrived \
             (alive={}, disconnected={:?})",
            handle.is_alive(),
            lock(&shared).disconnected
        );
        // The retry loop can have a second `use_item_on` in flight when the first
        // one lands, and each open assigns a **new** container id. A click sent to
        // a superseded id is silently dropped by the server
        // (`containerMenu.containerId == packet.containerId()`), which looks
        // exactly like a broken click machine. Settle, then take whichever window
        // the latest content packet built.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        lock(&shared)
            .menus
            .opened_window_id()
            .expect("a window was open a moment ago")
    };
    assert_ne!(
        window, 0,
        "an opened container must not reuse the player-inventory window id"
    );
    println!("opened the crafting table: window {window}");

    // ---------------------------------------------------------------------
    // Check 1: the menu the server described is a 3x3 crafting menu.
    //
    // Size comes from the server's own content length (`items.len() - 36`), never
    // a hand-written type->size table.
    // ---------------------------------------------------------------------
    {
        let session = lock(&shared);
        assert_eq!(
            session
                .menus
                .opened_menu_type()
                .map(std::string::ToString::to_string)
                .as_deref(),
            Some("minecraft:crafting"),
            "the server must advertise a crafting menu"
        );
        let menu = session.menus.opened().expect("open");
        assert_eq!(
            menu.slot_count(),
            CRAFT_SLOTS,
            "a crafting table is 10 container slots + 36 player slots"
        );
        assert_eq!(
            menu.craft_layout(),
            Some(CraftLayout {
                result_slot: 0,
                first_input: 1,
                width: 3,
                height: 3,
            }),
            "the 3x3 grid and its result slot must be where CraftingMenu puts them"
        );
    }
    checks += 1;
    println!("check 1 OK: server-described menu is a 3x3 crafting menu with 46 slots");

    // ---------------------------------------------------------------------
    // Check 2 (anti-transposition): the server's own content packet puts our
    // three seeded items at menu indices 37, 44 and 10.
    //
    // `/item replace entity … container.N` addresses the player inventory in
    // NATIVE order; the content packet is in MENU order. In a crafting menu the
    // hotbar is at 37..=45 and main storage at 10..=36 — there is no armour and no
    // off-hand, so the player-menu offsets (36 / 9) are wrong by one here. A
    // constant offset would still draw a full, plausible inventory.
    // ---------------------------------------------------------------------
    let seeded = poll_until(
        Duration::from_secs(15),
        Duration::from_millis(250),
        || async {
            let state = server_full_state(&shared, &handle, window).await;
            let planks_here = named(state.items.get(HOTBAR_0).and_then(Option::as_ref))
                == expect_item(PLANKS, 8);
            planks_here.then_some(state)
        },
    )
    .await
    .expect("the seeded planks never showed up at menu slot 37 in the server's own content packet");
    assert_eq!(
        named(seeded.items.get(HOTBAR_0).and_then(Option::as_ref)),
        expect_item(PLANKS, 8),
        "native container.0 (hotbar 0) is menu slot 37 in a crafting menu, not 36"
    );
    assert_eq!(
        named(seeded.items.get(HOTBAR_7).and_then(Option::as_ref)),
        expect_item(PROBE_HOTBAR, 1),
        "native container.7 (hotbar 7) is menu slot 44"
    );
    assert_eq!(
        named(seeded.items.get(MAIN_0).and_then(Option::as_ref)),
        expect_item(PROBE_MAIN, 1),
        "native container.9 (main storage 0) is menu slot 10"
    );
    assert_menu_matches_server("seeded", &shared, &seeded);
    checks += 1;
    println!("check 2 OK: menu-order layout agrees with the server's own 46-slot content packet");

    // ---------------------------------------------------------------------
    // Check 3: the result slot refuses a placement, live.
    //
    // Pick the planks up, then try to dump all 8 into slot 0. `ResultSlot.mayPlace`
    // is `false`, so `safeInsert` returns the cursor untouched. Its control is
    // check 4, which places from the *same* cursor into the grid and succeeds — so
    // "nothing happened" is not the detector being blind.
    // ---------------------------------------------------------------------
    let (_, pick_up) = send_click(&shared, &handle, Click::left(HOTBAR_0)).await;
    assert_eq!(
        named_game(pick_up.carried.as_ref()),
        expect_item(PLANKS, 8),
        "left-clicking the planks must load the cursor"
    );

    let (_, refused) = send_click(&shared, &handle, Click::left(RESULT)).await;
    assert!(
        refused.changed_slots.is_empty(),
        "our own prediction must not move anything into the take-only result slot: {:?}",
        refused.changed_slots
    );
    let after_refusal = server_full_state(&shared, &handle, window).await;
    assert_eq!(
        named(after_refusal.items.get(RESULT).and_then(Option::as_ref)),
        None,
        "the server must refuse a placement into the result slot"
    );
    assert_eq!(
        named(after_refusal.carried.as_ref()),
        expect_item(PLANKS, 8),
        "and the refused stack must still be on the cursor, server-side"
    );
    checks += 1;
    println!("check 3 OK: the server refused a placement into the result slot, cursor intact");

    // ---------------------------------------------------------------------
    // Check 4 (NEGATIVE CONTROL, observed): three planks in an L match no recipe,
    // and the server leaves the result slot empty.
    //
    // The same three clicks also prove the detector can see a placement land, so
    // check 5's "a result appeared" is a real transition and not a blind spot.
    // ---------------------------------------------------------------------
    for &cell in &CELLS_2X2[..3] {
        send_click(&shared, &handle, Click::right(cell)).await;
    }
    let three = server_full_state(&shared, &handle, window).await;
    for &cell in &CELLS_2X2[..3] {
        assert_eq!(
            named(three.items.get(cell).and_then(Option::as_ref)),
            expect_item(PLANKS, 1),
            "right-click must deposit exactly one plank into grid cell {cell}"
        );
    }
    assert_eq!(
        named(three.items.get(CELLS_2X2[3]).and_then(Option::as_ref)),
        None,
        "the fourth cell is deliberately still empty"
    );
    assert_eq!(
        named(three.items.get(RESULT).and_then(Option::as_ref)),
        None,
        "3 planks in an L are not a recipe; the server must leave the result slot empty"
    );
    assert_menu_matches_server("three-planks", &shared, &three);
    checks += 1;
    println!("check 4 OK: negative control fired — 3 planks, placements landed, result slot empty");

    // ---------------------------------------------------------------------
    // Check 5: the fourth plank completes the 2x2 and *the server* fills slot 0.
    //
    // The click we send must carry no slot-0 change of its own: the client does
    // not match recipes. The value asserted is a raw `container_set_slot` the
    // server pushed.
    // ---------------------------------------------------------------------
    let pushes_before = pushes_for(&shared, window, RESULT).len();
    let (_, completing) = send_click(&shared, &handle, Click::right(CELLS_2X2[3])).await;
    assert!(
        completing
            .changed_slots
            .iter()
            .all(|(slot, _)| usize::from(*slot) != RESULT),
        "the client must never predict the result slot; it carried {:?}",
        completing.changed_slots
    );

    let result_push = poll_until(
        Duration::from_secs(15),
        Duration::from_millis(200),
        || async {
            let pushes = pushes_for(&shared, window, RESULT);
            (pushes.len() > pushes_before).then(|| pushes.last().cloned().flatten())
        },
    )
    .await
    .flatten()
    .expect("the server never pushed a container_set_slot for the result slot");
    assert_eq!(
        named(Some(&result_push)),
        expect_item(TABLE, 1),
        "a 2x2 of planks must make the SERVER put a crafting table in slot 0"
    );
    // And the reconcile seam folded it into the menu the UI reads.
    assert_eq!(
        named_game(lock(&shared).menus.opened().unwrap().slot_item(RESULT)),
        expect_item(TABLE, 1),
        "Menus::apply must reconcile the server's result into the open menu"
    );
    checks += 1;
    println!("check 5 OK: server pushed container_set_slot(0) = crafting_table; client predicted nothing there");

    // ---------------------------------------------------------------------
    // Top the grid up to 2 planks per cell so a single shift-click can craft
    // twice. The cursor still holds 4 planks.
    // ---------------------------------------------------------------------
    for &cell in &CELLS_2X2 {
        send_click(&shared, &handle, Click::right(cell)).await;
    }
    let loaded = server_full_state(&shared, &handle, window).await;
    for &cell in &CELLS_2X2 {
        assert_eq!(
            named(loaded.items.get(cell).and_then(Option::as_ref)),
            expect_item(PLANKS, 2),
            "cell {cell} must hold 2 planks before the repeat-craft check"
        );
    }
    assert_eq!(
        named(loaded.carried.as_ref()),
        None,
        "all 8 planks are in the grid now"
    );
    assert_eq!(
        named(loaded.items.get(RESULT).and_then(Option::as_ref)),
        expect_item(TABLE, 1),
        "the result is still a single crafting table regardless of ingredient depth"
    );
    assert_menu_matches_server("loaded-grid", &shared, &loaded);

    // ---------------------------------------------------------------------
    // Check 6: our shift-click predicts exactly ONE craft, into the LAST hotbar
    // slot.
    //
    // `CraftingMenu.quickMoveStack` moves the result into `10..46` **backwards**,
    // so the player region fills from the back — slot 45 first. And the prediction
    // is one craft, not two: `doClick`'s QUICK_MOVE loop repeats only while the
    // slot still holds the same item, and a client's `CraftingMenu` has
    // a null level access, so nothing refills slot 0 between iterations.
    // Predicting more would mean matching the recipe locally.
    // ---------------------------------------------------------------------
    let (_, quick) = send_click(&shared, &handle, Click::shift(RESULT)).await;
    let predicted_destination = quick
        .changed_slots
        .iter()
        .find(|(slot, _)| usize::from(*slot) == HOTBAR_8)
        .map(|(_, item)| named_game(item.as_ref()))
        .unwrap_or_else(|| {
            panic!(
                "the shift-click must fill the player region from the back (menu slot 45); it \
                 changed {:?}",
                quick.changed_slots
            )
        });
    assert_eq!(
        predicted_destination,
        expect_item(TABLE, 1),
        "the client must predict exactly ONE craft"
    );
    for &cell in &CELLS_2X2 {
        let predicted_cell = quick
            .changed_slots
            .iter()
            .find(|(slot, _)| usize::from(*slot) == cell)
            .map(|(_, item)| named_game(item.as_ref()));
        assert_eq!(
            predicted_cell,
            Some(expect_item(PLANKS, 1)),
            "one predicted craft costs exactly one plank from cell {cell}"
        );
    }
    checks += 1;
    println!("check 6 OK: shift-click predicted 1 craft into menu slot 45 and charged the grid once");

    // ---------------------------------------------------------------------
    // Check 7: the SERVER crafted TWICE off that one shift-click.
    //
    // Same loop, different menu: the server's `CraftingMenu` has a real
    // `ContainerLevelAccess`, so `slotsChanged` recomputes the recipe and refills
    // slot 0 between iterations, and the loop runs until the grid runs out. The
    // extra craft comes back as `container_set_slot` corrections, which
    // `ClientMenu::reconcile` folds in — the divergence is asserted in both
    // directions, so "we agree with the server" cannot be satisfied by us having
    // guessed the repeat locally.
    // ---------------------------------------------------------------------
    let crafted = poll_until(
        Duration::from_secs(20),
        Duration::from_millis(300),
        || async {
            let state = server_full_state(&shared, &handle, window).await;
            let done = named(state.items.get(HOTBAR_8).and_then(Option::as_ref))
                == expect_item(TABLE, 2);
            done.then_some(state)
        },
    )
    .await
    .unwrap_or_else(|| {
        let last = lock(&shared).full_state.clone();
        panic!(
            "the server never produced 2 crafting tables from one shift-click; its last full \
             state had menu slot 45 = {:?} and cells {:?}",
            named(
                last.as_ref()
                    .and_then(|s| s.items.get(HOTBAR_8))
                    .and_then(Option::as_ref)
            ),
            CELLS_2X2.map(|cell| named(
                last.as_ref()
                    .and_then(|s| s.items.get(cell))
                    .and_then(Option::as_ref)
            )),
        )
    });
    assert_eq!(
        named(crafted.items.get(HOTBAR_8).and_then(Option::as_ref)),
        expect_item(TABLE, 2),
        "one shift-click of the result must craft until the grid runs out"
    );
    for &cell in &CELLS_2X2 {
        assert_eq!(
            named(crafted.items.get(cell).and_then(Option::as_ref)),
            None,
            "the repeat crafting must drain grid cell {cell}"
        );
    }
    assert_eq!(
        named(crafted.items.get(RESULT).and_then(Option::as_ref)),
        None,
        "an empty grid matches no recipe, so the server clears the result slot"
    );
    // The prediction really did disagree, and reconcile really did absorb it.
    assert_ne!(
        predicted_destination,
        named(crafted.items.get(HOTBAR_8).and_then(Option::as_ref)),
        "if the client had predicted 2 as well, this test would prove nothing about who \
         owns the repeat"
    );
    assert_menu_matches_server("after-repeat-craft", &shared, &crafted);
    checks += 1;
    println!("check 7 OK: server crafted 2 from one shift-click; client predicted 1 and reconciled");

    // ---------------------------------------------------------------------
    // Check 8: an independent server oracle, on a different channel.
    //
    // `/clear <player> <item> 0` counts without removing. Two crafts consumed all
    // 8 planks and produced 2 tables.
    // ---------------------------------------------------------------------
    let _ = handle.send_action(ClientAction::ContainerClose { window_id: window });
    tokio::time::sleep(SETTLE).await;
    let tables = count_items(&mut rcon, &user, TABLE).await;
    let planks_left = count_items(&mut rcon, &user, PLANKS).await;
    println!("  RCON count: crafting_table={tables} oak_planks={planks_left}");
    assert_eq!(
        tables, 2,
        "the server's own inventory count must report 2 crafting tables"
    );
    assert_eq!(
        planks_left, 0,
        "2 crafts consume 2x4 = all 8 planks; the server must report none left"
    );
    checks += 1;
    println!("check 8 OK: RCON inventory count agrees — 2 crafting tables, 0 planks");

    // ---------------------------------------------------------------------
    let _ = rcon
        .cmd(&format!(
            "setblock {} {} {} minecraft:air",
            table.x, table.y, table.z
        ))
        .await;
    assert_eq!(
        checks, EXPECTED_CHECKS,
        "expected {EXPECTED_CHECKS} live checks, only reached {checks}"
    );
    println!("=== all {checks} live crafting checks passed ===");

    handle.shutdown();
    let _ = drain.await;
}
