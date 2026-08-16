//! The Brain/Behavior AI system — vanilla's *other* mob-AI architecture.
//!
//! # Two architectures, one roster
//!
//! Minecraft has **two** mob-AI systems and 26.2 ships both. Older mobs
//! (zombies, skeletons, creepers, most animals) run on the flag-preemptive
//! [`GoalSelector`](crate::ai::GoalSelector) in [`crate::ai`]. Newer mobs run on
//! this **Brain** system: memory-driven, activity-scheduled, and coordinated
//! through a shared blackboard rather than mutually-exclusive flags.
//!
//! A census of the decompiled 26.2 sources (see the crate report) finds **20
//! concrete mobs on Brain** — allay, armadillo, axolotl, breeze, camel, copper
//! golem, creaking, frog, goat, happy ghast, hoglin, nautilus, piglin, piglin
//! brute, sniffer, tadpole, villager, warden, zoglin, zombie nautilus — versus
//! **~50 concrete mobs on `GoalSelector`**. Brain is a real minority (~29% of
//! mob classes) but covers the entire modern roster, so both systems are
//! first-class here.
//!
//! # How a brain differs
//!
//! There are no `MOVE`/`LOOK`/`JUMP`/`TARGET` flags. Instead:
//!
//! * **Sensors** ([`Sensor`]) write perception into **memory**
//!   ([`Memories`]) each tick.
//! * **Memories** ([`MemoryModuleType`]) are a shared blackboard with optional
//!   expiry. A behaviour declares which memories must be present or absent to
//!   run, and produces or consumes values.
//! * **Behaviours** ([`BehaviorControl`]) are scheduled by integer priority
//!   within the currently-active **activities** ([`Activity`]). Mutual exclusion
//!   is *emergent*: only one behaviour writes `WALK_TARGET`, and the move sink
//!   refuses to start while a path exists.
//! * **Activities** gate whole behaviour sets. A brain runs its core activities
//!   plus at most one non-core activity, switched by
//!   [`Brain::set_active_activity_to_first_valid`] (or a schedule).
//!
//! [`Brain::tick`] runs the exact vanilla order: forget outdated memories, tick
//! sensors, start each eligible non-running behaviour, then tick each running
//! one.

mod activity;
mod behavior;
mod behaviors;
mod driver;
mod gate;
mod memory;
mod mob;
pub mod roster;
mod sensor;

pub use activity::Activity;
pub use behavior::{Behavior, BehaviorControl, DEFAULT_DURATION, Leaf, Status};
pub use behaviors::{
    AvoidTarget, CopyMemoryWithExpiry, LookAtTargetSink, MoveToTargetSink, RandomStroll,
    SetPlayerLookTarget, WalkToPoi,
};
pub use driver::BrainGoal;
pub use gate::{GateBehavior, OrderPolicy, RunningPolicy};
pub use memory::{Memories, MemoryModuleType, MemoryStatus, MemoryValue, WalkTarget};
pub use mob::{BrainMob, NearbyBrainEntity};
pub use roster::{BRAIN_SPECIES, brain_for, is_brain_species, scaffold};
pub use sensor::{
    HurtBySensor, NearestHostileSensor, NearestPlayerSensor, NearestVisibleZombifiedSensor,
    Sensor, VillagerPoiSensor,
};

use std::collections::HashMap;

/// One scheduled behaviour with its priority and owning activity.
struct BehaviorEntry {
    priority: i32,
    activity: Activity,
    control: Option<Box<dyn BehaviorControl>>,
}

