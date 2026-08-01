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
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget, ScreenEffectRenderer};

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

/// The fire overlay, drawn onto a black target, must paint a genuinely
/// bottom-only strip: non-black rows only in the bottom ~35% of the frame
/// (`FIRE_STRIP_TOP = -0.3`), and the top rows must stay untouched. A
/// frame-average check could not tell this from a full-screen tint at a lower
/// intensity — see `CLAUDE.md`'s "measure by location" rule — so this checks
/// row bands, not one global fraction.
#[test]
#[ignore = "requires a GPU adapter"]
fn fire_overlay_paints_only_the_bottom_strip() {
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
    // `sky_pixels.rs`'s own row-band gates); the strip's NDC top
    // (`FIRE_STRIP_TOP = -0.3`) is 35% of the way up from the bottom, i.e.
    // the bottom 35% of rows.
    let strip_rows = ((HEIGHT as f64) * 0.35) as u32;
    let mut top_non_black = 0usize;
    let mut top_total = 0usize;
    let mut bottom_non_black = 0usize;
    let mut bottom_total = 0usize;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let idx = ((y * WIDTH + x) * 4) as usize;
            let px = &pixels[idx..idx + 4];
            let lit = px[0] > 8 || px[1] > 8 || px[2] > 8;
            if y < HEIGHT - strip_rows {
                top_total += 1;
                if lit {
                    top_non_black += 1;
                }
            } else {
                bottom_total += 1;
                if lit {
                    bottom_non_black += 1;
                }
            }
        }
    }
    let top_frac = top_non_black as f64 / top_total.max(1) as f64;
    let bottom_frac = bottom_non_black as f64 / bottom_total.max(1) as f64;
    assert!(
        bottom_frac > 0.5,
        "expected the fire overlay to light up the bottom strip, only {:.1}% non-black there",
        bottom_frac * 100.0
    );
    assert!(
        top_frac < 0.01,
        "fire overlay must not paint above its strip: {:.1}% non-black in the top rows \
         (bounding-box check per CLAUDE.md — a frame-average could not have caught this)",
        top_frac * 100.0
    );
}
