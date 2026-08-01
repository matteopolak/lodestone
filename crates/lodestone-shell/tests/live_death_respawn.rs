//! Live regression gate for death handling: when the server kills the local
//! player, the shell must **survive, gate on the death screen's Respawn click,
//! respawn, and resume streaming chunks** — not strand itself on a terminal
//! death screen forever, and not (issue #103's actual behaviour change) ride
//! through death with no screen at all.
//!
//! ## The bug this reproduces
//!
//! The old shell answered `ClientEvent::Death` by setting
//! `SessionPhase::Ended("player died")`, which flips the app to its terminal
//! Error screen. Meanwhile the client library's `RespawnPolicy::Automatic`
//! already answered the death packet with an unconditional `ClientAction::
//! Respawn` and kept the session alive underneath — so the *library* recovered
//! while the *shell* had declared the game over. One spider ended the session;
//! survival was untestable past the first mob.
//!
//! The status line `server: player dead (no chunks)` was also a lie: being dead
//! does not unload chunks. The server holds the death screen, but the world the
//! client already streamed stays put, and it streams again on respawn.
//!
//! ## Issue #103: manual respawn, not automatic
//!
//! `crate::net::run` now builds the client with `RespawnPolicy::Manual`
//! instead of the library's default `Automatic` — the whole point of a death
//! *screen* is that something gates the respawn on a click, and the automatic
//! policy answered the death packet before the shell (or this test) ever got
//! a chance to react. This test now stands in for that click: once
//! `sim.is_dead()` is observed, it calls `Sim::respawn` exactly once — the
//! same call the death screen's Respawn button makes
//! (`MenuAction::Respawn` → `apply_menu_action` in `app.rs`) — before waiting
//! for the server's confirmation. A run that never reaches `is_dead()` before
//! its deadline, or whose `sim.respawn()` call never gets a confirmed
//! respawn, is exactly the failure this exists to catch: the manual gate
//! wired up with nothing to click it.
//!
//! ## Structure — negative control first, then the invariant
//!
//! 1. **Negative control (pre-fix behaviour):** `recover_from_death = false`
//!    restores the old "death is terminal" path. After an RCON kill we assert the
//!    session goes **`Ended`** — the stuck-on-the-death-screen state the director
//!    hit. A gate for this bug that never observes the failure is worthless.
//! 2. **The invariant (post-fix):** default death handling, plus the manual
//!    `sim.respawn()` call standing in for the screen's button. After the same
//!    kill the session stays **`Connected`**, the player is **alive again**
//!    (`respawn_count >= 1`, `is_dead() == false`), and chunks are streaming with
//!    the player's own column loaded.
//!
//! Gated behind `--features live` **and** `#[ignore]`. Per §12.52 it **fails**
//! rather than skips when it cannot run — no server, no RCON, or missing vanilla
//! assets is a failure with a fix hint, because a skip here reads like a pass.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_death_respawn -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::sim::{SessionPhase, Sim};
use lodestone_testsupport::RconClient;

const HOST: &str = "127.0.0.1";
/// The survival 26.2 oracle: game on `:25565`, RCON on `:25566`. Named only as a
/// protocol *number* — the shell never names a version.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// Plains land spawn — the same walkable spawn the collision gate uses. A dry,
/// mob-free landing means the respawn resolves onto real ground.
const SPAWN_X: i32 = -45;
const SPAWN_Y: i32 = 72;
const SPAWN_Z: i32 = -377;

/// The outcome of a join + kill: what the shell did with the death.
struct DeathOutcome {
    /// Session phase after the death/respawn window.
    phase: SessionPhase,
    /// Whether the shell was still flagged dead at the end.
    dead_final: bool,
    /// Respawns observed (one per server-confirmed respawn).
    respawns: u64,
    /// Live columns streamed at the end.
    loaded: usize,
    /// Whether the player's own column was loaded at the end.
    player_chunk_loaded: bool,
    /// Server-reported health at the end (post-respawn should be > 0).
    health: Option<f32>,
}

/// The invariant: a server kill is survived, respawned, and resumes streaming.
#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn a_server_kill_is_survived_respawned_and_keeps_streaming() {
    // The vanilla atlas must load, or `Sim` takes the demo path and never
    // reaches a live server to die on. Fail loud rather than pass vacuously.
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the \
         live server world. Banner: {:?}.",
        probe.asset_banner()
    );
    drop(probe);

    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!(
            "cannot reach RCON at {RCON_ADDR}: {e}. Fix: start the survival 26.2 oracle \
             (game :25565, RCON :25566) with `./scripts/live-oracles/survival.sh` and run \
             with `--features live`."
        )
    });
    let reply = rcon.cmd(&format!("setworldspawn {SPAWN_X} {SPAWN_Y} {SPAWN_Z}"));
    assert!(
        reply.to_lowercase().contains("set the world spawn"),
        "RCON setworldspawn did not take: {reply:?}"
    );

    // --- Negative control: the pre-fix "death is terminal" path. ---------------
    let control = join_kill_and_watch(&mut rcon, false);
    eprintln!(
        "[negative control · death is terminal] phase={:?}, dead_final={}, respawns={}, \
         loaded={}, player_chunk_loaded={}, health={:?}",
        control.phase,
        control.dead_final,
        control.respawns,
        control.loaded,
        control.player_chunk_loaded,
        control.health,
    );
    assert!(
        matches!(control.phase, SessionPhase::Ended(_)),
        "the negative control did NOT reproduce the stuck-dead bug: after the kill the session \
         was {:?}, expected SessionPhase::Ended (the terminal death screen the shell used to \
         strand itself on). Without a reproduced failure this gate proves nothing.",
        control.phase
    );

    // Let the server release the player slot before re-joining with the same name.
    std::thread::sleep(Duration::from_secs(2));

    // --- The invariant: death is survived and the session keeps going. ---------
    let live = join_kill_and_watch(&mut rcon, true);
    eprintln!(
        "[live death handling] phase={:?}, dead_final={}, respawns={}, loaded={}, \
         player_chunk_loaded={}, health={:?}",
        live.phase,
        live.dead_final,
        live.respawns,
        live.loaded,
        live.player_chunk_loaded,
        live.health,
    );

    assert!(
        !matches!(live.phase, SessionPhase::Ended(_)),
        "the session ended on death ({:?}) — the shell still treats death as terminal. It must \
         stay Connected and ride through the respawn.",
        live.phase
    );
    assert_eq!(
        live.phase,
        SessionPhase::Connected,
        "after respawn the session should be Connected, was {:?}.",
        live.phase
    );
    assert!(
        live.respawns >= 1,
        "no respawn was observed after the kill (respawns={}). The client library auto-respawns \
         on death, so a zero here means either the kill did not land or the shell never saw the \
         `Respawned` event.",
        live.respawns
    );
    assert!(
        !live.dead_final,
        "the shell is still flagged dead after the respawn window — the dead state was never \
         cleared by `NetUpdate::Respawned`."
    );
    assert!(
        live.loaded > 0 && live.player_chunk_loaded,
        "chunks did not resume streaming at the respawn position (loaded={}, \
         player_chunk_loaded={}). Respawn must land the player back in a streamed column.",
        live.loaded,
        live.player_chunk_loaded,
    );
    assert!(
        live.health.is_some_and(|h| h > 0.0),
        "post-respawn health is not positive ({:?}); the player was not actually revived.",
        live.health
    );
}

