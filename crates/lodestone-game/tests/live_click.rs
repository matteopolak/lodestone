//! Live **item-ful** click oracle against a dedicated creative 26.2 server.
//!
//! `live_reconcile.rs` proved the reconcile seam agrees with the server, but on
//! a *fresh survival* player it could only ever exercise an EMPTY inventory —
//! the interesting half of the click machine (quick-move, drag-distribute,
//! hotbar-swap, double-click-collect, throw) was untested against the server.
//! This test closes that gap.
//!
//! ## How it works
//!
//! We stand up a separate creative server (`lodestone-creative`, port 25570,
//! RCON 25571 — see the test harness ledger), join it, and for each click type:
//!
//! 1. **Seed** a known inventory with RCON `/item replace` / `/give` (name-based,
//!    so no numeric item ids are needed on our side).
//! 2. **Capture S0**: force the server to emit its authoritative window-0
//!    `container_set_content` by sending a click with a *stale* state id — the
//!    server applies the click regardless and, on a state-id mismatch, replies
//!    with `broadcastFullState()` (verified in the decompiled
//!    `ServerGamePacketListenerImpl.handleContainerClick`). The click's predicted
//!    `changedSlots`/carried are ignored on resync, so we always send an empty
//!    prediction — the response is the server's own truth.
//! 3. **Predict** the same click on our version-free [`Menu`] model.
//! 4. **Apply** the real click on the server (again stale state id) and capture
//!    its authoritative post-click `container_set_content` = S1_server.
//! 5. **Assert** our predicted state equals S1_server **slot by slot** (and the
//!    cursor). A divergence localises to a specific slot and click type.
//!
//! ## Tick trap (paid for by `impl-entity`)
//!
//! A freshly `/give`n change is not visible until the next server tick, so after
//! every mutating RCON command we *poll* the server's own content (never assert
//! immediately) until the seed is reflected.
//!
//! ## Item decoding
//!
//! We decode the real clientbound `ItemStack` (count + item holder + component
//! patch) but assert the [`DataComponentPatch`] is EMPTY (0 added, 0 removed) —
//! true for plain `/give` items with no custom components — and represent each
//! item by a synthetic identifier from its numeric id. That keeps us
//! version/registry-free while giving exact slot-by-slot equality; we only seed
//! items that stack to 64 so our default max-stack-size matches the server's.
//!
//! ## Run it
//!
//! ```text
//! cargo test -p lodestone-game --features live-reconcile --test live_click -- --ignored --nocapture
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
const PLAYER_WINDOW: i32 = 0;
const PLAYER_MENU_SLOTS: usize = 46;

