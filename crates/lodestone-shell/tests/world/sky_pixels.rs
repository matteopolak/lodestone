//! Pixel gate: the sky pass reaches the screen through the **shell's real
//! render path** (`RenderState::render`), not just `SkyRenderer::render` in
//! isolation.
//!
//! `crates/lodestone-render/tests/sky_pipeline_gpu.rs` already proves the four
//! sky sub-pipelines paint pixels when driven directly — that is the pipeline
//! working. It says nothing about whether `RenderState::install_sky` /
//! `set_time_of_day_source` / the sky-before-block-pass ordering /
//! the `Clear`-vs-`Load` handoff in `gpu.rs::render_inner` are actually wired,
//! which is exactly the "individually built, individually tested, reaches zero
//! pixels" island shape `CLAUDE.md` names. This gate goes through
//! [`RenderState::render`] instead, with **no terrain** uploaded (no sections,
//! no entities) so every pixel in frame is either the sky pass's own paint or
//! the block pass's fallback clear — nothing else can produce a pixel.
//!
//! Camera looks steeply up (`pitch = -60`, matching the render-crate gate),
//! so the sky dome fills the frame and "coverage inside the sky's screen
//! rect" is simply "coverage of the frame".
//!
//! A synthetic in-memory pack (solid-colour sun/moon/cloud textures), not the
//! real jar — this gate is about proving the *wiring* reaches pixels, not
//! about vanilla art fidelity (the same call `sky_pipeline_gpu.rs` makes), and
//! it means this gate has no `client.jar` dependency at all.
//!
//! ```text
//! cargo test -p lodestone-shell --test sky_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_assets::{MemorySource, ResourceManager};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget, SkyRenderer};

const W: u32 = 256;
const H: u32 = 256;

fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(&buf).unwrap();
    }
    out
}

/// A synthetic pack with an opaque sun, all 8 moon phases, and an opaque
/// cloud texture — the same shape `sky_pipeline_gpu.rs` uses, so a change to
/// either gate's expectations should change both together.
fn sky_manager() -> ResourceManager {
    let mut src = MemorySource::new("shell-sky-pixel-gate");
    src.insert(
        "assets/minecraft/textures/environment/celestial/sun.png".to_string(),
        png(8, 8, [255, 220, 0, 255]),
    );
    for name in lodestone_assets::MOON_PHASE_NAMES {
        src.insert(
            format!("assets/minecraft/textures/environment/celestial/moon/{name}.png"),
            png(8, 8, [200, 200, 200, 255]),
        );
    }
    src.insert(
        "assets/minecraft/textures/environment/clouds.png".to_string(),
        png(4, 4, [255, 255, 255, 255]),
    );
    ResourceManager::new(vec![Box::new(src)])
}

/// Looking straight up keeps the whole 90-degree frustum inside the sky dome
/// (a flat plane 16 units above the camera) — see
/// `sky_pipeline_gpu.rs::sky_pass_paints_the_whole_frame`'s identical
/// reasoning for why a level camera would be the wrong shape for this gate.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0,
        pitch: -60.0,
        fov_y_degrees: 90.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 1024.0,
    }
}

fn sky_color_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

/// Rows below this are **excluded** from every assertion in this file, because
/// `RenderState::render` draws the first-person bare arm into the bottom-right
/// corner unconditionally — `gpu.rs`'s hand pass is gated only on
/// `third_person_body_drawn`, so first person always has an arm whether or not
/// anything else is installed.
///
/// This was measured, not assumed. The first version of this gate asserted that
/// a sky-less frame clears *uniformly* to `SKY_COLOR` and failed at 3.5%; a
/// location report put the offending pixels at `x221..255 y180..255` in dark
/// browns, i.e. the arm. The premise was false before the sky existed. The
/// assertions below therefore measure **inside the sky's own screen rect**,
/// which is what `CLAUDE.md` asks of a coverage gate, and
/// [`arm_is_what_we_excluded`] pins the reason so a future reader does not have
/// to trust this comment.
const ASSERT_ROWS: u32 = 160;

