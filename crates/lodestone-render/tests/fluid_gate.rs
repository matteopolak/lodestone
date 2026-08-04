//! Offscreen gate for translucent water: **the sea floor must be visible
//! through the water surface**.
//!
//! The scene is deliberately minimal so the assertion is unambiguous: clear the
//! framebuffer to a solid **red** "sea floor", then draw a full-frame water quad
//! through the real [`ModelPipeline::for_fluid`] pass (alpha blending, no depth
//! writes, no cutout discard). If water is genuinely translucent, the red floor
//! survives in the blended result.
//!
//! The **negative control** is executed in the same test and observed failing:
//! the identical draw with an *opaque* water texture (alpha `255`) instead of a
//! translucent one (alpha `180`, water's real value) hides the floor entirely —
//! the red channel collapses to ~0. That delta is the whole point: a gate that
//! only ever saw the pass case would not catch a regression back to opaque water
//! (the exact bug this fixes, where water rendered as nothing or as a solid
//! blob). Printing both makes the failure of the control visible in the log.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test fluid_gate -- --ignored --nocapture`.

use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex, model_anim_buffer,
    model_shared_camera_buffer, section_origin_buffer,
};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

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
                label: Some("fluid_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A full-frame water quad in clip space (identity camera): tinted (`tint = 0`)
/// so the fluid shader applies the water colour, full-bright, unoccluded.
fn water_quad() -> ModelMesh {
    let v = |x: f32, y: f32, u: f32, w: f32| ModelVertex {
        position: [x, y, 0.5],
        uv: [u, w],
        ao: 1.0,
        light: 0xFF,
        tint: 0,
        anim: 0,
        _pad: 0,
        tint_rgb_override: [0, 0, 0, 0],
    };
    ModelMesh {
        // CCW from the front; the fluid pass disables culling anyway.
        vertices: vec![
            v(-1.0, -1.0, 0.0, 1.0),
            v(1.0, -1.0, 1.0, 1.0),
            v(1.0, 1.0, 1.0, 0.0),
            v(-1.0, 1.0, 0.0, 0.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// Render the water quad over a red floor and read back the centre pixel.
/// `water_alpha` is the alpha of the (otherwise white) water texture: `180` is
/// water's real translucency, `255` is the opaque negative control.
fn render_center(gpu: &Gpu, water_alpha: u8) -> (u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::for_fluid(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(
        device,
        queue,
        4,
        4,
        &[255, 255, 255, water_alpha].repeat(16),
        &[],
    );
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);
    // The fluid shader carries the shared animation group (group 2); bind an
    // empty (all-static) slot table so no quad animates.
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    let mesh = GpuModelMesh::upload(device, &water_quad()).expect("non-empty water mesh");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("floor+water target"),
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
            label: Some("fluid gate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The "sea floor": a solid red already in the framebuffer.
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
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
        pass.set_bind_group(2, &anim_bg, &[]);
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.index_count, 0, 0..1);
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

    let (cx, cy) = (W / 2, H / 2);
    let i = (cy * padded + cx * 4) as usize;
    (data[i], data[i + 1], data[i + 2])
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn sea_floor_is_visible_through_translucent_water() {
    let Some(gpu) = setup() else {
        panic!(
            "fluid_gate: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for a real GPU frame — a headless CI box has none and should not run it."
        );
    };

    // Water's real texture alpha (180/255 ≈ 0.71): translucent.
    let (tr, tg, tb) = render_center(&gpu, 180);
    // Negative control: identical draw, opaque water texture (alpha 255).
    let (or, og, ob) = render_center(&gpu, 255);

    println!("translucent water over red floor: rgb=({tr},{tg},{tb})");
    println!(
        "opaque control  water over red floor: rgb=({or},{og},{ob})  <-- floor contributes no red"
    );

    // See-through proof is the *delta*: opaque water shows only its own colour
    // (its red channel `or` is the water tint's red, with zero floor). Translucent
    // water lets the red sea floor add on top, so its red `tr` is measurably
    // higher. If alpha blending regressed to opaque, `tr` would collapse to `or`
    // and this assertion would fail — which is exactly what makes the gate real.
    assert!(
        tr > or + 30,
        "the red sea floor must show through translucent water: translucent r={tr} \
         should exceed opaque-control r={or} by a clear margin"
    );
    // Sanity: the water tint itself is present (a blue-dominant surface), so we
    // are blending water, not drawing nothing.
    assert!(
        tb > tr && tb > 40,
        "water surface should be blue-dominant, got rgb=({tr},{tg},{tb})"
    );
}
