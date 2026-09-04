//! Live **multi-slot container** and **stack-size** oracle against the dedicated
//! creative 26.2 server (`lodestone-creative`, port 25570, RCON 25571).
//!
//! `live_click.rs` proved the click machine agrees with the server, but only for
//! the **player inventory** menu (window 0) and only with **64-stacking**
//! diamonds. Two branches of vanilla's click logic were therefore untested live:
//!
//! 1. **Per-menu-type quick-move.** `AbstractContainerMenu.quickMoveStack` is
//!    overridden per menu; a chest's rule (container <-> player inventory) is a
//!    different code path from the player menu's (hotbar/main/armour). This test
//!    opens a *real chest* by interacting with a placed block and drives
//!    shift-click both directions plus a boundary-crossing merge.
//! 2. **Stack sizes != 64.** The wire only sends component *overrides*, not an
//!    item's default max stack, so a model that assumes 64 passes the diamond
//!    tests while being wrong for ender pearls (16) or buckets (1). This test
//!    seeds those, injects their real caps by numeric id, and — for a
//!    `minecraft:max_stack_size` **component override** — decodes the cap off the
//!    wire. The scenarios are chosen so a 64-only model would *diverge*: a
//!    cap-16 drag, a cap-1 double-click-collect, and an override-vs-default swap.
//!
//! ## The oracle contract (same as `live_click.rs`)
//!
//! The server applies `menu.clicked()` unconditionally and, on a **stale**
//! `state_id`, replies with `broadcastFullState()` (the authoritative post-click
//! content), ignoring our predicted changes. So each click carries an empty
//! prediction, and we assert our version-free [`Menu`] equals the server's own
//! packet, slot by slot and cursor. A divergence localises to a slot + click.
//!
//! The S0 capture and the tick-trap poll use a `-999` (outside) `PICKUP` click
//! with an empty cursor, which `doClick` treats as a **guaranteed no-op** (unlike
//! `live_click.rs`'s slot-9 resync, which mutates and only survives by oscillating
//! across retries — verified in the decompiled `AbstractContainerMenu.doClick`).
//!
//! ## Run it
//!
//! ```text
//! cargo test -p lodestone-game --features live-reconcile --test live_container -- --ignored --nocapture
//! ```
#![cfg(feature = "live-reconcile")]

use lodestone_testsupport::{AsyncRconClient as Rcon, unique_username};
use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_game::click::{Click, PlayerCtx, drag_type};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;
use lodestone_net::Connection;
use tokio::net::TcpStream;
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25570;
const RCON_PORT: u16 = 25571;
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_776: i32 = 776;

/// A single chest is `generic_9x3`: 27 container slots then 36 player slots.
const CHEST_SIZE: usize = 27;
const CHEST_MENU_SLOTS: usize = CHEST_SIZE + 36;
const PLAYER_MENU_SLOTS: usize = 46;
const MENU_GENERIC_9X3: i32 = 2;

/// `minecraft:data_component_type` protocol id for `max_stack_size` (26.2).
const COMP_MAX_STACK_SIZE: i32 = 1;

/// Where we place and open the test chest. Flat world: surface top y=-61, so the
/// player stands at y=-60 and the chest sits on the grass, within creative reach.
const CHEST_X: i32 = 2;
const CHEST_Y: i32 = -60;
const CHEST_Z: i32 = 0;

/// Numeric item ids (from 26.2 `registries.json`) whose *default* max stack size
/// is not 64. The wire never sends these (they are item defaults, not overrides),
/// so we inject them so the model's stack-limit arithmetic matches the server.
fn known_max(item_id: i32) -> Option<i32> {
    match item_id {
        1144 => Some(16), // minecraft:ender_pearl
        1041 => Some(1),  // minecraft:water_bucket
        _ => None,
    }
}

/// Serverbound `ContainerInput` ordinals (VarInt on the wire).
mod mode {
    pub const PICKUP: i32 = 0;
    pub const QUICK_MOVE: i32 = 1;
    #[allow(dead_code)]
    pub const SWAP: i32 = 2;
    pub const THROW: i32 = 4;
    pub const QUICK_CRAFT: i32 = 5;
    pub const PICKUP_ALL: i32 = 6;
}