/// Serverbound `ContainerInput` ordinals (VarInt on the wire).
mod mode {
    pub const PICKUP: i32 = 0;
    pub const QUICK_MOVE: i32 = 1;
    pub const SWAP: i32 = 2;
    #[allow(dead_code)]
    pub const CLONE: i32 = 3;
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
        pub const CHUNK_BATCH_FINISHED: i32 = 11;
        pub const CONTAINER_SET_CONTENT: i32 = 18;
        pub const KEEP_ALIVE: i32 = 44;
        pub const LOGIN: i32 = 49;
        pub const SET_HEALTH: i32 = 104;
    }
    pub mod play_sb {
        pub const CHUNK_BATCH_RECEIVED: i32 = 11;
        pub const CLIENT_COMMAND: i32 = 12;
        pub const CONTAINER_CLICK: i32 = 18;
        pub const KEEP_ALIVE: i32 = 28;
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

/// A synthetic, registry-free identifier for a numeric item id. Two stacks of
/// the same server item map to the same identifier, so slot-by-slot equality
/// between our model and the server is exact without resolving names.
fn synthetic_item(item_id: i32) -> ItemStack {
    ItemStack::new(
        format!("oracle:i{item_id}")
            .parse()
            .expect("valid synthetic id"),
        1,
    )
}

/// Decodes one clientbound `ItemStack`. Asserts the component patch is empty
/// (0 added, 0 removed) — the case for plain `/give` items — and panics loudly
/// otherwise, because this oracle deliberately carries no component decoder.
fn read_item(r: &mut Reader) -> Option<ItemStack> {
    let count = r.var_i32().expect("item count");
    if count <= 0 {
        return None;
    }
    let item_id = r.var_i32().expect("item holder id");
    let added = r.var_i32().expect("component patch: added count");
    let removed = r.var_i32().expect("component patch: removed count");
    assert_eq!(
        (added, removed),
        (0, 0),
        "seeded item {item_id} carried a non-empty component patch (added={added}, \
         removed={removed}); this oracle only seeds plain items with an empty patch"
    );
    let mut stack = synthetic_item(item_id);
    stack.set_count(count);
    Some(stack)
}

/// A serverbound `container_click` with an EMPTY prediction (no changed slots,
/// empty carried). With a stale `state_id` the server ignores the prediction and
/// resyncs, so this carries a real click's slot/button/mode and nothing else.
fn encode_click(state_id: i32, slot: i16, button: u8, click_mode: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(PLAYER_WINDOW);
    w.var_i32(state_id);
    w.i16(slot);
    w.u8(button);
    w.var_i32(click_mode);
    w.var_i32(0); // changed slots: empty
    w.u8(0); // carried HashedStack: optional-false (empty)
    w.into_vec()
}

/// A decoded window-0 `container_set_content`.
#[derive(Clone)]
struct SetContent {
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
    assert_eq!(
        window, PLAYER_WINDOW,
        "expected the player inventory window"
    );
    SetContent {
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

/// Owns the play connection and pumps packets, echoing keep-alives and handling
/// the death trap, until a `container_set_content` arrives.
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
        session.settle().await;
        session
    }

    /// Drains any backlog (including the unsolicited join-time
    /// `container_set_content`) so that afterwards every click maps 1:1 to the
    /// content it produces. Reads, echoing keep-alives, until the socket goes
    /// quiet for a short idle window.
    async fn settle(&mut self) {
        let idle = Duration::from_millis(600);
        loop {
            match tokio::time::timeout(idle, self.conn.read_packet()).await {
                Ok(Ok(Some((id, payload)))) => self.handle_ambient(id, &payload).await,
                Ok(Ok(None)) => panic!("EOF during settle"),
                Ok(Err(e)) => panic!("read error during settle: {e}"),
                Err(_) => return, // quiet
            }
        }
    }

    /// Echoes keep-alives, acks chunk batches, and respawns a dead player. All
    /// other packets (including `container_set_content`) are discarded.
    async fn handle_ambient(&mut self, id: i32, payload: &[u8]) {
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
        }
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
                    // Stay in Play until the join-game packet arrives, so the
                    // player's InventoryMenu exists server-side before we click.
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

    /// Sends a click (stale state id forces a full resync) and pumps until the
    /// server's authoritative `container_set_content` arrives.
    async fn click(&mut self, slot: i16, button: u8, click_mode: i32) -> SetContent {
        const STALE: i32 = 30_000;
        self.conn
            .write_packet(
                pkt::play_sb::CONTAINER_CLICK,
                &encode_click(STALE, slot, button, click_mode),
            )
            .await
            .expect("send click");
        self.pump_content().await
    }

    /// A no-op resync click on a valid slot with an empty cursor.
    async fn resync(&mut self) -> SetContent {
        self.click(9, 0, mode::PICKUP).await
    }

    async fn pump_content(&mut self) -> SetContent {
        let deadline = Duration::from_secs(15);
        let step = Duration::from_secs(10);
        let got = tokio::time::timeout(deadline, async {
            loop {
                let (id, payload) = match tokio::time::timeout(step, self.conn.read_packet()).await
                {
                    Ok(Ok(Some(p))) => p,
                    Ok(Ok(None)) => panic!("EOF while awaiting content"),
                    Ok(Err(e)) => panic!("read error while awaiting content: {e}"),
                    Err(_) => panic!("timeout awaiting container_set_content"),
                };
                if id == pkt::play_cb::CONTAINER_SET_CONTENT {
                    return decode_set_content(&payload);
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
                } else if id == pkt::play_cb::SET_HEALTH {
                    let mut r = Reader::new(&payload);
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
                }
            }
        })
        .await;
        got.expect("content within deadline")
    }
}

/// Builds a version-free [`Menu`] mirroring a captured server state.
fn menu_from(state: &SetContent) -> Menu {
    let mut menu = Menu::player();
    #[allow(clippy::cast_sign_loss)]
    menu.set_state_id(state.state_id as u32);
    for (i, item) in state.items.iter().enumerate() {
        menu.set_slot_item(i, item.clone());
    }
    menu.set_carried(state.carried.clone());
    menu
}

/// Asserts our predicted menu equals the server's authoritative content, slot by
/// slot and cursor, localising any divergence.
fn assert_match(label: &str, local: &Menu, server: &SetContent) {
    let mut mismatches = Vec::new();
    for i in 0..PLAYER_MENU_SLOTS {
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
    eprintln!("[{label}] OK — predicted state matches the server slot-by-slot");
}

/// The container-slot argument for `/item replace entity … container.N` that
/// lands in menu slot `menu_slot`: hotbar menu 36..=44 -> container 0..=8, main
/// menu 9..=35 -> container 9..=35.
fn container_arg(menu_slot: usize) -> i32 {
    if (36..=44).contains(&menu_slot) {
        (menu_slot - 36) as i32
    } else {
        menu_slot as i32
    }
}

/// Clears the inventory, seeds `(menu_slot, item, count)` triples, then polls the
/// server (respecting the tick trap) until every seed is reflected. Returns S0.
async fn seed(
    rcon: &mut Rcon,
    session: &mut Session,
    user: &str,
    seeds: &[(usize, &str, i32)],
) -> SetContent {
    rcon.cmd(&format!("clear {user}")).await;
    for &(menu_slot, item, count) in seeds {
        rcon.cmd(&format!(
            "item replace entity {user} container.{} with {item} {count}",
            container_arg(menu_slot)
        ))
        .await;
    }
    for attempt in 0..5 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let s0 = session.resync().await;
        let ok = seeds.iter().all(|&(menu_slot, _, count)| {
            s0.items
                .get(menu_slot)
                .cloned()
                .flatten()
                .map(|i| i.count())
                == Some(count)
        });
        let filled = s0.items.iter().filter(|i| i.is_some()).count();
        if ok && filled == seeds.len() {
            return s0;
        }
        eprintln!("seed not yet reflected (attempt {attempt}, filled={filled}); polling…");
    }
    panic!("seeds never reflected in server state (tick trap or bad seed)");
}

// ---- the oracle ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the creative lodestone-creative server on 127.0.0.1:25570"]
async fn live_click_machine_agrees_with_server() {
    let user = unique_username();
    eprintln!("=== LIVE ITEM-FUL CLICK ORACLE (protocol {PROTOCOL_776}, creative :{PORT}) ===");
    eprintln!("username (unique per run): {user}");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect/authenticate RCON");
    rcon.cmd(&format!("gamemode creative {user}")).await; // no-op until joined; re-issued below
    let mut session = Session::join(&user).await;
    rcon.cmd(&format!("gamemode creative {user}")).await;

    let ctx = PlayerCtx::creative();
    let diamond = "minecraft:diamond"; // stacks to 64 -> matches our default max

    // -- left-click: pick up a whole stack --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 64)]).await;
        let mut menu = menu_from(&s0);
        Click::left(36).apply(&mut menu, ctx.clone());
        let s1 = session.click(36, 0, mode::PICKUP).await;
        assert_match("left-pickup-whole", &menu, &s1);
    }

    // -- right-click: pick up half --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 64)]).await;
        let mut menu = menu_from(&s0);
        Click::right(36).apply(&mut menu, ctx.clone());
        let s1 = session.click(36, 1, mode::PICKUP).await;
        assert_match("right-pickup-half", &menu, &s1);
    }

    // -- right-click place one (pick up whole, then drop one into an empty slot) --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 64)]).await;
        let mut menu = menu_from(&s0);
        Click::left(36).apply(&mut menu, ctx.clone());
        Click::right(37).apply(&mut menu, ctx.clone());
        session.click(36, 0, mode::PICKUP).await;
        let s1 = session.click(37, 1, mode::PICKUP).await;
        assert_match("right-place-one", &menu, &s1);
    }

    // -- quick-move (shift-click) hotbar -> main storage --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 64)]).await;
        let mut menu = menu_from(&s0);
        Click::shift(36).apply(&mut menu, ctx.clone());
        let s1 = session.click(36, 0, mode::QUICK_MOVE).await;
        assert_match("quick-move-shift", &menu, &s1);
    }

    // -- hotbar swap (number key): move a main-slot stack to a hotbar slot --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(9, diamond, 40)]).await;
        let mut menu = menu_from(&s0);
        Click::hotbar_swap(9, 3).apply(&mut menu, ctx.clone()); // main slot 9 <-> hotbar index 3 (menu 39)
        let s1 = session.click(9, 3, mode::SWAP).await;
        assert_match("hotbar-swap", &menu, &s1);
    }

    // -- double-click collect: gather matching stacks to fill the cursor --
    {
        let s0 = seed(
            &mut rcon,
            &mut session,
            &user,
            &[(36, diamond, 32), (37, diamond, 32), (9, diamond, 20)],
        )
        .await;
        let mut menu = menu_from(&s0);
        Click::left(36).apply(&mut menu, ctx.clone()); // cursor = 32
        Click::double(36).apply(&mut menu, ctx.clone()); // gather up to 64
        session.click(36, 0, mode::PICKUP).await;
        let s1 = session.click(36, 0, mode::PICKUP_ALL).await;
        assert_match("double-click-collect", &menu, &s1);
    }

    // -- throw: drop one (Q) and drop whole stack (Ctrl-Q) --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 10)]).await;
        let mut menu = menu_from(&s0);
        Click::drop_one(36).apply(&mut menu, ctx.clone());
        let s1 = session.click(36, 0, mode::THROW).await;
        assert_match("throw-drop-one", &menu, &s1);
    }
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 10)]).await;
        let mut menu = menu_from(&s0);
        Click::drop_stack(36).apply(&mut menu, ctx.clone());
        let s1 = session.click(36, 1, mode::THROW).await;
        assert_match("throw-drop-stack", &menu, &s1);
    }

    // -- left-drag even split across three slots --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 64)]).await;
        let mut menu = menu_from(&s0);
        Click::left(36).apply(&mut menu, ctx.clone()); // cursor = 64
        menu.perform_drag(drag_type::EVEN, &[9, 10, 11], ctx.clone());
        // server: pick up, then start/add/add/add/end (all stale-state resyncs)
        session.click(36, 0, mode::PICKUP).await;
        session
            .click(-999, drag_button(0, drag_type::EVEN), mode::QUICK_CRAFT)
            .await; // start
        session
            .click(9, drag_button(1, drag_type::EVEN), mode::QUICK_CRAFT)
            .await; // add
        session
            .click(10, drag_button(1, drag_type::EVEN), mode::QUICK_CRAFT)
            .await; // add
        session
            .click(11, drag_button(1, drag_type::EVEN), mode::QUICK_CRAFT)
            .await; // add
        let s1 = session
            .click(-999, drag_button(2, drag_type::EVEN), mode::QUICK_CRAFT)
            .await; // end
        assert_match("left-drag-even", &menu, &s1);
    }

    // -- right-drag one-each across three slots --
    {
        let s0 = seed(&mut rcon, &mut session, &user, &[(36, diamond, 64)]).await;
        let mut menu = menu_from(&s0);
        Click::left(36).apply(&mut menu, ctx.clone());
        menu.perform_drag(drag_type::ONE, &[9, 10, 11], ctx);
        session.click(36, 0, mode::PICKUP).await;
        session
            .click(-999, drag_button(0, drag_type::ONE), mode::QUICK_CRAFT)
            .await;
        session
            .click(9, drag_button(1, drag_type::ONE), mode::QUICK_CRAFT)
            .await;
        session
            .click(10, drag_button(1, drag_type::ONE), mode::QUICK_CRAFT)
            .await;
        session
            .click(11, drag_button(1, drag_type::ONE), mode::QUICK_CRAFT)
            .await;
        let s1 = session
            .click(-999, drag_button(2, drag_type::ONE), mode::QUICK_CRAFT)
            .await;
        assert_match("right-drag-one", &menu, &s1);
    }

    eprintln!("=== ITEM-FUL ORACLE PASSED: live server agrees with the click machine ===");
}

/// Packs a drag header (0=start,1=add,2=end) and type into the click button.
fn drag_button(header: i32, kind: i32) -> u8 {
    u8::try_from((header & 3) | ((kind & 3) << 2)).unwrap()
}
