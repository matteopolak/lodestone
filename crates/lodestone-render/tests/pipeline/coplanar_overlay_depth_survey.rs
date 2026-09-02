//! How much depth separation does a thin overlay quad actually have, and what
//! does a polygon offset really contribute on this backend?
//!
//! # What it is
//!
//! Three shipped subsystems put a thin quad a small **world-space** distance in
//! front of another surface and rely on the depth test to keep it there:
//!
//! | overlay | world clearance | its bias | the surface behind it | that surface's bias |
//! |---|---|---|---|---|
//! | a filled map in a **visible** item frame | `1.01 / 128` | `MAP_SURFACE_DEPTH_BIAS` | the frame body's front texture | `CAMERA_DEPTH_BIAS` |
//! | a filled map in an **invisible** (glow) frame | `1.01 / 128` | `MAP_SURFACE_DEPTH_BIAS` | the attachment wall's face | none — terrain |
//! | a **glowing** sign's outline | `1/256 + 2/2048` | `CAMERA_DEPTH_BIAS` | the sign board's front face | none — terrain |
//! | a sign's ordinary glyph ink | `1/256 + 2/2048` | twice `CAMERA_DEPTH_BIAS` | the sign board's front face | none — terrain |
//!
//! The last two share **one** world plane, so their separation from each other
//! is entirely their polygon offset — the row for the outline and the row for
//! the ink differ only in the bias column, and the geometric table below is
//! identical for both by construction.
//!
//! `fluid_coplanar_depth_gate` established the method and the units. This file
//! asks the same question of the four overlays above, and adds the axis that
//! file did not need: **the angle between the view direction and the surface
//! normal**, because every one of these defects was reported at oblique views.
//!
//! # This table is now a *before and after*
//!
//! Every number below was first taken under a **forward** `[0, 1]`
//! `Depth32Float` projection, which spends almost the whole float32 mantissa
//! near the near plane, so a fixed world-space clearance bought a count of
//! representable depth values that **collapsed as the square of the distance** —
//! a map's `1.01 / 128` was worth 1661 ULP at 2 blocks and **1 at 64**, and the
//! fluid pass's `0.001`-block inset went to **0 at 64**. That is the defect this
//! survey was written to measure and the reason the projection is now
//! reversed-Z (`Camera::projection_matrix`), under which the same separations
//! are two to three orders of magnitude larger and degrade only as the distance.
//! The forward figures are kept in the doc comments below as the before column;
//! the run prints the current ones.
//!
//! It then measures the half that cannot be derived: **what a
//! `wgpu::DepthBiasState` is worth in depth units on the real device.** The
//! constant term's unit `r` is format- and backend-defined — for a float depth
//! attachment the graphics APIs scale it by the primitive's own exponent, which
//! would make it a ULP count, but nothing in this workspace had ever checked
//! that against the device we actually ship on. [`polygon_offset_calibration`]
//! renders one quad with each bias and differences the read-back depth.
//!
//! # Where the expected values come from
//!
//! * The clearances are read out of the production constants, not restated.
//! * The depths come from the real [`Camera::view_projection`].
//! * The ULP unit is IEEE-754 float32 bit differencing, pinned by
//!   `fluid_coplanar_depth_gate::ulp_gap_is_a_real_ulp_count` against
//!   `f32::EPSILON`.
//! * The polygon offset's value comes from the **device**, not from a spec.
//!
//! # Scope
//!
//! This is an arithmetic and device-calibration survey of the depth buffer. It
//! says what separation exists; it says nothing about whether a given overlay is
//! *submitted*, lit, or culled. Pixel gates cover that half elsewhere, and they
//! cover only their own fixtures.
//!
//! ```text
//! cargo test -p lodestone-render --test coplanar_overlay_depth_survey -- --nocapture
//! cargo test -p lodestone-render --test coplanar_overlay_depth_survey -- --ignored --nocapture
//! ```

use lodestone_render::Camera;
use lodestone_render::model_pipeline::{CAMERA_DEPTH_BIAS, MAP_SURFACE_DEPTH_BIAS};

/// The float32 ULP distance between two same-signed non-negative depths. Same
/// construction as `fluid_coplanar_depth_gate::ulp_gap`, whose own control pins
/// it against `f32::EPSILON`.
fn ulp_gap(a: f32, b: f32) -> i64 {
    assert!(a.is_finite() && b.is_finite(), "non-finite depth: {a}, {b}");
    assert!(a >= 0.0 && b >= 0.0, "negative depth: {a}, {b}");
    i64::from(b.to_bits()) - i64::from(a.to_bits())
}

/// A viewport to denominate window-space depth slopes in. The slope-scaled half
/// of a polygon offset is `slope_scale * max(|dz/dx|, |dz/dy|)` in **window**
/// coordinates, so it depends on the resolution the frame is rendered at.
const VIEWPORT: (f32, f32) = (1920.0, 1080.0);

