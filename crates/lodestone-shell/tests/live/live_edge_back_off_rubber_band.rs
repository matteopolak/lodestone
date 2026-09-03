//! Live gate for issue's own two named checks: the server's per-packet
//! rubber-band correction, and the vertical-disagreement clamp that exempts
//! ordinary falling from it.
//!
//! ## Background
//!
//! The server replays every claimed movement itself and teleports the client
//! back onto the replayed result once the disagreement exceeds a
//! single-packet threshold (roughly 0.25 blocks, no accumulator across
//! packets) — a real, adopted `TeleportPlayer`, counted by
//! [`lodestone::sim::Sim::teleport_count`]
//! (`crates/lodestone-shell/src/sim/net_apply.rs`, incremented on every
//! adopted teleport inside `Sim::poll_net`). Two things about that check are
//! easy to get backwards without a live server to watch:
//!
//! 1. **The disagreement comparison is purely horizontal.** The vertical
//!    component is always zeroed before the threshold is applied (an "always
//!    true" disjunction in the real rule), so a claim can disagree on `y` by
//!    any amount and never trigger a correction on that account alone. An
//!    ordinary fall reaches well over 0.25 blocks of vertical travel in a
//!    single tick within a few ticks of leaving the ground — if the vertical
//!    component were checked the same way as horizontal, *every* fall of any
//!    real height would rubber-band constantly, which is not what playing
//!    the game looks like.
//!
//!    **But that is not the only rule that teleports a player back**, and
//!    reading it as though it were is how this file was first written. A
//!    second, *speed* check runs before it and is genuinely
//!    three-dimensional — the vertical term is not zeroed for it. Its budget
//!    is a squared distance of `100` per packet per tick, so ten blocks of
//!    claimed travel in one packet. A fall cannot reach that at any height
//!    (terminal speed is `3.92` blocks/tick), which is what makes "a fall is
//!    never corrected" a real guarantee — but a *stale* claim can reach it
//!    trivially, and the oracle's own log names which of the two fired. When
//!    this gate goes red, read that log before reading anything here: the two
//!    rules have different causes and only one of them is about falling.
//! 2. **The sneak-at-a-ledge back-off exists precisely so ordinary walking
//!    never disagrees with the server enough to trip the horizontal check.**
//!    `crates/lodestone-physics/tests/edge_back_off.rs` and `golden.rs`
//!    already prove the port matches an independent oracle bit for bit in
//!    isolation; what only a real server can show is that the *live* session
//!    — sneaking at a real ledge on real terrain — never reaches the
//!    correction threshold at all.
//!
//! Both tests below share the same discipline: an RCON `tp` used as this
//! gate's own control that [`Sim::teleport_count`] really can move within
//! the test's lifetime (an absence assertion with no such control is
//! satisfied by a counter that was never going to move), and a predicted
//! numeric claim — not merely "some correction happened or didn't" — checked
//! before the main assertion runs.
//!
//! Must run against the **survival** oracle specifically: the real
//! correction check is skipped outright for a creative-mode player (also
//! inside the same handler that applies it), so running this against
//! `creative.sh` — the default reflex — would give a guaranteed vacuous pass
//! regardless of whether either rule is modelled correctly.
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live -- \
//!   --ignored --nocapture edge_back_off
//! ```
//! (`live` is the consolidated binary every `tests/live/*.rs` module compiles
//! into — see `tests/live.rs`'s own doc.)
#![cfg(feature = "live")]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_controller::Action;
use lodestone_testsupport::{RconClient, unique_username};

/// Neither test here touches the world spawn or shares a location with the
/// other live gates, but both still join the same one oracle process — serialised
/// the same way every other live-gate file is, so a slow CI box running the
/// whole `live` binary at `--test-threads` > 1 cannot interleave two RCON
/// sessions against it.
static SERVER_LOCK: Mutex<()> = Mutex::new(());

