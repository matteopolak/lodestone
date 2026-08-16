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

/// Walks toward a claimed point-of-interest position read from `source` (e.g.
/// [`MemoryModuleType::JOB_SITE`]/`HOME`/`MEETING_POINT`) — vanilla's
/// `SetWalkTargetFromBlockMemory`, simplified. A one-shot: when farther than
/// `close_enough` from the position `source` names, it writes
/// [`MemoryModuleType::WALK_TARGET`] so [`MoveToTargetSink`] does the actual
/// walking; when already close enough it does nothing, leaving the mob to
/// whatever else its current activity's other behaviours do while "at work"
/// (or "in bed", or "at the bell").
///
/// **Two disclosed cuts against the jar original.** Vanilla additionally (1)
/// walks toward an intermediate point *along the way* rather than the POI
/// itself when the target is farther than a `tooFarDistance`, retrying up to
/// 1000 times before giving up, and (2) tracks
/// [`MemoryModuleType::CANT_REACH_WALK_TARGET_SINCE`] to abandon (release) a
/// claim that has stayed unreachable for a duration. Neither is ported: this
/// always walks straight at the claimed position and never releases a claim
/// for being unreachable — a villager whose workstation, bed or bell sits
/// behind unnavigable terrain keeps retrying rather than eventually giving up
/// and re-claiming elsewhere.
#[derive(Debug)]
pub struct WalkToPoi {
    source: MemoryModuleType,
    speed: f32,
    close_enough: i32,
    entry: [(MemoryModuleType, MemoryStatus); 2],
}

impl WalkToPoi {
    /// Walks toward `source`'s position at `speed`, stopping once within
    /// `close_enough` blocks.
    #[must_use]
    pub fn new(source: MemoryModuleType, speed: f32, close_enough: i32) -> Self {
        Self {
            source,
            speed,
            close_enough,
            entry: [
                (MemoryModuleType::WALK_TARGET, MemoryStatus::ValueAbsent),
                (source, MemoryStatus::ValuePresent),
            ],
        }
    }
}

impl Behavior for WalkToPoi {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    fn check_extra_start_conditions(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) -> bool {
        let Some(&MemoryValue::Pos(target)) = mem.get(self.source) else {
            return false;
        };
        let d = target - mob.position();
        let close_enough_sqr = f64::from(self.close_enough) * f64::from(self.close_enough);
        if d.dot(d) > close_enough_sqr {
            mem.set(
                MemoryModuleType::WALK_TARGET,
                MemoryValue::WalkTarget(WalkTarget::new(target, self.speed, self.close_enough)),
            );
        }
        true
    }

