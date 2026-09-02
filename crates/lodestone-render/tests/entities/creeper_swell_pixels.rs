//! Prove a creeper's pre-detonation **swell** reaches pixels.
//!
//! [`entity_anim_pixels`](./entity_anim_pixels.rs) proves that *rotating* a joint
//! moves the drawn silhouette. The swell is a different kind of animation —
//! vanilla's `CreeperRenderer.scale` grows the whole model rather than bending
//! it — and it travels a different code path here
//! ([`Skeleton::pose_swelling`]'s root transform, not `setup_anim`), so it needs
//! its own gate. A leg-swing gate passes unchanged on a build where the creeper
//! never inflates at all, which is exactly the reported defect.
//!
//! # What is measured, and why area rather than "pixels differ"
//!
//! A differing-pixel count answers "did anything change", which a mob that
//! merely *shifted* also satisfies — and a mis-composed scale (about the model
//! origin instead of the ground plane) does precisely that: it slides the
//! creeper down through the floor while barely changing its size. The swell's
//! defining property is that the creeper gets **bigger**, so the reading is
//! silhouette **area**, signed and directional.
//!
//! # The predictions, stated before the run
//!
//! * [`UNFIXED_AREA_RATIO`] — what the build *before* this change produces:
//!   `pose_swelling` ignoring its argument leaves the two frames identical, so
//!   the ratio is exactly `1.0`. The gate band must exclude it.
//! * [`PREDICTED_AREA_RATIO`] — what the ported formula predicts: at
//!   [`MAX_SWELL`] the horizontal factor is ~1.415 and the vertical ~1.11, so a
//!   broadside silhouette should grow by roughly their product.
//! * [`MIN_AREA_RATIO`] / [`MAX_AREA_RATIO`] bracket it. The **upper** bound is
//!   not decoration: a scale applied twice (once in the root transform, once by
//!   a caller) lands near 2.2 and would sail through a lower bound alone.
//!
//! # Controls
//!
//! 1. **Same input, twice, at exactly `swell == 0.0`** must produce *zero*
//!    differing pixels. Without it, "the areas differ" is also satisfied by a
//!    non-deterministic pipeline or an uninitialised buffer.
//! 2. **Neither silhouette touches the frame border.** A clipped mob has an area
//!    set by the viewport, not by the animation, and a swelling one is the mob
//!    most likely to run out of frame.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is an explicit opt-in; once opted in, a missing
//! adapter is a **failure**, never a skip.

use glam::{Mat4, Vec3};
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityInstance, EntityModelSet, plan_entities};
use lodestone_render::entity_anim::{AnimInput, MAX_SWELL};
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};

const W: u32 = 256;
const H: u32 = 256;

/// sRGB, matching the real swapchain — see `entity_anim_pixels` for why a plain
/// `Unorm` target quietly darkens the mob past a fixed brightness threshold.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Turn the creeper broadside, so the ~1.4× horizontal growth is across the
/// screen rather than into the depth buffer.
const BODY_YAW: f32 = 90.0;

/// The silhouette-area ratio a build that ignores the swell produces: the two
/// frames are byte-identical, so it is exactly one. The gate band below must
/// exclude this value, or the gate is vacuous.
const UNFIXED_AREA_RATIO: f32 = 1.0;

/// What the ported `CreeperRenderer.scale` predicts under a *flat* projection:
/// horizontal ~1.415 and vertical ~1.11 at [`MAX_SWELL`], so a broadside
/// silhouette grows with roughly their product.
///
/// The measured ratio runs ~8% above this (1.70 on the reference run) and that
/// is expected, not slop: the horizontal factor applies to **Z** as well, which
/// walks the creeper's near face ~0.16 blocks toward a camera 3 blocks away, and
/// perspective magnifies it. A gate band tight enough to exclude that would be
/// measuring the camera distance.
const PREDICTED_AREA_RATIO: f32 = 1.415 * 1.11;

