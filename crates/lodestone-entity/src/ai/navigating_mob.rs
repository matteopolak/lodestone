//! A reference mob composition that wires the goal scheduler to the *real*
//! pathfinder and navigator.
//!
//! Everywhere else the [`MobController`] seam is filled by a test fake whose
//! `move_to` just records a call and returns `true` — so a goal deciding to move
//! has never once driven an A\* search or followed a computed path. The goal
//! scheduler ([`GoalSelector`](super::GoalSelector)) is proven hermetically and
//! the [`PathFinder`] is proven against a live zombie, but *nothing composes
//! them*: they are two islands joined by a seam a fake always stubs. That is the
//! same shape as a decoder the adapter never calls.
//!
//! [`NavigatingMob`] is the composition that closes the gap. Its `move_to` runs
//! the real [`PathFinder`] over the [`PathWorld`] seam, [`advance`] follows the
//! resulting [`Path`](crate::pathfinding::Path) one step through the real
//! [`PathNavigator`], and the whole thing is drivable by a `GoalSelector`. It
//! owns only `lodestone-entity` parts over the version-free `PathWorld` seam, so
//! it introduces no world, physics or version dependency.
//!
//! The follower is deliberately **kinematic**, not the physics integrator: each
//! tick it steps toward the next waypoint at a caller-supplied blocks/tick
//! (derived from the mob's movement-speed attribute). The exact
//! ground-speed→velocity mapping is `lodestone-physics`' job. What this
//! composition proves is the goal→navigation→movement *wiring* and the
//! *topological* behaviour the seam's fakes could never show: that a
//! goal-driven mob actually invokes A\*, reaches its target, and detours an
//! unjumpable fence instead of walking through it.

use lodestone_model::{BlockPos, Vec3};

use super::goal::GoalSelector;
use super::mob::MobController;
use crate::pathfinding::{MobShape, PathFinder, PathNavigator, PathParams, PathStart, PathWorld};

/// A tiny deterministic RNG (SplitMix64) so a `NavigatingMob` needs no `rand`
/// dependency and its stroll behaviour is reproducible in tests.
#[derive(Debug, Clone)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A float in `[0, 1)`.
    fn next_unit(&mut self) -> f64 {
        // 53-bit mantissa, matching the usual `nextDouble` construction.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A reference mob that composes a [`GoalSelector`] with the real
/// [`PathFinder`] / [`PathNavigator`] over a [`PathWorld`].
///
/// Drive it each tick with [`NavigatingMob::tick`], which runs the goals (they
/// call back into this mob's [`MobController`] impl) and then advances the
/// follower one kinematic step.
pub struct NavigatingMob<'w> {
    world: &'w dyn PathWorld,
    shape: MobShape,
    finder: PathFinder,
    navigator: PathNavigator,
    pos: Vec3,
    /// Blocks travelled per tick along the path (kinematic follower speed).
    step_per_tick: f64,
    rng: SplitMix64,
    attack_target: Option<Vec3>,
    /// The block the current path was computed toward, so `move_to` reuses the
    /// active path instead of recomputing every tick (vanilla `moveTo` reuse).
    active_target_block: Option<BlockPos>,
    last_look: Option<Vec3>,
    jumping: bool,
    attacks: Vec<Vec3>,
    move_calls: u32,
    path_searches: u32,
    /// Monotonic tick counter (advanced once per [`advance`]/[`tick`]), used to
    /// throttle recomputation the way vanilla's game clock does.
    tick_count: u64,
    /// The tick a same-destination re-search last ran, so a wedged mob does not
    /// recompute A\* every tick (vanilla `PathNavigation.recomputePath` refuses
    /// to recompute within 20 ticks — `MAX_TIME_RECOMPUTE`).
    last_search_tick: Option<u64>,
}

impl std::fmt::Debug for NavigatingMob<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavigatingMob")
            .field("shape", &self.shape)
            .field("pos", &self.pos)
            .field("step_per_tick", &self.step_per_tick)
            .field("attack_target", &self.attack_target)
            .field("active_target_block", &self.active_target_block)
            .field("jumping", &self.jumping)
            .field("attacks", &self.attacks)
            .field("move_calls", &self.move_calls)
            .field("path_searches", &self.path_searches)
            .finish_non_exhaustive()
    }
}

