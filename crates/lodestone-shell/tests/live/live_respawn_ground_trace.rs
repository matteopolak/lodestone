//! Live regression gate for a player returning to solid ground at the configured
//! survival-oracle world spawn after death.
//!
//! ## Why this exists
//!
//! Respawn placement crosses several live paths: server spawn resolution,
//! death and respawn events, teleport application, collision hold-until-loaded
//! handling, and column arrival. A timing defect can occur when collision reads
//! the player's destination before that column streams in and never corrects
//! the result. Hermetic tests do not exercise this race because they construct
//! their world synchronously; this gate deliberately respawns into an unloaded
//! column and traces the transition until its terrain is present.
//!
//! ## The setup this gate uses to force the race
//!
//! 1. Join and settle onto the world spawn's real ground (same spot
//!    `live_stands_on_server_ground.rs` uses), establishing a real,
//!    server-verified ground height independent of anything under test.
//! 2. RCON-teleport the player thousands of blocks away — past the oracle's
//!    `view-distance=10` (160 blocks) — and drive ticks until the spawn
//!    column has actually left the client's loaded set. This is the
//!    precondition the race needs: without it, the destination may remain
//!    loaded and the hold-until-loaded path is not exercised.
//! 3. RCON-kill the player at the far location, wait for `is_dead()`, then
//!    call `Sim::respawn()` (the death screen's Respawn click) exactly once
//!    — a server with no bed sends the player back to world spawn, a column
//!    the client does **not** currently hold.
//! 4. Trace every tick from that point: local `y`, `on_ground`,
//!    `is_dead()`, whether the player's own chunk column is loaded, and the
//!    real block read directly from the client's world store at the feet and
//!    the cell below (an independent read of the exact store
//!    `Sim::live_collision` queries, through a different accessor —
//!    `ClientHandle::block_at` — so this is a second code path over the same
//!    data, not a restatement of `live_collision`'s own answer).
//!
//! ## Structure — negative control first, then the invariant
//!
//! The pre-live-collision "collide against nothing" path
//! (`collide_against_live_world = false`) is driven through the exact same
//! far-teleport/death/respawn sequence as the negative control: with no
//! terrain to collide against at all, the player must fall away from the
//! server-sent respawn height with no floor. A gate that cannot fail here
//! proves nothing about the live path passing.
//!
//! Gated behind `--features live` **and** `#[ignore]`. Per `DESIGN.md` §12 it
//! **fails** rather than skips when it cannot run — no server, no RCON, or
//! missing vanilla assets is a failure with an actionable hint, because a skip
//! here reads like a pass.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_respawn_ground_trace -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_client::{BlockPos, ChunkPos};
use lodestone_testsupport::{RconClient, unique_username};

const HOST: &str = "127.0.0.1";
/// The survival 26.2 oracle: game on `:25565`, RCON on `:25566`. Named only as
/// a protocol *number* — the shell never names a version.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// The same plains land spawn `live_stands_on_server_ground.rs` and
/// `live_death_respawn.rs` use — real, walkable ground at y≈69–70, far from
/// the world origin so a `(0, 64, 0)` fallback cannot coincide with it.
const SPAWN_X: i32 = -45;
const SPAWN_Y: i32 = 72;
const SPAWN_Z: i32 = -377;

/// The oracle's `view-distance=10` is 160 blocks; this offset is more than
/// twenty times that, so the spawn column is guaranteed to leave the client's
/// loaded set once the player has been here a few seconds — the precondition
/// the whole gate depends on. `FAR_Y` is only a starting altitude (real
/// terrain, real generation) — nothing about this test depends on where the
/// player lands there, only on being far enough for spawn to unload.
const FAR_X: i32 = SPAWN_X + 5000;
const FAR_Y: i32 = 100;
const FAR_Z: i32 = SPAWN_Z - 5000;

/// Ticks traced *after* the respawn is confirmed, once tracing has started.
const TRACE_TICKS: usize = 150;

/// Ticks to wait for the respawn confirmation before giving up — tracing
/// starts the instant `Sim::respawn()` is called (not after confirmation), so
/// this bounds the whole loop, not just the wait.
const CONFIRM_TIMEOUT_TICKS: usize = 300;

