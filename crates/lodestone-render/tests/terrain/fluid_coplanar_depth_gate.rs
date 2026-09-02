//! Does the fluid pass stay behind a coplanar block face at every view distance?
//!
//! # What it is
//!
//! `bake_fluid` insets every fluid side face 0.001 blocks off its block
//! boundary, exactly where vanilla's `FluidRenderer.tesselate` does, so that the
//! water face on a **partially** covered side sits behind the block's own
//! coplanar face and loses the depth test cleanly instead of fighting it. The
//! discriminating case is a waterlogged stair's front: the stair fills only the
//! bottom half of that square, so `FluidRenderer.isFaceOccludedBySelf` correctly
//! declines to cull the water face — vanilla emits it too — and the bottom half
//! is then two coplanar surfaces from two different passes.
//!
//! That inset is a **world-space** distance, and vanilla spends it in a
//! reversed-Z depth buffer where relative precision barely changes with
//! distance. This renderer's projection is now reversed-Z too (see
//! `lodestone_render::Camera::projection_matrix`), and this file is the
//! measurement of what that is worth: how many float32 **ULPs** of depth
//! separation the fluid pass actually has as a function of camera distance, held
//! to a floor and to a **predicted magnitude**.
//!
//! A separation of **zero** ULPs is not "slightly worse": the two surfaces
//! become bit-identical in depth, so every `depth_compare` decision at that
//! pixel falls to whatever rounding the rasterizer's interpolation happens to
//! produce for two *differently shaped* coplanar quads, and the winner changes
//! as the camera moves. That is the z-fight.
//!
//! # The before column, and why it is in this file
//!
//! This renderer used to project **forward** `[0,1]`, and under that projection
//! the same inset was worth 210 ULP at 2 blocks, 4 at 16, 1 at 32, **0 at 64 and
//! -1 at 128** — it collapsed and then inverted. `shaders/fluid.wgsl` carried a
//! constant `2^-21` window-depth nudge to pay that back, and this file was its
//! gate.
//!
//! Both are gone. The nudge is deleted (`model_pipeline`'s
//! `the_fluid_shader_adjusts_no_depth` is the guard), and the forward projection
//! survives here only as [`forward_window_depth_with_far`] — transcribed from
//! the standard DirectX right-handed perspective rather than from anything in
//! this crate — where it serves as the **observed-failing control**: the
//! detector must still be able to see a collapse, and the passing sweep below
//! must be reversed-Z doing work rather than a depth buffer that was fine all
//! along.
//!
//! # Where the expected values come from
//!
//! Outside this renderer entirely:
//!
//! * The inset is read out of [`bake_fluid`]'s real emitted geometry, not
//!   restated as a literal — the same expression the draw uses.
//! * The depth values come from the real [`Camera::view_projection`].
//! * The **unit** is IEEE-754 float32 ULP spacing, obtained by differencing
//!   `f32::to_bits`. That is a property of the format, not of this code, and
//!   [`ulp_gap_is_a_real_ulp_count`] pins it against `f32::EPSILON`.
//! * The **magnitude** is predicted before it is measured, in
//!   [`predicted_ulp_bracket`], by differentiating the reversed-Z depth function
//!   on paper and combining it with IEEE-754's per-binade ULP window. Asserting
//!   only "it improved" is the vacuous shape this file exists not to be.
//! * Which direction "behind" is in the depth buffer is **derived** from the
//!   projection ([`behind_is_smaller_depth`]) rather than written down, because
//!   that is precisely the fact a reversed-Z conversion changes.
//!
//! ```text
//! cargo test -p lodestone-render --test fluid_coplanar_depth_gate -- --nocapture
//! ```

use lodestone_assets::fluid::{FaceSet, FluidGeometry, SideOverlay, SpriteUv, bake_fluid};
use lodestone_render::Camera;

/// A sprite rect that maps unit UV straight through, so the emitted quads carry
/// their positions untouched by atlas layout.
fn unit_uv() -> SpriteUv {
    SpriteUv {
        min: [0.0, 0.0],
        max: [1.0, 1.0],
        anim: 0,
    }
}