impl<'w> NavigatingMob<'w> {
    /// Creates a mob at `pos` with body `shape`, moving `step_per_tick` blocks
    /// per tick, pathfinding through `world`.
    ///
    /// `visited_budget` bounds the A\* open set (vanilla derives it as
    /// `floor(followRange * 16)`).
    #[must_use]
    pub fn new(
        world: &'w dyn PathWorld,
        shape: MobShape,
        pos: Vec3,
        step_per_tick: f64,
        visited_budget: i32,
    ) -> Self {
        let width = shape.width;
        Self {
            world,
            shape,
            finder: PathFinder::new(visited_budget),
            navigator: PathNavigator::new(width),
            pos,
            step_per_tick,
            rng: SplitMix64(0x1234_5678_9ABC_DEF0),
            attack_target: None,
            active_target_block: None,
            last_look: None,
            jumping: false,
            attacks: Vec::new(),
            move_calls: 0,
            path_searches: 0,
            tick_count: 0,
            last_search_tick: None,
        }
    }

    /// Overrides the RNG seed (affects stroll target selection only).
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = SplitMix64(seed);
        self
    }

    /// The mob's current position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.pos
    }

    /// The targets the mob has struck, in order (for tests).
    #[must_use]
    pub fn attacks(&self) -> &[Vec3] {
        &self.attacks
    }

    /// How many times a goal asked this mob to move.
    #[must_use]
    pub fn move_calls(&self) -> u32 {
        self.move_calls
    }

    /// How many actual A\* searches ran — the count the seam's fakes can never
    /// produce, since their `move_to` never touches a pathfinder.
    #[must_use]
    pub fn path_searches(&self) -> u32 {
        self.path_searches
    }

    /// Whether the navigator gave up because the mob stopped progressing.
    #[must_use]
    pub fn is_stuck(&self) -> bool {
        self.navigator.is_stuck()
    }

    /// The last position a goal asked the mob to look at, if any.
    #[must_use]
    pub fn facing(&self) -> Option<Vec3> {
        self.last_look
    }

    /// Whether a goal has the mob holding jump this tick.
    #[must_use]
    pub fn is_jumping(&self) -> bool {
        self.jumping
    }

    /// Whether a path is currently being followed.
    #[must_use]
    pub fn has_path(&self) -> bool {
        !self.navigator.is_done()
    }

    /// Runs one AI tick: the goal selector (whose goals call back through the
    /// [`MobController`] seam) followed by one kinematic follower step.
    pub fn tick(&mut self, ai: &mut GoalSelector) {
        ai.tick(self);
        self.advance();
    }

    /// Advances the follower one step toward the current waypoint. Public so a
    /// caller running its own goal loop can drive movement explicitly.
    pub fn advance(&mut self) {
        self.tick_count += 1;
        let Some(waypoint) = self.navigator.tick(self.pos) else {
            return;
        };
        let dx = waypoint.x - self.pos.x;
        let dz = waypoint.z - self.pos.z;
        let horizontal = (dx * dx + dz * dz).sqrt();
        if horizontal <= self.step_per_tick || horizontal == 0.0 {
            self.pos.x = waypoint.x;
            self.pos.z = waypoint.z;
        } else {
            let scale = self.step_per_tick / horizontal;
            self.pos.x += dx * scale;
            self.pos.z += dz * scale;
        }
        // Grounded follower: snap the vertical to the waypoint's floor.
        self.pos.y = waypoint.y;
    }
}

