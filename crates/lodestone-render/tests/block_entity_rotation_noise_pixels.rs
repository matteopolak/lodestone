//! Owner report: dense 50/50 black-and-white speckle ("static") on banners, on
//! *some* player heads, and on player skins — with the observation that a head
//! is clean when its rotation lands on `0/90/180/270` and noisy otherwise.
//!
//! # What this file measures, and what it found
//!
//! The entity shader reconstructs its face normal from the screen-space
//! derivatives of a position varying (`entity.wgsl`'s `shade_entity`). Fed
//! **absolute world** coordinates, that varying quantises to the `f32` ULP at
//! the player's distance from the world origin, and the derivative of the
//! resulting staircase is either zero or one whole step, chosen per 2x2 quad —
//! so the normal is noise and the two-light diffuse term paints it as speckle.
//!
//! The rotation signature is the tell and it is exactly the owner's: an
//! axis-aligned face holds two of its three world components exactly constant,
//! so those derivatives are exactly zero and nothing can cancel. Measured on the
//! real skull rig with a **uniform** texture, so any spatial variation at all is
//! geometry or shading and never texel data:
//!
//! | world origin | 0°/90°/180°/270° | 22.5° | 45° | 67.5° |
//! |---|---|---|---|---|
//! | 0 … 8,000 | 0 / 0 | 0 / 0 | 0 / 0-1 | 0 / 0 |
//! | 30,000 | 0 / 0 | 8 / 24 | 4 / 8 | 0 / 38 |
//! | 100,000 | 0 | 8 | 3 | **175** |
//!
//! (speckle / roughness; the 100,000 row is speckle only, from the first run.)
//! Those are the numbers **before** the fix, observed failing — which is what
//! makes this gate's control real rather than constructed afterwards. After
//! carrying a model-local varying for the derivative instead, every arm is 0.
//!
//! **Scope, stated plainly.** This is a real defect with the reported
//! signature, but its amplitude only becomes measurable past roughly 20,000
//! blocks from the origin in this fixture. If the owner is nearer the origin
//! than that, something else is also in play and this gate cannot see it.
//! Every existing entity gate in this crate spawns at `[0, 0, 0]`, which is
//! precisely the one coordinate where the two hypotheses coincide.
//!
//! # The control was observed, not constructed
//!
//! With the one shader line put back to `dpdx(in.world)` and nothing else
//! changed, [`skull_rotation_segments_are_speckle_free`] fails and the other
//! two pass. So the skull gate is the discriminating one; the banner gate is
//! **not** sensitive to this defect (a flag is one flat quad whose normal is
//! well conditioned even out at 30,000) and exists for a different reason —
//! it is the only thing in this crate that draws the opaque body/flag pass and
//! the ordered mask layers **together**, the way production does.
//!
//! ```text
//! cargo test -p lodestone-render --test block_entity_rotation_noise_pixels -- --ignored --nocapture
//! ```

mod gate_harness;

use glam::Vec3;
use lodestone_render::block::DepthBuffer;
use lodestone_render::block_entity::{
    BlockEntityModelSet, SkullOrientation, SkullSpawn, SkullType,
};
use lodestone_render::camera::Camera;
use lodestone_render::entity::ENTITY_FULLBRIGHT;
use lodestone_render::entity_pipeline::{
    EntityPipeline, GpuEntityModel, InstanceTint, upload_instances_tinted,
};

/// 512, not 256, and that is sensitivity rather than taste: the defect this
/// file gates against is a *per-pixel step versus `f32` ULP* race, so halving
/// the step by doubling the resolution halves the world coordinate at which it
/// bites. Measured on the neutered shader, at `dist 1.2`, speckled pixels at
/// 67.5 degrees: at 256 the first non-zero arm is 30,000; at 1024 it is
/// **4,096** (320 px, against 0 at every axis-aligned rotation). A real
/// window is larger than either, so a frame that is clean here is not proof of
/// a frame that is clean on the owner's screen — only of the same inequality
/// with more margin.
const W: u32 = 512;
const H: u32 = 512;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const CLEAR: wgpu::Color = wgpu::Color { r: 0.0, g: 0.5, b: 0.0, a: 1.0 };

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn setup() -> Gpu {
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
            .expect("an adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("block-entity-rotation-noise"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .expect("a device");
        Gpu { device, queue }
    })
}

