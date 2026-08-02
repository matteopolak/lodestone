//! **Defect — "mobs are still super bright, even at night."** The follow-up to
//! `entity_light_pixels`, which proved entities *respond* to a light byte, and
//! passed, and still left the reported bug on screen.
//!
//! # Why the previous fix did not work
//!
//! Sampling was right and the shader was right. The *input* never changed. A
//! server's sky-light array is time-invariant — it records how much sky reaches a
//! block, not how bright the sky is now — so a mob under open sky samples the
//! same `0xF0` at midnight as at noon. Measured live against a vanilla 26.2
//! oracle, with the server's own clock as the control:
//!
//! ```text
//! noon     clock= 6000  packed=0xF0  sky=15 block=0  light_term=1.000
//! midnight clock=18000  packed=0xF0  sky=15 block=0  light_term=1.000
//! ```
//!
//! Vanilla darkens purely client-side, in `LightTexture.updateLightTexture`, by
//! scaling the **sky** half of the lightmap by `Level.getSkyDarken`. That term did
//! not exist in any shader in this repo. This file gates it *at pixels*, through
//! the real [`EntityPipeline`], because a sampler-level assertion is precisely
//! what let the bug ship the first time.
//!
//! # What is measured, and why by location
//!
//! Every surface here is painted with one mid-grey texel (byte `128`), so any
//! readback difference is a *shading* difference and never a texture difference.
//! Readings are taken from the mob silhouette only — classified against the
//! **encoded** clear colour, since the target is `_srgb` and comparing against
//! the unencoded triple classifies the whole frame as mob, which is how two of
//! the shell's own entity gates silently ended up measuring nothing.
//!
//! The target is sRGB, matching the real swapchain (`Bgra8UnormSrgb`): the shade
//! multiply lands in gamma space, and a plain `Unorm` target would move it.
//!
//! `#[ignore]`d: needs a real GPU adapter, and once opted in a missing adapter is
//! a failure, never a skip.

use glam::Vec3;
use lodestone_assets::entity_models::zombie_model;
use lodestone_render::CameraUniform;
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{
    EntityInstance, EntityMesh, plan_entities, sky_darken_for_time_of_day,
};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{
    EntityCameraUniform, EntityPipeline, GpuEntityModel, entity_camera_buffer, upload_instances,
};
use lodestone_render::fog::FogUniform;

const W: u32 = 192;
const H: u32 = 192;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The one texel every surface is painted with. Mid-grey in **sRGB bytes**.
const TEXEL: u8 = 128;

/// A mob under open sky: `sky = 15`, `block = 0`. This is what the live oracle
/// actually returns for a surface position, at *any* time of day.
const LIGHT_SKY: u8 = 15 << 4;
/// A mob lit by a torch and no sky at all: `sky = 0`, `block = 15`. Its
/// brightness must not move with the clock.
const LIGHT_TORCH: u8 = 15;

/// Noon. Vanilla's curve tops out at exactly 1.0.
const NOON: f32 = 1.0;
/// The lane left at its `0.0` default — the "sky darken was never wired" state
/// every caller that predates this term is in. Must render identically to noon.
const UNSET: f32 = 0.0;

/// Sky colour behind the mob; nothing else in the frame is this colour.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
    a: 1.0,
};

