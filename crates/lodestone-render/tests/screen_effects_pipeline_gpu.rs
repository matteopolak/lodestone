//! GPU-requiring pixel gate for the underwater/fire overlay pass
//! ([`ScreenEffectRenderer`]), issues #108 and #112.
//!
//! `#[ignore]`d so the default `cargo test` run stays hermetic and headless —
//! run with `cargo test -p lodestone-render --test screen_effects_pipeline_gpu
//! -- --ignored --nocapture` on a machine with a real adapter.
//!
//! Uses a synthetic in-memory resource pack (solid-colour textures), not the
//! real jar: this gate is about proving the *pass* paints pixels end to end
//! (one bind group, `Load` not `Clear`, alpha blend, no depth attachment),
//! not about matching real vanilla art. `crates/lodestone-shell/tests/
//! screen_overlay_pixels.rs` is the sibling gate that proves this pass is
//! actually wired into `RenderState::render_inner`, through the shell's real
//! per-frame call — this file is the pipeline working in isolation.

use lodestone_assets::{MemorySource, ResourceManager};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget, ScreenEffectRenderer, fire_overlay_vertical_extent};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

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

/// A synthetic pack: an opaque white `underwater.png` (so the tint's effect is
/// unambiguous rather than close to the fully-black backdrop) and an opaque
/// orange 32-frame `fire_1.png` strip.
fn manager() -> ResourceManager {
    let mut src = MemorySource::new("screen-effects-gpu-test");
    src.insert(
        "assets/minecraft/textures/misc/underwater.png".to_string(),
        png(16, 16, [255, 255, 255, 255]),
    );
    // 32 frames of 16x16, stacked vertically (a single opaque orange colour —
    // this gate is about the pass reaching pixels, not about frame content).
    src.insert(
        "assets/minecraft/textures/block/fire_1.png".to_string(),
        png(16, 16 * 32, [230, 130, 20, 255]),
    );
    // Opaque green pumpkin overlay: distinct from the white underwater and
    // orange fire textures so the three cannot be confused in a screenshot.
    src.insert(
        "assets/minecraft/textures/misc/pumpkinblur.png".to_string(),
        png(16, 16, [40, 200, 40, 255]),
    );
    // Opaque light-blue freeze vignette (#139).
    src.insert(
        "assets/minecraft/textures/misc/powder_snow_outline.png".to_string(),
        png(256, 256, [200, 230, 255, 255]),
    );
    // Opaque grey spyglass lens (#154) — distinct from black so the lens vs.
    // letterbox-bar split is unambiguous.
    src.insert(
        "assets/minecraft/textures/misc/spyglass_scope.png".to_string(),
        png(256, 256, [180, 180, 180, 255]),
    );
    // Opaque white nausea texture (#144) so the tint's own colour (green-biased,
    // see `confusion_overlay_triangles`) is what shows up, not the texture's.
    src.insert(
        "assets/minecraft/textures/misc/nausea.png".to_string(),
        png(256, 256, [255, 255, 255, 255]),
    );
    // Opaque magenta 32-frame portal strip (#149).
    src.insert(
        "assets/minecraft/textures/block/nether_portal.png".to_string(),
        png(16, 16 * 32, [200, 40, 200, 255]),
    );
    ResourceManager::new(vec![Box::new(src)])
}

fn non_black_fraction(pixels: &[u8]) -> f64 {
    let mut non_black = 0usize;
    let mut total = 0usize;
    for px in pixels.chunks_exact(4) {
        total += 1;
        if px[0] > 8 || px[1] > 8 || px[2] > 8 {
            non_black += 1;
        }
    }
    non_black as f64 / total.max(1) as f64
}

fn ctx() -> Option<GpuContext> {
    match GpuContext::new_headless_blocking() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipping: no GPU adapter available: {e}");
            None
        }
    }
}

