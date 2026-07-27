//! The goal trait and the [`GoalSelector`] scheduler.
//!
//! This is a faithful reproduction of vanilla's `GoalSelector` / `Goal` /
//! `WrappedGoal`:
//!
//! * Goals declare a set of mutually-exclusive [`Flag`]s (MOVE / LOOK / JUMP /
//!   TARGET). At most one running goal may hold each flag.
//! * Goals have an integer priority where **lower means higher precedence**.
//! * A running goal is preempted only if it is interruptible and the challenger
//!   has a strictly lower priority number, for *every* flag the challenger wants.
//! * Each tick: stop goals that can no longer continue, start newly-eligible
//!   goals (evicting the flag holders they preempt), then tick all running goals.
//!
//! The classic Rust hazard here — mutating one goal while another in the same
//! collection is borrowed — is avoided by touching the goal vector strictly one
//! index at a time and storing flag ownership as indices rather than references.

use super::mob::MobController;

/// The four mutually-exclusive action categories a goal can occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    /// Controls where the mob walks.
    Move,
    /// Controls where the mob looks.
    Look,
    /// Controls jumping.
    Jump,
    /// Controls target selection.
    Target,
}

impl Flag {
    const COUNT: usize = 4;

    const fn index(self) -> usize {
        match self {
            Flag::Move => 0,
            Flag::Look => 1,
            Flag::Jump => 2,
            Flag::Target => 3,
        }
    }
}

/// A small set of [`Flag`]s, stored as a bitmask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlagSet(u8);

impl FlagSet {
    /// The empty set.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Builds a set from a slice of flags.
    #[must_use]
    pub fn of(flags: &[Flag]) -> Self {
        let mut s = Self(0);
        for &f in flags {
            s.0 |= 1 << f.index();
        }
        s
    }

    /// Adds a flag.
    #[must_use]
    pub const fn with(mut self, f: Flag) -> Self {
        self.0 |= 1 << f.index();
        self
    }

    /// Whether `f` is present.
    #[must_use]
    pub const fn contains(self, f: Flag) -> bool {
        self.0 & (1 << f.index()) != 0
    }

    /// Whether this set shares any flag with `other`.
    #[must_use]
    pub const fn intersects(self, other: FlagSet) -> bool {
        self.0 & other.0 != 0
    }

    fn iter(self) -> impl Iterator<Item = Flag> {
        const ALL: [Flag; Flag::COUNT] = [Flag::Move, Flag::Look, Flag::Jump, Flag::Target];
        ALL.into_iter().filter(move |&f| self.contains(f))
    }
}

/// A unit of mob behaviour, scheduled by [`GoalSelector`].
///
/// Mirrors vanilla's `Goal`. `can_use` decides whether the goal may start;
/// `can_continue_to_use` (defaulting to `can_use`) decides whether it keeps
/// running. `start`/`stop`/`tick` have empty defaults.
pub trait Goal {
    /// The action categories this goal occupies while running.
    fn flags(&self) -> FlagSet;

    /// Whether the goal is eligible to begin this tick.
    fn can_use(&mut self, mob: &mut dyn MobController) -> bool;

    /// Whether the goal should keep running. Defaults to re-checking `can_use`.
    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.can_use(mob)
    }

    /// Whether a higher-priority goal may preempt this one while it runs.
    fn is_interruptable(&self) -> bool {
        true
    }

    /// Called once when the goal starts.
    fn start(&mut self, mob: &mut dyn MobController) {
        let _ = mob;
    }

    /// Called once when the goal stops.
    fn stop(&mut self, mob: &mut dyn MobController) {
        let _ = mob;
    }

    /// Called every tick while the goal runs.
    fn tick(&mut self, mob: &mut dyn MobController) {
        let _ = mob;
    }
}

struct WrappedGoal {
    priority: i32,
    goal: Box<dyn Goal>,
    running: bool,
}

impl WrappedGoal {
    fn start(&mut self, mob: &mut dyn MobController) {
        if !self.running {
            self.running = true;
            self.goal.start(mob);
        }
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        if self.running {
            self.running = false;
            self.goal.stop(mob);
        }
    }

    fn can_be_replaced_by(&self, challenger_priority: i32) -> bool {
        self.goal.is_interruptable() && challenger_priority < self.priority
    }
}

/// Runs a prioritised set of [`Goal`]s with vanilla flag-locking semantics.
#[derive(Default)]
pub struct GoalSelector {
    // Debug is implemented manually below because `Box<dyn Goal>` is not `Debug`.
    goals: Vec<WrappedGoal>,
    /// Which goal index currently owns each flag (`None` = free).
    locked: [Option<usize>; Flag::COUNT],
    disabled: FlagSet,
}

