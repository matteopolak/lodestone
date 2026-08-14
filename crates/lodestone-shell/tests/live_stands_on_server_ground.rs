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
//! server's correction. The free-fly camera hid it, because it replaced the whole
//! physics tick; walking physics — now the only mode, since that fix deleted the
//! free-fly toggle — exposes it.
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

use std::sync::Mutex;
use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_controller::Action;
use lodestone_testsupport::{RconClient, unique_username};

/// Both live gates join the oracle under the *same* player name and both
/// `setworldspawn`, so they must never run concurrently — a second join with a
/// duplicate name is kicked by the server. `cargo test` runs `#[test]`s in
/// parallel by default; serialize them through one process-wide lock.
static SERVER_LOCK: Mutex<()> = Mutex::new(());

const HOST: &str = "127.0.0.1";
/// The survival 26.2 oracle: game on `:25565`, RCON on `:25566`. Named only as a
/// protocol *number* — the shell never names a version.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// A deterministically land spawn on plains — the director moved world spawn
/// here (the old ocean spawn meshed water as opaque cubes and had no walkable
/// surface). Ground is at y≈69–70; `Y` is set a little above so the join resolves
/// onto the real surface.
const SPAWN_X: i32 = -45;
const SPAWN_Y: i32 = 72;
const SPAWN_Z: i32 = -377;

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
    let _serialized = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let (control_sim, control) = join_and_settle(false);
    drop(control_sim);
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
    let (live_sim, live) = join_and_settle(true);
    drop(live_sim);
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
/// collision (the negative control). Returns the still-connected `Sim` so the
/// caller can keep driving it (e.g. to observe a jump on the same session).
fn join_and_settle(collide_live: bool) -> (Sim, Settle) {
    let mut sim = Sim::new(live_config());
    sim.collide_against_live_world = collide_live;
    let demo_spawn = sim.player().position;
    // §4.1(c): `Sim::connect` threads the shell\'s one `World` into the
    // client, so the session fold lands where the HUD accessors read.
    // `connect_as`, not `connect`: a live gate needs a fresh identity per run
    // (a shared offline name is a shared player file, and a dead player is held
    // on the death screen, which sends no chunks). `connect` is the *stable*
    // persisted offline identity, which is production's job, not a gate's.
    sim.connect_as(HOST.into(), PORT, PROTOCOL, unique_username());

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

    // Let the placement teleport + first chunk burst settle so `y_placed` is the
    // server's surface, not a mid-flight sample.
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        std::thread::sleep(Duration::from_millis(20));
    }
    let y_placed = sim.player().position.y;

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
        y_final = sim.player().position.y;
        y_min = y_min.min(y_final);
        on_ground_final = sim.player().on_ground;
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
    let pcx = (sim.player().position.x.floor() as i32).div_euclid(16);
    let pcz = (sim.player().position.z.floor() as i32).div_euclid(16);
    let player_chunk_loaded = sim
        .net()
        .map(|n| n.loaded_chunks())
        .is_some_and(|cols| cols.iter().any(|c| c.x == pcx && c.z == pcz));
    let settle = Settle {
        y_placed,
        y_min,
        y_final,
        on_ground_frac: on_ground_hits as f64 / OBSERVE_TICKS as f64,
        on_ground_final,
        loaded,
        mesh_drops,
        player_chunk_loaded,
    };
    (sim, settle)
}

/// The vertical arc of a single jump observed on an already-settled session.
struct JumpArc {
    /// Feet-`y` the instant before the jump input is applied (the ground we
    /// leave from and must return to).
    ground_y: f64,
    /// Highest feet-`y` reached during the arc.
    apex_y: f64,
    /// Lowest feet-`y` seen during the arc — the value that exposes a
    /// "glitch down" *below* the launch ground.
    min_y: f64,
    /// Feet-`y` once the arc has fully completed.
    final_y: f64,
    /// Whether the player is `on_ground` on the final tick.
    on_ground_final: bool,
    /// Server `TeleportPlayer` corrections adopted between launch and landing.
    /// A clean vanilla jump draws none; a non-zero count is the fingerprint of
    /// the server rejecting the ascent and snapping the camera down.
    teleports_during: usize,
}

/// Ticks to observe the jump arc. A vanilla jump rises ~1.25 blocks over ~11
/// ticks and returns over ~11 more; 40 leaves margin for the landing.
const JUMP_TICKS: usize = 40;

/// Launch a single jump on an already-driven session and observe the full arc.
/// The caller passes a `Sim` that has just been settled by [`join_and_settle`];
/// `ground_y` is sampled at the moment of launch, so this is meaningful on both
/// the live (on-ground) session and the demo negative control (mid-fall).
fn observe_jump(sim: &mut Sim) -> JumpArc {
    let ground_y = sim.player().position.y;
    let tp_before = sim.teleport_count;

    // Vanilla jump is edge-triggered on a grounded tick: hold Space for exactly
    // one tick, then release so the arc is a single clean parabola rather than a
    // repeated bunny-hop.
    sim.input_mut(|i| i.set(Action::Jump, true));
    sim.step(1.0 / 20.0);
    let _ = sim.drain_meshes();
    let _ = sim.drain_removals();
    sim.input_mut(|i| i.set(Action::Jump, false));

    let mut apex_y = sim.player().position.y.max(ground_y);
    let mut min_y = sim.player().position.y.min(ground_y);
    for _ in 0..JUMP_TICKS {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        let y = sim.player().position.y;
        apex_y = apex_y.max(y);
        min_y = min_y.min(y);
        std::thread::sleep(Duration::from_millis(20));
    }

    JumpArc {
        ground_y,
        apex_y,
        min_y,
        final_y: sim.player().position.y,
        on_ground_final: sim.player().on_ground,
        teleports_during: (sim.teleport_count - tp_before) as usize,
    }
}

