//! Arithmetic gates for the client-authoritative vehicle tick.
//!
//! Every expected value here is re-derived from a formula read out of
//! `.cache/mc/26.2` and spelled out at the assertion, never obtained by calling
//! the function under test twice. That matters more than usual for this
//! subsystem: `decode(encode(x))` has no analogue for a physics tick, so the only
//! thing that separates "we ported the method" from "we ported *a* method" is
//! predicting the number.
//!
//! Where two readings of a clause would agree at the obvious input, the input is
//! moved. A boat accelerating forward for **one** tick from rest is `0.04` under
//! the correct order, under accelerate-then-drag, and under a wrong `invFriction`
//! — so the forward gate runs five ticks, by which point the three hypotheses are
//! 0.163804 / 0.147420 / 0.040000 apart.

use lodestone_physics::vehicle::{
    BOAT_GRAVITY, BoatInput, BoatState, BoatStatus, MountRule, boat_paddle_state, clamp_rider_yaw,
    control_boat, jump_riding_scale, player_jump_pending_scale, ridden_input, ridden_speed,
    tick_boat,
};
use lodestone_physics::{Aabb, CollisionView, EntityDimensions, EntityMotion, FluidCell, FluidKind, PhysicsProfile, Vec3d};

/// `EntityTypes.java`'s `sized(1.375F, 0.5625F)`, shared by the whole boat family.
const BOAT_WIDTH: f32 = 1.375;
const BOAT_HEIGHT: f32 = 0.5625;

/// A world that is water below `surface_y` and empty above, with no solids at all.
///
/// `surface_y` is the first **non**-water layer, so the topmost water cell is
/// `surface_y - 1` and — because its neighbour above is not water — reports
/// `getOwnHeight()` rather than a full block. That is the case a real ocean
/// surface is in, and the `8/9` it yields is load-bearing for every `waterLevel`
/// below.
#[derive(Debug)]
struct Ocean {
    surface_y: i32,
}

impl CollisionView for Ocean {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
    fn is_water(&self, _x: i32, y: i32, _z: i32) -> bool {
        y < self.surface_y
    }
    fn fluid_at(&self, _x: i32, y: i32, _z: i32) -> Option<FluidCell> {
        (y < self.surface_y).then_some(FluidCell {
            kind: FluidKind::Water,
            amount: 8,
            falling: false,
        })
    }
}

/// Nothing at all: no blocks, no fluid. `getGroundFriction` divides by a zero
/// count here, so its `NaN` fails `friction > 0.0F` and the boat is `IN_AIR`.
#[derive(Debug)]
struct Void;

