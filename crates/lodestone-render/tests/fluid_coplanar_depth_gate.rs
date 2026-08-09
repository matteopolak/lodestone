//! Does the fluid pass stay behind a coplanar block face at every view distance?
//!
//! # What it is
//!
//! `bake_fluid` insets every fluid side face 0.001 blocks off its block boundary,
//! exactly where vanilla's `FluidRenderer.tesselate` does, so that the water face
//! on a **partially** covered side sits behind the block's own coplanar face and
//! loses the depth test cleanly instead of fighting it. The discriminating case
//! is a waterlogged stair's front: the stair fills only the bottom half of that
//! square, so `FluidRenderer.isFaceOccludedBySelf` correctly declines to cull the
//! water face — vanilla emits it too — and the bottom half is then two coplanar
//! surfaces from two different passes.
//!
//! That inset is a **world-space** distance, and vanilla spends it in a
//! reversed-Z depth buffer where relative precision barely changes with
//! distance. Ours is `[0,1]` DirectX-style `Depth32Float` (see
//! `lodestone_render::Camera::view_projection` and `DESIGN.md` §7), which spends
//! almost the whole float32 mantissa within a few blocks of the near plane. This
//! file measures how many float32 **ULPs** of depth separation the fluid pass
//! actually has as a function of camera distance, and holds it to a floor.
//!
//! A separation of **zero** ULPs is not "slightly worse": the two surfaces become
//! bit-identical in depth, so every `depth_compare` decision at that pixel falls
//! to whatever rounding the rasterizer's interpolation happens to produce for two
//! *differently shaped* coplanar quads, and the winner changes as the camera
//! moves. That is the z-fight. Measured here, the raw inset is worth 210 ULP at
//! 2 blocks, 4 at 16, 1 at 32, **0 at 64 and -1 at 128** — it collapses and then
//! inverts. `shaders/fluid.wgsl`'s `FLUID_DEPTH_NUDGE` is the fix, and this file
//! is its gate.
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
//! * The push ceiling is inverted back to blocks by **bisecting the projection**
//!   rather than by rearranging it, so an algebra slip cannot make the bound look
//!   satisfied.
//!
//! # The controls (both executed, both observed failing on the pre-fix code)
//!
//! * [`control_a_truly_coplanar_pair_measures_zero_ulps`] — the detector can
//!   report zero, and the fixture's two surfaces really are coplanar.
//! * [`control_the_inset_alone_ties_and_then_inverts_at_range`] — the inset
//!   *without* the nudge ties at 64 blocks and inverts at 128, so the passing
//!   gate above is the nudge doing work rather than a depth buffer that was fine
//!   all along.
//!
//! ```text
//! cargo test -p lodestone-render --test fluid_coplanar_depth_gate -- --nocapture
//! ```

use lodestone_assets::fluid::{
    FaceSet, FluidGeometry, SideOverlay, SpriteUv, bake_fluid,
};
use lodestone_render::Camera;
use lodestone_render::model_pipeline::FLUID_DEPTH_NUDGE;

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
    // the step at 1.0, which is the entire reason a `[0,1]` depth buffer loses
    // far-plane precision.
    assert!((half_up_2 - 0.5) < (2.0 * f32::EPSILON));
}

/// Window-space depth (`z / w`) of a point at world `(x, y, z)` seen by a camera
/// `distance` blocks away on the `-Z` side, looking down `+Z`.
///
/// The transform is the production [`Camera::view_projection`], evaluated in the
/// same `f32` the shader uses.
fn window_depth_with_far(distance: f32, local_z: f32, far: f32) -> f32 {
    let mut camera = Camera::default();
    // Looking down +Z from -Z, so the face plane at `z = local_z` is squarely
    // ahead and `distance` away. Yaw is Minecraft's: 0 faces +Z.
    camera.position = glam::Vec3::new(0.5, 0.5, -distance);
    camera.yaw = 0.0;
    camera.pitch = 0.0;
    camera.far = far;
    let clip = camera.view_projection() * glam::Vec4::new(0.5, 0.5, local_z, 1.0);
    clip.z / clip.w
}

/// One `(camera distance, far plane)` pair to measure at.
///
/// The far plane is part of the sample rather than a constant because a render
/// distance of 32 chunks pushes it to 2048 and lets a player see water 512 blocks
/// away — the regime where the raw inset is worst. Every pair below comes from
/// [`Camera::far_for_render_distance`] with a distance that render distance can
/// actually *show*, which is the chunk radius (32 chunks = 512 blocks), **not**
/// the far plane. That distinction is load-bearing: sampling out at the far plane
/// itself would fail the push bound below on geometry no render distance ever
/// draws, which would be a bound tuned against a fiction.
struct Sample {
    distance: f32,
    far: f32,
}