mod pkt {
    pub mod hs_sb {
        pub const INTENTION: i32 = 0;
    }
    pub mod login_cb {
        pub const DISCONNECT: i32 = 0;
        pub const ENCRYPTION_REQUEST: i32 = 1;
        pub const LOGIN_FINISHED: i32 = 2;
        pub const COMPRESSION: i32 = 3;
    }
    pub mod login_sb {
        pub const HELLO: i32 = 0;
        pub const LOGIN_ACKNOWLEDGED: i32 = 3;
    }
    pub mod cfg_cb {
        pub const FINISH_CONFIGURATION: i32 = 3;
        pub const KEEP_ALIVE: i32 = 4;
        pub const SELECT_KNOWN_PACKS: i32 = 14;
    }
    pub mod cfg_sb {
        pub const FINISH_CONFIGURATION: i32 = 3;
        pub const KEEP_ALIVE: i32 = 4;
        pub const SELECT_KNOWN_PACKS: i32 = 7;
    }
    pub mod play_cb {
        pub const BLOCK_CHANGED_ACK: i32 = 4;
        pub const CHUNK_BATCH_FINISHED: i32 = 11;
        pub const CONTAINER_SET_CONTENT: i32 = 18;
        pub const KEEP_ALIVE: i32 = 44;
        pub const LOGIN: i32 = 49;
        pub const OPEN_SCREEN: i32 = 59;
        pub const PLAYER_POSITION: i32 = 72;
        pub const SET_HEALTH: i32 = 104;
    }
    pub mod play_sb {
        pub const ACCEPT_TELEPORTATION: i32 = 0;
        pub const CHUNK_BATCH_RECEIVED: i32 = 11;
        pub const CLIENT_COMMAND: i32 = 12;
        pub const CONTAINER_CLICK: i32 = 18;
        pub const KEEP_ALIVE: i32 = 28;
        pub const PLAYER_LOADED: i32 = 44;
        pub const USE_ITEM_ON: i32 = 66;
    }
}

// ---- wire helpers ----------------------------------------------------------

fn write_string(w: &mut Writer, s: &str) {
    w.var_i32(s.len() as i32);
    w.bytes(s.as_bytes());
}

fn read_string(r: &mut Reader) -> String {
    let len = r.var_i32().expect("string length") as usize;
    let bytes = r.bytes(len).expect("string bytes");
    String::from_utf8_lossy(bytes).into_owned()
}

/// A synthetic, registry-free identifier for a numeric item id.
fn synthetic_item(item_id: i32) -> ItemStack {
    ItemStack::new(
        format!("oracle:i{item_id}")
            .parse()
            .expect("valid synthetic id"),
        1,
    )
}

/// Decodes one clientbound `ItemStack`. Accepts an empty component patch (plain
/// items) and a single `max_stack_size` override (our deliberately-overridden
/// item); panics on any other component so an unexpected payload is loud, not
/// silently misparsed. The effective max stack size is the override if present,
/// else the item's known non-64 default, else 64.
fn read_item(r: &mut Reader) -> Option<ItemStack> {
    let count = r.var_i32().expect("item count");
    if count <= 0 {
        return None;
    }
    let item_id = r.var_i32().expect("item holder id");
    let added = r.var_i32().expect("component patch: added count");
    let removed = r.var_i32().expect("component patch: removed count");
    let mut patch_max = None;
    for _ in 0..added {
        let comp = r.var_i32().expect("component type id");
        if comp == COMP_MAX_STACK_SIZE {
            patch_max = Some(r.var_i32().expect("max_stack_size payload (VarInt)"));
        } else {
            panic!(
                "item {item_id} carried unexpected component id {comp}; this oracle only handles max_stack_size overrides"
            );
        }
    }
    assert_eq!(
        removed, 0,
        "item {item_id} carried a component removal; unsupported"
    );
    let mut stack = synthetic_item(item_id);
    stack.set_count(count);
    if let Some(max) = patch_max.or_else(|| known_max(item_id)) {
        stack = stack.with_max_stack_size(max);
    }
    Some(stack)
}

/// A serverbound `container_click` for `window` with an EMPTY prediction. With a
/// stale `state_id` the server ignores the prediction and resyncs.
fn encode_click(window: i32, state_id: i32, slot: i16, button: u8, click_mode: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window);
    w.var_i32(state_id);
    w.i16(slot);
    w.u8(button);
    w.var_i32(click_mode);
    w.var_i32(0); // changed slots: empty
    w.u8(0); // carried HashedStack: optional-false
    w.into_vec()
}

