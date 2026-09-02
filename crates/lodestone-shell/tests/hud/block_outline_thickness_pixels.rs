//! Pixel gate: the block-hit outline is not merely *present* but *thick
//! enough to read*.
//!
//! `gpu.rs`'s own `block_outline_draws_visible_edges` (in the crate's inline
//! test module) already proves the outline pass changes pixels — it passes
//! today and always will, because it only counts changed pixels, which a
//! 1-physical-pixel `LineList` line satisfies just as well as a thick one.
//! It structurally cannot fail on "visible but too thin", which is exactly
//! what was reported live. This gate instead measures the outline's
//! **thickness in pixels**, directly, by finding the longest run of
//! outline-changed pixels along scanlines that cross a straight edge —
//! the pixel-count-with-location idiom `CLAUDE.md` asks for, applied to a
//! run-length instead of a raw total so a wide-but-broken-up outline can't
//! pass by accident.
//!
//! Same differential idiom as `block_outline_draws_visible_edges`: render the
//! identical scene with and without the outline and diff. That is also this
//! gate's control against the documented first-person-bare-arm trap
//! (`sky_pixels.rs`'s `ASSERT_ROWS` doc, `CLAUDE.md`'s "a control's premise
//! can be false" section) — `RenderState::render` draws the bare arm on
//! *every* frame regardless of the outline flag, so it is bit-identical
//! between the two renders and cannot appear in the diff no matter where on
//! screen it sits. No row exclusion is needed here for that reason, unlike
//! `sky_pixels.rs`.
//!
//! ```text
//! cargo test -p lodestone-shell --test block_outline_thickness_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::RenderState;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// Per-pixel colour distance (Manhattan, RGB only) between two frames at the
/// same index.
fn changed(a: &[u8], b: &[u8]) -> Vec<bool> {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(pa, pb)| {
            let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
                + (i32::from(pa[1]) - i32::from(pb[1])).abs()
                + (i32::from(pa[2]) - i32::from(pb[2])).abs();
            d > 20
        })
        .collect()
}

