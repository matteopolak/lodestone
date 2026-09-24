//! Live end-to-end gate: a **server-sent** chat message must reach the shell's
//! display log through the real wiring, colour codes intact.
//!
//! The HUD's chat rendering is unit-tested (colour runs, zero-width codes, the
//! fade-out) and its rasterisation is GPU-tested (`hud_chat_text_rasterizes_to_pixels`),
//! but none of that proves a message the **server** actually sent survives the
//! chain the live client walks:
//!
//! ```text
//! live SYSTEM_CHAT → ClientEvent::Chat → Text::to_legacy_string
//!   → NetUpdate::Chat → NetClient::poll() → Sim::recent_chat() → HUD
//! ```
//!
//! This closes that gap. It connects the shell's own [`NetClient`] to the live
//! vanilla-26.2 oracle, broadcasts a uniquely-tagged **red** message over RCON
//! (`tellraw`, so the colour is server-authored, not synthesised here), and
//! polls the net client until that exact line arrives — asserting both that the
//! token is present and that the red `§c` legacy code survived the wire, which
//! is the whole reason chat is flattened with [`Text::to_legacy_string`] rather
//! than to plain text.
//!
//! We use the **oracle on :25567 (RCON :25575)** rather than mc262 on :25565 for
//! the same reason the entity gate does: it is the one server where we can both
//! *inject* a known message (RCON) and *observe* it arrive over the public API.
//! mc262 has no reachable RCON, so nothing could be broadcast there on demand.
//! Do not "fix" this to point at :25565 — the gate would then have no way to send
//! the message it asserts on.
//!
//! Gated behind the `live` feature (which compiles the v26-2 family into the
//! registry) **and** `#[ignore]`, so the default `cargo test` stays hermetic and
//! version-free. Run it explicitly:
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live_chat -- --ignored --nocapture
//! ```
//!
//! Per §12.52 this test **fails** rather than skips when it cannot run — no
//! server or no RCON is a failure, because a skip here reads exactly like a pass
//! and this is the only thing that proves the chat wiring end to end.
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};
use lodestone_testsupport::{RconClient, unique_username};

const GAME_HOST: &str = "127.0.0.1";
/// The summon+observe oracle: game on :25567, RCON on :25575. A real
/// vanilla-26.2 server (protocol 776), the one target where we can both *inject*
/// a known chat line and *watch* it arrive over the public API. mc262 on :25565
/// has no reachable RCON, so a message cannot be broadcast on demand there.
const GAME_PORT: u16 = 25567;
const RCON_ADDR: &str = "127.0.0.1:25575";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_26_2: i32 = 776;

#[test]
#[ignore = "requires the live vanilla-26.2 oracle on :25567 (+ RCON :25575)"]
fn server_sent_chat_reaches_the_display_log_with_colour() {
    // A token unique to this run, so an ambient/left-over message can never be
    // mistaken for the one we broadcast.
    let token = format!(
        "lodestonechatprobe{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    // --- Connect the shell's own net client to the live oracle. --------------
    // `connect_as`, not `connect`: a live gate needs a fresh identity per run
    // (a shared offline name is a shared player file, and a dead player is held
    // on the death screen, which sends no chunks). `connect` is the *stable*
    // persisted offline identity, which is production's job, not a gate's.
    let net = NetClient::connect_as(GAME_HOST.to_owned(), GAME_PORT, PROTOCOL_26_2, None, unique_username());

    // Wait until the bot is actually in the world; drain poll() meanwhile so the
    // net thread's update channel can't grow unbounded while we wait.
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    let mut in_world = false;
    while Instant::now() < ready_deadline {
        let _ = net.poll();
        if !net.loaded_chunks().is_empty() || !net.entities().is_empty() {
            in_world = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        in_world,
        "the shell's NetClient never reached the world on {GAME_HOST}:{GAME_PORT} — connection \
         or login fault (is the vanilla-26.2 oracle up?), not the chat path"
    );

    // --- Broadcast a red, uniquely-tagged message over RCON. -----------------
    // `tellraw @a` targets every player (our single bot included) and carries an
    // explicit colour, so the `§c` code we assert on is server-authored.
    {
        let mut r = RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
            "oracle RCON reachable/authenticated at 127.0.0.1:25575 — is the vanilla-26.2 \
             oracle up? A missing RCON is a harness failure, not a passing chat path.",
        );
        r.cmd(&format!(
            "tellraw @a {{\"text\":\"{token}\",\"color\":\"red\"}}"
        ));
    }

    // --- Poll the shell's net path until our line arrives. -------------------
    let mut matched: Option<String> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        for update in net.poll() {
            if let NetUpdate::Chat { text, .. } = update {
                // Flatten the same `Text` the shell stores in its `ChatFeed`;
                // colour survives as legacy `§` codes iff the adapter preserved
                // it (Claim 2 below).
                let line = text.resolve(&|_| None).to_legacy_string();
                if line.contains(&token) {
                    matched = Some(line);
                    break;
                }
            }
        }
        if matched.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let line = matched.unwrap_or_else(|| {
        panic!(
            "the server-broadcast message never reached NetClient::poll() as a chat update. \
             Either SYSTEM_CHAT isn't dispatching or the net→shell chat seam is broken — the \
             HUD would show an empty log on a live server despite this being wired."
        )
    });

    // Claim 1 (shell-owned, must hold): the server's message crosses the whole
    // net → shell chat seam and lands in the display log. This is the core
    // deliverable and is proven the moment we get here with the token present.
    println!("OK  server chat reached the display log through the shell: {line:?}");

    // Claim 2 (depends on the v26-2 chat decode): the server-authored colour must
    // survive the wire. `tellraw ... color:red` becomes the legacy `§c` code via
    // Text::to_legacy_string — *if* the adapter delivers a styled Text. v26-2
    // currently flattens chat NBT with `plain_text_from_nbt_component` +
    // `Text::literal`, dropping colour before it reaches `ClientEvent::Chat`
    // (adapter.rs SYSTEM_CHAT/DISGUISED_CHAT/PLAYER_CHAT). The fix (decode with
    // `Text::from_nbt`) is routed to impl-v26-2; when it lands, this goes green
    // with no shell change, and the HUD lights up server colours. Until then
    // this assertion is the honest tracker that coloured chat does NOT yet reach
    // pixels live — a flat-white log is the §12.24 "wired to nothing" shape.
    assert!(
        line.contains('\u{00a7}'),
        "chat reached the log but carries no legacy formatting codes — colour was flattened away \
         upstream in the v26-2 chat decode (Text::literal(plain_text_from_nbt_component(..))). \
         Routed to impl-v26-2: decode with Text::from_nbt. (line: {line:?})"
    );
    assert!(
        line.contains("\u{00a7}c"),
        "the server-authored red (`§c`) code did not survive to the display log — see the v26-2 \
         chat-decode fix routed to impl-v26-2. (line: {line:?})"
    );
}
