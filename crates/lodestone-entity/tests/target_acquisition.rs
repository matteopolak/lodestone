//! Does a hostile mob acquire a player it was never told about — and only
//! within its follow range?
//!
//! # What it is
//!
//! The behavioural gate for issue #455. `NavigatingMob::find_nearest_target`
//! used to `return self.attack_target`, the field written by the only goal that
//! calls it, so the loop could never bootstrap: **no mob in a running game ever
//! attacked unprovoked**, while every test of the mechanism passed.
//!
//! # Why this file is not in `navigating_mob.rs`'s own `mod tests`
//!
//! Because of *how* #455 hid, which is the whole lesson. Three separate doubles
//! — `ScriptMob` (`tests/mob_sim.rs`), `goals.rs`'s in-module fake, and
//! `ai/roster/probe.rs` — each override `find_nearest_target` with a working
//! implementation, so every existing test of `NearestAttackableTargetGoal` drove
//! a host that worked. The goal was always correct; the **host** was the defect.
//! Nothing here may use those doubles. Every assertion below runs against a real
//! [`NavigatingMob`] — the only production implementor of
//! [`MobController`] — over a real [`PathWorld`], with goals installed **only**
//! by [`goals_for`], the same function `MobSim::spawn_species` calls.
//!
//! # How to change it
//!
//! The numbers come from the jar, not from our tables:
//!
//! * `FOLLOW_RANGE` is `16.0` for every mob (`Mob.java:166-168`), raised to
//!   `35.0` by the zombie family (`monster/zombie/Zombie.java:133`).
//! * The cut is a full 3-D `distanceToSqr` against `max(range, 2.0)`
//!   (`ai/targeting/TargetingConditions.java:81-88`).
//! * A target that *leaves* follow range is dropped
//!   (`ai/goal/target/TargetGoal.java:57-60`).
//!
//! If you change a distance here, change it because the jar says so.
//!
//! Deliberately absent: line of sight. Vanilla checks it with an eye-to-eye ray
//! (`TargetingConditions.java:90`), which is not a query this seam can answer;
//! see `NavigatingMob::find_nearest_target`'s doc. Every mob here has clear
//! sight of its player, so no assertion depends on the omission.

use std::collections::HashSet;

use lodestone_entity::ai::navigating_mob::{DEFAULT_FOLLOW_RANGE, MIN_TARGET_VISIBILITY_DISTANCE};
use lodestone_entity::ai::{
    GoalSelector, MobController, NavigatingMob, SpeciesContext, goals_for,
};
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
use lodestone_model::Vec3;

/// Vanilla's zombie `FOLLOW_RANGE` (`monster/zombie/Zombie.java:133`,
/// `.add(Attributes.FOLLOW_RANGE, 35.0)`).
const ZOMBIE_FOLLOW_RANGE: f64 = 35.0;

/// Vanilla's zombie `MOVEMENT_SPEED` (`monster/zombie/Zombie.java:132`,
/// `0.23F`) expressed as the blocks-per-tick figure `MobSim` feeds
/// `NavigatingMob` as `step_per_tick`.
const ZOMBIE_STEP: f64 = 0.23;

/// A solid floor at `y <= -1` and open air above it, so A\* always has a route
/// and nothing except a goal can move the mob.
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

/// What one run of a real mob against a real player did.
struct Run {
    /// Distance to the player at the start and at the end.
    gap: (f64, f64),
    /// Whether a goal ever acquired the player as an attack target.
    acquired: bool,
    /// The tick index on which the target was first held, and the distance to
    /// the player at that moment. `None` if it never acquired.
    ///
    /// Both are needed to *predict* how far the mob should then travel:
    /// `NearestAttackableTargetGoal` throttles its search to one tick in
    /// `random_interval` (vanilla's `DEFAULT_RANDOM_INTERVAL = 10`,
    /// `NearestAttackableTargetGoal.java:15`), so a fixed fraction of the whole
    /// run is not a prediction, it is a guess with a tolerance.
    acquired_at: Option<(usize, f64)>,
    /// How many times a goal reached [`MobController::attack`].
    attacks: usize,
}