/// A jump on live-server terrain rises, peaks, and lands back on the *same*
/// ground with no net downward displacement and no dip below the launch height.
///
/// This is the gate for the user's "jumping makes me glitch down" report. The
/// suspected mechanism was that adopting a server `TeleportPlayer` mid-ascent
/// (landed in `8ca2d6c`) zeroes velocity and snaps the camera down — so the arc
/// records how many teleports were adopted during the jump and asserts the
/// trajectory is a clean vanilla parabola regardless.
#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn a_jump_returns_to_the_same_ground_without_glitching_down() {
    let _serialized = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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
             with `./scripts/live-oracles/survival.sh` and run with `--features live`."
        )
    });
    let reply = rcon.cmd(&format!("setworldspawn {SPAWN_X} {SPAWN_Y} {SPAWN_Z}"));
    assert!(
        reply.to_lowercase().contains("set the world spawn"),
        "RCON setworldspawn did not take: {reply:?}"
    );

    // --- Negative control: jump on the pre-fix demo-collision path. -------------
    // With collision reading the absent demo world the player is already falling,
    // so the jump neither arrests nor recovers — it keeps drifting down. This is
    // exactly "today's behaviour" the fix must beat.
    let (mut control_sim, _control_settle) = join_and_settle(false);
    let control_jump = observe_jump(&mut control_sim);
    drop(control_sim);
    eprintln!(
        "[negative control · demo-world jump] ground y={:.2}, apex y={:.2}, min y={:.2}, \
         final y={:.2}, on_ground_final={}, teleports_during={}",
        control_jump.ground_y,
        control_jump.apex_y,
        control_jump.min_y,
        control_jump.final_y,
        control_jump.on_ground_final,
        control_jump.teleports_during,
    );
    assert!(
        !control_jump.on_ground_final || control_jump.final_y < control_jump.ground_y - 1.0,
        "the negative control did NOT reproduce the fall: it ended on_ground={} at final \
         y={:.2} vs launch y={:.2}. Expected the demo-collision player to keep sinking \
         through absent ground. Without a reproduced failure this gate proves nothing.",
        control_jump.on_ground_final,
        control_jump.final_y,
        control_jump.ground_y,
    );

    std::thread::sleep(Duration::from_secs(2));

    // --- The invariant: a real jump on live terrain is a clean vanilla arc. -----
    let (mut live_sim, settle) = join_and_settle(true);
    assert!(
        settle.player_chunk_loaded && settle.on_ground_final,
        "precondition failed: the player must be standing on loaded server ground before \
         we can judge a jump (on_ground_final={}, player_chunk_loaded={}).",
        settle.on_ground_final,
        settle.player_chunk_loaded,
    );
    let jump = observe_jump(&mut live_sim);
    drop(live_sim);
    eprintln!(
        "[live collision · jump] ground y={:.2}, apex y={:.2} (rise {:.2}), min y={:.2}, \
         final y={:.2} (net {:+.2}), on_ground_final={}, teleports_during={}",
        jump.ground_y,
        jump.apex_y,
        jump.apex_y - jump.ground_y,
        jump.min_y,
        jump.final_y,
        jump.final_y - jump.ground_y,
        jump.on_ground_final,
        jump.teleports_during,
    );

    assert!(
        jump.apex_y - jump.ground_y > 0.9,
        "the jump barely lifted (apex {:.2}, ground {:.2}, rise {:.2}). A vanilla jump rises \
         ~1.25 blocks; a suppressed rise means the ascent was cancelled — check whether a \
         server teleport ({} adopted during the jump) or the collision hold-path zeroed the \
         upward velocity.",
        jump.apex_y,
        jump.ground_y,
        jump.apex_y - jump.ground_y,
        jump.teleports_during,
    );
    assert!(
        jump.min_y > jump.ground_y - 0.3,
        "the player glitched DOWN through the launch ground during the jump (min y={:.2} vs \
         ground y={:.2}). This is the reported 'jumping makes me glitch down' defect — the \
         arc dipped below where it started. {} server teleport(s) were adopted mid-jump.",
        jump.min_y,
        jump.ground_y,
        jump.teleports_during,
    );
    assert!(
        (jump.final_y - jump.ground_y).abs() < 0.2,
        "the jump did not return to the launch ground (final y={:.2}, ground y={:.2}, net \
         {:+.2}). A clean jump lands where it left; a net displacement means the arc was \
         corrupted (teleports adopted during jump: {}).",
        jump.final_y,
        jump.ground_y,
        jump.final_y - jump.ground_y,
        jump.teleports_during,
    );
    assert!(
        jump.on_ground_final,
        "player is not on_ground after the jump arc (final y={:.2}); the jump should end \
         standing on the same ground.",
        jump.final_y,
    );
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
