//! How many times a night frame rebuilds the star field.
//!
//! **Byte-identity of the geometry** is what makes "build once and keep" safe,
//! and it is not asserted here because it is already pinned in the crate's own
//! unit tests: `sky::tests::same_seed_is_fully_deterministic` asserts
//! `build_star_field(SEED) == build_star_field(SEED)`, and
//! `different_seeds_produce_different_fields` is its control. The stored base is
//! that exact call's result, and the rotation applied to it per frame is
//! unchanged, so the vertex stream written to the buffer is the same bytes.
//!
//! `#[ignore]`d, like every other gate that needs an adapter: run with
//! `cargo test -p lodestone-render --test sky_star_field_build_counts --
//! --ignored --nocapture`.
//!
//! # Why a GPU gate for an arithmetic problem
//!
//! [`lodestone_render::sky::build_star_field`] is a pure function of a fixed
//! seed, so its output is *already* pinned hermetically
//! (`sky::tests::the_star_field_is_deterministic_for_a_seed` asserts
//! `build_star_field(SEED) == build_star_field(SEED)`). What was wrong was the
//! **call frequency**: `SkyRenderer::render` rebuilt the field inside its
//! `star_brightness > 0.0` branch, i.e. once per night frame — ~1500 iterations
//! of four `SplitMix64` draws plus a `Vec` allocation, ~6000 PRNG steps a frame,
//! for a value that cannot change. `SkyRenderer::render` is the only thing that
//! can be asked "how often", and it needs a device.
//!
//! **World species.** A daytime frame cannot exercise any of this — `render`
//! never reaches the star branch when `star_brightness_for_time_of_day` is `0`.
//! Midnight is the load-bearing part of the fixture and the daytime frame at the
//! end is what says so.
//!
//! Everything is one test function on purpose: the counter is process-global, so
//! two test threads sharing this binary would interleave their deltas.

use lodestone_assets::{MemorySource, ResourceManager};
use lodestone_render::sky::star_field_builds;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, RenderTarget, SkyRenderer, star_brightness_for_time_of_day,
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;
/// Vanilla midnight. `star_brightness_for_time_of_day(18_000) > 0`, asserted
/// below rather than assumed.
const MIDNIGHT: i64 = 18_000;
/// Vanilla noon, where the star branch is unreachable.
const NOON: i64 = 6_000;
/// Night frames rendered. The pre-fix hypothesis for the total is
/// `1 + NIGHT_FRAMES`; the fixed one is `1`.
const NIGHT_FRAMES: u64 = 3;

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

/// The same synthetic pack `sky_pipeline_gpu` uses — solid-colour celestial and
/// cloud art. This gate counts calls, so real art buys it nothing.
fn manager() -> ResourceManager {
    let mut src = MemorySource::new("star-count-test");
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

fn ctx() -> Option<GpuContext> {
    match GpuContext::new_headless_blocking() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipping: no GPU adapter available: {e}");
            None
        }
    }
}

fn camera_looking_up() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0,
        // Positive pitch looks *down* in this project (see `camera.rs`), so the
        // stars are in frame at -60.
        pitch: -60.0,
        fov_y_degrees: 90.0,
        aspect: WIDTH as f32 / HEIGHT as f32,
        near: 0.05,
        far: 1024.0,
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_star_field_is_built_once_per_renderer_not_once_per_night_frame() {
    // The fixture's own premise, from the same expression `render` gates the
    // star draw on — not a restated constant.
    assert!(
        star_brightness_for_time_of_day(MIDNIGHT) > 0.0,
        "midnight must reach the star branch, or this gate measures nothing"
    );
    assert_eq!(
        star_brightness_for_time_of_day(NOON),
        0.0,
        "and noon must not, or the daytime control below is vacuous"
    );

    let Some(ctx) = ctx() else { return };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let before = star_field_builds();
    let sky = SkyRenderer::new(ctx.device(), ctx.queue(), format, &manager())
        .expect("build sky renderer over the synthetic pack");
    assert_eq!(
        star_field_builds() - before,
        1,
        "the field is sized (and now kept) at construction, so exactly one build \
         belongs here"
    );

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, format);
    let camera = camera_looking_up();

    for frame_index in 0..NIGHT_FRAMES {
        let frame = target.acquire().expect("acquire");
        let mut encoder = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("star-count-encoder"),
            });
        sky.render(
            ctx.device(),
            ctx.queue(),
            &mut encoder,
            frame.view(),
            &camera,
            // A different tick each frame, so the celestial rotation really does
            // change: a gate that rendered the same instant three times could be
            // satisfied by a cache keyed on time.
            &lodestone_render::SkyFrame::new(MIDNIGHT + frame_index as i64 * 7, [0.24, 0.46, 0.83])
                .with_cloud_status(lodestone_render::CloudStatus::Fast),
            wgpu::Color::BLACK,
        );
        ctx.queue().submit(std::iter::once(encoder.finish()));
        let _ = target.read_texels(ctx.device(), ctx.queue());
    }

    assert_eq!(
        star_field_builds() - before,
        1,
        "expected 1 build for the whole session; {} is the pre-fix hypothesis \
         (one per night frame on top of construction), and anything else means \
         `render` still touches the generator",
        1 + NIGHT_FRAMES
    );

    // The daytime control: a frame that cannot reach the star branch must not
    // move the counter either, which is what makes the midnight fixture above
    // the load-bearing half.
    let frame = target.acquire().expect("acquire");
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("star-count-day-encoder"),
        });
    sky.render(
        ctx.device(),
        ctx.queue(),
        &mut encoder,
        frame.view(),
        &camera,
        &lodestone_render::SkyFrame::new(NOON, [0.24, 0.46, 0.83])
            .with_cloud_status(lodestone_render::CloudStatus::Fast),
        wgpu::Color::BLACK,
    );
    ctx.queue().submit(std::iter::once(encoder.finish()));
    let _ = target.read_texels(ctx.device(), ctx.queue());
    assert_eq!(star_field_builds() - before, 1);
}