/// Spawns `species` at the origin, tells it about a player at `player` **only**
/// through the perception feed the server uses (`set_nearest_player`), and ticks
/// `ticks` times.
///
/// `set_attack_target` is never called. That is the point: before #455 the only
/// way a mob acquired anything was for the test to hand it a target, which is
/// what every existing gate does.
fn run(species: &str, follow_range: f64, step: f64, player: Vec3, ticks: usize) -> Run {
    let world = Flat::new();
    let ctx = SpeciesContext::new(step);
    let start = Vec3::new(0.5, 0.0, 0.5);
    let mut mob = NavigatingMob::new(
        &world,
        MobShape::land(0.6, 1.95),
        start,
        step,
        // Vanilla's own budget: `floor(followRange * 16)`, the same expression
        // `mobs.rs:1510` uses.
        (follow_range * 16.0).floor() as i32,
        0,
    );
    mob.set_follow_range(follow_range);

    let mut ai = GoalSelector::new();
    for (priority, goal) in goals_for(species, &ctx) {
        ai.add(priority, goal);
    }
    assert!(
        ai.len() > 0,
        "precondition: the roster installed no goals for {species}, so this run \
         measures nothing"
    );

    let gap_start = (start - player).length();
    let mut acquired_at = None;
    for t in 0..ticks {
        // The server refreshes this every tick with no range cut at all
        // (`mobs.rs`'s `feed_perception`), which is exactly why the range filter
        // has to live in the host.
        mob.set_nearest_player(Some(player));
        mob.tick(&mut ai);
        if acquired_at.is_none() && mob.attack_target().is_some() {
            acquired_at = Some((t, (mob.position() - player).length()));
        }
    }

    Run {
        gap: (gap_start, (mob.position() - player).length()),
        acquired: acquired_at.is_some(),
        acquired_at,
        attacks: mob.attacks().len(),
    }
}

/// The headline: an unprovoked zombie closes on a player it was never told
/// about, and gets close enough to hit it.
///
/// The prediction is not "it moved" — that is satisfied by a single twitch, or
/// by `RandomStrollGoal` wandering in a lucky direction. A pursuing mob walks
/// exactly `ZOMBIE_STEP` blocks per tick along its path, so once the throttled
/// search has acquired on tick `t`, the closure over the remaining
/// `ticks - 1 - t` ticks is **`(ticks - 1 - t) * ZOMBIE_STEP`, to within one
/// tick's worth**, and the run is sized so pursuit never saturates at melee
/// reach. The bracket excludes both wrong hypotheses: zero (acquired but not
/// pursuing) and a full-run figure (moving before it could have known).
#[test]
fn an_unprovoked_zombie_closes_on_a_player_it_was_never_told_about() {
    let ticks = 40;
    let player = Vec3::new(12.5, 0.0, 0.5);
    let r = run("zombie", ZOMBIE_FOLLOW_RANGE, ZOMBIE_STEP, player, ticks);

    let Some((t, gap_at_acquire)) = r.acquired_at else {
        panic!(
            "the zombie never acquired the player in {ticks} ticks. This is #455 \
             itself: find_nearest_target must read the perception feed, not the \
             attack target the calling goal writes"
        )
    };
    assert!(
        gap_at_acquire > 4.0,
        "the zombie was already {gap_at_acquire:.3} blocks away when it \
         acquired, so this run cannot separate pursuit from melee-reach \
         saturation"
    );

    let travelled = gap_at_acquire - r.gap.1;
    let predicted = (ticks - 1 - t) as f64 * ZOMBIE_STEP;
    assert!(
        (travelled - predicted).abs() <= ZOMBIE_STEP,
        "acquired on tick {t} at {gap_at_acquire:.3} blocks and ended at \
         {:.3}: closed {travelled:.3} blocks where a {ZOMBIE_STEP} blocks/tick \
         mob pursuing for {} ticks must close {predicted:.3}",
        r.gap.1,
        ticks - 1 - t
    );
}

/// The magnitude arm: follow range must *separate* two otherwise identical runs.
///
/// A creeper's `FOLLOW_RANGE` is the `16.0` default. A player at `15.5` blocks
/// is inside it and one at `16.5` is outside, and the difference between them is
/// **exact immobility** — the out-of-range mob's final position is compared
/// byte-for-byte with where it started.
///
/// A creeper rather than a zombie because a creeper's roster has no
/// `WaterAvoidingRandomStrollGoal`… it does, so immobility is not available from
/// the roster set; see [`the_range_cut_is_the_jars_and_not_a_rounded_guess`] for
/// the arm that isolates it. Here the separator is acquisition and pursuit.
#[test]
fn a_player_just_outside_follow_range_is_not_acquired_and_one_just_inside_is() {
    let ticks = 30;
    let inside = Vec3::new(0.5 + DEFAULT_FOLLOW_RANGE - 0.5, 0.0, 0.5);
    let outside = Vec3::new(0.5 + DEFAULT_FOLLOW_RANGE + 0.5, 0.0, 0.5);

    let near = run("creeper", DEFAULT_FOLLOW_RANGE, 0.25, inside, ticks);
    let far = run("creeper", DEFAULT_FOLLOW_RANGE, 0.25, outside, ticks);

    assert!(
        near.acquired,
        "a player {:.1} blocks away is inside a {DEFAULT_FOLLOW_RANGE}-block \
         follow range and must be acquired",
        near.gap.0
    );
    assert!(
        !far.acquired,
        "a player {:.1} blocks away is OUTSIDE a {DEFAULT_FOLLOW_RANGE}-block \
         follow range and must not be acquired. Reading the unbounded \
         nearest_player feed raw makes every mob in the world target the player",
        far.gap.0
    );

    // And the acquisition has to *matter*: the near mob closes, the far one
    // never gets within melee reach however much it strolls.
    assert!(
        near.gap.1 < near.gap.0 - 0.5 * (ticks as f64 * 0.25),
        "the acquiring creeper closed only {:.3} blocks",
        near.gap.0 - near.gap.1
    );
    assert_eq!(
        far.attacks, 0,
        "a creeper that cannot see the player attacked it {} times",
        far.attacks
    );
}

