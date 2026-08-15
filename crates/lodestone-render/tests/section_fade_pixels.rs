//! Offscreen gate for the per-section fade-in: a freshly built section must
//! render as the fog colour, not pop in solid, and the middle of the fade
//! must land on the arithmetic midpoint between the fog colour and the fully
//! materialised colour — not merely "somewhere between" them.
//!
//! # Why a mid-fade sample, not just the two ends
//!
//! A gate that only samples `t=0` and `t=end` cannot tell a real fade from a
//! delayed pop (visibility snapped straight from 0 to 1 partway through the
//! window would still pass an ends-only check). Sampling the exact middle and
//! asserting it lands on the arithmetic mean of the two ends is the
//! discriminating assertion — see `DESIGN.md`'s note on the mid-alpha anchor
//! for the banner-pattern gate, same shape.
//!
//! # Why this can be an exact byte prediction, not a bracket
//!
//! Unlike this codebase's `ALPHA_BLENDING` gates, the fade is **not** an
//! alpha blend — `model.wgsl`'s own comment on `materialised_srgb` is explicit
//! that only `rgb` moves and the pipeline's blend state is untouched. It is a
//! plain shader-side `mix()` between two colours the CPU already knows (the
//! configured fog colour, and the fully-materialised colour measured at
//! `visibility = 1.0` in the same test), so the midpoint is the arithmetic
//! mean of two measured bytes, computable exactly rather than bracketed.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test section_fade_pixels -- --ignored --nocapture`.

