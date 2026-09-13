//! Vanilla's own edge-back-off step — the sneak-at-a-ledge back-off.
//!
//! # What these tests are for, and what they are *not*
//!
//! The bit-exactness evidence lives in `tests/golden.rs`: `sneak_edge_stop`,
//! `sneak_edge_walk_off` and `sneak_edge_diagonal` are replayed from the
//! independent Python oracle in `gen_golden.py` with no tolerance, and the other
//! 26 traces are checked to be byte-identical to before the rule existed.
//!
//! This file carries the two things a trace cannot:
//!
//! 1. **A pure control.** `back_off_is_the_only_difference` runs the *same*
//!    delta, world, position and dimensions through [`move_entity`] twice, with
//!    [`EdgeBackOff::Player`] and [`EdgeBackOff::Entity`]. Nothing else differs, so
//!    the divergence is attributable to the rule alone — and the `Entity` arm must
//!    walk off the ledge, which is what proves the fixture can express the failure
//!    at all. A "the player stopped at the edge" assertion with no such arm is
//!    satisfied by a fixture with no edge in it.
//! 2. **Hand-derived expectations for the parts a smooth trace hides**: that the
//!    delta is walked toward zero in a *loop* rather than clamped once, that X and
//!    Z are probed independently before jointly, and that the fall-distance
//!    field gates the airborne branch — each with the number derived from the
//!    inequality in vanilla's own "can fall at least" check, not from running
//!    this crate.
//!
//! **On `fall_distance` specifically:** `fall_distance_gates_the_airborne_branch`
//! below tests the gate's *sensitivity* to the input at the [`move_entity`]
//! primitive level, with a hand-set `fall_distance` — that is still the right
//! tool for isolating the gate's own arithmetic from everything upstream of it.
//! What it does *not* cover, and what used to make the hand-set value the whole
//! story: whether `PlayerState::fall_distance` itself is ever anything other than
//! the permanent `0.0` it started as. It no longer is —
//! `PlayerState::fall_distance`'s own doc lists every accumulation/reset site
//! this crate now reproduces from the jar — and `tests/fall_distance.rs` is
//! where that maintenance is tested: every reset driven by real ticks through
//! the public `tick`/`tick_air`/`tick_water`/`tick_lava`/`tick_elytra` entry
//! points (never by hand-setting the field under test), plus one flagship test
//! that drives a real fall to a real `fall_distance` past the max-down-step value and shows it
//! changes the *committed position* at this exact gate, against a zero control.

use std::collections::HashSet;

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::Aabb;
use lodestone_physics::{
    EdgeBackOff, EntityDimensions, EntityMotion, MoveContext, PhysicsProfile, Vec3d, move_entity,
};

/// A world of unit-cube solid blocks.
#[derive(Default)]
struct World {
    solid: HashSet<(i32, i32, i32)>,
}

impl World {
    /// A floor at `y = 0` whose eastern edge is the `x = 1` plane: solid for
    /// `x <= 0` only.
    ///
    /// A player standing at `x = 0.5` has box `[0.2, 0.8]`, and vanilla's own
    /// "can fall at least" check insets the probe by `1e-7`, so the probe clears the support exactly when
    /// `0.2 + 1e-7 + deltaX >= 1.0`, i.e. `deltaX >= 0.8 - 1e-7`. Every expected
    /// value below is derived from that one inequality.
    fn ledge_at_x1() -> Self {
        let mut w = Self::default();
        for x in -6..=0 {
            for z in -6..=6 {
                w.solid.insert((x, 0, z));
            }
        }
        w
    }

    /// A floor at `y = 0` missing wherever `x >= 1` **and** `z >= 1` — an outside
    /// corner. Neither a pure-X nor a pure-Z probe from `x = z = 0.5` clears the
    /// support; only the joint probe does.
    fn outside_corner() -> Self {
        let mut w = Self::default();
        for x in -6..=6 {
            for z in -6..=6 {
                if x >= 1 && z >= 1 {
                    continue;
                }
                w.solid.insert((x, 0, z));
            }
        }
        w
    }
}

