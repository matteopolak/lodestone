//! Attack knockback: the velocity impulse a landed melee hit applies to its
//! target.
//!
//! # Where this fits, and why it is a pure function
//!
//! `lodestone-entity/src/damage.rs`'s own module doc names this crate as the
//! intended home: *"Knockback impulse. `impl-physics` builds the knockback
//! velocity from the other side; this crate only decides whether a hit lands
//! and how much it hurts."* [`knockback_impulse`] is that half — the exact
//! low-level velocity mechanic, taking an already-resolved `power` scalar the
//! same way [`lodestone_entity::apply_reductions`] takes an already-resolved
//! [`lodestone_entity::Defenses`]. Composing that `power` from a weapon's
//! `ATTACK_KNOCKBACK` attribute, the Knockback enchantment and the sprint-hit
//! `+0.5F` bonus is the caller's job — this
//! module does not read attributes, items or enchantments, matching how
//! `apply_reductions` does not read an inventory.
//!
//! # Status as of writing: no caller exists yet
//!
//! Grepped across the whole workspace: **nothing calls this**, because nothing
//! upstream of it exists yet either. `lodestone-server`'s `ServerBound` enum
//! (`crates/lodestone-server/src/protocol.rs`) has no `Attack`/`Interact`
//! variant at all, so a connected player's melee attack packet is never
//! decoded into a damage event server-side — `SimMob::apply_damage`
//! (`crates/lodestone-server/src/mobs.rs`) is reached today only by AI-driven
//! `MeleeAttackGoal` hits (mob-on-mob) and by [`lodestone_entity::explosion`],
//! never by a player's own swing. Building that dispatch is a materially
//! larger, cross-crate change (`protocol.rs`'s `ServerBound` enum, the v770
//! serverbound decode, and `integrated.rs`'s routing) than this single
//! function, and those files were contended/in-flight at the time this was
//! written — see the combat census posted to issue #12. This function is
//! placed here, tested against the jar's own formula, so that whoever builds
//! that dispatch has a correct, ready-to-call primitive rather than a second
//! reason to invent one under time pressure.
//!
//! Contrast with [`crate::push`]'s soft crowd push (an
//! **additive**, always-on, both-directions nudge with no attack involved) and
//! with `lodestone_entity::explosion::{knockback_power, knockback_direction}`
//! (an explosion's radial scalar/direction, also currently uncalled — see that
//! module's own doc). All three are real, distinct vanilla mechanics; none
//! shares a formula with either of the others.

use crate::geometry::Vec3d;

/// Vanilla's knockback velocity mechanic, restricted to just that — the
/// caller has already resolved `power` (attribute + enchantment + sprint bonus,
/// pre-multiplied by nothing) and `knockback_resistance` (the target's
/// `minecraft:knockback_resistance` attribute value, `0.0..=1.0`).
///
/// `xd`/`zd` is the horizontal push *direction*, not a unit vector — vanilla
/// normalizes it here, not at the call site. For a melee attack this is the
/// **attacker's facing**, not the vector toward the target: see
/// [`attack_direction`]. For an explosion it would be the radial direction
/// (`lodestone_entity::explosion::knockback_direction`), though that call site
/// does not exist yet either.
///
/// `jitter` supplies vanilla's `random.nextDouble() - random.nextDouble()`
/// pairs for the degenerate-direction fallback: each call must return one such
/// difference for `x` and one for `z`. It is
/// looped exactly as vanilla loops, so a caller whose first jitter is itself
/// degenerate is asked again — see
/// [`knockback_loops_the_jitter_until_a_non_degenerate_direction_lands`]. Real
/// callers only ever hit this branch when the attacker and target occupy the
/// exact same horizontal position; ordinary callers with a real facing never
/// invoke `jitter` at all.
///
/// Returns the target's new velocity (vanilla mutates `deltaMovement` in
/// place; this crate's callers already thread velocity through as a value,
/// matching [`crate::entity::EntityMotion`]).
#[must_use]
pub fn knockback_impulse(
    velocity: Vec3d,
    on_ground: bool,
    power: f64,
    xd: f64,
    zd: f64,
    knockback_resistance: f64,
    mut jitter: impl FnMut() -> (f64, f64),
) -> Vec3d {
    let power = power * (1.0 - knockback_resistance);
    if power <= 0.0 {
        return velocity;
    }

    let mut dir = Vec3d::new(xd, 0.0, zd);
    while dir.normalize() == Vec3d::ZERO {
        let (dx, dz) = jitter();
        dir = Vec3d::new(dx * 0.01, 0.0, dz * 0.01);
    }

    let delta_vector = dir.normalize().scale(power);
    let y = if on_ground {
        (velocity.y / 2.0 + power).min(0.4)
    } else {
        velocity.y
    };
    Vec3d::new(
        velocity.x / 2.0 - delta_vector.x,
        y,
        velocity.z / 2.0 - delta_vector.z,
    )
}