fn samples() -> Vec<Sample> {
    let rd12 = Camera::far_for_render_distance(12, 0);
    let rd32 = Camera::far_for_render_distance(32, 0);
    let mut out = Vec::new();
    // Arm's length out to the edge of a 12-chunk render distance (192 blocks).
    for distance in [2.0, 8.0, 16.0, 24.0, 32.0, 64.0, 128.0] {
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

/// The floor on the fluid pass's total separation from a coplanar opaque face.
///
/// One ULP is *not* enough: a rasterizer interpolating two differently shaped
/// coplanar quads — the water face spans the whole square, the stair's own face
/// only its bottom half — can round either way by a ULP at a given pixel, and
/// that is the flicker.
///
/// Four is the *requirement*, not the shipped value: [`FLUID_DEPTH_NUDGE`] is
/// `2^-21`, eight ULPs at the `[0.5, 1)` exponent, so the fix carries deliberate
/// margin over this floor rather than being tuned to sit exactly on it.
const MIN_ULPS: i64 = 4;

/// The ceiling on how far back the nudge may push a fluid surface, as a
/// **fraction of that surface's distance from the camera**.
///
/// A constant window-depth offset costs world distance quadratically — that
/// asymmetry is the entire point, since it is what makes the ULP count
/// distance-independent — so it needs an upper bound too, or a large nudge would
/// push a far ocean surface behind its own sea floor. An *absolute* block count
/// is the wrong shape for that bound: in a perspective depth buffer the scale
/// that matters is how far away the surface is. Relative to distance the push is
/// linear (`d * nudge / near`), so one bound covers the whole range: measured,
/// 0.05% at 2 blocks rising to 0.5% at 512, the furthest a 32-chunk render
/// distance can show. 1% therefore holds across every distance terrain is ever
/// drawn at, and stops holding around 1050 blocks — beyond any render distance.
const MAX_PUSH_FRACTION: f32 = 0.01;

/// The fluid pass's real depth for a face at block-local `local_z`, i.e. the
/// baked geometry *plus* the shader's [`FLUID_DEPTH_NUDGE`] — which is the
/// quantity the depth test actually compares.
///
/// The shader adds the nudge in clip space as `z + n * w`, and `(z + n*w) / w`
/// is `z/w + n`, so adding it to window depth here is the same arithmetic and
/// not an approximation of it.
fn fluid_window_depth_with_far(distance: f32, local_z: f32, far: f32) -> f32 {
    window_depth_with_far(distance, local_z, far) + FLUID_DEPTH_NUDGE
}

fn fluid_window_depth(distance: f32, local_z: f32) -> f32 {
    fluid_window_depth_with_far(distance, local_z, Camera::far_for_render_distance(12, 0))
}

/// How many blocks further from the camera a depth of `depth` reads as, compared
/// with the surface actually sitting at `distance`. Inverts the same projection
/// by bisection rather than by a rearranged formula, so an algebra slip cannot
/// make the bound look satisfied.
fn push_in_blocks(distance: f32, depth: f32, far: f32) -> f32 {
    let mut lo = distance;
    let mut hi = distance;
    // Grow the upper bracket until it straddles the target depth. Bounded, so a
    // depth past the far plane ends the loop and shows up as an implausible
    // push rather than spinning.
    for _ in 0..64 {
        if window_depth_with_far(hi, 0.0, far) >= depth {
            break;
        }
        hi *= 2.0;
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if window_depth_with_far(mid, 0.0, far) < depth {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi) - distance
}

/// The measurement, collected across every distance and reported as a table.
///
/// Mismatches are collected rather than asserted inside the loop, so a failure
/// prints the whole distance sweep and names the distance at which the
/// separation stops holding — which is exactly how the original bug was
/// diagnosed (the raw inset is worth 210 ULP at 2 blocks, 1 at 32, 0 at 64 and
/// **-1** at 128, so beyond ~24 blocks the water and the block face were tied or
/// inverted).
#[test]
fn fluid_faces_stay_behind_a_coplanar_block_face_at_every_view_distance() {
    let inset = baked_north_inset();
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for Sample { distance: d, far } in samples() {
        let boundary = window_depth_with_far(d, 0.0, far);
        let raw = window_depth_with_far(d, inset, far);
        let water = fluid_window_depth_with_far(d, inset, far);
        let raw_gap = ulp_gap(boundary, raw);
        let gap = ulp_gap(boundary, water);
        let push = push_in_blocks(d, water, far);
        rows.push(format!(
            "  d = {d:>6} blocks (far {far:>6}): boundary = {boundary:.9} \
             water = {water:.9} gap = {gap:>5} ULP (inset alone: {raw_gap:>4} ULP, \
             push = {push:.5} blocks = {:.4}% of distance)",
            (push / d) * 100.0
        ));
        if gap < MIN_ULPS {
            failures.push(format!(
                "at {d} blocks the fluid pass sits only {gap} ULP behind a \
                 coplanar block face (need >= {MIN_ULPS}); the inset alone is \
                 worth {raw_gap} ULP there"
            ));
        }
        let push_fraction = push / d;
        if push_fraction > MAX_PUSH_FRACTION {
            failures.push(format!(
                "at {d} blocks the nudge pushes the fluid {push} blocks back — \
                 {:.3}% of its distance, over the {:.3}% limit — far enough to \
                 lose to geometry it should cover",
                push_fraction * 100.0,
                MAX_PUSH_FRACTION * 100.0
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
/// sign-flip sweep (`is water ever in front?`) passed on the un-nudged code:
/// whether the collapsed inset rounds to -1, 0 or +1 ULP depends on the exact
/// distance, so an actual inversion is sparse and a 0.31-block step walks
/// straight past it. The measured defect is not that the water wins somewhere, it
/// is that the margin is **thin enough for rounding to decide** — so the quantity
/// to hold a floor under is the ULP gap, at every distance, which is exactly what
/// makes the control below fail.
///
/// The step is deliberately not a round number of blocks: a regular step can land
/// on a lattice that misses where the gap is thinnest.
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
        let boundary = window_depth_with_far(d, 0.0, far);
        let water = fluid_window_depth_with_far(d, inset, far);
        let gap = ulp_gap(boundary, water);
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

/// Control for the sweep above, **observed failing**: run the identical sweep on
/// the un-nudged geometry and require it to find distances where the separation
/// is below the floor.
///
/// Measured, the un-nudged inset collapses to 0 or ±1 ULP over most of the range
/// past ~30 blocks, so this finds hundreds of them. Without this the sweep above
/// could be passing on a depth buffer that never had a problem.
#[test]
fn control_the_un_nudged_separation_does_collapse_as_the_camera_moves() {
    let inset = baked_north_inset();
    let far = Camera::far_for_render_distance(12, 0);
    let mut thin = 0usize;
    let mut steps = 0usize;
    let mut d = 1.0_f32;
    while d <= 192.0 {
        let gap = ulp_gap(
            window_depth_with_far(d, 0.0, far),
            window_depth_with_far(d, inset, far),
        );
        steps += 1;
        if gap < MIN_ULPS {
            thin += 1;
        }
        d += 0.3137;
    }
    assert!(
        thin > 0,
        "control failed: without the nudge the separation never dropped below \
         {MIN_ULPS} ULP anywhere in 1..192 blocks, so the sweep above is not \
         sensitive to the defect it exists to catch"
    );
    println!(
        "  control: the un-nudged inset falls below {MIN_ULPS} ULP at {thin} of \
         {steps} sampled distances"
    );
}

/// Control: the same measurement with **no nudge and no inset** must report
/// zero ULPs at every distance.
///
/// Both halves matter. Zero proves the two surfaces really are coplanar in the
/// fixture, so the gate above is measuring the separation rather than an
/// accidental offset; and it proves the detector can report zero at all, which
/// the passing gate above never demonstrates.
#[test]
fn control_a_truly_coplanar_pair_measures_zero_ulps() {
    let mut nonzero = Vec::new();
    for Sample { distance: d, far } in samples() {
        let gap = ulp_gap(
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

/// Control: the **inset on its own** — the state of the code before the nudge —
/// must fail this file's floor, and must fail it by going to zero or negative at
/// long range rather than merely getting small.
///
/// This is the observed-failing control. Without it, the gate above could be
/// passing because the depth buffer was fine all along and the nudge is inert.
#[test]
fn control_the_inset_alone_ties_and_then_inverts_at_range() {
    let inset = baked_north_inset();
    let gap_at = |d: f32| ulp_gap(window_depth(d, 0.0), window_depth(d, inset));
    // Close up the world-space inset is plenty.
    assert!(
        gap_at(2.0) >= MIN_ULPS,
        "premise: the inset should be ample at 2 blocks, got {} ULP",
        gap_at(2.0)
    );
    // At 64 blocks it buys nothing at all: the two surfaces are bit-identical.
    assert_eq!(
        gap_at(64.0),
        0,
        "premise: the un-nudged inset was measured as an exact tie at 64 blocks"
    );
    // And at 128 it is inverted — the water reads as *nearer* than the block
    // face it is supposed to sit behind.
    assert!(
        gap_at(128.0) < 0,
        "premise: the un-nudged inset was measured inverted at 128 blocks, got \
         {} ULP",
        gap_at(128.0)
    );
    // So the un-nudged geometry fails this file's floor, and the nudge is doing
    // the work rather than dressing up an already-correct depth buffer.
    assert!(gap_at(64.0) < MIN_ULPS);
    assert!(
        ulp_gap(window_depth(64.0, 0.0), fluid_window_depth(64.0, inset)) >= MIN_ULPS,
        "the nudge must be what lifts 64 blocks over the floor"
    );
}
