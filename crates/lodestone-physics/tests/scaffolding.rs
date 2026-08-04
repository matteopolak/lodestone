//! Scaffolding's climb/descend behaviour vs. a ladder's — issue #210.
//!
//! Both blocks are `BlockTags.CLIMBABLE`, so `CollisionView::is_climbable` is
//! identical for them; the one difference vanilla codes is in
//! `LivingEntity.handleOnClimbable`'s sneak-to-hold clamp
//! (`LivingEntity.java:2693-2703`):
//!
//! ```text
//! yd = max(delta.y, -0.15);
//! if (yd < 0.0 && !inBlockState.is(SCAFFOLDING) && isSuppressingSlidingDownLadder())
//!     yd = 0.0;
//! ```
//!
//! On a ladder, sneaking while moving down clamps `yd` to `0.0` — you hang in
//! place. On scaffolding the extra conjunct is false, so the clamp never
//! engages and sneaking still descends, capped at the ordinary `-0.15`/tick.
//! These tests predict the two exact per-tick descent rates rather than just
//! their sign or relative order.

use lodestone_physics::{Aabb, CollisionView, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick};

/// `Mth.clamp(delta.y, -0.15F, 0.15F)`'s bound, widened `f32 -> f64` exactly as
/// `handle_on_climbable` does — `0.15` is not exact in `f32`, so the settled
/// descent rate is `-0.150000005960464...`, not the decimal literal `-0.15`.
const CLIMB_CAP: f64 = 0.15f32 as f64;

/// An infinite climbable shaft with no collision geometry at all — being
/// "climbable" and being "solid" are independent in vanilla (a ladder has a
/// thin collision plate, not blocking the column), and this crate's
/// `is_climbable` query is purely positional, so an empty-collision shaft
/// exercises exactly the clamp under test with nothing else in the way.
struct Shaft {
    scaffolding: bool,
}

impl CollisionView for Shaft {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}

    fn is_climbable(&self, _x: i32, _y: i32, _z: i32) -> bool {
        true
    }

    fn is_scaffolding(&self, _x: i32, _y: i32, _z: i32) -> bool {
        self.scaffolding
    }
}

fn sneaking() -> MovementInput {
    MovementInput {
        sneak: true,
        ..MovementInput::NONE
    }
}

/// Per-tick Y deltas over `ticks` ticks of sneaking in place inside `world`.
fn sneak_descent(world: &Shaft, ticks: usize) -> Vec<f64> {
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 200.0, 0.5), 0.0);
    let mut prev = s.position.y;
    let mut out = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        tick(&mut s, sneaking(), world, &profile);
        out.push(s.position.y - prev);
        prev = s.position.y;
    }
    out
}

#[test]
fn sneaking_on_a_ladder_holds_in_place_once_gravity_engages() {
    let ladder = Shaft { scaffolding: false };
    let deltas = sneak_descent(&ladder, 6);

    // Tick 0: velocity starts at exactly zero (nothing to clamp yet), so no
    // movement either way — not yet evidence of the hold.
    assert_eq!(deltas[0], 0.0, "no velocity to clamp on the very first tick");

    // From tick 1 on, gravity has produced a downward velocity every previous
    // tick, and the sneak-hold clamp zeroes it before each move: the position
    // must not move at all.
    for (i, &d) in deltas.iter().enumerate().skip(1) {
        assert_eq!(d, 0.0, "tick {i}: a ladder must hold a sneaking climber exactly in place, got delta {d}");
    }
}

#[test]
fn sneaking_on_scaffolding_keeps_descending_at_the_ordinary_cap() {
    let scaffolding = Shaft { scaffolding: true };
    let deltas = sneak_descent(&scaffolding, 6);

    assert_eq!(deltas[0], 0.0, "no velocity to clamp on the very first tick");

    // From tick 1 on, the sneak-hold conjunct is defeated by `is_scaffolding`,
    // so `yd` is only ever clamped to vanilla's ordinary climb-speed cap,
    // `-0.15`. Once gravity has pushed the raw delta past that cap (which it
    // does after a single tick: `-0.08 * 0.98 ≈ -0.0784` is still short of
    // `-0.15` on tick 1, but by tick 2 the accumulated/clamped velocity
    // reaches the cap and stays there), every subsequent tick must descend by
    // exactly `-0.15`.
    for (i, &d) in deltas.iter().enumerate().skip(2) {
        assert!(
            (d - (-CLIMB_CAP)).abs() < 1e-9,
            "tick {i}: scaffolding must not hold — expected exactly -{CLIMB_CAP}/tick, got {d}"
        );
    }
}

#[test]
fn the_same_sneak_input_produces_two_different_settled_rates() {
    // Stated as one comparison so the two tests above cannot independently
    // drift into agreement by accident: same input, same starting state,
    // only `is_scaffolding` differs, and the settled (post-tick-2) rates must
    // be exactly `0.0` vs. exactly `-0.15` — not merely "scaffolding is
    // faster".
    let ladder = Shaft { scaffolding: false };
    let scaffolding = Shaft { scaffolding: true };
    let ladder_deltas = sneak_descent(&ladder, 4);
    let scaffolding_deltas = sneak_descent(&scaffolding, 4);

    assert_eq!(ladder_deltas[3], 0.0);
    assert!((scaffolding_deltas[3] - (-CLIMB_CAP)).abs() < 1e-9);
    assert_ne!(
        ladder_deltas[3], scaffolding_deltas[3],
        "the tag membership is identical between the two; only the sneak-hold \
         exception should tell them apart"
    );
}