/// Eye in front of the head block at `origin`; the skull box occupies
/// roughly `0.25..0.75` in x/z and `0..0.5` in y of that block.
fn camera_at(origin: [i32; 3], distance: f32) -> Camera {
    Camera {
        position: Vec3::new(
            origin[0] as f32 + 0.5,
            origin[1] as f32 + 0.25,
            origin[2] as f32 - distance,
        ),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// A uniform opaque texture. Any spatial variation in the rendered rig must
/// then come from geometry or shading, never from the texel data — which is
/// what makes a speckle count interpretable.
fn uniform_sheet(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const N: u32 = 64;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rotation-noise-sheet"),
        size: wgpu::Extent3d { width: N, height: N, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes: Vec<u8> = std::iter::repeat([255u8, 255, 255, 255])
        .take((N * N) as usize)
        .flatten()
        .collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(N * 4), rows_per_image: Some(N) },
        wgpu::Extent3d { width: N, height: N, depth_or_array_layers: 1 },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("rotation-noise-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// Draws the real skull rig at `rotation_segment` through the real opaque
/// block-entity path (`EntityPipeline::pipeline`, one draw per part).
fn render_skull(gpu: &Gpu, rotation_segment: u8, origin: [i32; 3], distance: f32) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = camera_at(origin, distance);

    let models = BlockEntityModelSet::load();
    let spawn = SkullSpawn {
        orientation: SkullOrientation::Floor { rotation_segment },
        skull_type: SkullType::Player,
        ..SkullSpawn::at(origin)
    };
    let instance = models.resolve_skull(&spawn).expect("skull model in corpus");
    let mesh = models.get(instance.model).expect("skull mesh");
    let gpu_model = GpuEntityModel::upload_parts(
        device,
        &mesh.vertices,
        &mesh.indices,
        mesh.parts.clone(),
    )
    .expect("non-empty skull mesh");

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (view, sampler) = uniform_sheet(device, queue);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &view, &sampler);

    let light = u32::from(ENTITY_FULLBRIGHT);
    let part_buffers: Vec<Option<wgpu::Buffer>> = instance
        .part_transforms
        .iter()
        .map(|m| {
            upload_instances_tinted(device, &[*m], &[light], &[InstanceTint::rgb([255, 255, 255])])
        })
        .collect();

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rotation-noise-color"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rotation-noise-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(CLEAR), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        for (range, buf) in gpu_model.parts.iter().zip(&part_buffers) {
            let (Some(buf), true) = (buf, range.index_count > 0) else {
                continue;
            };
            pass.set_vertex_buffer(0, gpu_model.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_model.indices.slice(..), wgpu::IndexFormat::Uint32);
            let end = range.index_start + range.index_count;
            pass.draw_indexed(range.index_start..end, 0, 0..1);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rotation-noise-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map failed"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll failed");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let mut out = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let src = (y * padded) as usize;
        let dst = (y * bytes_per_row) as usize;
        out[dst..dst + bytes_per_row as usize].copy_from_slice(&mapped[src..src + bytes_per_row as usize]);
    }
    drop(mapped);
    readback.unmap();
    out
}