/// One tick's worth of the trace.
#[derive(Clone, Copy, Debug)]
struct TickSample {
    tick: usize,
    x: f64,
    y: f64,
    z: f64,
    on_ground: bool,
    dead: bool,
    /// Whether the player's own chunk column is loaded in the client's world
    /// store — the exact predicate `Sim::live_collision` holds the player on.
    chunk_loaded: bool,
    /// The block state directly at the player's feet cell, read straight from
    /// the client-owned world store (`ClientHandle::block_at`) — `None` means
    /// that section is not currently held by the client.
    block_at_feet: Option<u32>,
    /// The block one cell below the feet — the cell that decides whether the
    /// player is standing on something or embedded in it.
    block_below_feet: Option<u32>,
}

/// Everything one full join → far-teleport → kill → respawn run observed.
struct Outcome {
    /// Ground height sampled after the initial join settle, before anything
    /// else happens — an outside-of-the-respawn-path measurement of "real
    /// ground here", independent of the mechanism under test.
    join_settled_y: f64,
    /// X/Z alongside `join_settled_y`, so a y discrepancy against the
    /// respawn-settled position can be told apart from "different cell
    /// entirely" (independent searches landing on different, both-valid
    /// standable ground) versus "same cell, different height" (a real
    /// mapping defect).
    join_settled_x: f64,
    join_settled_z: f64,
    /// Whether the spawn column was confirmed to have left the client's
    /// loaded set before the kill — the setup precondition. `false` here
    /// means the race was never forced and the trace proves nothing about
    /// it.
    spawn_column_unloaded_before_kill: bool,
    /// The local player's `y` on the first tick `is_dead()` read `false`
    /// again after the kill — i.e. the tick `NetUpdate::Teleport` (which rides
    /// in on the same batch as `NetUpdate::Respawned`) applied the server's
    /// destination. This is "the server-sent respawn `y`" as observed by the
    /// client, not assumed.
    respawn_teleport_y: f64,
    /// Full per-tick trace starting the instant `Sim::respawn()` was called
    /// (index `0` is the first tick *after* that call), through confirmation
    /// and `TRACE_TICKS` beyond it.
    samples: Vec<TickSample>,
    /// Index into `samples` of the first tick whose column was loaded.
    first_loaded_tick: Option<usize>,
    /// Index into `samples` of the tick the respawn was confirmed
    /// (`is_dead() == false && respawn_count` increased).
    confirmed_at_tick: Option<usize>,
    /// Index into `samples` of the tick the respawn *position* actually
    /// landed — `confirmed_at_tick` plus however many ticks
    /// `NetUpdate::Teleport` lagged `NetUpdate::Respawned` by. `respawn_teleport_y`
    /// is read at this tick, not at `confirmed_at_tick`.
    teleport_landed_tick: Option<usize>,
}