/// A full-height still fluid cell emitting **only** its north (`-Z`) side face —
/// the face a waterlogged stair's front presents.
fn north_face_only() -> FluidGeometry {
    FluidGeometry {
        corners: [1.0; 4],
        flow: [0.0, 0.0],
        faces: FaceSet {
            up: false,
            down: false,
            north: true,
            south: false,
            east: false,
            west: false,
        },
        tint_index: Some(0),
        back_up_face: false,
        side_overlay: SideOverlay::default(),
    }
}

/// The `-Z` coordinate `bake_fluid` actually emits for a north side face, read
/// out of the real bake rather than restated from the constant. This is the
/// whole quantity under test.
fn baked_north_inset() -> f32 {
    let quads = bake_fluid(&north_face_only(), unit_uv(), unit_uv(), None);
    assert!(
        !quads.is_empty(),
        "precondition: bake_fluid emitted no north side face, so this file \
         measures nothing"
    );
    let z = quads[0].positions[0][2];
    for quad in &quads {
        for p in &quad.positions {
            assert_eq!(
                p[2], z,
                "precondition: the north side face is not planar in Z, so there \
                 is no single inset to measure"
            );
        }
    }
    assert!(
        z > 0.0,
        "precondition: bake_fluid emitted the north face flush with the block \
         boundary (z = {z}); this gate's subject does not exist"
    );
    z
}

/// The float32 ULP distance between two same-signed positive depth values.
///
/// Both operands come out of a `[0,1]` depth transform, so they are finite,
/// non-negative, and share a sign — the regime where the bit patterns of `f32`
/// are monotonic and their integer difference *is* the count of representable
/// values between them.
fn ulp_gap(a: f32, b: f32) -> i64 {
    assert!(a.is_finite() && b.is_finite(), "non-finite depth: {a}, {b}");
    assert!(a >= 0.0 && b >= 0.0, "negative depth: {a}, {b}");
    i64::from(b.to_bits()) - i64::from(a.to_bits())
}

/// Control for the *unit*: the ULP difference between adjacent representable
/// floats must be exactly 1, and `1.0 + EPSILON` must sit one ULP above the
/// float just below it. Pins `ulp_gap` against IEEE-754 itself.
#[test]
fn ulp_gap_is_a_real_ulp_count() {
    // `f32::EPSILON` is the gap between 1.0 and the next float up, by definition.
    assert_eq!(ulp_gap(1.0, 1.0 + f32::EPSILON), 1);
    assert_eq!(ulp_gap(1.0, 1.0), 0);
    // Two ULPs up from 0.5, and the same gap measured downward.
    let half_up_2 = f32::from_bits(0.5f32.to_bits() + 2);
    assert_eq!(ulp_gap(0.5, half_up_2), 2);
    assert_eq!(ulp_gap(half_up_2, 0.5), -2);
    // Spacing genuinely differs by exponent: the absolute step at 0.5 is half
    // the step at 1.0. Under a *forward* `[0,1]` projection that is what lost
    // far-plane precision; under reversed-Z it is what buys it, because depth
    // falls through the exponents as distance grows.
    assert!((half_up_2 - 0.5) < (2.0 * f32::EPSILON));
}

/// A camera `distance` blocks away on the `-Z` side, looking down `+Z`, so the
/// face plane at `z = local_z` is squarely ahead.
fn camera_at(distance: f32, far: f32) -> Camera {
    let mut camera = Camera::default();
    // Yaw is Minecraft's: 0 faces +Z.
    camera.position = glam::Vec3::new(0.5, 0.5, -distance);
    camera.yaw = 0.0;
    camera.pitch = 0.0;
    camera.far = far;
    camera
}

/// Window-space depth (`z / w`) of a point at world `(0.5, 0.5, local_z)` seen
/// from [`camera_at`].
///
/// The transform is the production [`Camera::view_projection`], evaluated in the
/// same `f32` the shader uses.
fn window_depth_with_far(distance: f32, local_z: f32, far: f32) -> f32 {
    let camera = camera_at(distance, far);
    let clip = camera.view_projection() * glam::Vec4::new(0.5, 0.5, local_z, 1.0);
    clip.z / clip.w
}