/// A camera `distance` blocks from the world origin, looking at it, offset by
/// `angle_deg` from the `-Z` surface normal of a plane through the origin.
///
/// The plane under test is `z = 0` with its front face toward `-Z`; the overlay
/// sits at `z = -clearance`. Camera yaw equals the angle by construction:
/// [`Camera::basis`]'s forward is `(-sin y cos p, -sin p, cos y cos p)`, so a
/// camera at `origin + d * (sin a, 0, -cos a)` looks back at the origin exactly
/// when `yaw == a`. At `angle_deg == 0` this is a head-on view.
fn camera_at(distance: f32, angle_deg: f32, far: f32) -> Camera {
    let a = angle_deg.to_radians();
    let mut camera = Camera::default();
    camera.position = glam::Vec3::new(a.sin() * distance, 0.0, -a.cos() * distance);
    camera.yaw = angle_deg;
    camera.pitch = 0.0;
    camera.far = far;
    camera.aspect = VIEWPORT.0 / VIEWPORT.1;
    camera
}

/// Window-space depth `z / w` of a world point through the real projection.
fn window_depth(camera: &Camera, world: glam::Vec3) -> f32 {
    let clip = camera.view_projection() * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    clip.z / clip.w
}

/// Window-space pixel coordinates and depth of a world point.
fn window_point(camera: &Camera, world: glam::Vec3) -> (f32, f32, f32) {
    let clip = camera.view_projection() * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc = glam::Vec3::new(clip.x / clip.w, clip.y / clip.w, clip.z / clip.w);
    (
        (ndc.x + 1.0) * 0.5 * VIEWPORT.0,
        (1.0 - ndc.y) * 0.5 * VIEWPORT.1,
        ndc.z,
    )
}

/// The `m` a polygon offset's slope term is multiplied by: the maximum
/// window-space depth gradient of the primitive, `max(|dz/dx|, |dz/dy|)`.
///
/// Measured on the real projection rather than derived, by projecting three
/// points of the plane `z = -clearance` and solving the resulting affine depth
/// gradient. A perspective projection makes depth only *approximately* affine in
/// window space, which is exactly what the hardware's own gradient estimate
/// does too, so a local three-point solve is the same quantity the rasterizer
/// computes.
fn window_depth_slope(camera: &Camera, plane_z: f32) -> f32 {
    // Three plane points spanning a patch about the size of a map (one block).
    let o = glam::Vec3::new(0.0, 0.0, plane_z);
    let dx = glam::Vec3::new(0.5, 0.0, plane_z);
    let dy = glam::Vec3::new(0.0, 0.5, plane_z);
    let (x0, y0, z0) = window_point(camera, o);
    let (x1, y1, z1) = window_point(camera, dx);
    let (x2, y2, z2) = window_point(camera, dy);
    // Solve [dz/dx, dz/dy] from the two edge vectors.
    let (ax, ay, az) = (x1 - x0, y1 - y0, z1 - z0);
    let (bx, by, bz) = (x2 - x0, y2 - y0, z2 - z0);
    let det = ax * by - ay * bx;
    if det.abs() < 1.0e-12 {
        return f32::INFINITY;
    }
    let dzdx = (az * by - bz * ay) / det;
    let dzdy = (ax * bz - bx * az) / det;
    dzdx.abs().max(dzdy.abs())
}

/// One overlay under survey: a name, its world-space clearance from the surface
/// behind it, and the two biases in play.
struct Overlay {
    name: &'static str,
    clearance: f32,
    overlay_bias: wgpu::DepthBiasState,
    behind_bias: wgpu::DepthBiasState,
}

/// `MapRenderer`'s picture-plane offset, `1.01 / 128` blocks. Read from the
/// shell's own constant would be a cross-crate test dependency the shell does
/// not export; it is restated here **and** pinned by
/// [`the_surveyed_clearances_match_the_shipped_geometry`] against the shell's
/// public geometry, so a drift fails rather than silently surveying a fiction.
const MAP_PLANE_CLEARANCE: f32 = 1.01 / 128.0;

/// `SignTextRenderer`'s `SIGN_FACE_CLEARANCE`, the base board clearance.
const SIGN_FACE_CLEARANCE: f32 = 1.0 / 256.0;

/// `SignTextRenderer`'s `SIGN_TEXT_SURFACE_CLEARANCE`. **Both** the glowing
/// outline and the ordinary ink are emitted on this one plane — the outline does
/// not get a plane of its own, so the two layers' entire separation from each
/// other is their polygon offset, and their geometric separation is exactly
/// zero at every distance and angle.
const SIGN_TEXT_CLEARANCE: f32 = SIGN_FACE_CLEARANCE + 2.0 / 2048.0;