/// Drive the real join path, wait until the server has placed us and chunks are
/// streaming, then kill the player over RCON and observe how the shell handles
/// it. `recover=false` sets the pre-fix "death is terminal" seam for the negative
/// control.
fn join_kill_and_watch(rcon: &mut RconClient, recover: bool) -> DeathOutcome {
    let mut sim = Sim::new(live_config());
    sim.recover_from_death = recover;
    let demo_spawn = sim.player().position;
    // §4.1(c): `Sim::connect` threads the shell\'s one `World` into the
    // client, so the session fold lands where the HUD accessors read.
    sim.connect(HOST.into(), PORT, PROTOCOL);

    // Phase 1: drive until the server has placed us and chunks are streaming.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut placed = false;
    while Instant::now() < deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if let Some(net) = sim.net()
            && net.world_dimensions().is_some()
            && !net.loaded_chunks().is_empty()
            && sim.player().position != demo_spawn
        {
            placed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        placed,
        "server never placed the player within 60s (still at demo spawn {demo_spawn:?}). \
         Fix: start the survival 26.2 oracle on :25565 and run with `--features live`."
    );

    // Settle a moment so we are alive and grounded before the kill.
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !sim.is_dead(),
        "player was already flagged dead before we killed them — the oracle is not in a clean \
         alive state."
    );
    let respawns_before = sim.respawn_count();

    // Kill every player (our unique-named client is the only one). `/kill` sets
    // health to zero → a real death, bypassing any invulnerability.
    let kill_reply = rcon.cmd("kill @a");
    eprintln!("[kill] RCON `kill @a` -> {kill_reply:?}");

    // Phase 2: watch the death/respawn window. With `RespawnPolicy::Manual`
    // (issue #103) nothing respawns on its own any more, so this stands in
    // for the death screen's Respawn click: the first tick `is_dead()` is
    // observed, call `sim.respawn()` exactly once, then keep watching for the
    // server's confirmation. `respawn_sent` is only meaningful on the fixed
    // path (`recover == true`) — the terminal control ends the session before
    // any of this matters.
    let mut saw_dead = false;
    let mut respawn_sent = false;
    let watch_deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < watch_deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if sim.is_dead() {
            saw_dead = true;
            if recover && !respawn_sent {
                sim.respawn();
                respawn_sent = true;
            }
        }
        // Stop early once we have a clean recovery (only meaningful on the fixed
        // path; the terminal control never satisfies it and runs the full window).
        if recover
            && sim.respawn_count() > respawns_before
            && !sim.is_dead()
            && sim.session_phase() == SessionPhase::Connected
        {
            // Give chunk streaming a few ticks to catch up at the new position.
            for _ in 0..15 {
                sim.step(1.0 / 20.0);
                let _ = sim.drain_meshes();
                let _ = sim.drain_removals();
                std::thread::sleep(Duration::from_millis(20));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("[watch] saw_dead flag at some tick = {saw_dead}, respawn_sent = {respawn_sent}");
    if recover {
        assert!(
            respawn_sent,
            "never observed is_dead() within the watch window, so `sim.respawn()` was never \
             called — the kill did not land, or the death event never reached `Sim`."
        );
    }

    let loaded = sim.net().map_or(0, |n| n.loaded_chunks().len());
    let pcx = (sim.player().position.x.floor() as i32).div_euclid(16);
    let pcz = (sim.player().position.z.floor() as i32).div_euclid(16);
    let player_chunk_loaded = sim
        .net()
        .map(|n| n.loaded_chunks())
        .is_some_and(|cols| cols.iter().any(|c| c.x == pcx && c.z == pcz));

    DeathOutcome {
        phase: sim.session_phase(),
        dead_final: sim.is_dead(),
        respawns: sim.respawn_count().saturating_sub(respawns_before),
        loaded,
        player_chunk_loaded,
        health: sim.health(),
    }
}

fn live_config() -> Config {
    Config {
        mode: Mode::Window,
        host: HOST.into(),
        port: PORT,
        protocol: PROTOCOL,
        connect_in_window: true,
        render_distance: 8,
        ..Config::default()
    }
}
