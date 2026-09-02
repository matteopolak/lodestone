//! Live **public-API physics gate**: drive a bot's movement through
//! `lodestone-client`'s public `ClientHandle` surface against a real vanilla
//! 26.2 server, and prove the server *validates and accepts* the walk.
//!
//! Gated behind the `live-v770` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic and version-free. Run it against a real server
//! (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-client --features live-v770 --test live_physics_bot -- --ignored --nocapture
//! ```
//!
//! # What this gate proves, and what it deliberately does not
//!
//! Its sibling `crates/protocol/v770/tests/live_physics.rs` is the **white-box
//! arithmetic gate**: it hand-builds `move_player_pos_rot` packets and proves
//! the engine reproduces vanilla's float/double physics bit-for-bit across
//! hundreds of ticks with zero corrections. That test owns the *arithmetic*
//! claim and must not move.
//!
//! This gate is one layer up and makes a *different* claim: that a bot can walk
//! **through the public client API** (`ClientHandle`), and that the server
//! accepts the reported positions. It asserts two things, each with a control
//! proving the detector would fire — because "the server never corrected us" is
//! worthless unless we prove (a) movement really left the client, and (b) the
//! server was actually validating:
//!
//!   1. **Displacement, not change.** A commanded walk arrives within tolerance
//!      of its target *and* traces a gradual, vanilla-walking-speed path
//!      (~0.215 blocks/tick). A bot that teleports there, or one that never
//!      moves, both fail. Control: a no-input phase must **not** displace.
//!   2. **Zero corrective teleports** while the server is validating us.
//!      Control: an impossible one-tick jump issued through the same public API
//!      **must** be corrected — proving the server is validating (see below) and
//!      that the "zero corrections" result above is not vacuous.
//!
//! ## The validation window (ground-truth, not a guess)
//!
//! The 26.2 server does **not** validate movement immediately after join. Its
//! own player-packet-listener gates its own movement handler on a
//! "has client loaded" check,
//! which is its own client-loaded timeout timer being `<= 0`; the constructor seeds that timer to
//! `CLIENT_LOADED_TIMEOUT_TIME = 60` ticks and decrements it once per tick, so
//! the server *silently ignores* our movement for the first ~60 ticks (~3 s)
//! unless the client sends `player_loaded` to zero it early. The driver now
//! sends `player_loaded` automatically on the first placement teleport (the
//! point the server has placed us), so the window is already zeroed by the time
//! `position()` is known and this bot moves immediately instead of waiting it
//! out. The impossible-move control below still *confirms* validation is live:
//! if the server were somehow still ignoring us, that control would fail loudly
//! rather than let the gate pass on an unvalidated walk.
//!
//! ## `position()` is optimistic local prediction
//!
//! The driver folds an outgoing `ClientAction::Move` straight into the
//! read-model (`set_local_movement`) so helpers make progress without waiting
//! for a server echo. That means `handle.position()` reflects what *we* claimed,
//! not server truth. The only server-truth signals the public API exposes are
//! corrective `ClientEvent::TeleportPlayer`s and `Disconnect`. So the
//! server-truth content of this gate lives entirely in its teleport assertions;
//! the position read-back proves the client's own movement primitive advances,
//! which is exactly the gap left open in `live_bot.rs` (walk observed, never
//! asserted).
#![cfg(feature = "live-v770")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestone_client::{
    BlockPos, ClientBuilder, ClientEvent, ClientHandle, LoginProfile, ServerAddress, Vec3,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

mod common;
use common::unique_username;

/// Version-free state id of `minecraft:air`. Confirmed empirically against the
/// live 26.2 superflat server: `block_at` returns `Some(0)` for the air above
/// the surface and `Some(9)` (a solid surface block) directly below the feet.
const AIR_STATE_ID: u32 = 0;
/// Bounded join retries. mc262 is a *shared* superflat server: sibling agents
/// dig and build near spawn (0,0), so a random spawn occasionally lands embedded
/// below the surface or over a dug-out lane, where a constant-Y walk is
/// correctly rejected by the server every tick. We reconnect (fresh spawn) until
/// the commanded lane is clean, and fail loudly if every attempt is obstructed —
/// a genuine physics regression would obstruct *every* spawn and surface here as
/// a hard failure, never a silent skip.
const MAX_JOIN_ATTEMPTS: usize = 8;

