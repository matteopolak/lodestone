//! Offscreen gate for the mining-crack pass: **the crack sprite must darken the
//! block surface it is drawn over**, and only where the sprite has cracks.
//!
//! The scene is minimal so the assertion is unambiguous. Clear the framebuffer
//! to a solid mid-grey "block face", clear depth to `1.0`, then draw a
//! full-frame crack quad through the real [`CrackPipeline`]. The crack atlas is
//! painted so its centre is opaque black (a crack) and its border is fully
//! transparent (no crack). If the pass works, the centre pixel is driven dark
//! while a corner pixel keeps the grey surface.
//!
//! The **negative control** runs in the same test and is observed failing: the
//! identical draw with a *fully transparent* atlas (alpha `0` everywhere, i.e.
//! no crack anywhere) leaves the centre grey. The whole point of the crack pass
//! is that delta; a gate that only saw the pass case would not catch a
//! regression to "crack draws nothing" — the exact defect this closes, where
//! the destroy-stage sprites were computed but never rendered.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test crack_gate -- --ignored --nocapture`.

use lodestone_render::crack::{CrackMesh, CrackVertex};
use lodestone_render::crack_pipeline::{CrackPipeline, GpuCrackMesh};
use lodestone_render::{CameraUniform, GpuAtlas, model_camera_buffer};

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
                label: Some("crack_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A full-frame crack quad in clip space (identity camera). The UVs span the
/// whole atlas so the painted crack maps straight onto the frame.
fn crack_quad() -> CrackMesh {
    let v = |x: f32, y: f32, u: f32, w: f32| CrackVertex {
        position: [x, y, 0.5],
        uv: [u, w],
    };
    CrackMesh {
        vertices: vec![
            v(-1.0, -1.0, 0.0, 1.0),
            v(1.0, -1.0, 1.0, 1.0),
            v(1.0, 1.0, 1.0, 0.0),
            v(-1.0, 1.0, 0.0, 0.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// A 4x4 crack atlas. `cracked` controls the centre texels' alpha: `200` is a
/// real crack (dark, mostly opaque), `0` is the transparent negative control.
/// The border texels are always transparent so a corner sample stays on the
/// grey surface.
fn crack_atlas_rgba(cracked: u8) -> Vec<u8> {
    let mut px = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4u32 {
        for x in 0..4u32 {
            let interior = (1..3).contains(&x) && (1..3).contains(&y);
            let a = if interior { cracked } else { 0 };
            // Crack colour is near-black; alpha carries the shape.
            px.extend_from_slice(&[10, 10, 10, a]);
        }
    }
    px
}

/// Render the crack quad over a grey "block face" and read back a pixel.
/// `center` picks the centre pixel (over a crack) when true, else a corner.
fn render_pixel(gpu: &Gpu, cracked: u8, center: bool) -> (u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = CrackPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &crack_atlas_rgba(cracked), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let cam_buffer = model_camera_buffer(
        device,
        CameraUniform {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer);
    let mesh = GpuCrackMesh::upload(device, &crack_quad()).expect("non-empty crack mesh");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("block face + crack target"),
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
            label: Some("crack gate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The "block face": a solid mid-grey already in the buffer.
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.5,
                        g: 0.5,
                        b: 0.5,
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
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &atlas_bg, &[]);
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

    let (px, py) = if center { (W / 2, H / 2) } else { (2, 2) };
    let i = (py * padded + px * 4) as usize;
    (data[i], data[i + 1], data[i + 2])
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn crack_sprite_darkens_the_block_surface() {
    let Some(gpu) = setup() else {
        panic!(
            "crack_gate: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for a real GPU frame — a headless CI box has none and should not run it."
        );
    };

    // The surface clears to grey 0.5 linear, which the sRGB framebuffer reads
    // back as byte ~188. A real crack (alpha ~0.78, colour ~black) must pull the
    // centre far below that.
    let (cr, _cg, _cb) = render_pixel(&gpu, 200, true);
    // A corner sits over a transparent border texel: the crack must NOT touch it.
    let (edge_r, _, _) = render_pixel(&gpu, 200, false);
    // Negative control: a fully transparent atlas (no crack) leaves the centre grey.
    let (control_r, _, _) = render_pixel(&gpu, 0, true);

    println!("crack centre  r={cr}   (grey surface reads ~188; a crack pulls it dark)");
    println!("crack corner  r={edge_r} (transparent border: must stay ~188)");
    println!("no-crack ctrl r={control_r} (transparent atlas: must stay ~188)");

    // The cracked centre is clearly darkened, well below the grey 188 surface.
    assert!(
        cr < 140,
        "the crack sprite must darken the block surface: centre r={cr} should be well below \
         the grey ~188 surface"
    );
    // The transparent border does not touch the surface.
    assert!(
        edge_r > 160,
        "the crack must only affect cracked texels: corner r={edge_r} should keep the grey surface"
    );
    // Negative control: with no crack, the centre must remain the grey surface —
    // and it must be visibly brighter than the cracked centre. This is the delta
    // that makes the gate real; the no-crack render (r={control_r}) does NOT meet
    // the darken criterion above, which is exactly the regression this guards.
    assert!(
        control_r > 160 && control_r > cr + 40,
        "no-crack control must leave the surface grey (r={control_r}) and far brighter than the \
         cracked centre (r={cr})"
    );
}
