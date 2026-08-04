//! Prove the creeper white-flash overlay reaches pixels, and prove its
//! **magnitude**, not merely its direction.
//!
//! `CLAUDE.md` records the hurt overlay's own history as the canonical
//! *magnitude* vacuous test: a gate that asserted silhouette pixels "moved
//! toward vanilla's overlay red" passed 3440/3440 while the shader was
//! rendering ~70% red where vanilla renders ~30% — the mix arguments were
//! swapped (issue #371), and a direction-only check could not see it. This
//! gate is written to not repeat that mistake: it predicts the **exact**
//! output byte from constants that originate outside the code under test
//! (vanilla's `OverlayTexture` alpha-derivation formula, transcribed in
//! `entity_pipeline.rs`'s `creeper_overlay_alpha_from_progress`, plus this
//! shader's own documented `srgb_to_linear` transfer function) and requires
//! the measurement to land on that value, not merely move in its direction.
//!
//! # Why a pure-black texture makes an exact prediction tractable
//!
//! `entity.wgsl`'s `shaded` term is `linear_to_srgb(tex_col.rgb) * tint *
//! diffuse * light_term`. A flat **black** (`0,0,0`) sheet makes `tex_col.rgb
//! == 0`, so `shaded == 0` regardless of `tint`, `diffuse` or `light_term` —
//! per-face lighting, camera angle and world light all multiply a hard zero
//! out of the equation. With the red overlay absent and fog disabled
//! (`EntityPipeline::camera_buffer`'s default), the whole pipeline collapses
//! to:
//!
//! ```text
//!   overlaid = mix(white, shaded=0, white_overlay) = (1 - white_overlay)
//!   output   = srgb_to_linear(overlaid)
//! ```
//!
//! — a value this test computes independently in Rust and compares against
//! the measured byte, not merely its sign.
//!
//! # The competing hypothesis
//!
//! The swapped-argument bug that bit the hurt overlay (`mix(shaded, red,
//! alpha)` instead of `mix(red, shaded, alpha)`) has a direct analogue here:
//! `mix(shaded, white, white_overlay)` instead of `mix(white, shaded,
//! white_overlay)`. On this black-texture rig that predicts the
//! **complementary** value, `srgb_to_linear(white_overlay)` rather than
//! `srgb_to_linear(1 - white_overlay)`. The two predictions are computed and
//! printed side by side; the assertion requires the measurement lands on the
//! correct one within rounding tolerance, and only there.
//!
//! # The controls
//!
//! - **Off is bit-identical to before the feature existed** — `control_a`
//!   never calls `with_creeper_white_overlay` at all, `control_b` calls it
//!   with `0`.
//! - **Determinism** — two renders of the same active overlay must match.
//! - **Located, not averaged** — measured only inside the mob's own
//!   silhouette; the background must be untouched.

use glam::Vec3;
use lodestone_assets::entity_models::zombie_model;
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityInstance, EntityMesh, plan_entities};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{
    EntityInstanceRaw, EntityPipeline, GpuEntityModel, creeper_overlay_alpha_from_progress,
};
use wgpu::util::DeviceExt;

const W: u32 = 256;
const H: u32 = 256;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const BODY_YAW: f32 = 90.0;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
    a: 1.0,
};

/// A flat, fully opaque **black** sheet. Every texel is `(0,0,0,255)`, which
/// is the whole reason this gate can predict an exact output byte — see the
/// module doc.
fn black_sheet() -> lodestone_assets::Image {
    const N: u32 = 64;
    lodestone_assets::Image {
        width: N,
        height: N,
        rgba: (0..N * N).flat_map(|_| [0u8, 0, 0, 255]).collect(),
    }
}

