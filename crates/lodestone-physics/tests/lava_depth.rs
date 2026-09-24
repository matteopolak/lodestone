//! Vanilla's own in-lava travel step's shallow-vs-deep branch.
//!
//! # What this file is for, and what it is not
//!
//! The bit-exactness evidence lives in `tests/golden.rs`: `lava_sink` (deep,
//! **unchanged** — the regression control that proves this port is additive)
//! and `lava_shallow` (new) are both replayed from the independent Python
//! oracle in `gen_golden.py` with no tolerance.
//!
//! This file carries what a smooth trace cannot show on its own:
//!
//! 1. **A pure control.** `shallow_vs_deep_is_the_only_difference` runs the
//!    *same* starting velocity, input and world through [`tick_lava`] twice,
//!    varying only [`FluidState::lava_height`] — the one input the branch
//!    actually reads. The two heights (a shin-deep puddle vs. a lava ocean)
//!    produce velocities from two *different formulas*, not one formula fed
//!    two numbers, and the divergence is attributable to `lava_height` alone
//!    because nothing else differs between the two runs.
//! 2. **A hand-derived expectation for each arm**, computed here from the jar's
//!    formula directly (`multiply(0.5, 0.8, 0.5)` + the falling-adjustment vs.
//!    a flat `scale(0.5)`), not by calling the crate's own private helpers —
//!    the same discipline `edge_back_off.rs` uses for its primitive-level
//!    checks.
//! 3. **The predicate's boundary.** Vanilla's own "is in shallow fluid" check
//!    is its own fluid-height accessor `<= ` its own fluid-jump-threshold
//!    accessor — `<=`, not `<`. `predicate_boundary_is_inclusive`
//!    checks a lava height exactly at the standing threshold (`0.4`) takes the
//!    shallow arm, and a hair above it takes the deep arm.
//!
//! **On `fall_distance`:** it deliberately plays no role in either test here.
//! Reading vanilla's own in-fluid travel / in-lava travel / falling-adjusted
//! fluid movement steps directly (not a summary) shows none of the three
//! references the fall-distance field
//! — the predicate and both arms are driven entirely by
//! [`FluidState::lava_height`] and [`PlayerState::velocity`]/`sprinting`. So,
//! unlike the `fall_distance` accumulation work elsewhere in this crate, this
//! branch was not silently gated by `fall_distance` being permanently `0.0`
//! before today: it was simply never ported, in both the shallow and deep
//! directions, and lava_sink's presence-only world only ever reached the deep
//! arm (a coarse `is_lava` cell reads as full height `1.0`, never `<= 0.4`).

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::Aabb;
use lodestone_physics::{FluidState, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick_lava};

/// An empty world: no collision, no fluid current, no coarse fluid presence.
/// `tick_lava` reads the shallow/deep predicate from the `FluidState` argument
/// passed in, not by re-deriving it from the world, so this is deliberately
/// inert — the world's only job here is to not get in the way.
struct EmptyWorld;

