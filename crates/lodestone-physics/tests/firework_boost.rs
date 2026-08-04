//! The elytra firework-rocket glide boost — issue #206.
//!
//! `FireworkRocketEntity.tick`'s attached-to-a-glider branch
//! (`FireworkRocketEntity.java:122-137`):
//!
//! ```text
//! lookAngle = attachedToEntity.getLookAngle();
//! movement = attachedToEntity.getDeltaMovement();
//! attachedToEntity.setDeltaMovement(movement.add(
//!     lookAngle.x * 0.1 + (lookAngle.x * 1.5 - movement.x) * 0.5,
//!     lookAngle.y * 0.1 + (lookAngle.y * 1.5 - movement.y) * 0.5,
//!     lookAngle.z * 0.1 + (lookAngle.z * 1.5 - movement.z) * 0.5
//! ));
//! ```
//!
//! All in `double`; `apply_firework_boost` is scoped to exactly this line —
//! see its doc for what triggering it (spawning/tracking the rocket) is not
//! this crate's job to model.

use lodestone_physics::{PlayerState, Vec3d, apply_firework_boost};

#[test]
fn from_rest_facing_due_south_level_the_boost_is_the_hand_derived_value() {
    // yaw = 0 (south), pitch = 0: `calculate_view_vector` collapses to exactly
    // `(0, 0, 1)` (sin(0)=0, cos(0)=1 — a trig identity, not a value read from
    // this crate). With `movement = (0,0,0)`:
    //   vz' = 0 + (1*0.1 + (1*1.5 - 0)*0.5) = 0.1 + 0.75 = 0.85
    // and x/y stay exactly zero since `lookAngle.x == lookAngle.y == 0`.
    let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    s.pitch = 0.0;
    s.velocity = Vec3d::ZERO;

    apply_firework_boost(&mut s);

    assert_eq!(s.velocity.x, 0.0);
    assert_eq!(s.velocity.y, 0.0);
    assert!(
        (s.velocity.z - 0.85).abs() < 1e-12,
        "expected exactly 0.85, got {}",
        s.velocity.z
    );
}

#[test]
fn the_existing_velocity_term_is_exercised_not_just_the_look_term() {
    // Same look direction, but with a pre-existing z velocity of 0.5:
    //   vz' = 0.5 + (1*0.1 + (1.5 - 0.5)*0.5) = 0.5 + (0.1 + 0.5) = 1.1
    // This is the case a zero-velocity-only test cannot distinguish from a
    // formula that dropped the `(lookAngle*1.5 - movement)*0.5` term's
    // dependence on `movement`.
    let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    s.pitch = 0.0;
    s.velocity = Vec3d::new(0.0, 0.0, 0.5);

    apply_firework_boost(&mut s);

    assert!(
        (s.velocity.z - 1.1).abs() < 1e-12,
        "expected exactly 1.1, got {}",
        s.velocity.z
    );
}

#[test]
fn repeated_boosts_converge_rather_than_diverge_or_runaway() {
    // Sanity/control: the update is a contraction toward `1.5 * lookAngle`
    // (`m' = 0.5*m + 0.85*look`, fixed point `m* = 1.7*look`), so repeated
    // application from rest must climb monotonically and never overshoot past
    // the fixed point nor blow up — a wrong sign or a missing `0.5` factor
    // would visibly runaway or oscillate here.
    let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    s.pitch = 0.0;
    s.velocity = Vec3d::ZERO;

    let mut prev = 0.0f64;
    for _ in 0..50 {
        apply_firework_boost(&mut s);
        assert!(
            s.velocity.z > prev - 1e-12,
            "must climb monotonically toward the fixed point, got {} after {}",
            s.velocity.z,
            prev
        );
        assert!(
            s.velocity.z < 1.7 + 1e-9,
            "must not overshoot the fixed point 1.7, got {}",
            s.velocity.z
        );
        prev = s.velocity.z;
    }
    assert!(
        (s.velocity.z - 1.7).abs() < 1e-6,
        "must have converged close to the fixed point after 50 iterations, got {}",
        s.velocity.z
    );
}