impl CollisionView for Void {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

/// Solid ground everywhere below `surface_y`, no fluid at all, and every block
/// reporting vanilla's ordinary `Block.getFriction` of `0.6`.
///
/// This is the world a beached boat is in, and until this gate existed the whole
/// vehicle corpus had **no** view with a collision box in it: `Ocean` and `Void`
/// both return no boxes, so `getGroundFriction` divided by a zero count in every
/// existing test and `ON_LAND` was unreachable by construction.
#[derive(Debug)]
struct Ground {
    surface_y: i32,
}

/// `Block.getFriction`'s default, which every block but ice, packed ice, blue
/// ice and slime reports.
const DEFAULT_BLOCK_FRICTION: f32 = 0.6;

impl CollisionView for Ground {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y < self.surface_y {
            // **World-space**, as `CollisionView::collision_boxes` requires: the
            // block-local unit cube already offset by its own coordinates. A view
            // that returned block-local boxes here would make this gate agree with
            // a consumer that offsets a second time, which is precisely the defect
            // it exists to catch.
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
    fn friction(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        DEFAULT_BLOCK_FRICTION
    }
}

fn boat_dims() -> EntityDimensions {
    // The step height passed in is overwritten inside `tick_boat` with
    // `BOAT_STEP_HEIGHT`; anything here would do.
    EntityDimensions::new(BOAT_WIDTH, BOAT_HEIGHT, 0.6)
}

/// `AbstractBoat.controlBoat`'s forward acceleration, at the precision the source
/// actually has: `0.04F` widened into a `double` multiply, **not** `0.04_f64`.
fn forward_accel() -> f64 {
    f64::from(0.04_f32)
}

/// `floatBoat`'s `IN_WATER` drag, likewise a `float` in vanilla.
fn water_inv_friction() -> f64 {
    f64::from(0.9_f32)
}

/// Five ticks of forward input on open water, against the exact recurrence the
/// two vanilla methods compose to.
///
/// `floatBoat` drags **first** (`movement.x * invFriction`) and `controlBoat` adds
/// the impulse **after**, so the horizontal recurrence is
/// `v ← v · 0.9 + 0.04` and not `(v + 0.04) · 0.9`. Both are plausible readings of
/// "a boat has drag and accelerates", both give 0.04 after one tick, and they
/// separate by 11% after five.
#[test]
fn a_boat_accelerating_forward_on_water_follows_the_drag_then_impulse_recurrence() {
    let view = Ocean { surface_y: 64 };
    let profile = PhysicsProfile::mc_1_21();
    let mut motion = EntityMotion::at(Vec3d::new(0.5, 63.8, 0.5));
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;
    let input = BoatInput {
        up: true,
        ..BoatInput::default()
    };

    // The two hypotheses, computed here from the vanilla constants rather than
    // from the code under test.
    let drag = water_inv_friction();
    let accel = forward_accel();
    let mut correct = 0.0_f64;
    let mut drag_last = 0.0_f64;
    for _ in 0..5 {
        correct = correct * drag + accel;
        drag_last = (drag_last + accel) * drag;
    }

    for tick in 0..5 {
        tick_boat(
            &mut motion,
            &mut state,
            &mut yaw,
            input,
            boat_dims(),
            &view,
            &profile,
        );
        assert_eq!(
            state.status,
            Some(BoatStatus::InWater),
            "tick {tick}: the boat must stay classified IN_WATER for this gate's \
             recurrence to be the one under test; it drifted to {:?}",
            state.status
        );
    }

    // 0.163804 under the real order, 0.147420 under accelerate-then-drag, and
    // 0.040000 if `invFriction` stayed at its `0.05F` initialiser (a status the
    // match failed to claim).
    assert!(
        (motion.velocity.z - correct).abs() < 1e-12,
        "five ticks of forward input gave vz = {}, expected {correct} \
         (accelerate-then-drag would give {drag_last})",
        motion.velocity.z
    );
    assert!(
        (motion.velocity.z - drag_last).abs() > 0.01,
        "the two orderings must be distinguishable at this tick count, or the \
         gate measures only that the boat moves"
    );
    // Yaw is untouched with no turn keys held, which is what makes the impulse
    // purely +Z and therefore comparable against a scalar recurrence at all.
    assert!(yaw.abs() < 1e-9, "yaw drifted to {yaw} with no turn input");
    assert!(
        motion.velocity.x.abs() < 1e-9,
        "a boat at yaw 0 must not acquire sideways velocity, got {}",
        motion.velocity.x
    );
}

/// The three-clause conjunct in `controlBoat`'s turning bonus.
///
/// `if (inputRight != inputLeft && !inputUp && !inputDown) acceleration += 0.005F`
/// — implementing only the first clause gives a forward *turn* `0.045`, which is
/// 12.5% too fast and looks entirely plausible in motion. The two arms are
/// asserted against numbers derived from the two literals, and the wrong
/// hypothesis is computed alongside so the gate can say which one it landed on.
#[test]
fn the_turning_bonus_is_suppressed_while_moving_forward() {
    let profile_free = PhysicsProfile::mc_1_21();
    let _ = &profile_free;

    // Turning on the spot: the bonus is the *only* acceleration.
    let mut motion = EntityMotion::at(Vec3d::ZERO);
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;
    control_boat(
        &mut motion,
        &mut state,
        &mut yaw,
        BoatInput {
            right: true,
            ..BoatInput::default()
        },
    );
    let pivot_speed = motion.velocity.horizontal_distance();
    let expected_pivot = f64::from(0.005_f32);
    assert!(
        (pivot_speed - expected_pivot).abs() < 1e-9,
        "a boat pivoting on the spot must accelerate by exactly 0.005, got {pivot_speed}"
    );
    // `deltaRotation++` on the right key, and the yaw is committed inside this
    // same call.
    assert!(
        (yaw - 1.0).abs() < 1e-6,
        "one tick of right input must turn the boat exactly 1 degree, got {yaw}"
    );

    // Turning *and* forward: the bonus must not apply.
    let mut motion = EntityMotion::at(Vec3d::ZERO);
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;
    control_boat(
        &mut motion,
        &mut state,
        &mut yaw,
        BoatInput {
            right: true,
            up: true,
            ..BoatInput::default()
        },
    );
    let speed = motion.velocity.horizontal_distance();
    let correct = f64::from(0.04_f32);
    let with_bonus = f64::from(0.04_f32 + 0.005_f32);
    assert!(
        (speed - correct).abs() < 1e-7,
        "a forward turn must accelerate by 0.04, not {with_bonus}; got {speed}"
    );
    assert!(
        (speed - with_bonus).abs() > 1e-4,
        "the correct and the one-clause hypotheses must be separable, or this gate \
         cannot see the missing conjuncts"
    );

    // And the impulse is applied along the **new** yaw: `setYRot` runs between the
    // bonus and the forward acceleration, so a boat that starts turning this tick
    // already moves in the direction it turned to. At yaw 1 degree the sideways
    // component is `sin(-1 deg) * 0.04` = -0.000698; at yaw 0 it is exactly zero.
    let expected_x = f64::from((-1.0_f32).to_radians().sin()) * correct;
    assert!(
        (motion.velocity.x - expected_x).abs() < 1e-6,
        "the impulse must be along the post-turn yaw: x = {}, expected {expected_x}",
        motion.velocity.x
    );
    assert!(
        motion.velocity.x.abs() > 1e-5,
        "a zero x would mean the yaw was applied *after* the impulse"
    );
}

/// A boat's gravity is `0.04`, not the living `0.08`.
///
/// `AbstractBoat.getDefaultGravity()` overrides `Entity`'s, and nothing drags the
/// vertical velocity on the way down (`floatBoat` scales x and z only, and a boat
/// never calls `travel`), so three ticks of free fall is exactly `-0.12` and
/// linear — which is also how the gate tells `0.04` from a profile lookup that
/// would give `-0.24`.
#[test]
fn a_boat_in_air_falls_at_its_own_gravity_and_the_fall_is_linear() {
    let profile = PhysicsProfile::mc_1_21();
    assert!(
        // f32-sized tolerance, not f64-sized: `0.08f32 as f64` is
        // 0.079_999_998_211_860_66, so a 1e-9 bound here fails on the correct
        // answer. Discrimination is untouched — the value this must differ from is
        // twice as large.
        (f64::from(profile.gravity) - 0.08).abs() < 1e-6,
        "premise check: the living gravity this must NOT be is 0.08, profile says {}",
        profile.gravity
    );
    let mut motion = EntityMotion::at(Vec3d::new(0.5, 100.0, 0.5));
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;
    let mut observed = Vec::new();
    for _ in 0..3 {
        tick_boat(
            &mut motion,
            &mut state,
            &mut yaw,
            BoatInput::default(),
            boat_dims(),
            &Void,
            &profile,
        );
        observed.push(motion.velocity.y);
    }
    assert_eq!(
        state.status,
        Some(BoatStatus::InAir),
        "premise check: an empty view must classify IN_AIR (getGroundFriction \
         divides by zero and its NaN fails `> 0.0F`), got {:?}",
        state.status
    );
    // Collected and asserted on the collection, not inside the loop, so a neuter
    // that breaks the second and third ticks reports all three rather than
    // aborting on the first.
    let expected: Vec<f64> = (1..=3).map(|n| -BOAT_GRAVITY * f64::from(n)).collect();
    let mismatches: Vec<(usize, f64, f64)> = observed
        .iter()
        .zip(&expected)
        .enumerate()
        .filter(|(_, (got, want))| (**got - **want).abs() > 1e-12)
        .map(|(i, (got, want))| (i, *got, *want))
        .collect();
    assert!(
        mismatches.is_empty(),
        "boat free-fall velocities diverged from -0.04 per tick: {mismatches:?} \
         (living gravity would give {:?})",
        (1..=3)
            .map(|n| -0.08 * f64::from(n))
            .collect::<Vec<f64>>()
    );
}

/// The buoyancy term, at the one input where a wrong divisor is visible.
///
/// `buoyancy = (waterLevel - y) / bbHeight`, then
/// `y' = (y + buoyancy * (0.04 / 0.65)) * 0.75`. Three constants, and the *whole*
/// expression is predicted here — including the `0.75` damping, which a reading
/// that stops at the impulse would drop.
#[test]
fn the_first_tick_of_buoyancy_lands_on_the_predicted_value() {
    let view = Ocean { surface_y: 64 };
    let profile = PhysicsProfile::mc_1_21();
    let feet = 63.8_f64;
    let mut motion = EntityMotion::at(Vec3d::new(0.5, feet, 0.5));
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;
    tick_boat(
        &mut motion,
        &mut state,
        &mut yaw,
        BoatInput::default(),
        boat_dims(),
        &view,
        &profile,
    );

    // `FluidState.getHeight` for a source cell whose neighbour above is not water
    // is `getOwnHeight()` = `amount / 9.0F` = `8/9`, and `checkInWater` computes
    // the surface as a **float** add before widening.
    let surface = f64::from(63.0_f32 + 8.0_f32 / 9.0_f32);
    let buoyancy = (surface - feet) / f64::from(BOAT_HEIGHT);
    let expected =
        (0.0 - BOAT_GRAVITY + buoyancy * (BOAT_GRAVITY / 0.65)) * 0.75;
    assert!(
        (motion.velocity.y - expected).abs() < 1e-12,
        "buoyant vy was {}, expected {expected}",
        motion.velocity.y
    );
    // The wrong-but-plausible neighbour: dropping the 0.75 damping. It differs by
    // a quarter of the value, so this gate can tell them apart.
    let without_damping = 0.0 - BOAT_GRAVITY + buoyancy * (BOAT_GRAVITY / 0.65);
    assert!(
        (motion.velocity.y - without_damping).abs() > 1e-4,
        "the 0.75 damping must be observable at this input"
    );
    assert!(
        (state.water_level - surface).abs() < 1e-9,
        "waterLevel was {}, expected the 8/9 surface {surface}",
        state.water_level
    );
}

/// `setPaddleState(inputRight && !inputLeft || inputUp, inputLeft && !inputRight || inputUp)`.
///
/// **The left paddle animates on the *right* key**, and a transposition of two
/// adjacent booleans survives every round trip through our own encoder. So each
/// arm here is chosen to make the pair *unequal* — `(true, false)` and
/// `(false, true)` — because a fixture where they coincide cannot see a swap at
/// all.
#[test]
fn the_paddle_pair_is_not_symmetric_and_the_asymmetry_is_the_right_way_round() {
    let key = |left, right, up, down| {
        boat_paddle_state(BoatInput {
            left,
            right,
            up,
            down,
        })
    };
    // Forward alone rows both oars.
    assert_eq!(key(false, false, true, false), (true, true));
    // Right key alone: the LEFT paddle. Unequal pair, so a swap is visible.
    assert_eq!(
        key(false, true, false, false),
        (true, false),
        "turning right must animate the left paddle"
    );
    assert_eq!(
        key(true, false, false, false),
        (false, true),
        "turning left must animate the right paddle"
    );
    // Both turn keys cancel (`inputRight != inputLeft` is false).
    assert_eq!(key(true, true, false, false), (false, false));
    // Backward rows neither.
    assert_eq!(key(false, false, false, true), (false, false));
}

/// The jump-charge ramp's two arms and the discontinuity between them.
///
/// A fixture at five ticks measures `ticks * 0.1` and cannot tell the second arm
/// exists. The interesting fact is that the ramp **peaks at exactly ten ticks and
/// then decays**, so "hold longer, jump higher" is wrong past ten — which is also
/// the shape a hand-written reimplementation gets wrong.
#[test]
fn the_horse_jump_ramp_peaks_at_ten_ticks_and_then_decays() {
    // First arm, `ticks < 10`.
    for ticks in 1..=9 {
        let expected = ticks as f32 * 0.1;
        assert!(
            (jump_riding_scale(ticks) - expected).abs() < 1e-6,
            "ticks {ticks}: got {}, expected {expected}",
            jump_riding_scale(ticks)
        );
    }
    // Second arm, `0.8F + 2.0F / (ticks - 9) * 0.1F`, evaluated by hand.
    let second_arm = [(10, 1.0_f32), (11, 0.9), (12, 0.8 + 2.0 / 3.0 * 0.1)];
    let mismatches: Vec<(i32, f32, f32)> = second_arm
        .iter()
        .filter(|(ticks, want)| (jump_riding_scale(*ticks) - *want).abs() > 1e-6)
        .map(|(ticks, want)| (*ticks, jump_riding_scale(*ticks), *want))
        .collect();
    assert!(
        mismatches.is_empty(),
        "the second arm of the ramp is wrong: {mismatches:?}"
    );
    // The discontinuity, stated as an inequality a monotonic implementation fails.
    assert!(
        jump_riding_scale(10) > jump_riding_scale(9),
        "the charge must jump at ten ticks"
    );
    assert!(
        jump_riding_scale(11) < jump_riding_scale(10),
        "the charge must *decay* past ten ticks; a monotonic ramp is the wrong \
         hypothesis and this is the input that separates them"
    );
    // Nine and eleven happen to coincide at 0.9 — which is exactly why neither
    // alone is a discriminating input, and why the gate above uses ten.
    assert!((jump_riding_scale(9) - jump_riding_scale(11)).abs() < 1e-6);
}

/// `PlayerRideableJumping.getPlayerJumpPendingScale`: the floor is **0.4**, not
/// zero, and the ceiling clamps at a boost of 90 rather than 100.
#[test]
fn the_jump_boost_byte_maps_onto_a_scale_with_a_floor_of_four_tenths() {
    assert!((player_jump_pending_scale(0) - 0.4).abs() < 1e-6);
    // `0.4 + 0.4 * 45 / 90` = 0.6.
    assert!((player_jump_pending_scale(45) - 0.6).abs() < 1e-6);
    // `0.4 + 0.4 * 89 / 90` = 0.795555…, which a `>= 90` clamp written as `> 90`
    // would also give at 90 — so 89 and 90 are both asserted.
    assert!((player_jump_pending_scale(89) - (0.4 + 0.4 * 89.0 / 90.0)).abs() < 1e-6);
    assert!((player_jump_pending_scale(90) - 1.0).abs() < 1e-6);
    assert!((player_jump_pending_scale(100) - 1.0).abs() < 1e-6);
}

/// `AbstractHorse.getRiddenInput`'s three clauses, each at an input where it is
/// the only one doing anything.
#[test]
fn a_horse_halves_sideways_input_and_quarters_reverse() {
    // Pairwise-distinct magnitudes so a transposition of the returned pair cannot
    // survive: 0.5 out of 1.0 sideways, 1.0 out of 1.0 forward.
    let (sideways, forward) = ridden_input(MountRule::Horse, 1.0, 1.0, true, false);
    assert!((sideways - 0.5).abs() < 1e-6, "sideways was {sideways}");
    assert!((forward - 1.0).abs() < 1e-6, "forward was {forward}");
    // Reverse is quartered, and the test is `<= 0.0` so the sign is preserved.
    let (_, back) = ridden_input(MountRule::Horse, 0.0, -1.0, true, false);
    assert!((back + 0.25).abs() < 1e-6, "reverse was {back}");
    // A reared mount on the ground refuses to move at all.
    assert_eq!(ridden_input(MountRule::Horse, 1.0, 1.0, true, true), (0.0, 0.0));
    // …but the same rear-up in mid-air does not suppress the input: the clause is
    // `onGround() && isStanding()`, and dropping the first conjunct would freeze a
    // falling mount.
    let (air_sideways, air_forward) = ridden_input(MountRule::Horse, 1.0, 1.0, false, true);
    assert!((air_sideways - 0.5).abs() < 1e-6);
    assert!((air_forward - 1.0).abs() < 1e-6);
}

/// **`AbstractHorse`'s rule is not universal**, and this is the gate for the
/// override that makes a pig un-steerable.
///
/// `Pig.getRiddenInput` and `Strider.getRiddenInput` both return a bare
/// `new Vec3(0.0, 0.0, 1.0)` and never look at the controller, so no key input
/// reaches them. The wrong hypothesis — reading `AbstractHorse`'s rule for the
/// whole family — is computed alongside, because at a forward-only input the two
/// **agree** and only a sideways or reverse input separates them.
#[test]
fn a_pig_and_a_strider_ignore_their_riders_keys_entirely() {
    for rule in [
        MountRule::Steered {
            speed_factor: 0.225,
        },
        MountRule::Steered { speed_factor: 0.55 },
    ] {
        // Forward-only is the coincident input: both hypotheses give (0, 1).
        assert_eq!(ridden_input(rule, 0.0, 1.0, true, false), (0.0, 1.0));
        // Reverse and strafe are the discriminating ones. The horse rule would give
        // (0.5, -0.25) here; the real one still gives a flat forward.
        assert_eq!(
            ridden_input(rule, 1.0, -1.0, true, false),
            (0.0, 1.0),
            "a steered mount must ignore strafe and reverse; the horse rule would \
             give {:?}",
            ridden_input(MountRule::Horse, 1.0, -1.0, true, false)
        );
        assert_ne!(
            ridden_input(rule, 1.0, -1.0, true, false),
            ridden_input(MountRule::Horse, 1.0, -1.0, true, false),
            "the two rules must be distinguishable at this input, or the gate \
             measures nothing"
        );
    }
}

/// `getRiddenSpeed`'s three arms, predicted from the literals.
///
/// The pig scale (`0.225`) and the strider's (`0.55`) are the reason reading one
/// rule for the family is not a rounding error: at the same attribute a pig moves
/// **4.4×** slower than the horse rule would give it.
#[test]
fn the_ridden_speed_scales_differ_by_family() {
    // A plausible per-instance horse speed; the value only has to be shared across
    // the arms for the ratios to be the claim.
    let attr = 0.2_f64;
    let horse = ridden_speed(MountRule::Horse, attr, false);
    assert!((f64::from(horse) - attr).abs() < 1e-6, "horse speed was {horse}");
    let pig = ridden_speed(
        MountRule::Steered {
            speed_factor: 0.225,
        },
        attr,
        false,
    );
    // f32-sized tolerances: vanilla narrows once at the end of the `Steered` arm
    // (`(float)(getAttributeValue(...) * 0.225 * boostFactor())`), so the returned
    // value is an `f32` and a 1e-9 bound is tighter than the type can represent.
    // Discrimination is untouched — the horse arm below is 4.4x away.
    assert!(
        (f64::from(pig) - attr * 0.225).abs() < 1e-6,
        "pig speed was {pig}, expected {}",
        attr * 0.225
    );
    let strider = ridden_speed(MountRule::Steered { speed_factor: 0.55 }, attr, false);
    assert!(
        (f64::from(strider) - attr * 0.55).abs() < 1e-6,
        "strider speed was {strider}"
    );
    assert!(
        f64::from(horse / pig) > 4.0,
        "the horse rule must be visibly faster than a pig's, or the override does \
         not matter: ratio {}",
        horse / pig
    );
    // The camel's bonus is **additive**, not multiplicative, and the two readings
    // differ: 0.2 + 0.1 = 0.3, where a `* 1.1` reading gives 0.22.
    let camel_walk = ridden_speed(MountRule::Camel, attr, false);
    let camel_sprint = ridden_speed(MountRule::Camel, attr, true);
    assert!((f64::from(camel_walk) - attr).abs() < 1e-6);
    assert!(
        (f64::from(camel_sprint) - 0.3).abs() < 1e-6,
        "a sprinting camel is base + 0.1 = 0.3, not base * 1.1 = 0.22; got \
         {camel_sprint}"
    );
}

/// `AbstractBoat.clampRotation`: ±105° of the boat's heading, on the **wrapped**
/// difference.
#[test]
fn a_boat_clamps_its_riders_yaw_to_a_window_around_its_own_heading() {
    // Inside the window: untouched.
    assert!((clamp_rider_yaw(50.0, 0.0) - 50.0).abs() < 1e-6);
    // Outside: `wrapDegrees(200 - 0)` is **-160**, not 200, so the clamp pulls the
    // rider to -105 and the returned yaw is `200 + (-105 - -160)` = 255. Reading
    // the difference unwrapped would clamp to +105 and give 105 — the opposite
    // direction, and a plausible-looking number.
    let clamped = clamp_rider_yaw(200.0, 0.0);
    assert!(
        (clamped - 255.0).abs() < 1e-4,
        "expected 255 (the wrapped reading), got {clamped}; 105 would mean the \
         difference was not wrapped"
    );
    // The invariant the number above is an instance of: the wrapped difference is
    // always inside the window afterwards.
    for rider in [-350.0_f32, -170.0, -106.0, 0.0, 104.0, 106.0, 200.0, 359.0] {
        let out = clamp_rider_yaw(rider, 30.0);
        let delta = (out - 30.0).rem_euclid(360.0);
        let delta = if delta > 180.0 { delta - 360.0 } else { delta };
        assert!(
            delta.abs() <= 105.0 + 1e-3,
            "rider {rider} ended {delta} degrees from the boat's heading"
        );
    }
}

/// The entry-into-water snap: crossing from `IN_AIR` to water places the hull at
/// `getWaterLevelAbove() - bbHeight + 0.101` and kills the vertical velocity.
///
/// This is the clause that makes a dropped boat *land* on the surface rather than
/// bobbing up through it over several ticks, and it is an edge on `oldStatus` — so
/// it needs two ticks to observe, not one.
#[test]
fn a_boat_falling_into_water_snaps_to_the_surface_and_stops_descending() {
    let profile = PhysicsProfile::mc_1_21();
    // Tick one in the void establishes `oldStatus = IN_AIR` and one tick of fall.
    // The start height is chosen so the *next* tick's floor sits below the 8/9
    // surface at 63.888889 — 63.92 minus one tick of 0.04 gravity is 63.88, and
    // `checkInWater`'s test is a strict `bb.minY < surface`, so a boat that ends
    // the tick at 63.89 is still classified IN_AIR and this gate would measure
    // nothing.
    let mut motion = EntityMotion::at(Vec3d::new(0.5, 63.92, 0.5));
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;
    tick_boat(
        &mut motion,
        &mut state,
        &mut yaw,
        BoatInput::default(),
        boat_dims(),
        &Void,
        &profile,
    );
    assert_eq!(state.status, Some(BoatStatus::InAir), "premise check");

    // Tick two, now over water. `getWaterLevelAbove` scans from the roof upward
    // for the first layer that is not full water; with the surface at y = 64 the
    // roof already sits in air, so the first non-full layer is the roof's own and
    // its water height is 0 — giving `64 - 0.5625 + 0.101`.
    let view = Ocean { surface_y: 64 };
    let before = motion.position.y;
    tick_boat(
        &mut motion,
        &mut state,
        &mut yaw,
        BoatInput::default(),
        boat_dims(),
        &view,
        &profile,
    );
    assert_eq!(
        state.status,
        Some(BoatStatus::InWater),
        "the entry branch reclassifies unconditionally, whether or not the snap fit"
    );
    // `getWaterLevelAbove` scans upward from the roof (which is already in air at
    // y = 64) for the first layer that is not full water; that layer's own water
    // height is 0, so the answer is a flat 64.0 and the target is
    // `64 - 0.5625 + 0.101`. Predicted exactly rather than as a direction.
    let expected_y = 64.0 - f64::from(BOAT_HEIGHT) + 0.101;
    assert!(
        (motion.position.y - expected_y).abs() < 1e-12,
        "the entry snap put the hull at {} instead of {expected_y} (it was at \
         {before} before the snap)",
        motion.position.y
    );
    let roof_layer = motion.position.y + f64::from(BOAT_HEIGHT);
    assert!(
        roof_layer > 64.0 && motion.position.y < before,
        "the hull must end straddling the surface: y went {before} -> {}",
        motion.position.y
    );
    // The snap zeroes the vertical velocity, so the boat is not still carrying its
    // fall into the next tick.
    assert!(
        motion.velocity.y.abs() < 1e-9,
        "the entry snap must zero vy, got {}",
        motion.velocity.y
    );
    assert!(
        state.last_yd.abs() < 1e-9,
        "…and lastYd with it, or the next getWaterLevelAbove scans a widened range"
    );
}


/// A boat sitting on ordinary ground must classify `ON_LAND` and take that
/// block's friction as its drag — the thing that makes a beached boat crawl.
///
/// # The two hypotheses, and why they are far apart
///
/// `floatBoat`'s `invFriction` is `landFriction` on `ON_LAND` and `0.9F` on
/// `IN_AIR` — and `IN_AIR`'s `0.9` is the *same number* `IN_WATER` uses, so a
/// classification that falls through to `IN_AIR` produces a boat that behaves
/// exactly as if it were afloat. That is the owner-reported symptom ("riding a
/// boat on land keeps it super fast, as if I'm in the water") and it is not a
/// tuning difference: over five ticks of forward input the two recurrences,
/// `v <- v * 0.6 + 0.04` and `v <- v * 0.9 + 0.04`, separate by a factor of
/// about 1.7, and the terminal speeds by a factor of four.
///
/// The cause was a double block offset in `getGroundFriction`'s probe: the boxes
/// `CollisionView::collision_boxes` yields are already world-space, and offsetting
/// them again by `(x, y, z)` put every candidate at roughly twice its height,
/// where nothing can meet a 1 mm slab under the hull. The touch count stayed `0`,
/// the mean came back `NaN`, `friction > 0.0` was false, and every boat on land
/// was `IN_AIR`. It is only invisible at `y == 0`, where the second offset is the
/// identity — so a fixture at the origin cannot see it either.
#[test]
fn a_boat_on_ground_classifies_on_land_and_drags_with_that_blocks_friction() {
    let view = Ground { surface_y: 64 };
    let profile = PhysicsProfile::mc_1_21();
    let mut motion = EntityMotion::at(Vec3d::new(0.5, 64.0, 0.5));
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;
    let input = BoatInput {
        up: true,
        ..BoatInput::default()
    };

    // Both recurrences derived here from the vanilla constants, not from the code
    // under test: `floatBoat` drags first and `controlBoat` adds the impulse
    // after, so `v <- v * invFriction + accel` under either classification.
    let accel = forward_accel();
    let mut on_land = 0.0_f64;
    let mut in_air = 0.0_f64;
    for _ in 0..5 {
        on_land = on_land * f64::from(DEFAULT_BLOCK_FRICTION) + accel;
        in_air = in_air * water_inv_friction() + accel;
    }

    for tick in 0..5 {
        tick_boat(
            &mut motion,
            &mut state,
            &mut yaw,
            input,
            boat_dims(),
            &view,
            &profile,
        );
        assert_eq!(
            state.status,
            Some(BoatStatus::OnLand),
            "tick {tick}: a boat resting on solid ground must classify ON_LAND; it \
             classified {:?}, and IN_AIR's own invFriction is water's 0.9",
            state.status
        );
        // Read **after** the tick, so this is the post-halving value:
        // `getStatus` latches `landFriction = getGroundFriction()` and
        // `floatBoat` then uses it and halves it, because a player is aboard.
        // The halving is not cumulative across ticks -- the next `getStatus`
        // re-latches the block's own `0.6` -- so the *drag* is `0.6` every tick
        // and only this residue is halved. Asserting `0.6` here would be
        // asserting the wrong side of that order.
        assert!(
            (state.land_friction - DEFAULT_BLOCK_FRICTION / 2.0).abs() < 1e-6,
            "tick {tick}: landFriction must latch the block's own 0.6 and then \
             halve for the player aboard, got {}",
            state.land_friction
        );
    }

    // 0.0954... on land against 0.163804 in air/water -- the wrong hypothesis is
    // 1.7x the right one here, and four times it at terminal speed.
    // `1e-6`, not the water gate's `1e-12`, and the difference is real rather
    // than slack: water's `invFriction` is the literal `0.9F`, while land's is an
    // `f32` **mean** -- `getGroundFriction` sums `0.6F` over however many cells
    // the hull touches and divides by an `int` count -- so its exact value
    // depends on the summation order and lands about `1e-7` off a clean `0.6`.
    // The two hypotheses here are `0.07` apart, so this tolerance is still five
    // orders of magnitude short of being able to confuse them.
    assert!(
        (motion.velocity.z - on_land).abs() < 1e-6,
        "five ticks of forward input on land gave vz = {}, expected {on_land} \
         (an IN_AIR misclassification would give {in_air})",
        motion.velocity.z
    );
    assert!(
        (on_land - in_air).abs() > 0.05,
        "the two classifications must be distinguishable at this tick count, or \
         this gate cannot fail: {on_land} vs {in_air}"
    );
}

/// The same boat with **nothing** under it must still classify `IN_AIR` — the
/// negative control for the gate above, and the arm that proves `ON_LAND` is
/// being chosen by the ground probe rather than returned unconditionally.
#[test]
fn a_boat_over_a_void_still_classifies_in_air() {
    let view = Void;
    let profile = PhysicsProfile::mc_1_21();
    let mut motion = EntityMotion::at(Vec3d::new(0.5, 64.0, 0.5));
    let mut state = BoatState::default();
    let mut yaw = 0.0_f32;

    tick_boat(
        &mut motion,
        &mut state,
        &mut yaw,
        BoatInput::default(),
        boat_dims(),
        &view,
        &profile,
    );

    assert_eq!(
        state.status,
        Some(BoatStatus::InAir),
        "an empty probe divides by a zero count, and NaN fails `friction > 0.0`"
    );
}
