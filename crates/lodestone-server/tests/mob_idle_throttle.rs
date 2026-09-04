//! Acceptance coverage for the `no_action_time` idle throttle and the movement
//! that remains possible while a player is near a mob.
//!
//! # Idle counter semantics
//!
//! [`MobSim::tick`] computes each mob's nearest-player distance from the values
//! supplied by [`MobSim::set_players`], resets `no_action_time` when that player
//! is within the category's immune radius, and then increments the counter for
//! the current tick. With no nearby player, no reset occurs: the counter is
//! monotonic for a world's whole life and crosses `RandomStrollGoal`'s idle
//! throttle of 100 after five seconds, after which no mob can stroll again.
//!
//! A nearby player clears the counter immediately before the mob's own tick.
//! Persistent status does not by itself clear the counter; proximity is the
//! relevant input for the simulation's throttle.
//!
//! # Why the throttle was fatal rather than merely slow
//!
//! `RandomStrollGoal::can_use` needs `next_i32(120) == 0`, so the tick a lone
//! stroll first fires is the first draw of that mob's stream where
//! `next_u64() % 120 == 0`. If that draw lands past the throttle at 100, a mob
//! with no player nearby can **never** stroll — total and deterministic rather
//! than a rare unlucky roll, which is what makes [`no_player_is_the_control`]
//! able to assert a hard `None`.
//!
//! # RNG stream selection
//!
//! Each mob's RNG stream is seeded from its id. A fresh simulation normally
//! starts at id `1`, whose first draw satisfying `next_u64() % 120 == 0` is
//! draw **9**, inside the throttle. A control using that subject is therefore
//! **structurally void**: the stroll fires before tick 100, so its outcome
//! cannot distinguish a working throttle from a missing one.
//!
//! [`STROLL_MOB_ID`] selects a stream whose first successful draw is still past
//! the throttle, and a compile-time assertion makes a void control a build
//! failure instead of a silent pass.
//!
//! Every draw index below is computed **outside the code under test**, by a
//! standalone program over the documented SplitMix64 recurrence
//! (`navigating_mob.rs:116-123`). The calculation is independent of the
//! producer and provides the expected first-hit positions below:
//!
//! | seed | first draw with `% 120 == 0` |
//! |---|---|
//! | `0x1234_5678_9ABC_DEF0` (reference stream) | 130 |
//! | `1` (the default first mob id) | **9** |
//! | `2` | 48 |
//! | `3` | **147** |
//!
//! Hermetic and deterministic, so these always run — no skip path.

use lodestone_entity::ai::goals::RandomStrollGoal;
use lodestone_entity::pathfinding::MobShape;
use lodestone_model::Vec3;
use lodestone_server::{ChunkWorld, MobSim, PlayerPerception, WorldgenChunkSource};
use lodestone_worldgen::density::Density;

/// The mob id [`observe`] spawns its subject with, and therefore
/// (`MobSim::spawn_with_type`'s `id as u64`) its RNG seed.
///
/// **Chosen, not observed.** Its first successful 1/120 stroll draw must land past
/// [`IDLE_THROTTLE_TICKS`], or [`no_player_is_the_control`] cannot separate a
/// working throttle from an absent one — see the module doc's table. `3` is the
/// lowest id that satisfies that; the default `1` does not, which is what broke
/// these gates.
const STROLL_MOB_ID: i32 = 3;

/// The tick a lone stroll goal first reaches `move_to`, once the throttle stops
/// blocking it: draw 147 of `SplitMix64(STROLL_MOB_ID)` (see the module doc's
/// table). `can_use` draws exactly once per tick for a mob whose only goal is the
/// stroll, so the draw index *is* the tick number.
const EXPECTED_FIRST_STROLL_TICK: usize = 147;

/// `RandomStrollGoal`'s idle throttle: `goals.rs` returns early when
/// `no_action_time() >= 100`. Restated here because it is the number the
/// control's whole premise rests on.
const IDLE_THROTTLE_TICKS: usize = 100;