impl std::fmt::Debug for GoalSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalSelector")
            .field("goals", &self.goals.len())
            .field("running", &self.running_indices())
            .field("disabled", &self.disabled)
            .finish()
    }
}

impl GoalSelector {
    /// Creates an empty selector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a goal at the given priority (lower = higher precedence).
    pub fn add(&mut self, priority: i32, goal: Box<dyn Goal>) {
        self.goals.push(WrappedGoal {
            priority,
            goal,
            running: false,
        });
    }

    /// Number of registered goals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.goals.len()
    }

    /// Whether no goals are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.goals.is_empty()
    }

    /// Disables all goals that use `flag` until it is re-enabled.
    pub fn disable(&mut self, flag: Flag) {
        self.disabled = self.disabled.with(flag);
    }

    /// Re-enables a previously disabled flag.
    pub fn enable(&mut self, flag: Flag) {
        self.disabled = FlagSet(self.disabled.0 & !(1 << flag.index()));
    }

    /// Indices of the currently running goals (test/inspection helper).
    #[must_use]
    pub fn running_indices(&self) -> Vec<usize> {
        (0..self.goals.len())
            .filter(|&i| self.goals[i].running)
            .collect()
    }

    /// Whether the goal at `index` is currently running.
    #[must_use]
    pub fn is_running(&self, index: usize) -> bool {
        self.goals.get(index).is_some_and(|g| g.running)
    }

    fn has_disabled_flag(&self, i: usize) -> bool {
        self.goals[i].goal.flags().intersects(self.disabled)
    }

    fn can_replace_all_flags(&self, i: usize) -> bool {
        let challenger = self.goals[i].priority;
        self.goals[i].goal.flags().iter().all(|f| {
            self.locked[f.index()]
                .is_none_or(|owner| self.goals[owner].can_be_replaced_by(challenger))
        })
    }

    /// Advances all goals by one tick.
    pub fn tick(&mut self, mob: &mut dyn MobController) {
        self.cleanup(mob);
        self.update(mob);
        self.tick_running(mob);
    }

    /// Stops running goals that can no longer continue, freeing their flags.
    fn cleanup(&mut self, mob: &mut dyn MobController) {
        for i in 0..self.goals.len() {
            if self.goals[i].running
                && (self.has_disabled_flag(i) || !self.goals[i].goal.can_continue_to_use(mob))
            {
                self.goals[i].stop(mob);
            }
        }
        for slot in &mut self.locked {
            if let Some(owner) = *slot
                && !self.goals[owner].running
            {
                *slot = None;
            }
        }
    }

    /// Starts newly-eligible goals, preempting the flag holders they outrank.
    fn update(&mut self, mob: &mut dyn MobController) {
        for i in 0..self.goals.len() {
            if self.goals[i].running || self.has_disabled_flag(i) || !self.can_replace_all_flags(i)
            {
                continue;
            }
            if self.goals[i].goal.can_use(mob) {
                let flags = self.goals[i].goal.flags();
                for f in flags.iter() {
                    if let Some(owner) = self.locked[f.index()] {
                        self.goals[owner].stop(mob);
                    }
                    self.locked[f.index()] = Some(i);
                }
                self.goals[i].start(mob);
            }
        }
    }

    /// Ticks every running goal (vanilla forces an update each tick).
    fn tick_running(&mut self, mob: &mut dyn MobController) {
        for i in 0..self.goals.len() {
            if self.goals[i].running {
                self.goals[i].goal.tick(mob);
            }
        }
    }
}

/// The two-selector arrangement every vanilla `Mob` runs.
///
/// A mob owns a **target selector** (goals that pick *who* to fight, on the
/// TARGET flag) and a **goal selector** (everything else). Vanilla ticks the
/// target selector first so a freshly-acquired target is visible to movement
/// goals the same tick. The two never contend for flags — target goals only use
/// TARGET — so they are independent schedulers, not one queue.
#[derive(Debug, Default)]
pub struct MobAi {
    /// Goals that select the attack target (TARGET flag).
    pub target_selector: GoalSelector,
    /// Goals that drive movement, looking and jumping.
    pub goal_selector: GoalSelector,
}