fn overlays() -> Vec<Overlay> {
    vec![
        Overlay {
            name: "map in a visible item frame (behind: frame body)",
            clearance: MAP_PLANE_CLEARANCE,
            overlay_bias: MAP_SURFACE_DEPTH_BIAS,
            behind_bias: CAMERA_DEPTH_BIAS,
        },
        Overlay {
            name: "map in an invisible glow frame (behind: wall face)",
            clearance: MAP_PLANE_CLEARANCE,
            overlay_bias: MAP_SURFACE_DEPTH_BIAS,
            behind_bias: wgpu::DepthBiasState { constant: 0, slope_scale: 0.0, clamp: 0.0 },
        },
        Overlay {
            name: "glowing sign outline (behind: sign board)",
            clearance: SIGN_TEXT_CLEARANCE,
            overlay_bias: CAMERA_DEPTH_BIAS,
            behind_bias: wgpu::DepthBiasState { constant: 0, slope_scale: 0.0, clamp: 0.0 },
        },
        Overlay {
            name: "ordinary sign glyph ink (behind: sign board)",
            clearance: SIGN_TEXT_CLEARANCE,
            overlay_bias: wgpu::DepthBiasState {
                constant: 2 * CAMERA_DEPTH_BIAS.constant,
                slope_scale: 2.0 * CAMERA_DEPTH_BIAS.slope_scale,
                clamp: CAMERA_DEPTH_BIAS.clamp,
            },
            behind_bias: wgpu::DepthBiasState { constant: 0, slope_scale: 0.0, clamp: 0.0 },
        },
    ]
}

/// The distances the reports come from — the owner sees these defects while
/// walking past a wall, so the near/mid regime is what matters — plus the long
/// tail, so a collapse point can be located.
const DISTANCES: [f32; 8] = [2.0, 4.0, 8.0, 12.0, 16.0, 24.0, 32.0, 64.0];

/// Angles off the surface normal, out to the grazing regime the reports name.
const ANGLES: [f32; 6] = [0.0, 30.0, 45.0, 60.0, 75.0, 85.0];

/// The ULP separation the **depth test actually sees** between a back plane at
/// `z = 0` and an overlay plane at `z = -clearance`, at the pixel a given back-
/// plane point projects to.
///
/// This is the whole correctness of the survey and the first draft got it
/// wrong in a way that inverted the angle conclusion. A depth comparison happens
/// **at a pixel**: the value already in the buffer came from wherever the
/// wall was hit by that pixel's ray, and the incoming fragment is wherever the
/// overlay was hit by the *same* ray. Those are two different world points
/// whenever the view is oblique. Differencing the depths of two points that
/// share a world `x`/`y` instead measures `clearance * cos(theta)` — separation
/// *shrinking* at grazing angles — where the real, ray-correct quantity is
/// `clearance / cos(theta)`, which **grows**. The two answers differ by
/// `1 / cos^2` and disagree by a factor of 130 at 85 degrees, so the wrong one
/// would have sent this whole investigation at the wrong mechanism.
///
/// Returns `None` for a ray parallel to the planes, which projects to nothing.
///
/// The **sign convention** is "how far the overlay is in front", and which way
/// that is in the buffer is asked of the projection ([`nearer_is_greater`])
/// rather than written down, because that is exactly the fact a reversed-Z
/// conversion changes. Hardcoded, every cell of this table silently negates.
fn ray_ulp_gap(camera: &Camera, back_point: glam::Vec3, clearance: f32) -> Option<i64> {
    let eye = camera.position;
    let dir = back_point - eye;
    // The back plane is `z = 0`, the overlay `z = -clearance`. Walk the same ray
    // from the eye to each.
    if dir.z.abs() < 1.0e-9 {
        return None;
    }
    let s = (-clearance - eye.z) / dir.z;
    if s <= 0.0 {
        return None;
    }
    let front_point = eye + dir * s;
    let back = window_depth(camera, back_point);
    let front = window_depth(camera, front_point);
    if !(back.is_finite() && front.is_finite()) || back < 0.0 || front < 0.0 {
        return None;
    }
    Some(if nearer_is_greater() {
        ulp_gap(back, front)
    } else {
        ulp_gap(front, back)
    })
}

/// Does a surface nearer the eye carry a **larger** window depth?
///
/// True under this renderer's reversed-Z projection and false under a forward
/// `[0, 1]` one. Derived from the production [`Camera`] so the survey reports a
/// positive separation for an overlay that really is in front, whichever
/// convention the projection carries.
fn nearer_is_greater() -> bool {
    let camera = camera_at(8.0, 0.0, Camera::far_for_render_distance(12, 0));
    let near = window_depth(&camera, glam::Vec3::new(0.0, 0.0, -1.0));
    let far = window_depth(&camera, glam::Vec3::new(0.0, 0.0, 0.0));
    assert_ne!(
        near, far,
        "premise: two surfaces a block apart projected to the same depth, so \
         this survey cannot tell which direction is toward the eye"
    );
    near > far
}

