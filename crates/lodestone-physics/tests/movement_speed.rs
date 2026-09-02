//! `PlayerState::movement_speed` reaching the integrator.
//!
//! # What this file is for, and what it is not
//!
//! The bit-exactness evidence lives in `tests/golden.rs`:
//! `walk_speed_ii_matches_golden` is a full multi-tick trace replayed against
//! the independent Python oracle in `gen_golden.py`, and its own doc explains
//! why it had to be *added* rather than found already covered — no
//! pre-existing scenario ever set `movement_speed` away from its `None`
//! default, so regenerating the golden file with the refactor alone (`gen_
//! golden.py`'s `player_speed` reading an override) produced a byte-for-byte
//! zero diff across all 42 pre-existing consts.
//!
//! This file carries what a smooth trace cannot show on its own, in the style
//! of `tests/lava_depth.rs`'s `shallow_vs_deep_is_the_only_difference`:
//!
//! 1. **A pure control.** [`speed_ii_and_sprint_are_the_only_difference`] runs
//!    the *same* starting state, world and input through [`tick_air`] twice
//!    (via the crate's own real dispatch, not a hand-rolled step), varying
//!    only [`PlayerState::movement_speed`] — the one input `effective_speed`
//!    actually reads. Two different injected values produce two different
//!    resulting positions, and the divergence is attributable to
//!    `movement_speed` alone because nothing else differs between the runs.
//! 2. **A hand-derived expectation for each arm**, computed here directly from
//!    the jar's formula (vanilla's own input-vector accessor at yaw 0, on a
//!    default-friction floor), not by calling the crate's own `tick`/`friction_
//!    influenced_speed`/`input_vector` helpers to produce the *expected*
//!    number — that would just be testing the code against itself.
//!
//! # Why one tick, and why yaw 0 on 0.6 friction
//!
//! Vanilla's own friction-influenced speed step only passes its own speed
//! accessor through unmodified when `blockFriction > 0.6F` is *false* — the
//! crate's own `friction_influenced_speed_default_ground_is_getspeed`
//! test already pins this for the default terrain friction (`0.6F` exactly,
//! and `0.6F > 0.6F` is `false`). Combined with yaw `0.0` (so vanilla's own
//! input-vector accessor's rotation is the identity: `sin = 0`, `cos = 1`) and a
//! flat floor with no obstruction, one tick from rest reduces to:
//!
//! ```text
//! Δz = 0.98F(widened) * movement_speed
//! ```
//!
//! — vanilla's analog-input scaling (its own "modify input speed for square
//! movement" step's `strafe * 0.98F` / `forward * 0.98F`, unchanged by the
//! unit-square normalization for a single-axis input whose squared length is
//! already `<= 1.0`) times the speed vanilla's own relative-move step /
//! input-vector accessor scales by. This
//! is exactly what the pure control below asserts against, for two different
//! `movement_speed` values, with no reliance on the crate's own maths to
//! produce the expected side.

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::Aabb;
use lodestone_physics::{MovementInput, PhysicsProfile, PlayerState, Vec3d, tick_air};

/// A single flat floor at `y = 0`, extending far enough that a one-tick walk
/// never reaches its edge — the world's only job is to not get in the way.
struct FlatFloor;

