//! Live regression gate for multiplayer collision: on a server whose spawn is
//! far from the world origin, the shell's physics must **stand on the server's
//! ground** — land at the streamed surface and stay there across ticks — instead
//! of falling through absent demo terrain.
//!
//! ## The bug this reproduces
//!
//! Before live collision existed, the shell's physics collided against the
//! offline demo world (a small platform near the origin). At a far spawn the demo
//! world has no blocks under the player, so gravity pulls them down while the
//! server — which places them on its real ground — repeatedly teleports them back
//! up. The result the director observed live was `pos` drifting `66.0 → 65.9 →
//! 64.0` and then oscillating: a rubber-band loop between the shell's fall and the
//! server's correction. Free-fly hid it; `mode=walk` exposes it.
//!
//! ## Why the existing gates missed it
//!
//! `live_world_mesh.rs` never runs physics, and `live_camera_follows_server_spawn`
//! only checks *where* the camera is, not whether it can *stand*. This gate drives
//! the real join path through `Sim::step()` with walk physics and watches the
//! player's vertical motion after the server places them.
//!
//! ## Structure — negative control first, then the invariant
//!
//! 1. **Negative control (pre-fix behaviour):** `collide_against_live_world =
//!    false` forces the old demo-world collision on the live path. We assert the
//!    player *does* drift/fall away from the placement height — this both proves
//!    the reproduction is real and prints the exact drift the director saw. A gate
//!    for this bug that never observes the failure is worthless.
//! 2. **The invariant (post-fix):** default live collision. The player must settle
//!    `on_ground` at the server's surface and **not** fall away from it.
//!
//! Gated behind `--features live` **and** `#[ignore]`. Per §12.52 it **fails**
//! rather than skips when it cannot run — no server, no RCON, or missing vanilla
//! assets is a failure with a fix hint, because a skip here reads like a pass.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_stands_on_server_ground -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::net::NetClient;
use lodestone::sim::Sim;
use lodestone_testsupport::RconClient;

const HOST: &str = "127.0.0.1";
/// The survival 26.2 oracle: game on `:25565`, RCON on `:25566`. Named only as a
/// protocol *number* — the shell never names a version.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// A deterministically far-from-origin world spawn — the region the user actually
/// hit on seed `lodestone`. `Y` is chosen high enough that a `setworldspawn`
/// resolves onto the real surface below it.
const SPAWN_X: i32 = -237;
const SPAWN_Y: i32 = 100;
const SPAWN_Z: i32 = -217;

/// Ticks to observe after the server places the player.
const OBSERVE_TICKS: usize = 80;

/// The outcome of one join: heights and ground-contact observed after placement.
struct Settle {
    /// Player feet-`y` captured the moment the server placed us (post-teleport).
    y_placed: f64,
    /// Lowest feet-`y` seen across the observation window.
    y_min: f64,
    /// Final feet-`y`.
    y_final: f64,
    /// Fraction of observed ticks with `on_ground == true`.
    on_ground_frac: f64,
    /// Whether the player was `on_ground` on the final tick.
    on_ground_final: bool,
    /// Number of live columns the client had streamed.
    loaded: usize,
    /// Live columns that failed to mesh (should stay 0 in a healthy session).
    mesh_drops: u64,
    /// Whether the player's own chunk column was loaded on the final tick. When
    /// true, `live_collision` returned a real snapshot (not the "hold until the
    /// column streams in" fallback), so the observed ground contact is genuine
    /// collision against server blocks — not a frozen hold masquerading as it.
    player_chunk_loaded: bool,
}