/// A mob's brain: memories, sensors, and priority-scheduled behaviours grouped
/// into activities.
///
/// Construct one with [`Brain::new`], wire it with the builder-style methods
/// ([`add_sensor`](Brain::add_sensor), [`add_activity`](Brain::add_activity),
/// [`set_core_activities`](Brain::set_core_activities)), then drive it once per
/// server tick with [`tick`](Brain::tick).
pub struct Brain {
    memories: Memories,
    sensors: Vec<Box<dyn Sensor>>,
    behaviors: Vec<BehaviorEntry>,
    activity_requirements: HashMap<Activity, Vec<(MemoryModuleType, MemoryStatus)>>,
    /// A second, **disjunctive** requirement table for activities registered
    /// through [`add_activity_any_of`](Self::add_activity_any_of): the
    /// activity is eligible if *any* listed `(memory, status)` pair holds,
    /// rather than [`activity_requirements`](Self::activity_requirements)'
    /// all-must-hold rule.
    ///
    /// Exists for villager-shaped panic triggers: vanilla's own
    /// `VillagerPanicTrigger` is an imperative `Behavior` that calls
    /// `brain.setActiveActivityIfPossible(Activity.PANIC)` directly from
    /// inside `start()`, which this crate's [`Behavior`] trait has no seam
    /// for (it receives `&mut Memories` and `&mut dyn BrainMob`, never `&mut
    /// Brain` — deliberately, so a behaviour cannot reach into the scheduler
    /// that owns it). The declarative equivalent already wired through
    /// [`BrainGoal::tick`](super::BrainGoal)'s per-tick
    /// `set_active_activity_to_first_valid` call is "PANIC is eligible
    /// whenever hurt OR a hostile is nearby, and takes precedence over IDLE
    /// in the candidate list" — which needs OR, not AND, hence this table
    /// rather than reusing [`activity_requirements`](Self::activity_requirements).
    activity_any_requirements: HashMap<Activity, Vec<(MemoryModuleType, MemoryStatus)>>,
    activity_memories_to_erase_when_stopped: HashMap<Activity, Vec<MemoryModuleType>>,
    core_activities: Vec<Activity>,
    active_activities: Vec<Activity>,
    default_activity: Activity,
    schedule: Option<Vec<(i32, Activity)>>,
    last_schedule_update: i64,
}

impl Brain {
    const SCHEDULE_UPDATE_DELAY: i64 = 20;

    /// A fresh brain with [`Activity::CORE`] as the sole core activity and
    /// [`Activity::IDLE`] as the default, mirroring vanilla's `Brain()`
    /// constructor.
    #[must_use]
    pub fn new() -> Self {
        let mut brain = Self {
            memories: Memories::new(),
            sensors: Vec::new(),
            behaviors: Vec::new(),
            activity_requirements: HashMap::new(),
            activity_any_requirements: HashMap::new(),
            activity_memories_to_erase_when_stopped: HashMap::new(),
            core_activities: vec![Activity::CORE],
            active_activities: Vec::new(),
            default_activity: Activity::IDLE,
            schedule: None,
            last_schedule_update: -9999,
        };
        brain.use_default_activity();
        brain
    }

    /// Direct access to the memory blackboard, for hosts and sensors.
    #[must_use]
    pub fn memories(&self) -> &Memories {
        &self.memories
    }

    /// Mutable access to the memory blackboard, for hosts setting a target.
    pub fn memories_mut(&mut self) -> &mut Memories {
        &mut self.memories
    }

    /// Registers a memory slot so it can later hold a value.
    pub fn register_memory(&mut self, ty: MemoryModuleType) {
        self.memories.register(ty);
    }

    /// Sets which activities are always active. Also makes them the initial
    /// active set via [`use_default_activity`](Brain::use_default_activity).
    pub fn set_core_activities(&mut self, activities: &[Activity]) {
        self.core_activities = activities.to_vec();
        self.use_default_activity();
    }

    /// Sets the fallback activity used when no requested activity's requirements
    /// are met.
    pub fn set_default_activity(&mut self, activity: Activity) {
        self.default_activity = activity;
    }

    /// Adds a sensor, registering the memories it writes.
    pub fn add_sensor(&mut self, sensor: Box<dyn Sensor>) {
        for ty in sensor.output_memories() {
            self.memories.register(ty);
        }
        self.sensors.push(sensor);
    }