impl MobController for NavigatingMob<'_> {
    fn next_f32(&mut self) -> f32 {
        self.rng.next_unit() as f32
    }

    fn next_i32(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        (self.rng.next_u64() % bound as u64) as i32
    }

    fn next_f64(&mut self) -> f64 {
        self.rng.next_unit()
    }

    fn position(&self) -> Vec3 {
        self.pos
    }

    fn move_to(&mut self, target: Vec3, speed: f64) -> bool {
        let block = BlockPos::new(
            target.x.floor() as i32,
            target.y.floor() as i32,
            target.z.floor() as i32,
        );
        // Reuse the active path unless it finished or the goal now wants a
        // different destination block (vanilla `PathNavigation.moveTo` reuse).
        let same_target = self.active_target_block == Some(block);
        let recompute = self.navigator.is_done() || !same_target;
        if !recompute {
            self.move_calls += 1;
            return true;
        }

        // Vanilla `recomputePath` refuses to re-search the *same* destination
        // within `MAX_TIME_RECOMPUTE` (20) ticks. Only a genuinely new target
        // block bypasses the throttle; a wedged mob whose path finished stands
        // still until the cooldown elapses instead of hammering A\* every tick.
        if same_target
            && self
                .last_search_tick
                .is_some_and(|last| self.tick_count.saturating_sub(last) < 20)
        {
            // Report whether we still hold a followable path.
            return !self.navigator.is_done();
        }

        self.path_searches += 1;
        self.last_search_tick = Some(self.tick_count);
        // Remember the block we searched toward *regardless of success*, so an
        // unreachable target throttles re-search the same as a reachable one
        // (otherwise a wedged mob resets `same_target` every tick and hammers A*).
        self.active_target_block = Some(block);
        let start = PathStart::grounded(self.pos.x, self.pos.y, self.pos.z);
        let params = PathParams {
            max_path_length: 200.0,
            reach_range: 1,
            visited_multiplier: 1.0,
        };
        match self
            .finder
            .find_path(self.world, &self.shape, start, &[block], params)
        {
            Some(path) => {
                self.navigator.start(path, speed as f32);
                self.move_calls += 1;
                true
            }
            None => false,
        }
    }

    fn navigation_done(&self) -> bool {
        self.navigator.is_done()
    }

    fn stop_navigation(&mut self) {
        self.navigator.stop();
        self.active_target_block = None;
    }

    fn set_jumping(&mut self, jumping: bool) {
        self.jumping = jumping;
    }

    fn look_at(&mut self, target: Vec3) {
        self.last_look = Some(target);
    }

    fn look_toward(&mut self, dx: f64, dz: f64) {
        self.last_look = Some(Vec3::new(self.pos.x + dx, self.pos.y, self.pos.z + dz));
    }

    fn attack_target(&self) -> Option<Vec3> {
        self.attack_target
    }

    fn set_attack_target(&mut self, target: Option<Vec3>) {
        self.attack_target = target;
    }

    fn find_nearest_target(&mut self) -> Option<Vec3> {
        self.attack_target
    }

    fn attack(&mut self, target: Vec3) {
        self.attacks.push(target);
    }

    fn random_stroll_target(&mut self) -> Option<Vec3> {
        // A random destination in a 10-block box around the mob, matching
        // `RandomStroll`'s ±7 horizontal reach closely enough for the seam.
        let dx = (self.rng.next_unit() * 20.0 - 10.0).round();
        let dz = (self.rng.next_unit() * 20.0 - 10.0).round();
        Some(Vec3::new(self.pos.x + dx, self.pos.y, self.pos.z + dz))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::ai::goals::MeleeAttackGoal;
    use crate::pathfinding::{Aabb, PathType};

    /// Flat ground one block below `y=0`, plus a set of fence cells with a 1.5
    /// collision top (unjumpable). Mirrors the live-navigation arena so the
    /// composition is exercised against the same block classification a live
    /// zombie was measured on.
    struct Arena {
        walls: HashSet<(i32, i32, i32)>,
    }

    impl Arena {
        fn is_ground(y: i32) -> bool {
            y <= -1
        }
        fn is_wall(&self, x: i32, y: i32, z: i32) -> bool {
            self.walls.contains(&(x, y, z))
        }
        fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
            self.is_wall(x, y, z) || Self::is_ground(y)
        }
    }

    impl PathWorld for Arena {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
            if self.is_solid(x, y, z) {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
            if self.is_wall(x, y, z) {
                1.5
            } else if Self::is_ground(y) {
                1.0
            } else {
                0.0
            }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            let x0 = aabb.min_x.floor() as i32;
            let x1 = (aabb.max_x - 1e-7).floor() as i32;
            let y0 = aabb.min_y.floor() as i32;
            let y1 = (aabb.max_y - 1e-7).floor() as i32;
            let z0 = aabb.min_z.floor() as i32;
            let z1 = (aabb.max_z - 1e-7).floor() as i32;
            for x in x0..=x1 {
                for y in y0..=y1 {
                    for z in z0..=z1 {
                        if self.is_solid(x, y, z) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
            false
        }
    }

    fn fence_wall() -> Arena {
        let mut walls = HashSet::new();
        for z in -3..=3 {
            walls.insert((5, -1, z));
            // Fence occupies the standing layer too (its collision is 1.5 tall).
            walls.insert((5, 0, z));
        }
        Arena { walls }
    }

    fn run_to_target(world: &dyn PathWorld, target: Vec3) -> (bool, f64, Vec<Vec3>) {
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 8000);
        mob.set_attack_target(Some(target));

        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        let mut route = vec![mob.position()];
        let mut reached = false;
        for _ in 0..2000 {
            mob.tick(&mut ai);
            let p = mob.position();
            route.push(p);
            let dx = target.x - p.x;
            let dz = target.z - p.z;
            if (dx * dx + dz * dz).sqrt() < 1.5 {
                reached = true;
                break;
            }
            if mob.is_stuck() {
                break;
            }
        }
        let max_abs_z = route.iter().map(|p| p.z.abs()).fold(0.0f64, f64::max);
        (reached, max_abs_z, route)
    }

    #[test]
    fn goal_drives_pathfinder_straight_line_with_no_obstacle() {
        // Control: no wall. A melee goal must reach the target on a near-straight
        // line — max|z| stays small because nothing forces a detour. This is the
        // anti-vacuity partner of the fence test: if the mob detoured here, the
        // pathfinder (not the goal wiring) would be the thing under test.
        let world = Arena {
            walls: HashSet::new(),
        };
        let (reached, max_abs_z, _route) = run_to_target(&world, Vec3::new(10.5, 0.0, 0.5));
        assert!(reached, "mob reached the open-ground target");
        assert!(
            max_abs_z < 2.0,
            "with no obstacle the goal-driven path stays near z=0, got max|z|={max_abs_z:.2}"
        );
    }

    #[test]
    fn goal_drives_pathfinder_to_detour_an_unjumpable_fence() {
        // The load-bearing test: a `MeleeAttackGoal` — through the real
        // `MobController` seam — must invoke A\*, and the path must go *around*
        // the fence (|z| beyond ±3), not through it. A fake `move_to` (the only
        // other implementor of this seam) could never exercise any of this.
        let world = fence_wall();
        let (reached, max_abs_z, _route) = run_to_target(&world, Vec3::new(10.5, 0.0, 0.5));
        assert!(
            reached,
            "goal-driven mob reached the target past the fence (max|z|={max_abs_z:.2})"
        );
        assert!(
            max_abs_z >= 4.0,
            "goal-driven mob must detour the fence end (|z|>=4), got max|z|={max_abs_z:.2}"
        );
    }

    #[test]
    fn goal_actually_invokes_astar_and_strikes_in_reach() {
        // Proves the seam is wired end to end: real searches ran (not a counter
        // bump), and the mob struck the target once within melee reach.
        let world = fence_wall();
        let shape = MobShape::land(0.6, 1.95);
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 8000);
        mob.set_attack_target(Some(target));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        for _ in 0..2000 {
            mob.tick(&mut ai);
            if !mob.attacks().is_empty() {
                break;
            }
            if mob.is_stuck() {
                break;
            }
        }
        assert!(
            mob.path_searches() >= 1,
            "a real A* search must have run; got {}",
            mob.path_searches()
        );
        assert!(
            !mob.attacks().is_empty(),
            "mob never reached melee reach to strike (searches={}, pos={:?})",
            mob.path_searches(),
            mob.position()
        );
        let hit = mob.attacks()[0];
        assert!((hit.x - target.x).abs() < 0.01 && (hit.z - target.z).abs() < 0.01);
    }

    #[test]
    fn goal_driven_mob_approaches_but_cannot_strike_a_sealed_target() {
        // A target enclosed by a solid wall two cells thick: vanilla's pathfinder
        // returns a *best-effort partial* path (not `None`), so the mob genuinely
        // walks up to the wall — but the nearest reachable cell is >2 blocks from
        // the sealed target, so a `MeleeAttackGoal` can never strike. This asserts
        // two things a fake `move_to` (which teleports/strikes unconditionally)
        // could never satisfy: the mob *does* make forward progress (it followed a
        // real partial path), yet *never* reaches melee reach of the sealed cell.
        let mut walls = HashSet::new();
        for z in -2..=2 {
            for x in 8..=12 {
                for y in -1..=1 {
                    walls.insert((x, y, z));
                }
            }
        }
        // Carve out the target pocket: a walkable floor at (10,-1,0) with open
        // standing space at (10,0,0), fully surrounded by the solid shell.
        walls.remove(&(10, -1, 0));
        walls.remove(&(10, 0, 0));
        walls.remove(&(10, 1, 0));
        let world = Arena { walls };
        let shape = MobShape::land(0.6, 1.95);
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 3000);
        mob.set_attack_target(Some(target));

        // move_to yields a (partial) path, matching vanilla best-effort behaviour.
        let found = mob.move_to(target, 1.0);
        assert!(
            found,
            "vanilla returns a partial path toward an unreachable target"
        );

        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        let mut closest = f64::INFINITY;
        let mut last_x = mob.position().x;
        let mut stalled = 0u32;
        for _ in 0..800 {
            mob.tick(&mut ai);
            let p = mob.position();
            let dx = target.x - p.x;
            let dz = target.z - p.z;
            closest = closest.min((dx * dx + dz * dz).sqrt());
            // Stop once the mob has clearly stalled against the wall: it cannot
            // make progress, so further ticks only re-run A* fruitlessly.
            if (p.x - last_x).abs() < 1e-4 {
                stalled += 1;
                if stalled > 40 {
                    break;
                }
            } else {
                stalled = 0;
            }
            last_x = p.x;
            if mob.is_stuck() {
                break;
            }
        }
        // It walked toward the target (real path following, not a no-op)...
        assert!(
            mob.position().x > 3.0,
            "mob should have advanced along the partial path, stuck at x={:.2}",
            mob.position().x
        );
        // ...but the sealed shell keeps it >2 blocks out, so it never strikes.
        assert!(
            mob.attacks().is_empty(),
            "a sealed target is unreachable and must never be struck (closest={closest:.2})"
        );
        assert!(
            closest > 2.0,
            "the solid shell must keep the mob out of melee reach, got closest={closest:.2}"
        );
    }

    /// Builds the two-thick sealed shell around the target pocket at (10,0,0).
    fn sealed_shell() -> Arena {
        let mut walls = HashSet::new();
        for z in -2..=2 {
            for x in 8..=12 {
                for y in -1..=1 {
                    walls.insert((x, y, z));
                }
            }
        }
        walls.remove(&(10, -1, 0));
        walls.remove(&(10, 0, 0));
        walls.remove(&(10, 1, 0));
        Arena { walls }
    }

    #[test]
    fn endurance_wedged_mob_neither_hammers_astar_nor_oscillates() {
        // Duration test (the class a 200-tick gate cannot see): a mob chasing an
        // *unreachable* target for 4000 ticks. Two end-state invariants:
        //   1. The 20-tick recompute throttle holds for the whole run — a
        //      regression to per-tick searching would make `path_searches` ~4000;
        //      the throttle caps it near ticks/20. This is the "navigator that
        //      leaks / hammers over time" detector.
        //   2. The mob *settles* against the wall rather than pacing forever — its
        //      position over the final 500 ticks stays inside a <1-block box.
        let world = sealed_shell();
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, 0.0, 0.5),
            0.25,
            600,
        );
        mob.set_attack_target(Some(target));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        const TICKS: usize = 2000;
        let mut tail: Vec<Vec3> = Vec::new();
        for t in 0..TICKS {
            mob.tick(&mut ai);
            if t >= TICKS - 500 {
                tail.push(mob.position());
            }
        }

        // (1) Throttle held all run: far below one search per tick.
        assert!(
            mob.path_searches() < (TICKS as u32) / 15,
            "wedged mob hammered A* — {} searches over {TICKS} ticks (throttle regressed?)",
            mob.path_searches()
        );
        // (2) Settled, not oscillating: bounded box over the final 500 ticks.
        let min_x = tail.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let max_x = tail.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        let min_z = tail.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        let max_z = tail.iter().map(|p| p.z).fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (max_x - min_x) < 1.0 && (max_z - min_z) < 1.0,
            "mob never settled: final-500 span x={:.2} z={:.2}",
            max_x - min_x,
            max_z - min_z
        );
        // Never phased through the shell.
        assert!(
            mob.attacks().is_empty(),
            "unreachable target must never be struck"
        );
    }

    #[test]
    fn endurance_reached_mob_settles_at_target_and_does_not_wander_off() {
        // The mirror invariant: a mob that *reaches* a reachable target and then
        // keeps ticking for thousands more ticks must stay *at* the target, not
        // drift away or orbit it. Asserts the end state after long idling — the
        // "works then wanders" bug a short test that breaks-on-reach cannot see.
        let world = Arena {
            walls: HashSet::new(),
        };
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, 0.0, 0.5),
            0.25,
            800,
        );
        mob.set_attack_target(Some(target));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        const TICKS: usize = 2000;
        let mut ever_reached = false;
        let mut tail: Vec<Vec3> = Vec::new();
        for t in 0..TICKS {
            mob.tick(&mut ai);
            let p = mob.position();
            let d = ((target.x - p.x).powi(2) + (target.z - p.z).powi(2)).sqrt();
            if d < 1.5 {
                ever_reached = true;
            }
            if t >= TICKS - 500 {
                tail.push(p);
            }
        }
        assert!(ever_reached, "mob never reached the reachable target");
        // End state after 3500+ ticks of idling at the target: still there.
        let final_pos = *tail.last().unwrap();
        let final_dist =
            ((target.x - final_pos.x).powi(2) + (target.z - final_pos.z).powi(2)).sqrt();
        assert!(
            final_dist < 2.0,
            "mob wandered away from the target it reached (final dist={final_dist:.2})"
        );
        // And it struck it (melee goal actually engaged), repeatedly over the run.
        assert!(
            !mob.attacks().is_empty(),
            "a reached mob should have struck the target at least once"
        );
    }
}