impl CollisionView for World {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.solid.contains(&(x, y, z)) {
            let (fx, fy, fz) = (f64::from(x), f64::from(y), f64::from(z));
            out.push(Aabb::new(fx, fy, fz, fx + 1.0, fy + 1.0, fz + 1.0));
        }
    }
}

/// Runs one vanilla-own move step with the given candidate delta and returns
/// the position it commits. `on_ground` is the pre-move flag vanilla's own
/// "is above ground" check reads.
fn move_once(
    world: &World,
    position: Vec3d,
    delta: Vec3d,
    on_ground: bool,
    back_off: EdgeBackOff,
) -> Vec3d {
    let profile = PhysicsProfile::mc_1_21();
    let mut motion = EntityMotion::at(position);
    motion.velocity = delta;
    motion.on_ground = on_ground;
    move_entity(
        &mut motion,
        EntityDimensions::PLAYER,
        world,
        &profile,
        MoveContext {
            edge_back_off: back_off,
            ..MoveContext::default()
        },
    );
    motion.position
}

/// Vanilla's own "is staying on ground surface" check held, nothing else notable.
const SNEAKING: EdgeBackOff = EdgeBackOff::Player {
    staying_on_ground_surface: true,
    fall_distance: 0.0,
};

#[test]
fn back_off_is_the_only_difference() {
    // THE PURE CONTROL. One delta, one world, one position; the only thing that
    // varies between the two arms is which edge-back-off override the
    // entity has. `EdgeBackOff::Entity` is vanilla's base implementation
    // (`return delta`), i.e. exactly the behaviour this crate had before the rule
    // was modelled.
    let world = World::ledge_at_x1();
    let start = Vec3d::new(0.5, 1.0, 0.5);
    // 0.8 clears the support (>= 0.8 - 1e-7), so the rule has something to do.
    let delta = Vec3d::new(0.8, 0.0, 0.0);

    let without = move_once(&world, start, delta, true, EdgeBackOff::Entity);
    let with = move_once(&world, start, delta, true, SNEAKING);

    // The control must FAIL the property the rule exists to establish: its box
    // ends up entirely past x = 1.0, hanging over nothing.
    let control_box_min_x = without.x - 0.3;
    assert!(
        control_box_min_x >= 1.0,
        "control did not leave the ledge (box min_x {control_box_min_x}) — the \
         fixture cannot express the failure and the positive test below is vacuous"
    );

    // And the rule must establish it: the box still overlaps the support.
    let held_box_min_x = with.x - 0.3;
    assert!(
        held_box_min_x < 1.0,
        "back-off let the box leave the support (box min_x {held_box_min_x})"
    );
    assert_ne!(without.x, with.x, "the rule made no difference at all");
}

#[test]
fn back_off_steps_in_005_increments_rather_than_clamping_once() {
    // The distinction a single clamp cannot reproduce. `canFallAtLeast(dx)` is true
    // for every `dx >= 0.8 - 1e-7` here (the probe only has to clear x = 1.0, and
    // there is nothing further east to stop it), so from delta.x = 1.0 the loop
    // steps 1.00 -> 0.95 -> 0.90 -> 0.85 -> 0.80 -> 0.75 and exits at the first
    // value that fails the probe: FIVE subtractions of 0.05.
    //
    // A clamp-to-the-boundary implementation would instead land on ~0.7999999 (the
    // largest non-falling delta). The two answers differ by ~0.05, which is why the
    // loop is not an implementation detail.
    let world = World::ledge_at_x1();
    let start = Vec3d::new(0.5, 1.0, 0.5);
    let moved = move_once(&world, start, Vec3d::new(1.0, 0.0, 0.0), true, SNEAKING);

    // The loop written as straight-line arithmetic. The *count* is the derived
    // claim; the rounding is IEEE's, not a choice.
    let expected_delta_x = 1.0_f64 - 0.05 - 0.05 - 0.05 - 0.05 - 0.05;
    assert_eq!(
        moved.x.to_bits(),
        (0.5_f64 + expected_delta_x).to_bits(),
        "expected five 0.05 steps (delta.x {expected_delta_x}), got {}",
        moved.x - 0.5
    );
    // Guard against the clamp-once answer specifically.
    assert!(
        moved.x - 0.5 < 0.79,
        "delta.x {} looks clamped to the boundary, not stepped",
        moved.x - 0.5
    );
}

