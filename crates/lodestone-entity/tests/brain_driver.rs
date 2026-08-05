//! The behavioural gate for issue #209: a Brain-driven mob actually moves.
//!
//! # What this has to prove, and what would have passed without proving it
//!
//! `lodestone-entity::brain` shipped ~1900 lines with a green hermetic suite and a
//! live gate over `Brain::tick`, and reached **zero mobs** —
//! `grep -rn 'lodestone_entity::brain\|BrainMob'` outside the crate was empty. A
//! test that constructs a `Brain`, hands it a `TestMob` double, and asserts
//! `Sensor::tick` ran is exactly the closed loop that already existed. So is a
//! test that calls `BrainGoal::tick` directly.
//!
//! Two properties make this file a gate rather than another closed loop:
//!
//! * **The mob is the production body.** `NavigatingMob` is the only implementor
//!   of [`MobController`] outside test doubles, and the only one whose
//!   `brain_mob()` answers `Some`. Every double inherits the `None` default, so a
//!   brain installed on a fake does *nothing* — the shape that hid the islands in
//!   #441 and #455, where `ScriptMob` and `ai/roster/probe.rs` override all eight
//!   perception methods and a constant-`false` `can_use` stayed green.
//! * **The goals come from `goals_for`, not from this file.** That is the same
//!   function `MobSim::spawn_species` calls, so if the roster stopped installing a
//!   brain, these tests would stop moving. Constructing a `BrainGoal` here by hand
//!   would have measured the goal and not the wiring.
//!
//! What is asserted is **movement and head-turning**, i.e. bytes that would reach
//! a client through the entity encoders — never "a sensor was ticked". A sensor
//! that runs and whose output nothing reads is the same island one layer down.

use std::collections::HashSet;

// `MobController` is deliberately not imported: every method this file calls on
// the mob (`position`, `set_nearest_player`, `path_searches`, `facing`, `tick`) is
// an inherent `NavigatingMob` method. The brain reaches the trait from the inside,
// which is the point — the test never touches the seam by hand.
use lodestone_entity::ai::{GoalSelector, NavigatingMob, SpeciesContext, goals_for};
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
use lodestone_model::Vec3;

/// A brain-driven species. `frog` is a `Frog` in 26.2, whose AI is `FrogAi` —
/// there is no `registerGoals` to fall back on, which is the whole point.
const BRAIN_SPECIES: &str = "frog";

/// A goal-driven species, for the negative arm.
const GOAL_SPECIES: &str = "zombie";

/// Blocks per tick the follower steps. An arbitrary but *fixed* figure, because
/// every distance predicted below is derived from it rather than tolerated.
const STEP: f64 = 0.23;

/// The look distance the brain scaffold's `SetPlayerLookTarget` gates on, in
/// blocks — `lodestone_entity::brain::roster::SCAFFOLD_LOOK_DISTANCE`. Restated
/// here as an *independent* prediction: if the scaffold's figure changes, the
/// bracketing test below must fail rather than silently follow it.
const LOOK_DISTANCE: f64 = 8.0;

/// The melee reach `SpeciesContext::new` gives every species — the figure
/// `MobSim::spawn_species` has always used. A zombie that has acquired a player
/// paths until it is inside this.
const ATTACK_REACH: f64 = 2.0;

/// How far away the goal-driven arm puts its player. Chosen well inside a zombie's
/// `FOLLOW_RANGE` so acquisition is immediate, and well outside [`ATTACK_REACH`] so
/// closing the gap is a real, measurable journey rather than a rounding artefact.
const START_GAP: f64 = 5.0;

/// Flat solid ground at `y <= -1`, open above. A\* always has a route, and
/// nothing but a goal can move the mob — so a non-zero displacement has exactly
/// one possible cause.
struct Flat {
    walls: HashSet<(i32, i32, i32)>,
}

impl Flat {
    fn new() -> Self {
        Self {
            walls: HashSet::new(),
        }
    }

    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        y <= -1 || self.walls.contains(&(x, y, z))
    }
}