/// **Negative control.** A target the pass never touches must read back as
/// the fully-black texture wgpu hands out on creation — proves
/// `non_black_fraction` can tell a painted target from an untouched one.
#[test]
#[ignore = "requires a GPU adapter"]
fn control_an_untouched_target_reads_back_as_black() {
    let Some(ctx) = ctx() else { return };
    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let _ = target.acquire().expect("acquire");
    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac < 0.01,
        "control failed: an untouched target should read back as black, got {:.1}% non-black",
        frac * 100.0
    );
}

/// The underwater overlay, drawn once at full brightness onto a black target,
/// must cover the whole frame (it is a full-NDC quad — see
/// `underwater_overlay_quad`'s doc) and leave it majority non-black even
/// though the tint alpha is a subtle `0.1`.
#[test]
#[ignore = "requires a GPU adapter"]
fn underwater_overlay_paints_the_whole_frame() {
    let Some(ctx) = ctx() else { return };
    let fx = ScreenEffectRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build screen-effect renderer over the synthetic pack");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");

    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("underwater-gpu-test-encoder"),
        });
    fx.draw_underwater(ctx.queue(), &mut encoder, frame.view(), 0.0, 0.0, 0xFF);
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.95,
        "expected the underwater overlay to cover the whole frame, only {:.1}% non-black",
        frac * 100.0
    );
}

/// The fire overlay, drawn onto a black target, must paint rows matching
/// its real two-quad geometry's predicted vertical extent
/// (`fire_overlay_vertical_extent`, issue #420) — not the old design's
/// hardcoded bottom-35% strip, and genuinely untouched above that real
/// extent. A frame-average check could not tell "painted in the right
/// place" from "a full-screen tint at a lower intensity" — see `CLAUDE.md`'s
/// "measure by location" rule — so this checks row bands, not one global
/// fraction, and predicts the band boundary from the same constants the
/// geometry itself uses rather than a restated decimal.
#[test]
#[ignore = "requires a GPU adapter"]
fn fire_overlay_paints_its_predicted_extent_not_the_old_bottom_strip() {
    let Some(ctx) = ctx() else { return };
    let fx = ScreenEffectRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build screen-effect renderer over the synthetic pack");
    assert_eq!(fx.fire_frame_count(), 32, "the synthetic strip is 32 frames");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");

    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fire-gpu-test-encoder"),
        });
    fx.draw_fire(ctx.queue(), &mut encoder, frame.view(), 5);
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    // Row 0 is the top of the readback in this project's convention (see
    // `sky_pixels.rs`'s own row-band gates). Predict the real geometry's
    // vertical extent from the same constants `fire_overlay_triangles`
    // itself uses, rather than a restated decimal.
    let (_predicted_min_ndc, predicted_max_ndc) = fire_overlay_vertical_extent();
    let frac_from_bottom = |y_ndc: f32| (f64::from(y_ndc) + 1.0) / 2.0;
    let row_from_top = |frac: f64| (f64::from(HEIGHT) * (1.0 - frac)) as u32;
    let predicted_top_row = row_from_top(frac_from_bottom(predicted_max_ndc));

    // A small margin either side of the exact predicted edge absorbs
    // rasterisation rounding at the boundary itself, without weakening the
    // claim that rows well outside the predicted extent are untouched.
    const MARGIN_ROWS: u32 = 2;
    let untouched_above_row = predicted_top_row.saturating_sub(MARGIN_ROWS);
    let lit_below_row = predicted_top_row + MARGIN_ROWS;
    // The old design's own top edge (`FIRE_STRIP_TOP = -0.3`, 35% up from
    // the bottom) — the rejected hypothesis this test falsifies below.
    let old_design_top_row = row_from_top(0.35);
    assert!(
        untouched_above_row < old_design_top_row,
        "sanity: the real extent's top ({untouched_above_row}) should sit well above \
         (numerically less than) the old strip's top ({old_design_top_row}), or this test \
         cannot distinguish the two hypotheses at all"
    );

    let (mut untouched_lit, mut untouched_total) = (0usize, 0usize);
    let (mut lit_lit, mut lit_total) = (0usize, 0usize);
    // Rows the *old* bottom-35% strip could never have touched but the real
    // predicted extent now reaches — proof the fix moved where pixels land,
    // not merely how the old band is described.
    let (mut reclaimed_lit, mut reclaimed_total) = (0usize, 0usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let idx = ((y * WIDTH + x) * 4) as usize;
            let px = &pixels[idx..idx + 4];
            let lit = px[0] > 8 || px[1] > 8 || px[2] > 8;
            if y < untouched_above_row {
                untouched_total += 1;
                untouched_lit += usize::from(lit);
            }
            if y >= lit_below_row {
                lit_total += 1;
                lit_lit += usize::from(lit);
            }
            if y >= untouched_above_row && y < old_design_top_row {
                reclaimed_total += 1;
                reclaimed_lit += usize::from(lit);
            }
        }
    }
    let untouched_frac = untouched_lit as f64 / untouched_total.max(1) as f64;
    let lit_frac = lit_lit as f64 / lit_total.max(1) as f64;
    let reclaimed_frac = reclaimed_lit as f64 / reclaimed_total.max(1) as f64;

    eprintln!(
        "predicted top row={predicted_top_row}, old design top row={old_design_top_row}, \
         untouched={:.1}% lit_below={:.1}% reclaimed_band={:.1}%",
        untouched_frac * 100.0,
        lit_frac * 100.0,
        reclaimed_frac * 100.0
    );

    assert!(
        untouched_frac < 0.01,
        "fire overlay must not paint above its real predicted extent: {:.1}% non-black \
         above row {untouched_above_row} (bounding-box check per CLAUDE.md — a \
         frame-average could not have caught this)",
        untouched_frac * 100.0
    );
    assert!(
        lit_frac > 0.3,
        "expected the fire overlay to light up rows well below its top edge, only \
         {:.1}% non-black there",
        lit_frac * 100.0
    );
    // The rejected hypothesis, made concrete and numeric: under the old
    // bottom-35%-only strip, this band's lit fraction would be forced to
    // (near) zero, because the old geometry structurally never reached it.
    assert!(
        reclaimed_frac > 0.1,
        "expected the band between the real top edge (row {untouched_above_row}) and the \
         old strip's top edge (row {old_design_top_row}) to now show real fire coverage — \
         only {:.1}% non-black, indistinguishable from the retired bottom-35%-only \
         hypothesis, which predicts ~0% here",
        reclaimed_frac * 100.0
    );
}