/// The cut is the jar's number, to the block — not "roughly 16".
///
/// This is the arm that can assert **exact immobility**: the goal set is the
/// bare target registration plus melee attack, so nothing else can move the
/// mob, and an out-of-range mob's position must be bit-identical to its start.
/// It also pins the `max(range, 2.0)` floor
/// (`TargetingConditions.java:83`), which is invisible to any test using a
/// normal follow range.
#[test]
fn the_range_cut_is_the_jars_and_not_a_rounded_guess() {
    let world = Flat::new();
    // Sweep the boundary from both sides at three different attribute values,
    // one of them below the floor.
    for follow_range in [4.0, DEFAULT_FOLLOW_RANGE, ZOMBIE_FOLLOW_RANGE, 1.0] {
        let effective = follow_range.max(MIN_TARGET_VISIBILITY_DISTANCE);
        for (offset, want) in [(-0.25_f64, true), (0.25_f64, false)] {
            let d = effective + offset;
            let mut mob = NavigatingMob::new(
                &world,
                MobShape::land(0.6, 1.95),
                Vec3::new(0.5, 0.0, 0.5),
                0.25,
                256,
                0,
            );
            mob.set_follow_range(follow_range);
            mob.set_nearest_player(Some(Vec3::new(0.5 + d, 0.0, 0.5)));
            assert_eq!(
                mob.find_nearest_target().is_some(),
                want,
                "follow_range {follow_range} (effective {effective}): a player \
                 {d} blocks away should{} be a target",
                if want { "" } else { " not" }
            );
        }
    }
}

/// Vanilla drops a target that walks out of follow range
/// (`ai/goal/target/TargetGoal.java:57-60`) and re-writes the target's live
/// position every tick it does not (`:70`). Ours did neither: it held the point
/// it acquired, for ever.
///
/// Both directions in one run, so a goal that clears the target unconditionally
/// fails the first half and one that never clears it fails the second. Nothing
/// here writes `attack_target` by hand — an earlier draft of this test did, and
/// it was then asserting on its own write rather than on the goal.
#[test]
fn a_pursued_player_is_tracked_while_in_range_and_released_when_it_leaves() {
    let world = Flat::new();
    let ctx = SpeciesContext::new(0.25);
    let mut mob = NavigatingMob::new(
        &world,
        MobShape::land(0.6, 1.95),
        Vec3::new(0.5, 0.0, 0.5),
        0.25,
        256,
        0,
    );
    mob.set_follow_range(DEFAULT_FOLLOW_RANGE);
    let mut ai = GoalSelector::new();
    for (priority, goal) in goals_for("creeper", &ctx) {
        ai.add(priority, goal);
    }

    // Acquire a player 4 blocks away.
    let close = Vec3::new(4.5, 0.0, 0.5);
    for _ in 0..20 {
        mob.set_nearest_player(Some(close));
        mob.tick(&mut ai);
    }
    assert_eq!(
        mob.attack_target(),
        Some(close),
        "precondition: the creeper must acquire the player 4 blocks away before \
         this gate can say anything about tracking or releasing one"
    );

    // The player walks to the far edge of follow range, still inside it. The
    // held target must follow, or a mob chases a ghost.
    let moved = Vec3::new(0.5 + DEFAULT_FOLLOW_RANGE - 1.0, 0.0, 0.5);
    mob.set_nearest_player(Some(moved));
    mob.tick(&mut ai);
    assert_eq!(
        mob.attack_target(),
        Some(moved),
        "the creeper is still holding the position the player left; vanilla's \
         target is a live entity reference (TargetGoal.java:70 re-writes it \
         every tick)"
    );

    // Now the player leaves follow range entirely.
    let gone = Vec3::new(400.5, 0.0, 0.5);
    for _ in 0..3 {
        mob.set_nearest_player(Some(gone));
        mob.tick(&mut ai);
    }
    assert_eq!(
        mob.attack_target(),
        None,
        "the creeper is still holding a target 400 blocks away; \
         can_continue_to_use must re-test the distance"
    );
}

