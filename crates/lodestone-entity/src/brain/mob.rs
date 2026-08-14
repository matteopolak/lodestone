//! The body seam a brain's behaviours drive.
//!
//! Analogous to [`MobController`](crate::ai::MobController) for the goal system:
//! [`BrainMob`] is the narrow set of *body* operations behaviours need, keeping
//! the brain free of world and physics. Coordination between behaviours happens
//! through [`Memories`](super::memory::Memories), not through this trait — the
//! trait is only for things the brain cannot express as memory (rolling dice,
//! reading the clock, commanding the real navigator, perceiving the world).

use lodestone_model::Vec3;

/// The mob-facing operations a brain behaviour or sensor may perform.
///
/// # This trait deliberately overlaps [`MobController`], and that has a cost
///
/// `NavigatingMob` implements **both**, and the two declare
/// same-named methods: `position`, `next_i32`, `next_f32`, `in_water`, `move_to`,
/// `navigation_done`, `stop_navigation`, `look_at`. On a type implementing both,
/// every call to one of those is `E0034 multiple applicable items in scope` and
/// must be spelled `MobController::in_water(self)` or
/// `BrainMob::move_to(self, …)`. Three call sites in `navigating_mob.rs`'s tests
/// pay that tax today.
///
/// **Worse, the two `move_to`s differ in float width** — `MobController`'s speed is
/// `f64`, this one's is `f32` — so a disambiguation that picks the wrong trait
/// changes the literal's type rather than failing to compile. The split is
/// pre-existing and faithful (vanilla's `WalkTarget.speedModifier` is a `float`
/// while the goal system's speeds are doubles), but it means the ambiguity cannot
/// be resolved by inference and never will be.
///
/// **Making this trait `BrainMob: MobController` would remove the ambiguity
/// permanently, and was considered and rejected.** It would force every
/// implementor — including the brain's own hermetic `TestMob` — to supply all ~35
/// `MobController` methods, most of which no behaviour ever calls; and it would
/// make the Brain system *depend on* the goal system's seam rather than sit beside
/// it, which is a layering inversion for two architectures vanilla treats as
/// peers. Revisit if a third implementor appears, or if the disambiguation tax
/// spreads beyond test code.
///
/// [`MobController`]: crate::ai::MobController
pub trait BrainMob {
    /// A uniform random `i32` in `[0, bound)` (vanilla's `random.nextInt`).
    fn next_i32(&mut self, bound: i32) -> i32;

    /// A uniform random `f32` in `[0, 1)`.
    fn next_f32(&mut self) -> f32;

    /// The current game time in ticks. Behaviour timeouts and cooldowns are
    /// measured against this.
    fn game_time(&self) -> i64;

    /// The mob's current position.
    fn position(&self) -> Vec3;

    /// Whether the mob is currently in water (gates swim/stroll variants).
    fn in_water(&self) -> bool {
        false
    }

    /// Commands navigation toward `target` at `speed`. Returns whether a path
    /// was found (vanilla's `navigation.moveTo`).
    fn move_to(&mut self, target: Vec3, speed: f32) -> bool;

    /// Whether navigation has finished or has no path.
    fn navigation_done(&self) -> bool;

    /// Whether navigation is stuck (used to set a retry cooldown).
    fn navigation_stuck(&self) -> bool {
        false
    }

    /// Stops navigation.
    fn stop_navigation(&mut self);

    /// Points the look control at a world position.
    fn look_at(&mut self, target: Vec3);

    /// The nearest visible player's position, if any is within perception
    /// range. Feeds the nearest-player sensor.
    fn nearest_visible_player(&self) -> Option<Vec3> {
        None
    }

    /// A candidate land wander destination within the given block radii
    /// (vanilla's `LandRandomPos.getPos`). `None` means none was found.
    fn random_land_pos(&mut self, max_xz: i32, max_y: i32) -> Option<Vec3>;
}