/// The pumpkin overlay (issue #185), drawn once onto a black target, must
/// cover the whole frame like the underwater overlay — it is also a static
/// full-NDC quad (`pumpkin_overlay_triangles`'s doc), but untinted (opaque
/// white vertex colour) rather than a 0.1-alpha tint, so it should paint
/// **more** strongly than the underwater overlay: this distinguishes "the
/// pass ran" from "the pass ran with the wrong alpha", which a bare
/// non-black fraction against a black backdrop alone could not.
#[test]
#[ignore = "requires a GPU adapter"]
fn pumpkin_overlay_paints_the_whole_frame_at_full_strength() {
    let Some(ctx) = ctx() else { return };
    let fx = ScreenEffectRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build screen-effect renderer over the synthetic pack");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");

    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pumpkin-gpu-test-encoder"),
        });
    fx.draw_pumpkin(&mut encoder, frame.view());
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.99,
        "expected the pumpkin overlay to fully cover the frame at opaque tint, only {:.1}% non-black",
        frac * 100.0
    );

    // Predict the value, not just the sign (CLAUDE.md's "magnitude" species):
    // the source texture is opaque green (40, 200, 40) with no vertex tint at
    // all, drawn straight over a black target with standard alpha blending,
    // so the readback should land close to the source green channel, not some
    // partially-blended value a wrong tint or wrong blend factor would leave.
    let mut green_sum = 0u64;
    let mut n = 0u64;
    for px in pixels.chunks_exact(4) {
        green_sum += u64::from(px[1]);
        n += 1;
    }
    let avg_green = green_sum as f64 / n.max(1) as f64;
    assert!(
        avg_green > 150.0,
        "opaque untinted green texture should read back close to 200 on the green channel, got avg {avg_green:.1}"
    );
}