/// **The control's premise, enforced at compile time.**
///
/// A subject whose stroll fires *before* the throttle closes makes
/// [`no_player_is_the_control`] vacuous while it still reads as rigorous. A
/// build failure is the only version of this check that cannot be skipped:
/// whoever changes the seed scheme must pick a new [`STROLL_MOB_ID`] rather than
/// transcribe whatever tick a run reports.
const _: () = assert!(EXPECTED_FIRST_STROLL_TICK > IDLE_THROTTLE_TICKS);

/// Long enough to pass `EXPECTED_FIRST_STROLL_TICK` with room to spare, and to
/// let the control arm climb far past the throttle.
const TICKS: usize = 600;

const _: () = assert!(TICKS > EXPECTED_FIRST_STROLL_TICK);

/// A flat solid floor with its surface at y=0, from the server's own
/// density-function terrain source — the same shape `tests/mob_sim.rs` uses.
fn floor_density() -> Density {
    Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    }
}

/// What one arm observed over `TICKS` ticks of a mob whose only goal is a
/// `RandomStrollGoal`.
struct Observed {
    /// The tick its first A\* search ran, i.e. the first tick the stroll goal
    /// actually reached `move_to`. `None` if it never strolled.
    first_stroll_tick: Option<usize>,
    /// The highest `no_action_time` the goals ever saw, read through the same
    /// `MobController` seam `RandomStrollGoal::can_use` reads.
    peak_no_action_time: i32,
    /// Its position at the end, to prove a stroll really moved it.
    end: Vec3,
}

/// Ticks one mob with exactly one goal — a `RandomStrollGoal` — so the mob RNG
/// is drawn once per tick and nothing else competes for the MOVE flag.
///
/// `players` is fed every tick, the way a connection feeds
/// [`MobSim::set_players`] from a real client's per-tick movement packet.
fn observe(persistent: bool, players: Vec<PlayerPerception>) -> Observed {
    let source = WorldgenChunkSource::new(floor_density(), -64, 128);
    let world = ChunkWorld::from_source(&source, -1..=1, -1..=1);
    // Ground truth: the mob has real floor to path over, so a failure to move
    // cannot be blamed on there being nowhere to walk.
    assert!(world.is_solid(8, -1, 8), "expected solid floor at y=-1");
    assert!(!world.is_solid(8, 0, 8), "expected air at the y=0 surface");

    let mut sim = MobSim::new(&world);
    // The mob's RNG seed is its id, so this call selects the stroll stream — see
    // `STROLL_MOB_ID`. The default id `1` strolls at tick 9, inside the
    // throttle, which would void the control.
    sim.set_next_id(STROLL_MOB_ID);
    let id = {
        let m = sim.spawn(Vec3::new(8.5, 0.0, 8.5), MobShape::land(0.6, 1.95), 0.23, 560);
        m.set_persistent(persistent);
        m.add_goal(7, Box::new(RandomStrollGoal::new(1.0)));
        m.id()
    };
    assert_eq!(
        id, STROLL_MOB_ID,
        "the subject's id is its RNG seed, so a different id means a different stroll \
         schedule and every tick number below is about another stream"
    );

    let mut first_stroll_tick = None;
    let mut peak_no_action_time = 0;
    for t in 1..=TICKS {
        sim.set_players(players.clone());
        sim.tick();
        let m = sim.get(id).expect("mob still present");
        peak_no_action_time = peak_no_action_time.max(m.mob_no_action_time());
        if first_stroll_tick.is_none() && m.path_searches() > 0 {
            first_stroll_tick = Some(t);
        }
    }
    Observed {
        first_stroll_tick,
        peak_no_action_time,
        end: sim.position(id).expect("mob still present"),
    }
}

fn player_at(pos: Vec3) -> Vec<PlayerPerception> {
    vec![PlayerPerception {
        position: pos,
        held_item: None,
        view_direction: Vec3::new(0.0, 0.0, 1.0),
    }]
}

