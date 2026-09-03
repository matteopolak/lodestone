//! Live chat-component resolution oracle (protocol 776, survival :25565).
//!
//! ## What this proves, and why it needs a real server
//!
//! A death message is not a string on the wire — it is a *structured* component:
//! `translate("death.attack.mob", [victim, translate("entity.minecraft.spider")])`.
//! The client is what turns that into words, by resolving each `translate` key
//! against the language pack (`en_us.json`). The reported defect —
//! `<name> WAS SLAIN BY ENTITY.MINECRAFT.SPIDER` — is what you see when the outer
//! key resolves (the built-in stub table happens to carry `death.attack.mob`) but
//! the *entity* key does not, so it renders verbatim.
//!
//! This gate stages a genuine death-by-mob on the live server, captures the
//! server's real `system_chat` component through the client's `ClientEvent`
//! stream, and shows two renderings of the *same* captured component:
//!
//! - **Negative control** — `Text::to_plain_string()`, which uses the model's
//!   tiny built-in table. This reproduces the exact defect: the entity key is
//!   left raw. Its output is printed and asserted, so the gate is provably
//!   discriminating rather than vacuously green.
//! - **Fixed** — [`lodestone_model::Text::resolve`] against the **real** vanilla
//!   `en_us.json`, read out of the downloaded `client.jar` (the same asset the
//!   renderer loads). The expected words "was slain by" / "Spider" therefore
//!   originate from a real Mojang asset, not from a table typed into this test.
//!
//! Run with:
//! ```text
//! cargo test -p lodestone-game --features live-chat --test live_chat_resolve -- --ignored --nocapture
//! ```
#![cfg(feature = "live-chat")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_assets::{Language, ZipSource};
use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};

use lodestone_model::{ChatKind, Text};
use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const RCON_PORT: u16 = 25566;
const RCON_PASSWORD: &str = "lodestone";

/// Locates the downloaded `client.jar` the same way the shell's asset loader
/// does: `LODESTONE_ASSETS` if set, else the newest `.cache/mc/<ver>/client.jar`
/// found walking up from the working directory.
fn find_client_jar() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("LODESTONE_ASSETS") {
        let jar = PathBuf::from(root).join("client.jar");
        return jar.is_file().then_some(jar);
    }
    let cwd = std::env::current_dir().ok()?;
    for base in cwd.ancestors() {
        let mc = base.join(".cache/mc");
        let Ok(entries) = std::fs::read_dir(&mc) else {
            continue;
        };
        // Highest-sorted version directory wins (26.2 > 1.20.1).
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("client.jar").is_file())
            .collect();
        versions.sort();
        if let Some(best) = versions.pop() {
            return Some(best.join("client.jar"));
        }
    }
    None
}