impl PathWorld for Flat {
    fn min_y(&self) -> i32 {
        -8
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if self.solid(x, y, z) {
            PathType::Blocked
        } else {
            PathType::Open
        }
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if self.solid(x, y, z) { 1.0 } else { 0.0 }
    }

    fn collides(&self, aabb: Aabb) -> bool {
        let (x0, x1) = (aabb.min_x.floor() as i32, (aabb.max_x - 1e-7).floor() as i32);
        let (y0, y1) = (aabb.min_y.floor() as i32, (aabb.max_y - 1e-7).floor() as i32);
        let (z0, z1) = (aabb.min_z.floor() as i32, (aabb.max_z - 1e-7).floor() as i32);
        (x0..=x1).any(|x| (y0..=y1).any(|y| (z0..=z1).any(|z| self.solid(x, y, z))))
    }

    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// The spawn point: a block centre, matching how `MobSim` places a mob.
///
/// A function rather than a `const` on purpose — `Vec3::new` is the constructor
/// every other test in this crate uses, and a struct literal would couple this
/// file to `Vec3`'s field list for no benefit.
fn start() -> Vec3 {
    Vec3::new(0.5, 0.0, 0.5)
}

/// Horizontal distance between two points.
///
/// **Horizontal, not 3-D, and that is the whole point of measuring it this way.**
/// `NavigatingMob::advance` applies `step_per_tick` to the *horizontal* component
/// only — `scale = step_per_tick / horizontal` — and then snaps `pos.y` to the
/// waypoint's floor outright. So the horizontal delta is a quantity the follower
/// controls exactly, whereas a 3-D delta folds in an uncontrolled vertical snap
/// and could only ever be asserted with a tolerance. Predicting the exact value
/// requires measuring the axis the code actually decides.
fn horizontal(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

/// What one run of a real mob over a real world did.
struct Run {
    /// Horizontal distance from the spawn point at the end.
    net_displacement: f64,
    /// Sum of every per-tick horizontal displacement — the actual path walked.
    distance_travelled: f64,
    /// How many ticks the mob's position changed at all.
    moving_ticks: usize,
    /// The largest single-tick horizontal displacement seen.
    max_tick_step: f64,
    /// How many real A\* searches ran. **A test double's `move_to` cannot produce
    /// a non-zero here**, which is what makes it evidence about the composition
    /// rather than about the seam.
    path_searches: u32,
    /// The last position the mob was told to look at.
    facing: Option<Vec3>,
    /// Where the mob finished. Needed to measure a *gap to a target*, which
    /// `net_displacement` cannot express — a mob that walks 4 blocks the wrong way
    /// scores the same displacement as one that closes on its target.
    end: Vec3,
    /// How many goals the roster installed. Recorded so a run that measures
    /// nothing is a *reported* precondition failure rather than a green zero.
    goals_installed: usize,
}

/// Spawns `species` through the production roster path and ticks it `ticks` times.
///
/// `install_goals` is the control knob: passing `false` builds the identical mob
/// and world with an **empty** `GoalSelector`, which is how the driver
/// registration is "removed" without editing a file. `player` is fed through
/// `set_nearest_player`, the same one-line feed `mobs.rs`'s `feed_perception`
/// uses, and nothing else about the player is communicated.
fn run(species: &str, ticks: usize, player: Option<Vec3>, install_goals: bool) -> Run {
    let world = Flat::new();
    let ctx = SpeciesContext::new(STEP);
    let spawn = start();
    let mut mob = NavigatingMob::new(&world, MobShape::land(0.6, 1.95), spawn, STEP, 560);

    let mut ai = GoalSelector::new();
    let mut goals_installed = 0;
    if install_goals {
        for (priority, goal) in goals_for(species, &ctx) {
            ai.add(priority, goal);
            goals_installed += 1;
        }
    }

    let mut distance_travelled = 0.0;
    let mut moving_ticks = 0;
    let mut max_tick_step: f64 = 0.0;
    let mut previous = spawn;
    for _ in 0..ticks {
        if let Some(p) = player {
            mob.set_nearest_player(Some(p));
        }
        mob.tick(&mut ai);
        let here = mob.position();
        let delta = horizontal(here, previous);
        if delta > 0.0 {
            moving_ticks += 1;
            distance_travelled += delta;
            max_tick_step = max_tick_step.max(delta);
        }
        previous = here;
    }

    Run {
        net_displacement: horizontal(mob.position(), spawn),
        distance_travelled,
        moving_ticks,
        max_tick_step,
        path_searches: mob.path_searches(),
        facing: mob.facing(),
        end: mob.position(),
        goals_installed,
    }
}

/// The gate. A brain-driven species, spawned the way the server spawns one, walks
/// a real A\* path.
///
/// The prediction is not "it moved". Three quantities are predicted from `STEP`
/// and the follower's own definition:
///
/// * `max_tick_step` is **exactly** `STEP`, to `1e-9`. `advance` scales the
///   horizontal delta by `step_per_tick / horizontal` whenever the waypoint is
///   further away than one step, so a mob mid-path covers precisely `STEP` and the
///   maximum over a 200-tick run must land on it — no tolerance. Any other value
///   means something other than this follower moved the mob, and a stubbed
///   `move_to` scores `0.0`.
/// * `moving_ticks` clears a floor derived from the mechanism rather than guessed:
///   on flat ground `RandomStroll` writes a target the first tick it is absent and
///   A\* always succeeds, so the mob is walking on all but the handful of
///   hand-off ticks per stroll. Half the run is a deliberately loose floor around
///   a value that should be near-total; the point is the gap from the control's
///   `0`, not the fraction.
/// * `distance_travelled` must be consistent with the other two: `moving_ticks`
///   steps of at most `STEP` each cannot exceed `moving_ticks * STEP`. Asserting
///   the *upper* bound too is what stops a "mob teleported" bug reading as
///   success.
#[test]
fn a_brain_driven_mob_walks_a_real_astar_path() {
    let ticks = 200;
    let r = run(BRAIN_SPECIES, ticks, None, true);

    assert_eq!(
        r.goals_installed, 1,
        "precondition: a brain species must get exactly one goal (the BrainGoal) \
         and no fallback stroll — got {}. Two writers on movement is the bug this \
         count exists to catch.",
        r.goals_installed
    );
    assert!(
        r.path_searches > 0,
        "the brain never reached the real pathfinder: {} A* searches. A stubbed \
         `move_to` scores exactly this.",
        r.path_searches
    );
    assert!(
        (r.max_tick_step - STEP).abs() < 1e-9,
        "largest single-tick displacement was {} but the kinematic follower can \
         only ever apply exactly {STEP}; some other code moved this mob",
        r.max_tick_step
    );
    assert!(
        r.moving_ticks >= ticks / 2,
        "the mob moved on only {}/{ticks} ticks; a brain on flat ground strolls \
         continuously, and the no-driver control scores 0",
        r.moving_ticks
    );
    assert!(
        r.distance_travelled <= r.moving_ticks as f64 * STEP + 1e-9,
        "travelled {} over {} moving ticks, which exceeds {} — the follower \
         cannot cover more than {STEP} per tick, so this is a teleport",
        r.distance_travelled,
        r.moving_ticks,
        r.moving_ticks as f64 * STEP
    );
    assert!(
        r.net_displacement > 1.0,
        "net displacement {} is within a block of the spawn point, so whatever \
         moved is jitter rather than travel",
        r.net_displacement
    );
}

/// **Control, run and observed.** The same mob, the same world, the same ticks,
/// with the driver registration removed — the behavioural assertions must vanish,
/// not merely weaken.
///
/// This is the arm that makes the test above evidence. Without it, "the mob moved"
/// is compatible with the harness moving it: `NavigatingMob::advance` runs
/// unconditionally inside `tick`, and a follower with a stale path would keep
/// walking with no goal at all.
///
/// Its premise is worth stating because a control that cannot fire is worse than
/// none: with an empty `GoalSelector` nothing ever calls `move_to`, so
/// `PathNavigator` never receives a path, and `advance`'s `navigator.tick` returns
/// `None` and early-returns with zero velocity. The zero is therefore *caused by*
/// the missing registration and not by the world being unwalkable — which the
/// positive arm above independently proves, since it walks the same `Flat`.
#[test]
fn without_the_driver_registration_the_mob_does_not_move_at_all() {
    let ticks = 200;
    let r = run(BRAIN_SPECIES, ticks, None, false);

    assert_eq!(r.goals_installed, 0, "control must install no goals");
    assert_eq!(
        r.moving_ticks, 0,
        "control moved on {} ticks — the positive arm's movement cannot then be \
         attributed to the brain",
        r.moving_ticks
    );
    assert_eq!(
        r.path_searches, 0,
        "control ran {} A* searches with no goals installed",
        r.path_searches
    );
    assert_eq!(r.distance_travelled, 0.0, "control travelled a non-zero distance");
    assert_eq!(r.facing, None, "control turned its head with no goal to ask it to");
}

/// The second observable channel, end to end: sensor → memory → behaviour → body.
///
/// `set_nearest_player` is the only thing the test tells the mob. From there the
/// chain is entirely internal: `NearestPlayerSensor` writes
/// `NEAREST_VISIBLE_PLAYER`, `SetPlayerLookTarget` reads that and writes
/// `LOOK_TARGET`, and `LookAtTargetSink` — a *different* behaviour, in a
/// *different* activity, that never names the first one — reads `LOOK_TARGET` and
/// calls `look_at` on the real body. Four links, three of them memory-mediated.
///
/// The asserted value is **byte-exact and has no tolerance**: `LookAtTargetSink`
/// passes the remembered position straight through, so `facing()` must equal the
/// player position it was fed, not merely point somewhere near it.
///
/// Two ticks, because the hand-off costs one: tick 1's sensor+gate write
/// `LOOK_TARGET` *after* the sink already tried and failed to start, so the sink
/// starts and ticks on tick 2. That is `Brain::tick`'s documented ordering, and a
/// one-tick version of this test would fail for the right reason.
#[test]
fn the_brain_turns_a_real_mobs_head_toward_a_player_inside_the_look_distance() {
    let spawn = start();
    let player = Vec3::new(spawn.x + 7.0, spawn.y, spawn.z);
    assert!(
        (player - spawn).length() < LOOK_DISTANCE,
        "precondition: the near player must be inside {LOOK_DISTANCE}"
    );

    let r = run(BRAIN_SPECIES, 2, Some(player), true);

    assert_eq!(
        r.facing,
        Some(player),
        "the look chain did not reach the body; facing was {:?}",
        r.facing
    );
}

/// The same chain, bracketed across its **exact** threshold.
///
/// `SetPlayerLookTarget` gates on `d.dot(d) > max_dist_sqr` with `max_dist` =
/// [`LOOK_DISTANCE`]. So a player at `7.0` blocks is looked at and one at `9.0` is
/// not — the two arms straddle `8.0` by a full block each, which separates the
/// real threshold from the adjacent hypotheses (a `6.0` look distance, or no gate
/// at all). "It looked at the player" passes for any of those; this does not.
///
/// The mob can drift at most `2 * STEP = 0.46` blocks over the two ticks, an order
/// of magnitude inside the `1.0` margin, so the brackets hold without a tolerance
/// on the distance itself. In the far arm it drifts even less: with the player out
/// of look range the gate falls through to `RandomStroll`, whose walk target the
/// move sink cannot act on until tick 3.
#[test]
fn a_player_beyond_the_look_distance_does_not_turn_the_head() {
    let spawn = start();
    let near = Vec3::new(spawn.x + 7.0, spawn.y, spawn.z);
    let far = Vec3::new(spawn.x + 9.0, spawn.y, spawn.z);
    assert!((near - spawn).length() < LOOK_DISTANCE);
    assert!((far - spawn).length() > LOOK_DISTANCE);

    let inside = run(BRAIN_SPECIES, 2, Some(near), true);
    let outside = run(BRAIN_SPECIES, 2, Some(far), true);

    assert_eq!(inside.facing, Some(near), "the near arm must look");
    assert_eq!(
        outside.facing, None,
        "the far arm looked at {:?} despite being beyond {LOOK_DISTANCE} blocks — \
         the distance gate is not being applied",
        outside.facing
    );
}

/// The negative arm on the *roster* side: installing brains must not have changed
/// what a goal-driven species gets.
///
/// `goals_for` grew an early return, and the failure mode of that edit is silent —
/// a species wrongly classified as brain-driven loses its entire vanilla goal
/// table and starts wandering instead of attacking, which looks like a working mob.
/// So this checks both halves: the table is still *built* (a count floor), and it
/// still *works* (the mob does the thing the table exists to make it do).
///
/// # Why the behavioural half supplies a player
///
/// The first draft of this arm ran the zombie for 200 ticks with **no** player and
/// asserted it navigated. That assertion was **never true**, before or after this
/// change — a premise-false control, caught only by running it. Measured in this
/// exact arena:
///
/// | ticks | 200 | 400 | 1000 | 2000 | 5000 |
/// |---|---|---|---|---|---|
/// | A\* searches | **0** | 1 | 5 | 14 | 32 |
/// | moving ticks | **0** | 27 | 87 | 337 | 929 |
///
/// An undisturbed zombie's only mover is `RandomStrollGoal`, whose `can_use`
/// requires `mob.next_i32(120) == 0` (`ai/goals.rs`, vanilla's 120-tick interval),
/// so the first stroll lands somewhere between tick 200 and 400 on this mob's
/// deterministic seed. Raising the budget would have "fixed" it while still
/// resting the whole arm on a 1-in-120 lottery — the weakest thing the table does.
///
/// Supplying a player instead exercises the zombie table's actual signature —
/// `NearestAttackableTargetGoal` acquires, `MeleeAttackGoal` paths in — which is
/// immediate and deterministic (10 searches inside 200 ticks). It is a strictly
/// stronger claim about the same wiring.
///
/// The predicted value is the **final gap**, not a displacement: a mob that walks
/// four blocks the wrong way has the same displacement as one that closes. Starting
/// gap is `5.0` and the mob must end inside `ATTACK_REACH` (`2.0`), so the two
/// hypotheses — "closed on the player" and "never moved" — are separated by three
/// full blocks, and "walked through the player" is excluded from below.
#[test]
fn a_goal_driven_species_keeps_its_whole_roster_table() {
    let ctx = SpeciesContext::new(STEP);
    let zombie = goals_for(GOAL_SPECIES, &ctx);
    assert!(
        zombie.len() > 1,
        "{GOAL_SPECIES} got {} goals; the brain early-return has swallowed a \
         goal-system species",
        zombie.len()
    );

    let spawn = start();
    let player = Vec3::new(spawn.x + START_GAP, spawn.y, spawn.z);
    assert!(
        (horizontal(spawn, player) - START_GAP).abs() < 1e-9,
        "precondition: the player must start exactly {START_GAP} blocks away"
    );

    let r = run(GOAL_SPECIES, 200, Some(player), true);

    assert!(
        r.path_searches > 0,
        "{GOAL_SPECIES} never reached the pathfinder with a player in range: {} \
         searches. Its target/melee goals are not driving navigation.",
        r.path_searches
    );
    let gap = horizontal(r.end, player);
    assert!(
        gap < ATTACK_REACH,
        "{GOAL_SPECIES} ended {gap:.3} blocks from the player, outside its \
         {ATTACK_REACH}-block reach — it did not close the {START_GAP}-block gap \
         (a mob that never moved scores exactly {START_GAP})"
    );
    assert!(
        gap > 0.0,
        "{GOAL_SPECIES} ended on top of the player ({gap:.3}); the follower is \
         walking through its target rather than stopping at reach"
    );
}