const HOST: &str = "127.0.0.1";
/// The survival 26.2 oracle: game on `:25565`, RCON on `:25566`. Named only as a
/// protocol *number* — the shell never names a version.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// The single-packet correction threshold the real server applies —
/// documented as roughly `0.25` blocks of horizontal disagreement in a
/// single movement packet, no accumulator across packets. Used here only to
/// size how far the horizontal control-teleport must move the player: it
/// must clear this by a wide margin so the assertion is not sensitive to the
/// exact figure.
const CORRECTION_THRESHOLD_BLOCKS: f64 = 0.25;

/// The **other** rule that can teleport a player back, and the one this file
/// used to overlook: a speed check that is genuinely three-dimensional — the
/// vertical term is *not* zeroed for it. It compares the squared distance a
/// packet claims against a budget of `100` per packet per tick, so the limit is
/// ten blocks of travel in one packet. Free fall cannot reach it at any height
/// (see [`FALL_TERMINAL_BLOCKS_PER_TICK`]), which is what makes "a fall never
/// gets corrected" a real guarantee rather than a coincidence of this drop.
const SPEED_BUDGET_BLOCKS_PER_TICK: f64 = 10.0;

/// Free fall's per-tick drag factor and the terminal speed it converges to:
/// `v <- (v - 0.08) * 0.98` has fixed point `-0.08 * 0.98 / 0.02 = -3.92`
/// blocks per tick. Both are stated here as arithmetic, deliberately not read
/// from `lodestone-physics` — an expected value taken from the code under test
/// is satisfied by any pair of matching mistakes.
const FALL_DRAG: f64 = 0.98;
/// See [`FALL_DRAG`].
const FALL_TERMINAL_BLOCKS_PER_TICK: f64 = 3.92;
/// The index of the last *complete* tick of this file's own drop. Summing
/// `|v_n| = 3.92 * (1 - 0.98^n)` first passes `FALL_FROM_Y - GROUND_Y` = 151
/// blocks during tick 78, so tick 77 is the fastest one that runs to its full
/// length and tick 78 is cut short by the collision that lands the player —
/// making `3.92 * (1 - 0.98^77)` the largest per-tick delta the fall can show.
/// Fixed by this file's own drop height, and by nothing else.
const FULL_FALL_TICKS: i32 = 77;

/// Far from the origin and from every other live gate's own coordinates
/// (`live_stands_on_server_ground.rs`/`live_respawn_ground_trace.rs` both use
/// `(-45, ~72, -377)`), well above real terrain height in this seed so a
/// hand-built platform floats in open air with nothing else nearby to
/// collide with.
const PLATFORM_X: i32 = 5000;
const PLATFORM_Y: i32 = 250;
const PLATFORM_Z: i32 = -5000;
/// Half-width of the built platform, in blocks either side of
/// `PLATFORM_X`/`PLATFORM_Z` — a 7x7 stone floor.
const PLATFORM_HALF: i32 = 3;

/// Yaw that makes `Action::Forward` walk in `+x` — see
/// `lodestone_physics::player::input_vector`'s own rotation: at yaw `270`,
/// `sin(yaw)=-1, cos(yaw)=0`, giving `x = -speed*sin(yaw) = +speed`.
const YAW_FACING_PLUS_X: f32 = 270.0;

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

fn connect_rcon() -> RconClient {
    RconClient::connect(RCON_ADDR, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!(
            "cannot reach RCON at {RCON_ADDR}: {e}. Fix: start the survival 26.2 oracle \
             (game :25565, RCON :25566) with `./scripts/live-oracles/survival.sh` and run \
             with `--features live`."
        )
    })
}

/// Installs the `tracing` subscriber the failure messages in this file tell an
/// operator to reach for, so that instruction is real rather than decorative:
/// without one, `RUST_LOG=warn,transfer=debug` sets an environment variable that
/// nothing reads.
///
/// The default filter is `warn`, so an ordinary green run prints nothing new —
/// but the version adapter escalates to `warn` on exactly the event this gate
/// exists to catch (the first movement packet after a teleport claiming a
/// position far from that teleport's target), so a red run explains itself
/// without needing to be re-run with the variable set. `try_init` because the
/// three tests here share one process with every other live gate.
fn install_trace_subscriber() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

