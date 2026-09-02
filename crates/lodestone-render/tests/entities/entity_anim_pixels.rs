//! Prove entity **animation** reaches pixels — not just entity geometry.
//!
//! [`entity_gate`](./entity_gate.rs) proves a mob is drawn at all. That gate
//! passes unchanged if every limb is frozen in its rest pose, because a frozen
//! mob is still a mob-shaped silhouette. This gate asserts the thing the
//! animator exists for: that changing [`AnimInput`] changes what the GPU
//! produces, in the region of the frame the moving part occupies.
//!
//! # Why this gate has to exist
//!
//! `entity_anim`'s unit tests prove the matrices swing. They cannot prove the
//! matrices are *uploaded*, that the per-part draw ranges line up with the
//! per-part instance buffers, or that the part-local vertices are still in the
//! right place. Every one of those is a silent failure: a mis-paired range and
//! buffer draws a mob whose leg is welded to its body, and the mesh tests stay
//! green. This project has found the same shape seven times — a verified
//! subsystem nothing draws — so the animator is not finished until pixels move.
//!
//! # The three assertions, and their controls
//!
//! 1. **Legs move.** Two frames a half-cycle apart differ in the leg band by a
//!    real number of pixels, not one or two anti-aliasing texels.
//! 2. **A standing mob does not.** The identical two-frame comparison with
//!    `AnimInput::REST` on both sides must produce *zero* differing pixels.
//!    Without this control, "the frames differ" is also satisfied by a
//!    non-deterministic renderer, an uninitialised buffer, or a camera that
//!    moved — none of which have anything to do with animation.
//! 3. **The body is not being flung around.** The head/body band differs far
//!    less than the leg band. A transform bug that displaced the *whole* mob
//!    would sail through assertion 1; only a per-region reading separates
//!    "the legs swung" from "the mob teleported".
//!
//! Reporting per region rather than as a frame total is deliberate: a single
//! whole-frame difference count merges two populations and describes neither.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is an explicit opt-in; once opted in, a missing
//! adapter is a **failure**, never a skip.

use glam::Vec3;
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityModelSet, plan_entities};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};