    fn name(&self) -> &'static str {
        "walk_to_poi"
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

/// Rolls `min + next_i32(max + 1 - min)` — vanilla's `UniformInt.sample`,
/// reused by both goat-ram cooldown rolls below rather than duplicated.
fn uniform_roll(mob: &mut dyn BrainMob, min: i32, max: i32) -> i32 {
    let span = max + 1 - min;
    if span > 0 { min + mob.next_i32(span) } else { min }
}

/// `PrepareRamNearestTarget` (`world/entity/ai/behavior/PrepareRamNearestTarget.java`)
/// — the goat ram's first phase: pick the nearest visible living entity, back
/// away from it to a ramming distance, wait
/// [`ram_prepare_time`](Self::new)'s worth of ticks once there, then hand off
/// to [`RamTarget`] by writing [`MemoryModuleType::RAM_TARGET`].
///
/// # Three disclosed cuts from the jar original
///
/// * **No `TargetingConditions` species/world-border filter.** Vanilla's
///   `RAM_TARGET_CONDITIONS` excludes other goats and (with `mobGriefing`
///   off) armour stands; this crate's [`NearbyBrainEntity`](super::mob::NearbyBrainEntity)
///   carries no species tag a filter could read (see
///   [`NearestVisibleLivingEntitiesSensor`](super::sensor::NearestVisibleLivingEntitiesSensor)'s
///   own doc), so the nearest visible living entity is always the candidate —
///   including another goat.
/// * **No real pathfinding-based start-position search.** Vanilla scans the
///   four cardinal directions for the walkable cell furthest from the target
///   (up to [`max_ram_distance`](Self::new)) and picks whichever reachable one
///   is nearest the goat. This picks the point [`min_ram_distance`](Self::new)–[`max_ram_distance`](Self::new)
///   blocks from the target **directly behind the goat's current bearing to
///   it** — no walkability check — and leans on [`BrainMob::move_to`]'s own
///   real A\* to fail closed: an unreachable point simply never lets the goat
///   arrive, so the behaviour times out and pays the fail cooldown exactly as
///   a vanilla goat with no reachable ramming cell would.
/// * **"Did the target move" is a >1-block horizontal displacement, not a
///   block-position inequality.** [`BrainMob`] has no block-grid concept; a
///   sub-block displacement is deliberately not treated as movement so a
///   goat is not perpetually re-aiming at a target taking small steps.
#[derive(Debug)]
pub struct PrepareRam {
    min_ram_distance: f64,
    max_ram_distance: f64,
    walk_speed: f32,
    ram_prepare_time: i32,
    cooldown_on_fail_min: i32,
    cooldown_on_fail_max: i32,
    candidate: Option<Vec3>,
    target_id: Option<i32>,
    target_pos: Option<Vec3>,
    reached_at: Option<i64>,
    entry: [(MemoryModuleType, MemoryStatus); 3],
}

impl PrepareRam {
    /// `min_ram_distance`/`max_ram_distance` are `RAM_MIN_DISTANCE`/`RAM_MAX_DISTANCE`
    /// (4/7 for a goat), `walk_speed` is `SPEED_MULTIPLIER_WHEN_PREPARING_TO_RAM`
    /// (1.25), `ram_prepare_time` is `RAM_PREPARE_TIME` (20), and the two
    /// cooldown bounds are `GoatAi.TIME_BETWEEN_RAMS`'s own range (600–6000
    /// for a non-screaming goat; this crate does not model the screaming
    /// variant's shorter 100–300 range).
    #[must_use]
    pub fn new(
        min_ram_distance: f64,
        max_ram_distance: f64,
        walk_speed: f32,
        ram_prepare_time: i32,
        cooldown_on_fail_min: i32,
        cooldown_on_fail_max: i32,
    ) -> Self {
        Self {
            min_ram_distance,
            max_ram_distance,
            walk_speed,
            ram_prepare_time,
            cooldown_on_fail_min,
            cooldown_on_fail_max,
            candidate: None,
            target_id: None,
            target_pos: None,
            reached_at: None,
            entry: [
                (MemoryModuleType::RAM_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
                (MemoryModuleType::RAM_TARGET, MemoryStatus::ValueAbsent),
                (
                    MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES,
                    MemoryStatus::ValuePresent,
                ),
            ],
        }
    }

    /// Picks a backing-away point `[min_ram_distance, max_ram_distance]`
    /// blocks from `target_pos`, on the line through the mob's own current
    /// position — see this struct's own doc for why this replaces vanilla's
    /// walkable-cell search.
    fn choose_start_position(&self, origin: Vec3, target_pos: Vec3) -> Vec3 {
        let dx = origin.x - target_pos.x;
        let dz = origin.z - target_pos.z;
        let current = dx.hypot(dz);
        let (dir_x, dir_z) = if current > 1.0e-4 {
            (dx / current, dz / current)
        } else {
            (1.0, 0.0)
        };
        let distance = current.clamp(self.min_ram_distance, self.max_ram_distance);
        Vec3::new(
            target_pos.x + dir_x * distance,
            target_pos.y,
            target_pos.z + dir_z * distance,
        )
    }

    fn fail_cooldown(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) {
        let cooldown = uniform_roll(mob, self.cooldown_on_fail_min, self.cooldown_on_fail_max);
        mem.set_with_expiry(
            MemoryModuleType::RAM_COOLDOWN_TICKS,
            MemoryValue::Unit,
            i64::from(cooldown),
        );
    }
}

impl Behavior for PrepareRam {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    // `PrepareRamNearestTarget`'s own `super(..., 160)`: a fixed duration, not
    // a rolled range.
    fn min_duration(&self) -> i32 {
        160
    }

