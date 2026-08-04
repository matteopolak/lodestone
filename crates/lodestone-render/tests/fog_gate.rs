//! Offscreen gate for distance fog: **a fragment beyond the fog range must be
//! pulled to the fog colour**, and one with fog disabled must not.
//!
//! The scene is deliberately trivial so the assertion is unambiguous. An
//! identity camera draws a full-frame white quad at clip `z = 0`, but the fog
//! uniform places the *eye* 1000 units behind it, so every fragment sits ~1000
//! units away — well past the fog `end` of 500. With fog enabled the lit white
//! fragment is fully replaced by the fog colour (pure green here); with fog
//! disabled (a degenerate range) the same fragment stays white.
//!
//! The **negative control** is executed in the same test and observed failing:
//! the identical draw with [`FogUniform::disabled`] leaves the fragment white,
//! so the green channel is the only one high and red/blue collapse. A gate that
//! only saw the enabled case would not catch fog silently doing nothing.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test fog_gate -- --ignored --nocapture`.

use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex,
    fog::{FogSettings, FogUniform},
    model_anim_buffer, model_palette_buffer, model_shared_camera_buffer_with_fog,
    section_origin_buffer,
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
                label: Some("fog_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A full-frame opaque quad in clip space (identity camera), untinted
/// (`tint = 255`) and full-bright so its lit colour is the plain white texture.
fn white_quad() -> ModelMesh {
    let v = |x: f32, y: f32, u: f32, w: f32| ModelVertex {
        position: [x, y, 0.0],
        uv: [u, w],
        ao: 1.0,
        light: 0xFF,
        tint: 255,
        anim: 0,
        _pad: 0,
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

/// Render the white quad through the opaque model pipeline with the given fog
/// uniform and read back the centre pixel.
fn render_center(gpu: &Gpu, fog: FogUniform) -> (u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[255, 255, 255, 255].repeat(16), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let cam_buffer = model_shared_camera_buffer_with_fog(
        device,
        glam::Mat4::IDENTITY.to_cols_array_2d(),
        fog,
    );
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);
    let palette_buffer = model_palette_buffer(device, &[[1.0, 1.0, 1.0, 1.0]; 256]);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);

    let mesh = GpuModelMesh::upload(device, &white_quad()).expect("non-empty quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fog target"),
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
            label: Some("fog gate"),
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
fn distant_fragment_is_pulled_to_the_fog_colour() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter for fog_gate");
    };

    // Eye 1000 units behind the quad; fog full past 500 → the fragment is beyond
    // end, so it should read as the fog colour (pure green).
    let settings = FogSettings {
        color: [0.0, 1.0, 0.0],
        // No sky pass in this gate; the sky colour is inert here and tracks the
        // fog colour, which is what every `FogSettings` constructor defaults to.
        sky_color: [0.0, 1.0, 0.0],
        start: 100.0,
        end: 500.0,
        // The environmental term stays disabled: this scene's eye offset is
        // along world Z, which the shader's cylindrical metric groups with X
        // as "horizontal" (`fog.glsl:36-40`'s `max(length(rel.xz),
        // abs(rel.y))`), so the render-distance term alone already reads
        // ~1000 at the centre pixel — no environmental term is needed to
        // saturate past `end`.
        environmental_start: 0.0,
        environmental_end: 0.0,
    };
    let fog_on = FogUniform::new(&settings, [0.0, 0.0, -1000.0]);
    let (r_on, g_on, b_on) = render_center(&gpu, fog_on);

    // Negative control: fog disabled leaves the white quad white.
    let (r_off, g_off, b_off) = render_center(&gpu, FogUniform::disabled());

    println!("fog on : ({r_on}, {g_on}, {b_on})");
    println!("fog off: ({r_off}, {g_off}, {b_off})");

    // Enabled: fogged fragment is green-dominant (fog colour), red/blue low.
    assert!(
        g_on > 200 && r_on < 60 && b_on < 60,
        "distant fragment should read as the green fog colour, got ({r_on}, {g_on}, {b_on})"
    );
    // Disabled (control): the same fragment stays white — all channels high.
    assert!(
        r_off > 200 && g_off > 200 && b_off > 200,
        "with fog disabled the fragment should stay white, got ({r_off}, {g_off}, {b_off})"
    );
    // The delta that proves fog did something: red collapses from white to near
    // zero once fog is on.
    assert!(
        r_off as i32 - r_on as i32 > 150,
        "fog must measurably change the fragment (r {r_off} -> {r_on})"
    );
}
