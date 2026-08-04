//! Pixel gate: the underwater/fire overlay pass (issues #108, #112) reaches
//! the screen through the **shell's real render path**
//! (`RenderState::render_with_effects`/`render_with_crack_and_effects`), not
//! just `ScreenEffectRenderer` in isolation.
//!
//! `crates/lodestone-render/tests/screen_effects_pipeline_gpu.rs` already
//! proves the pipeline paints pixels when driven directly — that is the
//! pipeline working. It says nothing about whether `RenderState::render_inner`
//! actually checks `self.screen_effects`/`ScreenEffects::eye_in_water`/
//! `ScreenEffects::on_fire`, which is exactly the "individually built,
//! individually tested, reaches zero pixels" island shape `CLAUDE.md` names.
//! This gate goes through `RenderState::render_with_effects` instead, with an
//! empty world (no sections, no entities) so nothing else in the frame can
//! produce the measured pixels.
//!
//! # Comparing frames to each other, not to a fixed reference colour
//!
//! `RenderState` draws the first-person bare arm into a corner unconditionally
//! whenever no third-person body is installed (`sky_pixels.rs` measured this
//! the hard way: a control asserting a uniform clear failed at 3.5%, entirely
//! from the arm). Both this file's overlay passes draw *after* the hand pass
//! and cover the pixels it painted too, so rather than re-deriving which rect
//! the arm occupies at this resolution, every assertion here diffs the
//! **positive** frame against a **negative control** frame from the same
//! camera and the same (installed) pass, with only the `ScreenEffects` flag
//! toggled. Whatever the arm painted is present, identically, in both frames,
//! so it cancels out of the diff and cannot produce a false positive.
//!
//! # What "on_fire" being real here does and does not prove
//!
//! `app.rs`'s real per-frame call always passes `on_fire: false` today — see
//! `docs/screen-overlays.md`'s "what does not reach the shell yet" section:
//! the local player's on-fire bit decodes in `metadata.rs` but no
//! session-scoped fold carries it to `PlayerSnapshot`, unlike `air_supply`.
//! The fire assertions below prove the **pass and its wiring into
//! `render_inner`** are correct — `on_fire: true` really does paint the
//! bottom strip through the real render path — which is the mechanism this
//! gate exists to check. It does not and cannot prove production ever passes
//! `true`, because today it structurally cannot.
//!
//! ```text
//! cargo test -p lodestone-shell --test screen_overlay_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, ScreenEffects};
use lodestone_assets::{MemorySource, ResourceManager};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget, ScreenEffectRenderer};

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

/// A synthetic pack: an opaque white `underwater.png` (unambiguous against
/// any background) and an opaque orange 32-frame `fire_1.png` strip.
fn manager() -> ResourceManager {
    let mut src = MemorySource::new("screen-overlay-shell-gate");
    src.insert(
        "assets/minecraft/textures/misc/underwater.png".to_string(),
        png(16, 16, [255, 255, 255, 255]),
    );
    src.insert(
        "assets/minecraft/textures/block/fire_1.png".to_string(),
        png(16, 16 * 32, [230, 130, 20, 255]),
    );
    src.insert(
        "assets/minecraft/textures/misc/pumpkinblur.png".to_string(),
        png(16, 16, [40, 200, 40, 255]),
    );
    ResourceManager::new(vec![Box::new(src)])
}

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 90.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 1024.0,
    }
}

fn ctx() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

/// Fraction of pixels that differ between two same-sized RGBA8 buffers.
fn differs_fraction(a: &[u8], b: &[u8]) -> f64 {
    let mut differs = 0usize;
    let mut total = 0usize;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        total += 1;
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d > 12 {
            differs += 1;
        }
    }
    differs as f64 / total.max(1) as f64
}

