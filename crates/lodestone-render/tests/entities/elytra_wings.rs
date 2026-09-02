//! The elytra's geometry and pose, against `ElytraModel`/`ElytraAnimationState`.
//!
//! # What these gates cover, and what they do not
//!
//! These are **hermetic CPU gates over the mesh and the pose maths**. They
//! prove the wings bake on the right sheet, that the right wing is the left
//! one mirrored, and that the animation state's three-way branch lands on the
//! right triple. They rasterise nothing and they touch no producer.
//!
//! They therefore say nothing at all about whether an elytra reaches pixels
//! in play. That is
//! `crates/lodestone-shell/tests/elytra_wings_pixels.rs`, which drives the
//! shell's `prepare_elytra` and the real draw loop; read a pass *here* as
//! "the geometry and the pose are right", never as "the feature works".
//!
//! Every expected value below is derived from the 26.2 record definitions
//! (see `docs/elytra-rendering.md` for the citations), never from our own
//! output.

use glam::{Mat4, Vec3};
use lodestone_render::{ElytraMesh, ElytraWing, elytra_target_rotations, elytra_wing_transform};

/// `ElytraModel.createLayer` declares `LayerDefinition.create(mesh, 64, 32)`.
///
/// A 64x64 assumption — the size the *player* sheet uses, and the size the
/// cape model declares — halves every V and paints the wings with whatever
/// sits in the top half of the strip. The discriminating quantity is the
/// unwrap's V extent, which is predicted from outside: one `texOffs(22, 0)`
/// box of size 10x20x2 lays its rows out depth-then-height, so it spans
/// `2 + 20 = 22` texels of V. On a 64x**32** sheet that is `22/32 = 0.6875`;
/// on 64x64 it would be `22/64 = 0.34375`. Both hypotheses are computed here
/// and the measurement must land on one.
///
/// Inflation is deliberately not in that arithmetic: `CubeDeformation` grows
/// the *geometry* and leaves the unwrap alone.
#[test]
fn wings_unwrap_onto_a_sixty_four_by_thirty_two_sheet() {
    let def = lodestone_assets::entity::elytra_model();
    assert_eq!(
        (def.texture_width, def.texture_height),
        (64, 32),
        "ElytraModel.createLayer declares a 64x32 sheet"
    );

    let mesh = ElytraMesh::load();
    assert!(!mesh.vertices.is_empty(), "the elytra mesh baked no vertices");
    let max_v = mesh
        .vertices
        .iter()
        .map(|v| v.uv[1])
        .fold(f32::NEG_INFINITY, f32::max);

    let on_64x32 = 22.0 / 32.0;
    let on_64x64 = 22.0 / 64.0;
    let d32 = (max_v - on_64x32).abs();
    let d64 = (max_v - on_64x64).abs();
    assert!(
        d32 < 1.0e-5,
        "the wing unwrap's max V is {max_v}; the 64x32 hypothesis predicts {on_64x32} \
         (off by {d32}) and the 64x64 hypothesis predicts {on_64x64} (off by {d64})"
    );
}

/// Both wings bake, in a fixed order, and neither is empty.
///
/// A wing that silently baked nothing would leave the other one drawing
/// alone, which reads as "the elytra is half missing" and is exactly the
/// failure the `!p.quads.is_empty()` filter in `ElytraMesh::load` can produce
/// if a part is renamed.
#[test]
fn both_wings_bake_with_geometry() {
    let mesh = ElytraMesh::load();
    let sides: Vec<ElytraWing> = mesh.parts.iter().map(|(w, _)| *w).collect();
    assert_eq!(
        sides,
        vec![ElytraWing::Left, ElytraWing::Right],
        "both wings must bake, left first"
    );
    for (wing, range) in &mesh.parts {
        assert!(
            range.index_count > 0 && range.vertex_count > 0,
            "{wing:?} baked an empty range"
        );
    }
}

/// Mirror the X axis.
fn flip_x() -> Mat4 {
    Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0))
}