    /// Registers an activity: its prioritised behaviours, the memory conditions
    /// that make it eligible, and memories to erase when it stops being active.
    ///
    /// Registering also registers every memory the behaviours require, matching
    /// vanilla — which is why [`Memories::set`] on an unregistered memory is a
    /// silent no-op rather than an error.
    pub fn add_activity(
        &mut self,
        activity: Activity,
        behaviors: Vec<(i32, Box<dyn BehaviorControl>)>,
        conditions: Vec<(MemoryModuleType, MemoryStatus)>,
        memories_to_erase_when_stopped: Vec<MemoryModuleType>,
    ) {
        self.activity_requirements.insert(activity, conditions);
        if !memories_to_erase_when_stopped.is_empty() {
            self.activity_memories_to_erase_when_stopped
                .insert(activity, memories_to_erase_when_stopped);
        }
        for (priority, control) in behaviors {
            for ty in control.required_memories() {
                self.memories.register(ty);
            }
            self.behaviors.push(BehaviorEntry {
                priority,
                activity,
                control: Some(control),
            });
        }
        // Stable sort keeps insertion order within a priority (vanilla's
        // LinkedHashSet), while ordering the buckets ascending (its TreeMap).
        self.behaviors.sort_by_key(|e| e.priority);
    }

    /// Registers an activity exactly like [`add_activity`](Self::add_activity),
    /// except its eligibility is **disjunctive**: it is eligible if *any* of
    /// `any_conditions` holds, not only if all of them do. See
    /// [`activity_any_requirements`](Self::activity_any_requirements)'s own
    /// doc for why this exists and when to reach for it over `add_activity`.
    ///
    /// `any_conditions` must not be empty — an activity with no way to become
    /// eligible is better expressed by omitting the call entirely, and an
    /// empty list here would otherwise silently mean "never eligible" (vacuously
    /// true for `all()`, vacuously false for `any()` — the two registration
    /// paths disagree on an empty list for exactly this reason, so this method
    /// asserts rather than silently picking one).
    pub fn add_activity_any_of(
        &mut self,
        activity: Activity,
        behaviors: Vec<(i32, Box<dyn BehaviorControl>)>,
        any_conditions: Vec<(MemoryModuleType, MemoryStatus)>,
        memories_to_erase_when_stopped: Vec<MemoryModuleType>,
    ) {
        assert!(
            !any_conditions.is_empty(),
            "add_activity_any_of needs at least one condition; an activity that \
             should always be eligible belongs in add_activity with an empty list, \
             not here, where an empty list means the opposite"
        );
        self.activity_any_requirements
            .insert(activity, any_conditions);
        if !memories_to_erase_when_stopped.is_empty() {
            self.activity_memories_to_erase_when_stopped
                .insert(activity, memories_to_erase_when_stopped);
        }
        for (priority, control) in behaviors {
            for ty in control.required_memories() {
                self.memories.register(ty);
            }
            self.behaviors.push(BehaviorEntry {
                priority,
                activity,
                control: Some(control),
            });
        }
        self.behaviors.sort_by_key(|e| e.priority);
    }

    /// Attaches a time-of-day schedule as `(start_tick, activity)` pairs sorted
    /// ascending; the latest pair whose start is at or before the current day
    /// time wins.
    pub fn set_schedule(&mut self, mut schedule: Vec<(i32, Activity)>) {
        schedule.sort_by_key(|&(start, _)| start);
        self.schedule = Some(schedule);
    }

    /// Whether a schedule was ever attached via [`set_schedule`](Self::set_schedule).
    /// [`BrainGoal::tick`](super::BrainGoal) reads this to decide whether it is
    /// safe to call [`update_activity_from_schedule`](Self::update_activity_from_schedule)
    /// at all — without a schedule that call falls back to treating `IDLE` as
    /// "the scheduled activity", which would fight a species like the goat
    /// that switches activities through its own candidate list instead.
    #[must_use]
    pub fn has_schedule(&self) -> bool {
        self.schedule.is_some()
    }

    /// The activities currently active (core plus at most one non-core).
    #[must_use]
    pub fn active_activities(&self) -> &[Activity] {
        &self.active_activities
    }