/// Packs a `BlockPos` the way vanilla does: 26 bits x, 26 bits z, 12 bits y.
fn block_pos(x: i64, y: i64, z: i64) -> i64 {
    ((x & 0x3FF_FFFF) << 38) | ((z & 0x3FF_FFFF) << 12) | (y & 0xFFF)
}

/// A serverbound `use_item_on` that right-clicks the top face of the chest.
fn encode_use_item_on() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(0); // hand = MAIN_HAND
    w.i64(block_pos(CHEST_X.into(), CHEST_Y.into(), CHEST_Z.into()));
    w.var_i32(1); // direction = UP
    w.f32(0.5); // cursor x
    w.f32(1.0); // cursor y (top face)
    w.f32(0.5); // cursor z
    w.bool(false); // inside
    w.bool(false); // world border hit
    w.var_i32(0); // sequence
    w.into_vec()
}

/// A decoded `container_set_content` (any window).
#[derive(Clone)]
struct SetContent {
    window: i32,
    state_id: i32,
    items: Vec<Option<ItemStack>>,
    carried: Option<ItemStack>,
}

fn decode_set_content(payload: &[u8]) -> SetContent {
    let mut r = Reader::new(payload);
    let window = r.var_i32().expect("container id");
    let state_id = r.var_i32().expect("state id");
    let count = r.var_i32().expect("item list length") as usize;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        items.push(read_item(&mut r));
    }
    let carried = read_item(&mut r);
    r.ensure_empty()
        .expect("container_set_content consumes the whole packet (zero trailing bytes)");
    SetContent {
        window,
        state_id,
        items,
        carried,
    }
}

// ---- the live session ------------------------------------------------------

#[derive(PartialEq, Debug)]
enum Phase {
    Login,
    Configuration,
    Play,
}

struct Session {
    conn: Connection<TcpStream>,
}

impl Session {
    async fn join(username: &str) -> Self {
        let mut conn = Connection::connect((HOST, PORT)).await.expect("connect to lodestone-creative on 127.0.0.1:25570 (game port); start it with docker run --rm -p 25570:25570 -p 25571:25571 ... lodestone-creative");

        let mut hs = Writer::default();
        hs.var_i32(PROTOCOL_776);
        write_string(&mut hs, HOST);
        hs.u16(PORT);
        hs.var_i32(2);
        conn.write_packet(pkt::hs_sb::INTENTION, &hs.into_vec())
            .await
            .expect("handshake");

        let mut hello = Writer::default();
        write_string(&mut hello, username);
        hello.bytes(Uuid::new_v4().as_bytes());
        conn.write_packet(pkt::login_sb::HELLO, &hello.into_vec())
            .await
            .expect("login hello");

        let mut session = Self { conn };
        session.drive_to_play().await;
        // The server gates block interaction (`handleUseItemOn`) on
        // `hasClientLoaded()`, so announce we've loaded before opening a chest.
        session
            .conn
            .write_packet(pkt::play_sb::PLAYER_LOADED, &[])
            .await
            .expect("player loaded");
        session.settle().await;
        session
    }

    /// Drains any backlog (including the unsolicited join-time window-0
    /// `container_set_content`) until the socket goes quiet.
    async fn settle(&mut self) {
        let idle = Duration::from_millis(600);
        loop {
            match tokio::time::timeout(idle, self.conn.read_packet()).await {
                Ok(Ok(Some((id, payload)))) => {
                    self.handle_common(id, &payload).await;
                }
                Ok(Ok(None)) => panic!("EOF during settle"),
                Ok(Err(e)) => panic!("read error during settle: {e}"),
                Err(_) => return,
            }
        }
    }

