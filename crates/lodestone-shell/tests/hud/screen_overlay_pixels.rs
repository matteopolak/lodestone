//! Pixel gate: the underwater/fire overlay pass reaches
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
use lodestone_render::screen_effects::fire_overlay_vertical_extent;
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

fn border_vignette_png() -> Vec<u8> {
    let mut buf = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let edge = x < 32 || x >= W - 32 || y < 32 || y >= H - 32;
            let rgba = if edge { [255, 255, 255, 255] } else { [0, 0, 0, 255] };
            let offset = ((y * W + x) * 4) as usize;
            buf[offset..offset + 4].copy_from_slice(&rgba);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, W, H);
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
    src.insert(
        "assets/minecraft/textures/misc/powder_snow_outline.png".to_string(),
        png(256, 256, [200, 230, 255, 255]),
    );
    src.insert(
        "assets/minecraft/textures/misc/spyglass_scope.png".to_string(),
        png(256, 256, [180, 180, 180, 255]),
    );
    src.insert(
        "assets/minecraft/textures/misc/nausea.png".to_string(),
        png(256, 256, [255, 255, 255, 255]),
    );
    src.insert(
        "assets/minecraft/textures/block/nether_portal.png".to_string(),
        png(16, 16 * 32, [200, 40, 200, 255]),
    );
    src.insert(
        "assets/minecraft/textures/misc/vignette.png".to_string(),
        border_vignette_png(),
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

/// The existing border-strength producer reaches a dedicated render draw,
/// while the exact zero-strength mutation opens no pass. The synthetic white
/// mask changes the whole frame, making a missing route impossible to hide in
/// the first-person arm's small pixel footprint.
#[test]
#[ignore = "requires a GPU adapter"]
fn border_warning_strength_reaches_pixels_and_zero_is_a_real_control() {
    let ctx = ctx();
    let (device, queue) = (ctx.device(), ctx.queue());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut warning = RenderState::new(device, queue, format, W, H, None);
    warning.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let warning_stats = warning.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            wearing_pumpkin: true,
            border_warning_strength: 0.5,
            ..ScreenEffects::default()
        },
    );
    let warning_pixels = target.read_texels(device, queue);

    let mut zero = RenderState::new(device, queue, format, W, H, None);
    zero.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    let frame = target.acquire().expect("acquire");
    let zero_stats = zero.render_with_effects(
        device,
        queue,
        frame.view(),
        &cam,
        None,
        &[],
        ScreenEffects {
            wearing_pumpkin: true,
            border_warning_strength: 0.0,
            ..ScreenEffects::default()
        },
    );
    let zero_pixels = target.read_texels(device, queue);

    let changed = differs_fraction(&warning_pixels, &zero_pixels);
    let mut edge_changed = 0usize;
    let mut edge_total = 0usize;
    let mut centre_changed = 0usize;
    let mut centre_total = 0usize;
    for y in 0..H {
        for x in 0..W {
            let offset = ((y * W + x) * 4) as usize;
            let a = &warning_pixels[offset..offset + 4];
            let b = &zero_pixels[offset..offset + 4];
            let delta = (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs();
            if x < 28 || x >= W - 28 || y < 28 || y >= H - 28 {
                edge_total += 1;
                edge_changed += usize::from(delta > 12);
            } else if x >= 40 && x < W - 40 && y >= 40 && y < H - 40 {
                centre_total += 1;
                centre_changed += usize::from(delta > 12);
            }
        }
    }
    let edge_fraction = edge_changed as f64 / edge_total as f64;
    let centre_fraction = centre_changed as f64 / centre_total as f64;
    assert!(warning_stats.border_warning_overlay_drawn, "positive strength must issue the warning draw");
    assert!(!zero_stats.border_warning_overlay_drawn, "zero-strength mutation must issue no warning draw");
    assert!(
        edge_fraction > 0.95,
        "warning should change the synthetic mask's edge, only {:.1}% differed ({})",
        edge_fraction * 100.0,
        differs_bbox(&warning_pixels, &zero_pixels, W),
    );
    assert!(
        centre_fraction < 0.01,
        "warning must leave the black centre of the mask unchanged, {:.1}% differed; whole-frame {:.1}%",
        centre_fraction * 100.0,
        changed * 100.0,
    );
}