/// As [`differs_fraction`], restricted to the top [`ASSERT_ROWS`] rows.
fn differs_fraction_in_rect(pixels: &[u8], reference: [u8; 3], w: u32) -> f64 {
    let mut differs = 0usize;
    let mut total = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        if i as u32 / w >= ASSERT_ROWS {
            continue;
        }
        total += 1;
        let d = (i32::from(px[0]) - i32::from(reference[0])).abs()
            + (i32::from(px[1]) - i32::from(reference[1])).abs()
            + (i32::from(px[2]) - i32::from(reference[2])).abs();
        if d > 12 {
            differs += 1;
        }
    }
    differs as f64 / total.max(1) as f64
}

/// Where the differing pixels are, and what colours they actually hold.
///
/// `differs_fraction` alone cannot tell a uniform-but-wrong clear from a
/// localised blob, and this repo has a documented case (`DESIGN.md` §12) where
/// averaging a frame produced a confident wrong conclusion that clustering by
/// *location* immediately overturned. So when a control here fails, report the
/// bounding box and the colour histogram rather than a percentage.
fn differs_report(pixels: &[u8], reference: [u8; 3], w: u32) -> String {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut counts: std::collections::BTreeMap<[u8; 3], usize> = std::collections::BTreeMap::new();
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let d = (i32::from(px[0]) - i32::from(reference[0])).abs()
            + (i32::from(px[1]) - i32::from(reference[1])).abs()
            + (i32::from(px[2]) - i32::from(reference[2])).abs();
        if d > 12 {
            let (x, y) = (i as u32 % w, i as u32 / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            *counts.entry([px[0], px[1], px[2]]).or_default() += 1;
        }
    }
    if counts.is_empty() {
        return "no differing pixels".to_string();
    }
    let mut top: Vec<_> = counts.into_iter().collect();
    top.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    let colours: Vec<String> = top
        .iter()
        .take(4)
        .map(|(c, n)| format!("{c:?}x{n}"))
        .collect();
    format!(
        "bbox x{x0}..{x1} y{y0}..{y1}, reference {reference:?}, top colours: {}",
        colours.join(" ")
    )
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_sky_pass_reaches_the_screen_through_render_state_render() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();
    let reference = sky_color_bytes();

    // ------------------------------------------------------------------
    // Positive: sky installed, midnight (`time_of_day = 18_000`) — stars and
    // moon both active, same as the render-crate gate's own choice, so this
    // exercises every one of the four draws through the real shell path.
    // ------------------------------------------------------------------
    let mut lit = RenderState::new(device, queue, format, W, H, None);
    let sky = SkyRenderer::new(device, queue, format, &sky_manager())
        .expect("build sky renderer over the synthetic pack");
    lit.install_sky(sky);
    lit.set_time_of_day_source(|| Some(18_000));

    let frame = target.acquire().expect("headless acquire");
    let stats = lit.render(device, queue, frame.view(), &cam, None, &[]);
    let lit_pixels = target.read_texels(device, queue);
    let lit_frac = differs_fraction_in_rect(&lit_pixels, reference, W);

    // ------------------------------------------------------------------
    // Control, EXECUTED: no sky installed at all — same camera, same empty
    // world. The block pass must clear straight to `SKY_COLOR`, uniformly —
    // getting the `Clear`-vs-`Load` handoff wrong here (unconditional `Load`)
    // would leave this frame reading back as the *untouched* target (black),
    // not `SKY_COLOR`, which this control also catches.
    // ------------------------------------------------------------------
    let dark = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("headless acquire");
    let dark_stats = dark.render(device, queue, frame.view(), &cam, None, &[]);
    let dark_pixels = target.read_texels(device, queue);
    let dark_frac = differs_fraction_in_rect(&dark_pixels, reference, W);

    eprintln!("=== sky wiring pixel gate (through RenderState::render) ===");
    eprintln!(
        "sky installed, midnight: sky_drawn={}, {:.1}% of frame differs from SKY_COLOR",
        stats.sky_drawn,
        lit_frac * 100.0
    );
    eprintln!(
        "control, no sky installed: sky_drawn={}, {:.1}% of frame differs from SKY_COLOR",
        dark_stats.sky_drawn,
        dark_frac * 100.0
    );
    eprintln!("control detail: {}", differs_report(&dark_pixels, reference, W));

    assert!(
        stats.sky_drawn,
        "RenderState::render reported sky_drawn=false with a sky installed — \
         install_sky did not take, or render_inner is not checking self.sky"
    );
    assert!(
        lit_frac > 0.5,
        "expected the sky pass to paint most of the frame through the real render \
         path (disc alone is opaque and covers the FOV), only {:.1}% differed from \
         SKY_COLOR",
        lit_frac * 100.0
    );

    assert!(
        !dark_stats.sky_drawn,
        "the control installed no sky, but sky_drawn=true — RenderState::new must not \
         default to an installed sky"
    );
    assert!(
        dark_frac < 0.02,
        "control failed to fail: with no sky installed the top {ASSERT_ROWS} rows must \
         clear uniformly to SKY_COLOR (the pre-existing behaviour), but {:.1}% of them \
         differ — either \
         the block pass is wrongly `Load`ing over an untouched target, or something \
         else is painting without a sky installed",
        dark_frac * 100.0
    );
}