/// Bounding box of the pixels that differ, for a failure report — a fraction
/// alone cannot distinguish a uniform-but-wrong frame from a localised blob
/// (`CLAUDE.md`'s "measure by location" rule).
fn differs_bbox(a: &[u8], b: &[u8], w: u32) -> String {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut n = 0usize;
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d > 12 {
            let (x, y) = (i as u32 % w, i as u32 / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            n += 1;
        }
    }
    if n == 0 {
        "no differing pixels".to_string()
    } else {
        format!("bbox x{x0}..{x1} y{y0}..{y1}, {n} pixels differ")
    }
}

/// The underwater overlay covers the full NDC screen (see
/// `underwater_overlay_quad`'s doc), so toggling `eye_in_water` with the pass
/// installed must change essentially the whole frame.
#[test]
#[ignore = "requires a GPU adapter"]
fn underwater_overlay_reaches_the_screen_through_render_with_effects() {
    let ctx = ctx();
    let (device, queue) = (ctx.device(), ctx.queue());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut wet = RenderState::new(device, queue, format, W, H, None);
    wet.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let wet_stats = wet.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            eye_in_water: true,
            ..ScreenEffects::default()
        },
    );
    let wet_pixels = target.read_texels(device, queue);

    // Control A, EXECUTED: same installed pass, `eye_in_water: false`. Proves
    // the *flag*, not just installation, gates the draw.
    let mut dry = RenderState::new(device, queue, format, W, H, None);
    dry.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let dry_stats = dry.render_with_effects(device, queue, frame.view(), &cam, None, &[], ScreenEffects::default());
    let dry_pixels = target.read_texels(device, queue);

    // Control B, EXECUTED: `eye_in_water: true` but no pass installed at all.
    // Proves `RenderState::new` does not spontaneously draw an overlay.
    let uninstalled = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("acquire");
    let uninstalled_stats = uninstalled.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            eye_in_water: true,
            ..ScreenEffects::default()
        },
    );
    let uninstalled_pixels = target.read_texels(device, queue);

    let wet_vs_dry = differs_fraction(&wet_pixels, &dry_pixels);
    let wet_vs_uninstalled = differs_fraction(&wet_pixels, &uninstalled_pixels);

    eprintln!("=== underwater overlay pixel gate (through RenderState::render_with_effects) ===");
    eprintln!(
        "eye_in_water=true: underwater_overlay_drawn={}, differs from dry control by {:.1}%",
        wet_stats.underwater_overlay_drawn,
        wet_vs_dry * 100.0
    );
    eprintln!(
        "control A (installed, eye_in_water=false): underwater_overlay_drawn={}",
        dry_stats.underwater_overlay_drawn
    );
    eprintln!(
        "control B (not installed, eye_in_water=true): underwater_overlay_drawn={}, differs from wet by {:.1}%",
        uninstalled_stats.underwater_overlay_drawn,
        wet_vs_uninstalled * 100.0
    );
    if wet_vs_dry < 0.9 {
        eprintln!("wet-vs-dry detail: {}", differs_bbox(&wet_pixels, &dry_pixels, W));
    }

    assert!(wet_stats.underwater_overlay_drawn, "eye_in_water=true must draw the overlay");
    assert!(
        !dry_stats.underwater_overlay_drawn,
        "control A failed to fail: eye_in_water=false must not draw the overlay"
    );
    assert!(
        !uninstalled_stats.underwater_overlay_drawn,
        "control B failed to fail: no pass installed must not draw the overlay"
    );
    assert!(
        wet_vs_dry > 0.9,
        "expected the underwater overlay to change ~the whole frame vs the dry control, \
         only {:.1}% differed",
        wet_vs_dry * 100.0
    );
    assert!(
        wet_vs_uninstalled > 0.9,
        "expected the underwater overlay to change ~the whole frame vs the uninstalled \
         control, only {:.1}% differed",
        wet_vs_uninstalled * 100.0
    );
}