/// **The survey.** For each overlay, the worst ULP separation the depth test
/// sees anywhere across a one-block patch of the surface, by camera distance and
/// by angle off the surface normal.
///
/// The minimum over the patch is reported rather than the centre, because the
/// artefact only needs one region of the quad to tie — and the **location** of
/// the worst sample is printed alongside so a thin band can be told from a
/// uniformly-collapsed quad.
///
/// This test asserts only what must hold for the table to *mean* anything — that
/// the clearances are positive and the detector can move. The floor lives in the
/// gate this survey motivated; here the numbers are the deliverable.
#[test]
fn geometric_separation_by_distance_and_angle() {
    let far = Camera::far_for_render_distance(12, 0);
    for overlay in overlays() {
        assert!(
            overlay.clearance > 0.0,
            "precondition: {} has no clearance to survey",
            overlay.name
        );
        println!("\n== {} ==", overlay.name);
        println!("   clearance {:.7} blocks", overlay.clearance);
        println!(
            "   overlay bias (const {}, slope {}) vs behind (const {}, slope {})",
            overlay.overlay_bias.constant,
            overlay.overlay_bias.slope_scale,
            overlay.behind_bias.constant,
            overlay.behind_bias.slope_scale
        );
        print!("   {:>9}", "d \\ angle");
        for a in ANGLES {
            print!("{:>16}", format!("{a:.0}deg"));
        }
        println!();
        for d in DISTANCES {
            print!("   {d:>9.0}");
            for a in ANGLES {
                let camera = camera_at(d, a, far);
                let mut worst = i64::MAX;
                let mut worst_at = (0.0f32, 0.0f32);
                let mut sampled = 0usize;
                // A one-block patch of the surface, the size of a map or a
                // sign board, sampled on a grid so a thin band is not missed.
                for i in 0i32..=8 {
                    for j in 0i32..=8 {
                        let x = -0.5 + i as f32 / 8.0;
                        let y = -0.5 + j as f32 / 8.0;
                        let p = glam::Vec3::new(x, y, 0.0);
                        if let Some(gap) = ray_ulp_gap(&camera, p, overlay.clearance) {
                            sampled += 1;
                            if gap < worst {
                                worst = gap;
                                worst_at = (x, y);
                            }
                        }
                    }
                }
                assert!(sampled > 0, "no ray reached the planes at d={d} angle={a}");
                let m = window_depth_slope(&camera, -overlay.clearance);
                print!(
                    "{:>16}",
                    format!("{worst}@({:.2},{:.2})", worst_at.0, worst_at.1)
                );
                let _ = m;
            }
            println!();
        }
        // The slope row is per-angle only (it does not depend on which overlay
        // plane is sampled to three digits), so print it once.
        print!("   {:>9}", "slope m");
        for a in ANGLES {
            let camera = camera_at(16.0, a, far);
            print!("{:>16}", format!("{:.3e}", window_depth_slope(&camera, -overlay.clearance)));
        }
        println!("   (at d=16)");
    }
    println!(
        "\n   (cell is: the WORST ULP separation over a one-block patch, and the \
         patch coordinate it occurs at)"
    );
}

/// Precondition for the survey: the clearances above are the ones the shipped
/// geometry actually uses.
///
/// The map plane is the only one this crate can check against a production
/// symbol; the two sign clearances live in `lodestone-shell` and are private, so
/// this pins their **arithmetic identity** instead — the glyph plane is exactly
/// `2 / 2048` further out than the outline plane, which is the relationship the
/// survey's conclusion rests on.
#[test]
fn the_surveyed_clearances_match_the_shipped_geometry() {
    assert!(
        (MAP_PLANE_CLEARANCE - 1.01 / 128.0).abs() < f32::EPSILON,
        "the map plane clearance drifted"
    );
    assert!(
        (SIGN_TEXT_CLEARANCE - (1.0 / 256.0 + 2.0 / 2048.0)).abs() < 1.0e-9,
        "the sign text plane drifted"
    );
    // And the biases: vanilla's magnitudes, the outline one step and the map
    // two. The **sign** is deliberately not restated here — it is a property of
    // the projection, and `model_pipeline`'s own
    // `camera_depth_bias_pulls_coplanar_world_geometry_toward_the_eye` derives
    // it from a real camera. Asserting it in two places is how one of them ends
    // up certifying a bias pointing into the wall.
    assert_eq!(CAMERA_DEPTH_BIAS.constant.abs(), 10);
    assert_eq!(CAMERA_DEPTH_BIAS.slope_scale.abs(), 1.0);
    assert_eq!(MAP_SURFACE_DEPTH_BIAS.constant, 2 * CAMERA_DEPTH_BIAS.constant);
    assert_eq!(MAP_SURFACE_DEPTH_BIAS.slope_scale, CAMERA_DEPTH_BIAS.slope_scale);
}