    /// Handles keep-alives, chunk-batch acks, teleport confirms, block-change
    /// acks, and the death trap. Returns whether the packet was consumed here.
    async fn handle_common(&mut self, id: i32, payload: &[u8]) -> bool {
        if id == pkt::play_cb::KEEP_ALIVE {
            self.conn
                .write_packet(pkt::play_sb::KEEP_ALIVE, payload)
                .await
                .expect("play ka");
        } else if id == pkt::play_cb::CHUNK_BATCH_FINISHED {
            let mut w = Writer::default();
            w.f32(16.0);
            self.conn
                .write_packet(pkt::play_sb::CHUNK_BATCH_RECEIVED, &w.into_vec())
                .await
                .expect("chunk ack");
        } else if id == pkt::play_cb::PLAYER_POSITION {
            let mut r = Reader::new(payload);
            let teleport_id = r.var_i32().expect("teleport id");
            let mut w = Writer::default();
            w.var_i32(teleport_id);
            self.conn
                .write_packet(pkt::play_sb::ACCEPT_TELEPORTATION, &w.into_vec())
                .await
                .expect("accept tp");
        } else if id == pkt::play_cb::BLOCK_CHANGED_ACK {
            // no-op: we don't do client-side block prediction
        } else if id == pkt::play_cb::SET_HEALTH {
            let mut r = Reader::new(payload);
            let health = r.f32().expect("health");
            if health <= 0.0 {
                eprintln!("!! set_health = {health}: inherited a dead player; respawning");
                let mut w = Writer::default();
                w.var_i32(0);
                self.conn
                    .write_packet(pkt::play_sb::CLIENT_COMMAND, &w.into_vec())
                    .await
                    .expect("respawn");
            }
        } else {
            return false;
        }
        true
    }

    async fn drive_to_play(&mut self) {
        let mut phase = Phase::Login;
        let deadline = Duration::from_secs(45);
        let step = Duration::from_secs(10);
        let ok = tokio::time::timeout(deadline, async {
            loop {
                let (id, payload) = match tokio::time::timeout(step, self.conn.read_packet()).await
                {
                    Ok(Ok(Some(p))) => p,
                    Ok(Ok(None)) => panic!("EOF before Play"),
                    Ok(Err(e)) => panic!("read error before Play: {e}"),
                    Err(_) => panic!("timeout before Play in {phase:?}"),
                };
                match phase {
                    Phase::Login => {
                        if id == pkt::login_cb::COMPRESSION {
                            let mut r = Reader::new(&payload);
                            self.conn.set_compression(r.var_i32().expect("threshold"));
                        } else if id == pkt::login_cb::LOGIN_FINISHED {
                            self.conn
                                .write_packet(pkt::login_sb::LOGIN_ACKNOWLEDGED, &[])
                                .await
                                .expect("login ack");
                            phase = Phase::Configuration;
                        } else if id == pkt::login_cb::ENCRYPTION_REQUEST {
                            panic!("unexpected encryption request (server must be offline-mode)");
                        } else if id == pkt::login_cb::DISCONNECT {
                            let mut r = Reader::new(&payload);
                            panic!("login disconnect: {}", read_string(&mut r));
                        }
                    }
                    Phase::Configuration => {
                        if id == pkt::cfg_cb::KEEP_ALIVE {
                            self.conn
                                .write_packet(pkt::cfg_sb::KEEP_ALIVE, &payload)
                                .await
                                .expect("cfg ka");
                        } else if id == pkt::cfg_cb::SELECT_KNOWN_PACKS {
                            self.conn
                                .write_packet(pkt::cfg_sb::SELECT_KNOWN_PACKS, &payload)
                                .await
                                .expect("packs");
                        } else if id == pkt::cfg_cb::FINISH_CONFIGURATION {
                            self.conn
                                .write_packet(pkt::cfg_sb::FINISH_CONFIGURATION, &[])
                                .await
                                .expect("cfg fin");
                            phase = Phase::Play;
                        }
                    }
                    Phase::Play => {
                        if id == pkt::play_cb::LOGIN {
                            return;
                        } else if id == pkt::play_cb::KEEP_ALIVE {
                            self.conn
                                .write_packet(pkt::play_sb::KEEP_ALIVE, &payload)
                                .await
                                .expect("play ka");
                        } else if id == pkt::play_cb::CHUNK_BATCH_FINISHED {
                            let mut w = Writer::default();
                            w.f32(16.0);
                            self.conn
                                .write_packet(pkt::play_sb::CHUNK_BATCH_RECEIVED, &w.into_vec())
                                .await
                                .expect("chunk ack");
                        }
                    }
                }
            }
        })
        .await;
        assert!(ok.is_ok(), "did not reach Play");
        eprintln!("reached Play");
    }

