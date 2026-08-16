//! A representative behaviour set: the CORE + IDLE scaffold every brain-mob shares.
//!
//! These five behaviours reproduce the universal spine of a vanilla brain — the
//! part that is present in the axolotl, camel, allay, villager and warden alike:
//!
//! * [`RandomStroll`] writes a random [`WalkTarget`](super::memory::WalkTarget)
//!   when none is set.
//! * [`MoveToTargetSink`] consumes a `WALK_TARGET`, drives navigation, and
//!   clears it on arrival.
//! * [`SetPlayerLookTarget`] writes a `LOOK_TARGET` from the nearest player.
//! * [`LookAtTargetSink`] consumes a `LOOK_TARGET` and turns the head.
//!
//! The stroll ⇄ move-sink pair is the key demonstration: two behaviours that
//! never reference each other coordinate entirely through the shared
//! `WALK_TARGET` memory. That memory-mediated hand-off *is* the Brain
//! architecture, and it is what these behaviours exist to prove.

use super::behavior::{Behavior, DEFAULT_DURATION};
use super::memory::{Memories, MemoryModuleType, MemoryStatus, MemoryValue, WalkTarget};
use super::mob::BrainMob;
use lodestone_model::Vec3;

fn horizontal_distance(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    dx.hypot(dz)
}

/// Picks a random nearby land position and stores it as the walk target, but
/// only while no walk target is already set. A one-shot (runs a single tick).
#[derive(Debug)]
pub struct RandomStroll {
    speed: f32,
    max_xz: i32,
    max_y: i32,
    may_stroll_from_water: bool,
    entry: [(MemoryModuleType, MemoryStatus); 1],
}

impl RandomStroll {
    /// A land stroll at `speed`, radii 10×7 as in vanilla.
    #[must_use]
    pub fn new(speed: f32) -> Self {
        Self {
            speed,
            max_xz: 10,
            max_y: 7,
            may_stroll_from_water: true,
            entry: [(MemoryModuleType::WALK_TARGET, MemoryStatus::ValueAbsent)],
        }
    }
}

impl Behavior for RandomStroll {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    fn check_extra_start_conditions(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) -> bool {
        if !self.may_stroll_from_water && mob.in_water() {
            return false;
        }
        let target = mob
            .random_land_pos(self.max_xz, self.max_y)
            .map(|pos| MemoryValue::WalkTarget(WalkTarget::new(pos, self.speed, 0)));
        mem.set_or_erase(MemoryModuleType::WALK_TARGET, target);
        true
    }

    fn name(&self) -> &'static str {
        "random_stroll"
    }
}

/// Consumes a walk target, drives navigation toward it, and clears it once
/// reached or unreachable. This is the only behaviour that commands movement.
#[derive(Debug)]
pub struct MoveToTargetSink {
    min_duration: i32,
    max_duration: i32,
    remaining_cooldown: i32,
    has_path: bool,
    entry: [(MemoryModuleType, MemoryStatus); 3],
}

impl MoveToTargetSink {
    /// A move sink with vanilla's default 150–250 tick timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::with_timeout(150, 250)
    }

    /// A move sink with an explicit timeout range.
    #[must_use]
    pub fn with_timeout(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
            remaining_cooldown: 0,
            has_path: false,
            entry: [
                (
                    MemoryModuleType::CANT_REACH_WALK_TARGET_SINCE,
                    MemoryStatus::Registered,
                ),
                (MemoryModuleType::PATH, MemoryStatus::ValueAbsent),
                (MemoryModuleType::WALK_TARGET, MemoryStatus::ValuePresent),
            ],
        }
    }

    fn walk_target(mem: &Memories) -> Option<WalkTarget> {
        match mem.get(MemoryModuleType::WALK_TARGET) {
            Some(MemoryValue::WalkTarget(wt)) => Some(*wt),
            _ => None,
        }
    }

    fn reached(mob: &dyn BrainMob, wt: &WalkTarget) -> bool {
        horizontal_distance(mob.position(), wt.pos) <= f64::from(wt.close_enough) + 0.5
    }
}