macro_rules! pass_desc {
    ($color:expr, $depth:expr) => {
        wgpu::RenderPassDescriptor {
            label: Some("entity-night-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: $color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: $depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }
    };
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("entity_night_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

fn side_camera() -> Camera {
    Camera {
        position: Vec3::new(0.0, 0.9, -2.2),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

fn upload_flat_sheet(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::Texture, wgpu::Sampler) {
    const N: u32 = 16;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-night-sheet"),
        size: wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // sRGB, like the real entity sheets the shell uploads. Binding a vanilla
        // PNG as plain `Unorm` is a measured +48% on every mob pixel.
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let rgba: Vec<u8> = [TEXEL, TEXEL, TEXEL, 255].repeat((N * N) as usize);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(N * 4),
            rows_per_image: Some(N),
        },
        wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
    );
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("entity-night-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (texture, sampler)
}

/// Render one zombie carrying packed `light`, under a sky darkening of
/// `sky_darken`, through the real entity pipeline. Returns the whole RGBA frame.
///
/// `sky_darken` is written into the group-0 uniform exactly the way
/// `RenderState::prepare_entities` writes it — via
/// [`EntityCameraUniform::with_sky_darken`] — so this gate exercises the shipping
/// path and not a test-only shortcut.
fn mob_frame(gpu: &Gpu, light: u8, sky_darken: f32) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = side_camera();
    let def = zombie_model();
    let mesh = EntityMesh::from_named_model("zombie", &def);

    let inst = EntityInstance::new("zombie", &mesh, Vec3::ZERO, 90.0, 1.0, &AnimInput::REST)
        .with_light(light);
    let frame = plan_entities(std::slice::from_ref(&inst), &camera.frustum());
    assert_eq!(frame.instance_count(), 1, "the mob must be on screen");

    let pipeline = EntityPipeline::new(device, FORMAT);
    let (tex, sampler) = upload_flat_sheet(device, queue);
    let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let uniform = EntityCameraUniform {
        camera: CameraUniform::new(&camera, [0.0, 0.0, 0.0]),
        fog: FogUniform::disabled(),
    }
    .with_sky_darken(sky_darken);
    let cam_buf = entity_camera_buffer(device, uniform);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_mesh = GpuEntityModel::upload(device, &mesh).expect("non-empty mesh");

    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_mesh.parts.iter().zip(&batch.parts) {
            if range.index_count == 0 {
                continue;
            }
            if let Some(buf) = upload_instances(device, mats, &batch.lights) {
                per_part.push((
                    mats.len() as u32,
                    range.index_start..range.index_start + range.index_count,
                    buf,
                ));
            }
        }
    }
    assert!(!per_part.is_empty(), "nothing would be drawn");

    let (color, color_view) = color_target(device);
    let depth = DepthBuffer::new(device, W, H);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&pass_desc!(&color_view, &depth.view));
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        for (count, range, buf) in &per_part {
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }
    }
    readback(device, queue, enc, &color)
}

/// Is this pixel part of the mob (i.e. not the sky clear colour)?
///
/// The clear is specified in **linear** light and this target is `_srgb`, so what
/// lands in the readback is the *encoded* clear, not `CLEAR * 255`.
fn is_mob(frame: &[u8], i: usize) -> bool {
    let encode = |c: f64| {
        let v = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (v * 255.0).round() as u8
    };
    let clear = [encode(CLEAR.r), encode(CLEAR.g), encode(CLEAR.b)];
    frame[i..i + 3]
        .iter()
        .zip(clear)
        .any(|(got, want)| got.abs_diff(want) > 8)
}

/// Mean red over the mob silhouette, plus the silhouette area. The sheet is
/// neutral grey, so red is a faithful luma proxy.
fn mob_mean(frame: &[u8]) -> (f32, u32) {
    let (mut sum, mut n) = (0u64, 0u32);
    for i in 0..(W * H) as usize {
        if is_mob(frame, i * 4) {
            sum += u64::from(frame[i * 4]);
            n += 1;
        }
    }
    assert!(
        n > 2000,
        "only {n} mob pixels found — readings would be slivers, not populations"
    );
    (sum as f32 / n as f32, n)
}

/// Count of silhouette pixels whose byte differs between two frames. Zero is the
/// only honest way to say "these renders are identical".
fn differing_pixels(a: &[u8], b: &[u8]) -> u32 {
    let mut n = 0;
    for i in 0..(W * H) as usize {
        if (is_mob(a, i * 4) || is_mob(b, i * 4)) && a[i * 4..i * 4 + 3] != b[i * 4..i * 4 + 3] {
            n += 1;
        }
    }
    n
}

fn color_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-night-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = color.create_view(&wgpu::TextureViewDescriptor::default());
    (color, view)
}

fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut enc: wgpu::CommandEncoder,
    color: &wgpu::Texture,
) -> Vec<u8> {
    let padded = (W * 4).next_multiple_of(256);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("entity-night-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().expect("map readback");
    let data = slice.get_mapped_range().expect("mapped range");
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H as usize {
        let start = y * padded as usize;
        out.extend_from_slice(&data[start..start + (W * 4) as usize]);
    }
    drop(data);
    buf.unmap();
    out
}

// ------------------------------------------------------------------- the gates

/// The acceptance band for the sky-lit night/day ratio.
///
/// # Three predictions, named
///
/// Every number here is arithmetic on `assets/minecraft/shaders/core/lightmap.fsh`
/// from the real 26.2 `client.jar` and on `Options.java:900`'s default gamma —
/// **not** read back from this crate. For a sky-15, block-0 mob:
///
/// * **correct** (`NIGHT_RATIO`): `get_brightness(15/15)` is `1.0`; `SkyFactor` at
///   midnight is `0.24`, so the sky contribution is `0.24`; `AmbientColor` seeds the
///   accumulator with the overworld's `0x0A0A0A` = `0.039216`
///   (`DimensionTypes.java:36`), giving a combined `0.279216`; then `lightmap.fsh`'s
///   last line mixes `notGamma` in at the default `BrightnessFactor` of `0.5`,
///   and for a grey value `notGamma(c) == 1 - (1-c)^4`, giving **0.50465**. Noon is
///   exactly `1.0` — the ambient term clamps away up there — so that is the ratio.
/// * **`AmbientColor` dropped** (`AMBIENT_FREE_RATIO`): the same chain believing the
///   overworld's ambient is black, which is what this gate first asserted:
///   **0.45319**. It is the closest wrong answer, `0.05` away, and it sets how wide
///   the band can be.
/// * **the retired linear ramp** (`OLD_RAMP_RATIO`): `0.2 + 0.8 * 0.24` =
///   **0.392**. This is what shipped before the curve landed, and the band must
///   exclude it or this gate cannot see the change.
/// * **the original shipped bug** (`BUG_RATIO`): no time-of-day term at all, so
///   both frames render at `1.0` — a ratio of exactly **1.000**.
///
/// The band admits only the first, and every exclusion is asserted in the test body
/// rather than described here.
const NIGHT_RATIO: f32 = 0.504_65;
const AMBIENT_FREE_RATIO: f32 = 0.453_19;
const OLD_RAMP_RATIO: f32 = 0.392;
const BUG_RATIO: f32 = 1.000;
const BAND: std::ops::RangeInclusive<f32> = 0.482..=0.528;

fn gpu_or_fail() -> Gpu {
    setup().unwrap_or_else(|| {
        panic!(
            "entity_night_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a \
             failure, never a skip"
        )
    })
}

/// **The gate.** A mob under open sky must be measurably darker at midnight than
/// at noon, with the *same* sampled light byte — because that byte is all the
/// server ever gives us.
///
/// The negative control is run, not described: the same-`sky_darken` ratio is
/// pushed through the same band check, and this test asserts that check *fails*.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fire"]
fn a_sky_lit_mob_is_darker_at_midnight_than_at_noon() {
    let gpu = gpu_or_fail();

    let midnight = sky_darken_for_time_of_day(18_000);
    let (day, day_px) = mob_mean(&mob_frame(&gpu, LIGHT_SKY, NOON));
    let (night, night_px) = mob_mean(&mob_frame(&gpu, LIGHT_SKY, midnight));
    let ratio = night / day;

    // The negative control: hold sky_darken fixed at noon for both renders. This
    // is the world the shipped code lives in — no time-of-day term anywhere — and
    // it must land on 1.000 and be *rejected* by the band.
    let (control_a, _) = mob_mean(&mob_frame(&gpu, LIGHT_SKY, NOON));
    let (control_b, _) = mob_mean(&mob_frame(&gpu, LIGHT_SKY, NOON));
    let control_ratio = control_b / control_a;

    println!("=== MOB SKY-DARKEN GATE (texel {TEXEL}, sRGB target) ===");
    println!("sky_darken at midnight       = {midnight:.4} (vanilla's curve)");
    println!("mob mean, sky 15, noon       = {day:.1} over {day_px}px");
    println!("mob mean, sky 15, midnight   = {night:.1} over {night_px}px");
    println!("measured ratio               = {ratio:.3}");
    println!("correct prediction           = {NIGHT_RATIO:.3} (lightmap.fsh, gamma 0.5)");
    println!("ambient-dropped control      = {AMBIENT_FREE_RATIO:.3} (AmbientColor believed black)");
    println!("retired-linear-ramp control  = {OLD_RAMP_RATIO:.3} (0.2 + 0.8 * 0.24)");
    println!("shipped-bug control          = {BUG_RATIO:.3}");
    println!("negative control (same input) = {control_ratio:.3}, in band = {}", BAND.contains(&control_ratio));

    // Same input, exactly zero difference — the control has no slack to hide in.
    assert!(
        (control_ratio - 1.0).abs() < f32::EPSILON,
        "same-input control must be exactly 1.0, got {control_ratio}"
    );
    assert!(
        !BAND.contains(&control_ratio),
        "NEGATIVE CONTROL DID NOT FIRE: a mob rendered twice with no time-of-day difference \
         produced ratio {control_ratio:.3}, which this gate's band {BAND:?} *accepts*. The band \
         cannot distinguish the fix from the bug and the gate is vacuous."
    );

    // The band must reject the retired ramp too, or this gate cannot see the
    // curve change — asserted rather than described, so a widened band is caught
    // here and not by a player.
    assert!(
        BAND.contains(&NIGHT_RATIO)
            && !BAND.contains(&AMBIENT_FREE_RATIO)
            && !BAND.contains(&OLD_RAMP_RATIO)
            && !BAND.contains(&BUG_RATIO),
        "the band {BAND:?} must admit {NIGHT_RATIO:.3} and reject {AMBIENT_FREE_RATIO:.3} \
         (AmbientColor dropped), {OLD_RAMP_RATIO:.3} (retired ramp) and {BUG_RATIO:.3} \
         (no time-of-day term)"
    );
    assert!(
        !BAND.contains(&OLD_RAMP_RATIO),
        "the band {BAND:?} accepts the retired linear ramp's {OLD_RAMP_RATIO:.3}, so it cannot \
         distinguish vanilla's curve from `0.2 + 0.8 * l`"
    );
    assert!(
        BAND.contains(&ratio),
        "a sky-lit mob at midnight must render near {NIGHT_RATIO:.3} of its noon brightness, got \
         {ratio:.3} (noon {day:.1}, midnight {night:.1}). A ratio near \
         {AMBIENT_FREE_RATIO:.3} means `AmbientColor` was dropped; a ratio near \
         {OLD_RAMP_RATIO:.3} means \
         the retired `0.2 + 0.8 * l` ramp is back; a ratio near {BUG_RATIO:.3} means there is \
         still no time-of-day term and mobs are full-bright at night."
    );
}

/// A **torch-lit** mob must not dim at night. Vanilla scales only the sky half of
/// the lightmap (`get_brightness(sky_level) * SkyFactor`, with the block half
/// untouched), so the combined value is pinned by `block` here and the clock
/// cannot reach it.
///
/// Without this, the obvious wrong fix — scaling the whole `light_term` — passes
/// the gate above and turns every lit cave and every torch-lit room black at
/// sunset. The assertion is an exact zero-pixel difference, not a tolerance.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn a_torch_lit_mob_is_identical_at_midnight_and_noon() {
    let gpu = gpu_or_fail();

    let midnight = sky_darken_for_time_of_day(18_000);
    let day = mob_frame(&gpu, LIGHT_TORCH, NOON);
    let night = mob_frame(&gpu, LIGHT_TORCH, midnight);
    let (day_mean, _) = mob_mean(&day);
    let (night_mean, _) = mob_mean(&night);
    let diff = differing_pixels(&day, &night);

    // Positive control for this detector: the *sky*-lit pair must differ in a
    // large number of pixels, proving `differing_pixels` can see a change at all.
    let sky_diff = differing_pixels(
        &mob_frame(&gpu, LIGHT_SKY, NOON),
        &mob_frame(&gpu, LIGHT_SKY, midnight),
    );

    println!("=== TORCH-LIT INVARIANCE ===");
    println!("torch mob, noon     = {day_mean:.1}");
    println!("torch mob, midnight = {night_mean:.1}");
    println!("differing pixels    = {diff} (must be 0)");
    println!("detector control: sky-lit pair differs in {sky_diff} pixels");

    assert!(
        sky_diff > 2000,
        "DETECTOR CONTROL DID NOT FIRE: the sky-lit pair differs in only {sky_diff} pixels, so a \
         zero difference below would prove nothing about the torch-lit pair"
    );
    assert_eq!(
        diff, 0,
        "a torch-lit mob (sky 0, block 15) changed in {diff} pixels between noon and midnight \
         (means {day_mean:.1} -> {night_mean:.1}). Sky darkening must scale only the sky half of \
         the lightmap; scaling the whole light_term blacks out every lit interior at sunset."
    );
}