/// Fraction of pixels that are near-black (`NIGHT` in `sky_color_for_time_of_day`
/// is `[0.006, 0.008, 0.02]`, i.e. bytes `~[2, 2, 5]`) — the discriminator for
/// the noon-vs-midnight control below. Deliberately **not** "differs from
/// `SKY_COLOR`": the cloud plane's own colour (an opaque-white cloud tint at
/// alpha 0.8, composited over the disc) means even a noon frame differs from
/// an exact `SKY_COLOR` clear wherever clouds cover the
/// frustum, which would make that comparison fail on a correctly-wired sky,
/// not just a broken one. Near-black has no such ambiguity: neither the day
/// disc nor the day clouds are anywhere near it, only the night blend is. Night
/// clouds carry their own `rgb(25, 25, 38)` track, whose linear value is
/// `~0.010`–`0.019`, i.e.
/// bytes `[3, 3, 5]` on this test's non-sRGB target — still comfortably inside
/// the `< 20` threshold.
fn near_black_fraction(pixels: &[u8]) -> f64 {
    let mut near_black = 0usize;
    let mut total = 0usize;
    for px in pixels.chunks_exact(4) {
        total += 1;
        if px[0] < 20 && px[1] < 20 && px[2] < 20 {
            near_black += 1;
        }
    }
    near_black as f64 / total.max(1) as f64
}