    /// Whether `activity` is currently active.
    #[must_use]
    pub fn is_active(&self, activity: Activity) -> bool {
        self.active_activities.contains(&activity)
    }

    /// The active non-core activity, if any.
    #[must_use]
    pub fn active_non_core_activity(&self) -> Option<Activity> {
        self.active_activities
            .iter()
            .find(|a| !self.core_activities.contains(a))
            .copied()
    }

    /// Resets the active set to the core activities plus the default activity.
    pub fn use_default_activity(&mut self) {
        self.set_active_activity(self.default_activity);
    }

    fn set_active_activity(&mut self, activity: Activity) {
        if self.is_active(activity) {
            return;
        }
        self.erase_memories_for_other_activities_than(activity);
        self.active_activities.clear();
        self.active_activities
            .extend_from_slice(&self.core_activities);
        if !self.active_activities.contains(&activity) {
            self.active_activities.push(activity);
        }
    }

    fn erase_memories_for_other_activities_than(&mut self, activity: Activity) {
        let to_erase: Vec<MemoryModuleType> = self
            .active_activities
            .iter()
            .filter(|&&old| old != activity)
            .filter_map(|old| self.activity_memories_to_erase_when_stopped.get(old))
            .flatten()
            .copied()
            .collect();
        for ty in to_erase {
            self.memories.erase(ty);
        }
    }

    fn activity_requirements_are_met(&self, activity: Activity) -> bool {
        // The two registration paths are mutually exclusive per activity in
        // practice (whichever `add_activity*` call named it last wins the
        // table it lands in), checked in a fixed order so a caller that
        // somehow registered both is not left to platform-dependent map
        // iteration to decide which rule applies.
        if let Some(conditions) = self.activity_requirements.get(&activity) {
            return conditions
                .iter()
                .all(|&(ty, status)| self.memories.check(ty, status));
        }
        if let Some(any_conditions) = self.activity_any_requirements.get(&activity) {
            return any_conditions
                .iter()
                .any(|&(ty, status)| self.memories.check(ty, status));
        }
        false
    }

    /// Switches to `activity` if its requirements are met, otherwise falls back
    /// to the default activity.
    pub fn set_active_activity_if_possible(&mut self, activity: Activity) {
        if self.activity_requirements_are_met(activity) {
            self.set_active_activity(activity);
        } else {
            self.use_default_activity();
        }
    }

    /// Switches to the first activity in `candidates` whose requirements are
    /// met. If none are met, the active set is left unchanged.
    pub fn set_active_activity_to_first_valid(&mut self, candidates: &[Activity]) {
        for &activity in candidates {
            if self.activity_requirements_are_met(activity) {
                self.set_active_activity(activity);
                break;
            }
        }
    }

    /// Every 20 ticks, consults the schedule and switches activity if the
    /// scheduled one is not already active. No-op without a schedule.
    pub fn update_activity_from_schedule(&mut self, day_time: i32, game_time: i64) {
        if game_time - self.last_schedule_update <= Self::SCHEDULE_UPDATE_DELAY {
            return;
        }
        self.last_schedule_update = game_time;
        let scheduled = self.scheduled_activity(day_time);
        if !self.is_active(scheduled) {
            self.set_active_activity_if_possible(scheduled);
        }
    }

    fn scheduled_activity(&self, day_time: i32) -> Activity {
        let Some(schedule) = &self.schedule else {
            return Activity::IDLE;
        };
        let mut current = Activity::IDLE;
        for &(start, activity) in schedule {
            if start <= day_time {
                current = activity;
            }
        }
        current
    }

    /// Runs one brain tick in vanilla's order: forget outdated memories, tick
    /// sensors, start eligible non-running behaviours, then tick running ones.
    pub fn tick(&mut self, mob: &mut dyn BrainMob) {
        let time = mob.game_time();
        self.memories.tick();
        self.tick_sensors(mob);
        self.start_each_non_running_behavior(mob, time);
        self.tick_each_running_behavior(mob, time);
    }