/// Lower gate bound. Sits well above [`UNFIXED_AREA_RATIO`] and well below
/// [`PREDICTED_AREA_RATIO`], so neither a no-op nor ordinary rasterisation
/// jitter can decide the result.
const MIN_AREA_RATIO: f32 = 1.25;

/// Upper gate bound: catches a swell applied more than once, or an absolute
/// scale used where a relative one belongs.
const MAX_AREA_RATIO: f32 = 2.0;

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
                label: Some("creeper_swell_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// Flat opaque magenta: one colour over the whole sheet, so a differing pixel
/// can only mean the silhouette moved, never that a texture seam shifted.
fn test_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const TW: u32 = 64;
    const TH: u32 = 64;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("swell-sheet"),
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
        label: Some("swell-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// Far enough back that a creeper inflated to ~1.8 blocks still leaves clear sky
/// on every side — the border check below enforces that this stays true rather
/// than trusting the arithmetic.
fn framing_camera() -> Camera {
    Camera {
        position: Vec3::new(0.0, 0.85, -3.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// Render one creeper at `swell` and return the RGBA frame, row-major, tightly
/// packed.
///
/// The instance is assembled by hand rather than through
/// [`EntityModelSet::resolve`] because that path calls `Skeleton::pose`, which
/// has no swell parameter — threading one through it is the wiring this change
/// was scoped out of. Note also that the instance keeps the AABB
/// `EntityInstance::new` computed from the **rest** bounds: a swelling creeper
/// is drawn larger than its own culling box, which is why this gate centres it
/// in frame instead of letting the culler decide.
fn render_creeper(gpu: &Gpu, models: &EntityModelSet, swell: f32) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = framing_camera();
    let feet = Vec3::ZERO;

    let mesh = models.get("creeper").expect("creeper has a baked model");
    let mut creeper = EntityInstance::new("creeper", mesh, feet, BODY_YAW, 1.0, &AnimInput::REST);
    let placement = creeper.transform;
    creeper.part_transforms = mesh
        .skeleton
        .pose_swelling(&AnimInput::REST, swell)
        .into_iter()
        .map(|part| placement * part)
        .collect::<Vec<Mat4>>();

    let instances = [creeper];
    let frame = plan_entities(&instances, &camera.frustum());
    assert_eq!(
        frame.instance_count(),
        1,
        "the creeper was culled — this gate measures its silhouette, so it must be on screen"
    );

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = test_texture(device, queue);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);

    let gpu_creeper = GpuEntityModel::upload(device, mesh).expect("creeper mesh is non-empty");

    // One instance buffer per part: vertices are part-local, so each part is
    // drawn against its own matrices.
    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_creeper.parts.iter().zip(&batch.parts) {
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
        label: Some("swell-color"),
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
            label: Some("swell-pass"),
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
            pass.set_vertex_buffer(0, gpu_creeper.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_creeper.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("swell-readback"),
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

/// Is this pixel part of the mob? Green separates the magenta mob (~20) from the
/// blue sky clear (~200) independently of how dark a face's shade is — the same
/// discriminator `entity_anim_pixels` uses.
fn is_mob(frame: &[u8], i: usize) -> bool {
    frame[i + 1] < 120 && frame[i] > 40
}

/// Silhouette area in pixels, plus the bounding rows/columns it occupies.
fn silhouette(frame: &[u8]) -> (u32, u32, u32, u32, u32) {
    let (mut top, mut bottom) = (H, 0u32);
    let (mut left, mut right) = (W, 0u32);
    let mut area = 0u32;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if is_mob(frame, i) {
                top = top.min(y);
                bottom = bottom.max(y);
                left = left.min(x);
                right = right.max(x);
                area += 1;
            }
        }
    }
    assert!(area > 0, "no creeper silhouette found at all");
    (area, top, bottom, left, right)
}

/// Count pixels differing between two whole frames.
fn differing(a: &[u8], b: &[u8]) -> u32 {
    let mut n = 0;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if a[i..i + 3] != b[i..i + 3] {
                n += 1;
            }
        }
    }
    n
}

/// Fails if the silhouette reaches the viewport border, where its area would be
/// set by the frame rather than by the animation.
fn assert_unclipped(label: &str, top: u32, bottom: u32, left: u32, right: u32) {
    assert!(
        top > 0 && bottom < H - 1 && left > 0 && right < W - 1,
        "{label}: silhouette spans rows {top}..={bottom} and columns {left}..={right} of a \
         {W}×{H} frame — it is clipped, so its area measures the viewport, not the swell"
    );
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the creeper swell reaches pixels"]
fn a_swelling_creeper_grows_on_screen_and_a_calm_one_does_not() {
    let Some(gpu) = setup() else {
        panic!(
            "creeper_swell_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let models = EntityModelSet::load();

    let calm = render_creeper(&gpu, &models, 0.0);
    let primed = render_creeper(&gpu, &models, MAX_SWELL);
    // The control: the identical render path, same input on both sides.
    let calm_again = render_creeper(&gpu, &models, 0.0);

    let (calm_area, c_top, c_bottom, c_left, c_right) = silhouette(&calm);
    let (primed_area, p_top, p_bottom, p_left, p_right) = silhouette(&primed);
    let (control_area, ..) = silhouette(&calm_again);
    let control_diff = differing(&calm, &calm_again);
    let ratio = primed_area as f32 / calm_area as f32;

    println!("=== CREEPER SWELL PIXEL GATE ===");
    println!("swell 0.0   : {calm_area} px, rows {c_top}..={c_bottom}, cols {c_left}..={c_right}");
    println!(
        "swell {MAX_SWELL:.4}: {primed_area} px, rows {p_top}..={p_bottom}, cols \
         {p_left}..={p_right}"
    );
    println!("control     : {control_area} px, {control_diff} px differ (must be 0)");
    println!("area ratio  : {ratio:.4}");
    println!(
        "predictions : unfixed {UNFIXED_AREA_RATIO:.2}, ported ~{PREDICTED_AREA_RATIO:.2}, band \
         {MIN_AREA_RATIO:.2}..{MAX_AREA_RATIO:.2}"
    );

    assert_eq!(
        control_diff, 0,
        "two renders of the *same* unlit creeper differ by {control_diff} px — the pipeline is \
         not deterministic, so the growth below proves nothing about the swell"
    );
    assert_eq!(
        control_area, calm_area,
        "the control silhouette changed size on its own"
    );
    assert_unclipped("swell 0.0", c_top, c_bottom, c_left, c_right);
    assert_unclipped("full swell", p_top, p_bottom, p_left, p_right);
    assert!(
        ratio > MIN_AREA_RATIO && ratio < MAX_AREA_RATIO,
        "a fully-primed creeper covered {ratio:.4}× the pixels of a calm one, outside the \
         {MIN_AREA_RATIO}..{MAX_AREA_RATIO} band. A build that ignores the swell reads exactly \
         {UNFIXED_AREA_RATIO}; the ported formula predicts ~{PREDICTED_AREA_RATIO:.2}"
    );
    // Directional: it grows *upward* out of the ground, it does not slide.
    assert!(
        p_top < c_top,
        "the primed creeper's crown sat at row {p_top} against the calm one's {c_top} — a swell \
         that does not raise the head is scaling about the wrong origin"
    );
    // A few pixels of drift here are perspective, not a moving sole: the same
    // ~1.415 factor applies to Z, bringing the near foot ~0.16 blocks closer to
    // a camera 3 blocks away, which lowers it ~3 px. Anchoring the scale at the
    // model origin instead of the ground plane moves the soles ~0.16 *blocks*,
    // an order of magnitude more, which is what this bound is sized to catch.
    const FEET_DRIFT_PX: u32 = 8;
    assert!(
        p_bottom.abs_diff(c_bottom) < FEET_DRIFT_PX,
        "the primed creeper's feet moved from row {c_bottom} to {p_bottom}, more than the \
         {FEET_DRIFT_PX} px perspective allowance — vanilla scales before the ground lift, so \
         the soles stay on the floor"
    );
}
