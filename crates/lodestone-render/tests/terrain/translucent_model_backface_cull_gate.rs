//! Offscreen gate for the second half of the ice fix: "looking at the bottom
//! of the ice (from the top) shows no opacity at all."
//!
//! Traced to `ModelPipeline::for_layer`'s prior `cull_mode: None` for
//! `RenderLayer::Translucent`. Real vanilla diverges: `RenderPipelines.
//! TRANSLUCENT_TERRAIN`/`TRANSLUCENT_BLOCK` both build on `TERRAIN_SNIPPET`/
//! `BLOCK_SNIPPET`, and neither those nor their translucent variants ever call
//! `.withCull(false)` — `RenderPipeline.Builder`'s own default is
//! `this.cull.orElse(true)`. So real translucent terrain (ice included)
//! renders **single-sided**, exactly like opaque terrain: only the
//! camera-facing side of a quad draws, the other is culled by the GPU.
//!
//! With culling disabled, a solid cube's *far* face — e.g. ice's `Down` quad,
//! back-facing to a camera looking down through its `Up` quad — draws too,
//! double-compositing the same partial alpha along the view ray and reading
//! as markedly *more* opaque than a single correct blend: the reported
//! "shows no opacity at all".
//!
//! This gate proves the mechanism the same way `fluid_lava_backface_gate.rs`
//! proves lava's opaque cull: two quads, identical shape, opposite winding
//! (one "front", the way `Up` faces a downward camera; one "back", the way
//! `Down` does), through the real fixed pipeline
//! (`ModelPipeline::for_layer(.., RenderLayer::Translucent)`) — the back one
//! must be culled (invisible) and the front one must render.
//!
//! `ModelPipeline::build` is private and deliberately not exported as a way
//! to construct a known-wrong pipeline, so the negative control (the pre-fix
//! `cull_mode: None` reproducing "back quad also renders") was run manually
//! by temporarily reverting `model_pipeline.rs`'s `cull_back_face` wiring,
//! observing this gate fail, and restoring from an md5-checked backup — see
//! the fix's own commit/report for that reading. This file keeps only the
//! permanent regression gate.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test translucent_model_backface_cull_gate -- --ignored --nocapture`.

use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex, RenderLayer, model_anim_buffer,
    model_palette_buffer, model_shared_camera_buffer, section_origin_buffer,
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
                label: Some("translucent_model_backface_cull_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// One square filling clip space, `front` selecting CCW (front-facing under
/// `FrontFace::Ccw`, the way ice's `Up` quad winds toward a camera looking
/// down through it) or CW (the way its `Down` quad winds — back-facing to
/// that same camera). Fullbright, untinted (`light = 0xFF`, `tint = 255`,
/// `ao = 1.0`), alpha 255: this gate is about whether the quad draws **at
/// all**, not about a blended byte value (which this backend does not let a
/// test predict exactly — see `CLAUDE.md`'s `ALPHA_BLENDING` note).
fn quad(front: bool) -> ModelMesh {
    let mut positions = [
        [-1.0f32, -1.0, 0.5],
        [1.0, -1.0, 0.5],
        [1.0, 1.0, 0.5],
        [-1.0, 1.0, 0.5],
    ];
    if !front {
        positions.reverse();
    }
    let mut mesh = ModelMesh::default();
    for p in positions {
        mesh.vertices.push(ModelVertex {
            position: p,
            uv: [0.0, 0.0],
            ao: 1.0,
            light: 0xFF,
            tint: 255,
            anim: 0,
            cutout_bypass: 0,
            tint_rgb_override: [0, 0, 0, 0],
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 2, 3, 0]);
    mesh
}

/// Every non-pipeline piece of the scene: an opaque light-blue "ice" texture
/// (alpha 255 — see [`quad`]'s doc for why this gate does not need partial
/// alpha), an identity camera, and a distinctive dark clear colour so a
/// culled draw is unambiguous.
struct Scene<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    atlas: GpuAtlas,
}

impl<'a> Scene<'a> {
    fn new(gpu: &'a Gpu) -> Self {
        let atlas = GpuAtlas::from_rgba(
            &gpu.device,
            &gpu.queue,
            4,
            4,
            &[200, 220, 255, 255].repeat(16),
            &[],
        );
        Scene {
            device: &gpu.device,
            queue: &gpu.queue,
            atlas,
        }
    }

    /// Render `mesh` through `pipeline` and read back the centre pixel.
    fn render_center(&self, pipeline: &ModelPipeline, mesh: &ModelMesh) -> (u8, u8, u8) {
        let device = self.device;
        let queue = self.queue;
        let atlas_bg = pipeline.atlas_bind_group(device, &self.atlas);
        let cam_buffer =
            model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
        let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
        let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);
        let palette_buffer = model_palette_buffer(device, &[[1.0, 1.0, 1.0, 1.0]; 256]);
        let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
        let anim_buffer = model_anim_buffer(device, &[]);
        let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
        let gpu_mesh = GpuModelMesh::upload(device, mesh).expect("non-empty mesh");

        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("translucent backface target"),
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

        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("translucent backface gate"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.2,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
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
            pass.set_bind_group(0, &cam_bg, &[0]);
            pass.set_bind_group(1, &atlas_bg, &[]);
            pass.set_bind_group(2, &palette_bg, &[]);
            pass.set_bind_group(3, &anim_bg, &[]);
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
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
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn translucent_pipeline_culls_the_far_face_of_a_solid_cube() {
    let Some(gpu) = setup() else {
        panic!(
            "translucent_model_backface_cull_gate: no GPU adapter. This test is #[ignore]d, \
             so running it is an explicit request for a real GPU frame."
        );
    };
    let scene = Scene::new(&gpu);

    let fixed = ModelPipeline::for_layer(&gpu.device, FORMAT, RenderLayer::Translucent);

    let (fr, fg, fb) = scene.render_center(&fixed, &quad(true));
    println!(
        "fixed pipeline, front-winding quad (ice's Up face, camera-facing): rgb=({fr},{fg},{fb}) \
         <-- expected to be the ice texture colour: front faces must still render"
    );
    let (br, bg, bb) = scene.render_center(&fixed, &quad(false));
    println!(
        "fixed pipeline, back-winding quad (ice's Down face, facing away): rgb=({br},{bg},{bb}) \
         <-- expected to be the clear colour: the far face must now be culled"
    );

    // The ice texture is (200, 220, 255): unambiguous against the clear
    // colour on every channel (its `r`/`g` are 0; its sRGB-encoded `b` from
    // `wgpu::Color { b: 0.2, .. }` measures ~124, well short of ice's 255).
    let front_visible = fr > 150 && fg > 150 && fb > 150;
    let back_culled = br < 20 && bg < 20 && bb < 200;
    assert!(
        front_visible,
        "the camera-facing quad must render through the fixed Translucent pipeline: \
         got rgb=({fr},{fg},{fb})"
    );
    assert!(
        back_culled,
        "the far-facing quad must be culled (clear colour) through the fixed \
         Translucent pipeline — this is the fix: got rgb=({br},{bg},{bb})"
    );
}