/// The **forward** `[0,1]` projection this renderer used to carry, for the
/// before column and the observed-failing control.
///
/// Transcribed from the standard DirectX right-handed perspective — `zz =
/// -far/(far - near)`, `tz = -near·far/(far - near)`, `zw = -1` — rather than
/// from anything in `lodestone-render`, so it stays a genuine outside reference
/// after the production projection changed out from under it. The view half is
/// the real [`Camera::view_matrix`], which reversed-Z does not touch.
fn forward_window_depth_with_far(distance: f32, local_z: f32, far: f32) -> f32 {
    let camera = camera_at(distance, far);
    let h = 1.0 / (0.5 * camera.fov_y_degrees.to_radians()).tan();
    let z_range_inv = 1.0 / (far - camera.near);
    let projection = glam::Mat4::from_cols(
        glam::Vec4::new(h / camera.aspect, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, h, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, -far * z_range_inv, -1.0),
        glam::Vec4::new(0.0, 0.0, -camera.near * far * z_range_inv, 0.0),
    );
    let clip = (projection * camera.view_matrix()) * glam::Vec4::new(0.5, 0.5, local_z, 1.0);
    clip.z / clip.w
}

/// Control for the reference above: it really is the forward projection, i.e.
/// the near plane maps to `0` and the far plane to `1` — the opposite of what
/// the production projection now does.
///
/// Without this the "before" column could be a transcription slip, and every
/// improvement this file reports would be measured against a fiction.
#[test]
fn control_the_forward_reference_is_the_opposite_convention() {
    let far = Camera::far_for_render_distance(12, 0);
    let camera = camera_at(8.0, far);
    // A point at the near plane, and one at the far plane, in this fixture's
    // straight-ahead geometry: the camera sits at z = -8 looking down +Z.
    let near_plane_z = -8.0 + camera.near;
    let far_plane_z = -8.0 + far;
    let fwd_near = forward_window_depth_with_far(8.0, near_plane_z, far);
    let fwd_far = forward_window_depth_with_far(8.0, far_plane_z, far);
    assert!(fwd_near.abs() < 1e-3, "forward near must be ~0, got {fwd_near}");
    assert!(
        (fwd_far - 1.0).abs() < 1e-3,
        "forward far must be ~1, got {fwd_far}"
    );
    // And production is the other way round, which is what makes the two arms
    // of this file different measurements rather than the same one twice.
    let rev_near = window_depth_with_far(8.0, near_plane_z, far);
    let rev_far = window_depth_with_far(8.0, far_plane_z, far);
    assert!(
        (rev_near - 1.0).abs() < 1e-3,
        "production near must be ~1, got {rev_near}"
    );
    assert!(rev_far.abs() < 1e-3, "production far must be ~0, got {rev_far}");
}

/// Which way a surface further from the eye moves in the depth buffer, asked of
/// the real projection rather than assumed.
///
/// The fluid face must end up **behind** its block boundary, and "behind" is
/// smaller depth under reversed-Z and larger depth under a forward projection.
/// Deriving it means a future projection change fails this file loudly instead
/// of silently inverting every separation it reports.
fn behind_is_smaller_depth() -> bool {
    let far = Camera::far_for_render_distance(12, 0);
    let near_surface = window_depth_with_far(8.0, 0.0, far);
    let far_surface = window_depth_with_far(8.0, 1.0, far);
    assert_ne!(
        near_surface, far_surface,
        "premise: two surfaces a block apart have identical depth, so this file \
         cannot tell which direction 'behind' is"
    );
    far_surface < near_surface
}

/// How many representable depth values the fluid face sits **behind** the block
/// boundary. Positive is correct; zero is a tie; negative means the water has
/// come out in front.
fn ulps_behind(boundary: f32, water: f32) -> i64 {
    if behind_is_smaller_depth() {
        ulp_gap(water, boundary)
    } else {
        ulp_gap(boundary, water)
    }
}

/// The forward-projection counterpart of [`ulps_behind`], for the control arms.
fn forward_ulps_behind(boundary: f32, water: f32) -> i64 {
    // The forward reference maps far to 1, so "behind" is the larger depth.
    ulp_gap(boundary, water)
}

/// One `(camera distance, far plane)` pair to measure at.
///
/// The far plane is part of the sample rather than a constant because a render
/// distance of 32 chunks pushes it to 2048 and lets a player see water 512 blocks
/// away — the regime where the separation is thinnest. Every pair below comes
/// from [`Camera::far_for_render_distance`] with a distance that render distance
/// can actually *show*, which is the chunk radius (32 chunks = 512 blocks),
/// **not** the far plane.
struct Sample {
    distance: f32,
    far: f32,
}