/// The freeze overlay (issue #139), drawn at `percent_frozen = 0.5` onto a
/// black target, covers the whole frame (same static full-NDC shape as
/// pumpkin — see `freeze_overlay_triangles`'s doc) but at half the opacity a
/// full `1.0` would give. Magnitude, not just sign: the source is opaque
/// light-blue `(200, 230, 255)`; standard alpha blending over black at
/// `alpha=0.5` predicts a readback close to half that, `(100, 115, 128)`, not
/// the full source colour a wrong (always-`1.0`) alpha would give, and not
/// the near-zero a wrong (near-`0.0`) alpha would give.
#[test]
#[ignore = "requires a GPU adapter"]
fn freeze_overlay_paints_the_whole_frame_at_the_predicted_half_alpha() {
    let Some(ctx) = ctx() else { return };
    let fx = ScreenEffectRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build screen-effect renderer over the synthetic pack");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("freeze-gpu-test-encoder"),
        });
    fx.draw_freeze(ctx.queue(), &mut encoder, frame.view(), 0.5);
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.95,
        "expected the freeze overlay to cover the whole frame even at half alpha, only {:.1}% non-black",
        frac * 100.0
    );

    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for px in pixels.chunks_exact(4) {
        sum[0] += u64::from(px[0]);
        sum[1] += u64::from(px[1]);
        sum[2] += u64::from(px[2]);
        n += 1;
    }
    let avg = [sum[0] as f64 / n.max(1) as f64, sum[1] as f64 / n.max(1) as f64, sum[2] as f64 / n.max(1) as f64];
    // Predicted midpoint: standard `src*a + dst*(1-a)` over a black dst at
    // a=0.5 puts each channel near half the source's sRGB byte value —
    // loosely bounded (not "close to 200/230/255" which a wrong alpha=1.0
    // would also satisfy, and not "close to 0" which alpha=0.0 would).
    assert!(
        avg[0] > 60.0 && avg[0] < 160.0,
        "half-alpha freeze overlay red channel should land near the source's half-blend, got {:.1}",
        avg[0]
    );
    assert!(
        avg[2] > avg[0],
        "the source texture is light-blue (blue channel highest); half-alpha blending must \
         preserve that channel ordering, got avg {avg:?}"
    );
}

/// The spyglass overlay (issue #154), drawn onto a black target at a 16:9
/// aspect, must paint the whole frame (lens + letterbox bars together tile
/// the full screen, see `spyglass_letterbox_triangles`'s doc) with two
/// visually distinct regions: the centre (lens, opaque grey `(180,180,180)`)
/// and the far corners (bars, opaque black) — a location check, not a frame
/// average, since a pure average could not distinguish "grey lens + black
/// bars" from a uniform mid-grey wash.
#[test]
#[ignore = "requires a GPU adapter"]
fn spyglass_overlay_paints_a_grey_lens_surrounded_by_black_bars() {
    let Some(ctx) = ctx() else { return };
    let fx = ScreenEffectRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build screen-effect renderer over the synthetic pack");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("spyglass-gpu-test-encoder"),
        });
    fx.draw_spyglass(ctx.queue(), &mut encoder, frame.view(), 16.0 / 9.0);
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.3,
        "expected the spyglass lens to paint a visible fraction of the frame, only {:.1}% non-black",
        frac * 100.0
    );

    // Centre pixel: inside the lens, must be lit (non-black).
    let px_at = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * WIDTH + x) * 4) as usize;
        [pixels[idx], pixels[idx + 1], pixels[idx + 2], pixels[idx + 3]]
    };
    let centre = px_at(WIDTH / 2, HEIGHT / 2);
    assert!(
        centre[0] > 8 || centre[1] > 8 || centre[2] > 8,
        "the screen centre must be inside the lens and non-black, got {centre:?}"
    );

    // At 16:9, `spyglass_lens_half_extent` gives hw ≈ 0.6326, hh = 1.125 —
    // i.e. the lens overflows top/bottom entirely (no top/bottom bars) but
    // leaves real left/right bars. Sample a corner pixel, well outside the
    // lens's horizontal extent (hw*WIDTH/2 ≈ 0.317*WIDTH from centre).
    let corner = px_at(2, HEIGHT / 2);
    assert!(
        corner[0] < 8 && corner[1] < 8 && corner[2] < 8,
        "the far-left edge at mid-height must be inside a letterbox bar and pure black, got {corner:?}"
    );
}