    fn max_duration(&self) -> i32 {
        160
    }

    fn check_extra_start_conditions(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) -> bool {
        let Some(MemoryValue::Entities(ids)) = mem.get(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES)
        else {
            return false;
        };
        // Nearest-first (the sensor's own contract), so the first id present
        // in the live snapshot is vanilla's `findClosest`.
        let ids = ids.clone();
        let nearby = mob.nearby_entities();
        let Some(target) = ids.iter().find_map(|id| nearby.iter().find(|e| e.id == *id)) else {
            self.fail_cooldown(mem, mob);
            return false;
        };
        let origin = mob.position();
        self.candidate = Some(self.choose_start_position(origin, target.position));
        self.target_id = Some(target.id);
        self.target_pos = Some(target.position);
        self.reached_at = None;
        true
    }

    fn can_still_use(&mut self, _mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) -> bool {
        self.candidate.is_some()
    }

    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) {
        let (Some(start_pos), Some(target_id)) = (self.candidate, self.target_id) else {
            return;
        };
        let live = mob.nearby_entities().into_iter().find(|e| e.id == target_id);
        let Some(live) = live else {
            // The target is no longer in the perceived set — vanilla's
            // `canStillUse` reads `target.isAlive()`, which this seam cannot
            // query directly; a target that vanished from perception stands
            // in for it.
            self.candidate = None;
            return;
        };
        mem.set(
            MemoryModuleType::WALK_TARGET,
            MemoryValue::WalkTarget(WalkTarget::new(start_pos, self.walk_speed, 0)),
        );
        mem.set(MemoryModuleType::LOOK_TARGET, MemoryValue::Pos(live.position));

        let moved = self
            .target_pos
            .is_some_and(|prev| horizontal_distance(prev, live.position) > 1.0);
        if moved {
            self.candidate = Some(self.choose_start_position(mob.position(), live.position));
            self.target_pos = Some(live.position);
            self.reached_at = None;
            mob.stop_navigation();
            return;
        }

        if horizontal_distance(mob.position(), start_pos) <= 0.5 {
            if self.reached_at.is_none() {
                self.reached_at = Some(time);
            }
            if time - self.reached_at.expect("just set") >= i64::from(self.ram_prepare_time) {
                mem.set(MemoryModuleType::RAM_TARGET, MemoryValue::Pos(live.position));
                self.candidate = None;
            }
        }
    }

    fn stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) {
        if !mem.has_value(MemoryModuleType::RAM_TARGET) {
            self.fail_cooldown(mem, mob);
        }
        mem.erase(MemoryModuleType::LOOK_TARGET);
        self.candidate = None;
        self.target_id = None;
        self.target_pos = None;
        self.reached_at = None;
    }

    fn name(&self) -> &'static str {
        "prepare_ram"
    }
}

/// `RamTarget` (`world/entity/ai/behavior/RamTarget.java`) — the goat ram's
/// second phase: charge [`MemoryModuleType::RAM_TARGET`] at `speed`, and hit
/// the first thing that comes within [`CONTACT_RANGE`](Self::CONTACT_RANGE).
///
/// **Three disclosed cuts from the jar original.** The impact deals no
/// jar-derived knockback direction/force of its own — it records a plain
/// [`BrainMob::attack`] at the victim's position and leaves
/// damage/knockback to whatever pipeline already resolves a goal-driven
/// melee hit, rather than porting `RamTarget`'s own charge-direction,
/// speed-scaled knockback formula (this is the same seam
/// [`super::mob::BrainMob::attack`]'s own doc already names). The
/// horn-breaking-block check (`hasRammedHornBreakingBlock`) is not ported —
/// this seam has no block-state read. And the "reached or gave up" exit
/// check reads the **mob's own position**, not vanilla's literal
/// (effectively self-comparing) one — see [`tick`](Behavior::tick)'s own
/// inline comment for why a faithful transcription would not test distance
/// at all.
#[derive(Debug)]
pub struct RamTarget {
    speed: f32,
    cooldown_min: i32,
    cooldown_max: i32,
    entry: [(MemoryModuleType, MemoryStatus); 2],
}