/// The fire overlay covers only the bottom strip (`FIRE_STRIP_TOP = -0.3`,
/// ~35% of the frame height), so this checks a bounding box, not a
/// frame-average — the same discipline `CLAUDE.md` asks for after the sky/HUD
/// gates were fooled by a percentage once each.
#[test]
#[ignore = "requires a GPU adapter"]
fn fire_overlay_reaches_only_the_bottom_strip_through_render_with_effects() {
    let ctx = ctx();
    let (device, queue) = (ctx.device(), ctx.queue());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut burning = RenderState::new(device, queue, format, W, H, None);
    burning.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let burning_stats = burning.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            on_fire: true,
            tick: 5,
            ..ScreenEffects::default()
        },
    );
    let burning_pixels = target.read_texels(device, queue);

    // Control, EXECUTED: same installed pass, `on_fire: false`.
    let mut not_burning = RenderState::new(device, queue, format, W, H, None);
    not_burning.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let not_burning_stats =
        not_burning.render_with_effects(device, queue, frame.view(), &cam, None, &[], ScreenEffects::default());
    let not_burning_pixels = target.read_texels(device, queue);

    // Row bands: `FIRE_STRIP_TOP = -0.3` NDC is 35% of the way up from the
    // bottom of the frame, i.e. the bottom 35% of rows.
    let strip_rows = ((H as f64) * 0.35) as u32;
    let top_rows = H - strip_rows;

    let top_frac = differs_fraction(
        &burning_pixels[..(top_rows * W * 4) as usize],
        &not_burning_pixels[..(top_rows * W * 4) as usize],
    );
    let bottom_frac = differs_fraction(
        &burning_pixels[(top_rows * W * 4) as usize..],
        &not_burning_pixels[(top_rows * W * 4) as usize..],
    );

    eprintln!("=== fire overlay pixel gate (through RenderState::render_with_effects) ===");
    eprintln!(
        "on_fire=true: fire_overlay_drawn={}, top rows differ {:.1}%, bottom rows differ {:.1}%",
        burning_stats.fire_overlay_drawn,
        top_frac * 100.0,
        bottom_frac * 100.0
    );
    eprintln!(
        "control (installed, on_fire=false): fire_overlay_drawn={}",
        not_burning_stats.fire_overlay_drawn
    );
    if top_frac > 0.01 {
        eprintln!(
            "unexpected top-row difference: {}",
            differs_bbox(&burning_pixels, &not_burning_pixels, W)
        );
    }

    assert!(burning_stats.fire_overlay_drawn, "on_fire=true must draw the overlay");
    assert!(
        !not_burning_stats.fire_overlay_drawn,
        "control failed to fail: on_fire=false must not draw the overlay"
    );
    assert!(
        bottom_frac > 0.3,
        "expected the fire overlay to change the bottom strip, only {:.1}% differed",
        bottom_frac * 100.0
    );
    assert!(
        top_frac < 0.01,
        "fire overlay must not paint above its strip: {:.1}% of the top rows differ \
         (bounding-box check per CLAUDE.md — a frame-average could not have caught this)",
        top_frac * 100.0
    );
}

/// Spectator mode suppresses both overlays even when both flags are set,
/// matching vanilla's `!this.minecraft.player.isSpectator()` gate in
/// `ScreenEffectRenderer.submit`.
#[test]
#[ignore = "requires a GPU adapter"]
fn spectator_suppresses_both_overlays() {
    let ctx = ctx();
    let (device, queue) = (ctx.device(), ctx.queue());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut render = RenderState::new(device, queue, format, W, H, None);
    render.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let stats = render.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            eye_in_water: true,
            on_fire: true,
            spectator: true,
            tick: 0,
            wearing_pumpkin: true,
        },
    );

    eprintln!("=== spectator control ===");
    eprintln!(
        "spectator=true, eye_in_water=true, on_fire=true, wearing_pumpkin=true: \
         underwater_overlay_drawn={}, fire_overlay_drawn={}, pumpkin_overlay_drawn={}",
        stats.underwater_overlay_drawn, stats.fire_overlay_drawn, stats.pumpkin_overlay_drawn
    );

    assert!(
        !stats.underwater_overlay_drawn,
        "a spectator must not draw the underwater overlay even with eye_in_water=true"
    );
    assert!(
        !stats.fire_overlay_drawn,
        "a spectator must not draw the fire overlay even with on_fire=true"
    );
    assert!(
        !stats.pumpkin_overlay_drawn,
        "a spectator must not draw the pumpkin overlay even with wearing_pumpkin=true"
    );
}