/// **No regression for callers that predate this term.** Every existing path
/// builds the group-0 uniform from a `FogUniform` that leaves the sky-darken lane
/// at `0.0`. That must render *byte-identically* to explicit noon, or this change
/// silently renders every mob in the demo world and in a dozen other gates pure
/// black — the old ramp's `0.2` floor at least left them dimly visible, so this
/// sentinel matters more now than it did.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn the_unset_lane_renders_identically_to_explicit_noon() {
    let gpu = gpu_or_fail();

    let unset = mob_frame(&gpu, LIGHT_SKY, UNSET);
    let noon = mob_frame(&gpu, LIGHT_SKY, NOON);
    let diff = differing_pixels(&unset, &noon);
    let (unset_mean, _) = mob_mean(&unset);
    let (noon_mean, _) = mob_mean(&noon);

    // Control that the comparison is capable of seeing a difference at all.
    let midnight_diff = differing_pixels(&unset, &mob_frame(&gpu, LIGHT_SKY, 0.24));

    println!("=== UNSET-LANE SENTINEL ===");
    println!("lane 0.0 (unset) = {unset_mean:.1}");
    println!("lane 1.0 (noon)  = {noon_mean:.1}");
    println!("differing pixels = {diff} (must be 0)");
    println!("detector control: unset vs midnight differs in {midnight_diff} pixels");

    assert!(
        midnight_diff > 2000,
        "DETECTOR CONTROL DID NOT FIRE: unset vs midnight differs in only {midnight_diff} \
         pixels, so the zero below is not evidence"
    );
    assert_eq!(
        diff, 0,
        "the unset (0.0) sky-darken lane must read as full daylight, but it differs from \
         explicit noon in {diff} pixels ({unset_mean:.1} vs {noon_mean:.1}). Taken literally, \
         0.0 renders every pre-existing caller's sky-lit mobs pure black."
    );
}