/// Byte-for-byte `entity.wgsl`'s `srgb_to_linear`, transcribed independently
/// so the prediction does not share a bug with the shader it is checking.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
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
                label: Some("creeper_white_overlay_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

fn upload_sheet(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &lodestone_assets::Image,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("white-overlay-sheet"),
        size: wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
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
        wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("white-overlay-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
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

/// Built directly rather than through `entity_pipeline::upload_instances`,
/// since that helper has no overlay parameter — same rationale as the hurt
/// overlay gate's own `upload_instances_hurt`.
fn upload_instances_white(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    lights: &[u32],
    alpha_byte: Option<u8>,
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let fallback = u32::from(lodestone_render::entity::ENTITY_FULLBRIGHT);
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let inst = EntityInstanceRaw::new(*m, lights.get(i).copied().unwrap_or(fallback));
            match alpha_byte {
                Some(alpha) => inst.with_creeper_white_overlay(alpha),
                None => inst,
            }
        })
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-white-overlay-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// Render a flat-black zombie. `alpha`: `None` = never call
/// `with_creeper_white_overlay` (the pre-feature code path), `Some(0)`/
/// `Some(byte)` = call it explicitly.
fn render(gpu: &Gpu, mesh: &EntityMesh, alpha: Option<u8>) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = side_camera();
    let img = black_sheet();

    let inst = EntityInstance::new("mob", mesh, Vec3::ZERO, BODY_YAW, 1.0, &AnimInput::REST);
    let frame = plan_entities(std::slice::from_ref(&inst), &camera.frustum());
    assert_eq!(frame.instance_count(), 1, "the mob was culled");

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = upload_sheet(device, queue, &img);
    // Fog disabled by default (see the module doc's derivation).
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_mesh = GpuEntityModel::upload(device, mesh).expect("mesh is non-empty");

    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_mesh.parts.iter().zip(&batch.parts) {
            if range.index_count == 0 {
                continue;
            }
            if let Some(buf) = upload_instances_white(device, mats, &batch.lights, alpha) {
                per_part.push((
                    mats.len() as u32,
                    range.index_start..range.index_start + range.index_count,
                    buf,
                ));
            }
        }
    }
    assert!(!per_part.is_empty(), "no part produced an instance buffer");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("white-overlay-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("white-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
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

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("white-overlay-readback"),
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
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
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
    readback.unmap();
    out
}

fn differing(a: &[u8], b: &[u8]) -> u32 {
    (0..(W * H) as usize)
        .filter(|i| a[i * 4..i * 4 + 3] != b[i * 4..i * 4 + 3])
        .count() as u32
}

fn is_mob(frame: &[u8], i: usize) -> bool {
    let clear = [
        (CLEAR.r * 255.0).round() as u8,
        (CLEAR.g * 255.0).round() as u8,
        (CLEAR.b * 255.0).round() as u8,
    ];
    frame[i..i + 3]
        .iter()
        .zip(clear)
        .any(|(got, want)| got.abs_diff(want) > 8)
}

fn bbox(frame: &[u8]) -> (u32, u32, u32, u32, u32) {
    let (mut x0, mut x1, mut y0, mut y1, mut area) = (W, 0u32, H, 0u32, 0u32);
    for y in 0..H {
        for x in 0..W {
            if is_mob(frame, ((y * W + x) * 4) as usize) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
                area += 1;
            }
        }
    }
    assert!(area > 3000, "only {area} px of mob found");
    (x0, x1, y0, y1, area)
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the creeper white overlay reaches pixels"]
fn white_overlay_matches_the_predicted_byte_on_a_black_mob() {
    let Some(gpu) = setup() else {
        panic!(
            "creeper_white_overlay_pixels: no GPU adapter. This test is #[ignore]d, so running \
             it is an explicit request for the full GPU path — run it on a machine with an \
             adapter."
        );
    };
    let mesh = EntityMesh::from_model(&zombie_model());

    // Executed negative controls, mirroring the hurt-overlay gate.
    let control_a = render(&gpu, &mesh, None);
    let control_b = render(&gpu, &mesh, Some(0));
    let off_vs_off = differing(&control_a, &control_b);
    println!(
        "control A (no with_creeper_white_overlay call) vs control B (with_creeper_white_overlay(0)): \
         {off_vs_off} px differ (must be 0)"
    );
    assert_eq!(
        off_vs_off, 0,
        "never calling with_creeper_white_overlay differs from calling it with 0 by {off_vs_off} \
         px — the new code path is not a true no-op when absent"
    );

    // `progress = 1.0` (full swell, an "on" blink pulse) -> alpha 64, per
    // `creeper_overlay_alpha_from_progress`'s own transcribed formula.
    let alpha = creeper_overlay_alpha_from_progress(1.0);
    assert_eq!(alpha, 64, "sanity: this is the value entity_pipeline.rs itself derives");

    let on = render(&gpu, &mesh, Some(alpha));
    let on_repeat = render(&gpu, &mesh, Some(alpha));
    let determinism = differing(&on, &on_repeat);
    assert_eq!(
        determinism, 0,
        "two renders of the same active overlay differ by {determinism} px — the pipeline is \
         not deterministic, so the measurement below proves nothing"
    );

    let (x0, x1, y0, y1, area) = bbox(&control_a);

    // The two hypotheses, both computed from constants outside the shader:
    // vanilla's `alpha` is the weight on the entity's own colour
    // (`mix(white, colour, alpha)`); the swapped-argument bug (the hurt
    // overlay's own issue #371, in white-overlay form) would compute
    // `mix(colour, white, alpha)` instead, the complementary value on this
    // black-textured rig.
    let alpha_frac = f32::from(alpha) / 255.0;
    let predicted_correct = (srgb_to_linear(1.0 - alpha_frac) * 255.0).round() as i32;
    let predicted_swapped = (srgb_to_linear(alpha_frac) * 255.0).round() as i32;

    let mut samples: Vec<i32> = Vec::new();
    let mut outside_silhouette_changed = 0u32;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if is_mob(&control_a, i) {
                // R, G and B must agree (the black texture is channel-symmetric)
                // — record all three as independent samples.
                samples.push(i32::from(on[i]));
                samples.push(i32::from(on[i + 1]));
                samples.push(i32::from(on[i + 2]));
            } else if control_a[i..i + 3] != on[i..i + 3] {
                outside_silhouette_changed += 1;
            }
        }
    }
    assert!(!samples.is_empty(), "no silhouette samples collected");
    samples.sort_unstable();
    let measured = samples[samples.len() / 2];

    println!("=== CREEPER WHITE OVERLAY PIXEL GATE ===");
    println!("mob bbox: x[{x0}..{x1}] y[{y0}..{y1}], area {area} px");
    println!("alpha byte: {alpha} (progress 1.0)");
    println!("measured byte (median over {} samples): {measured}", samples.len());
    println!("predicted correct  (mix(white, colour, alpha)): {predicted_correct}");
    println!("predicted swapped  (mix(colour, white, alpha)): {predicted_swapped}");
    println!("background pixels changed by the overlay: {outside_silhouette_changed} (must be 0)");

    assert_eq!(
        outside_silhouette_changed, 0,
        "the overlay changed {outside_silhouette_changed} background pixels — this must be a \
         per-entity effect, never a full-screen one"
    );
    assert!(
        (measured - predicted_correct).abs() <= 2,
        "measured byte {measured} is not within rounding tolerance of the correct prediction \
         {predicted_correct} (mix(white, colour, alpha)) — bbox x[{x0}..{x1}] y[{y0}..{y1}]"
    );
    assert!(
        (measured - predicted_swapped).abs() > 8,
        "measured byte {measured} is suspiciously close to the SWAPPED-argument prediction \
         {predicted_swapped} (mix(colour, white, alpha)) — this is issue #371's exact bug shape, \
         reproduced for the white overlay"
    );
}