#[test]
fn x_and_z_are_probed_independently_before_jointly() {
    // At the outside corner from (0.5, 1.0, 0.5) with delta (0.8, 0, 0.8):
    //
    // * the pure-X probe still overlaps block (1, 0, 0) — that column is solid
    //   because only `x >= 1 && z >= 1` is missing — so loop 1 leaves X alone;
    // * by symmetry loop 2 leaves Z alone;
    // * the joint probe lands inside the missing quadrant and clears everything, so
    //   loop 3 takes exactly one step off each axis before the probe fails.
    //
    // A vector-clamped implementation, or one that ran only the two independent
    // loops, would leave the delta untouched and walk into the hole.
    let world = World::outside_corner();
    let start = Vec3d::new(0.5, 1.0, 0.5);
    let moved = move_once(&world, start, Vec3d::new(0.8, 0.0, 0.8), true, SNEAKING);

    let expected_delta = 0.8_f64 - 0.05;
    assert_eq!(
        moved.x.to_bits(),
        (0.5_f64 + expected_delta).to_bits(),
        "loop 3 did not step X once (delta.x {})",
        moved.x - 0.5
    );
    assert_eq!(
        moved.z.to_bits(),
        (0.5_f64 + expected_delta).to_bits(),
        "loop 3 did not step Z once (delta.z {})",
        moved.z - 0.5
    );

    // And the independent loops must provably have been no-ops: a pure-X move of
    // the same magnitude is left completely alone, because block (1, 0, 0) is
    // solid. Same delta, same world — only the Z component differs from above.
    let pure_x = move_once(&world, start, Vec3d::new(0.8, 0.0, 0.0), true, SNEAKING);
    assert_eq!(
        pure_x.x.to_bits(),
        (0.5_f64 + 0.8).to_bits(),
        "loop 1 fired when the pure-X probe should have been blocked"
    );
}

#[test]
fn upward_delta_and_released_shift_both_veto_the_rule() {
    let world = World::ledge_at_x1();
    let start = Vec3d::new(0.5, 1.0, 0.5);
    let off_the_edge = 0.5_f64 + 0.8;

    // `!(delta.y > 0.0)`: any upward component disables the back-off outright, so a
    // sneak-jump off a ledge still leaves the ledge.
    let jumping = move_once(&world, start, Vec3d::new(0.8, 0.1, 0.0), true, SNEAKING);
    assert_eq!(
        jumping.x.to_bits(),
        off_the_edge.to_bits(),
        "an upward delta must not be backed off"
    );

    // Vanilla's own "is staying on ground surface" check: the raw shift key.
    let not_sneaking = move_once(
        &world,
        start,
        Vec3d::new(0.8, 0.0, 0.0),
        true,
        EdgeBackOff::Player {
            staying_on_ground_surface: false,
            fall_distance: 0.0,
        },
    );
    assert_eq!(
        not_sneaking.x.to_bits(),
        off_the_edge.to_bits(),
        "the rule fired without shift held"
    );

    // A zero delta.y is *not* upward — this is the walking case and must back off.
    let walking = move_once(&world, start, Vec3d::new(0.8, 0.0, 0.0), true, SNEAKING);
    assert_ne!(
        walking.x.to_bits(),
        off_the_edge.to_bits(),
        "delta.y == 0.0 was treated as upward"
    );
}

