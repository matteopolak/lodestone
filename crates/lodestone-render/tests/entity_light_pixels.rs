//! **Defect 1 — "mobs are super bright, blocks are dark."** Measure a mob's
//! surface and terrain's four lighting clusters *in the same units*, through the
//! two real pipelines, and prove entities track world light the way terrain does.
//!
//! # Why this file exists, and why it measures by location
//!
//! The player-visible symptom is a *ratio between two populations*: mob pixels
//! versus block pixels. A frame average merges them into a number describing
//! neither, so every reading here is taken from a named cluster —
//! `(direction, light level)` for terrain, and the mob silhouette for entities —
//! with the same mid-grey texel (byte `128`) feeding both, so a difference in the
//! readback can only be a difference in *shading*, never in the texture.
//!
//! Terrain goes through [`mesh_models`] (the model path live server terrain
//! actually uses via `lodestone-shell`'s mesher), never `mesh_simple`: only
//! `mesh_models` calls `face_shade`, and a gate built on the demo path would be
//! structurally unable to see per-face shading at all.
//!
//! Both pipelines render to an **sRGB** target, matching the real swapchain
//! (`Bgra8UnormSrgb`), because the whole defect is about where the shade
//! multiply lands relative to the transfer curve.
//!
//! `#[ignore]`d: needs a real GPU adapter, and once opted in a missing adapter is
//! a failure, never a skip.

use glam::Vec3;
use lodestone_assets::entity_models::zombie_model;
use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityInstance, EntityMesh, plan_entities};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};
use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelPipeline, ModelSectionView, mesh_models, model_anim_buffer,
    model_palette_buffer, model_shared_camera_buffer, section_origin_buffer,
};

const W: u32 = 192;
const H: u32 = 192;
/// Matches the real swapchain (`Bgra8UnormSrgb`) in transfer behaviour: the
/// shader's output is gamma-encoded on write. Measuring on a plain `Unorm`
/// target would hide exactly the colour-space half of this defect.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The one texel value every surface in this file is painted with, so a readback
/// difference is a shading difference. Mid-grey in **sRGB bytes**.
const TEXEL: u8 = 128;

/// Full sky light (`sky = 15`, `block = 0`) packed as the shader reads it.
const LIGHT_FULL: u8 = 15 << 4;
/// No light at all. Under vanilla's curve this is exactly `0.0` — the retired
/// `0.2 + 0.8 * l` ramp's floor is gone — so a frame at this level is **black**
/// and every *ratio* taken against it is degenerate. Kept for the census and for
/// [`the_light_floor_is_gone`], which is the one assertion that wants it.
const LIGHT_DARK: u8 = 0;
/// Half-ish sky light (`sky = 7`, `block = 0`), which is where the two candidate
/// curves actually disagree: vanilla's `get_brightness(7/15)` is `0.17949`, and
/// mixing `notGamma` in at the default gamma of `0.5` gives `0.36312`, against
/// the retired ramp's `0.2 + 0.8 * 0.46667 = 0.57333`. Both curves are exactly
/// `1.0` at [`LIGHT_FULL`] and exactly equal at nothing else, so the interior is
/// the only place a gate can tell them apart.
const LIGHT_DIM: u8 = 7 << 4;

/// Sky colour behind the mob; nothing else in the entity frame is this colour,
/// which is what separates mob pixels from background.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
    a: 1.0,
};

