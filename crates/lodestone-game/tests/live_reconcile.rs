//! Live predict → click → reconcile oracle against a real vanilla 26.2 server.
//!
//! This is the external-grounding test for [`lodestone_game::reconcile`]: it
//! joins the live `lodestone-mc262` server (protocol 776) on `127.0.0.1:25565`,
//! forces the server to emit its *authoritative* `container_set_content` for the
//! player inventory (window 0), decodes it, and drives it through
//! [`ClientMenu::reconcile`]. Hermetic unit tests can prove the click machine is
//! self-consistent; only a real server can prove it *agrees with the server* —
//! which is the entire point of reconciliation.
//!
//! ## Isolation: no version crate
//!
//! A version-free crate cannot take a dependency on a `crates/versions/*`
//! family. So rather than reuse `V770Adapter`, this test hand-drives the
//! handshake using only the SHARED wire crates ([`lodestone_core`],
//! [`lodestone_net`]) plus a handful of documented protocol-776 packet-id
//! constants declared locally below. Those constants are facts (like a port
//! number), not a code dependency: deleting `crates/versions/26.2` would not
//! touch this file, and `cargo xtask check-isolation` stays clean.
//!
//! ## Run it
//!
//! ```text
//! cargo test -p lodestone-game --features live-reconcile --test live_reconcile -- --ignored --nocapture
//! ```
//!
//! ## Offline-mode landmine (see the module rustdoc in `live_server.rs`)
//!
//! In offline mode the server derives the account UUID from the *username*
//! (`OfflinePlayer:<name>`) and discards the UUID we send, so a shared username
//! shares one persisted player file. A player killed by a mob persists
//! `Health = 0.0` and is then held on the death screen, receiving zero chunks
//! and (crucially here) an interaction-hostile state. We therefore
//! [`unique_username`] per run AND decode `set_health`, reporting `0.0` loudly
//! and sending `client_command(perform_respawn)` if we ever inherit a corpse.
#![cfg(feature = "live-reconcile")]

use lodestone_testsupport::unique_username;
use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;
use lodestone_game::reconcile::{ClientMenu, ServerUpdate};
use lodestone_net::Connection;
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const PROTOCOL_776: i32 = 776;

/// The player inventory (`InventoryMenu`) always occupies window 0 and has
/// exactly 46 slots: result(1) + crafting 2×2(4) + armour(4) + main(27) +
/// hotbar(9) + offhand(1). This is the known value the live content is checked
/// against — a mismatch means our menu model disagrees with the real server.
const PLAYER_WINDOW: i32 = 0;
const PLAYER_MENU_SLOTS: usize = 46;

/// Documented protocol-776 packet ids (from Mojang's generated report; mirrored
/// in `crates/versions/26.2/src/generated/packet_ids.rs`). Local constants, not
/// a crate dependency.
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
        pub const CONTAINER_SET_SLOT: i32 = 20;
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

/// `ContainerInput::PICKUP` ordinal (mode 0 on the wire).
const INPUT_PICKUP: i32 = 0;

// ---- Wire helpers (protocol-776 primitives) --------------------------------

fn write_string(w: &mut Writer, s: &str) {
    w.var_i32(s.len() as i32);
    w.bytes(s.as_bytes());
}

fn read_string(r: &mut Reader) -> String {
    let len = r.var_i32().expect("string length") as usize;
    let bytes = r.bytes(len).expect("string bytes");
    String::from_utf8_lossy(bytes).into_owned()
}

/// Decodes one `ItemStack` in the clientbound optional form: a VarInt count,
/// and if `count > 0`, the item holder and its component patch. A fresh survival
/// player's inventory is entirely empty, so a non-empty stack here is
/// unexpected; we surface it loudly rather than silently misparsing (this crate
/// deliberately does not carry a version-specific component decoder).
fn read_optional_item(r: &mut Reader) -> Option<ItemStack> {
    let count = r.var_i32().expect("item count");
    if count <= 0 {
        return None;
    }
    let item_id = r.var_i32().expect("item holder id");
    panic!(
        "live server sent a NON-EMPTY stack (count={count}, item raw id={item_id}); this oracle \
         assumes a fresh empty survival inventory and has no component decoder — seed inventory \
         support (creative server) is required to reconcile item-ful clicks"
    );
}

/// Serverbound `container_click` for a no-op PICKUP on an empty slot with an
/// empty cursor. Layout (protocol 776): CONTAINER_ID varint, state_id varint,
/// slot short, button byte, ContainerInput varint, changed-slots map (varint
/// length + entries), carried HashedStack (optional bool). Empty cursor and no
/// changed slots means zero item encoding is required.
fn encode_noop_click(window: i32, state_id: i32, slot: i16) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window);
    w.var_i32(state_id);
    w.i16(slot);
    w.u8(0); // button 0 (left)
    w.var_i32(INPUT_PICKUP);
    w.var_i32(0); // changed slots: empty map
    w.u8(0); // carried HashedStack EMPTY == Optional present-flag false
    w.into_vec()
}