    /// Reads packets (handling ambient traffic) until the wanted packet arrives,
    /// returning its payload.
    async fn pump_until(&mut self, want: i32, label: &str) -> Vec<u8> {
        let deadline = Duration::from_secs(15);
        let step = Duration::from_secs(10);
        let got = tokio::time::timeout(deadline, async {
            loop {
                let (id, payload) = match tokio::time::timeout(step, self.conn.read_packet()).await
                {
                    Ok(Ok(Some(p))) => p,
                    Ok(Ok(None)) => panic!("EOF while awaiting {label}"),
                    Ok(Err(e)) => panic!("read error while awaiting {label}: {e}"),
                    Err(_) => panic!("timeout awaiting {label}"),
                };
                if id == want {
                    return payload;
                }
                self.handle_common(id, &payload).await;
            }
        })
        .await;
        got.unwrap_or_else(|_| panic!("{label} within deadline"))
    }

    /// Sends a click (stale state id forces a full resync) and returns the
    /// server's authoritative `container_set_content` for `window`.
    async fn click(&mut self, window: i32, slot: i16, button: u8, click_mode: i32) -> SetContent {
        const STALE: i32 = 30_000;
        self.conn
            .write_packet(
                pkt::play_sb::CONTAINER_CLICK,
                &encode_click(window, STALE, slot, button, click_mode),
            )
            .await
            .expect("send click");
        let payload = self
            .pump_until(pkt::play_cb::CONTAINER_SET_CONTENT, "container_set_content")
            .await;
        let content = decode_set_content(&payload);
        assert_eq!(content.window, window, "content window mismatch");
        content
    }

    /// A guaranteed no-op resync: `-999` PICKUP with an empty cursor. Forces the
    /// server to broadcast the full authoritative content without mutating it.
    async fn resync(&mut self, window: i32) -> SetContent {
        self.click(window, -999, 0, mode::PICKUP).await
    }

    /// Opens the chest by interacting with the placed block; returns the assigned
    /// container id after asserting the menu type is a single chest.
    async fn open_chest(&mut self) -> i32 {
        self.conn
            .write_packet(pkt::play_sb::USE_ITEM_ON, &encode_use_item_on())
            .await
            .expect("use_item_on");
        let payload = self
            .pump_until(pkt::play_cb::OPEN_SCREEN, "open_screen")
            .await;
        let mut r = Reader::new(&payload);
        let window = r.var_i32().expect("open_screen container id");
        let menu_type = r.var_i32().expect("open_screen menu type");
        assert_eq!(
            menu_type, MENU_GENERIC_9X3,
            "expected a single chest (generic_9x3)"
        );
        assert_ne!(
            window, 0,
            "chest window must differ from the player inventory"
        );
        // Drain the initial content the server sends right after open_screen.
        let _ = self
            .pump_until(pkt::play_cb::CONTAINER_SET_CONTENT, "initial chest content")
            .await;
        eprintln!("opened chest: window {window}, type generic_9x3");
        window
    }
}

// ---- prediction plumbing ---------------------------------------------------

fn menu_from_player(state: &SetContent) -> Menu {
    apply_state(Menu::player(), state)
}

fn menu_from_generic(state: &SetContent, size: usize) -> Menu {
    apply_state(Menu::generic(size), state)
}

fn apply_state(mut menu: Menu, state: &SetContent) -> Menu {
    #[allow(clippy::cast_sign_loss)]
    menu.set_state_id(state.state_id as u32);
    for (i, item) in state.items.iter().enumerate() {
        menu.set_slot_item(i, item.clone());
    }
    menu.set_carried(state.carried.clone());
    menu
}

fn assert_match(label: &str, slots: usize, local: &Menu, server: &SetContent) {
    let mut mismatches = Vec::new();
    for i in 0..slots {
        let l = local.slot_item(i).cloned();
        let s = server.items.get(i).cloned().flatten();
        if l != s {
            mismatches.push(format!("  slot {i}: predicted {l:?} != server {s:?}"));
        }
    }
    let lc = local.carried().cloned();
    if lc != server.carried {
        mismatches.push(format!(
            "  cursor: predicted {lc:?} != server {:?}",
            server.carried
        ));
    }
    assert!(
        mismatches.is_empty(),
        "[{label}] predicted state diverged from the live server:\n{}",
        mismatches.join("\n")
    );
    eprintln!("[{label}] OK — {slots}-slot state matches the server slot-by-slot");
}