/// Joins under a fresh identity and drives ticks until the server has placed
/// the player and chunks are streaming — the same "phase 1" every other live
/// gate in this tree uses, extracted since both tests below need it and
/// neither needs anything past it (unlike `live_stands_on_server_ground.rs`,
/// neither test cares about the *demo* spawn height). Returns the username
/// this session joined under, for the RCON `tp` calls that follow — nothing
/// on `Sim`/`NetClient` exposes the connected username back, so the caller
/// would otherwise have no way to name this session's own player over RCON.
fn join_and_wait_for_placement(sim: &mut Sim) -> String {
    let demo_spawn = sim.player().position;
    let username = unique_username();
    sim.connect_as(HOST.into(), PORT, PROTOCOL, username.clone());
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
        "server never placed the player within 60s. Fix: start the survival 26.2 oracle on \
         :25565 and run with `--features live`."
    );
    username
}

/// RCON-teleports the player and drives ticks until the client has adopted a
/// `TeleportPlayer` landing within `0.5` blocks **horizontally** of the
/// requested position. Returns the [`Sim::teleport_count`] observed once it
/// has landed, so a caller has an exact baseline to diff later corrections
/// against.
///
/// **Horizontal only, and that is the whole point.** Every caller here aims
/// at a target above the surface — a platform plus two or five blocks, or
/// the control's y=250 — so the player begins falling the instant the
/// teleport is adopted and leaves any vertical window almost immediately.
/// Requiring `|dy| < 0.5` made this helper unsatisfiable: on its first real
/// run against the oracle the control asked for `(5050, 250, -5000)`, the
/// client adopted it, x and z landed exactly, and the assertion still failed
/// thirty seconds later reporting `y = 56` — the ground. The count had
/// incremented twice, so the teleport was never in doubt; only the criterion
/// was wrong.
///
/// x and z are stable because nothing moves the player horizontally here, so
/// they identify the teleport unambiguously. Anything vertical belongs to the
/// physics under test, not to the confirmation that a teleport arrived.
fn rcon_teleport_and_confirm(sim: &mut Sim, rcon: &mut RconClient, username: &str, x: f64, y: f64, z: f64, yaw: f32) -> u64 {
    let before = sim.teleport_count;
    let reply = rcon.cmd(&format!("tp {username} {x} {y} {z} {yaw} 0"));
    assert!(
        reply.to_lowercase().contains("teleported") || reply.to_lowercase().contains("moved"),
        "RCON tp did not report success: {reply:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        let p = sim.player().position;
        let dx = p.x - x;
        let dz = p.z - z;
        // 1.0, not 0.5, and the extra is not slack. A `tp` given integer
        // coordinates places the player at the BLOCK CENTRE, so each
        // horizontal axis lands exactly 0.5 off the number asked for and the
        // diagonal is hypot(0.5, 0.5) = 0.707. Measured, not assumed: with a
        // 0.5 threshold the control asked for x=5050, z=-5000 and sat at
        // x=5050.5, z=-4999.5 for the full thirty seconds. 1.0 admits the
        // centre offset and nothing else — the next block over is 1.414 away
        // diagonally and 1.0 away on an axis, so a teleport to the wrong
        // block still cannot satisfy this.
        if sim.teleport_count > before && dx.hypot(dz) < 1.0 {
            return sim.teleport_count;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "RCON tp to ({x}, {y}, {z}) never landed horizontally within 30s (final pos {:?}, \
         teleport_count {} -> {}). A moved count with the wrong x/z means the client adopted \
         some other teleport; an unchanged count means it adopted none.",
        sim.player().position,
        before,
        sim.teleport_count
    );
}

/// **The control this whole gate depends on**: an RCON `tp` genuinely moves
/// the player and increments [`Sim::teleport_count`] — proving the counter
/// this gate reads is not a value that was never going to change inside the
/// test's own lifetime, independent of anything about the edge-back-off or
/// clamp rules under test.
#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn the_teleport_counter_control_an_rcon_teleport_is_observed_and_counted() {
    let _serialized = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    install_trace_subscriber();
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the live \
         server world. Banner: {:?}.",
        probe.asset_banner()
    );
    drop(probe);

    let mut sim = Sim::new(live_config());
    let username = join_and_wait_for_placement(&mut sim);
    let mut rcon = connect_rcon();

    let before = sim.teleport_count;
    // Comfortably over the correction threshold and over the "confirmed
    // landing" tolerance both — an unmissable, unconditional teleport.
    let count_after = rcon_teleport_and_confirm(
        &mut sim,
        &mut rcon,
        &username,
        PLATFORM_X as f64 + 50.0,
        PLATFORM_Y as f64,
        PLATFORM_Z as f64,
        0.0,
    );
    assert!(
        count_after > before,
        "control failed: an RCON teleport must increment Sim::teleport_count \
         (before={before}, after={count_after}). Without this control, an unmoving counter \
         elsewhere in this file proves nothing."
    );
}