    fn tick_sensors(&mut self, mob: &mut dyn BrainMob) {
        for i in 0..self.sensors.len() {
            self.sensors[i].tick(&mut self.memories, mob);
        }
    }

    fn start_each_non_running_behavior(&mut self, mob: &mut dyn BrainMob, time: i64) {
        for i in 0..self.behaviors.len() {
            let activity = self.behaviors[i].activity;
            if !self.active_activities.contains(&activity) {
                continue;
            }
            let Some(mut ctrl) = self.behaviors[i].control.take() else {
                continue;
            };
            if ctrl.status() == Status::Stopped {
                ctrl.try_start(&mut self.memories, mob, time);
            }
            self.behaviors[i].control = Some(ctrl);
        }
    }

    fn tick_each_running_behavior(&mut self, mob: &mut dyn BrainMob, time: i64) {
        for i in 0..self.behaviors.len() {
            let is_running = self.behaviors[i]
                .control
                .as_ref()
                .is_some_and(|c| c.status() == Status::Running);
            if !is_running {
                continue;
            }
            let Some(mut ctrl) = self.behaviors[i].control.take() else {
                continue;
            };
            ctrl.tick_or_stop(&mut self.memories, mob, time);
            self.behaviors[i].control = Some(ctrl);
        }
    }

    /// The names of every currently-running behaviour, in priority order (for
    /// debugging and tests).
    #[must_use]
    pub fn running_behavior_names(&self) -> Vec<&'static str> {
        self.behaviors
            .iter()
            .filter_map(|e| e.control.as_ref())
            .filter(|c| c.status() == Status::Running)
            .map(|c| c.name())
            .collect()
    }
}

impl Default for Brain {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Brain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Brain")
            .field("active_activities", &self.active_activities)
            .field("sensors", &self.sensors.len())
            .field("behaviors", &self.behaviors.len())
            .field("running", &self.running_behavior_names())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::Vec3;

    /// A deterministic [`BrainMob`] test double. It records every navigation and
    /// look command so a test can assert on the *intents* a brain expressed,
    /// exactly as the goal-system tests do with `ScriptMob`.
    struct TestMob {
        pos: Vec3,
        time: i64,
        player: Option<Vec3>,
        stroll: Vec<Vec3>,
        nav_done: bool,
        moved_to: Vec<(Vec3, f32)>,
        looked_at: Vec<Vec3>,
        ints: Vec<i32>,
        int_i: usize,
        floats: Vec<f32>,
        float_i: usize,
    }

    impl TestMob {
        fn new() -> Self {
            Self {
                pos: Vec3::default(),
                time: 100,
                player: None,
                stroll: Vec::new(),
                nav_done: false,
                moved_to: Vec::new(),
                looked_at: Vec::new(),
                ints: Vec::new(),
                int_i: 0,
                floats: Vec::new(),
                float_i: 0,
            }
        }
    }

    impl BrainMob for TestMob {
        fn next_i32(&mut self, bound: i32) -> i32 {
            if self.ints.is_empty() || bound <= 0 {
                return 0;
            }
            let v = self.ints[self.int_i % self.ints.len()];
            self.int_i += 1;
            v % bound
        }

        fn next_f32(&mut self) -> f32 {
            if self.floats.is_empty() {
                return 0.5;
            }
            let v = self.floats[self.float_i % self.floats.len()];
            self.float_i += 1;
            v
        }

        fn game_time(&self) -> i64 {
            self.time
        }

        fn position(&self) -> Vec3 {
            self.pos
        }

        fn move_to(&mut self, target: Vec3, speed: f32) -> bool {
            self.moved_to.push((target, speed));
            true
        }

        fn navigation_done(&self) -> bool {
            self.nav_done
        }

        fn stop_navigation(&mut self) {}

        fn look_at(&mut self, target: Vec3) {
            self.looked_at.push(target);
        }

        fn nearest_visible_player(&self) -> Option<Vec3> {
            self.player
        }

