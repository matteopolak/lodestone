//! Phase-5 entity gate: prove a *mob* reaches actual pixels.
//!
//! The block gate ([`live_gate`](../live_gate.rs)) proves a chunk becomes
//! terrain pixels; this is its entity equivalent. It bakes a real vanilla model
//! (a pig) through the version-free assets seam, uploads it through the instanced
//! [`EntityPipeline`], draws it to an offscreen target, reads the pixels back,
//! and asserts three anti-vacuity properties that a "the mesh built" test can't:
//!
//! 1. **Non-blank** — the mob covers a real, bounded fraction of the frame
//!    (neither 0% nor the whole screen).
//! 2. **Localized** — the silhouette is where the entity is (image centre) and
//!    *not* where it isn't (the four corners stay background). A mob that filled
//!    the frame, or one that leaked into the corners, fails.
//! 3. **Culling is real** — a second entity placed behind the camera is culled,
//!    and [`EntityCullStats::is_meaningful`] holds (drew something *and* culled
//!    something), so "fast because it culled everything" can't pass.
//!
//! # Why a synthetic entity, not a live one
//!
//! This gate renders a pig at a **known, fixed** position rather than whatever
//! mobs happen to spawn near a live server's origin. Live mob spawns are
//! nondeterministic in count, type and position, which would make the
//! coverage/localization asserts flaky — the opposite of a gate. A synthetic pig
//! is reproducible frame to frame, so a regression in the placement transform,
//! the instance buffer, or the pipeline shows up as a deterministic pixel
//! change. The type→model→pixels path exercised here is identical to the live
//! one; only the entity's provenance differs.
//!
//! # Fail closed
//!
//! The test is `#[ignore]`d, so running it is an explicit opt-in. Once opted in,
//! a missing GPU adapter is a **failure**, not a skip — a silent pass here would
//! be exactly the vacuous gate this project keeps rediscovering. On a machine
//! with no adapter, don't run it.

use glam::Vec3;
use lodestone_render::GpuCapabilities;
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityModelSet, plan_entities};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};