/// Builds a 7x7 stone platform centred on `(PLATFORM_X, PLATFORM_Y - 1,
/// PLATFORM_Z)` (top surface at `PLATFORM_Y`), placing the player above it
/// first so the block-change updates the `/fill` provokes are actually sent
/// to this session (the server only broadcasts an edit to players who
/// already have that chunk loaded) — then confirms client-side that the
/// platform is real by driving ticks until the player settles `on_ground`
/// there, an independent confirmation through collision rather than trusting
/// the RCON reply text alone.
fn build_platform_and_land_on_it(sim: &mut Sim, rcon: &mut RconClient, username: &str, land_x: f64) {
    // Place the player just above the platform's future surface first, so
    // the chunk is subscribed before the `fill` broadcasts its block changes.
    rcon_teleport_and_confirm(sim, rcon, username, land_x, f64::from(PLATFORM_Y) + 5.0, f64::from(PLATFORM_Z), YAW_FACING_PLUS_X);

    let reply = rcon.cmd(&format!(
        "fill {} {} {} {} {} {} minecraft:stone",
        PLATFORM_X - PLATFORM_HALF,
        PLATFORM_Y - 1,
        PLATFORM_Z - PLATFORM_HALF,
        PLATFORM_X + PLATFORM_HALF,
        PLATFORM_Y - 1,
        PLATFORM_Z + PLATFORM_HALF,
    ));
    assert!(
        reply.to_lowercase().contains("chang") || reply.to_lowercase().contains("fill"),
        "RCON fill did not report success: {reply:?}"
    );

    // Drop the player onto it and confirm collision client-side — not the
    // RCON reply text, a real `on_ground` reached through this session's own
    // collision code.
    rcon_teleport_and_confirm(sim, rcon, username, land_x, f64::from(PLATFORM_Y) + 2.0, f64::from(PLATFORM_Z), YAW_FACING_PLUS_X);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut landed = false;
    while Instant::now() < deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if sim.player().on_ground && (sim.player().position.y - f64::from(PLATFORM_Y)).abs() < 0.6 {
            landed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        landed,
        "the built platform was never confirmed under the player (final pos {:?}). Either the \
         fill never reached this session, or collision against it failed.",
        sim.player().position
    );
}