impl RamTarget {
    /// A hit lands once the mob is within this many blocks (horizontally) of
    /// whatever it is perceiving — a standin for vanilla's own bounding-box
    /// overlap test (`level.getNearbyEntities(..., body.getBoundingBox())`),
    /// which this seam has no box to evaluate.
    pub const CONTACT_RANGE: f64 = 1.2;

    /// `speed` is `SPEED_MULTIPLIER_WHEN_RAMMING` (3.0 for a goat); the
    /// cooldown bounds are the same `GoatAi.TIME_BETWEEN_RAMS` range
    /// [`PrepareRam::new`] takes, reused here for the post-ram cooldown vanilla's
    /// own `finishRam` rolls from the identical supplier.
    #[must_use]
    pub fn new(speed: f32, cooldown_min: i32, cooldown_max: i32) -> Self {
        Self {
            speed,
            cooldown_min,
            cooldown_max,
            entry: [
                (MemoryModuleType::RAM_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
                (MemoryModuleType::RAM_TARGET, MemoryStatus::ValuePresent),
            ],
        }
    }

    fn finish(&self, mem: &mut Memories, mob: &mut dyn BrainMob) {
        let cooldown = uniform_roll(mob, self.cooldown_min, self.cooldown_max);
        mem.set_with_expiry(
            MemoryModuleType::RAM_COOLDOWN_TICKS,
            MemoryValue::Unit,
            i64::from(cooldown),
        );
        mem.erase(MemoryModuleType::RAM_TARGET);
        mob.stop_navigation();
    }
}

impl Behavior for RamTarget {
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)] {
        &self.entry
    }

    // `RamTarget`'s own `super(..., 200)`: fixed, not rolled.
    fn min_duration(&self) -> i32 {
        200
    }

    fn max_duration(&self) -> i32 {
        200
    }

    fn start(&mut self, mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) {
        if let Some(&MemoryValue::Pos(target)) = mem.get(MemoryModuleType::RAM_TARGET) {
            mem.set(
                MemoryModuleType::WALK_TARGET,
                MemoryValue::WalkTarget(WalkTarget::new(target, self.speed, 0)),
            );
        }
    }

    fn can_still_use(&mut self, mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) -> bool {
        mem.has_value(MemoryModuleType::RAM_TARGET)
    }

    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) {
        let origin = mob.position();
        let hit = mob
            .nearby_entities()
            .into_iter()
            .find(|e| horizontal_distance(origin, e.position) <= Self::CONTACT_RANGE);
        if let Some(hit) = hit {
            mob.attack(hit.position);
            self.finish(mem, mob);
            return;
        }

        let Some(&MemoryValue::Pos(ram_target)) = mem.get(MemoryModuleType::RAM_TARGET) else {
            self.finish(mem, mob);
            return;
        };
        // Vanilla's own `lostOrReachedTarget` compares
        // `walkTarget.get().getTarget().currentPosition()` — the *WalkTarget's
        // own* fixed position, built from this exact `ramTargetPos` in
        // `start()` — against `ramTarget.get()`, the same value read back from
        // memory. Both sides trace to the identical constant, so the literal
        // transcription is within `0.25` of itself on every tick regardless of
        // whether the goat has moved at all — not a distance check in any
        // useful sense. This port instead checks the **mob's own current
        // position** against `ram_target`, which is what "reached" or "gave up
        // without connecting" actually has to mean for the charge to be
        // visible.
        let reached_or_lost = match mem.get(MemoryModuleType::WALK_TARGET) {
            Some(_) => horizontal_distance(origin, ram_target) <= 0.25,
            None => true,
        };
        if reached_or_lost {
            self.finish(mem, mob);
        }
    }

    fn stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, _time: i64) {
        // A safety net for the 200-tick hard timeout — `tick`'s own paths
        // already call `finish` (and erase `RAM_TARGET`) for every other
        // exit, so this only fires if none of them did.
        if mem.has_value(MemoryModuleType::RAM_TARGET) {
            self.finish(mem, mob);
        }
    }

    fn name(&self) -> &'static str {
        "ram_target"
    }
}