/// The fire overlay, through the real `render_with_effects` path, must
/// change rows matching its real two-quad geometry's predicted vertical
/// extent (`fire_overlay_vertical_extent`, that fix) — not the retired
/// hardcoded bottom-35% strip — and must leave rows above that extent
/// unchanged. Checks row bands against a predicted boundary, not a
/// frame-average — the same discipline `CLAUDE.md` asks for after the
/// sky/HUD gates were fooled by a percentage once each.
#[test]
#[ignore = "requires a GPU adapter"]
fn fire_overlay_reaches_its_predicted_extent_through_render_with_effects() {
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

    // Row bands, predicted from the real geometry's own constants rather
    // than a restated decimal (replaced a hardcoded
    // `FIRE_STRIP_TOP = -0.3` bottom-35% strip with vanilla's real two-quad
    // transform).
    let (_predicted_min_ndc, predicted_max_ndc) = fire_overlay_vertical_extent();
    let frac_from_bottom = (f64::from(predicted_max_ndc) + 1.0) / 2.0;
    let predicted_top_row = ((H as f64) * (1.0 - frac_from_bottom)) as u32;
    const MARGIN_ROWS: u32 = 2;
    let untouched_rows = predicted_top_row.saturating_sub(MARGIN_ROWS);
    let lit_from_row = predicted_top_row + MARGIN_ROWS;
    // The old design's own top edge (35% up from the bottom) — the rejected
    // hypothesis this test falsifies via the "reclaimed" band below.
    let old_design_top_row = ((H as f64) * 0.65) as u32;
    assert!(
        untouched_rows < old_design_top_row,
        "sanity: the real extent's top ({untouched_rows}) must sit above the old \
         strip's top ({old_design_top_row}), or this test cannot tell the two apart"
    );

    let top_frac = differs_fraction(
        &burning_pixels[..(untouched_rows * W * 4) as usize],
        &not_burning_pixels[..(untouched_rows * W * 4) as usize],
    );
    let bottom_frac = differs_fraction(
        &burning_pixels[(lit_from_row * W * 4) as usize..],
        &not_burning_pixels[(lit_from_row * W * 4) as usize..],
    );
    // The band the *old* bottom-35%-only strip could never have touched but
    // the real predicted extent now reaches.
    let reclaimed_frac = differs_fraction(
        &burning_pixels[(untouched_rows * W * 4) as usize..(old_design_top_row * W * 4) as usize],
        &not_burning_pixels[(untouched_rows * W * 4) as usize..(old_design_top_row * W * 4) as usize],
    );

    eprintln!("=== fire overlay pixel gate (through RenderState::render_with_effects) ===");
    eprintln!(
        "on_fire=true: fire_overlay_drawn={}, top rows differ {:.1}%, bottom rows differ {:.1}%, \
         reclaimed band differs {:.1}%",
        burning_stats.fire_overlay_drawn,
        top_frac * 100.0,
        bottom_frac * 100.0,
        reclaimed_frac * 100.0
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
    // The rejected hypothesis, made numeric: under the retired
    // bottom-35%-only strip, this band would be forced to ~0% difference,
    // because the old geometry structurally never reached it.
    assert!(
        reclaimed_frac > 0.1,
        "expected the band between the real top edge (row {untouched_rows}) and the old \
         strip's top edge (row {old_design_top_row}) to now differ from the control — only \
         {:.1}%, indistinguishable from the retired bottom-35%-only hypothesis, which \
         predicts ~0% here",
        reclaimed_frac * 100.0
    );
    assert!(
        top_frac < 0.01,
        "fire overlay must not paint above its real predicted extent: {:.1}% of the rows \
         above it differ (bounding-box check per CLAUDE.md — a frame-average could not \
         have caught this)",
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
            freeze_percent: 1.0,
            scoping: true,
            nausea_intensity: 1.0,
            portal_intensity: 1.0,
            border_warning_strength: 0.0,
        },
    );

    eprintln!("=== spectator control ===");
    eprintln!(
        "spectator=true, every flag set: underwater={}, fire={}, pumpkin={}, spyglass={}, \
         freeze={}, confusion={}, portal={}",
        stats.underwater_overlay_drawn,
        stats.fire_overlay_drawn,
        stats.pumpkin_overlay_drawn,
        stats.spyglass_overlay_drawn,
        stats.freeze_overlay_drawn,
        stats.confusion_overlay_drawn,
        stats.portal_overlay_drawn
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
    assert!(
        !stats.spyglass_overlay_drawn,
        "a spectator must not draw the spyglass overlay even with scoping=true"
    );
    assert!(
        !stats.freeze_overlay_drawn,
        "a spectator must not draw the freeze overlay even with freeze_percent=1.0 -- this \
         codebase's own spectator convention, not a vanilla literal (see ScreenEffects::any_active's doc)"
    );
    assert!(
        !stats.confusion_overlay_drawn,
        "a spectator must not draw the confusion overlay even with nausea_intensity=1.0"
    );
    assert!(
        !stats.portal_overlay_drawn,
        "a spectator must not draw the portal overlay even with portal_intensity=1.0"
    );
}

/// Freeze/confusion/portal are **not** first-person-gated in vanilla
/// (`Hud`'s own decompiled source are siblings of the `isFirstPerson` block) — unlike
/// every overlay above. This is the control that proves it: third person
/// (`body_state` installed) must still draw them.
#[test]
#[ignore = "requires a GPU adapter"]
fn freeze_confusion_and_portal_survive_third_person_unlike_the_others() {
    let ctx = ctx();
    let (device, queue) = (ctx.device(), ctx.queue());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut render = RenderState::new(device, queue, format, W, H, None);
    render.install_screen_effects(
        ScreenEffectRenderer::new(device, queue, format, &manager()).expect("build over synthetic pack"),
    );
    // Force third person the same way other gates in this codebase do:
    // install a third-person body source so `stats.third_person_body_drawn`
    // is true, which is what `RenderState::render_inner` reads as
    // `!first_person`. Content doesn't matter for this control — only that a
    // body is installed at all.
    render.set_third_person_body_source(|| {
        Some(lodestone::gpu::ThirdPersonBodyState {
            // No skin: this fixture installs a body to suppress the first-person
            // arm, not to assert a sheet. The draw falls back to the model's own
            // texture, exactly as it did before this field existed.
            player_skin: None,
            feet: glam::Vec3::new(0.0, 70.0, 0.0),
            body_yaw_deg: 0.0,
            anim: lodestone_render::AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
            equipment_skin: Vec::new(),
        })
    });
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
            wearing_pumpkin: true,
            scoping: true,
            freeze_percent: 1.0,
            nausea_intensity: 1.0,
            portal_intensity: 1.0,
            border_warning_strength: 0.0,
            spectator: false,
            tick: 0,
        },
    );

    eprintln!("=== third-person split control ===");
    eprintln!(
        "third_person_body_drawn={}: underwater={}, fire={}, pumpkin={}, spyglass={}, \
         freeze={}, confusion={}, portal={}",
        stats.third_person_body_drawn,
        stats.underwater_overlay_drawn,
        stats.fire_overlay_drawn,
        stats.pumpkin_overlay_drawn,
        stats.spyglass_overlay_drawn,
        stats.freeze_overlay_drawn,
        stats.confusion_overlay_drawn,
        stats.portal_overlay_drawn
    );

    assert!(stats.third_person_body_drawn, "control setup: third-person body must actually be installed");
    assert!(!stats.underwater_overlay_drawn, "underwater is first-person-only");
    assert!(!stats.fire_overlay_drawn, "fire is first-person-only");
    assert!(!stats.pumpkin_overlay_drawn, "pumpkin is first-person-only");
    assert!(!stats.spyglass_overlay_drawn, "spyglass is first-person-only");
    assert!(
        stats.freeze_overlay_drawn,
        "freeze must draw in third person too -- vanilla's decompiled hud source is not nested in isFirstPerson"
    );
    // Portal takes priority over confusion when both are positive (both are
    // 1.0 above), matching vanilla's decompiled hud source's if/else if.
    assert!(
        stats.portal_overlay_drawn,
        "portal must draw in third person too, and win priority over confusion"
    );
    assert!(!stats.confusion_overlay_drawn, "confusion must lose to portal when both are positive");
}

/// The pumpkin overlay covers the full NDC screen like the
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