/// The confusion overlay (issue #144, screen-space half), drawn onto a black
/// target at maximum strength, must cover the whole frame (see
/// `confusion_overlay_triangles`'s doc: `size >= 1.0` always) with the
/// predicted green-biased tint — magnitude, not just "some colour appeared":
/// vanilla's own tint at `strength=1.0` is `(0.2, 0.4, 0.2)` in linear-ish
/// float space multiplied onto an opaque white source, so green should read
/// back roughly double red/blue, not merely "greater than zero".
#[test]
#[ignore = "requires a GPU adapter"]
fn confusion_overlay_paints_the_whole_frame_with_a_green_biased_tint() {
    let Some(ctx) = ctx() else { return };
    let fx = ScreenEffectRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build screen-effect renderer over the synthetic pack");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("confusion-gpu-test-encoder"),
        });
    fx.draw_confusion(ctx.queue(), &mut encoder, frame.view(), 1.0);
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.95,
        "expected the confusion overlay to cover the whole frame at strength 1.0, only {:.1}% non-black",
        frac * 100.0
    );

    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for px in pixels.chunks_exact(4) {
        sum[0] += u64::from(px[0]);
        sum[1] += u64::from(px[1]);
        sum[2] += u64::from(px[2]);
        n += 1;
    }
    let avg = [sum[0] as f64 / n.max(1) as f64, sum[1] as f64 / n.max(1) as f64, sum[2] as f64 / n.max(1) as f64];
    assert!(
        avg[1] > avg[0] * 1.3 && avg[1] > avg[2] * 1.3,
        "confusion overlay's green channel must dominate red/blue by roughly the tint's own \
         0.4-vs-0.2 ratio, got avg {avg:?}"
    );
}

/// The portal overlay (issue #149, screen-space half), drawn onto a black
/// target at full intensity, must cover the whole frame and select the
/// requested animation frame from the 32-frame strip (same shape as the fire
/// overlay's own frame-selection gate).
#[test]
#[ignore = "requires a GPU adapter"]
fn portal_overlay_paints_the_whole_frame_at_full_intensity() {
    let Some(ctx) = ctx() else { return };
    let fx = ScreenEffectRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build screen-effect renderer over the synthetic pack");
    assert_eq!(fx.portal_frame_count(), 32, "the synthetic strip is 32 frames");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("portal-gpu-test-encoder"),
        });
    fx.draw_portal(ctx.queue(), &mut encoder, frame.view(), 5, 1.0);
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.95,
        "expected the portal overlay to cover the whole frame at full intensity, only {:.1}% non-black",
        frac * 100.0
    );

    // Magnitude: opaque magenta (200, 40, 200) source, full alpha (intensity
    // 1.0 is the identity case of `portal_overlay_alpha`), straight over
    // black — red and blue should dominate green, not merely be nonzero.
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for px in pixels.chunks_exact(4) {
        sum[0] += u64::from(px[0]);
        sum[1] += u64::from(px[1]);
        sum[2] += u64::from(px[2]);
        n += 1;
    }
    let avg = [sum[0] as f64 / n.max(1) as f64, sum[1] as f64 / n.max(1) as f64, sum[2] as f64 / n.max(1) as f64];
    assert!(
        avg[0] > 130.0 && avg[2] > 130.0 && avg[1] < 80.0,
        "opaque magenta source at full intensity should read back close to (200, 40, 200), got avg {avg:?}"
    );
}