impl CollisionView for FlatFloor {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == 0 {
            out.push(Aabb::new(
                f64::from(x),
                0.0,
                f64::from(z),
                f64::from(x) + 1.0,
                1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
}

/// A standing, grounded player at rest, facing `yaw = 0`, with `movement_speed`
/// injected — the same construction [`golden.rs`]'s `grounded` helper uses,
/// plus the attribute override this test exists to isolate.
fn standing_state(movement_speed: f64) -> PlayerState {
    let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0).with_movement_speed(movement_speed);
    s.on_ground = true;
    s
}

#[test]
fn speed_ii_and_sprint_are_the_only_difference() {
    let world = FlatFloor;
    let profile = PhysicsProfile::mc_1_21();
    let input = MovementInput {
        forward: 1.0,
        ..MovementInput::NONE
    };

    // The player's own movement-speed attribute base (`0.1F` — not the
    // generic ranged-attribute default of `0.7`).
    let base_speed = f64::from(profile.base_movement_speed);
    // The widening is observable (0.1f64 != f64::from(0.1f32), same footgun
    // `effect.rs`'s `speed_modifier_is_widened_float_times_level` pins), so
    // compare against the widened f32 bit pattern, not the f64 literal.
    assert_eq!(
        base_speed.to_bits(),
        f64::from(0.1f32).to_bits(),
        "profile.base_movement_speed must match vanilla's own 0.1F"
    );

    // Vanilla's own speed mob-effect definition: movement-speed `+0.2F`,
    // added as an "add multiplied total" modifier. Amount is
    // `base * (amplifier + 1)` in the widened `f64` (vanilla's own
    // attribute-modifier construction); amplifier 1 = Speed II.
    let speed_ii_amount = f64::from(0.2f32) * 2.0;
    // Vanilla's own attribute-value-calculation step's multiplicative stage:
    // result = base * (1 + amount). This is the value the entity layer would fold and hand to
    // PlayerState::with_movement_speed — computed here from the jar constants
    // directly, not via lodestone_entity::attribute's own fold.
    let speed_ii = base_speed * (1.0 + speed_ii_amount);
    assert!(speed_ii > base_speed, "Speed II must walk faster than base");

    // Sprinting (vanilla's own sprinting speed-modifier constant: `+0.3F`
    // added as an "add multiplied total" modifier) folded onto the *same* base the way
    // `player_physics`'s scoped fix combines the attribute-derived base with
    // the existing sprint arithmetic (`docs/swimming.md`): base * (1 + 0.3).
    let sprint_amount = 0.3;
    let sprinting = base_speed * (1.0 + sprint_amount);

    let run = |movement_speed: f64| -> PlayerState {
        let mut s = standing_state(movement_speed);
        tick_air(&mut s, input, &world, &profile);
        s
    };

    let base_state = run(base_speed);
    let speed_ii_state = run(speed_ii);
    let sprint_state = run(sprinting);

    // Pure control: three different `movement_speed` values, everything else
    // held fixed, must produce three different positions — attributable to
    // `movement_speed` alone.
    assert_ne!(
        base_state.position.z, speed_ii_state.position.z,
        "Speed II must move the player further than the base speed in one tick"
    );
    assert_ne!(
        base_state.position.z, sprint_state.position.z,
        "the sprint-folded speed must differ from the base speed too"
    );
    assert_ne!(
        speed_ii_state.position.z, sprint_state.position.z,
        "Speed II (0.4 multiplier) and sprint (0.3 multiplier) must not coincide"
    );

    // Hand-derived expectation for each arm, per this file's module doc: at
    // yaw 0 on default (0.6F) terrain friction, one tick from rest is
    // `Δz = 0.98F(widened) * movement_speed`. `movement_speed` here is the
    // double-precision attribute-instance value; the `(float)` cast happens
    // where vanilla's own per-tick AI step does it — its own speed field is
    // set to its own resolved attribute value cast to `float`, reproduced by
    // `effective_speed`'s `v as f32` — so the hand-derived side must apply
    // that same truncation, not multiply the full f64 precision through.
    let scale = f64::from(0.98f32);
    for (label, state, speed) in [
        ("base", &base_state, base_speed),
        ("speed_ii", &speed_ii_state, speed_ii),
        ("sprinting", &sprint_state, sprinting),
    ] {
        let expected_z = 0.5 + scale * f64::from(speed as f32);
        assert!(
            (state.position.z - expected_z).abs() < 1e-12,
            "{label}: got z={} expected z={expected_z} (movement_speed={speed})",
            state.position.z
        );
        // The X axis carries no forward input, so it does not move at all —
        // this pins that the divergence above is on the axis the input
        // actually drives, not an artefact of some other field.
        assert_eq!(
            state.position.x, 0.5,
            "{label}: no strafe input, x must not move"
        );
    }
}