        fn random_land_pos(&mut self, _max_xz: i32, _max_y: i32) -> Option<Vec3> {
            if self.stroll.is_empty() {
                None
            } else {
                Some(self.stroll.remove(0))
            }
        }
    }

    fn leaf<B: Behavior + 'static>(b: B) -> Box<dyn BehaviorControl> {
        Box::new(Leaf::new(b))
    }

    /// The universal CORE + IDLE scaffold: move/look sinks in CORE, a run-one
    /// gate of look-at-player / stroll in IDLE.
    fn scaffold_brain() -> Brain {
        let mut brain = Brain::new();
        brain.add_sensor(Box::new(NearestPlayerSensor));
        brain.add_activity(
            Activity::CORE,
            vec![
                (0, leaf(LookAtTargetSink::default())),
                (1, leaf(MoveToTargetSink::new())),
            ],
            Vec::new(),
            Vec::new(),
        );
        brain.add_activity(
            Activity::IDLE,
            vec![(
                5,
                Box::new(GateBehavior::run_one(
                    "idle_gate",
                    vec![
                        leaf(SetPlayerLookTarget::new(8.0)),
                        leaf(RandomStroll::new(1.0)),
                    ],
                )),
            )],
            Vec::new(),
            Vec::new(),
        );
        brain
    }

    #[test]
    fn stroll_and_move_sink_hand_off_through_walk_target_memory() {
        let mut brain = scaffold_brain();
        let mut mob = TestMob::new();
        let dest = Vec3::new(6.0, 0.0, 0.0);
        mob.stroll.push(dest);

        // Tick 1: no walk target yet, so the IDLE gate runs RandomStroll, which
        // writes WALK_TARGET. MoveToTargetSink cannot start (target absent).
        brain.tick(&mut mob);
        assert!(brain.memories().has_value(MemoryModuleType::WALK_TARGET));
        assert!(mob.moved_to.is_empty(), "move must not be issued yet");

        // Tick 2: MoveToTargetSink now sees the target and commands navigation.
        mob.time += 1;
        brain.tick(&mut mob);
        assert_eq!(mob.moved_to.len(), 1);
        assert_eq!(mob.moved_to[0].0, dest);
        assert_eq!(mob.moved_to[0].1, 1.0);

        // Arrive: MoveToTargetSink stops and clears WALK_TARGET, freeing the
        // loop to stroll again. This is the whole architecture in one assertion.
        mob.pos = dest;
        mob.time += 1;
        brain.tick(&mut mob);
        assert!(!brain.memories().has_value(MemoryModuleType::WALK_TARGET));
    }

    #[test]
    fn run_one_gate_picks_look_over_stroll_when_a_player_is_near() {
        let mut brain = scaffold_brain();
        let mut mob = TestMob::new();
        mob.player = Some(Vec3::new(2.0, 0.0, 0.0));
        mob.stroll.push(Vec3::new(6.0, 0.0, 0.0));

        brain.tick(&mut mob);

        // The gate started SetPlayerLookTarget (first child that accepts) and
        // stopped there — RandomStroll never ran, so no walk target was set.
        assert!(brain.memories().has_value(MemoryModuleType::LOOK_TARGET));
        assert!(!brain.memories().has_value(MemoryModuleType::WALK_TARGET));
    }

    #[test]
    fn run_one_gate_falls_through_to_stroll_when_no_player() {
        let mut brain = scaffold_brain();
        let mut mob = TestMob::new();
        mob.player = None;
        mob.stroll.push(Vec3::new(6.0, 0.0, 0.0));

        brain.tick(&mut mob);

        // First child (look-at-player) rejected; the gate fell through to stroll.
        assert!(!brain.memories().has_value(MemoryModuleType::LOOK_TARGET));
        assert!(brain.memories().has_value(MemoryModuleType::WALK_TARGET));
    }

    #[test]
    fn look_sink_turns_the_head_toward_the_player() {
        let mut brain = scaffold_brain();
        let mut mob = TestMob::new();
        let player = Vec3::new(3.0, 1.0, 0.0);
        mob.player = Some(player);

        // Tick 1: sensor writes NEAREST_VISIBLE_PLAYER, gate sets LOOK_TARGET.
        brain.tick(&mut mob);
        // Tick 2: LookAtTargetSink (CORE) consumes LOOK_TARGET and looks.
        mob.time += 1;
        brain.tick(&mut mob);

        assert!(mob.looked_at.contains(&player));
    }

    #[test]
    fn activity_switches_to_first_valid_when_its_memory_requirement_is_met() {
        let mut brain = Brain::new();
        brain.register_memory(MemoryModuleType::ATTACK_TARGET);
        // FIGHT requires an attack target; IDLE is unconditional.
        brain.add_activity(
            Activity::FIGHT,
            Vec::new(),
            vec![(MemoryModuleType::ATTACK_TARGET, MemoryStatus::ValuePresent)],
            Vec::new(),
        );
        brain.add_activity(Activity::IDLE, Vec::new(), Vec::new(), Vec::new());

        // No target: FIGHT's requirement fails, so IDLE stays active.
        brain.set_active_activity_to_first_valid(&[Activity::FIGHT, Activity::IDLE]);
        assert!(brain.is_active(Activity::IDLE));
        assert!(!brain.is_active(Activity::FIGHT));

        // Acquire a target: now FIGHT wins and IDLE is dropped.
        brain
            .memories_mut()
            .set(MemoryModuleType::ATTACK_TARGET, MemoryValue::Entity(42));
        brain.set_active_activity_to_first_valid(&[Activity::FIGHT, Activity::IDLE]);
        assert!(brain.is_active(Activity::FIGHT));
        assert!(!brain.is_active(Activity::IDLE));
        // CORE is always active regardless.
        assert!(brain.is_active(Activity::CORE));

        // Lose the target: fall back to IDLE.
        brain.memories_mut().erase(MemoryModuleType::ATTACK_TARGET);
        brain.set_active_activity_if_possible(Activity::FIGHT);
        assert!(brain.is_active(Activity::IDLE));
        assert!(!brain.is_active(Activity::FIGHT));
    }

    /// [`Brain::add_activity_any_of`]'s whole reason to exist: an activity
    /// eligible when **either** of two memories holds, which
    /// [`Brain::add_activity`]'s all-must-hold rule cannot express. Checked
    /// in both directions (only A, only B, neither) so this cannot be
    /// satisfied by an accidental `all()` that happens to pass on the one
    /// case tried.
    #[test]
    fn add_activity_any_of_is_eligible_when_either_condition_holds() {
        let mut brain = Brain::new();
        brain.register_memory(MemoryModuleType::HURT_BY);
        brain.register_memory(MemoryModuleType::NEAREST_HOSTILE);
        brain.add_activity_any_of(
            Activity::PANIC,
            Vec::new(),
            vec![
                (MemoryModuleType::HURT_BY, MemoryStatus::ValuePresent),
                (MemoryModuleType::NEAREST_HOSTILE, MemoryStatus::ValuePresent),
            ],
            Vec::new(),
        );
        brain.add_activity(Activity::IDLE, Vec::new(), Vec::new(), Vec::new());

        // Neither memory set: PANIC is not eligible, IDLE remains active.
        brain.set_active_activity_to_first_valid(&[Activity::PANIC, Activity::IDLE]);
        assert!(brain.is_active(Activity::IDLE));

        // Only HURT_BY: eligible.
        brain
            .memories_mut()
            .set(MemoryModuleType::HURT_BY, MemoryValue::Pos(lodestone_model::Vec3::default()));
        brain.set_active_activity_to_first_valid(&[Activity::PANIC, Activity::IDLE]);
        assert!(brain.is_active(Activity::PANIC), "HURT_BY alone must be enough");

        // Back to neither: falls out of PANIC (not sticky).
        brain.memories_mut().erase(MemoryModuleType::HURT_BY);
        brain.set_active_activity(Activity::IDLE);
        assert!(!brain.activity_requirements_are_met(Activity::PANIC));

        // Only NEAREST_HOSTILE: eligible too, proving it is a real OR and not
        // just "the first condition in the list happens to work".
        brain
            .memories_mut()
            .set(MemoryModuleType::NEAREST_HOSTILE, MemoryValue::Entity(7));
        brain.set_active_activity_to_first_valid(&[Activity::PANIC, Activity::IDLE]);
        assert!(brain.is_active(Activity::PANIC), "NEAREST_HOSTILE alone must be enough");
    }

    #[test]
    fn stopping_an_activity_erases_its_scoped_memories() {
        let mut brain = Brain::new();
        brain.register_memory(MemoryModuleType::ATTACK_TARGET);
        brain.add_activity(
            Activity::FIGHT,
            Vec::new(),
            vec![(MemoryModuleType::ATTACK_TARGET, MemoryStatus::ValuePresent)],
            vec![MemoryModuleType::ATTACK_TARGET],
        );
        brain.add_activity(Activity::IDLE, Vec::new(), Vec::new(), Vec::new());

        brain
            .memories_mut()
            .set(MemoryModuleType::ATTACK_TARGET, MemoryValue::Entity(7));
        brain.set_active_activity_if_possible(Activity::FIGHT);
        assert!(brain.is_active(Activity::FIGHT));

        // Switching away from FIGHT erases its scoped ATTACK_TARGET memory.
        brain.set_active_activity(Activity::IDLE);
        assert!(!brain.memories().has_value(MemoryModuleType::ATTACK_TARGET));
    }

    #[test]
    fn schedule_selects_activity_by_day_time_every_twenty_ticks() {
        let mut brain = Brain::new();
        brain.add_activity(Activity::IDLE, Vec::new(), Vec::new(), Vec::new());
        brain.add_activity(Activity::WORK, Vec::new(), Vec::new(), Vec::new());
        brain.add_activity(Activity::REST, Vec::new(), Vec::new(), Vec::new());
        brain.set_schedule(vec![
            (0, Activity::IDLE),
            (2000, Activity::WORK),
            (12000, Activity::REST),
        ]);

        // Daytime -> WORK, but only after the 20-tick gate elapses.
        brain.update_activity_from_schedule(5000, 1000);
        assert!(brain.is_active(Activity::WORK));

        // Same 20-tick window: no re-evaluation even though day time changed.
        brain.update_activity_from_schedule(13000, 1010);
        assert!(brain.is_active(Activity::WORK));

        // Next window: night -> REST.
        brain.update_activity_from_schedule(13000, 1030);
        assert!(brain.is_active(Activity::REST));
    }

    #[test]
    fn weighted_shuffle_biases_toward_higher_weight() {
        // A run-one gate whose two children both accept; SHUFFLED order with a
        // controlled RNG must let weight decide which starts.
        let mut brain = Brain::new();
        brain.add_activity(
            Activity::IDLE,
            vec![(
                1,
                Box::new(GateBehavior::new(
                    "gate",
                    Vec::new(),
                    Vec::new(),
                    OrderPolicy::Shuffled,
                    RunningPolicy::RunOne,
                    vec![
                        (leaf(RandomStroll::new(1.0)), 1),
                        (leaf(RandomStroll::new(2.0)), 20),
                    ],
                )),
            )],
            Vec::new(),
            Vec::new(),
        );
        let mut mob = TestMob::new();
        // Equal raw randoms: the weight-20 child gets key -r^(1/20) ~ closer to
        // -1 than the weight-1 child's -r^1, so it sorts first and starts. It
        // strolls at speed 2.0.
        mob.floats = vec![0.5, 0.5];
        mob.stroll.push(Vec3::new(4.0, 0.0, 0.0));
        brain.tick(&mut mob);
        match brain.memories().get(MemoryModuleType::WALK_TARGET) {
            Some(MemoryValue::WalkTarget(wt)) => assert_eq!(wt.speed, 2.0),
            other => panic!("expected walk target from weighted child, got {other:?}"),
        }
    }
}