/// Where a generic-menu index seeds: chest block slot or player entity slot.
enum SeedTarget {
    Chest(i32),
    Player(i32),
}

/// Maps a generic-menu index to its seed target. Generic layout: `0..size`
/// chest, then player main (native 9..36), then player hotbar (native 0..9).
fn generic_target(menu_index: usize, size: usize) -> SeedTarget {
    if menu_index < size {
        SeedTarget::Chest(menu_index as i32)
    } else {
        let rel = menu_index - size;
        if rel < 27 {
            SeedTarget::Player((rel + 9) as i32) // main storage
        } else {
            SeedTarget::Player((rel - 27) as i32) // hotbar
        }
    }
}

/// The `/item replace entity … container.N` arg for a **player-menu** index:
/// hotbar 36..=44 -> 0..=8, main 9..=35 -> 9..=35.
fn player_container_arg(menu_slot: usize) -> i32 {
    if (36..=44).contains(&menu_slot) {
        (menu_slot - 36) as i32
    } else {
        menu_slot as i32
    }
}

/// Polls the open `window` (respecting the tick trap) until every seed count is
/// reflected and exactly `expected_filled` slots are occupied. Returns S0.
async fn poll_seed(
    session: &mut Session,
    window: i32,
    slots: usize,
    expect: &[(usize, i32)],
) -> SetContent {
    for attempt in 0..6 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let s0 = session.resync(window).await;
        let counts_ok = expect
            .iter()
            .all(|&(idx, c)| s0.items.get(idx).cloned().flatten().map(|i| i.count()) == Some(c));
        let filled = s0.items.iter().take(slots).filter(|i| i.is_some()).count();
        if counts_ok && filled == expect.len() {
            return s0;
        }
        eprintln!("seed not yet reflected (attempt {attempt}, filled={filled}); polling…");
    }
    panic!("seeds never reflected in server state (tick trap or bad seed)");
}

fn drag_button(header: i32, kind: i32) -> u8 {
    u8::try_from((header & 3) | ((kind & 3) << 2)).unwrap()
}