/// A decoded `container_set_content` for the player inventory.
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
        items.push(read_optional_item(&mut r));
    }
    let carried = read_optional_item(&mut r);
    r.ensure_empty()
        .expect("container_set_content decode consumes the whole packet (zero trailing bytes)");
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

// ---- The oracle ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a live Minecraft 26.2 server on 127.0.0.1:25565"]
async fn live_reconcile_agrees_with_server() {
    let username = unique_username();
    eprintln!("=== LIVE RECONCILE ORACLE (protocol {PROTOCOL_776}) ===");
    eprintln!("username (unique per run): {username}");

    let mut conn = Connection::connect((HOST, PORT)).await.expect(
        "connect to lodestone-mc262 on 127.0.0.1:25565 — ensure the shared 26.2 server is running",
    );

    // ---- Handshake: intention (next state = 2, login) ----
    let mut hs = Writer::default();
    hs.var_i32(PROTOCOL_776);
    write_string(&mut hs, HOST);
    hs.u16(PORT);
    hs.var_i32(2);
    conn.write_packet(pkt::hs_sb::INTENTION, &hs.into_vec())
        .await
        .expect("send handshake");

    // ---- Login: hello (name + 16-byte uuid) ----
    let mut hello = Writer::default();
    write_string(&mut hello, &username);
    hello.bytes(Uuid::new_v4().as_bytes());
    conn.write_packet(pkt::login_sb::HELLO, &hello.into_vec())
        .await
        .expect("send login hello");

    // ---- Drive login → configuration → play ----
    #[derive(PartialEq, Debug)]
    enum Phase {
        Login,
        Configuration,
        Play,
    }
    let mut phase = Phase::Login;
    let mut reached_play = false;

    let overall = Duration::from_secs(45);
    let read_timeout = Duration::from_secs(10);

    // The authoritative content the oracle asserts against, captured live.
    let mut authoritative: Option<SetContent> = None;
    let mut forced_resync_sent = false;
    let mut last_health: Option<f32> = None;

    let result = tokio::time::timeout(overall, async {
        loop {
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => {
                    eprintln!("read timeout in phase {phase:?} (server quiet)");
                    return;
                }
                Ok(Ok(Some(p))) => p,
                Ok(Ok(None)) => {
                    eprintln!("clean EOF in phase {phase:?}");
                    return;
                }
                Ok(Err(e)) => panic!("read error in phase {phase:?}: {e}"),
            };

            match phase {
                Phase::Login => {
                    if packet_id == pkt::login_cb::COMPRESSION {
                        let mut r = Reader::new(&payload);
                        let threshold = r.var_i32().expect("compression threshold");
                        conn.set_compression(threshold);
                    } else if packet_id == pkt::login_cb::LOGIN_FINISHED {
                        conn.write_packet(pkt::login_sb::LOGIN_ACKNOWLEDGED, &[])
                            .await
                            .expect("send login_acknowledged");
                        phase = Phase::Configuration;
                    } else if packet_id == pkt::login_cb::ENCRYPTION_REQUEST {
                        panic!(
                            "server requested encryption — this test targets an offline-mode \
                             server (online-mode=false)"
                        );
                    } else if packet_id == pkt::login_cb::DISCONNECT {
                        let mut r = Reader::new(&payload);
                        panic!("login disconnect: {}", read_string(&mut r));
                    }
                }
                Phase::Configuration => {
                    if packet_id == pkt::cfg_cb::KEEP_ALIVE {
                        conn.write_packet(pkt::cfg_sb::KEEP_ALIVE, &payload)
                            .await
                            .expect("echo config keep_alive");
                    } else if packet_id == pkt::cfg_cb::SELECT_KNOWN_PACKS {
                        // Echo the server's pack list back to accept it.
                        conn.write_packet(pkt::cfg_sb::SELECT_KNOWN_PACKS, &payload)
                            .await
                            .expect("echo known packs");
                    } else if packet_id == pkt::cfg_cb::FINISH_CONFIGURATION {
                        conn.write_packet(pkt::cfg_sb::FINISH_CONFIGURATION, &[])
                            .await
                            .expect("ack finish_configuration");
                        phase = Phase::Play;
                        reached_play = true;
                        eprintln!("reached Play");
                    }
                    // All other configuration packets (registry data, tags,
                    // brand, feature flags…) are read and discarded.
                }
                Phase::Play => {
                    if packet_id == pkt::play_cb::KEEP_ALIVE {
                        conn.write_packet(pkt::play_sb::KEEP_ALIVE, &payload)
                            .await
                            .expect("echo play keep_alive");
                    } else if packet_id == pkt::play_cb::CHUNK_BATCH_FINISHED {
                        let mut w = Writer::default();
                        w.f32(16.0);
                        conn.write_packet(pkt::play_sb::CHUNK_BATCH_RECEIVED, &w.into_vec())
                            .await
                            .expect("ack chunk batch");
                    } else if packet_id == pkt::play_cb::SET_HEALTH {
                        let mut r = Reader::new(&payload);
                        let health = r.f32().expect("health");
                        last_health = Some(health);
                        if health <= 0.0 {
                            eprintln!(
                                "!! set_health = {health} — inherited a DEAD player (offline-mode \
                                 trap). Sending client_command(perform_respawn)."
                            );
                            let mut w = Writer::default();
                            w.var_i32(0); // perform_respawn
                            conn.write_packet(pkt::play_sb::CLIENT_COMMAND, &w.into_vec())
                                .await
                                .expect("send perform_respawn");
                        }
                    } else if packet_id == pkt::play_cb::LOGIN {
                        // Join game received; the inventory menu now exists on
                        // the server. Force it to broadcast its authoritative
                        // window-0 content by sending a click with a stale
                        // state id (any no-op click with state_id != server's
                        // triggers broadcastFullState()).
                        if !forced_resync_sent {
                            let stale = 30_000;
                            conn.write_packet(
                                pkt::play_sb::CONTAINER_CLICK,
                                &encode_noop_click(PLAYER_WINDOW, stale, 9),
                            )
                            .await
                            .expect("send forced-resync click");
                            forced_resync_sent = true;
                            eprintln!("sent forced-resync click (stale state_id {stale})");
                        }
                    } else if packet_id == pkt::play_cb::CONTAINER_SET_CONTENT {
                        let content = decode_set_content(&payload);
                        eprintln!(
                            "container_set_content: window {PLAYER_WINDOW}, state_id {}, {} slots, \
                             carried={:?}",
                            content.state_id,
                            content.items.len(),
                            content.carried.is_some()
                        );
                        authoritative = Some(content);
                        return; // captured the authoritative snapshot
                    } else if packet_id == pkt::play_cb::CONTAINER_SET_SLOT {
                        // Not expected for an empty inventory, but decode-safe.
                        let mut r = Reader::new(&payload);
                        let _w = r.var_i32();
                        let _s = r.var_i32();
                        let _slot = r.i16();
                        let _item = read_optional_item(&mut r);
                    }
                }
            }
        }
    })
    .await;

    assert!(result.is_ok(), "oracle timed out before capturing content");
    assert!(reached_play, "never reached Play");
    let content = authoritative.expect(
        "server never sent container_set_content for window 0 — if set_health was 0.0 above, we \
         inherited a dead player; otherwise the forced-resync click was rejected",
    );

    if let Some(h) = last_health {
        eprintln!("last set_health seen: {h}");
    }

    // ---- Assertion 1: the real server's inventory shape matches our model ----
    assert_eq!(
        content.items.len(),
        PLAYER_MENU_SLOTS,
        "server's player window has a different slot count than our Menu model"
    );
    let non_empty = content.items.iter().filter(|i| i.is_some()).count();
    eprintln!("non-empty slots in live inventory: {non_empty} (fresh survival player => 0)");
    assert_eq!(non_empty, 0, "expected a fresh empty survival inventory");
    assert!(content.carried.is_none(), "expected an empty cursor");

    // Build the version-free client model from the live server's authoritative
    // snapshot and stamp it with the server's state id.
    let mut menu = Menu::player();
    #[allow(clippy::cast_sign_loss)]
    menu.set_state_id(content.state_id as u32);
    let faithful = ClientMenu::new(menu);

    // ---- Assertion 2 (agreement): a faithful model does not get corrected ----
    let mut agree = faithful.clone();
    let recon = agree.reconcile(ServerUpdate::SetContent {
        state_id: lodestone_model::ContainerStateId::from_wire(content.state_id),
        items: content.items.clone(),
        carried: content.carried.clone(),
    });
    eprintln!("agreement reconcile: corrected = {}", recon.corrected);
    assert!(
        !recon.corrected,
        "a model that mirrors the live server should reconcile with NO correction"
    );

    // ---- Assertion 3 (divergence detected): a wrong prediction IS corrected --
    // Inject a misprediction (a phantom stack the server does not have) and feed
    // the SAME authoritative live snapshot back. Reconciliation must detect the
    // divergence and roll the prediction back to the server's truth.
    let mut wrong_menu = Menu::player();
    #[allow(clippy::cast_sign_loss)]
    wrong_menu.set_state_id(content.state_id as u32);
    wrong_menu.set_slot_item(
        9,
        Some(ItemStack::new("minecraft:stone".parse().unwrap(), 64)),
    );
    let mut diverged = ClientMenu::new(wrong_menu);
    let recon = diverged.reconcile(ServerUpdate::SetContent {
        state_id: lodestone_model::ContainerStateId::from_wire(content.state_id),
        items: content.items.clone(),
        carried: content.carried.clone(),
    });
    eprintln!("divergence reconcile: corrected = {}", recon.corrected);
    assert!(
        recon.corrected,
        "a phantom stack absent from the live server must be corrected on reconcile"
    );
    assert!(
        diverged.menu().slot_item(9).is_none(),
        "the phantom stack must be rolled back to the server's (empty) truth"
    );

    eprintln!("=== ORACLE PASSED: live server agrees with the reconcile model ===");
}