const W: u32 = 256;
const H: u32 = 256;
/// **sRGB, matching the real swapchain (`Bgra8UnormSrgb`) — and so must the
/// sheet below.** The entity shader multiplies its shade into the texel in
/// *gamma* space (as vanilla does, and as the model shader already did), so what
/// a face's shade does to the final byte is only correct when the sampled texel
/// is linear-light and the target re-encodes on write. Measured on a plain
/// `Unorm` target the same mob renders far darker than the player ever sees it,
/// and a fixed brightness threshold calibrated there quietly stops finding the
/// mob at all.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Turn the mob broadside to the camera. Limbs swing about the model's **X**
/// axis, so side-on the swing is horizontal screen motion; head-on it would be
/// almost pure depth and the silhouette would barely move — the gate would then
/// be measuring the camera angle rather than the animator.
const BODY_YAW: f32 = 90.0;

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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("entity_anim_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// Flat opaque magenta: every mob texel is one colour, so a differing pixel can
/// only mean the silhouette moved, never that a texture seam shifted.
fn test_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const TW: u32 = 64;
    const TH: u32 = 64;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("anim-sheet"),
        size: wgpu::Extent3d {
            width: TW,
            height: TH,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
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
        label: Some("anim-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// The same head-on camera as [`entity_gate`](./entity_gate.rs), which is known
/// to frame a pig well. The *mob* is turned side-on instead (see `BODY_YAW`), so
/// nothing here depends on re-deriving the yaw convention.
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

/// Render one pig at `anim` and return the RGBA frame, row-major, tightly packed.
fn render_pig(gpu: &Gpu, models: &EntityModelSet, anim: &AnimInput) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = side_camera();

    let pig = models
        .resolve("pig", Vec3::new(0.0, 0.0, 0.0), BODY_YAW, 1.0, anim)
        .expect("pig has a baked model");
    let instances = [pig];
    let frame = plan_entities(&instances, &camera.frustum());
    assert_eq!(
        frame.instance_count(),
        1,
        "the pig was culled — this gate measures animation, so it must be on screen"
    );

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = test_texture(device, queue);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);

    let mesh = models.get("pig").expect("pig mesh");
    let gpu_pig = GpuEntityModel::upload(device, mesh).expect("pig mesh is non-empty");

    // One instance buffer per part: vertices are part-local, so each part is
    // drawn against its own matrices. Uploading `batch.transforms` and drawing
    // the whole index range would collapse every part onto the model origin.
    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_pig.parts.iter().zip(&batch.parts) {
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
    assert!(
        !per_part.is_empty(),
        "no part produced an instance buffer — nothing would be drawn"
    );

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("anim-color"),
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
            label: Some("anim-pass"),
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
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
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
            pass.set_vertex_buffer(0, gpu_pig.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_pig.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("anim-readback"),
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

/// Is this pixel part of the mob?
///
/// The sheet is flat magenta but the shader shades it, so the rendered mob spans
/// a range of darkened magentas — matching the raw texel value only would find a
/// sparse scattering of the brightest faces and nothing else. Same threshold as
/// [`entity_gate`](./entity_gate.rs).
fn is_mob(frame: &[u8], i: usize) -> bool {
    // Green separates the magenta mob (~20) from the blue sky clear (~200) and
    // is independent of how dark a face's shade is; see `entity_gate`'s note on
    // why a red-brightness floor is the wrong discriminator here.
    frame[i + 1] < 120 && frame[i] > 40
}

/// Count pixels that differ between two frames within a horizontal band of rows.
fn differing_in_band(a: &[u8], b: &[u8], row_lo: u32, row_hi: u32) -> u32 {
    let mut n = 0;
    for y in row_lo..row_hi {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if a[i..i + 3] != b[i..i + 3] {
                n += 1;
            }
        }
    }
    n
}

/// Rows the pig's legs occupy, and rows its head/body occupy, for this camera.
/// Derived from the rendered silhouette's own extent rather than hardcoded, so a
/// change in framing moves the bands with the mob instead of silently emptying
/// them.
fn bands(frame: &[u8]) -> (u32, u32, u32, u32) {
    let mut top = H;
    let mut bottom = 0;
    let mut area = 0u32;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if is_mob(frame, i) {
                top = top.min(y);
                bottom = bottom.max(y);
                area += 1;
            }
        }
    }
    assert!(
        bottom > top,
        "no mob silhouette found — the gate cannot locate the legs to look at"
    );
    assert!(
        area > 4000,
        "the mob covers only {area} px, so the bands below are slivers and a \"the legs moved\" \
         reading would come from a handful of edge texels. A too-tight pixel classifier does this: \
         the shader shades the sheet, so the mob is *darkened* magenta, not the raw texel value"
    );
    let height = bottom - top;
    // The lower ~40% of a pig is legs; the upper ~45% is body and head.
    let leg_lo = top + (height * 6) / 10;
    let body_hi = top + (height * 45) / 100;
    (leg_lo, bottom + 1, top, body_hi)
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove entity animation reaches pixels"]
fn walking_legs_change_pixels_and_standing_legs_do_not() {
    let Some(gpu) = setup() else {
        panic!(
            "entity_anim_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let models = EntityModelSet::load();

    // Two points a half cycle apart, at full amplitude: legs at opposite extremes.
    let walk_a = AnimInput {
        limb_swing: 0.0,
        limb_swing_amount: 1.0,
        ..AnimInput::REST
    };
    let walk_b = AnimInput {
        limb_swing: std::f32::consts::PI / 0.6662, // half a cycle of vanilla's 0.6662 rate
        limb_swing_amount: 1.0,
        ..AnimInput::REST
    };

    let frame_a = render_pig(&gpu, &models, &walk_a);
    let frame_b = render_pig(&gpu, &models, &walk_b);

    // The control: the *same* two-render path with no animation on either side.
    // If this differs, the difference above is the renderer, not the animator.
    let rest_1 = render_pig(&gpu, &models, &AnimInput::REST);
    let rest_2 = render_pig(&gpu, &models, &AnimInput::REST);

    let (leg_lo, leg_hi, body_lo, body_hi) = bands(&frame_a);
    let walk_legs = differing_in_band(&frame_a, &frame_b, leg_lo, leg_hi);
    let walk_body = differing_in_band(&frame_a, &frame_b, body_lo, body_hi);
    let rest_legs = differing_in_band(&rest_1, &rest_2, leg_lo, leg_hi);

    println!("=== ENTITY ANIMATION PIXEL GATE ===");
    println!("leg band rows      : {leg_lo}..{leg_hi}");
    println!("body band rows     : {body_lo}..{body_hi}");
    println!("walking, leg band  : {walk_legs} px differ");
    println!("walking, body band : {walk_body} px differ");
    println!("control (rest×2)   : {rest_legs} px differ  (must be 0)");

    assert_eq!(
        rest_legs, 0,
        "two renders of a *standing* pig differ in the leg band by {rest_legs} px — the pipeline \
         is not deterministic, so the walking difference below proves nothing about animation"
    );
    assert!(
        walk_legs >= 200,
        "a full-amplitude walk cycle moved only {walk_legs} leg pixels. The animator's own unit \
         tests pass on matrices; this means the matrices are not reaching the draw — check that \
         each part's instance buffer is paired with that part's index range"
    );
    assert!(
        walk_body * 2 < walk_legs,
        "the body band changed by {walk_body} px against the legs' {walk_legs} — the whole mob is \
         moving, not its limbs, which is a transform bug that assertion 1 alone would pass"
    );
}