#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn respawn_far_from_the_loaded_area_lands_on_real_ground_not_inside_it() {
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the \
         live server world. Banner: {:?}. Fix: put a vanilla pack at .cache/mc/26.2 \
         (client.jar + generated/reports/blocks.json) or set LODESTONE_ASSETS.",
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

    // --- Negative control: no terrain to collide against at all. ---------------
    let control = run_repro(&mut rcon, false);
    print_trace("negative control (no terrain)", &control);
    assert!(
        control.spawn_column_unloaded_before_kill,
        "setup precondition failed in the control run: the spawn column never left the \
         client's loaded set, so the far-teleport did not force the race this gate exists \
         to observe. Increase FAR_X/FAR_Z or the settle window."
    );
    let control_final = control
        .samples
        .last()
        .expect("the control traced at least one tick");
    // With no terrain to collide against, `on_ground` can never legitimately
    // become true — the "falling" the control reproduces is not a monotonic
    // drop (the server corrects our claimed position most ticks, snapping us
    // back near the respawn height — the same rubber-band
    // `live_stands_on_server_ground.rs` documents), so the discriminating
    // signal is "never actually rests on anything", not "ends up far below
    // where it started".
    let control_on_ground_ticks = control.samples.iter().filter(|s| s.on_ground).count();
    let control_drop = control.respawn_teleport_y
        - control
            .samples
            .iter()
            .map(|s| s.y)
            .fold(f64::INFINITY, f64::min);
    assert!(
        control_on_ground_ticks == 0,
        "the negative control did NOT reproduce a floorless fall: {} of {} ticks after respawn \
         reported on_ground=true. With no terrain to collide against the player must never \
         settle on anything. Without a reproduced failure this gate proves nothing about the \
         live path passing. (min y reached was {:.2} below the respawn-teleport y {:.2}.)",
        control_on_ground_ticks,
        control.samples.len(),
        control_drop,
        control.respawn_teleport_y,
    );

    std::thread::sleep(Duration::from_secs(2));

    // --- The invariant: live collision resolves the real column. ---------------
    let live = run_repro(&mut rcon, true);
    print_trace("live collision", &live);
    assert!(
        live.spawn_column_unloaded_before_kill,
        "setup precondition failed in the live run: the spawn column never left the client's \
         loaded set, so respawn landed somewhere already streamed and this run cannot tell \
         apart 'no bug' from 'the race was never forced'. Increase FAR_X/FAR_Z or the settle \
         window."
    );

    // The core diagnostic: collect every tick whose column was loaded but
    // whose y sits far from the respawn teleport's y (i.e. genuinely
    // resolved, not merely held) — the shape a stale pre-column-arrival read
    // would leave behind. Collected, not asserted per-tick, so a run with
    // multiple bad ticks reports all of them.
    let mut mismatches = Vec::new();
    for sample in &live.samples {
        if sample.chunk_loaded
            && !sample.dead
            && sample.block_below_feet.is_some_and(|id| id == 0)
            && sample.block_at_feet.is_some_and(|id| id == 0)
            && !sample.on_ground
            && live
                .first_loaded_tick
                .is_some_and(|first| sample.tick > first + 5)
        {
            // Loaded, air at and below the feet, not on the ground, five
            // ticks after the column arrived — falling through genuinely
            // absent ground under a *loaded* column, as distinct from the
            // ordinary settle immediately after the column streams in.
            mismatches.push(format!(
                "tick {}: y={:.2}, chunk_loaded=true, block_at_feet=air, \
                 block_below_feet=air, on_ground=false, {} ticks after first load",
                sample.tick,
                sample.y,
                sample.tick - live.first_loaded_tick.unwrap_or(sample.tick)
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "the player was falling through a genuinely loaded, genuinely air column well after \
         it streamed in:\n{}",
        mismatches.join("\n")
    );

    let final_sample = live
        .samples
        .last()
        .expect("the live run traced at least one tick");
    assert!(
        final_sample.chunk_loaded,
        "the player's own column was still not loaded after {} ticks post-respawn \
         (final y={:.2}). Either the server never streamed the spawn column back, or the \
         client never adopted it.",
        live.samples.len(),
        final_sample.y,
    );
    assert!(
        final_sample.on_ground,
        "the player is not on_ground {} ticks after respawn (final y={:.2}, respawn-teleport \
         y={:.2}). Expected to settle standing on the server's real ground.",
        live.samples.len(),
        final_sample.y,
        live.respawn_teleport_y,
    );
    let final_drop = (live.respawn_teleport_y - final_sample.y).abs();
    assert!(
        final_drop < 3.0,
        "the player's final y ({:.2}) is {:.2} blocks away from the server-sent respawn y \
         ({:.2}) — outside the tolerance for 'the teleport height was already close to real \
         ground'. (Negative control's drop was {:.2}.)",
        final_sample.y,
        final_drop,
        live.respawn_teleport_y,
        control_drop,
    );
    // Independent cross-check against the pre-death measurement: the world
    // spawn ground the player originally settled on before ever dying, read
    // through the *join* path rather than the respawn path. Both searches
    // are handed the same RCON `setworldspawn` coordinate, but each is free
    // to resolve to a different nearby standable cell — so this is only a
    // real finding when the two land on essentially the *same* x/z and still
    // disagree on y. A large x/z difference means "two independently valid
    // cells", not a defect, and is reported rather than failed on.
    let xz_dist = ((final_sample.x - live.join_settled_x).powi(2)
        + (final_sample.z - live.join_settled_z).powi(2))
    .sqrt();
    let vs_join = (final_sample.y - live.join_settled_y).abs();
    eprintln!(
        "[join vs respawn] join=({:.2}, {:.2}, {:.2}) respawn=({:.2}, {:.2}, {:.2}) \
         xz_dist={xz_dist:.2} y_diff={vs_join:.2}",
        live.join_settled_x,
        live.join_settled_y,
        live.join_settled_z,
        final_sample.x,
        final_sample.y,
        final_sample.z,
    );
    assert!(
        xz_dist > 3.0 || vs_join < 3.0,
        "the respawn-settled position ({:.2}, {:.2}, {:.2}) is within {xz_dist:.2} blocks (x/z) \
         of the join-settled position ({:.2}, {:.2}, {:.2}) measured at the same world spawn \
         before the player ever died, but the two disagree on y by {vs_join:.2} blocks — same \
         ground, two different resolved heights.",
        final_sample.x,
        final_sample.y,
        final_sample.z,
        live.join_settled_x,
        live.join_settled_y,
        live.join_settled_z,
    );
}

/// One full join -> settle -> far-teleport -> unload-confirm -> kill ->
/// respawn -> trace run.
fn run_repro(rcon: &mut RconClient, collide_live: bool) -> Outcome {
    let username = unique_username();
    let mut sim = Sim::new(live_config());
    sim.collide_against_live_world = collide_live;
    let demo_spawn = sim.player().position;
    // §4.1(c): `Sim::connect` threads the shell's one `World` into the
    // client. `connect_as`, not `connect`: a live gate needs a fresh
    // identity per run (a shared offline name is a shared player file, and a
    // dead player is held on the death screen, which sends no chunks).
    sim.connect_as(HOST.into(), PORT, PROTOCOL, username.clone());

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
        "server never placed '{username}' within 60s (still at demo spawn {demo_spawn:?}). \
         Fix: start the survival 26.2 oracle on :25565 and run with `--features live`."
    );

    // Settle onto real ground before doing anything else — an
    // outside-the-respawn-path measurement of ground height at world spawn.
    for _ in 0..30 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        std::thread::sleep(Duration::from_millis(20));
    }
    let join_settled = sim.player().position;
    let (join_settled_x, join_settled_y, join_settled_z) =
        (join_settled.x, join_settled.y, join_settled.z);

    // Phase 2: teleport far away over RCON and drive ticks until the spawn
    // column has actually left the client's loaded set — the precondition
    // that forces a respawn to land on an unloaded column.
    let tp_reply = rcon.cmd(&format!("tp {username} {FAR_X} {FAR_Y} {FAR_Z}"));
    eprintln!("[{username}] RCON `tp` far away -> {tp_reply:?}");
    let spawn_chunk = ChunkPos {
        x: SPAWN_X.div_euclid(16),
        z: SPAWN_Z.div_euclid(16),
    };
    let unload_deadline = Instant::now() + Duration::from_secs(60);
    let mut spawn_column_unloaded_before_kill = false;
    while Instant::now() < unload_deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if let Some(net) = sim.net()
            && !net.is_chunk_loaded(spawn_chunk)
        {
            spawn_column_unloaded_before_kill = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // A little extra settle time far away so any in-flight streaming at the
    // far location also quiesces before we kill.
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        std::thread::sleep(Duration::from_millis(20));
    }

    // Phase 3: kill at the far location and wait for the death event.
    let kill_reply = rcon.cmd(&format!("kill {username}"));
    eprintln!("[{username}] RCON `kill` -> {kill_reply:?}");
    let death_deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_dead = false;
    while Instant::now() < death_deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if sim.is_dead() {
            saw_dead = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        saw_dead,
        "'{username}' was never observed dead within 15s of the RCON kill — the kill did not \
         land, or the death event never reached Sim."
    );

    // Phase 4+5: respawn (the death screen's button click) and trace **from
    // the same tick the call is made** — not from confirmation onward. The
    // spawn column may reload while confirmation is pending, so omitting this
    // interval can hide the unloaded-column race. Capture every tick in the
    // interval. `NetUpdate::Respawned` (which flips `is_dead()` and bumps
    // `respawn_count`) and the position-carrying `NetUpdate::Teleport` are two
    // separate updates and are not guaranteed to land in the same
    // `poll_net` batch. "Confirmed" and "position actually landed" are
    // therefore tracked as separate ticks below.
    let pre_respawn_y = sim.player().position.y;
    let respawns_before = sim.respawn_count();
    sim.respawn();

    let mut samples = Vec::with_capacity(CONFIRM_TIMEOUT_TICKS + TRACE_TICKS);
    let mut first_loaded_tick = None;
    let mut respawn_teleport_y = None;
    let mut confirmed_at_tick = None;
    let mut teleport_landed_tick = None;
    for tick in 0..(CONFIRM_TIMEOUT_TICKS + TRACE_TICKS) {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();

        let player = sim.player();
        let dead = sim.is_dead();
        let feet = BlockPos {
            x: player.position.x.floor() as i32,
            y: player.position.y.floor() as i32,
            z: player.position.z.floor() as i32,
        };
        let below = BlockPos {
            y: feet.y - 1,
            ..feet
        };
        let pcx = feet.x.div_euclid(16);
        let pcz = feet.z.div_euclid(16);
        let (chunk_loaded, block_at_feet, block_below_feet) = match sim.net() {
            Some(net) => (
                net.is_chunk_loaded(ChunkPos { x: pcx, z: pcz }),
                net.block_at(feet),
                net.block_at(below),
            ),
            None => (false, None, None),
        };
        if chunk_loaded && first_loaded_tick.is_none() {
            first_loaded_tick = Some(tick);
        }
        if confirmed_at_tick.is_none() && !dead && sim.respawn_count() > respawns_before {
            confirmed_at_tick = Some(tick);
        }
        // The server's respawn *position* landing: confirmed, and the
        // position has actually moved off wherever we died (a real respawn
        // in this gate's setup always relocates by tens of blocks, so a
        // half-block threshold cannot mistake ordinary settle jitter for it).
        if teleport_landed_tick.is_none()
            && confirmed_at_tick.is_some()
            && (player.position.y - pre_respawn_y).abs() > 0.5
        {
            teleport_landed_tick = Some(tick);
            respawn_teleport_y = Some(player.position.y);
        }
        samples.push(TickSample {
            tick,
            x: player.position.x,
            y: player.position.y,
            z: player.position.z,
            on_ground: player.on_ground,
            dead,
            chunk_loaded,
            block_at_feet,
            block_below_feet,
        });
        // Once the respawn position has actually landed, keep tracing for
        // TRACE_TICKS more and then stop — no reason to run the full budget
        // if respawn took only a few ticks.
        if let Some(landed) = teleport_landed_tick
            && tick >= landed + TRACE_TICKS
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Dump the raw per-tick trace unconditionally, *before* either check
    // below can panic — a run that never confirms is exactly the run whose
    // log matters most, and a panic before this point would have thrown the
    // whole trace away with it.
    dump_samples(&format!("[{username}] collide_live={collide_live}"), &samples);
    if confirmed_at_tick.is_none() {
        panic!(
            "'{username}' respawn was never confirmed within {} ticks of calling \
             Sim::respawn() (respawn_count still {}). Full trace was logged above.",
            CONFIRM_TIMEOUT_TICKS,
            sim.respawn_count()
        );
    }
    let respawn_teleport_y = respawn_teleport_y.unwrap_or_else(|| {
        panic!(
            "'{username}' respawn was confirmed at tick {:?} (dead cleared, respawn_count \
             incremented) but the position never moved more than 0.5 blocks off the \
             pre-respawn y ({pre_respawn_y:.2}) within {} ticks. Either the destination \
             coincided with the death y (should not happen at this gate's far-teleport \
             setup), or the placement teleport never arrived. Full trace was logged above.",
            confirmed_at_tick,
            CONFIRM_TIMEOUT_TICKS,
        )
    });

    Outcome {
        join_settled_x,
        join_settled_y,
        join_settled_z,
        spawn_column_unloaded_before_kill,
        respawn_teleport_y,
        samples,
        first_loaded_tick,
        confirmed_at_tick,
        teleport_landed_tick,
    }
}

/// The summary line for a completed [`Outcome`] — the full per-tick table was
/// already dumped by [`dump_samples`] inside `run_repro`, unconditionally, so
/// a run that panics before returning still has its trace on record.
fn print_trace(label: &str, outcome: &Outcome) {
    eprintln!(
        "\n=== [{label}] join_settled_y={:.2}, spawn_unloaded_before_kill={}, \
         respawn_teleport_y={:.2}, first_loaded_tick={:?}, confirmed_at_tick={:?}, \
         teleport_landed_tick={:?}, samples={} ===",
        outcome.join_settled_y,
        outcome.spawn_column_unloaded_before_kill,
        outcome.respawn_teleport_y,
        outcome.first_loaded_tick,
        outcome.confirmed_at_tick,
        outcome.teleport_landed_tick,
        outcome.samples.len(),
    );
}

fn dump_samples(label: &str, samples: &[TickSample]) {
    eprintln!("\n--- per-tick trace: {label} ({} ticks) ---", samples.len());
    for s in samples {
        eprintln!(
            "  tick={:3} x={:8.3} y={:8.3} z={:8.3} on_ground={:5} dead={:5} chunk_loaded={:5} \
             feet={:?} below={:?}",
            s.tick,
            s.x,
            s.y,
            s.z,
            s.on_ground,
            s.dead,
            s.chunk_loaded,
            s.block_at_feet,
            s.block_below_feet,
        );
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