/// The survey's own headline claim, asserted rather than left in prose: every
/// one of these overlays now has a separation that **holds** at 64 blocks, where
/// the forward projection this replaced left the thinnest of them at 1 ULP.
///
/// The threshold is `fluid_coplanar_depth_gate`'s `MIN_ULPS` of 4 — the floor
/// below which a rasterizer interpolating two differently-shaped coplanar quads
/// can round either way — and the head-on column is the one asserted because it
/// is the *worst* one: the geometric separation along a ray grows as
/// `clearance / cos(theta)`, so obliquity only ever helps.
#[test]
fn every_surveyed_overlay_clears_the_rounding_floor_head_on_at_range() {
    const MIN_ULPS: i64 = 4;
    let far = Camera::far_for_render_distance(12, 0);
    let mut thin = Vec::new();
    for overlay in overlays() {
        for d in DISTANCES {
            let camera = camera_at(d, 0.0, far);
            let mut worst = i64::MAX;
            for i in 0i32..=8 {
                for j in 0i32..=8 {
                    let p = glam::Vec3::new(-0.5 + i as f32 / 8.0, -0.5 + j as f32 / 8.0, 0.0);
                    if let Some(gap) = ray_ulp_gap(&camera, p, overlay.clearance) {
                        worst = worst.min(gap);
                    }
                }
            }
            assert_ne!(worst, i64::MAX, "no ray reached the planes at d={d}");
            if worst < MIN_ULPS {
                thin.push(format!(
                    "  {} at {d} blocks head-on: {worst} ULP",
                    overlay.name
                ));
            }
        }
    }
    assert!(
        thin.is_empty(),
        "these overlays are within rounding distance of the surface behind them, \
         so their polygon offset is load-bearing rather than a tiebreak:\n{}",
        thin.join("\n")
    );
}

/// Control for the survey's detector: an overlay with **zero** clearance must
/// measure exactly zero ULP at every distance and angle.
///
/// Without this the table above could be reporting an artefact of the camera
/// construction rather than the clearance.
#[test]
fn control_a_zero_clearance_overlay_measures_zero_ulps_everywhere() {
    let far = Camera::far_for_render_distance(12, 0);
    let mut nonzero = Vec::new();
    for d in DISTANCES {
        for a in ANGLES {
            let camera = camera_at(d, a, far);
            let gap = ray_ulp_gap(&camera, glam::Vec3::ZERO, 0.0)
                .expect("the centre ray must reach the plane");
            if gap != 0 {
                nonzero.push(format!("d={d} a={a}: {gap} ULP"));
            }
        }
    }
    assert!(
        nonzero.is_empty(),
        "control failed: a zero clearance did not measure zero, so the survey is \
         not measuring the clearance:\n{}",
        nonzero.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Device calibration: what a wgpu::DepthBiasState is really worth.
// ---------------------------------------------------------------------------

const PROBE_W: u32 = 64;
const PROBE_H: u32 = 64;
const PROBE_DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The probe's depth clear. Aliased to the production
/// [`lodestone_render::DEPTH_CLEAR`] because the saturation sweep's whole
/// detection is "a discarded fragment reads back the clear value", which only
/// works while the clear sits at the opposite end of the range from the
/// direction a toward-the-eye bias pushes.
const PROBE_CLEAR: f32 = lodestone_render::DEPTH_CLEAR;

/// The depth the saturation sweep draws its quad at: near the clear end, so an
/// over-large toward-the-eye bias has the whole range to travel before it
/// saturates.
const PROBE_SATURATION_BASE: f32 = 0.02;

/// The sign a `constant`/`slope_scale` must carry to move a fragment **toward
/// the eye**, derived from the production projection rather than written down.
///
/// The probe below draws in clip space and never touches `Camera`, but the
/// question it exists to answer — "does the shipped bias pull the way its users
/// need" — is about the projection those users draw through.
fn toward_the_eye() -> f32 {
    let camera = Camera {
        position: glam::Vec3::new(0.0, 0.0, 4.0),
        yaw: 180.0,
        ..Camera::default()
    };
    let vp = camera.view_projection();
    let depth = |z: f32| {
        let c = vp * glam::Vec4::new(0.0, 0.0, z, 1.0);
        c.z / c.w
    };
    let (close, distant) = (depth(1.0), depth(0.0));
    assert_ne!(close, distant, "premise: the projection is degenerate in z");
    if close > distant { 1.0 } else { -1.0 }
}

/// A pass-through shader: the vertex buffer already holds clip-space positions,
/// so the probe controls `z/w` exactly and the calibration does not inherit a
/// projection's rounding.
const PROBE_WGSL: &str = r#"
struct VsOut { @builtin(position) pos: vec4<f32> };

@vertex
fn vs_main(@location(0) clip: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.pos = clip;
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;

/// Vertex attribute layout for [`PROBE_WGSL`], hoisted so the descriptor can
/// borrow it for the pipeline's lifetime.
const PROBE_ATTRS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x4];

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    backend: wgpu::Backend,
}

fn setup() -> Option<Gpu> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await
            .ok()?;
        let backend = adapter.get_info().backend;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("coplanar_overlay_depth_survey device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue, backend })
    })
}