/// The horizontal push direction a melee attack uses: the **attacker's
/// facing**, not the vector toward the target — vanilla derives it as
/// `sin(yRot * (Math.PI/180))`, `-cos(yRot * (Math.PI/180))` using its own
/// quantized trigonometry, where `yRot` is the attacker's body yaw at the
/// moment the hit landed. `yaw_degrees` is that same value.
///
/// This is a real, load-bearing detail worth calling out explicitly: standing
/// still and hitting a target behind you still knocks it away from *your*
/// front, not away from you-toward-it, even though the two usually look the
/// same because an attacker is usually facing the target.
#[must_use]
pub fn attack_direction(yaw_degrees: f32) -> (f64, f64) {
    // `(float) (Math.PI / 180.0)` — the same deg->rad cast-then-multiply
    // vanilla's own yaw conversions use (see `crate::player`'s
    // `real_x_rot`/`real_y_rot`), not `f32::to_radians`'s own constant, so this
    // widens to `f64` from the identical `f32` product vanilla computes before
    // its own quantized sin/cos widen it further.
    let deg_to_rad = (core::f64::consts::PI / 180.0) as f32;
    let rad = f64::from(yaw_degrees * deg_to_rad);
    (f64::from(crate::mth::sin(rad)), -f64::from(crate::mth::cos(rad)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No jitter needed for any test here: every direction below is already
    /// non-degenerate. A jitter closure that panics on call proves that.
    fn no_jitter() -> impl FnMut() -> (f64, f64) {
        || panic!("jitter must not be called for a non-degenerate direction")
    }

    /// Hand-derived from the jar formula, not from this function: airborne,
    /// facing dir (0, -1), power 1.5, starting velocity (1.0, -0.5, 2.0).
    /// `deltaVector = normalize(0,0,-1).scale(1.5) = (0, 0, -1.5)`.
    /// `y` stays `velocity.y` because `on_ground` is false.
    /// `x' = 1.0/2 - 0 = 0.5`, `z' = 2.0/2 - (-1.5) = 2.5`.
    #[test]
    fn airborne_knockback_keeps_vertical_velocity_and_halves_horizontal() {
        let out = knockback_impulse(
            Vec3d::new(1.0, -0.5, 2.0),
            false,
            1.5,
            0.0,
            -1.0,
            0.0,
            no_jitter(),
        );
        assert!((out.x - 0.5).abs() < 1e-9, "x = {}", out.x);
        assert!((out.y - -0.5).abs() < 1e-9, "y = {}", out.y);
        assert!((out.z - 2.5).abs() < 1e-9, "z = {}", out.z);
    }

    /// Hand-derived: grounded, at rest, power 1.0, direction (1, 0).
    /// `deltaVector = (1.0, 0, 0)`. `y' = min(0.4, 0/2 + 1.0) = 0.4` — the
    /// **magnitude** check: a formula that forgot the `min(0.4, ..)` cap would
    /// predict `y' = 1.0`, a materially different (and wrong, vanilla players
    /// do not rocket upward on every ground hit) number.
    /// `x' = 0/2 - 1.0 = -1.0`, `z' = 0/2 - 0 = 0.0`.
    #[test]
    fn grounded_knockback_caps_vertical_lift_at_zero_point_four() {
        let out = knockback_impulse(Vec3d::ZERO, true, 1.0, 1.0, 0.0, 0.0, no_jitter());
        assert!((out.x - -1.0).abs() < 1e-9, "x = {}", out.x);
        assert!((out.y - 0.4).abs() < 1e-9, "y = {}", out.y);
        assert!((out.z - 0.0).abs() < 1e-9, "z = {}", out.z);
    }

    /// Grounded but already moving upward fast enough that `deltaMovement.y/2
    /// + power` exceeds `0.4` still clamps to exactly `0.4`, not to the
    /// uncapped sum — the cap is a `min`, not a conditional bypass.
    #[test]
    fn grounded_knockback_cap_applies_even_with_existing_upward_velocity() {
        let out = knockback_impulse(
            Vec3d::new(0.0, 2.0, 0.0),
            true,
            1.0,
            1.0,
            0.0,
            0.0,
            no_jitter(),
        );
        assert!((out.y - 0.4).abs() < 1e-9, "y = {}", out.y);
    }

    /// Full knockback resistance (`1.0`) nullifies the impulse entirely —
    /// velocity comes back unchanged, matching
    /// `lodestone_entity::explosion`'s own resistance control
    /// (`knockback_power_scales_and_resists`), same shape for the melee
    /// formula.
    #[test]
    fn full_knockback_resistance_is_a_no_op() {
        let v = Vec3d::new(3.0, -1.0, -3.0);
        let out = knockback_impulse(v, false, 5.0, 0.0, -1.0, 1.0, no_jitter());
        assert_eq!(out, v, "full resistance must leave velocity untouched");
    }

    /// Partial resistance scales `power` before anything else runs — 50%
    /// resistance on `power = 2.0` must behave exactly like an unresisted
    /// `power = 1.0` call (same direction, same on/off-ground branch).
    #[test]
    fn partial_resistance_scales_power_linearly() {
        let resisted = knockback_impulse(Vec3d::ZERO, false, 2.0, 1.0, 0.0, 0.5, no_jitter());
        let equivalent = knockback_impulse(Vec3d::ZERO, false, 1.0, 1.0, 0.0, 0.0, no_jitter());
        assert_eq!(resisted, equivalent);
    }

    /// Zero or negative effective power is a no-op — vanilla's own
    /// `if (!(power <= 0.0))` guard, checked *after* the resistance multiply.
    #[test]
    fn zero_power_is_a_no_op() {
        let v = Vec3d::new(1.0, 1.0, 1.0);
        let out = knockback_impulse(v, true, 0.0, 1.0, 0.0, 0.0, no_jitter());
        assert_eq!(out, v);
    }

    /// The degenerate-direction fallback actually **loops**: the first jitter
    /// draw is itself degenerate (`(0.0, 0.0)`, scaled to `(0,0,0)`, still
    /// below the `1e-5` threshold), so a correct implementation must call
    /// `jitter` a second time rather than accepting the first draw
    /// unconditionally. This is the negative control that distinguishes a
    /// `while` from an `if`: an `if`-shaped bug would use the degenerate
    /// `(0,0,0)` direction, `normalize()` it to `ZERO`, and produce a
    /// `deltaVector` of `(0,0,0)` — i.e. it would look like `zero_power_is_a_no_op`
    /// above even though `power > 0.0`, silently dropping the hit's knockback.
    #[test]
    fn knockback_loops_the_jitter_until_a_non_degenerate_direction_lands() {
        let mut draws = vec![(0.0, 0.0), (1.0, 0.0)].into_iter();
        let out = knockback_impulse(Vec3d::ZERO, false, 1.0, 0.0, 0.0, 0.0, move || {
            draws.next().expect("jitter drawn more times than expected")
        });
        // Second draw is (1.0, 0.0) * 0.01 = (0.01, 0, 0), normalized to (1,0,0).
        assert!((out.x - -1.0).abs() < 1e-9, "x = {}", out.x);
        assert_eq!(out.y, 0.0);
        assert_eq!(out.z, 0.0);
    }

    /// [`attack_direction`] at yaw `0.0`, hand-derived: `sin(0) = 0`,
    /// `-cos(0) = -1`.
    #[test]
    fn attack_direction_at_yaw_zero_faces_negative_z() {
        let (x, z) = attack_direction(0.0);
        assert!(x.abs() < 1e-6, "x = {x}");
        assert!((z - -1.0).abs() < 1e-6, "z = {z}");
    }

    /// At yaw `90.0`, hand-derived: `sin(90°) = 1`, `-cos(90°) = 0`.
    #[test]
    fn attack_direction_at_yaw_ninety_faces_positive_x() {
        let (x, z) = attack_direction(90.0);
        assert!((x - 1.0).abs() < 1e-6, "x = {x}");
        assert!(z.abs() < 1e-6, "z = {z}");
    }

    /// At yaw `180.0`, hand-derived: `sin(180°) = 0`, `-cos(180°) = 1`.
    #[test]
    fn attack_direction_at_yaw_one_eighty_faces_positive_z() {
        let (x, z) = attack_direction(180.0);
        assert!(x.abs() < 1e-6, "x = {x}");
        assert!((z - 1.0).abs() < 1e-6, "z = {z}");
    }
}
