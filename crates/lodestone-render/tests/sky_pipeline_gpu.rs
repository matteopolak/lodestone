//! GPU-requiring pixel gate for the sky pass ([`SkyRenderer`]).
//!
//! `#[ignore]`d so the default `cargo test` run stays hermetic and headless —
//! run with `cargo test -p lodestone-render --test sky_pipeline_gpu --
//! --ignored --nocapture` on a machine with a real adapter.
//!
//! Uses a synthetic in-memory resource pack (solid-colour sun/moon/cloud
//! textures), not the real jar: this gate is about proving the *pass* paints
//! pixels end to end (disc + celestial + star + cloud draws, in one render
//! pass with no depth attachment), not about matching real vanilla art.

use lodestone_assets::{MemorySource, ResourceManager};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget, SkyRenderer};

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

/// A synthetic pack with an opaque sun, all 8 moon phases, and an opaque
/// cloud texture (fully opaque so a single cloud draw covers the whole
/// screen, keeping the pixel assertion simple and robust).
fn manager() -> ResourceManager {
    let mut src = MemorySource::new("sky-gpu-test");
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

/// Counts pixels that differ noticeably from pure black (the render pass's
/// clear colour), from a tightly-packed RGBA8 readback.
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

/// **Negative control.** A target that the sky pass never touches must read
/// back as the fully-black texture wgpu hands out on creation. This is what
/// proves the affirmative test below is measuring something real: if the
/// detector (`non_black_fraction`) could not tell a painted target from an
/// untouched one, the affirmative test would be worthless.
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

/// The sky pass, run once at night (so the star/moon draws are active too,
/// not just the disc), must leave the color target majority non-black.
///
/// The camera looks steeply *up* (`pitch = -60`, this project's convention is
/// positive pitch looks down — see `camera.rs`), not level: the sky disc is a
/// flat plane 16 units above the camera and this pass deliberately does not
/// draw vanilla's below-horizon "dark disc" (see `SkyRenderer::render`'s
/// doc comment on that omission), so a level camera only ever paints the
/// upper ~half of the frame — correct (the lower half is where a real
/// terrain pass would draw), but not what this gate is checking. Looking up
/// keeps the frustum inside the painted region.
#[test]
#[ignore = "requires a GPU adapter"]
fn sky_pass_paints_the_whole_frame() {
    let Some(ctx) = ctx() else { return };
    let sky = SkyRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build sky renderer over the synthetic pack");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");

    let camera = Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0,
        pitch: -60.0,
        fov_y_degrees: 90.0,
        aspect: WIDTH as f32 / HEIGHT as f32,
        near: 0.05,
        far: 1024.0,
    };

    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sky-gpu-test-encoder"),
        });
    // Midnight: `time_of_day = 18_000` — stars and the moon are both fully
    // active (`star_brightness_for_time_of_day(18_000) > 0`), so this frame
    // exercises every one of the four draws, not just the disc.
    sky.render(
        ctx.device(),
        ctx.queue(),
        &mut encoder,
        frame.view(),
        &camera,
        18_000,
        [0.24, 0.46, 0.83],
    );
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.5,
        "expected the sky pass to paint most of the frame (disc alone is opaque \
         and covers the FOV), only {:.1}% non-black",
        frac * 100.0
    );
}
