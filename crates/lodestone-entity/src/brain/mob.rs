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