use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex, SECTION_FADE_DURATION_SECS,
    SectionOriginUniform,
    fog::{FogSettings, FogUniform},
    model_anim_buffer, model_palette_buffer, model_shared_camera_buffer_with_fog,
};
use wgpu::util::DeviceExt;

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The fog colour this gate mixes toward at `visibility = 0`: pure red,
/// chosen distinct from the white quad so a transposition or a no-op mix
/// cannot pass by coincidence (this file's fixture-distinctness rule).
const FOG_RED: [f32; 3] = [1.0, 0.0, 0.0];

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
                label: Some("section_fade_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A full-frame opaque quad in clip space (identity camera), untinted and
/// full-bright, exactly `fog_gate.rs`'s own fixture — the lit colour it
/// resolves to is not asserted against a predicted constant (that is the
/// whole lighting pipeline), it is *measured* once at `visibility = 1.0` and
/// used as this test's own outside anchor for the midpoint prediction.
fn white_quad() -> ModelMesh {
    let v = |x: f32, y: f32, u: f32, w: f32| ModelVertex {
        position: [x, y, 0.0],
        uv: [u, w],
        ao: 1.0,
        light: 0xFF,
        tint: 255,
        anim: 0,
        cutout_bypass: 0,
        tint_rgb_override: [0, 0, 0, 0],
    };
    ModelMesh {
        vertices: vec![
            v(-1.0, -1.0, 0.0, 1.0),
            v(1.0, -1.0, 1.0, 1.0),
            v(1.0, 1.0, 1.0, 0.0),
            v(-1.0, 1.0, 0.0, 0.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// Render the white quad with distance fog **disabled** (a degenerate range,
/// so `fog_amount` is unconditionally 0 and the only colour movement left is
/// the section fade) and a section origin whose `build_time` puts it
/// `elapsed` seconds into its fade. Returns the centre pixel.
fn render_center(gpu: &Gpu, fog_color: [f32; 3], now_secs: f32, build_time_secs: f32) -> (u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[255, 255, 255, 255].repeat(16), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let settings = FogSettings {
        color: fog_color,
        sky_color: fog_color,
        // Degenerate range: `FogUniform::new` reads this as disabled, so the
        // distance-fog term (`fog_amount`) is unconditionally 0 and only the
        // section fade can move the fragment's colour.
        start: 0.0,
        end: 0.0,
        environmental_start: 0.0,
        environmental_end: 0.0,
    };
    let mut fog = FogUniform::new(&settings, [0.0, 0.0, -1000.0]);
    // The section fade's clock — see `model.wgsl`'s `Camera.fog_ambient_light.w`.
    fog.ambient_light[3] = now_secs;
    let cam_buffer = model_shared_camera_buffer_with_fog(device, glam::Mat4::IDENTITY.to_cols_array_2d(), fog);

    // A custom origin buffer carrying this section's fade `build_time` —
    // `section_origin_buffer` always defaults to the never-fades sentinel
    // (correctly, for its one-off callers), so this gate builds its own.
    let origin_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("section_fade_pixels origin"),
        contents: bytemuck::bytes_of(&SectionOriginUniform::with_build_time([0.0, 0.0, 0.0], build_time_secs)),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);
    let palette_buffer = model_palette_buffer(device, &[[1.0, 1.0, 1.0, 1.0]; 256]);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);

    let mesh = GpuModelMesh::upload(device, &white_quad()).expect("non-empty quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("section fade target"),
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
            label: Some("section fade gate"),
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
        pass.set_bind_group(0, &cam_bg, &[0]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_bind_group(2, &palette_bg, &[]);
        pass.set_bind_group(3, &anim_bg, &[]);
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.index_count, 0, 0..1);
    }

    let padded = (W * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
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

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = slice.get_mapped_range().expect("mapped range");
    let row = (H / 2) as usize;
    let col = (W / 2) as usize;
    let i = row * padded as usize + col * 4;
    (data[i], data[i + 1], data[i + 2])
}

#[test]
#[ignore = "requires a GPU adapter"]
fn fresh_section_starts_at_the_fog_colour_and_mid_fade_lands_on_the_midpoint() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter for section_fade_pixels");
    };

    let now = 1.0_f32;

    // t=0: just built. Distance fog is disabled, so this must read as the
    // fog colour (red) with no lighting attenuation — the fade mix target is
    // `camera.fog_color_start.rgb` directly, unmultiplied by shade.
    let start = render_center(&gpu, FOG_RED, now, now);

    // t=duration: fully materialised. This is this test's own outside anchor
    // for the true lit colour, measured rather than predicted from the
    // lighting pipeline.
    let end = render_center(&gpu, FOG_RED, now, now - SECTION_FADE_DURATION_SECS);

    // t=duration/2: the discriminator. Predicted as the arithmetic mean of
    // the two measured anchors above, not merely bracketed between them.
    let mid = render_center(&gpu, FOG_RED, now, now - SECTION_FADE_DURATION_SECS * 0.5);

    println!("start (t=0)      : {start:?}");
    println!("end   (t=duration): {end:?}");
    println!("mid   (t=duration/2): {mid:?}");

    // Negative control, executed and observed to fail: a section whose
    // `build_time` predates the fade window entirely (the
    // `SECTION_FADE_ALREADY_VISIBLE` sentinel) must render as the lit colour
    // immediately, matching `end` and clearly disagreeing with `start` — the
    // detector that would catch a fade that never turns off.
    let already_visible = render_center(&gpu, FOG_RED, now, lodestone_render::SECTION_FADE_ALREADY_VISIBLE);
    println!("control (sentinel): {already_visible:?}");
    assert_eq!(
        already_visible, end,
        "the always-visible sentinel must render exactly like the fully materialised anchor"
    );
    assert!(
        (already_visible.0 as i32 - start.0 as i32).abs() > 100
            || (already_visible.1 as i32 - start.1 as i32).abs() > 100,
        "control must clearly disagree with the fresh (t=0) colour: {already_visible:?} vs {start:?}"
    );

    // t=0 reads as the fog colour: red-dominant, green/blue collapsed.
    assert!(
        start.0 > 200 && start.1 < 40 && start.2 < 40,
        "a freshly built section should read as the red fog colour, got {start:?}"
    );

    // The fully materialised anchor must clearly differ from the fog colour
    // — otherwise the two anchors are coincident and the midpoint check
    // below is vacuous (this file's own coincident-input trap).
    let end_i = (end.0 as i32, end.1 as i32, end.2 as i32);
    let start_i = (start.0 as i32, start.1 as i32, start.2 as i32);
    assert!(
        (end_i.0 - start_i.0).abs() > 60 || (end_i.1 - start_i.1).abs() > 60 || (end_i.2 - start_i.2).abs() > 60,
        "the two anchors must clearly differ or the midpoint predicts nothing: start={start:?} end={end:?}"
    );

    // The mid-fade discriminator: within 3/255 of the arithmetic mean of the
    // two measured anchors, on every channel. A delayed-pop implementation
    // (visibility snapped to 0 or 1 partway through the window instead of
    // ramping linearly) would land on one anchor exactly, not the mean of
    // both — well outside this tolerance.
    let predicted = (
        ((start.0 as i32 + end.0 as i32) / 2) as u8,
        ((start.1 as i32 + end.1 as i32) / 2) as u8,
        ((start.2 as i32 + end.2 as i32) / 2) as u8,
    );
    for (ch, (got, want)) in [mid.0, mid.1, mid.2].into_iter().zip([predicted.0, predicted.1, predicted.2]).enumerate() {
        let diff = (got as i32 - want as i32).abs();
        assert!(
            diff <= 3,
            "channel {ch}: mid-fade byte {got} is more than 3/255 from the predicted midpoint {want} \
             (start={start:?}, end={end:?}, mid={mid:?}) — this is the delayed-pop discriminator"
        );
    }
}