/// The pumpkin overlay (issue #185) covers the full NDC screen like the
/// underwater overlay (see `pumpkin_overlay_triangles`'s doc), so toggling
/// `wearing_pumpkin` with the pass installed must change essentially the
/// whole frame, through the real `RenderState::render_with_effects` path —
/// not just `ScreenEffectRenderer::draw_pumpkin` in isolation.
#[test]
#[ignore = "requires a GPU adapter"]
fn pumpkin_overlay_reaches_the_screen_through_render_with_effects() {
    let ctx = ctx();
    let (device, queue) = (ctx.device(), ctx.queue());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut wearing = RenderState::new(device, queue, format, W, H, None);
    wearing.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let wearing_stats = wearing.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            wearing_pumpkin: true,
            ..ScreenEffects::default()
        },
    );
    let wearing_pixels = target.read_texels(device, queue);

    // Control A, EXECUTED: same installed pass, `wearing_pumpkin: false`.
    // Proves the *flag*, not just installation, gates the draw.
    let mut bare = RenderState::new(device, queue, format, W, H, None);
    bare.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let bare_stats =
        bare.render_with_effects(device, queue, frame.view(), &cam, None, &[], ScreenEffects::default());
    let bare_pixels = target.read_texels(device, queue);

    // Control B, EXECUTED: `wearing_pumpkin: true` but no pass installed.
    // Proves `RenderState::new` does not spontaneously draw an overlay.
    let uninstalled = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("acquire");
    let uninstalled_stats = uninstalled.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            wearing_pumpkin: true,
            ..ScreenEffects::default()
        },
    );
    let uninstalled_pixels = target.read_texels(device, queue);

    let wearing_vs_bare = differs_fraction(&wearing_pixels, &bare_pixels);
    let wearing_vs_uninstalled = differs_fraction(&wearing_pixels, &uninstalled_pixels);

    eprintln!("=== pumpkin overlay pixel gate (through RenderState::render_with_effects) ===");
    eprintln!(
        "wearing_pumpkin=true: pumpkin_overlay_drawn={}, differs from bare control by {:.1}%",
        wearing_stats.pumpkin_overlay_drawn,
        wearing_vs_bare * 100.0
    );
    eprintln!(
        "control A (installed, wearing_pumpkin=false): pumpkin_overlay_drawn={}",
        bare_stats.pumpkin_overlay_drawn
    );
    eprintln!(
        "control B (not installed, wearing_pumpkin=true): pumpkin_overlay_drawn={}, differs from worn by {:.1}%",
        uninstalled_stats.pumpkin_overlay_drawn,
        wearing_vs_uninstalled * 100.0
    );
    if wearing_vs_bare < 0.9 {
        eprintln!("worn-vs-bare detail: {}", differs_bbox(&wearing_pixels, &bare_pixels, W));
    }

    assert!(wearing_stats.pumpkin_overlay_drawn, "wearing_pumpkin=true must draw the overlay");
    assert!(
        !bare_stats.pumpkin_overlay_drawn,
        "control A failed to fail: wearing_pumpkin=false must not draw the overlay"
    );
    assert!(
        !uninstalled_stats.pumpkin_overlay_drawn,
        "control B failed to fail: no pass installed must not draw the overlay"
    );
    assert!(
        wearing_vs_bare > 0.9,
        "expected the pumpkin overlay to change ~the whole frame vs the bare control, \
         only {:.1}% differed",
        wearing_vs_bare * 100.0
    );
    assert!(
        wearing_vs_uninstalled > 0.9,
        "expected the pumpkin overlay to change ~the whole frame vs the uninstalled \
         control, only {:.1}% differed",
        wearing_vs_uninstalled * 100.0
    );
}
