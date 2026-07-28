//! Offscreen gate for **animated block sprites**: a sprite on a later animation
//! frame must draw *different pixels*, driven entirely by the existing
//! `anim.rs` timing and the per-slot V-offset the model shader now samples.
//!
//! Why this shape, and why the two controls are mandatory (the discipline is
//! borrowed from `break_particles_pixels.rs`):
//!
//! The scene draws two quads **in one pass**, so they share the same clear, the
//! same camera and the same frame — nothing but the sprite content can differ:
//!
//!   * LEFT half — an **animated** sprite (`anim = 1`). Its atlas strip is red on
//!     frame 0 (top 16 rows) and blue on frame 1 (bottom 16 rows), `frametime =
//!     2` ticks, non-interpolating. Frame 0's baked UV plus the slot's V offset
//!     is what the shader samples.
//!   * RIGHT half — a **static** green sprite (`anim = 0`), the same every tick.
//!
//! Subject: LEFT must go red → blue between tick 0 and tick 2 (different frames).
//!
//! Control 1 — **the static RIGHT half must not change.** Without it, "the pixels
//! differed" is satisfied by any per-frame jitter (a clear colour wobble, a
//! non-deterministic sampler). Green staying green proves the delta is localised
//! to the animated sprite, not the whole frame.
//!
//! Control 2 — **an intra-hold tick pair (0 vs 1) must be identical.** Both ticks
//! fall inside frame 0's 2-tick hold, so a correct animator holds frame 0. If
//! LEFT changed here, we'd be sampling garbage that moves every tick rather than
//! animating on the real frame schedule — indistinguishable from the subject
//! without this control.
//!
//! Negative control — **force the animated quad's `anim` byte to 0** and watch
//! the subject assertion fail: with no slot the shader takes one static sample,
//! LEFT stays red at tick 2, and red→blue never happens. Its observed failure is
//! printed; a gate whose failure mode has never been seen is not yet evidence.
//!
//! Region reporting is a LEFT/RIGHT split, never a whole-frame average: a total
//! count cannot tell localised animation from a global tint shift.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test animated_block_pixels -- --ignored --nocapture`.