impl Default for MoveToTargetSink {
    fn default() -> Self {
        Self::new()
    }
}

impl Behavior for MoveToTargetSink {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    fn min_duration(&self) -> i32 {
        self.min_duration
    }

    fn max_duration(&self) -> i32 {
        self.max_duration
    }

    fn check_extra_start_conditions(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) -> bool {
        if self.remaining_cooldown > 0 {
            self.remaining_cooldown -= 1;
            return false;
        }
        let Some(wt) = Self::walk_target(mem) else {
            return false;
        };
        let reached = Self::reached(mob, &wt);
        if !reached && mob.move_to(wt.pos, wt.speed) {
            self.has_path = true;
            return true;
        }
        mem.erase(MemoryModuleType::WALK_TARGET);
        if reached {
            mem.erase(MemoryModuleType::CANT_REACH_WALK_TARGET_SINCE);
        }
        false
    }

    fn can_still_use(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) -> bool {
        if !self.has_path {
            return false;
        }
        let Some(wt) = Self::walk_target(mem) else {
            return false;
        };
        !mob.navigation_done() && !Self::reached(mob, &wt)
    }

    fn stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) {
        let unreached = Self::walk_target(mem).is_some_and(|wt| !Self::reached(mob, &wt));
        if unreached && mob.navigation_stuck() {
            self.remaining_cooldown = mob.next_i32(40);
        }
        mob.stop_navigation();
        mem.erase(MemoryModuleType::WALK_TARGET);
        mem.erase(MemoryModuleType::PATH);
        self.has_path = false;
    }

    fn name(&self) -> &'static str {
        "move_to_target_sink"
    }
}

/// Writes the nearest visible player as the look target, within `max_dist`.
/// A one-shot.
#[derive(Debug)]
pub struct SetPlayerLookTarget {
    max_dist_sqr: f64,
    entry: [(MemoryModuleType, MemoryStatus); 2],
}

impl SetPlayerLookTarget {
    /// Looks at players within `max_dist` blocks.
    #[must_use]
    pub fn new(max_dist: f32) -> Self {
        Self {
            max_dist_sqr: f64::from(max_dist) * f64::from(max_dist),
            entry: [
                (MemoryModuleType::LOOK_TARGET, MemoryStatus::ValueAbsent),
                (
                    MemoryModuleType::NEAREST_VISIBLE_PLAYER,
                    MemoryStatus::ValuePresent,
                ),
            ],
        }
    }
}

impl Behavior for SetPlayerLookTarget {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    fn check_extra_start_conditions(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) -> bool {
        let Some(&MemoryValue::Pos(player)) = mem.get(MemoryModuleType::NEAREST_VISIBLE_PLAYER)
        else {
            return false;
        };
        let d = player - mob.position();
        if d.dot(d) > self.max_dist_sqr {
            return false;
        }
        mem.set(MemoryModuleType::LOOK_TARGET, MemoryValue::Pos(player));
        true
    }

    fn name(&self) -> &'static str {
        "set_player_look_target"
    }
}

/// Consumes a look target and turns the head toward it each tick, clearing it on
/// stop.
#[derive(Debug)]
pub struct LookAtTargetSink {
    min_duration: i32,
    max_duration: i32,
    entry: [(MemoryModuleType, MemoryStatus); 1],
}

impl LookAtTargetSink {
    /// A look sink with the given timeout range.
    #[must_use]
    pub fn new(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
            entry: [(MemoryModuleType::LOOK_TARGET, MemoryStatus::ValuePresent)],
        }
    }
}

impl Default for LookAtTargetSink {
    fn default() -> Self {
        Self::new(DEFAULT_DURATION, DEFAULT_DURATION)
    }
}

impl Behavior for LookAtTargetSink {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    fn min_duration(&self) -> i32 {
        self.min_duration
    }

    fn max_duration(&self) -> i32 {
        self.max_duration
    }