#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn player_stands_on_the_server_ground_not_the_demo_world() {
    // The vanilla atlas must load, or `Sim` takes the demo path and never
    // exercises live collision. Fail loud rather than pass vacuously.
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the \
         live server world. Banner: {:?}. Fix: put a vanilla pack at .cache/mc/26.2 \
         (client.jar + generated/reports/blocks.json) or set LODESTONE_ASSETS.",
        probe.asset_banner()
    );
    drop(probe);

    // Push the world spawn far from the origin so a fresh join is placed there
    // deterministically.
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

    // --- Negative control: pre-fix demo-world collision on the live path. ------
    let control = join_and_settle(false);
    let control_drop = control.y_placed - control.y_min;
    eprintln!(
        "[negative control · demo-world collision] placed y={:.2}, min y={:.2}, final y={:.2}, \
         drop={:.2}, on_ground {:.0}% (final {}), live_cols={}",
        control.y_placed,
        control.y_min,
        control.y_final,
        control_drop,
        control.on_ground_frac * 100.0,
        control.on_ground_final,
        control.loaded,
    );
    assert!(
        control_drop > 1.0 || control.on_ground_frac < 0.5,
        "the negative control did NOT reproduce the fall (drop {control_drop:.2} blocks, \
         on_ground {:.0}%). Expected the player to fall through the absent demo terrain and \
         rubber-band against the server. Without a reproduced failure this gate proves nothing.",
        control.on_ground_frac * 100.0
    );

    // Let the server release the player slot before re-joining with the same name.
    std::thread::sleep(Duration::from_secs(2));

    // --- The invariant: live collision stands on the server's ground. ----------
    let live = join_and_settle(true);
    let live_drop = live.y_placed - live.y_min;
    eprintln!(
        "[live collision] placed y={:.2}, min y={:.2}, final y={:.2}, drop={:.2}, \
         on_ground {:.0}% (final {}), live_cols={}, mesh_drops={}, player_chunk_loaded={}",
        live.y_placed,
        live.y_min,
        live.y_final,
        live_drop,
        live.on_ground_frac * 100.0,
        live.on_ground_final,
        live.loaded,
        live.mesh_drops,
        live.player_chunk_loaded,
    );

    assert!(
        live.loaded > 0,
        "client never streamed chunks; cannot judge collision. Fix: start the survival oracle."
    );
    assert!(
        live.mesh_drops == 0,
        "the live session logged {} mesh drop(s) — a live column that a chunk event dirtied \
         produced no geometry (the 'invisible blocks' defect class). The counter is meant to \
         stay 0 in a healthy session; a non-zero value means columns are silently failing to \
         mesh. Check the `live-all-air-column` / `live-guard-rejected` warnings.",
        live.mesh_drops
    );
    assert!(
        live.player_chunk_loaded,
        "the player's own chunk was not loaded, so `live_collision` fell back to the \
         'hold until the column streams in' path — the observed ground contact would be a \
         frozen hold, not real collision. This gate must exercise genuine collision against \
         server blocks; a longer settle or a nearer spawn is needed."
    );
    assert!(
        live.on_ground_final,
        "player is not on the ground after settling (final y={:.2}, min y={:.2}). With live \
         collision they should stand on the server's surface. This is the multiplayer \
         fall-through / rubber-band bug.",
        live.y_final, live.y_min
    );
    assert!(
        live_drop < 1.5,
        "player fell {live_drop:.2} blocks away from the server's placement height (placed \
         {:.2} → min {:.2}); live collision is not holding them on the server's ground. \
         (Negative control drop was {control_drop:.2}.)",
        live.y_placed,
        live.y_min
    );
}

/// Drive the real join path once and observe the player's vertical motion after
/// the server places them. `collide_live=false` forces the pre-fix demo-world
/// collision (the negative control).
fn join_and_settle(collide_live: bool) -> Settle {
    let mut sim = Sim::new(live_config());
    sim.collide_against_live_world = collide_live;
    let demo_spawn = sim.player.position;
    sim.attach_net(NetClient::connect(HOST.into(), PORT, PROTOCOL));

    // Phase 1: drive until the server has placed us (teleport moved us off the
    // demo spawn) and chunks are streaming.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut placed = false;
    while Instant::now() < deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if let Some(net) = sim.net()
            && net.world_dimensions().is_some()
            && !net.loaded_chunks().is_empty()
            && sim.player.position != demo_spawn
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

    // Let the placement teleport + first chunk burst settle so `y_placed` is the
    // server's surface, not a mid-flight sample.
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        std::thread::sleep(Duration::from_millis(20));
    }
    let y_placed = sim.player.position.y;

    // Phase 2: observe. No movement input — pure gravity vs. collision. Time the
    // steps so the gate also reports the per-tick cost of the live-collision
    // snapshot (an upper bound: `step` also polls the net and updates entities).
    let mut y_min = y_placed;
    let mut y_final = y_placed;
    let mut on_ground_hits = 0usize;
    let mut on_ground_final = false;
    let mut step_time = Duration::ZERO;
    for _ in 0..OBSERVE_TICKS {
        let t0 = Instant::now();
        sim.step(1.0 / 20.0);
        step_time += t0.elapsed();
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        y_final = sim.player.position.y;
        y_min = y_min.min(y_final);
        on_ground_final = sim.player.on_ground;
        if on_ground_final {
            on_ground_hits += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let avg_step_us = step_time.as_micros() as f64 / OBSERVE_TICKS as f64;
    if collide_live {
        eprintln!(
            "[live collision] avg step() incl. live-collision snapshot = {avg_step_us:.1} µs \
             over {OBSERVE_TICKS} ticks"
        );
    }

    let loaded = sim.net().map_or(0, |n| n.loaded_chunks().len());
    let mesh_drops = sim.stats.mesh_drops;
    let pcx = (sim.player.position.x.floor() as i32).div_euclid(16);
    let pcz = (sim.player.position.z.floor() as i32).div_euclid(16);
    let player_chunk_loaded = sim
        .net()
        .map(|n| n.loaded_chunks())
        .is_some_and(|cols| cols.iter().any(|c| c.x == pcx && c.z == pcz));
    Settle {
        y_placed,
        y_min,
        y_final,
        on_ground_frac: on_ground_hits as f64 / OBSERVE_TICKS as f64,
        on_ground_final,
        loaded,
        mesh_drops,
        player_chunk_loaded,
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