/// Vanilla ground walking speed: 4.317 blocks/s = 0.2158 blocks/tick. Stepping
/// at this rate is indistinguishable, to the server's movement checks, from a
/// player holding "forward" on flat ground.
const STEP_PER_TICK: f64 = 0.215;
/// One server tick. Movement is emitted at this cadence, as a real client does.
const TICK: Duration = Duration::from_millis(50);
/// Horizontal blocks to walk. Small enough to stay on the flat spawn platform
/// (the neighbouring 3x3 of chunks the server streams first), large enough that
/// the path is unambiguously many ticks of gradual movement, not a jump.
const WALK_BLOCKS: f64 = 4.0;
/// Arrival tolerance for the commanded target.
const ARRIVE_TOL: f64 = 0.35;

/// Horizontal (XZ) distance between two positions; vertical is ignored so the
/// assertions are about walking, not the tiny Y settle a server may apply.
fn horizontal_dist(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

/// Server-truth signals observed off the event stream by a background drain
/// task, so the bounded channel never back-pressures the driver.
#[derive(Default)]
struct Corrections {
    /// Count of `TeleportPlayer` events (join placement + any corrections).
    teleports: AtomicUsize,
    /// Set if the server disconnected us mid-test.
    disconnected: AtomicBool,
    /// The most recent teleport target the server sent.
    last: Mutex<Option<Vec3>>,
}

/// Is the commanded +X walk lane walkable? For every block column the walk
/// crosses, the block below the feet must be a loaded solid and the feet/head
/// blocks must be loaded air. `None` (chunk not yet loaded) counts as not clean.
/// This is a pure public-API check (`block_at`), so the gate needs no RCON.
fn lane_is_clean(handle: &ClientHandle, start: Vec3, blocks: f64) -> bool {
    let fx = start.x.floor() as i32;
    let fy = start.y.floor() as i32;
    let fz = start.z.floor() as i32;
    let last = blocks.ceil() as i32 + 1;
    for dx in 0..=last {
        let below = handle.block_at(BlockPos::new(fx + dx, fy - 1, fz));
        let feet = handle.block_at(BlockPos::new(fx + dx, fy, fz));
        let head = handle.block_at(BlockPos::new(fx + dx, fy + 1, fz));
        match (below, feet, head) {
            (Some(b), Some(AIR_STATE_ID), Some(AIR_STATE_ID)) if b != AIR_STATE_ID => {}
            _ => return false,
        }
    }
    true
}

/// A fully joined, settled bot whose commanded walk lane is verified clean.
struct Joined {
    handle: ClientHandle,
    drain: JoinHandle<()>,
    corrections: Arc<Corrections>,
    start: Vec3,
    health: Option<f32>,
}

/// Connect, reach Play, load terrain, wait out the client-load validation
/// window, and verify the +X walk lane is clean. Returns `Some(Joined)` on
/// success, or `None` if this spawn is unsuitable (obstructed lane, inherited
/// corpse, or a mid-settle disconnect) and the caller should retry with a fresh
/// join. Truly fatal conditions (cannot reach Play at all) panic.
async fn join_with_clean_lane(server: &ServerAddress) -> Option<Joined> {
    let profile = LoginProfile {
        // Per-run unique: a shared offline-mode name can inherit a persisted dead
        // player, which silently blacks out chunks. See `common::unique_username`.
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via the live-v770 feature");

    let (handle, mut events) = ClientBuilder::new(server.clone(), profile, adapter)
        .connect()
        .await
        .expect("connect to live server");

    // Drain events on a background task: count teleports (the only server-truth
    // correction signal the public API exposes) and note disconnects.
    let corrections = Arc::new(Corrections::default());
    let drain = {
        let corrections = Arc::clone(&corrections);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ClientEvent::TeleportPlayer { pos, .. } => {
                        *corrections.last.lock().unwrap() = Some(pos);
                        corrections.teleports.fetch_add(1, Ordering::SeqCst);
                    }
                    ClientEvent::Disconnect { reason } => {
                        eprintln!("server disconnected us: {}", reason.to_plain_string());
                        corrections.disconnected.store(true, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
        })
    };

    async fn abort(mut handle: ClientHandle, drain: JoinHandle<()>) {
        handle.shutdown();
        let _ = handle.join().await;
        drain.abort();
    }

    // Reaching Play at all is fatal, not a retry: if login fails the server or
    // build is broken, and silently retrying would mask it.
    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("should reach Play");

    // An inherited corpse (health 0, blacked-out chunks) is a per-spawn hazard —
    // retry rather than fail.
    if handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .is_err()
    {
        eprintln!("attempt: no chunks within 30s (likely inherited corpse); retrying");
        abort(handle, drain).await;
        return None;
    }
    let health = handle.health();
    if !health.is_some_and(|h| h > 0.0) {
        eprintln!("attempt: health {health:?} not positive (inherited corpse); retrying");
        abort(handle, drain).await;
        return None;
    }

    // Wait for the server to place us (the join teleport), so `position()` is a
    // real server-derived spawn rather than `None`.
    if handle
        .wait_for(Duration::from_secs(10), |h| h.position().is_some())
        .await
        .is_err()
    {
        eprintln!("attempt: server never placed the player within 10s; retrying");
        abort(handle, drain).await;
        return None;
    }

    // The driver auto-sends `player_loaded` on the first placement teleport,
    // zeroing the server's own client-loaded timeout timer before `position()` (waited
    // on above) is known — so movement is validated from here on and there is no
    // client-load window left to wait out. The impossible-move control at the end
    // of the test *verifies* validation is live.
    if corrections.disconnected.load(Ordering::SeqCst) {
        eprintln!("attempt: disconnected before walking; retrying");
        abort(handle, drain).await;
        return None;
    }

    let start = handle.position().expect("position known after settle");
    if !lane_is_clean(&handle, start, WALK_BLOCKS) {
        eprintln!(
            "attempt: spawn ({:.1},{:.1},{:.1}) has an obstructed +X walk lane \
             (shared server terrain); retrying",
            start.x, start.y, start.z
        );
        abort(handle, drain).await;
        return None;
    }

    Some(Joined {
        handle,
        drain,
        corrections,
        start,
        health,
    })
}

#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn bot_walks_through_public_api_and_server_validates() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };

    // Join, settle past the validation window, and secure a clean walk lane —
    // retrying fresh spawns because mc262 is a shared server whose terrain near
    // spawn siblings modify. Exhausting every attempt fails loudly.
    let mut joined = None;
    for attempt in 1..=MAX_JOIN_ATTEMPTS {
        eprintln!("join attempt {attempt}/{MAX_JOIN_ATTEMPTS}");
        if let Some(j) = join_with_clean_lane(&server).await {
            joined = Some(j);
            break;
        }
    }
    let Joined {
        mut handle,
        drain,
        corrections,
        start,
        health,
    } = joined.unwrap_or_else(|| {
        panic!(
            "no clean spawn in {MAX_JOIN_ATTEMPTS} attempts — every +X lane near spawn was \
             obstructed. Either the shared server's spawn area is heavily modified, or the \
             engine is diverging from vanilla on flat ground (which would obstruct every \
             spawn). Re-run; if it persists, inspect the physics, not the terrain."
        )
    });

    // Walk along +X on the verified-clean flat lane, holding Y and Z constant so
    // this is a pure horizontal walk.
    let target = Vec3::new(start.x + WALK_BLOCKS, start.y, start.z);

    // --- Phase A: commanded walk through the public API -------------------
    //
    // `step_toward` is the public movement primitive `walk_to` loops over; we
    // drive the loop ourselves only so we can sample and assert the path inline
    // (something `walk_to`'s timeout-swallowing return cannot express). Each
    // iteration must advance by at most ~one walk step — a larger jump would mean
    // the API teleported, which is as wrong as not moving.
    let teleports_before_walk = corrections.teleports.load(Ordering::SeqCst);
    let walk_start = Instant::now();
    let mut prev = start;
    let mut ticks = 0u32;
    let max_ticks = 200u32; // 10 s ceiling; the walk should need ~19.
    loop {
        let pos = handle
            .position()
            .expect("position stays known while walking");
        if horizontal_dist(pos, target) <= ARRIVE_TOL {
            break;
        }
        assert!(
            ticks < max_ticks,
            "walk did not arrive within {max_ticks} ticks"
        );
        handle
            .step_toward(target, STEP_PER_TICK)
            .expect("step_toward through the public API");
        tokio::time::sleep(TICK).await;
        ticks += 1;

        let now = handle
            .position()
            .expect("position stays known while walking");
        // Per-tick anti-teleport: a single step advances by at most one walk
        // stride. A larger jump means the movement primitive teleported.
        let step = horizontal_dist(now, prev);
        assert!(
            step <= STEP_PER_TICK + 0.05,
            "single tick advanced {step:.4} blocks (> one {STEP_PER_TICK}-block stride) — \
             the public movement primitive teleported instead of walking"
        );
        prev = now;
    }
    let walk_elapsed = walk_start.elapsed();
    let end = handle.position().expect("position known after walk");

    // Arrival: reached the commanded target (not merely "changed").
    let miss = horizontal_dist(end, target);
    assert!(
        miss <= ARRIVE_TOL,
        "walk ended {miss:.4} blocks from target (tol {ARRIVE_TOL}) — bot did not arrive"
    );
    // Displacement: actually crossed the commanded distance from the start.
    let displaced = horizontal_dist(end, start);
    assert!(
        displaced >= WALK_BLOCKS - ARRIVE_TOL,
        "bot displaced only {displaced:.4} of {WALK_BLOCKS} commanded blocks"
    );
    // Speed / anti-teleport: took roughly the expected number of ticks. A single
    // teleport to the target would finish in one tick; require at least 70% of
    // the ideal walk duration so a regression that stops integrating is caught.
    let ideal_ticks = (WALK_BLOCKS / STEP_PER_TICK).floor() as u32;
    let min_ticks = (ideal_ticks * 7) / 10;
    assert!(
        ticks >= min_ticks,
        "walk finished in {ticks} ticks; a vanilla-speed {WALK_BLOCKS}-block walk needs \
         ~{ideal_ticks} (>= {min_ticks}) — too fast to be walking"
    );
    // Server truth: it validated us (window elapsed, proven by the control
    // below) and issued no corrective teleport during the walk.
    let teleports_after_walk = corrections.teleports.load(Ordering::SeqCst);
    assert!(
        !corrections.disconnected.load(Ordering::SeqCst),
        "server disconnected us during the walk"
    );
    assert_eq!(
        teleports_after_walk,
        teleports_before_walk,
        "server issued {} corrective teleport(s) during a vanilla-speed walk — physics \
         diverged from vanilla",
        teleports_after_walk - teleports_before_walk
    );

    // --- Phase B: negative control — no input must not displace -----------
    //
    // Proves the displacement measurement is not reporting phantom motion: with
    // no movement sent, the position must hold and the server must not move us.
    let idle_start = handle.position().expect("position known before idle");
    let teleports_before_idle = corrections.teleports.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1500)).await; // ~30 ticks, no input
    let idle_end = handle.position().expect("position known after idle");
    let idle_drift = horizontal_dist(idle_end, idle_start);
    assert!(
        idle_drift <= 0.02,
        "position drifted {idle_drift:.4} blocks with no movement input — displacement \
         detector is not measuring real motion"
    );
    assert_eq!(
        corrections.teleports.load(Ordering::SeqCst),
        teleports_before_idle,
        "server teleported a stationary player during the no-input control"
    );

    // --- Phase C: negative control — the correction detector must fire ----
    //
    // This is the linchpin: it proves the server is *validating* our movement,
    // so Phase A's "zero corrections" is a real result and not the server
    // silently ignoring an unloaded client. Command a physically impossible
    // one-tick jump through the same public API; the server must snap us back
    // with a corrective teleport.
    let teleports_before_jump = corrections.teleports.load(Ordering::SeqCst);
    let here = handle
        .position()
        .expect("position known before impossible move");
    handle
        .set_position(Vec3::new(here.x + 500.0, here.y, here.z))
        .expect("set_position through the public API");

    let mut corrected = false;
    let jump_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < jump_deadline {
        if corrections.teleports.load(Ordering::SeqCst) > teleports_before_jump {
            corrected = true;
            break;
        }
        tokio::time::sleep(TICK).await;
    }
    assert!(
        corrected,
        "server did NOT correct a 500-block one-tick jump within 5s. Either it is not \
         validating our movement (client-load window not elapsed, or `player_loaded` \
         required and unsendable via the public API), or the correction detector is \
         broken. Phase A's zero-corrections result cannot be trusted without this control."
    );

    eprintln!(
        "=== LIVE PUBLIC-API PHYSICS GATE REPORT ===\n\
         health at spawn        : {health:?}\n\
         walk ticks             : {ticks:<6} walk distance      : {displaced:.4} blocks\n\
         walk wall-clock        : {walk_elapsed:.2?}\n\
         corrections during walk: {}\n\
         idle drift (no input)  : {idle_drift:.4} blocks\n\
         impossible-move control: server corrected = {corrected} (snapped to {:?})",
        teleports_after_walk - teleports_before_walk,
        *corrections.last.lock().unwrap(),
    );

    handle.shutdown();
    let _ = handle.join().await;
    drain.abort();
}