// ---- G4a: multi-slot chest menu -------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the creative lodestone-creative server on 127.0.0.1:25570"]
async fn live_chest_menu_agrees_with_server() {
    let user = unique_username();
    eprintln!("=== LIVE CHEST-MENU ORACLE (protocol {PROTOCOL_776}, creative :{PORT}) ===");
    eprintln!("username (unique per run): {user}");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect/authenticate RCON");
    let mut session = Session::join(&user).await;
    rcon.cmd(&format!("gamemode creative {user}")).await;

    // Stand the player next to a freshly placed lone chest, then open it.
    rcon.cmd(&format!("tp {user} 0.5 {CHEST_Y} 0.5")).await;
    rcon.cmd(&format!(
        "setblock {CHEST_X} {CHEST_Y} {CHEST_Z} minecraft:chest"
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    session.settle().await; // confirm the teleport, drain chunk traffic
    let window = session.open_chest().await;

    let ctx = PlayerCtx::creative();
    let diamond = "minecraft:diamond";
    let n = CHEST_MENU_SLOTS;

    async fn clear(rcon: &mut Rcon, user: &str) {
        rcon.cmd(&format!("clear {user}")).await;
        rcon.cmd(&format!(
            "data merge block {CHEST_X} {CHEST_Y} {CHEST_Z} {{Items:[]}}"
        ))
        .await;
    }

    // Seeds a set of (generic-menu index, item, count) triples.
    async fn seed(rcon: &mut Rcon, user: &str, seeds: &[(usize, &str, i32)]) {
        for &(idx, item, count) in seeds {
            let cmd = match generic_target(idx, CHEST_SIZE) {
                SeedTarget::Chest(c) => {
                    format!(
                        "item replace block {CHEST_X} {CHEST_Y} {CHEST_Z} container.{c} with {item} {count}"
                    )
                }
                SeedTarget::Player(c) => {
                    format!("item replace entity {user} container.{c} with {item} {count}")
                }
            };
            rcon.cmd(&cmd).await;
        }
    }

    // -- C1: quick-move chest -> player (empty inventory) --
    {
        clear(&mut rcon, &user).await;
        seed(&mut rcon, &user, &[(0, diamond, 64)]).await;
        let s0 = poll_seed(&mut session, window, n, &[(0, 64)]).await;
        let mut menu = menu_from_generic(&s0, CHEST_SIZE);
        Click::shift(0).apply(&mut menu, ctx.clone());
        let s1 = session.click(window, 0, 0, mode::QUICK_MOVE).await;
        assert_match("chest->player", n, &menu, &s1);
    }

    // -- C2: quick-move player hotbar -> chest (empty chest) --
    {
        clear(&mut rcon, &user).await;
        seed(&mut rcon, &user, &[(54, diamond, 64)]).await; // menu 54 = hotbar native 0
        let s0 = poll_seed(&mut session, window, n, &[(54, 64)]).await;
        let mut menu = menu_from_generic(&s0, CHEST_SIZE);
        Click::shift(54).apply(&mut menu, ctx.clone());
        let s1 = session.click(window, 54, 0, mode::QUICK_MOVE).await;
        assert_match("player->chest", n, &menu, &s1);
    }

    // -- C3: quick-move chest -> player with a boundary-crossing merge --
    {
        clear(&mut rcon, &user).await;
        seed(&mut rcon, &user, &[(0, diamond, 40), (62, diamond, 30)]).await; // menu 62 = hotbar native 8
        let s0 = poll_seed(&mut session, window, n, &[(0, 40), (62, 30)]).await;
        let mut menu = menu_from_generic(&s0, CHEST_SIZE);
        Click::shift(0).apply(&mut menu, ctx);
        let s1 = session.click(window, 0, 0, mode::QUICK_MOVE).await;
        assert_match("chest->player-merge", n, &menu, &s1);
    }

    eprintln!("=== CHEST-MENU ORACLE PASSED: generic quick-move agrees with the server ===");
}

// ---- G4b: stack sizes != 64 ------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the creative lodestone-creative server on 127.0.0.1:25570"]
async fn live_stack_sizes_agree_with_server() {
    let user = unique_username();
    eprintln!("=== LIVE STACK-SIZE ORACLE (protocol {PROTOCOL_776}, creative :{PORT}) ===");
    eprintln!("username (unique per run): {user}");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect/authenticate RCON");
    let mut session = Session::join(&user).await;
    rcon.cmd(&format!("gamemode creative {user}")).await;

    let ctx = PlayerCtx::creative();
    let w0 = 0;
    let n = PLAYER_MENU_SLOTS;

    async fn seed(rcon: &mut Rcon, user: &str, seeds: &[(usize, &str, i32)]) {
        rcon.cmd(&format!("clear {user}")).await;
        for &(menu_slot, item, count) in seeds {
            rcon.cmd(&format!(
                "item replace entity {user} container.{} with {item} {count}",
                player_container_arg(menu_slot)
            ))
            .await;
        }
    }

    // -- P1: ender pearls (max 16) right-click picks up half = 8 --
    {
        seed(&mut rcon, &user, &[(36, "minecraft:ender_pearl", 16)]).await;
        let s0 = poll_seed(&mut session, w0, n, &[(36, 16)]).await;
        let mut menu = menu_from_player(&s0);
        Click::right(36).apply(&mut menu, ctx.clone());
        let s1 = session.click(w0, 36, 1, mode::PICKUP).await;
        assert_match("pearl-pickup-half", n, &menu, &s1);
    }

    // -- P2: cap-16 left-drag even split over two near-full slots --
    // Cursor of 16 pearls dragged onto slots already holding 14 each: each slot
    // can only take 2 (up to the 16 cap), so 4 are distributed and 12 remain. A
    // model that assumed a 64 cap would try to add ~8 to each and diverge.
    {
        seed(
            &mut rcon,
            &user,
            &[
                (36, "minecraft:ender_pearl", 16),
                (9, "minecraft:ender_pearl", 14),
                (10, "minecraft:ender_pearl", 14),
            ],
        )
        .await;
        let s0 = poll_seed(&mut session, w0, n, &[(36, 16), (9, 14), (10, 14)]).await;
        let mut menu = menu_from_player(&s0);
        Click::left(36).apply(&mut menu, ctx.clone()); // cursor = 16 pearls
        menu.perform_drag(drag_type::EVEN, &[9, 10], ctx.clone());
        session.click(w0, 36, 0, mode::PICKUP).await;
        session
            .click(w0, -999, drag_button(0, drag_type::EVEN), mode::QUICK_CRAFT)
            .await;
        session
            .click(w0, 9, drag_button(1, drag_type::EVEN), mode::QUICK_CRAFT)
            .await;
        session
            .click(w0, 10, drag_button(1, drag_type::EVEN), mode::QUICK_CRAFT)
            .await;
        let s1 = session
            .click(w0, -999, drag_button(2, drag_type::EVEN), mode::QUICK_CRAFT)
            .await;
        assert_match("pearl-drag-cap16", n, &menu, &s1);
    }

    // -- P3: cap-1 double-click-collect gathers nothing --
    // A water bucket stacks to 1, so with a bucket already on the cursor a
    // double-click collects nothing. A 64-cap model would vacuum up the other
    // buckets and diverge.
    {
        seed(
            &mut rcon,
            &user,
            &[
                (36, "minecraft:water_bucket", 1),
                (9, "minecraft:water_bucket", 1),
                (10, "minecraft:water_bucket", 1),
            ],
        )
        .await;
        let s0 = poll_seed(&mut session, w0, n, &[(36, 1), (9, 1), (10, 1)]).await;
        let mut menu = menu_from_player(&s0);
        Click::left(36).apply(&mut menu, ctx.clone()); // cursor = 1 bucket (at its cap)
        Click::double(36).apply(&mut menu, ctx.clone());
        session.click(w0, 36, 0, mode::PICKUP).await;
        let s1 = session.click(w0, 36, 0, mode::PICKUP_ALL).await;
        assert_match("bucket-collect-cap1", n, &menu, &s1);
    }

    // -- P4: a max_stack_size override does not stack with the default item --
    // A diamond[max_stack_size=16] carries a component the plain diamond lacks,
    // so picking up a plain stack and clicking onto the overridden one SWAPS
    // rather than merges. A component-blind model would merge and diverge.
    {
        rcon.cmd(&format!("clear {user}")).await;
        rcon.cmd(&format!(
            "item replace entity {user} container.9 with minecraft:diamond 10"
        ))
        .await;
        rcon.cmd(&format!(
            "item replace entity {user} container.10 with minecraft:diamond[minecraft:max_stack_size=16] 5"
        ))
        .await;
        let s0 = poll_seed(&mut session, w0, n, &[(9, 10), (10, 5)]).await;
        // Sanity: the two diamonds must be distinguishable (different components).
        let plain = s0.items[9].clone().expect("plain diamond");
        let overridden = s0.items[10].clone().expect("overridden diamond");
        assert!(
            !ItemStack::is_same_item_same_components(&plain, &overridden),
            "override must make the stacks non-mergeable: {plain:?} vs {overridden:?}"
        );
        let mut menu = menu_from_player(&s0);
        Click::left(9).apply(&mut menu, ctx.clone()); // cursor = plain diamond x10
        Click::left(10).apply(&mut menu, ctx.clone()); // onto overridden -> swap
        session.click(w0, 9, 0, mode::PICKUP).await;
        let s1 = session.click(w0, 10, 0, mode::PICKUP).await;
        assert_match("override-no-stack-swap", n, &menu, &s1);
    }

    // -- P5: throw the whole overridden stack (Ctrl-Q) keeps its cap --
    {
        rcon.cmd(&format!("clear {user}")).await;
        rcon.cmd(&format!(
            "item replace entity {user} container.0 with minecraft:diamond[minecraft:max_stack_size=16] 7"
        ))
        .await;
        let s0 = poll_seed(&mut session, w0, n, &[(36, 7)]).await;
        let mut menu = menu_from_player(&s0);
        Click::drop_stack(36).apply(&mut menu, ctx);
        let s1 = session.click(w0, 36, 1, mode::THROW).await;
        assert_match("override-drop-stack", n, &menu, &s1);
    }

    eprintln!("=== STACK-SIZE ORACLE PASSED: non-64 caps and overrides agree with the server ===");
}
