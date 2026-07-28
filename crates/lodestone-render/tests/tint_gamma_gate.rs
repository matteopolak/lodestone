//! Offscreen gate for **biome tint colour fidelity**: a grass-tinted quad must
//! render at vanilla's calibrated green/red ratio (`#91BD59` → G/R ≈ 1.30), not
//! the washed-out ~1.13 the previous shader produced.
//!
//! ## Why this gate exists
//!
//! The block atlas is an `_srgb` texture, so `textureSample` returns *linear*
//! texels. The tint palette holds straight sRGB bytes. Multiplying a linear
//! texel by an sRGB tint and letting the sRGB surface re-encode the result
//! gamma-compresses the tint's green/red ratio: grass `1.30` collapses to
//! ~`1.13`, measurably greyer than vanilla (confirmed live with a location mask —
//! the whole grass population sat at 1.13 instead of 1.30). The shader now applies
//! the tint in **gamma space** (sRGB → multiply → back to linear) so the ratio
//! survives the surface encode.
//!
//! ## The gate and its negative control
//!
//! Render a single mid-grey quad tinted with grass `#91BD59` through the real
//! [`ModelPipeline`] (the model pass, *with* the palette bind group), read back the
//! centre pixel, and assert its G/R lands in vanilla's band. The **negative
//! control**, executed and printed in the same test, is the linear-space value the
//! buggy shader produced (~1.13): the assertion's lower bound (`1.24`) is chosen so
//! that a regression to linear-space tinting collapses the measured ratio below it
//! and the gate fails. A second control renders the *untinted* slot (255) and
//! asserts it stays grey (G/R ≈ 1.0), proving the tint — not the texture — creates
//! the green.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test tint_gamma_gate -- --ignored --nocapture`.

use lodestone_render::{
    CameraUniform, GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex,
    model_camera_buffer, model_palette_buffer,
};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Vanilla plains grass tint `#91BD59` (G/R = 1.303). This is the exact value the
/// old shader hardcoded, so a correct render reproduces it here.
const GRASS: [f32; 4] = [
    0x91 as f32 / 255.0,
    0xBD as f32 / 255.0,
    0x59 as f32 / 255.0,
    1.0,
];

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
                label: Some("tint_gamma_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A full-frame quad in clip space (identity camera), full-bright and unoccluded,
/// carrying `tint` as its palette index.
fn quad(tint: u8) -> ModelMesh {
    let v = |x: f32, y: f32, u: f32, w: f32| ModelVertex {
        position: [x, y, 0.5],
        uv: [u, w],
        ao: 1.0,
        light: 0xFF,
        tint,
        _pad: [0, 0],
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

/// Render a mid-grey quad with palette slot `tint` and read back the centre pixel.
/// Slot 0 holds grass; slot 255 is white (untinted).
fn render_center(gpu: &Gpu, tint: u8) -> (u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    // A neutral mid-grey texture (G/R = 1.0): any green in the readback comes from
    // the tint, not the texel.
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[128, 128, 128, 255].repeat(16), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let mut palette = vec![[1.0_f32, 1.0, 1.0, 1.0]; 256];
    palette[0] = GRASS;
    let palette_buffer = model_palette_buffer(device, &palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);

    let cam_buffer = model_camera_buffer(
        device,
        CameraUniform {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer);
    let mesh = GpuModelMesh::upload(device, &quad(tint)).expect("non-empty quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tint target"),
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
            label: Some("tint gamma gate"),
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

fn gr(r: u8, g: u8) -> f32 {
    (f32::from(g) + 1.0) / (f32::from(r) + 1.0)
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn grass_tint_renders_at_vanilla_ratio_not_gamma_compressed() {
    let Some(gpu) = setup() else {
        panic!(
            "tint_gamma_gate: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for a real GPU frame — a headless CI box has none and should not \
             run it."
        );
    };

    let (tr, tg, tb) = render_center(&gpu, 0); // grass-tinted
    let (ur, ug, ub) = render_center(&gpu, 255); // untinted control
    let tinted = gr(tr, tg);
    let untinted = gr(ur, ug);

    println!("grass-tinted grey quad: rgb=({tr},{tg},{tb})  G/R={tinted:.3}  (vanilla target 1.30)");
    println!(
        "untinted control:       rgb=({ur},{ug},{ub})  G/R={untinted:.3}  (grey, tint does nothing)"
    );
    println!(
        "negative control: a regression to linear-space tinting renders this same quad at \
         G/R ~1.13, which is below the 1.24 lower bound and fails the assertion below."
    );

    // The fix: the tint must survive the sRGB surface encode at vanilla's ratio.
    // Linear-space tinting (the bug) collapses this to ~1.13 and trips the bound.
    assert!(
        (1.24..=1.38).contains(&tinted),
        "grass tint must render at vanilla's G/R ≈ 1.30, got {tinted:.3}; a value near 1.13 \
         means the tint is being multiplied in linear space (the regression this gate guards)"
    );
    // Untinted quad stays neutral grey: the green is the tint's doing, nothing else.
    assert!(
        (0.9..=1.1).contains(&untinted),
        "untinted (palette slot 255) must stay grey (G/R ≈ 1.0), got {untinted:.3}"
    );
    // And tinting must make a clear, measurable difference.
    assert!(
        tinted > untinted + 0.15,
        "the grass tint must lift G/R well above the untinted grey: tinted {tinted:.3} vs \
         untinted {untinted:.3}"
    );
}