/// A cow must not target the player, and the reason it cannot is worth an
/// assertion rather than a comment: hostility is not a filter inside
/// `find_nearest_target`, it is the **absence of the goal**. No passive table
/// registers `NearestAttackableTargetGoal`, so a cow never asks.
///
/// This is the control that catches the naive form of #455's fix — reading
/// `nearest_player` in a host that every species shares, with the goal installed
/// for everyone.
#[test]
fn no_passive_species_can_acquire_a_target() {
    // A player right on top of them, far inside any follow range.
    let player = Vec3::new(2.5, 0.0, 0.5);
    for species in ["cow", "sheep", "pig", "chicken", "mooshroom", "rabbit"] {
        let r = run(species, DEFAULT_FOLLOW_RANGE, 0.25, player, 30);
        assert!(
            !r.acquired,
            "{species} acquired the player as an attack target. Hostility comes \
             from the roster row, and no passive table has one"
        );
        assert_eq!(
            r.attacks, 0,
            "{species} attacked the player {} times",
            r.attacks
        );
    }
}

/// The regression the coordinator's B4 unit predicted: a *neutral* species must
/// not become hostile the moment `find_nearest_target` starts searching.
///
/// Vanilla's zombified piglin, wolf and bee registrations end in a
/// `this::isAngryAt` selector, which narrows the candidate set to the entity
/// their persistent grudge names (`NeutralMob.isAngryAt`). Our predicate-free
/// `NearestAttackableTargetGoal` has no equivalent, so those three rows are
/// `Coverage::Missing` — and while `find_nearest_target` was circular, a wrongly
/// `Modelled` row would have been *invisible*. It is not invisible any more:
/// this gate fails if one is flipped without the anger primitive.
///
/// `NearestAttackableTargetGoal::anger_gated` is the shape that may eventually
/// carry them, and it is checked here too: a mob with no grudge acquires
/// nothing, a mob with one acquires the grudge holder rather than the nearest
/// player. Nothing installs it yet, which is why the neutral rows stay
/// `Missing`.
#[test]
fn a_neutral_species_does_not_turn_hostile_on_sight() {
    let player = Vec3::new(2.5, 0.0, 0.5);
    for species in ["zombified_piglin", "wolf", "bee", "enderman"] {
        let r = run(species, DEFAULT_FOLLOW_RANGE, 0.25, player, 30);
        assert!(
            !r.acquired,
            "{species} is neutral in vanilla and just attacked a player on \
             sight. Its target row is gated on isAngryAt; modelling it with our \
             predicate-free goal is what this forbids"
        );
    }
}

/// The anger-gated registration reads the grudge, not the neighbourhood — the
/// two directions, so the gate is not satisfied by a goal that always fires.
#[test]
fn an_anger_gated_registration_targets_only_the_grudge_holder() {
    use lodestone_entity::ai::goals::NearestAttackableTargetGoal;

    let world = Flat::new();
    let nearby_player = Vec3::new(3.5, 0.0, 0.5);
    let grudge = Vec3::new(7.5, 0.0, 0.5);

    let build = || {
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, 0.0, 0.5),
            0.25,
            256,
            0,
        );
        mob.set_follow_range(DEFAULT_FOLLOW_RANGE);
        mob.set_nearest_player(Some(nearby_player));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(NearestAttackableTargetGoal::anger_gated()));
        (mob, ai)
    };

    let (mut calm, mut ai) = build();
    for _ in 0..20 {
        calm.tick(&mut ai);
    }
    assert!(
        calm.attack_target().is_none(),
        "an anger-gated goal acquired a player with no grudge outstanding — it \
         is reading the perception feed, which is the hostile registration's job"
    );

    let (mut angry, mut ai) = build();
    angry.set_angry_target(Some(grudge));
    for _ in 0..20 {
        angry.set_angry_target(Some(grudge));
        angry.tick(&mut ai);
    }
    assert_eq!(
        angry.attack_target(),
        Some(grudge),
        "an angry mob must target the entity its grudge names, not the nearer \
         player at {nearby_player:?}"
    );
}