    fn can_still_use(&mut self, mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) -> bool {
        mem.has_value(MemoryModuleType::LOOK_TARGET)
    }

    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) {
        if let Some(&MemoryValue::Pos(target)) = mem.get(MemoryModuleType::LOOK_TARGET) {
            mob.look_at(target);
        }
    }

    fn stop(&mut self, mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) {
        mem.erase(MemoryModuleType::LOOK_TARGET);
    }

    fn name(&self) -> &'static str {
        "look_at_target_sink"
    }
}

/// `AnimalPanic` (`world/entity/ai/behavior/AnimalPanic.java`) — flees a
/// recent attacker at `speed_multiplier` for 100–120 ticks, re-picking a
/// random fleeing destination every time navigation finishes. Lives in
/// `CORE` in vanilla (goat, camel, armadillo, frog, sniffer, allay all
/// register it there), which is why it interrupts whatever `IDLE` behaviour
/// was running rather than competing with it for a turn — matching the
/// `RandomStroll`/[`MoveToTargetSink`] pair's own "coordinate only through
/// `WALK_TARGET`" shape, one activity level up.
///
/// **Two disclosed cuts**, both already named on [`super::sensor::HurtBySensor`]:
/// no damage-type filter (every hurt panics, not just
/// `DamageTypeTags.PANIC_CAUSES`), and no on-fire water-seeking branch
/// (`AnimalPanic.getPanicPos`'s `lookForWater` needs a block/fluid read no
/// [`BrainMob`] seam exposes). Per-species extras on top of the plain
/// constructor — the sniffer resets its sniffing memory on start, the
/// armadillo rolls out of its ball — are not modelled either; each is a
/// single vanilla override with no equivalent memory in this crate yet.
#[derive(Debug)]
pub struct Panic {
    speed_multiplier: f32,
    entry: [(MemoryModuleType, MemoryStatus); 2],
}

impl Panic {
    /// `new AnimalPanic(speedMultiplier)` — the per-species figure is the
    /// caller's own jar citation, not this struct's.
    #[must_use]
    pub fn new(speed_multiplier: f32) -> Self {
        Self {
            speed_multiplier,
            entry: [
                (MemoryModuleType::IS_PANICKING, MemoryStatus::Registered),
                (MemoryModuleType::HURT_BY, MemoryStatus::Registered),
            ],
        }
    }
}

impl Behavior for Panic {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    // `AnimalPanic`'s own constructor: `super(..., 100, 120)`.
    fn min_duration(&self) -> i32 {
        100
    }

    fn max_duration(&self) -> i32 {
        120
    }

    fn check_extra_start_conditions(&mut self, mem: &mut Memories, _mob: &mut dyn BrainMob) -> bool {
        // `AnimalPanic.checkExtraStartConditions`: a fresh hurt, or a panic
        // already in progress (so a hurt landing mid-flee re-arms the timer
        // rather than letting the behaviour lapse and restart from scratch).
        mem.has_value(MemoryModuleType::HURT_BY) || mem.has_value(MemoryModuleType::IS_PANICKING)
    }

    fn can_still_use(&mut self, _mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) -> bool {
        true
    }

    fn start(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) {
        // `AnimalPanic.start`: mark panicking, drop whatever walk target was
        // already in flight, and stop navigating toward it.
        mem.set(MemoryModuleType::IS_PANICKING, MemoryValue::Unit);
        mem.erase(MemoryModuleType::WALK_TARGET);
        mob.stop_navigation();
    }

    fn stop(&mut self, mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) {
        mem.erase(MemoryModuleType::IS_PANICKING);
    }

    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) {
        // `AnimalPanic.tick`: only pick a new fleeing point once the current
        // one is exhausted, not every tick — a panicking mob commits to each
        // leg of its flight rather than juddering toward a new point every
        // frame.
        if mob.navigation_done()
            && let Some(pos) = mob.random_land_pos(5, 4)
        {
            mem.set(
                MemoryModuleType::WALK_TARGET,
                MemoryValue::WalkTarget(WalkTarget::new(pos, self.speed_multiplier, 0)),
            );
        }
    }

    fn name(&self) -> &'static str {
        "panic"
    }
}