/// **The main gate**: sneaking at a real ledge on the survival oracle
/// produces zero server corrections, and a control proves the same ledge
/// really would drop the player without sneaking — so "no correction" is not
/// merely "there was nothing to correct".
#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn sneaking_at_a_real_ledge_on_the_oracle_produces_no_server_correction() {
    let _serialized = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    install_trace_subscriber();
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the live \
         server world. Banner: {:?}.",
        probe.asset_banner()
    );
    drop(probe);

    let mut sim = Sim::new(live_config());
    let username = join_and_wait_for_placement(&mut sim);
    let mut rcon = connect_rcon();

    // The ledge: platform is solid for block x-index in
    // [PLATFORM_X-3, PLATFORM_X+3], i.e. solid world x < PLATFORM_X+4. The
    // start position is 1.7 blocks of clearance back from that boundary
    // (box half-width 0.3, so the box's own leading edge starts 2.0 blocks
    // back) — enough runway to see the approach, not so much that a 20s
    // walk timeout is close.
    let start_x = f64::from(PLATFORM_X) + 2.0;
    build_platform_and_land_on_it(&mut sim, &mut rcon, &username, start_x);

    // --- Sanity control: walking off *without* sneaking must fall. ---------
    // Proves the ledge is real and reachable by ordinary forward input,
    // exactly the setup the sneak run below reuses. A "no correction" result
    // from a setup that can never produce a fall in the first place would
    // prove nothing about the back-off rule.
    sim.input_mut(|i| i.set(Action::Forward, true));
    let walk_deadline = Instant::now() + Duration::from_secs(15);
    let mut fell = false;
    while Instant::now() < walk_deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if sim.player().position.y < f64::from(PLATFORM_Y) - 2.0 {
            fell = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    sim.input_mut(|i| i.set(Action::Forward, false));
    assert!(
        fell,
        "control failed: walking forward with no sneak input must fall off this ledge (final \
         y={:.2}, platform y={PLATFORM_Y}). Without a reproduced fall this gate's ledge is not \
         real and the sneak run below proves nothing.",
        sim.player().position.y
    );

    // --- Reset onto the same platform and take the teleport-count baseline
    // right where the main observation begins. ---------------------------
    let baseline = rcon_teleport_and_confirm(&mut sim, &mut rcon, &username, start_x, f64::from(PLATFORM_Y), f64::from(PLATFORM_Z), YAW_FACING_PLUS_X);

    // --- The main observation: sneak forward at the ledge. ------------------
    sim.input_mut(|i| {
        i.set(Action::Sneak, true);
        i.set(Action::Forward, true);
    });
    let mut max_x = start_x;
    let mut min_y = sim.player().position.y;
    let mut on_ground_failures = 0usize;
    // Sneak speed is roughly 0.065 blocks/tick — 150 ticks (7.5s) covers the
    // 1.7-block approach and a long linger at the edge besides.
    for _ in 0..150 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        let p = sim.player().position;
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        if !sim.player().on_ground {
            on_ground_failures += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    sim.input_mut(|i| {
        i.set(Action::Sneak, false);
        i.set(Action::Forward, false);
    });

    eprintln!(
        "[sneak-at-ledge] start_x={start_x:.3} ledge_boundary_x={:.1} max_x={max_x:.4} \
         min_y={min_y:.3} platform_y={PLATFORM_Y} on_ground_failures={on_ground_failures}/150 \
         teleport_count baseline={baseline} final={}",
        f64::from(PLATFORM_X + PLATFORM_HALF + 1),
        sim.teleport_count,
    );

    assert!(
        max_x < f64::from(PLATFORM_X + PLATFORM_HALF + 1),
        "the player crossed the ledge boundary while sneaking (max_x={max_x:.4}, boundary={}) \
         — the back-off rule failed to hold the walk at the edge.",
        f64::from(PLATFORM_X + PLATFORM_HALF + 1)
    );
    assert!(
        min_y > f64::from(PLATFORM_Y) - 0.6,
        "the player dropped below the platform while sneaking at its edge (min_y={min_y:.3}, \
         platform_y={PLATFORM_Y}) — sneaking should have stopped the walk before the fall."
    );
    assert_eq!(
        on_ground_failures, 0,
        "the player left the ground at some point while sneaking at the ledge \
         ({on_ground_failures}/150 ticks airborne) — the back-off should keep them planted."
    );
    assert_eq!(
        sim.teleport_count, baseline,
        "the server issued {} correction(s) while sneaking at a real ledge (baseline \
         {baseline}, final {}). The control above proved the counter can move; this means the \
         client's claimed position disagreed with the server's own replay by more than the \
         single-packet threshold (~{CORRECTION_THRESHOLD_BLOCKS} blocks) at some point during \
         the approach.",
        sim.teleport_count - baseline,
        sim.teleport_count,
    );
}

/// **The vertical-disagreement clamp**: a real fall reaches a per-tick
/// vertical delta well past the single-packet correction threshold — the
/// gate's own predicted-value premise, checked before the main claim — and
/// still produces zero corrections, because the real rule always zeroes the
/// vertical component before comparing. Falls onto the same real ground
/// `live_stands_on_server_ground.rs` already establishes as walkable plains.
#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn a_real_fall_never_triggers_a_vertical_correction_despite_exceeding_the_threshold() {
    let _serialized = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    install_trace_subscriber();
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the live \
         server world. Banner: {:?}.",
        probe.asset_banner()
    );
    drop(probe);

    // The same land spawn `live_stands_on_server_ground.rs` uses — real,
    // walkable plains ground at y≈69-70, established there independently of
    // this file.
    const SPAWN_X: i32 = -45;
    /// The feet-`y` this column actually settles at, measured on the oracle
    /// (`final_y=69.000`, repeatably) rather than guessed from the launch
    /// column. `FALL_FROM_Y - GROUND_Y` is the drop height every predicted
    /// number below is derived from, so it has to be the settled figure and not
    /// a nearby round one.
    const GROUND_Y: i32 = 69;
    const SPAWN_Z: i32 = -377;
    const FALL_FROM_Y: f64 = 220.0;

    let mut sim = Sim::new(live_config());
    let username = join_and_wait_for_placement(&mut sim);
    let mut rcon = connect_rcon();

    let baseline = rcon_teleport_and_confirm(
        &mut sim,
        &mut rcon,
        &username,
        f64::from(SPAWN_X),
        FALL_FROM_Y,
        f64::from(SPAWN_Z),
        0.0,
    );

    let mut previous_y = sim.player().position.y;
    let mut max_abs_delta_y = 0.0_f64;
    let mut landed = false;
    let mut ticks = 0usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && ticks < 400 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        let y = sim.player().position.y;
        max_abs_delta_y = max_abs_delta_y.max((y - previous_y).abs());
        previous_y = y;
        ticks += 1;
        if sim.player().on_ground && y < FALL_FROM_Y - 5.0 {
            landed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    eprintln!(
        "[real-fall] ticks={ticks} max_abs_delta_y={max_abs_delta_y:.4} final_y={:.3} \
         landed={landed} teleport_count baseline={baseline} final={}",
        sim.player().position.y,
        sim.teleport_count,
    );

    assert!(
        landed,
        "the player never settled back on the ground within {ticks} ticks (final y={:.3}). \
         Fix: confirm the survival oracle's real terrain at ({SPAWN_X}, ., {SPAWN_Z}) is still \
         walkable plains.",
        sim.player().position.y
    );
    // The fall must land back on the *known* plains ground
    // `live_stands_on_server_ground.rs` established at this exact column, not
    // merely "somewhere below the launch height". The tolerance is tight
    // because `GROUND_Y` is the settled figure this drop actually reaches, and
    // because the drop height it defines is what every predicted number below
    // is derived from: a landing a block out is a different fall, and the
    // prediction would then be checking the wrong one.
    assert!(
        (sim.player().position.y - f64::from(GROUND_Y)).abs() < 1.5,
        "the fall landed at y={:.3}, far from the known ground height {GROUND_Y} at this \
         column — the fall may have caught on something other than the real ground this test \
         intends to measure against.",
        sim.player().position.y
    );
    // The premise, as a *predicted number* rather than a direction. Free fall
    // integrates `v <- (v - 0.08) * 0.98` from rest, whose fixed point is
    // `-0.08 * 0.98 / 0.02 = -3.92` blocks/tick and whose closed form is
    // `v_n = -3.92 * (1 - 0.98^n)`. Summing `|v_n|` reaches this drop's
    // `FALL_FROM_Y - GROUND_Y` = 151 blocks during tick 78, so the largest
    // *complete* tick of the fall is the 77th and the 78th is clipped short by
    // the collision that lands the player. `FALL_TERMINAL_BLOCKS_PER_TICK` and
    // the exponent are therefore both fixed by this test's own constants and by
    // arithmetic that never runs the code under test — the value below is
    // predicted, not read back.
    let predicted_max_abs_delta_y =
        FALL_TERMINAL_BLOCKS_PER_TICK * (1.0 - FALL_DRAG.powi(FULL_FALL_TICKS));
    assert!(
        (max_abs_delta_y - predicted_max_abs_delta_y).abs() < 0.05,
        "the fall's largest per-tick |dy| was {max_abs_delta_y:.4}, not the predicted \
         {predicted_max_abs_delta_y:.4} that a {}-block drop under `v <- (v - 0.08) * 0.98` \
         must produce. Either the drop height changed or the fall is no longer this engine's \
         own free fall — until that agrees, nothing below is a statement about the server.",
        FALL_FROM_Y - f64::from(GROUND_Y)
    );
    // What that number buys, and it is the whole point of predicting it: the
    // fall is fast enough to be well past the 0.25-block single-packet
    // disagreement threshold in its vertical component (so the run is not
    // vacuous), and still far short of the *other* rule that can teleport a
    // player back — the speed check, which unlike the disagreement check is
    // genuinely three-dimensional and includes the vertical term. Its budget is
    // a squared distance of 100 per packet per tick, i.e. ten blocks in one
    // packet; free fall cannot reach it at any height, because 3.92 blocks/tick
    // is a hard ceiling. Both arms of the sandwich are checked, so a future
    // edit that moves either constant fails here rather than silently.
    assert!(
        max_abs_delta_y > CORRECTION_THRESHOLD_BLOCKS,
        "the fall never reached a per-tick |dy| past the {CORRECTION_THRESHOLD_BLOCKS}-block \
         disagreement threshold (max observed {max_abs_delta_y:.4}) — this run cannot tell \
         apart 'the vertical component is exempt' from 'this fall was too gentle to test it'. \
         Increase FALL_FROM_Y."
    );
    assert!(
        max_abs_delta_y < SPEED_BUDGET_BLOCKS_PER_TICK,
        "the fall reached {max_abs_delta_y:.4} blocks/tick, at or past the server's own \
         {SPEED_BUDGET_BLOCKS_PER_TICK}-block per-packet speed budget — at that speed the \
         server is *entitled* to teleport the player back, and the assertion below would be \
         asking for a guarantee it never made."
    );
    // With both arms of that sandwich established, zero corrections is a claim
    // about the server's guarantee and not about the fall's luck: the
    // disagreement check cannot fire on a fall (it zeroes the vertical term
    // before comparing) and the speed check cannot fire either (the fall is
    // four times inside its budget). Anything the counter records here is
    // therefore movement *we* claimed and the server did not authorise — in
    // practice a claim built from a pose the server had already replaced, which
    // is what this window has produced before.
    assert_eq!(
        sim.teleport_count, baseline,
        "the server issued {} correction(s) across a fall that both rules exempt: its largest \
         per-tick |dy| was {max_abs_delta_y:.4} blocks, past the \
         {CORRECTION_THRESHOLD_BLOCKS}-block disagreement threshold (which zeroes the vertical \
         term before comparing, so it cannot fire on a fall) and far inside the \
         {SPEED_BUDGET_BLOCKS_PER_TICK}-block per-packet speed budget (which does include the \
         vertical term). Neither rule can account for this, so look for a movement packet \
         claiming a position the server had already overruled — run again with \
         `RUST_LOG=warn,transfer=debug` and read the `xfer:` lines either side of the \
         teleport, and check the oracle's own log for which rule it named.",
        sim.teleport_count - baseline,
    );
}