fn samples() -> Vec<Sample> {
    let rd12 = Camera::far_for_render_distance(12, 0);
    let rd32 = Camera::far_for_render_distance(32, 0);
    let mut out = Vec::new();
    // Arm's length out to the edge of a 12-chunk render distance (192 blocks).
    for distance in [2.0, 8.0, 16.0, 24.0, 32.0, 64.0, 128.0, 192.0] {
        out.push(Sample { distance, far: rd12 });
    }
    // And the long-range regime a 32-chunk render distance opens up, out to its
    // own 512-block radius.
    for distance in [256.0, 512.0] {
        out.push(Sample { distance, far: rd32 });
    }
    out
}

/// Window depth at the render-distance-12 far plane — the sweep's default, used
/// by the controls, which are about the near/mid regime.
fn window_depth(distance: f32, local_z: f32) -> f32 {
    window_depth_with_far(distance, local_z, Camera::far_for_render_distance(12, 0))
}

/// The floor on the fluid pass's separation from a coplanar opaque face.
///
/// One ULP is *not* enough: a rasterizer interpolating two differently shaped
/// coplanar quads — the water face spans the whole square, the stair's own face
/// only its bottom half — can round either way by a ULP at a given pixel, and
/// that is the flicker.
const MIN_ULPS: i64 = 4;

/// The ULP separation a clearance of `c` blocks **must** produce at `distance`
/// under the shipped reversed-Z projection, as an inclusive `(low, high)`
/// bracket — derived on paper, before any measurement.
///
/// Reversed-Z window depth for a surface `d` blocks ahead is
///
/// ```text
/// D(d) = near · (far - d) / ((far - near) · d)
/// ```
///
/// so `dD/dd = -near · far / ((far - near) · d^2)` and the **relative**
/// separation two surfaces `c` apart have is
///
/// ```text
/// |dD| / D = c · far / (d · (far - d))
/// ```
///
/// IEEE-754 supplies the other half: for any positive float, `value / ulp(value)`
/// lies in `[2^23, 2^24)`, because the spacing is fixed within a binade and the
/// value ranges over a factor of two inside it. Multiplying the two gives the
/// bracket, and the factor-of-two width is the binade sawtooth rather than
/// slack — a measurement is expected to sit anywhere inside it.
///
/// Nothing here reads the projection matrix, so the bracket is an independent
/// prediction and not a restatement. The 2% margin absorbs the first-order
/// truncation in `dD/dd` and `f32` rounding, and is far narrower than the
/// factor-of-two the bracket itself spans.
fn predicted_ulp_bracket(distance: f32, clearance: f32, far: f32) -> (i64, i64) {
    let relative = f64::from(clearance) * f64::from(far)
        / (f64::from(distance) * (f64::from(far) - f64::from(distance)));
    let low = relative * f64::from(1u32 << 23) * 0.98;
    let high = relative * f64::from(1u32 << 24) * 1.02;
    (low.floor() as i64, high.ceil() as i64)
}