/// A macro rather than a function because `RenderPassDescriptor` borrows the
/// `color_attachments` slice, which cannot outlive a helper function's frame.
macro_rules! pass_desc {
    ($color:expr, $depth:expr) => {
        wgpu::RenderPassDescriptor {
            label: Some("entity-light-pass"),
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
                label: Some("entity_light_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

// ---------------------------------------------------------------- terrain side

/// A full-frame quad facing `dir`. `direction` is what `face_shade` reads; the
/// positions are a clip-space full-frame quad under the identity camera, exactly
/// as `model_shade_gamma_gate` does it.
fn face_quad(dir: Direction) -> BakedQuad {
    BakedQuad {
        positions: [
            [-1.0, -1.0, 0.5],
            [1.0, -1.0, 0.5],
            [1.0, 1.0, 0.5],
            [-1.0, 1.0, 0.5],
        ],
        uvs: [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        direction: dir,
        cullface: None,
        tint_index: None,
        shade: true,
        layer: 0,
        anim: 0,
    }
}

struct OneQuad {
    quads: Vec<BakedQuad>,
    light: u8,
}

impl ModelSectionView for OneQuad {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        if (x, y, z) == (0, 0, 0) {
            &self.quads
        } else {
            &[]
        }
    }
    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    fn light_at(&self, _x: usize, _y: usize, _z: usize) -> u8 {
        self.light
    }
}

/// Render one terrain face at one light level through the real model path and
/// return its displayed byte.
fn terrain_luma(gpu: &Gpu, dir: Direction, light: u8) -> u8 {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(
        device,
        queue,
        4,
        4,
        &[TEXEL, TEXEL, TEXEL, 255].repeat(16),
        &[],
    );
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);
    let palette = vec![[1.0_f32, 1.0, 1.0, 1.0]; 256];
    let palette_buffer = model_palette_buffer(device, &palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    let cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);

    let view = OneQuad {
        quads: vec![face_quad(dir)],
        light,
    };
    let mesh = mesh_models(&view);
    assert_eq!(mesh.quad_count(), 1, "expected one meshed quad for {dir:?}");
    let gpu_mesh = GpuModelMesh::upload(device, &mesh).expect("non-empty");

    let (color, color_view) = color_target(device);
    let depth = DepthBuffer::new(device, W, H);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&pass_desc!(&color_view, &depth.view));
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[0]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_bind_group(2, &palette_bg, &[]);
        pass.set_bind_group(3, &anim_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }
    let frame = readback(device, queue, enc, &color);
    let i = ((H / 2) * W + W / 2) as usize * 4;
    frame[i]
}

// ----------------------------------------------------------------- entity side

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
        label: Some("entity-light-sheet"),
        size: wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // sRGB so the sampled texel is linear-light, exactly like the real
        // entity sheets the shell uploads.
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
        label: Some("entity-light-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (texture, sampler)
}

/// Render one zombie lit at `light` and return the whole RGBA frame.
fn mob_frame(gpu: &Gpu, light: u8) -> Vec<u8> {
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
    let cam_buf = pipeline.camera_buffer(device, &camera);
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
/// The clear is specified in **linear** light and this target is `_srgb`, so the
/// value that lands in the readback is the *encoded* clear, not `CLEAR * 255`.
/// Comparing against the unencoded triple classifies the entire frame — corners
/// included — as mob, which is how two of the shell's own entity gates silently
/// ended up measuring the whole frame instead of a silhouette.
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

/// `(min, max, mean, area)` of the mob silhouette's red channel. The sheet is
/// neutral grey, so red is a faithful luma proxy.
fn mob_stats(frame: &[u8]) -> (u8, u8, f32, u32) {
    let (mut lo, mut hi, mut sum, mut n) = (255u8, 0u8, 0u64, 0u32);
    for i in 0..(W * H) as usize {
        if is_mob(frame, i * 4) {
            let v = frame[i * 4];
            lo = lo.min(v);
            hi = hi.max(v);
            sum += u64::from(v);
            n += 1;
        }
    }
    assert!(n > 2000, "only {n} mob pixels found — readings would be slivers");
    (lo, hi, sum as f32 / n as f32, n)
}

// -------------------------------------------------------------- shared plumbing

fn color_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-light-color"),
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
        label: Some("entity-light-readback"),
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

/// Print every cluster's number. Not an assertion — this is the measurement the
/// diagnosis is built on, kept so the numbers can be re-derived on any machine.
#[test]
#[ignore = "requires a GPU adapter; run explicitly for the lighting census"]
fn lighting_census_by_location() {
    let Some(gpu) = setup() else {
        panic!("entity_light_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a failure")
    };
    println!("=== LIGHTING CENSUS (texel {TEXEL}, sRGB target) ===");
    for (name, dir, light) in [
        ("terrain top,  sunlit  ", Direction::Up, LIGHT_FULL),
        ("terrain side,  sunlit  ", Direction::East, LIGHT_FULL),
        ("terrain N/S,   sunlit  ", Direction::North, LIGHT_FULL),
        ("terrain bottom,sunlit  ", Direction::Down, LIGHT_FULL),
        ("terrain top,   dim     ", Direction::Up, LIGHT_DIM),
        ("terrain side,  dim     ", Direction::East, LIGHT_DIM),
        ("terrain top,   shadow  ", Direction::Up, LIGHT_DARK),
        ("terrain side,  shadow  ", Direction::East, LIGHT_DARK),
    ] {
        println!("{name} -> byte {}", terrain_luma(&gpu, dir, light));
    }
    for (name, light) in [
        ("mob, sunlit ", LIGHT_FULL),
        ("mob, dim    ", LIGHT_DIM),
        ("mob, shadow ", LIGHT_DARK),
    ] {
        let (lo, hi, mean, area) = mob_stats(&mob_frame(&gpu, light));
        println!("{name} -> min {lo} max {hi} mean {mean:.1} over {area}px");
    }
}

/// **The gate.** A mob in dim light must be measurably darker than the same mob
/// in sunlight, and must land on vanilla's own curve — not merely "somewhat
/// dimmer", and not on the retired linear ramp either.
///
/// # Three predictions, named
///
/// All three are arithmetic on `lightmap.fsh` and `Options.java:900`, written out
/// below rather than read back from `lodestone_render::light`.
///
/// * [`LIT_RATIO`] — the correct one. At [`LIGHT_DIM`] the combined level is
///   `7/15`, `get_brightness` gives `0.17949`, and `notGamma` mixed in at the
///   default gamma of `0.5` gives **0.36312**. Full light is exactly `1.0`, so
///   that is the ratio.
/// * [`OLD_RAMP_RATIO`] — the retired `0.2 + 0.8 * l` ramp: **0.57333**.
/// * [`FULLBRIGHT_RATIO`] — the original defect, in which the shader ignored
///   world light entirely and both frames rendered identically: **1.0**.
///
/// The band admits only the first. Note the measurement is a plain byte ratio
/// because the shade multiply happens in gamma space against an sRGB target, so
/// the transfer functions cancel.
const LIT_RATIO: f32 = 0.363_12;
const OLD_RAMP_RATIO: f32 = 0.573_33;
const FULLBRIGHT_RATIO: f32 = 1.0;
const BAND: std::ops::RangeInclusive<f32> = 0.345..=0.382;

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fire"]
fn a_mob_in_shadow_is_darker_than_the_same_mob_in_sunlight() {
    let Some(gpu) = setup() else {
        panic!("entity_light_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a failure")
    };

    let (_, _, sunlit, _) = mob_stats(&mob_frame(&gpu, LIGHT_FULL));
    let (_, _, dim, _) = mob_stats(&mob_frame(&gpu, LIGHT_DIM));
    let ratio = dim / sunlit;

    println!("=== MOB WORLD-LIGHT GATE ===");
    println!("mob mean, sky 15 = {sunlit:.1}");
    println!("mob mean, sky 7  = {dim:.1}");
    println!("measured ratio            = {ratio:.3}");
    println!("vanilla curve       (fix) = {LIT_RATIO:.3}");
    println!("retired linear ramp       = {OLD_RAMP_RATIO:.3}");
    println!("fullbright control  (bug) = {FULLBRIGHT_RATIO:.3}");
    println!(
        "negative control: the shipped entity shader had no world-light term at all — every mob \
         vertex carried a hardcoded `ENTITY_LIGHT = 15 << 4` and the shader never read it — so \
         both frames rendered identically and this ratio was exactly {FULLBRIGHT_RATIO:.3}, far \
         outside the band below."
    );

    // Both wrong hypotheses must sit outside the band, asserted rather than
    // asserted-about: a band wide enough to admit either measures nothing.
    assert!(
        !BAND.contains(&OLD_RAMP_RATIO) && !BAND.contains(&FULLBRIGHT_RATIO),
        "the band {BAND:?} admits a wrong hypothesis ({OLD_RAMP_RATIO:.3} or \
         {FULLBRIGHT_RATIO:.3}), so this gate cannot see which curve is installed"
    );
    assert!(
        BAND.contains(&ratio),
        "a mob at sky 7 must render at {LIT_RATIO:.3} of its sunlit brightness (vanilla's \
         `get_brightness` plus `notGamma`), got {ratio:.3} (sunlit {sunlit:.1}, dim {dim:.1}); \
         {OLD_RAMP_RATIO:.3} is the retired linear ramp and {FULLBRIGHT_RATIO:.3} means entities \
         ignore world light entirely"
    );
}

/// **The `0.2` floor is gone.** Issue #386 named that floor as the mechanism: a
/// hard 20% minimum no darkening could go below, where vanilla's
/// `get_brightness(0)` is `0` and `notGamma(0)` is `0`. So a mob at light 0 must
/// render **pure black**, not at 20% of daylight.
///
/// This is an assertion about an extreme, which is exactly the kind that passes
/// vacuously if nothing drew — hence the two controls: the sunlit frame must
/// cover the same silhouette, and it must be far from black.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn the_light_floor_is_gone() {
    let Some(gpu) = setup() else {
        panic!("entity_light_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a failure")
    };

    let (_, sunlit_max, sunlit, sunlit_px) = mob_stats(&mob_frame(&gpu, LIGHT_FULL));
    let (_, dark_max, dark, dark_px) = mob_stats(&mob_frame(&gpu, LIGHT_DARK));

    println!("=== THE 0.2 FLOOR IS GONE ===");
    println!("mob at sky 15: mean {sunlit:.1} max {sunlit_max} over {sunlit_px}px");
    println!("mob at light 0: mean {dark:.1} max {dark_max} over {dark_px}px");
    println!(
        "the retired ramp floored this at 0.2 of daylight, i.e. a mean near {:.1}",
        sunlit * 0.2
    );

    // Control: the mob is still there and still drawing, so a black readback is
    // "the light term is zero" and not "nothing was rasterised".
    assert!(
        dark_px > 2000 && (dark_px as i64 - sunlit_px as i64).abs() < 200,
        "the silhouette must be the same in both frames for this to be a lighting \
         measurement ({dark_px}px dark vs {sunlit_px}px sunlit)"
    );
    assert!(
        sunlit_max > 60,
        "control's premise is false: the sunlit frame is nearly black too ({sunlit_max}), so \
         the assertion below would pass under a build that draws nothing visible"
    );
    assert!(
        dark_max <= 1,
        "a mob at light 0 must be pure black (vanilla's curve reaches 0.0), but its brightest \
         pixel is {dark_max} and its mean {dark:.1}. A mean near {:.1} is the retired ramp's \
         0.2 floor, which is the mechanism issue #386 named",
        sunlit * 0.2
    );
}

/// **The cross-population gate.** The player's complaint is comparative, so
/// assert the comparison: a mob in the dark must not out-shine the terrain
/// around it. Measured against the *brightest* face terrain can show at that
/// same light level (`Up`, shade 1.0), which is the most generous possible
/// comparison for the mob.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fire"]
fn a_mob_does_not_outshine_the_terrain_it_stands_on() {
    let Some(gpu) = setup() else {
        panic!("entity_light_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a failure")
    };

    // Measured at [`LIGHT_DIM`], not at light 0: vanilla's curve takes light 0 to
    // exactly 0.0, so both populations there are black and `mob <= terrain`
    // becomes `0 <= 0` — true under every possible build, including one that
    // draws nothing. The comparison needs a light level at which both sides are
    // visible, which is what the dim level is for.
    let terrain_dim = f32::from(terrain_luma(&gpu, Direction::Up, LIGHT_DIM));
    let (_, mob_max, mob_mean, _) = mob_stats(&mob_frame(&gpu, LIGHT_DIM));

    println!("=== MOB vs TERRAIN AT SKY 7 ===");
    println!("terrain Up face, sky 7 = {terrain_dim:.1}");
    println!("mob mean,        sky 7 = {mob_mean:.1} (max {mob_max})");
    println!("ratio mob/terrain      = {:.2}", mob_mean / terrain_dim);
    println!(
        "negative control: before the fix the mob rendered full-bright (mean ~103 for this \
         texel) against the same terrain byte ~{terrain_dim:.0}, a ratio near 4 — the assertion \
         below caps it at 1.0."
    );

    // Anti-vacuity: both populations must actually be lit, or the inequality is
    // satisfied by two black frames.
    assert!(
        terrain_dim > 20.0 && mob_mean > 5.0,
        "both populations must be visibly lit for this comparison to mean anything \
         (terrain {terrain_dim:.1}, mob {mob_mean:.1})"
    );
    assert!(
        mob_mean <= terrain_dim,
        "a mob at sky 7 (mean {mob_mean:.1}) must not be brighter than the brightest terrain \
         face at the same light level ({terrain_dim:.1}); the mob also carries a directional \
         shade <= 1.0, so equality is the ceiling. A mean several times this bound is the \
         reported 'mobs are super bright' defect"
    );
}