#[cfg(test)]
mod walk_to_poi_tests {
    use super::*;

    struct RecordingMob {
        pos: Vec3,
        moved_to: Option<(Vec3, f32)>,
    }

    impl BrainMob for RecordingMob {
        fn next_i32(&mut self, _bound: i32) -> i32 {
            0
        }
        fn next_f32(&mut self) -> f32 {
            0.0
        }
        fn game_time(&self) -> i64 {
            0
        }
        fn position(&self) -> Vec3 {
            self.pos
        }
        fn move_to(&mut self, target: Vec3, speed: f32) -> bool {
            self.moved_to = Some((target, speed));
            true
        }
        fn navigation_done(&self) -> bool {
            true
        }
        fn stop_navigation(&mut self) {}
        fn look_at(&mut self, _target: Vec3) {}
        fn random_land_pos(&mut self, _max_xz: i32, _max_y: i32) -> Option<Vec3> {
            None
        }
    }

    const JOB_SITE: MemoryModuleType = MemoryModuleType::JOB_SITE;

    fn memories_with_job_site(pos: Option<Vec3>) -> Memories {
        let mut mem = Memories::new();
        mem.register(JOB_SITE);
        mem.register(MemoryModuleType::WALK_TARGET);
        if let Some(pos) = pos {
            mem.set(JOB_SITE, MemoryValue::Pos(pos));
        }
        mem
    }

    /// Far from the claimed position: a fresh `WALK_TARGET` is written toward
    /// it, at the configured speed and close-enough distance.
    #[test]
    fn writes_a_walk_target_toward_the_claim_when_far() {
        let mut mem = memories_with_job_site(Some(Vec3::new(20.0, 0.0, 0.0)));
        let mut mob = RecordingMob {
            pos: Vec3::default(),
            moved_to: None,
        };
        let mut b = WalkToPoi::new(JOB_SITE, 0.4, 1);
        assert!(b.check_extra_start_conditions(&mut mem, &mut mob));
        assert_eq!(
            mem.get(MemoryModuleType::WALK_TARGET),
            Some(&MemoryValue::WalkTarget(WalkTarget::new(
                Vec3::new(20.0, 0.0, 0.0),
                0.4,
                1
            )))
        );
    }

    /// Already within `close_enough`: no walk target is written — the mob
    /// stays put rather than jittering in place.
    #[test]
    fn writes_nothing_when_already_close_enough() {
        let mut mem = memories_with_job_site(Some(Vec3::new(0.5, 0.0, 0.0)));
        let mut mob = RecordingMob {
            pos: Vec3::default(),
            moved_to: None,
        };
        let mut b = WalkToPoi::new(JOB_SITE, 0.4, 1);
        assert!(b.check_extra_start_conditions(&mut mem, &mut mob));
        assert!(!mem.has_value(MemoryModuleType::WALK_TARGET));
    }

    /// No claim at all: the behaviour cannot start (its entry condition
    /// requires the source memory present), matching the `Leaf` gate rather
    /// than `check_extra_start_conditions` — asserted directly here since a
    /// `WalkToPoi` used in isolation must not crash on an absent memory.
    #[test]
    fn does_nothing_when_the_claim_memory_holds_no_value() {
        let mut mem = memories_with_job_site(None);
        let mut mob = RecordingMob {
            pos: Vec3::default(),
            moved_to: None,
        };
        let mut b = WalkToPoi::new(JOB_SITE, 0.4, 1);
        assert!(!b.check_extra_start_conditions(&mut mem, &mut mob));
        assert!(!mem.has_value(MemoryModuleType::WALK_TARGET));
    }
}