/// The measurement, collected across every distance and reported as a table.
///
/// Mismatches are collected rather than asserted inside the loop, so a failure
/// prints the whole distance sweep and names the distance at which the
/// separation stops holding — and so that a run reports *every* arm rather than
/// only the first that failed.
///
/// Three claims per row, and the second is the one that makes this not a
/// direction-only test: the separation clears [`MIN_ULPS`], it lands inside the
/// bracket [`predicted_ulp_bracket`] computed from the projection's algebra, and
/// the forward projection this replaced is reported alongside it.
#[test]
fn fluid_faces_stay_behind_a_coplanar_block_face_at_every_view_distance() {
    let inset = baked_north_inset();
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for Sample { distance: d, far } in samples() {
        let boundary = window_depth_with_far(d, 0.0, far);
        let water = window_depth_with_far(d, inset, far);
        let gap = ulps_behind(boundary, water);
        let (low, high) = predicted_ulp_bracket(d, inset, far);
        let before = forward_ulps_behind(
            forward_window_depth_with_far(d, 0.0, far),
            forward_window_depth_with_far(d, inset, far),
        );
        rows.push(format!(
            "  d = {d:>6} blocks (far {far:>6}): gap = {gap:>6} ULP \
             (predicted {low}..={high}; the forward projection this replaced: {before})"
        ));
        if gap < MIN_ULPS {
            failures.push(format!(
                "at {d} blocks the fluid pass sits only {gap} ULP behind a \
                 coplanar block face (need >= {MIN_ULPS})"
            ));
        }
        if gap < low || gap > high {
            failures.push(format!(
                "at {d} blocks the separation is {gap} ULP, outside the \
                 {low}..={high} predicted from the reversed-Z depth function — \
                 the projection is not doing what this file's arithmetic says \
                 it does"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the fluid pass does not hold a stable depth separation from a coplanar \
         block face:\n{}\nsweep:\n{}",
        failures.join("\n"),
        rows.join("\n")
    );
    println!("sweep:\n{}", rows.join("\n"));
}

/// The user-visible symptom, stated directly: **the winner must not change as
/// the camera moves.**
///
/// Worth being precise about why this is the right shape of gate, because the
/// obvious one is vacuous. A z-fight here is *not* frame-to-frame
/// nondeterminism — the rasterizer is deterministic, so re-rendering the same
/// scene from the same camera is byte-identical even while the artefact is
/// present, and a repeated-draw determinism gate would pass on the broken code.
/// What actually changes is the *camera*: the depth comparison at a given pixel
/// is decided by a ULP or two, so an inch of movement re-rounds it and the face
/// flips. That is why the report was "swapping rapidly" while moving.
///
/// So this sweeps the distance finely and requires the **separation** to hold at
/// every step.
///
/// Asserting the separation rather than the comparison's *outcome* is the whole
/// point, and the first draft of this gate got it wrong in an instructive way. A
/// sign-flip sweep (`is water ever in front?`) passed on the pre-fix code:
/// whether a collapsed inset rounds to -1, 0 or +1 ULP depends on the exact
/// distance, so an actual inversion is sparse and a 0.31-block step walks
/// straight past it. The measured defect is not that the water wins somewhere,
/// it is that the margin is **thin enough for rounding to decide**.
///
/// The step is deliberately not a round number of blocks: a regular step can
/// land on a lattice that misses where the gap is thinnest.
#[test]
fn the_depth_separation_never_collapses_as_the_camera_moves() {
    let inset = baked_north_inset();
    let far = Camera::far_for_render_distance(12, 0);
    let mut thin = Vec::new();
    let mut steps = 0usize;
    let mut worst = i64::MAX;
    let mut worst_at = 0.0_f32;
    let mut d = 1.0_f32;
    while d <= 192.0 {
        let gap = ulps_behind(
            window_depth_with_far(d, 0.0, far),
            window_depth_with_far(d, inset, far),
        );
        steps += 1;
        if gap < worst {
            worst = gap;
            worst_at = d;
        }
        if gap < MIN_ULPS {
            thin.push(format!("  d = {d:.4}: gap {gap} ULP"));
        }
        d += 0.3137;
    }
    assert!(
        steps > 500,
        "the sweep must be fine enough to catch a narrow band; only {steps} \
         samples were taken"
    );
    println!("  thinnest separation over {steps} samples: {worst} ULP at d = {worst_at:.4}");
    assert!(
        thin.is_empty(),
        "the fluid pass's separation from a coplanar block face drops below \
         {MIN_ULPS} ULP at {} of {steps} sampled distances, so at those camera \
         positions the depth test is decided by rounding and the face flips as \
         the camera moves:\n{}",
        thin.len(),
        thin.join("\n")
    );
}

/// Control for the sweep above, **observed failing on the projection this
/// replaced**: run the identical sweep through
/// [`forward_window_depth_with_far`] and require it to find distances where the
/// separation is below the floor.
///
/// Without this the sweep above could be passing on a depth buffer that never
/// had a problem, and the whole reversed-Z conversion would be unevidenced.
#[test]
fn control_the_forward_projection_does_collapse_as_the_camera_moves() {
    let inset = baked_north_inset();
    let far = Camera::far_for_render_distance(12, 0);
    let mut thin = 0usize;
    let mut steps = 0usize;
    let mut d = 1.0_f32;
    while d <= 192.0 {
        let gap = forward_ulps_behind(
            forward_window_depth_with_far(d, 0.0, far),
            forward_window_depth_with_far(d, inset, far),
        );
        steps += 1;
        if gap < MIN_ULPS {
            thin += 1;
        }
        d += 0.3137;
    }
    assert!(
        thin > 0,
        "control failed: under the forward projection the separation never \
         dropped below {MIN_ULPS} ULP anywhere in 1..192 blocks, so the sweep \
         above is not sensitive to the defect it exists to catch"
    );
    println!(
        "  control: the forward projection falls below {MIN_ULPS} ULP at {thin} \
         of {steps} sampled distances"
    );
}

/// Control: the same measurement with **no inset at all** must report zero ULPs
/// at every distance.
///
/// Both halves matter. Zero proves the two surfaces really are coplanar in the
/// fixture, so the gate above is measuring the separation rather than an
/// accidental offset; and it proves the detector can report zero at all, which
/// the passing gate above never demonstrates.
#[test]
fn control_a_truly_coplanar_pair_measures_zero_ulps() {
    let mut nonzero = Vec::new();
    for Sample { distance: d, far } in samples() {
        let gap = ulps_behind(
            window_depth_with_far(d, 0.0, far),
            window_depth_with_far(d, 0.0, far),
        );
        if gap != 0 {
            nonzero.push(format!("d = {d} (far {far}): gap = {gap} ULP"));
        }
    }
    assert!(
        nonzero.is_empty(),
        "control failed: two identical depths differed, so the ULP measurement \
         above is not measuring the separation:\n{}",
        nonzero.join("\n")
    );
}

/// The before/after table, pinned as numbers rather than as a direction.
///
/// The forward figures are this repo's own recorded measurement of the state it
/// shipped in — 209 ULP at 2 blocks, an exact **tie at 64** and an **inversion
/// at 128** — and they are asserted exactly, so a change to the reference
/// transcription fails here rather than quietly re-baselining what "before"
/// means. The reversed figures are asserted against
/// [`predicted_ulp_bracket`]'s paper arithmetic.
///
/// The last claim is the discriminating one: at 64 blocks the two hypotheses
/// must not merely differ in size but be separated by orders of magnitude, so no
/// tolerance on either could let the wrong projection pass this file.
#[test]
fn the_reversed_projection_is_what_makes_the_inset_resolvable() {
    let inset = baked_north_inset();
    let forward_at = |d: f32| {
        forward_ulps_behind(
            forward_window_depth_with_far(d, 0.0, Camera::far_for_render_distance(12, 0)),
            forward_window_depth_with_far(d, inset, Camera::far_for_render_distance(12, 0)),
        )
    };
    let reversed_at = |d: f32| ulps_behind(window_depth(d, 0.0), window_depth(d, inset));

    // The forward projection: ample close up, an exact tie at 64, inverted at
    // 128. These are the recorded numbers, not a range.
    assert_eq!(
        forward_at(2.0),
        210,
        "the forward reference no longer reproduces the recorded 210 ULP at 2 \
         blocks, so it is not the projection this renderer used to have"
    );
    assert_eq!(
        forward_at(64.0),
        0,
        "the forward reference must tie exactly at 64 blocks"
    );
    assert!(
        forward_at(128.0) < 0,
        "the forward reference must be inverted at 128 blocks, got {}",
        forward_at(128.0)
    );

    // Reversed-Z, at the same three distances, each inside its own prediction.
    let far = Camera::far_for_render_distance(12, 0);
    for d in [2.0_f32, 64.0, 128.0] {
        let (low, high) = predicted_ulp_bracket(d, inset, far);
        let measured = reversed_at(d);
        assert!(
            (low..=high).contains(&measured),
            "at {d} blocks the reversed-Z separation is {measured} ULP, outside \
             the predicted {low}..={high}"
        );
    }

    // And the two hypotheses are **disjoint**, which is the claim that makes
    // this a discriminating test rather than a tolerance: at every sampled
    // distance the forward projection's measurement falls outside the bracket
    // predicted for the reversed one, so no slack on either could let a
    // half-converted projection pass this file. Stated as a non-overlap rather
    // than as a ratio, because a ratio here would be a threshold fitted to the
    // answer.
    for Sample { distance: d, far } in samples() {
        let (low, high) = predicted_ulp_bracket(d, inset, far);
        let before = forward_ulps_behind(
            forward_window_depth_with_far(d, 0.0, far),
            forward_window_depth_with_far(d, inset, far),
        );
        assert!(
            before < low || before > high,
            "at {d} blocks the forward projection measures {before} ULP, which \
             falls inside the {low}..={high} predicted for reversed-Z — the two \
             conventions are not distinguishable at this distance, so a passing \
             sweep proves nothing here"
        );
    }
}