/// **Second control, EXECUTED**: a sky *is* installed, but at noon
/// (`time_of_day = 6000`). `star_brightness_for_time_of_day(6000) == 0` (no
/// stars) and the disc/cloud colours sit at the *day* end of
/// `sky_color_for_time_of_day`'s blend, nowhere near the `NIGHT` constant the
/// midnight frame above is dominated by. This proves the midnight test's
/// darkness is the sky pass *responding to the clock*, not merely "a sky
/// object exists and paints something dark regardless of the hour".
#[test]
#[ignore = "requires a GPU adapter"]
fn noon_paints_no_night_darkness_where_midnight_does() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut noon = RenderState::new(device, queue, format, W, H, None);
    let sky = SkyRenderer::new(device, queue, format, &sky_manager())
        .expect("build sky renderer over the synthetic pack");
    noon.install_sky(sky);
    noon.set_time_of_day_source(|| Some(6_000));

    let frame = target.acquire().expect("headless acquire");
    let noon_stats = noon.render(device, queue, frame.view(), &cam, None, &[]);
    let noon_pixels = target.read_texels(device, queue);
    let noon_dark_frac = near_black_fraction(&noon_pixels);

    // Same sky object, same camera, only the clock differs — the midnight
    // shot this control is measured against.
    let mut midnight = RenderState::new(device, queue, format, W, H, None);
    let sky = SkyRenderer::new(device, queue, format, &sky_manager())
        .expect("build sky renderer over the synthetic pack");
    midnight.install_sky(sky);
    midnight.set_time_of_day_source(|| Some(18_000));

    let frame = target.acquire().expect("headless acquire");
    let midnight_stats = midnight.render(device, queue, frame.view(), &cam, None, &[]);
    let midnight_pixels = target.read_texels(device, queue);
    let midnight_dark_frac = near_black_fraction(&midnight_pixels);

    eprintln!("=== sky wiring pixel gate: noon vs midnight control ===");
    eprintln!(
        "noon:     sky_drawn={}, {:.1}% near-black",
        noon_stats.sky_drawn,
        noon_dark_frac * 100.0
    );
    eprintln!(
        "midnight: sky_drawn={}, {:.1}% near-black",
        midnight_stats.sky_drawn,
        midnight_dark_frac * 100.0
    );

    assert!(noon_stats.sky_drawn && midnight_stats.sky_drawn);
    assert!(
        midnight_dark_frac > 0.5,
        "expected midnight's disc+cloud paint to be predominantly near-black \
         (NIGHT ~= [2,2,5]), got only {:.1}%",
        midnight_dark_frac * 100.0
    );
    assert!(
        noon_dark_frac < 0.05,
        "control failed to fail: noon must not paint substantial near-black \
         area (got {:.1}%) — if this fails, the sky pass is not actually \
         responding to time_of_day",
        noon_dark_frac * 100.0
    );
}

/// Pins the reason [`ASSERT_ROWS`] exists: the excluded rows contain the
/// first-person bare arm, drawn by `RenderState::render` with nothing installed
/// at all.
///
/// Without this, `ASSERT_ROWS` is an unexplained magic number and the next
/// person to widen it re-discovers the 3.5% failure from scratch. With it, the
/// exclusion is a measurement: the arm is reported drawn, its pixels sit
/// **below** the assertion window, and the window itself is clean.
#[test]
#[ignore = "requires a GPU adapter"]
fn arm_is_what_we_excluded() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();
    let reference = sky_color_bytes();

    let state = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &cam, None, &[]);
    let pixels = target.read_texels(device, queue);

    eprintln!("=== what ASSERT_ROWS excludes ===");
    eprintln!(
        "arm_drawn={}, item_drawn={}, sky_drawn={}",
        stats.first_person_arm_drawn, stats.first_person_item_drawn, stats.sky_drawn
    );
    eprintln!("whole frame: {}", differs_report(&pixels, reference, W));

    assert!(
        stats.first_person_arm_drawn,
        "the excluded rows are excluded *because* the bare arm draws there; if the arm \
         no longer draws with nothing installed, ASSERT_ROWS should shrink back to the \
         full frame rather than silently keep hiding whatever is there now"
    );
    assert!(
        !stats.sky_drawn,
        "no sky was installed, so nothing should report having drawn one"
    );

    // The whole point: everything that differs is below the window we assert in.
    let mut lowest_differing = u32::MAX;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let d = (i32::from(px[0]) - i32::from(reference[0])).abs()
            + (i32::from(px[1]) - i32::from(reference[1])).abs()
            + (i32::from(px[2]) - i32::from(reference[2])).abs();
        if d > 12 {
            lowest_differing = lowest_differing.min(i as u32 / W);
        }
    }
    assert!(
        lowest_differing >= ASSERT_ROWS,
        "something paints at row {lowest_differing}, inside the assertion window \
         (rows 0..{ASSERT_ROWS}) — the sky gate's control would then be measuring that \
         instead of the sky, so either move ASSERT_ROWS up or find out what is drawing"
    );
}