use lodestone_render::{
    AnimFrame, AnimSlotUniform, CameraUniform, GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline,
    ModelVertex, SpriteAnimation, model_anim_buffer, model_camera_buffer, model_palette_buffer,
};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Normalised height of one physical frame in the 32×32 atlas (16 px of 32).
const FRAME_V: f32 = 0.5;

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
                label: Some("animated_block_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A 32×32 atlas: left 16 columns are the animated strip (rows 0–15 red = frame
/// 0, rows 16–31 blue = frame 1); right 16 columns are solid green (the static
/// sprite). Every texel is fully opaque so the cutout never discards.
fn atlas_rgba() -> Vec<u8> {
    let mut px = vec![0u8; (32 * 32 * 4) as usize];
    for y in 0..32u32 {
        for x in 0..32u32 {
            let i = ((y * 32 + x) * 4) as usize;
            let (r, g, b) = if x < 16 {
                if y < 16 {
                    (220, 0, 0) // frame 0: red
                } else {
                    (0, 0, 220) // frame 1: blue
                }
            } else {
                (0, 200, 0) // static: green
            };
            px[i] = r;
            px[i + 1] = g;
            px[i + 2] = b;
            px[i + 3] = 255;
        }
    }
    px
}

/// One clip-space quad spanning `x∈[x0,x1]`, `y∈[-1,1]`, with the given atlas UV
/// rect and slot byte `anim`. `tint = 255` (untinted), full-bright, CCW winding
/// (front faces out; the solid pipeline back-face culls, so winding matters).
fn quad(x0: f32, x1: f32, u0: f32, u1: f32, v0: f32, v1: f32, anim: u8) -> ModelMesh {
    let corner = |x: f32, y: f32, u: f32, w: f32| ModelVertex {
        position: [x, y, 0.5],
        uv: [u, w],
        ao: 1.0,
        light: 0xFF,
        tint: 255,
        anim,
        _pad: 0,
    };
    ModelMesh {
        vertices: vec![
            corner(x0, -1.0, u0, v1),
            corner(x1, -1.0, u1, v1),
            corner(x1, 1.0, u1, v0),
            corner(x0, 1.0, u0, v0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// Render the two-quad scene at `tick` and read back a LEFT (animated) and RIGHT
/// (static) centre sample as `((lr,lg,lb), (rr,rg,rb))`. When `force_static` is
/// set, the animated quad's slot byte is forced to `0` — the negative control.
fn render_scene(gpu: &Gpu, tick: u64, force_static: bool) -> ((u8, u8, u8), (u8, u8, u8)) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(device, queue, 32, 32, &atlas_rgba(), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    // Untinted palette (slot 255 is white); no quad here is tinted, but the model
    // pipeline still binds group 2.
    let palette = vec![[1.0_f32, 1.0, 1.0, 1.0]; 256];
    let palette_buffer = model_palette_buffer(device, &palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);

    // The animation slot table for this tick, driven by the real anim.rs timing:
    // slot 0 static, slot 1 the two-frame strip sampled at `tick`.
    let animation = SpriteAnimation {
        frames: vec![
            AnimFrame {
                region: 0,
                hold_ticks: 2,
            },
            AnimFrame {
                region: 1,
                hold_ticks: 2,
            },
        ],
        interpolate: false,
    };
    let slots = vec![
        AnimSlotUniform::static_slot(),
        AnimSlotUniform::from_sample(animation.sample(tick), FRAME_V),
    ];
    let anim_buffer = model_anim_buffer(device, &slots);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);

    let cam_buffer = model_camera_buffer(
        device,
        CameraUniform {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer);

    // LEFT half: animated sprite, uv over frame 0 (u∈[0,0.5], v∈[0,0.5]).
    let anim_slot = if force_static { 0 } else { 1 };
    let left = GpuModelMesh::upload(device, &quad(-1.0, 0.0, 0.0, 0.5, 0.0, 0.5, anim_slot))
        .expect("non-empty left quad");
    // RIGHT half: static green sprite (u∈[0.5,1.0], full v), anim 0.
    let right = GpuModelMesh::upload(device, &quad(0.0, 1.0, 0.5, 1.0, 0.0, 1.0, 0))
        .expect("non-empty right quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("anim target"),
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
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("anim gate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
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
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_bind_group(2, &palette_bg, &[]);
        pass.set_bind_group(3, &anim_bg, &[]);
        pass.set_vertex_buffer(0, left.vertices.slice(..));
        pass.set_index_buffer(left.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..left.index_count, 0, 0..1);
        pass.set_vertex_buffer(0, right.vertices.slice(..));
        pass.set_index_buffer(right.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..right.index_count, 0, 0..1);
    }

    let padded = (W * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
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
    queue.submit(std::iter::once(enc.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = readback.slice(..).get_mapped_range().expect("mapped range");

    let sample = |cx: u32, cy: u32| -> (u8, u8, u8) {
        let i = (cy * padded + cx * 4) as usize;
        (data[i], data[i + 1], data[i + 2])
    };
    // LEFT and RIGHT half centres — the region split.
    (sample(W / 4, H / 2), sample(3 * W / 4, H / 2))
}

/// L1 distance between two RGB samples.
fn diff((ar, ag, ab): (u8, u8, u8), (br, bg, bb): (u8, u8, u8)) -> u32 {
    let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).unsigned_abs();
    d(ar, br) + d(ag, bg) + d(ab, bb)
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn animated_sprite_changes_frame_while_static_sprite_holds() {
    let Some(gpu) = setup() else {
        panic!(
            "animated_block_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for a real GPU frame — a headless CI box has none and should not \
             run it."
        );
    };

    let (l0, r0) = render_scene(&gpu, 0, false);
    let (l1, r1) = render_scene(&gpu, 1, false);
    let (l2, r2) = render_scene(&gpu, 2, false);

    println!("LEFT  animated: tick0={l0:?} tick1={l1:?} tick2={l2:?}");
    println!("RIGHT static  : tick0={r0:?} tick1={r1:?} tick2={r2:?}");
    println!(
        "region deltas: LEFT 0->2={} (subject, must change)  RIGHT 0->2={} (control 1, must hold)  LEFT 0->1={} (control 2, intra-hold, must hold)",
        diff(l0, l2),
        diff(r0, r2),
        diff(l0, l1),
    );

    // Subject: the animated half advances from frame 0 (red) to frame 1 (blue).
    assert!(
        l2.2 > 150 && l2.0 < 80,
        "animated half at tick 2 must be blue (frame 1), got {l2:?}"
    );
    assert!(
        l0.0 > 150 && l0.2 < 80,
        "animated half at tick 0 must be red (frame 0), got {l0:?}"
    );
    assert!(
        diff(l0, l2) > 200,
        "animated half must visibly change frame between tick 0 and tick 2 (got delta {})",
        diff(l0, l2)
    );

    // Control 1: the static half is identical across every tick (localisation).
    assert_eq!(r0, r2, "static half must not change with the animation tick");
    assert_eq!(r0, r1, "static half must not change with the animation tick");
    assert!(
        diff(r0, r2) == 0,
        "static-half delta must be exactly zero, got {}",
        diff(r0, r2)
    );

    // Control 2: within frame 0's 2-tick hold, tick 0 and tick 1 are identical.
    assert_eq!(
        l0, l1,
        "animated half must hold frame 0 across ticks 0 and 1 (same 2-tick frame)"
    );

    // Negative control: force the slot to 0 and confirm the subject stops being
    // true — LEFT no longer reaches blue at tick 2. Observed failure is printed.
    let (nl0, _) = render_scene(&gpu, 0, true);
    let (nl2, _) = render_scene(&gpu, 2, true);
    println!("NEGATIVE CONTROL (forced anim=0): LEFT tick0={nl0:?} tick2={nl2:?}  delta={}", diff(nl0, nl2));
    assert_eq!(
        nl0, nl2,
        "with the animation slot forced off, the sprite must stay frozen on frame 0 — \
         this is the bug the gate exists to catch"
    );
}