/// Bounding box of the `true` entries in a row-major `w`-wide changed-mask,
/// plus the total count. `None` if nothing changed.
fn bbox_and_count(mask: &[bool], w: u32) -> Option<(u32, u32, u32, u32, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut count = 0usize;
    for (i, &c) in mask.iter().enumerate() {
        if !c {
            continue;
        }
        count += 1;
        let (x, y) = (i as u32 % w, i as u32 / w);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    if count == 0 {
        None
    } else {
        Some((x0, y0, x1, y1, count))
    }
}

/// Lengths of every contiguous run of `true` entries in row `y`.
fn row_runs(mask: &[bool], w: u32, y: u32) -> Vec<u32> {
    let row = &mask[(y * w) as usize..((y + 1) * w) as usize];
    let mut runs = Vec::new();
    let mut run = 0u32;
    for &c in row {
        if c {
            run += 1;
        } else if run > 0 {
            runs.push(run);
            run = 0;
        }
    }
    if run > 0 {
        runs.push(run);
    }
    runs
}

/// Lengths of every contiguous run of `true` entries in column `x`.
fn col_runs(mask: &[bool], w: u32, h: u32, x: u32) -> Vec<u32> {
    let mut runs = Vec::new();
    let mut run = 0u32;
    for y in 0..h {
        if mask[(y * w + x) as usize] {
            run += 1;
        } else if run > 0 {
            runs.push(run);
            run = 0;
        }
    }
    if run > 0 {
        runs.push(run);
    }
    runs
}

/// The pixel *thickness* of the box's roughly-vertical edges (its left and
/// right sides), isolated from their *length*.
///
/// A naive "longest run in any row" is the wrong metric: a scanline through
/// the box's top or bottom row crosses a horizontal edge, whose run spans
/// nearly the whole box width — that's the edge's length, not its thickness,
/// and it dominates a plain max. So this scans only a band of rows in the
/// vertical middle of the box (clear of the top/bottom edges), collects every
/// run in that band, and discards any run that spans more than half the box
/// width — the signature of a scanline that grazed a corner or a
/// near-diagonal edge rather than cutting cleanly through a vertical side.
/// What's left are pure left/right-edge crossings; the max of those is the
/// vertical edge's true pixel thickness.
fn vertical_edge_thickness(mask: &[bool], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> u32 {
    let box_w = x1 - x0 + 1;
    let box_h = y1 - y0 + 1;
    let band = box_h / 4;
    let (lo, hi) = (y0 + band, y1.saturating_sub(band));
    let mut best = 0u32;
    for y in lo..=hi.max(lo) {
        for run in row_runs(mask, w, y) {
            if run < box_w / 2 {
                best = best.max(run);
            }
        }
    }
    best
}

/// As [`vertical_edge_thickness`], transposed: the pixel thickness of the
/// box's roughly-horizontal (top/bottom) edges, isolated from their length by
/// scanning a band of columns in the horizontal middle of the box.
fn horizontal_edge_thickness(mask: &[bool], w: u32, h: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> u32 {
    let box_w = x1 - x0 + 1;
    let box_h = y1 - y0 + 1;
    let band = box_w / 4;
    let (lo, hi) = (x0 + band, x1.saturating_sub(band));
    let mut best = 0u32;
    for x in lo..=hi.max(lo) {
        for run in col_runs(mask, w, h, x) {
            if run < box_h / 2 {
                best = best.max(run);
            }
        }
    }
    best
}

#[test]
#[ignore = "requires a GPU adapter"]
fn block_outline_is_thicker_than_one_physical_pixel() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let world = lodestone::worldgen::generate(2);
    let classifier = lodestone::blocks::DemoClassifier;
    let mut state = RenderState::new(device, queue, format, W, H, None);
    for cz in -2..=2 {
        for cx in -2..=2 {
            for si in 0..lodestone::worldgen::SECTION_COUNT {
                let key = lodestone::mesher::SectionKey {
                    cx,
                    cz,
                    si,
                    min_y: lodestone::worldgen::MIN_Y,
                };
                if let Some(snap) = lodestone::mesher::snapshot_section(&world, key) {
                    let mesh = lodestone::mesher::mesh_snapshot(&snap, &classifier);
                    state.upload_section(
                        device,
                        queue,
                        key,
                        &lodestone::mesher::SectionGeometry::Packed(mesh),
                    );
                }
            }
        }
    }

    // Same framing as `block_outline_draws_visible_edges`: a cube floating in
    // open sky, camera level and facing it head-on, so the cube's near face
    // projects as an axis-aligned rectangle and its edges are (close to)
    // perfectly horizontal/vertical on screen — required for a scanline run
    // length to equal the edge's true pixel thickness rather than a
    // foreshortened diagonal slice of it.
    let target_block = [0i32, lodestone::worldgen::surface_height(0, 0) + 12, 6];
    let camera = Camera {
        position: glam::Vec3::new(0.5, target_block[1] as f32 + 0.5, -2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let plain = target.read_texels(device, queue);

    let frame = target.acquire().expect("acquire");
    state.render(
        device,
        queue,
        frame.view(),
        &camera,
        Some(target_block),
        &[],
    );
    let outlined = target.read_texels(device, queue);

    let mask = changed(&plain, &outlined);
    let (x0, y0, x1, y1, count) =
        bbox_and_count(&mask, W).expect("outline should change at least one pixel");
    let v_thick = vertical_edge_thickness(&mask, W, x0, y0, x1, y1);
    let h_thick = horizontal_edge_thickness(&mask, W, H, x0, y0, x1, y1);

    eprintln!("=== block outline thickness readback ===");
    eprintln!("changed pixels           = {count}");
    eprintln!("bbox                     = x{x0}..{x1} y{y0}..{y1}");
    eprintln!("vertical-edge thickness  = {v_thick}px (left/right sides)");
    eprintln!("horizontal-edge thickness= {h_thick}px (top/bottom sides)");

    // Location control: the outlined cube sits dead ahead of a level camera,
    // so its screen projection must fall well inside the frame, not hug an
    // edge or corner (which would suggest we picked up something else, e.g.
    // a HUD element or an artifact at the frame boundary rather than the
    // wireframe box).
    assert!(
        x0 > 10 && x1 < W - 10 && y0 > 10 && y1 < H - 10,
        "changed-pixel bbox x{x0}..{x1} y{y0}..{y1} touches the frame border — \
         expected the outline to sit well inside a {W}x{H} frame; \
         re-check the scene/camera rather than trusting this as the outline"
    );

    // The actual legibility assertion. A `PrimitiveTopology::LineList` line
    // rasterizes at exactly one physical pixel of thickness regardless of
    // resolution (mode of the old `outline.rs`, that fix) — GPU
    // rasterizers only occasionally cover a second pixel row/column at a
    // sub-pixel edge straddle, so an honest measurement of that geometry
    // tops out at 1, rarely 2. Vanilla's real (non-debug) hit outline uses
    // `Window.getAppropriateLineWidth` (`Window.java`,
    // `max(2.5, windowWidth / 1920 * 2.5)`), i.e. never thinner than 2.5
    // logical pixels. `>= 3` is comfortably above what a bare `LineList`
    // could produce and comfortably at/under vanilla's own floor, so it
    // discriminates "thickness fixed" from "thickness untouched" without
    // being so tight it flakes on antialiasing.
    assert!(
        v_thick >= 3,
        "vertical (left/right) outline edges are only {v_thick}px thick — a \
         1-physical-pixel LineList line reads exactly this thin; the wireframe is \
         not resolvable as a legibly thick line (issue #364)"
    );
    assert!(
        h_thick >= 3,
        "horizontal (top/bottom) outline edges are only {h_thick}px thick — a \
         1-physical-pixel LineList line reads exactly this thin; the wireframe is \
         not resolvable as a legibly thick line (issue #364)"
    );
}