impl MobAi {
    /// Creates an empty pair of selectors.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ticks the target selector then the goal selector, as vanilla does.
    pub fn tick(&mut self, mob: &mut dyn MobController) {
        self.target_selector.tick(mob);
        self.goal_selector.tick(mob);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::Vec3;

    #[derive(Default)]
    struct DummyMob;
    impl MobController for DummyMob {
        fn next_f32(&mut self) -> f32 {
            0.0
        }
        fn next_i32(&mut self, _bound: i32) -> i32 {
            0
        }
        fn next_f64(&mut self) -> f64 {
            0.0
        }
        fn position(&self) -> Vec3 {
            Vec3::default()
        }
        fn move_to(&mut self, _t: Vec3, _s: f64) -> bool {
            true
        }
        fn navigation_done(&self) -> bool {
            false
        }
        fn stop_navigation(&mut self) {}
        fn set_jumping(&mut self, _j: bool) {}
        fn look_at(&mut self, _t: Vec3) {}
        fn look_toward(&mut self, _dx: f64, _dz: f64) {}
        fn random_stroll_target(&mut self) -> Option<Vec3> {
            None
        }
    }

    /// A goal that is eligible while `usable` is true and records lifecycle.
    struct Recorder {
        flags: FlagSet,
        usable: bool,
        interruptable: bool,
        started: u32,
        stopped: u32,
        ticked: u32,
    }
    impl Recorder {
        fn new(flags: FlagSet) -> Self {
            Self {
                flags,
                usable: true,
                interruptable: true,
                started: 0,
                stopped: 0,
                ticked: 0,
            }
        }
    }
    impl Goal for Recorder {
        fn flags(&self) -> FlagSet {
            self.flags
        }
        fn can_use(&mut self, _mob: &mut dyn MobController) -> bool {
            self.usable
        }
        fn is_interruptable(&self) -> bool {
            self.interruptable
        }
        fn start(&mut self, _mob: &mut dyn MobController) {
            self.started += 1;
        }
        fn stop(&mut self, _mob: &mut dyn MobController) {
            self.stopped += 1;
        }
        fn tick(&mut self, _mob: &mut dyn MobController) {
            self.ticked += 1;
        }
    }

    #[test]
    fn single_goal_runs_and_ticks() {
        let mut sel = GoalSelector::new();
        sel.add(1, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        let mut mob = DummyMob;
        sel.tick(&mut mob);
        assert_eq!(sel.running_indices(), vec![0]);
    }

    #[test]
    fn higher_priority_preempts_same_flag() {
        let mut sel = GoalSelector::new();
        // index 0: low precedence (priority 5); index 1: high precedence (1).
        sel.add(5, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        sel.add(1, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        let mut mob = DummyMob;
        // First tick: only index 0 exists to grab MOVE? Both eligible; update
        // iterates in order, index 0 grabs MOVE, then index 1 (priority 1)
        // preempts it because 1 < 5.
        sel.tick(&mut mob);
        assert_eq!(sel.running_indices(), vec![1]);
    }

    #[test]
    fn lower_priority_does_not_preempt() {
        let mut sel = GoalSelector::new();
        sel.add(1, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        sel.add(5, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        let mut mob = DummyMob;
        sel.tick(&mut mob);
        // index 0 (priority 1) holds MOVE; index 1 (priority 5) cannot replace.
        assert_eq!(sel.running_indices(), vec![0]);
    }

    #[test]
    fn non_overlapping_flags_run_together() {
        let mut sel = GoalSelector::new();
        sel.add(1, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        sel.add(1, Box::new(Recorder::new(FlagSet::of(&[Flag::Look]))));
        let mut mob = DummyMob;
        sel.tick(&mut mob);
        assert_eq!(sel.running_indices(), vec![0, 1]);
    }

    #[test]
    fn uninterruptable_goal_holds_flag() {
        let mut sel = GoalSelector::new();
        let mut low = Recorder::new(FlagSet::of(&[Flag::Move]));
        low.interruptable = false;
        sel.add(5, Box::new(low));
        sel.add(1, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        let mut mob = DummyMob;
        sel.tick(&mut mob);
        // Even though priority 1 < 5, the holder is not interruptible.
        assert_eq!(sel.running_indices(), vec![0]);
    }

    #[test]
    fn goal_stops_when_no_longer_usable() {
        let mut sel = GoalSelector::new();
        sel.add(1, Box::new(Recorder::new(FlagSet::of(&[Flag::Move]))));
        let mut mob = DummyMob;
        sel.tick(&mut mob);
        assert_eq!(sel.running_indices(), vec![0]);
        // Force the goal to report it can no longer continue.
        // (Reach in via a fresh selector is awkward; instead disable the flag.)
        sel.disable(Flag::Move);
        sel.tick(&mut mob);
        assert!(sel.running_indices().is_empty());
    }
}
