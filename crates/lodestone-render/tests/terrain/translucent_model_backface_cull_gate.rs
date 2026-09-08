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

/// Append one square filling clip space. `front` selects CCW (front-facing
/// under `FrontFace::Ccw`, the way ice's `Up` quad winds toward a camera
/// looking down through it) or CW (the way its `Down` quad winds — back-facing
/// to that same camera). `z` is an identity-camera depth and `ao` supplies a
/// controlled material difference for the depth-order regression below.
fn append_quad(mesh: &mut ModelMesh, front: bool, z: f32, ao: f32) {
    let mut positions = [
        [-1.0f32, -1.0, z],
        [1.0, -1.0, z],
        [1.0, 1.0, z],
        [-1.0, 1.0, z],
    ];
    if !front {
        positions.reverse();
    }
    let base = mesh.vertices.len() as u32;
    for p in positions {
        mesh.vertices.push(ModelVertex {
            position: p,
            uv: [0.0, 0.0],
            ao,
            light: 0xFF,
            tint: 255,
            anim: 0,
            cutout_bypass: 0,
            tint_rgb_override: [0, 0, 0, 0],
        });
    }
    mesh.indices.extend_from_slice(&[
        base,
        base + 1,
        base + 2,
        base + 2,
        base + 3,
        base,
    ]);
}

/// One full-screen quad at the depth used by the original culling gate.
fn quad(front: bool) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    append_quad(&mut mesh, front, 0.5, 1.0);
    mesh
}

fn max_rgb_delta(a: (u8, u8, u8), b: (u8, u8, u8)) -> u8 {
    [a.0.abs_diff(b.0), a.1.abs_diff(b.1), a.2.abs_diff(b.2)]
        .into_iter()
        .max()
        .expect("three RGB channels")
}

fn srgb_byte(linear: f64) -> u8 {
    let srgb = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (srgb.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Two overlapping, camera-facing ice surfaces. The near surface is at `z =
/// 0.65` and is brighter than the far surface, so a depth-writing pipeline
/// must reject the far surface when it arrives after the near one.
fn overlapping_ice(near_first: bool) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    let append = |mesh: &mut ModelMesh, near: bool| {
        append_quad(mesh, true, if near { 0.65 } else { 0.5 }, if near { 1.0 } else { 0.5 });
    };
    if near_first {
        append(&mut mesh, true);
        append(&mut mesh, false);
    } else {
        append(&mut mesh, false);
        append(&mut mesh, true);
    }
    mesh
}

/// One or two front-facing ice layers used by the alpha/transmission gate.
fn ice_layers(stacked: bool) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    append_quad(&mut mesh, true, 0.5, 1.0);
    if stacked {
        append_quad(&mut mesh, true, 0.65, 1.0);
    }
    mesh
}