impl CollisionView for EmptyWorld {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

/// A standing-pose player, falling (`velocity.y < 0`), with no horizontal
/// input — isolates the branch's Y-axis behaviour, which is where deep and
/// shallow actually differ (both scale X/Z by the same `0.5`).
fn falling_state() -> PlayerState {
    let mut s = PlayerState::at(Vec3d::new(0.5, 64.0, 0.5), 0.0);
    s.velocity = Vec3d::new(0.0, -0.1, 0.0);
    s
}

#[test]
fn shallow_vs_deep_is_the_only_difference() {
    let world = EmptyWorld;
    let profile = PhysicsProfile::mc_1_21();

    // 0.2: a shin-deep puddle, <= the 0.4 jump threshold => isInShallowFluid(LAVA).
    let shallow_fluid = FluidState {
        lava_height: 0.2,
        ..FluidState::NONE
    };
    // 1.0: fully submerged, far above the threshold => the deep arm.
    let deep_fluid = FluidState {
        lava_height: 1.0,
        ..FluidState::NONE
    };

    let mut shallow_state = falling_state();
    tick_lava(
        &mut shallow_state,
        MovementInput::NONE,
        &shallow_fluid,
        &world,
        &profile,
    );

    let mut deep_state = falling_state();
    tick_lava(
        &mut deep_state,
        MovementInput::NONE,
        &deep_fluid,
        &world,
        &profile,
    );

    assert_ne!(
        shallow_state.velocity.y, deep_state.velocity.y,
        "branch must be attributable to lava_height alone: shallow {:?} vs deep {:?}",
        shallow_state.velocity, deep_state.velocity
    );

    // Deep: a flat `scale(0.5)`, then the shared `-baseGravity/4` term.
    let gravity = f64::from(profile.gravity);
    let deep_expected_y = -0.1 * 0.5 + (-gravity / 4.0);
    assert!(
        (deep_state.velocity.y - deep_expected_y).abs() < 1e-12,
        "deep: got {} expected {}",
        deep_state.velocity.y,
        deep_expected_y
    );

    // Shallow: `multiply(0.5, 0.8, 0.5)`, then vanilla's own falling-adjusted
    // fluid movement step's `movement.y - baseGravity/16` arm (the `-0.003`
    // slow-sink clamp does not fire here — `|movement.y - baseGravity/16|` is
    // far outside its `0.003` window), then the same shared `-baseGravity/4`
    // term.
    let shallow_movement_y = -0.1 * f64::from(0.8f32);
    let shallow_expected_y = (shallow_movement_y - gravity / 16.0) + (-gravity / 4.0);
    assert!(
        (shallow_state.velocity.y - shallow_expected_y).abs() < 1e-12,
        "shallow: got {} expected {}",
        shallow_state.velocity.y,
        shallow_expected_y
    );

    // The one thing the two arms share: X/Z both scale by a flat 0.5 (deep's
    // `scale(0.5)` and shallow's `multiply(0.5, 0.8, 0.5)` agree on X and Z).
    // A divergence limited to Y is exactly what the branch predicts.
    assert_eq!(shallow_state.velocity.x, deep_state.velocity.x);
    assert_eq!(shallow_state.velocity.z, deep_state.velocity.z);
}

#[test]
fn predicate_boundary_is_inclusive() {
    // Vanilla's own "is in shallow fluid" check: its own fluid-height
    // accessor <= its own fluid-jump-threshold accessor. Standing eye height
    // (1.62) keeps the threshold at 0.4 (vanilla's own jump-threshold formula:
    // eyeHeight < 0.4 ? 0.0 : 0.4). At exactly 0.4 the branch must still be
    // shallow; a hair above it must be deep.
    let world = EmptyWorld;
    let profile = PhysicsProfile::mc_1_21();

    let at_threshold = FluidState {
        lava_height: 0.4,
        ..FluidState::NONE
    };
    let just_above = FluidState {
        lava_height: 0.400_000_1,
        ..FluidState::NONE
    };

    let mut at_state = falling_state();
    tick_lava(
        &mut at_state,
        MovementInput::NONE,
        &at_threshold,
        &world,
        &profile,
    );

    let mut above_state = falling_state();
    tick_lava(
        &mut above_state,
        MovementInput::NONE,
        &just_above,
        &world,
        &profile,
    );

    assert_ne!(
        at_state.velocity.y, above_state.velocity.y,
        "0.4 must take the shallow arm and 0.4000001 the deep arm"
    );

    let gravity = f64::from(profile.gravity);
    let deep_expected_y = -0.1 * 0.5 + (-gravity / 4.0);
    assert!(
        (above_state.velocity.y - deep_expected_y).abs() < 1e-12,
        "just above the threshold must match the deep formula"
    );

    let shallow_movement_y = -0.1 * f64::from(0.8f32);
    let shallow_expected_y = (shallow_movement_y - gravity / 16.0) + (-gravity / 4.0);
    assert!(
        (at_state.velocity.y - shallow_expected_y).abs() < 1e-12,
        "exactly at the threshold must match the shallow formula"
    );
}