/// The headline gate: a mob standing next to a player strolls, and its idle
/// counter never climbs past `1`.
///
/// `1`, not `0`, is the expected value for the ordering: the nearby-player reset
/// runs and *then* the mob tick increments the counter before goals run. A gate
/// asserting `0` would encode the opposite ordering, and one asserting merely
/// `< 100` would pass with the reset firing once every 99 ticks.
#[test]
fn a_player_nearby_clears_the_idle_throttle_every_tick_and_the_mob_strolls() {
    let near = observe(false, player_at(Vec3::new(10.5, 0.0, 8.5)));

    assert_eq!(
        near.peak_no_action_time, 1,
        "with a player 2 blocks away, vanilla clears noActionTime every tick and \
         serverAiStep re-increments it to exactly 1 before the goals run \
         (vanilla's own checkDespawn then serverAiStep); a higher peak means the reset is missing \
         or runs in the wrong order"
    );
    assert_eq!(
        near.first_stroll_tick,
        Some(EXPECTED_FIRST_STROLL_TICK),
        "the stroll must first fire on this mob's own RNG stream's first successful \
         1/120 draw (draw {EXPECTED_FIRST_STROLL_TICK} for seed {STROLL_MOB_ID}). \
         Landing on None is the pre-fix behaviour — the throttle closed at tick \
         {IDLE_THROTTLE_TICKS}, {} draws early",
        EXPECTED_FIRST_STROLL_TICK - IDLE_THROTTLE_TICKS
    );
    // The stroll reached the wire-visible state, not just the goal scheduler:
    // `move_to` ran A* and the follower really carried the mob off its spawn.
    assert_ne!(
        near.end,
        Vec3::new(8.5, 0.0, 8.5),
        "a mob that strolled must have left its spawn point: {:?}",
        near.end
    );
}

/// The control, and it must fail the arm above's assertions: with **no** player
/// anywhere, the nearby-player pass resets nothing, so the counter climbs, the
/// throttle closes at 100, and the mob never strolls.
///
/// This is the no-player control: the counter remains monotonic and movement is
/// suppressed after the threshold, while the nearby-player arm above exercises
/// the reset path.
#[test]
fn no_player_is_the_control() {
    let alone = observe(false, Vec::new());

    assert_eq!(
        alone.peak_no_action_time, TICKS as i32,
        "with no player, nothing may reset the counter — it must reach one per \
         tick ({TICKS})"
    );
    assert_eq!(
        alone.first_stroll_tick, None,
        "a mob with no player nearby is idle-throttled from tick {IDLE_THROTTLE_TICKS} \
         and cannot reach draw {EXPECTED_FIRST_STROLL_TICK}, so it must never stroll — \
         this is vanilla, not the bug. Note `can_use` returns before drawing once the \
         throttle closes, so the stream stops advancing at \
         {IDLE_THROTTLE_TICKS} draws too"
    );
    assert_eq!(
        alone.end,
        Vec3::new(8.5, 0.0, 8.5),
        "the control must not move at all"
    );
}

/// **The `persistent` flag must not open the throttle.** The nearby-player
/// reset is independent of persistence in this simulation.
///
/// `SimMob::persistent` is deliberately broader than the condition that clears
/// the throttle: `MobSim::spawn_species` sets it from `!hostile`, so passive
/// animals carry it even when no player is present. Treating the flag as a reset
/// request would give every such mob a permanently open idle throttle.
///
/// Same input as [`no_player_is_the_control`] except the flag, so a difference in
/// outcome could only come from the flag — and there must be none.
#[test]
fn the_persistent_flag_alone_does_not_clear_the_throttle() {
    let persistent = observe(true, Vec::new());
    let plain = observe(false, Vec::new());

    assert_eq!(
        persistent.peak_no_action_time, plain.peak_no_action_time,
        "`persistent` is not `isPersistenceRequired` in this crate and must not \
         change the idle counter — see this test's own doc comment"
    );
    assert_eq!(
        persistent.first_stroll_tick, None,
        "so a persistent mob with no player nearby is idle-throttled exactly like \
         any other, and never reaches draw {EXPECTED_FIRST_STROLL_TICK}"
    );
}
