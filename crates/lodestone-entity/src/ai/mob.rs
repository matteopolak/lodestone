//! The interface goals drive.
//!
//! Vanilla goals operate on a `Mob`, reaching into its navigation, look control,
//! jump control and random source. Reproducing the whole `Mob` here would drag
//! in the world and physics; instead [`MobController`] is a narrow seam of the
//! *intents* goals actually express (move toward a point, look at a target,
//! jump, perceive the nearest player). A host wires these to the real navigator,
//! physics and world. This keeps the AI module about **scheduler semantics** —
//! which the design brief calls out as the thing that matters — rather than
//! about re-deriving movement.

use lodestone_model::Vec3;

/// The mob-facing operations a [`Goal`](crate::ai::Goal) may perform.
///
/// All methods take `&mut self` because goals both observe and command the mob;
/// a host implementation typically holds the entity state, a
/// [`PathNavigator`](crate::pathfinding::PathNavigator) and an RNG.
pub trait MobController {
    /// A uniform random `f32` in `[0, 1)` (vanilla's `random.nextFloat`).
    fn next_f32(&mut self) -> f32;

    /// A uniform random `i32` in `[0, bound)` (vanilla's `random.nextInt`).
    fn next_i32(&mut self, bound: i32) -> i32;

    /// A uniform random `f64` in `[0, 1)`.
    fn next_f64(&mut self) -> f64;

    /// The mob's current position.
    fn position(&self) -> Vec3;

    /// Whether the mob is in water.
    fn in_water(&self) -> bool {
        false
    }

    /// Whether the mob is in lava.
    fn in_lava(&self) -> bool {
        false
    }

    /// Ticks the mob has spent taking no deliberate action (vanilla's
    /// `getNoActionTime`), used by strolling to yield when idle-throttled.
    fn no_action_time(&self) -> i32 {
        0
    }

    /// Commands the navigation to move toward `target` at `speed`. Returns
    /// whether a path was found (vanilla's `navigation.moveTo`).
    fn move_to(&mut self, target: Vec3, speed: f64) -> bool;

    /// Whether the navigation has finished or has no path.
    fn navigation_done(&self) -> bool;

    /// Stops the navigation.
    fn stop_navigation(&mut self);

    /// Requests the jump control to jump this tick.
    fn set_jumping(&mut self, jumping: bool);

    /// Points the look control at a world position.
    fn look_at(&mut self, target: Vec3);

    /// Sets the desired look direction from a horizontal offset (used by the
    /// random-look goal).
    fn look_toward(&mut self, dx: f64, dz: f64);

    /// The nearest player's position, if one is within perception range.
    fn nearest_player(&self) -> Option<Vec3> {
        None
    }

    /// A candidate wander destination (vanilla's `DefaultRandomPos.getPos`).
    /// Returning `None` means no valid spot was found this attempt.
    fn random_stroll_target(&mut self) -> Option<Vec3>;

    /// The current attack target's position, if the mob has one.
    fn attack_target(&self) -> Option<Vec3> {
        None
    }

    /// Sets (or clears, with `None`) the mob's attack target. Target-selection
    /// goals call this; movement goals read it back via [`attack_target`].
    ///
    /// [`attack_target`]: MobController::attack_target
    fn set_attack_target(&mut self, target: Option<Vec3>) {
        let _ = target;
    }

    /// The nearest position the mob considers an attackable target — the host
    /// applies the version/type-specific filter (hostility, follow range, line
    /// of sight). Drives `NearestAttackableTargetGoal`.
    fn find_nearest_target(&mut self) -> Option<Vec3> {
        None
    }

    /// The position of the entity that most recently damaged this mob, within
    /// the retaliation window. Drives `HurtByTargetGoal`.
    fn last_hurt_by(&self) -> Option<Vec3> {
        None
    }

    /// The position of a nearby entity currently tempting this mob (e.g. a
    /// player holding food). Drives `TemptGoal`.
    fn temptation(&self) -> Option<Vec3> {
        None
    }

    /// Whether this mob is a baby (`Age < 0` for animals). Gates
    /// `FollowParentGoal`.
    fn is_baby(&self) -> bool {
        false
    }

    /// The position of the nearest adult of the same kind, if one is in range.
    /// Drives `FollowParentGoal`.
    fn parent_position(&self) -> Option<Vec3> {
        None
    }

    /// Performs a melee attack against `target`.
    fn attack(&mut self, target: Vec3) {
        let _ = target;
    }

    /// A position of a nearby entity the mob wants to avoid, if any.
    fn avoid_threat(&self) -> Option<Vec3> {
        None
    }

    /// Whether the mob is currently panicking (e.g. was just hurt).
    fn is_panicking(&self) -> bool {
        false
    }
}

/// Squared horizontal+vertical distance between two points.
#[must_use]
pub fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    let d = a - b;
    d.x * d.x + d.y * d.y + d.z * d.z
}
