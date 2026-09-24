//! Rasterised width of the F3 debug-line ribbon (`gpu/debug_lines.rs`).
//!
//! The owner reported the chunk-border lines as the wrong thickness. The pass
//! had already stopped being a `PrimitiveTopology::LineList` — it expands every
//! segment into a screen-space-thickened quad, exactly as `OutlineRenderer`
//! does — so the geometry existed; what was wrong was the *quad*.
//!
//! `DebugLineRenderer::prepare` writes a `side` (-1/+1) per corner and the
//! vertex shader offsets along the segment's screen-space normal. That normal is
//! derived from `screen(other) - screen(this)`, which points the opposite way at
//! the segment's far endpoint, so the far endpoint's stored `side` has to be
//! negated to land on the same edge of the ribbon. It was not, so the two
//! triangles picked *opposite* diagonals of the quad and the result was a
//! bow-tie: measured here at a 6.4 px line width, the segment rasterised 6 px at
//! both endpoints and 3 px at its midpoint, and sat off-centre rather than
//! centred on the line.
//!
//! **A geometry-level assertion structurally cannot see this**, which is why the
//! gate is a pixel readback: the CPU-side vertex list is byte-plausible either
//! way, and the whole defect lives in what the rasteriser does with it.
//!
//! The viewport is deliberately very wide (8192 px) because
//! `MIN_LINE_WIDTH_PX`'s `max(min, width / 1920 * min)` shape means a 1080p
//! target draws a 1.5 px line — a taper to 0.75 px is real but too small to
//! count reliably. At 8192 the same expression gives 6.4 px, where a 2x taper is
//! unmissable.

use lodestone::gpu::{DebugLineVertex, RenderState};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 8192;
const H: u32 = 512;

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 64.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn seg(a: [f32; 3], b: [f32; 3]) -> Vec<DebugLineVertex> {
    const C: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    vec![
        DebugLineVertex { position: a, color: C },
        DebugLineVertex { position: b, color: C },
    ]
}

fn lit(px: &[u8], base: &[u8], i: usize) -> bool {
    let d = (i32::from(px[i * 4]) - i32::from(base[i * 4])).abs()
        + (i32::from(px[i * 4 + 1]) - i32::from(base[i * 4 + 1])).abs()
        + (i32::from(px[i * 4 + 2]) - i32::from(base[i * 4 + 2])).abs();
    d > 20
}

/// The covered widths along a segment, and the label to print them under.
///
/// The endpoints' own scanlines are dropped: a segment's first and last covered
/// line is a partially-covered end cap whose width is a rasterisation detail, not
/// the ribbon's width.
fn interior(profile: &[usize]) -> &[usize] {
    if profile.len() <= 4 {
        profile
    } else {
        &profile[2..profile.len() - 2]
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn debug_line_ribbon_width_is_uniform_along_the_segment() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut state = RenderState::new(device, queue, format, W, H, None);
    let cam = camera();

    let frame = target.acquire().expect("headless acquire");
    state.render(device, queue, frame.view(), &cam, None, &[]);
    let baseline = target.read_texels(device, queue);

    let mut shoot = |verts: Vec<DebugLineVertex>| -> Vec<u8> {
        state.set_debug_lines_source(move |_| verts.clone());
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &cam, None, &[]);
        target.read_texels(device, queue)
    };

    // Screen-vertical: a world segment along +Y, 20 blocks ahead of the eye.
    let vertical = shoot(seg([0.0, 60.0, 20.0], [0.0, 68.0, 20.0]));
    // Screen-horizontal: a world segment along +X, same distance, same length.
    let horizontal = shoot(seg([-4.0, 64.0, 20.0], [4.0, 64.0, 20.0]));

    // A screen-vertical line's width is measured across x, once per covered row;
    // a screen-horizontal line's across y, once per covered column.
    let vertical_profile: Vec<usize> = (0..H as usize)
        .map(|y| (0..W as usize).filter(|x| lit(&vertical, &baseline, y * W as usize + x)).count())
        .filter(|n| *n > 0)
        .collect();
    let horizontal_profile: Vec<usize> = (0..W as usize)
        .map(|x| (0..H as usize).filter(|y| lit(&horizontal, &baseline, y * W as usize + x)).count())
        .filter(|n| *n > 0)
        .collect();

    eprintln!("=== debug-line ribbon width ===");
    eprintln!("vertical   segment: {} scanlines covered", vertical_profile.len());
    eprintln!("horizontal segment: {} scanlines covered", horizontal_profile.len());

    for (label, profile) in [
        ("vertical", &vertical_profile),
        ("horizontal", &horizontal_profile),
    ] {
        assert!(
            profile.len() > 40,
            "control: the {label} segment covered only {} scanlines — it barely drew, so a \
             width measurement over it would mean nothing",
            profile.len()
        );
        let body = interior(profile);
        let min = *body.iter().min().expect("non-empty");
        let max = *body.iter().max().expect("non-empty");
        eprintln!("{label}: width min={min} max={max}, profile={body:?}");
        // The two hypotheses, both computed from outside this code: a ribbon
        // expanded perpendicular to the segment in screen space has *one* width
        // for the whole segment, so `max - min` is 0 (1 with rasterisation
        // jitter). A bow-tie quad is full width at the endpoints and half width
        // at the midpoint, so at this viewport's 6.4 px line it spans 6 down to
        // 3 and `max - min` is 3. The measurement has to land on one of them.
        assert!(
            max - min <= 1,
            "{label}: the ribbon is {min}..{max} px wide along one segment. A correctly \
             expanded ribbon has one width; a bow-tie quad tapers to half width at the \
             midpoint, which at this viewport is exactly this 6..3 spread. profile={body:?}"
        );
    }

    // Orientation independence: the expansion happens after the projection
    // divide, in pixel space, so a screen-vertical and a screen-horizontal
    // segment at the same depth must rasterise to the same width. A width
    // expanded in world or view space instead varies with orientation, which is
    // what "too thick on the sides" describes.
    let v = interior(&vertical_profile)[interior(&vertical_profile).len() / 2];
    let h = interior(&horizontal_profile)[interior(&horizontal_profile).len() / 2];
    assert!(
        v.abs_diff(h) <= 1,
        "a screen-vertical segment rasterised {v} px wide and a screen-horizontal one at the \
         same depth {h} px — the thickness is orientation-dependent, so it is not being \
         expanded in pixel space"
    );
}