fn px(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

/// The clear colour as it lands in the readback. [`CLEAR`] is a *linear*
/// `0.5` green and the target is `Rgba8UnormSrgb`, so the stored byte is its
/// sRGB encoding (~188), not `128` — reading the linear value here is what
/// made the first run of this probe classify every pixel as rig and measure
/// nothing.
fn is_bg(p: [u8; 4]) -> bool {
    p[0] < 8 && p[1] > 180 && p[1] < 196 && p[2] < 8
}

/// A pixel differing from both horizontal neighbours by more than `8/255` in
/// some channel, counted only where the **whole 3x3 neighbourhood** is rig.
///
/// The interior restriction is part of what the metric means, not a threshold
/// fitted to an answer: a silhouette pixel's value is decided by which
/// primitive won the rasteriser's coverage test, which moves by one pixel for
/// any sub-pixel change in vertex position, and that is a different phenomenon
/// from the per-quad shading noise this file is about.
fn speckle(pixels: &[u8]) -> usize {
    let mut n = 0;
    for y in 1..H - 1 {
        for x in 1..W - 1 {
            let c = px(pixels, x, y);
            let l = px(pixels, x - 1, y);
            let r = px(pixels, x + 1, y);
            let mut any_bg = false;
            for dy in 0..3u32 {
                for dx in 0..3u32 {
                    any_bg |= is_bg(px(pixels, x + dx - 1, y + dy - 1));
                }
            }
            if any_bg {
                continue;
            }
            let d = |a: [u8; 4], b: [u8; 4]| {
                (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0)
            };
            if d(c, l) > 8 && d(c, r) > 8 {
                n += 1;
            }
        }
    }
    n
}

/// Mean absolute deviation of each rig pixel's green channel from the median
/// of its 3x3 neighbourhood, in 1/1000 of a byte. [`speckle`] only counts
/// isolated single-pixel outliers above a hard threshold, so it is blind to
/// dense low-amplitude noise; this is the sensitive companion.
fn roughness(pixels: &[u8]) -> u32 {
    let mut total = 0u64;
    let mut n = 0u64;
    for y in 1..H - 1 {
        for x in 1..W - 1 {
            let c = px(pixels, x, y);
            if is_bg(c) {
                continue;
            }
            let mut window: Vec<u8> = Vec::with_capacity(9);
            let mut any_bg = false;
            for dy in 0..3u32 {
                for dx in 0..3u32 {
                    let q = px(pixels, x + dx - 1, y + dy - 1);
                    any_bg |= is_bg(q);
                    window.push(q[1]);
                }
            }
            if any_bg {
                continue;
            }
            window.sort_unstable();
            total += u64::from(c[1].abs_diff(window[4])) * 1000;
            n += 1;
        }
    }
    if n == 0 { 0 } else { (total / n) as u32 }
}

fn covered(pixels: &[u8]) -> usize {
    (0..W * H)
        .filter(|i| !is_bg(px(pixels, i % W, i / W)))
        .count()
}

/// One measured configuration, collected rather than asserted in the loop:
/// an `assert!` inside the sweep would abort on the first failure and leave
/// every later arm an argument rather than an observation.
#[derive(Debug)]
struct Arm {
    distance: f32,
    origin: i32,
    segment: u8,
    speckle: usize,
    roughness: u32,
}

#[test]
#[ignore = "requires a GPU adapter"]
fn skull_rotation_segments_are_speckle_free() {
    let gpu = setup();
    let mut noisy: Vec<Arm> = Vec::new();
    for distance in [1.2f32, 8.0] {
        for origin in [
            [0, 0, 0],
            [512, 0, 512],
            [2_000, 0, 2_000],
            [4_096, 0, 4_096],
            [8_000, 0, 8_000],
            [30_000, 0, 30_000],
            [100_000, 0, 100_000],
        ] {
            for segment in [0u8, 1, 2, 3, 4] {
                let frame = render_skull(&gpu, segment, origin, distance);
                let speckle = speckle(&frame);
                let covered = covered(&frame);
                let roughness = roughness(&frame);
                println!(
                    "dist {distance:4.1} origin {:7} segment {segment} ({:5.1}°): covered {covered:6}  speckle {speckle:5}  roughness {roughness:6}",
                    origin[0],
                    f32::from(segment) * 22.5
                );
                assert!(covered > 100, "the rig must actually be on screen");
                if speckle > 0 || roughness > 0 {
                    noisy.push(Arm {
                        distance,
                        origin: origin[0],
                        segment,
                        speckle,
                        roughness,
                    });
                }
            }
        }
    }
    assert!(noisy.is_empty(), "speckled configurations: {noisy:#?}");
}

// ---------------------------------------------------------------------------
// The banner half: the **production composition** — the opaque body/flag pass
// (`EntityPipeline::pipeline`, depth-write on, `banner_base` sheet) followed by
// the ordered mask layers (`banner_layer_pipeline`, alpha-blended,
// depth-write off) over the *same* flag geometry. `banner_pattern_layer_pixels.rs`
// draws only the layers, so nothing in this crate has ever rendered the two
// together — and the reported artefact is on a banner that has both.
// ---------------------------------------------------------------------------

/// Real decoded sprite → GPU texture, mirroring what the shell's own
/// `entity_texture_from_image` does (single mip level, nearest, sRGB).
fn sheet_from_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &lodestone_assets::Image,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rotation-noise-real-sheet"),
        size: wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &img.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(img.width * 4),
            rows_per_image: Some(img.height),
        },
        wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("rotation-noise-real-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

fn jar_manager() -> lodestone_assets::ResourceManager {
    let path = gate_harness::require_client_jar();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let zip = lodestone_assets::ZipSource::from_bytes(bytes)
        .unwrap_or_else(|e| panic!("open jar: {e}"));
    lodestone_assets::ResourceManager::new(vec![Box::new(zip) as Box<dyn lodestone_assets::ResourceSource>])
}

fn banner_camera(origin: [i32; 3]) -> Camera {
    Camera {
        position: Vec3::new(
            origin[0] as f32 + 0.5,
            origin[1] as f32 + 1.0,
            origin[2] as f32 - 2.0,
        ),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

fn render_banner(
    gpu: &Gpu,
    manager: &lodestone_assets::ResourceManager,
    origin: [i32; 3],
    rotation_segment: u8,
    phase: f32,
    patterns: &[lodestone_render::banner_pattern::StoredPatternLayer],
) -> Vec<u8> {
    use lodestone_render::block_entity::{BannerAttachment, BannerSpawn};

    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = banner_camera(origin);

    let masks = lodestone_assets::BannerPatternAtlas::load(manager).expect("banner masks");
    let base_png = manager
        .read("assets/minecraft/textures/entity/banner/banner_base.png")
        .expect("banner_base.png in jar");
    let base_img = lodestone_assets::Image::decode_png(&base_png).expect("decode banner_base");

    let models = BlockEntityModelSet::load();
    let spawn = BannerSpawn {
        attachment: BannerAttachment::Ground { rotation_segment },
        phase,
        patterns: patterns.to_vec(),
        ..BannerSpawn::at(origin)
    };
    let resolved = models.resolve_banner(&spawn).expect("banner rig");

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let layer_pipeline = pipeline.banner_layer_pipeline(device, COLOR_FORMAT);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);

    let (base_view, base_sampler) = sheet_from_image(device, queue, &base_img);
    let base_bg = pipeline.texture_bind_group(device, &base_view, &base_sampler);

    let light = u32::from(ENTITY_FULLBRIGHT);
    let white = InstanceTint::rgb([255, 255, 255]);

    struct Opaque {
        gpu_model: GpuEntityModel,
        parts: Vec<Option<wgpu::Buffer>>,
    }
    let opaque: Vec<Opaque> = [&resolved.body, &resolved.flag]
        .into_iter()
        .map(|inst| {
            let mesh = models.get(inst.model).expect("mesh");
            let gpu_model =
                GpuEntityModel::upload_parts(device, &mesh.vertices, &mesh.indices, mesh.parts.clone())
                    .expect("non-empty mesh");
            let parts = inst
                .part_transforms
                .iter()
                .map(|m| upload_instances_tinted(device, &[*m], &[light], &[white]))
                .collect();
            Opaque { gpu_model, parts }
        })
        .collect();

    let flag_mesh = models.get(resolved.flag.model).expect("flag mesh");
    let flag_index = flag_mesh.index_of("flag").expect("flag part");
    let flag_gpu = GpuEntityModel::upload_parts(
        device,
        &flag_mesh.vertices,
        &flag_mesh.indices,
        flag_mesh.parts.clone(),
    )
    .expect("non-empty flag mesh");

    struct LayerDraw {
        buf: wgpu::Buffer,
        bg: wgpu::BindGroup,
        _keep: (wgpu::TextureView, wgpu::Sampler),
    }
    let layer_draws: Vec<LayerDraw> = resolved
        .layers
        .iter()
        .filter_map(|layer| {
            let img = masks.get_sprite(&layer.sprite)?;
            let keep = sheet_from_image(device, queue, img);
            let bg = pipeline.texture_bind_group(device, &keep.0, &keep.1);
            let rgb = lodestone_render::gamma_rgb_to_bytes(layer.color);
            let buf = upload_instances_tinted(
                device,
                &[layer.transform],
                &[u32::from(layer.light)],
                &[InstanceTint::rgb(rgb)],
            )?;
            Some(LayerDraw { buf, bg, _keep: keep })
        })
        .collect();
    assert_eq!(
        layer_draws.len(),
        resolved.layers.len(),
        "every resolved layer must have a real jar mask"
    );

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rotation-noise-banner-color"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rotation-noise-banner-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(CLEAR), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &base_bg, &[]);
        for o in &opaque {
            for (range, buf) in o.gpu_model.parts.iter().zip(&o.parts) {
                let (Some(buf), true) = (buf, range.index_count > 0) else {
                    continue;
                };
                pass.set_vertex_buffer(0, o.gpu_model.vertices.slice(..));
                pass.set_vertex_buffer(1, buf.slice(..));
                pass.set_index_buffer(o.gpu_model.indices.slice(..), wgpu::IndexFormat::Uint32);
                let end = range.index_start + range.index_count;
                pass.draw_indexed(range.index_start..end, 0, 0..1);
            }
        }
        if let Some(range) = flag_gpu.parts.get(flag_index).filter(|r| r.index_count > 0) {
            pass.set_pipeline(&layer_pipeline);
            pass.set_bind_group(0, &cam_bg, &[]);
            pass.set_vertex_buffer(0, flag_gpu.vertices.slice(..));
            pass.set_index_buffer(flag_gpu.indices.slice(..), wgpu::IndexFormat::Uint32);
            for draw in &layer_draws {
                pass.set_bind_group(1, &draw.bg, &[]);
                pass.set_vertex_buffer(1, draw.buf.slice(..));
                let end = range.index_start + range.index_count;
                pass.draw_indexed(range.index_start..end, 0, 0..1);
            }
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rotation-noise-banner-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map failed"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll failed");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let mut out = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let src = (y * padded) as usize;
        let dst = (y * bytes_per_row) as usize;
        out[dst..dst + bytes_per_row as usize].copy_from_slice(&mapped[src..src + bytes_per_row as usize]);
    }
    drop(mapped);
    readback.unmap();
    out
}

/// The banner half of the same question, over the **production composition**:
/// the opaque body/flag pass (depth-write on, `banner_base` sheet) followed by
/// the ordered real-jar mask layers (alpha-blended, depth-write off) over the
/// same flag geometry. `banner_pattern_layer_pixels.rs` draws only the layers,
/// so nothing in this crate had ever rendered the two together.
///
/// **Segment 0 only, and that is a scope statement rather than a convenience.**
/// At segment 0 the cloth faces the camera square-on and covers ~10,700 px, so
/// the frame is almost all flag interior. Turned edge-on the rig is mostly its
/// pole and bar, whose left and right faces are genuinely one pixel apart and
/// genuinely differently shaded — [`speckle`] counts those real face seams and
/// measured 7 / 14 / 69 / 127 at segments 1 / 2 / 3 / 4, **unchanged by the
/// fix this file exists for** (127 before and after at segment 4). Those are
/// thin geometry, not noise, so asserting on them would be asserting on the
/// wrong thing; they are printed as diagnostics instead.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar"]
fn banner_body_flag_and_mask_layers_are_speckle_free() {
    let gpu = setup();
    let manager = jar_manager();
    let mut noisy: Vec<(i32, f32, usize, u32)> = Vec::new();
    for origin in [[0, 0, 0], [30_000, 0, 30_000]] {
        for phase in [0.0f32, 0.13, 0.37] {
            let frame = render_banner(&gpu, &manager, origin, 0, phase, &[]);
            let speckle = speckle(&frame);
            let covered = covered(&frame);
            let roughness = roughness(&frame);
            println!(
                "white banner origin {:7} phase {phase:4.2}: covered {covered:6}  speckle {speckle:5}  roughness {roughness:6}"
                , origin[0]
            );
            assert!(covered > 5_000, "the cloth must actually fill the frame");
            // Speckle only. `roughness` is meaningless here and is printed
            // rather than asserted: the real `base` mask is a grey ramp
            // (`base.png`'s palette runs f6, f5, f4, ...), so a real banner
            // legitimately varies texel to texel and the median-filter
            // deviation sits around 155 in a perfectly correct frame. The
            // skull gate above can use it only because its texture is uniform
            // by construction.
            if speckle > 0 {
                noisy.push((origin[0], phase, speckle, roughness));
            }
        }
    }
    assert!(
        noisy.is_empty(),
        "speckled (origin, phase, speckle, roughness): {noisy:?}"
    );
}

/// The two things the gate above deliberately does not assert on, printed so
/// the numbers stay on record.
///
/// **Edge-on rotations.** Turned away from the camera the rig is mostly its
/// pole and bar, whose left and right faces are genuinely one pixel apart and
/// genuinely differently shaded; [`speckle`] counts those real face seams.
/// Measured 6 / 14 / 68 / 127 at segments 1 / 2 / 3 / 4, and **unchanged by
/// the normal-precision fix** (127 before and after at segment 4), which is
/// what identifies them as geometry rather than noise.
///
/// **A banner 100,000 blocks from the origin** still leaves 3 interior
/// speckled pixels, and that residue has a *different* cause with a different
/// fix. `vs_main` still computes `world = model * position` and
/// `clip = view_proj * world` in absolute world space, so the vertex positions
/// themselves quantise: one `f32` ULP at 100,000 is 0.0078 blocks, against a
/// flag box only 0.0625 blocks deep — eight ULPs — so its 1-texel side faces
/// genuinely collapse toward degenerate. Removing that needs the *instance
/// matrices* built camera-relative on the CPU, in `entity_pipeline.rs` and its
/// callers, not a shader change. The skull rig is a full 0.5-block cube and is
/// clean at 100,000, so this is specific to very thin geometry very far out.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar"]
fn banner_edge_on_and_very_far_speckle_have_other_causes() {
    let gpu = setup();
    let manager = jar_manager();
    for segment in [1u8, 2, 3, 4] {
        let frame = render_banner(&gpu, &manager, [0, 0, 0], segment, 0.0, &[]);
        println!(
            "white banner segment {segment} edge-on: covered {:6}  speckle {:5}",
            covered(&frame),
            speckle(&frame)
        );
    }
    let far = render_banner(&gpu, &manager, [100_000, 0, 100_000], 0, 0.0, &[]);
    println!(
        "white banner origin 100000 front-on: covered {:6}  speckle {:5}",
        covered(&far),
        speckle(&far)
    );
}