/// Rasterise one full-viewport quad whose clip-space depth runs from `z_left` on
/// the left edge to `z_right` on the right edge (both with `w = 1`, so those are
/// window depths directly), through a pipeline carrying `bias`, and return the
/// depth actually written at the centre pixel.
fn probe_depth(gpu: &Gpu, z_left: f32, z_right: f32, bias: wgpu::DepthBiasState) -> f32 {
    let device = &gpu.device;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("probe"),
        source: wgpu::ShaderSource::Wgsl(PROBE_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("probe-layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("probe-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 16,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &PROBE_ATTRS,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: PROBE_DEPTH,
            depth_write_enabled: Some(true),
            // `Always` so the probe measures what the polygon offset *wrote*,
            // never what a comparison rejected.
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState::default(),
            bias,
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // Two triangles covering the viewport, depth linear in clip x.
    let v = |x: f32, y: f32, z: f32| [x, y, z, 1.0f32];
    let verts: [[f32; 4]; 6] = [
        v(-1.0, -1.0, z_left),
        v(1.0, -1.0, z_right),
        v(1.0, 1.0, z_right),
        v(-1.0, -1.0, z_left),
        v(1.0, 1.0, z_right),
        v(-1.0, 1.0, z_left),
    ];
    let vbuf = wgpu::util::DeviceExt::create_buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("probe-verts"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        },
    );

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("probe-color"),
        size: wgpu::Extent3d { width: PROBE_W, height: PROBE_H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("probe-depth"),
        size: wgpu::Extent3d { width: PROBE_W, height: PROBE_H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PROBE_DEPTH,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe-readback"),
        size: u64::from(PROBE_W * PROBE_H * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let cv = color.create_view(&wgpu::TextureViewDescriptor::default());
    let dv = depth.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("probe-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &cv,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &dv,
                depth_ops: Some(wgpu::Operations {
                    // Deliberately the *far* end of the range from where a
                    // toward-the-eye bias pushes, so a discarded fragment is
                    // distinguishable from a clamped one in the saturation
                    // sweep. `lodestone_render::DEPTH_CLEAR` is that end under
                    // reversed-Z, and naming it keeps the two in step if the
                    // convention ever moves again.
                    load: wgpu::LoadOp::Clear(PROBE_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.draw(0..6, 0..1);
    }
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &depth,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(PROBE_W * 4),
                rows_per_image: Some(PROBE_H),
            },
        },
        wgpu::Extent3d { width: PROBE_W, height: PROBE_H, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([enc.finish()]);

    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    let data = readback.slice(..).get_mapped_range().expect("mapped range");
    let floats: &[f32] = bytemuck::cast_slice(&data);
    // Centre pixel: for a slope-free quad every pixel is the same, and for a
    // sloped one the centre is where the un-biased reference is also sampled.
    let centre = floats[(PROBE_H / 2 * PROBE_W + PROBE_W / 2) as usize];
    drop(data);
    readback.unmap();
    centre
}

/// The same probe, reporting the **depth-range saturation** question instead:
/// does a bias large enough to drive the depth past `0` clamp the fragment to
/// `0`, or discard it?
///
/// This distinguishes two completely different failure modes for an overlay
/// carrying a large slope-scaled offset at a grazing angle. If the hardware
/// clamps, an over-large bias is merely a very strong win. If it *discards*, an
/// over-large bias makes the primitive **vanish** — and because the slope term
/// is evaluated per primitive, the two triangles of a quad can cross the
/// threshold at different moments, which would present as a piece of the quad
/// disappearing along its diagonal.
fn probe_saturation(gpu: &Gpu, base: f32, bias: wgpu::DepthBiasState) -> f32 {
    probe_depth(gpu, base, base, bias)
}

/// **The device calibration.** What does a `DepthBiasState` actually add?
///
/// Two questions, both unanswerable from this repo's source:
///
/// 1. Is the `constant` term scaled by the depth format's representable step —
///    a ULP count — or added as a raw float? A raw `-10` would saturate every
///    biased primitive to depth `0`; a ULP-scaled `-10` is ten representable
///    values, which is a completely different subsystem behaviour.
/// 2. Does the `slope_scale` term grow without bound at grazing angles, and how
///    large is it relative to the constant term at the slopes the survey above
///    reports?
///
/// The measurement is the difference between the depth a quad writes with a
/// bias and the depth the identical quad writes without one, in ULPs.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to read the polygon-offset calibration"]
fn polygon_offset_calibration() {
    let Some(gpu) = setup() else {
        panic!(
            "no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for a device measurement; a skip would report \
             nothing while looking like a pass."
        );
    };
    println!("   backend: {:?}", gpu.backend);

    let none = wgpu::DepthBiasState::default();
    let sign = toward_the_eye();
    let c10 = (10.0 * sign) as i32;
    let c20 = (20.0 * sign) as i32;
    println!("\n== constant term, flat quad (slope 0) ==");
    println!("   {:>12}{:>16}{:>16}{:>16}", "base depth", "unbiased", format!("const {c10}"), "ULP delta");
    for base in [0.02f32, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99] {
        let plain = probe_depth(&gpu, base, base, none);
        let biased = probe_depth(
            &gpu,
            base,
            base,
            wgpu::DepthBiasState { constant: c10, slope_scale: 0.0, clamp: 0.0 },
        );
        let d20 = probe_depth(
            &gpu,
            base,
            base,
            wgpu::DepthBiasState { constant: c20, slope_scale: 0.0, clamp: 0.0 },
        );
        println!(
            "   {base:>12.4}{plain:>16.9}{biased:>16.9}{:>16}  (const {c20} -> {} ULP)",
            ulp_gap(plain, biased).abs(),
            ulp_gap(plain, d20).abs()
        );
    }

    println!("\n== slope term, quad tilted across the viewport ==");
    println!(
        "   a quad running z_left..z_right over {PROBE_W} px has window slope \
         m = |z_right - z_left| / {PROBE_W}"
    );
    println!(
        "   {:>12}{:>14}{:>18}{:>18}",
        "m",
        "unbiased",
        format!("slope {sign:+.0} ULP"),
        format!("slope {:+.0} ULP", 2.0 * sign)
    );
    for spread in [0.0f32, 0.001, 0.01, 0.05, 0.2, 0.4] {
        let (l, r) = (0.5 - spread * 0.5, 0.5 + spread * 0.5);
        let m = spread / PROBE_W as f32;
        let plain = probe_depth(&gpu, l, r, none);
        let s1 = probe_depth(
            &gpu,
            l,
            r,
            wgpu::DepthBiasState { constant: 0, slope_scale: sign, clamp: 0.0 },
        );
        let s2 = probe_depth(
            &gpu,
            l,
            r,
            wgpu::DepthBiasState { constant: 0, slope_scale: 2.0 * sign, clamp: 0.0 },
        );
        println!(
            "   {m:>12.3e}{plain:>14.9}{:>18}{:>18}",
            ulp_gap(plain, s1).abs(),
            ulp_gap(plain, s2).abs()
        );
    }

    println!("\n== depth-range saturation: does an over-large bias clamp or discard? ==");
    println!(
        "   a quad at depth {PROBE_SATURATION_BASE} cleared to {PROBE_CLEAR}; if \
         the fragment is discarded the read-back depth stays at the CLEAR value"
    );
    println!("   {:>16}{:>18}", "constant", "written depth");
    for magnitude in [10i32, 1_000, 100_000, 10_000_000, 1_000_000_000] {
        let constant = (magnitude as f32 * sign) as i32;
        let d = probe_saturation(
            &gpu,
            0.02,
            wgpu::DepthBiasState { constant, slope_scale: 0.0, clamp: 0.0 },
        );
        println!("   {constant:>16}{d:>18.9}");
    }
    println!("   {:>16}{:>18}", "slope_scale", "written depth (m = 6.25e-3)");
    for magnitude in [1.0f32, 10.0, 100.0, 10_000.0] {
        let slope = magnitude * sign;
        let d = probe_depth(
            &gpu,
            PROBE_SATURATION_BASE - 0.2,
            PROBE_SATURATION_BASE + 0.2,
            wgpu::DepthBiasState { constant: 0, slope_scale: slope, clamp: 0.0 },
        );
        println!("   {slope:>16}{d:>18.9}");
    }

    println!("\n== the shipped pairs, flat, at mid depth ==");
    let base = 0.5f32;
    let plain = probe_depth(&gpu, base, base, none);
    for (name, bias) in [
        ("CAMERA_DEPTH_BIAS", CAMERA_DEPTH_BIAS),
        ("MAP_SURFACE_DEPTH_BIAS", MAP_SURFACE_DEPTH_BIAS),
    ] {
        let b = probe_depth(&gpu, base, base, bias);
        println!(
            "   {name:<26} -> {:>8} ULP toward the eye",
            ulp_gap(plain, b).abs()
        );
    }

    // ---- the assertions, each predicting a magnitude against a named wrong
    // ---- hypothesis rather than a direction.

    // 1. Direction. The shipped `CAMERA_DEPTH_BIAS` must move the fragment
    //    TOWARD the eye, which is the LARGER depth under this renderer's
    //    reversed-Z projection and the smaller one under a forward `[0,1]` one.
    //    Both the expected direction and the bias's own sign come from outside
    //    this assertion — `toward_the_eye()` asks the real projection — so the
    //    wrong hypothesis, that the ported sign is inverted and it pushes
    //    primitives away, fails here rather than at grazing angles in play.
    let flat = probe_depth(&gpu, 0.5, 0.5, none);
    let pulled = probe_depth(&gpu, 0.5, 0.5, CAMERA_DEPTH_BIAS);
    let moved_toward_eye = if sign > 0.0 { pulled > flat } else { pulled < flat };
    assert!(
        moved_toward_eye,
        "the shipped depth bias must move a fragment toward the eye, which for \
         this projection is the {} depth; measured unbiased {flat} vs biased \
         {pulled}",
        if sign > 0.0 { "larger" } else { "smaller" }
    );

    // 2. Magnitude, against the two hypotheses that differ by ~7 orders of
    //    magnitude. If the `constant` were added as a raw float, `-10` would
    //    saturate a 0.5 fragment to 0.0. If it is scaled by the format's
    //    representable step (`r = 2^(exp - 23)` for a float depth attachment),
    //    it is a small ULP count. Predict both and require the measurement to
    //    land on one.
    let raw_float_hypothesis = if sign > 0.0 { 1.0f32 } else { 0.0 };
    let ulp_hypothesis_max = 64i64;
    assert!(
        (pulled - 0.5).abs() < 0.1,
        "a `constant` of {} landed at {pulled}, which is the raw-float \
         hypothesis ({raw_float_hypothesis}) rather than a ULP-scaled offset",
        CAMERA_DEPTH_BIAS.constant
    );
    let constant_ulps = ulp_gap(flat, pulled).abs();
    assert!(
        (1..=ulp_hypothesis_max).contains(&constant_ulps),
        "a `constant` of {} moved the depth {constant_ulps} ULP; the ULP-scaled \
         hypothesis predicts a small multiple of 10 and this is outside 1..={ulp_hypothesis_max}",
        CAMERA_DEPTH_BIAS.constant
    );

    // 3. The constant term scales with the magnitude of `constant`, so doubling
    //    it doubles the offset — the property `MAP_SURFACE_DEPTH_BIAS` relies on
    //    to sit one step ahead of `CAMERA_DEPTH_BIAS`.
    let doubled = ulp_gap(flat, probe_depth(&gpu, 0.5, 0.5, MAP_SURFACE_DEPTH_BIAS)).abs();
    assert_eq!(
        doubled,
        constant_ulps * 2,
        "doubling `constant` must double the offset: {} gave {constant_ulps} ULP \
         and {} gave {doubled}",
        CAMERA_DEPTH_BIAS.constant,
        MAP_SURFACE_DEPTH_BIAS.constant
    );

    // 4. The slope term is real, grows with the primitive's window-space depth
    //    slope, and is **large** compared with the constant term at the slopes a
    //    grazing view produces. The wrong hypothesis — that the slope term is
    //    negligible, which is what a reader would assume from `slope_scale: 1.0`
    //    sitting next to `constant: 10` — predicts the two are comparable.
    let spread = 0.001f32;
    let sloped_plain = probe_depth(&gpu, 0.5 - spread * 0.5, 0.5 + spread * 0.5, none);
    let sloped_biased = probe_depth(
        &gpu,
        0.5 - spread * 0.5,
        0.5 + spread * 0.5,
        wgpu::DepthBiasState { constant: 0, slope_scale: sign, clamp: 0.0 },
    );
    let slope_ulps = ulp_gap(sloped_plain, sloped_biased).abs();
    assert!(
        slope_ulps > constant_ulps * 10,
        "at a window depth slope of {:.3e} the slope term contributed \
         {slope_ulps} ULP against the constant term's {constant_ulps}; the two \
         being comparable would mean the slope term cannot be what defends a \
         grazing view",
        spread / PROBE_W as f32
    );

    // 5. Saturation clamps; it does not discard. This is the measurement that
    //    retires the hypothesis that an unbounded slope-scaled offset at a
    //    grazing angle makes an overlay *vanish*: the depth attachment is
    //    cleared to `PROBE_CLEAR`, the far end of the range, so a discarded
    //    fragment would read back at the clear and a clamped one at the
    //    opposite limit.
    let saturated = probe_depth(
        &gpu,
        PROBE_SATURATION_BASE,
        PROBE_SATURATION_BASE,
        wgpu::DepthBiasState {
            constant: (1_000_000_000.0f32 * sign) as i32,
            slope_scale: 0.0,
            clamp: 0.0,
        },
    );
    assert!(
        (saturated - PROBE_CLEAR).abs() > 0.5,
        "an over-large bias read back as {saturated}; a value at the \
         {PROBE_CLEAR} clear would mean the fragment was discarded rather than \
         clamped, which is a completely different failure mode for a grazing \
         overlay"
    );
}