fn max_abs_diff(a: Mat4, b: Mat4) -> f32 {
    a.to_cols_array()
        .iter()
        .zip(b.to_cols_array().iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// The right wing is the left wing reflected through the `x = 0` plane.
///
/// # Why this and not "the right wing negates yRot and zRot"
///
/// That restates the implementation. The reflection identity comes from
/// somewhere else entirely — the model's own symmetry — and it is what makes
/// vanilla's three-of-five sign choices *predictable* rather than
/// transcribed. Writing `S = diag(-1, 1, 1)`:
///
/// ```text
/// S * T(+5/16, y, 0) * S = T(-5/16, y, 0)     the pivot flips
/// S * Rz(z)         * S = Rz(-z)              Rz mixes x and y
/// S * Ry(y)         * S = Ry(-y)              Ry mixes x and z
/// S * Rx(x)         * S = Rx(x)               Rx mixes y and z only
/// S * T(0, 0, 0.125)* S = T(0, 0, 0.125)      Z-only, untouched
/// ```
///
/// so `right == S * left * S` exactly, and it predicts that `xRot` is the one
/// angle that must **not** flip. `ElytraModel.setupAnim` agrees
/// (`rightWing.xRot = leftWing.xRot`, `yRot`/`zRot` negated), which is the
/// point: two independent derivations of the same five signs.
///
/// The angles are **pairwise distinct and all non-zero** so that a
/// transposition or a dropped negation cannot survive — with `y_rot = 0`, the
/// value every non-crouching, non-gliding wearer actually has, a missing
/// Y negation is invisible.
#[test]
fn right_wing_is_the_left_wing_mirrored_through_x() {
    let s = flip_x();
    // Deliberately not the rest pose: its y_rot is 0, which makes one of the
    // two negations unobservable.
    let (x_rot, y_rot, z_rot) = (0.31, 0.17, -0.43);
    let mut failures = Vec::new();
    for crouching in [false, true] {
        let left = elytra_wing_transform(ElytraWing::Left, x_rot, y_rot, z_rot, crouching);
        let right = elytra_wing_transform(ElytraWing::Right, x_rot, y_rot, z_rot, crouching);
        let predicted = s * left * s;
        let d = max_abs_diff(right, predicted);
        if d > 1.0e-6 {
            failures.push(format!("crouching={crouching}: off by {d}"));
        }
        // The control the identity itself cannot supply: a "mirror
        // everything" right wing, which also negates xRot. If that satisfied
        // the identity too, the identity would not be discriminating.
        let over_mirrored =
            elytra_wing_transform(ElytraWing::Left, -x_rot, -y_rot, -z_rot, crouching);
        let over_mirrored = Mat4::from_translation(Vec3::new(-10.0 / 16.0, 0.0, 0.0)) * over_mirrored;
        if max_abs_diff(over_mirrored, predicted) <= 1.0e-6 {
            failures.push(format!(
                "crouching={crouching}: an xRot-negating wing satisfies the mirror identity too, \
                 so the identity proves nothing"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// Crouching lifts **both** wings by 3 model texels, and lifts nothing else.
///
/// `setupAnim` assigns `y` and leaves `x` and `z` alone, so the difference
/// between the crouched and un-crouched transform must be a pure `+3/16`
/// translation on Y — predicted, not merely "non-zero".
#[test]
fn crouching_raises_both_wings_by_three_texels_and_nothing_else() {
    let (x_rot, y_rot, z_rot) = (0.31, 0.17, -0.43);
    let expected_shift = Mat4::from_translation(Vec3::new(0.0, 3.0 / 16.0, 0.0));
    let mut failures = Vec::new();
    for wing in [ElytraWing::Left, ElytraWing::Right] {
        let standing = elytra_wing_transform(wing, x_rot, y_rot, z_rot, false);
        let crouched = elytra_wing_transform(wing, x_rot, y_rot, z_rot, true);
        // The crouch `y` sits inside the pivot translate, which is composed
        // to the left of every rotation, so the whole transform shifts.
        let predicted = expected_shift * standing;
        let d = max_abs_diff(crouched, predicted);
        if d > 1.0e-6 {
            failures.push(format!("{wing:?}: off by {d}"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// `ElytraAnimationState.tick`'s three-way branch.
///
/// # Choosing inputs that discriminate
///
/// Two coincidences in this function would make a careless fixture vacuous,
/// and both are avoided deliberately:
///
/// * **A vertical dive returns the resting triple.** `motion = (0, -1, 0)`
///   normalises to `y = -1`, so `ratio = 1 - 1^1.5 = 0` and both lerps return
///   their `start` — which is exactly `(PI/12, 0, -PI/12)`, the not-flying
///   branch's answer. The steepest possible glide and standing still produce
///   *identical* angles, so a gate that dives straight down cannot tell the
///   fall-flying branch from the fallback.
/// * **Level flight is the other endpoint.** `motion.y >= 0` short-circuits
///   `ratio` to 1, so both lerps return their `end`.
///
/// The interesting input is therefore a *shallow* dive, where `ratio` is
/// strictly between 0 and 1 and every constant in the branch contributes.
/// `motion = (0.5, -0.5, 0.0)` normalises to `y = -1/sqrt(2)`, giving
/// `ratio = 1 - (1/sqrt(2))^1.5`, which is `0.4052...` — a number no other
/// branch can produce.
#[test]
fn target_rotations_take_the_right_branch() {
    use std::f32::consts::PI;
    let sqrt_half = (0.5f64).sqrt();
    let ratio = (1.0 - sqrt_half.powf(1.5)) as f32;
    let lerp = |start: f32, end: f32| start + ratio * (end - start);

    let cases: Vec<(&str, (bool, bool, Vec3), (f32, f32, f32))> = vec![
        (
            "standing",
            (false, false, Vec3::ZERO),
            (PI / 12.0, 0.0, -PI / 12.0),
        ),
        (
            "crouching",
            (false, true, Vec3::ZERO),
            (PI * 2.0 / 9.0, 0.08726646, -PI / 4.0),
        ),
        (
            "gliding level",
            (true, false, Vec3::new(0.9, 0.0, 0.0)),
            (PI / 9.0, 0.0, -PI / 2.0),
        ),
        (
            "gliding in a shallow dive",
            (true, false, Vec3::new(0.5, -0.5, 0.0)),
            (lerp(PI / 12.0, PI / 9.0), 0.0, lerp(-PI / 12.0, -PI / 2.0)),
        ),
        // Precedence: `isFallFlying()` is the first branch, and a player can
        // be crouching and gliding at once. The expected triple is the level
        // glide's, not the crouch's, and the two share no component.
        (
            "gliding while crouching",
            (true, true, Vec3::new(0.9, 0.0, 0.0)),
            (PI / 9.0, 0.0, -PI / 2.0),
        ),
    ];

    // Collected, not asserted in the loop: a wrongly-ordered branch fails
    // several of these at once, and an assert inside the loop would report
    // only the first and leave the rest as arguments rather than
    // observations.
    let mut failures = Vec::new();
    for (name, (flying, crouching, motion), want) in cases {
        let got = elytra_target_rotations(flying, crouching, motion);
        let d = (got.0 - want.0)
            .abs()
            .max((got.1 - want.1).abs())
            .max((got.2 - want.2).abs());
        if d > 1.0e-6 {
            failures.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// The shallow-dive ratio is not 0 and not 1 — the property the case above
/// depends on, asserted rather than assumed.
///
/// Without this, "gliding in a shallow dive" could silently degenerate into a
/// duplicate of one of the two endpoint cases and the suite would still be
/// green.
#[test]
fn a_shallow_dive_lands_strictly_between_the_two_glide_endpoints() {
    use std::f32::consts::PI;
    let (x, _, z) = elytra_target_rotations(true, false, Vec3::new(0.5, -0.5, 0.0));
    let (rest_x, rest_z) = (PI / 12.0, -PI / 12.0);
    let (full_x, full_z) = (PI / 9.0, -PI / 2.0);
    assert!(
        x > rest_x + 1.0e-4 && x < full_x - 1.0e-4,
        "xRot {x} is not strictly between {rest_x} and {full_x}"
    );
    assert!(
        z < rest_z - 1.0e-4 && z > full_z + 1.0e-4,
        "zRot {z} is not strictly between {rest_z} and {full_z}"
    );
}