const W: u32 = 256;
const H: u32 = 256;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A sky-blue clear colour, distinct from any pig texel, so "background" is
/// unambiguous in the readback.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
    a: 1.0,
};

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
        // Probe so a future capability-gated entity path has the same seam the
        // block path uses; unused here beyond confirming the adapter is real.
        let _caps = GpuCapabilities::probe(&adapter);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("entity_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// Build a solid magenta test sheet (opaque). The pig UVs address into it, so
/// every rendered texel is magenta — a colour that appears nowhere in the sky
/// clear, making the silhouette trivially separable from background. Real
/// mob-skin loading is the application's job, exactly as the block atlas is.
fn test_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const TW: u32 = 64;
    const TH: u32 = 64;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-gate-sheet"),
        size: wgpu::Extent3d {
            width: TW,
            height: TH,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pixels: Vec<u8> = (0..TW * TH).flat_map(|_| [230u8, 30, 200, 255]).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(TW * 4),
            rows_per_image: Some(TH),
        },
        wgpu::Extent3d {
            width: TW,
            height: TH,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("entity-gate-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove a mob reaches pixels"]
fn entity_gate_pig_to_pixels() {
    let Some(gpu) = setup() else {
        panic!(
            "entity_gate: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for the full GPU path — run it on a machine with an adapter, or don't opt in."
        );
    };
    let device = &gpu.device;
    let queue = &gpu.queue;

    // --- stated configuration ---------------------------------------------
    // A pig standing at the origin, facing the camera; the camera looks at it
    // from a few blocks away along -Z, eye level with the pig's body.
    let pig_feet = Vec3::new(0.0, 0.0, 0.0);
    let camera = Camera {
        position: Vec3::new(0.0, 0.9, -3.0),
        yaw: 0.0, // faces +Z, toward the pig
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    };

    let models = EntityModelSet::load();
    let pig = models
        .resolve("pig", pig_feet, 0.0, 1.0, &AnimInput::REST)
        .expect("pig has a baked model");

    // A second pig placed behind the camera (along -Z, further than the eye) so
    // frustum culling has something real to remove — the anti-vacuity guard.
    let behind = models
        .resolve(
            "pig",
            Vec3::new(0.0, 0.0, -12.0),
            0.0,
            1.0,
            &AnimInput::REST,
        )
        .expect("pig has a baked model");

    let instances = [pig, behind];
    let frame = plan_entities(&instances, &camera.frustum());
    assert!(
        frame.stats.is_meaningful(),
        "gate is vacuous unless it both drew and culled: {:?}",
        frame.stats
    );

    // --- GPU resources ----------------------------------------------------
    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = test_texture(device, queue);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);

    // Upload the one model type we draw and its surviving instance transforms.
    let pig_mesh = models.get("pig").expect("pig mesh");
    let gpu_pig = GpuEntityModel::upload(device, pig_mesh).expect("pig mesh is non-empty");

    // Pre-build each batch's instance buffer so they outlive the render pass.
    // One buffer per part: the mesh's vertices are part-local, so each part is
    // drawn over its own matrices.
    let mut instance_buffers: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        if batch.model != "pig" {
            continue;
        }
        for (range, mats) in gpu_pig.parts.iter().zip(&batch.parts) {
            if range.index_count == 0 {
                continue;
            }
            if let Some(buf) = upload_instances(device, mats) {
                instance_buffers.push((
                    mats.len() as u32,
                    range.index_start..range.index_start + range.index_count,
                    buf,
                ));
            }
        }
    }

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-gate-color"),
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

    let drawn_instances: u32;
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("entity-gate-pass"),
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

        for (count, range, inst_buf) in &instance_buffers {
            pass.set_vertex_buffer(0, gpu_pig.vertices.slice(..));
            pass.set_vertex_buffer(1, inst_buf.slice(..));
            pass.set_index_buffer(gpu_pig.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }
        drawn_instances = frame.instance_count() as u32;
    }

    // --- read back --------------------------------------------------------
    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("entity-gate-readback"),
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
    queue.submit(std::iter::once(encoder.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = readback.slice(..).get_mapped_range().expect("mapped range");

    // A texel is "mob" if it is clearly not the sky clear (blue-dominant). The
    // pig sheet is magenta (high R, high B, low G), the sky is (102,153,242).
    let is_mob = |r: u8, g: u8, _b: u8| -> bool { r > 150 && g < 120 };

    let mut mob_px = 0u32;
    let mut corner_mob = 0u32;
    let mut center_mob = 0u32;
    let margin = W / 8; // corner boxes are margin×margin
    let cx0 = W / 2 - W / 8;
    let cx1 = W / 2 + W / 8;
    let cy0 = H / 2 - H / 8;
    let cy1 = H / 2 + H / 8;
    for y in 0..H {
        let row = (y * padded) as usize;
        for x in 0..W {
            let i = row + (x * 4) as usize;
            let (r, g, b) = (data[i], data[i + 1], data[i + 2]);
            if is_mob(r, g, b) {
                mob_px += 1;
                let in_corner = (x < margin || x >= W - margin) && (y < margin || y >= H - margin);
                if in_corner {
                    corner_mob += 1;
                }
                if x >= cx0 && x < cx1 && y >= cy0 && y < cy1 {
                    center_mob += 1;
                }
            }
        }
    }
    drop(data);
    readback.unmap();

    let total = f64::from(W * H);
    let coverage = 100.0 * f64::from(mob_px) / total;
    let center_area = f64::from((cx1 - cx0) * (cy1 - cy0));
    let center_fill = 100.0 * f64::from(center_mob) / center_area;
    println!("=== PHASE-5 ENTITY GATE: pig → pixels ===");
    println!("target:             {W}x{H}");
    println!(
        "instances:          {} placed, {drawn_instances} drawn (1 culled behind camera)",
        instances.len(),
    );
    println!("mob coverage:       {coverage:.1}% of frame");
    println!("center fill:        {center_fill:.1}% of the central box");
    println!("corner mob pixels:  {corner_mob} (must be 0)");

    // 1. Non-blank, but not the whole screen: a real, bounded silhouette.
    assert!(
        mob_px > (W * H) / 200,
        "the pig covers too little to be real: {mob_px} px ({coverage:.2}%)"
    );
    assert!(
        coverage < 85.0,
        "the pig filled the frame — that isn't a mob silhouette, it's a bug: {coverage:.1}%"
    );
    // 2. Localized: present at the centre, absent from every corner.
    assert!(
        center_mob > 0,
        "no mob pixels where the entity is (image centre) — geometry is misplaced"
    );
    assert!(
        corner_mob == 0,
        "mob pixels leaked into the corners where no entity is: {corner_mob} px"
    );
    // 3. Culling was real, and exactly one instance survived.
    assert_eq!(
        drawn_instances, 1,
        "exactly the front pig should have drawn; the one behind the camera must be culled"
    );

    println!("=== PHASE-5 ENTITY GATE PASSED: a mob reached pixels ===");
}