#[test]
fn fall_distance_gates_the_airborne_branch() {
    // Vanilla's own "is above ground" check: on ground, or fall-distance
    // below the max-down-step value and the "can fall at least" check (over
    // the remaining down-step allowance) fails.
    //
    // Feet at y = 1.3, airborne, descending: the floor top is 0.3 below.
    //
    // * `fallDistance = 0.0` probes the full 0.6, which reaches the floor, so
    //   the "can fall at least" check is false, "is above ground" is TRUE,
    //   and the rule applies.
    // * `fallDistance = 0.45` probes only 0.15, which does *not* reach the
    //   floor 0.3 below, so the "can fall at least" check is true, "is above
    //   ground" is FALSE, and the rule does not apply.
    //
    // That asymmetry is why `fall_distance` is a real input and not decoration:
    // defaulting it to 0.0 makes the gate open more often than vanilla's, never
    // less.
    let world = World::ledge_at_x1();
    let start = Vec3d::new(0.5, 1.3, 0.5);
    let delta = Vec3d::new(0.8, -0.1, 0.0);
    let off_the_edge = 0.5_f64 + 0.8;

    let fresh = move_once(
        &world,
        start,
        delta,
        false,
        EdgeBackOff::Player {
            staying_on_ground_surface: true,
            fall_distance: 0.0,
        },
    );
    assert_ne!(
        fresh.x.to_bits(),
        off_the_edge.to_bits(),
        "airborne with fallDistance 0.0 should still back off"
    );

    let falling = move_once(
        &world,
        start,
        delta,
        false,
        EdgeBackOff::Player {
            staying_on_ground_surface: true,
            fall_distance: 0.45,
        },
    );
    assert_eq!(
        falling.x.to_bits(),
        off_the_edge.to_bits(),
        "fallDistance 0.45 should have closed the airborne branch"
    );
}

#[test]
fn a_mob_never_gets_the_player_override() {
    // Vanilla's own base edge-back-off step is the identity, and `MoveContext::default()` —
    // what `lodestone-shell`'s dropped-item mover and any mob loop pass — selects
    // it. This is the structural guarantee the enum buys over a bare bool.
    let world = World::ledge_at_x1();
    let start = Vec3d::new(0.5, 1.0, 0.5);
    assert_eq!(MoveContext::default().edge_back_off, EdgeBackOff::Entity);
    let moved = move_once(
        &world,
        start,
        Vec3d::new(0.8, 0.0, 0.0),
        true,
        MoveContext::default().edge_back_off,
    );
    assert_eq!(moved.x.to_bits(), (0.5_f64 + 0.8).to_bits());
}

#[test]
fn back_off_does_not_zero_the_velocity_it_cancels() {
    // Vanilla rewrites only the *local* candidate delta; it never rewrites
    // its own velocity field. So the X-collision flag (which compares against
    // the backed-off delta) reads false, restitution never fires, and the velocity survives the
    // tick intact. Releasing shift therefore launches you at full speed — this is
    // vanilla behaviour, and a "clamp the velocity" implementation would lose it.
    let world = World::ledge_at_x1();
    let profile = PhysicsProfile::mc_1_21();
    let mut motion = EntityMotion::at(Vec3d::new(0.5, 1.0, 0.5));
    motion.velocity = Vec3d::new(0.8, 0.0, 0.0);
    motion.on_ground = true;
    move_entity(
        &mut motion,
        EntityDimensions::PLAYER,
        &world,
        &profile,
        MoveContext {
            edge_back_off: SNEAKING,
            ..MoveContext::default()
        },
    );
    assert!(
        !motion.horizontal_collision,
        "a fully cancelled component must not register as a horizontal collision"
    );
    assert_eq!(
        motion.velocity.x.to_bits(),
        0.8_f64.to_bits(),
        "the back-off must not touch deltaMovement"
    );
}