/// One camera-facing ice surface. `near` selects the surface at `z = 0.65`
/// used by the edge-face side of the overlap scene; the other surface is at
/// `z = 0.5` and stands in for the connected sheet behind it.
fn ice_surface(near: bool) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    append_quad(&mut mesh, true, if near { 0.65 } else { 0.5 }, 1.0);
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
        Self::with_alpha(gpu, 255)
    }

    fn with_alpha(gpu: &'a Gpu, alpha: u8) -> Self {
        let atlas = GpuAtlas::from_rgba(
            &gpu.device,
            &gpu.queue,
            4,
            4,
            &[200, 220, 255, alpha].repeat(16),
            &[],
        );
        Scene {
            device: &gpu.device,
            queue: &gpu.queue,
            atlas,
        }
    }

    /// Render `mesh` through `pipeline` over the usual dark clear and read
    /// back the centre pixel.
    fn render_center(&self, pipeline: &ModelPipeline, mesh: &ModelMesh) -> (u8, u8, u8) {
        self.render_center_over(
            pipeline,
            mesh,
            wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.2,
                a: 1.0,
            },
        )
    }

    /// Render `mesh` through `pipeline` over a caller-selected background and
    /// read back the centre pixel. Distinct bright/dark backgrounds keep the
    /// alpha measurement independent from the source material colour.
    fn render_center_over(
        &self,
        pipeline: &ModelPipeline,
        mesh: &ModelMesh,
        clear: wgpu::Color,
    ) -> (u8, u8, u8) {
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
                        load: wgpu::LoadOp::Clear(clear),
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

/// The real ice texture carries alpha `180/255`, not an opaque mask. A single
/// isolated surface must therefore leave a measurable amount of both a dark
/// and a bright outside background visible. A second connected front-facing
/// layer must move the result toward the ice material; this is a distinct
/// control from the depth-order scene below, and prevents a gate from passing
/// with a fixed colour or an accidentally opaque atlas upload.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn ice_alpha_transmits_outside_background_for_isolated_and_connected_layers() {
    let Some(gpu) = setup() else {
        panic!(
            "translucent_model_backface_cull_gate: no GPU adapter. This test is #[ignore]d, \
             so running it is an explicit request for a real GPU frame."
        );
    };
    let pipeline = ModelPipeline::for_layer(&gpu.device, FORMAT, RenderLayer::Translucent);
    let translucent = Scene::with_alpha(&gpu, 180);
    let opaque = Scene::with_alpha(&gpu, 255);

    for (name, clear, dark_background) in [
        (
            "dark",
            wgpu::Color {
                r: 0.08,
                g: 0.08,
                b: 0.08,
                a: 1.0,
            },
            true,
        ),
        (
            "bright",
            wgpu::Color {
                r: 0.92,
                g: 0.92,
                b: 0.92,
                a: 1.0,
            },
            false,
        ),
    ] {
        let isolated = translucent.render_center_over(&pipeline, &ice_layers(false), clear);
        let connected = translucent.render_center_over(&pipeline, &ice_layers(true), clear);
        let opaque_surface = opaque.render_center_over(&pipeline, &ice_layers(false), clear);
        let floor_r = srgb_byte(clear.r);
        println!(
            "{name} floor, centre ({}, {}): floor={floor_r}, isolated={isolated:?}, \
             connected={connected:?}, opaque-control={opaque_surface:?}",
            W / 2,
            H / 2
        );

        let low = floor_r.min(opaque_surface.0);
        let high = floor_r.max(opaque_surface.0);
        assert!(
            isolated.0 > low + 4 && isolated.0 + 4 < high,
            "alpha=180 ice must leave the outside background between the floor and opaque \
             material endpoints at centre ({}, {}): floor={floor_r}, isolated={isolated:?}, \
             opaque={opaque_surface:?}",
            W / 2,
            H / 2
        );
        if dark_background {
            assert!(
                connected.0 > isolated.0 + 5,
                "a second connected ice layer must move a dark background toward the ice \
                 material: isolated={isolated:?}, connected={connected:?} at centre ({}, {})",
                W / 2,
                H / 2
            );
        } else {
            assert!(
                connected.0 + 5 < isolated.0,
                "a second connected ice layer must move a bright background toward the ice \
                 material: isolated={isolated:?}, connected={connected:?} at centre ({}, {})",
                W / 2,
                H / 2
            );
        }
    }
}

/// Two overlapping ice surfaces can come from an edge face and a connected
/// sheet in one section. Their materials differ through the measured AO/light
/// inputs, so draw order is observable. When the nearer surface at `z=0.65`
/// is submitted first, depth writes must reject the farther `z=0.5` surface;
/// this is the order that made an exposed edge face paint over a nearer sheet
/// before the fix. The opposite order is retained as a control: standard
/// alpha blending correctly shows both surfaces when the farther one arrives
/// first. The old no-write state is the executed negative control: the bad
/// near-first order changes the centre pixel because both fragments blend.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the no-depth-write control fail"]
fn a_far_ice_surface_cannot_paint_over_a_nearer_surface_submitted_first() {
    let Some(gpu) = setup() else {
        panic!(
            "translucent_model_backface_cull_gate: no GPU adapter. This test is #[ignore]d, \
             so running it is an explicit request for a real GPU frame."
        );
    };
    let scene = Scene::with_alpha(&gpu, 180);
    let pipeline = ModelPipeline::for_layer(&gpu.device, FORMAT, RenderLayer::Translucent);
    let clear = wgpu::Color {
        r: 0.08,
        g: 0.08,
        b: 0.08,
        a: 1.0,
    };
    let near_first = scene.render_center_over(&pipeline, &overlapping_ice(true), clear);
    let far_first = scene.render_center_over(&pipeline, &overlapping_ice(false), clear);
    let near_only = scene.render_center_over(&pipeline, &ice_surface(true), clear);
    println!(
        "overlapping ice centre ({}, {}): near-first={near_first:?}, far-first={far_first:?}, \
         near-only={near_only:?}",
        W / 2,
        H / 2
    );
    assert!(
        max_rgb_delta(near_first, near_only) <= 2,
        "a farther ice surface must be rejected after the nearer one at centre ({}, {}): \
         near-first={near_first:?}, near-only={near_only:?}",
        W / 2,
        H / 2
    );
    assert!(
        far_first.0 >= near_only.0.saturating_add(2),
        "far-first control must retain the farther alpha layer at centre ({}, {}): \
         far-first={far_first:?}, near-only={near_only:?}",
        W / 2,
        H / 2
    );
}