/// Loads the real vanilla English language table from `client.jar`.
fn load_real_en_us() -> Language {
    let jar = find_client_jar().expect(
        "no client.jar found — set LODESTONE_ASSETS to a pack root or populate \
         .cache/mc/<ver>/client.jar (the same asset the live client renders from)",
    );
    let bytes = std::fs::read(&jar).unwrap_or_else(|e| panic!("read {}: {e}", jar.display()));
    let zip = ZipSource::from_bytes(bytes).expect("open client.jar as a zip");
    Language::en_us_from_source(&zip)
        .expect("en_us.json parsed")
        .expect("client.jar carries assets/minecraft/lang/en_us.json")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565 (RCON :25566)"]
async fn death_message_resolves_against_real_language_pack() {
    println!("=== LIVE CHAT RESOLUTION (protocol 776, survival :25565) ===");

    // Surface a fatal "adapter rejected packet" (a v26-2 decode gap) rather than a
    // silent event-stream close, as in the other live oracles.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lodestone_client=debug")),
        )
        .with_test_writer()
        .try_init();

    // Load the real table up front so a missing asset fails fast, before we
    // perturb the live world.
    let lang = load_real_en_us();
    println!("loaded real en_us.json: {} keys", lang.len());
    assert!(
        lang.len() > 3000,
        "en_us.json looks truncated ({} keys); expected the full ~7k-key vanilla table",
        lang.len()
    );

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
            "connect to lodestone-survival on 127.0.0.1:25565 — recreate it with \
             ./scripts/live-oracles/survival.sh",
        );

    // Collect every System/GameInfo chat component the server sends. The death
    // broadcast arrives here as a `system_chat` packet.
    let system_texts: Arc<Mutex<Vec<Text>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&system_texts);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::Chat { text, kind, .. } => {
                    if matches!(kind, ChatKind::System | ChatKind::GameInfo) {
                        eprintln!("!!! system chat: {:?}", text.to_plain_string());
                        sink.lock().unwrap().push(text);
                    }
                }
                ClientEvent::Death { message } => {
                    // Death also arrives as a dedicated event carrying the same
                    // component; capture it too so the gate does not depend on
                    // which packet the server used.
                    eprintln!("!!! death event: {:?}", message.to_plain_string());
                    sink.lock().unwrap().push(message);
                }
                ClientEvent::Disconnect { reason } => {
                    eprintln!("!!! driver saw Disconnect: {}", reason.to_plain_string());
                    break;
                }
                _ => {}
            }
        }
        eprintln!("!!! drain loop ended (event stream closed)");
    });

    // Reach Play: the server must know our player before RCON targets it.
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
        "player {user} never appeared in the live tab list — is lodestone-survival on :25565 in Play? (alive={})",
        handle.is_alive()
    );
    println!("player is in-game");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect RCON on 127.0.0.1:25566 (password 'lodestone') — is lodestone-survival up?");

    // Survival + showDeathMessages so the server broadcasts the death line.
    println!(
        "  RCON gamemode -> {:?}",
        rcon.cmd(&format!("gamemode survival {user}")).await
    );
    let _ = rcon.cmd("gamerule showDeathMessages true").await;

    // Settle on a position so we can summon the killer beside the player.
    let pos = poll_until(Duration::from_secs(15), Duration::from_millis(200), || async {
        handle.position()
    })
    .await
    .expect("client never reported a position");
    println!("  player at ({:.1}, {:.1}, {:.1})", pos.x, pos.y, pos.z);

    // Summon a stationary, silent spider next to the player, then deal fatal
    // mob-attack damage *attributed to that spider*. A mob attacker with no held
    // item yields the `death.attack.mob` component whose killer argument is
    // `translate("entity.minecraft.spider")` — exactly the reported defect.
    println!(
        "  RCON summon   -> {:?}",
        rcon.cmd(&format!(
            "execute at {user} run summon minecraft:spider ~ ~1 ~ \
             {{NoAI:1b,Silent:1b,PersistenceRequired:1b}}"
        ))
        .await
    );
    // Give the client a moment to be a valid damage target and drain any join
    // spam, then kill it by the spider.
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!(
        "  RCON damage   -> {:?}",
        rcon.cmd(&format!(
            "damage {user} 1000 minecraft:mob_attack by @e[type=minecraft:spider,limit=1,sort=nearest]"
        ))
        .await
    );

    // Wait for the death component to arrive on the event stream.
    let captured = poll_until(Duration::from_secs(15), Duration::from_millis(150), || {
        let texts = Arc::clone(&system_texts);
        let user = user.clone();
        async move {
            texts
                .lock()
                .unwrap()
                .iter()
                .find(|t| {
                    // The defect is visible in the *stub* rendering: the entity
                    // key survives unresolved.
                    let stub = t.to_plain_string();
                    stub.contains("entity.minecraft.spider")
                        || (stub.contains(&user) && stub.contains("slain"))
                })
                .cloned()
        }
    })
    .await;

    let captured = captured.unwrap_or_else(|| {
        let all: Vec<String> = system_texts
            .lock()
            .unwrap()
            .iter()
            .map(Text::to_plain_string)
            .collect();
        panic!(
            "no death-by-spider component arrived within 15s (alive={}). System messages seen: {all:?}",
            handle.is_alive()
        )
    });

    let mut checked = 0usize;

    // --- Negative control: the pre-fix rendering (model's built-in stub table) ---
    let before = captured.to_plain_string();
    println!("NEGATIVE CONTROL (stub table, pre-fix): {before:?}");
    assert!(
        before.contains("entity.minecraft.spider"),
        "expected the raw entity key to survive in the stub rendering (the exact defect); got {before:?}"
    );
    checked += 1;

    // --- Fixed: resolve the same component against the real en_us.json ---
    let after = captured.resolve(&lang.translator()).to_plain_string();
    println!("FIXED (real en_us.json):               {after:?}");
    assert!(
        !after.contains("entity.minecraft.spider"),
        "the entity key must be resolved away against the real language pack; got {after:?}"
    );
    checked += 1;
    assert!(
        after.contains("Spider"),
        "the resolved killer name must be the real vanilla word 'Spider' from en_us.json; got {after:?}"
    );
    checked += 1;
    assert!(
        after.contains("was slain by"),
        "the resolved death line must read '... was slain by ...' from en_us.json; got {after:?}"
    );
    checked += 1;

    // The strongest form: the whole vanilla line, victim + verb + killer, with
    // every word originating from the real asset.
    let expected = format!("{user} was slain by Spider");
    assert_eq!(after, expected, "resolved line must match vanilla's exact wording");
    checked += 1;

    // Resolution must materially change the output — otherwise the 'fix' is a
    // no-op and the assertions above could pass on a coincidence.
    assert_ne!(before, after, "the fixed rendering must differ from the defective one");
    checked += 1;

    // Ground the table itself against the real asset: entity + death keys that
    // are NOT hand-typed as *expected output* resolve to real words.
    assert_eq!(lang.get("entity.minecraft.spider"), Some("Spider"));
    assert_eq!(lang.get("death.attack.mob"), Some("%1$s was slain by %2$s"));
    checked += 1;

    const EXPECTED_CHECKS: usize = 7;
    assert!(
        checked >= EXPECTED_CHECKS,
        "anti-vacuity floor: only {checked} comparisons ran, expected >= {EXPECTED_CHECKS}"
    );

    // Cleanup: remove the summoned spider.
    let _ = rcon.cmd("kill @e[type=minecraft:spider]").await;

    println!(
        "=== CHAT RESOLUTION ORACLE PASSED: {checked} comparisons — the server's real death \
         component rendered {before:?} through the stub and {after:?} against the real en_us.json ==="
    );

    handle.shutdown();
    let _ = drain.await;
}
