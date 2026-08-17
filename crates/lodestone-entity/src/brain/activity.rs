//! Activities: the coarse behavioural modes a brain switches between.
//!
//! A brain is always running its **core** activities (usually just
//! [`Activity::CORE`]) plus at most **one** non-core activity — `IDLE`, `FIGHT`,
//! `PANIC`, and so on. Switching the non-core activity is how a brain changes
//! gears: entering `FIGHT` when an attack target appears, `PANIC` when hurt.
//! Only behaviours registered under a currently-active activity are eligible to
//! run, so the activity acts as a gate over whole behaviour sets at once.
//!
//! Faithful to vanilla `Activity`. The constants here are the common vanilla
//! activities; a version crate may add more with [`Activity::new`].

/// A version-free activity key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Activity(&'static str);

impl Activity {
    /// Mints an activity key from a stable name.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The stable name of this activity.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.0
    }

    /// Always-on behaviours (look/move sinks, swimming). Registered as core.
    pub const CORE: Self = Self("core");
    /// The default resting/wandering activity.
    pub const IDLE: Self = Self("idle");
    /// Working (villager professions).
    pub const WORK: Self = Self("work");
    /// Playing (baby villagers).
    pub const PLAY: Self = Self("play");
    /// Resting (sleeping at night).
    pub const REST: Self = Self("rest");
    /// Gathering at a meeting point.
    pub const MEET: Self = Self("meet");
    /// Fleeing from a threat.
    pub const PANIC: Self = Self("panic");
    /// Actively fighting a target.
    pub const FIGHT: Self = Self("fight");
    /// Avoiding a specific entity.
    pub const AVOID: Self = Self("avoid");
    /// Swimming (aquatic mobs).
    pub const SWIM: Self = Self("swim");
    /// Preparing, then charging, a ram attack (goat).
    pub const RAM: Self = Self("ram");
    /// Chasing and eating tongue-attack prey (frog) — vanilla `Activity.TONGUE`.
    pub const TONGUE: Self = Self("tongue");
}
