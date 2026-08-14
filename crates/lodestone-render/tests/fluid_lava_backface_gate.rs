//! Offscreen gate for lava's back faces (issue #18, gap 4: "no back faces").
//!
//! Unlike water, lava is **not** rendered through the no-culling translucent
//! fluid pipeline — `mesh_snapshot_fluids` merges `FluidMeshes::lava` into the
//! **opaque** mesh (`crates/lodestone-shell/src/mesher.rs`,
//! `opaque.merge(&fluids.lava)`), which draws through
//! [`ModelPipeline::new`] (`RenderLayer::Solid`): back-face culling **on**
//! (`cull_mode: Some(Face::Back)`, `front_face: Ccw`). So for lava, unlike
//! water, `bake_fluid`'s back-copy quad is not redundant geometry — it is the
//! only thing standing between "this face is invisible from one side" and
//! correct vanilla behaviour.
//!
//! The scene: one `bake_fluid`-produced lava side face (`North`), inset by the
//! real `0.001` z-fight nudge. Vertex `positions` are block-local (`0..=1`);
//! this test maps them straight into clip space (`x' = x*2-1`, `y' = y*2-1`, a
//! flat `z' = 0.5`) with an identity camera, exactly like `fluid_gate.rs`'s
//! `water_quad()` — so "front" and "back" windings project to genuinely
//! opposite screen-space orientations, which is what `cull_mode: Back` acts
//! on.
//!
//! **Negative control (executed):** the pre-fix quad list — `bake_fluid`'s
//! *first* quad only, i.e. exactly what this file emitted before back faces
//! existed — drawn through the same opaque pipeline. It must render as the
//! clear colour (culled, invisible), which is the whole reported gap.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test fluid_lava_backface_gate -- --ignored --nocapture`.

use lodestone_assets::fluid::{FaceSet, FluidGeometry, SideOverlay, SpriteUv, bake_fluid};
use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex, model_anim_buffer,
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
                label: Some("fluid_lava_backface_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A real `bake_fluid` lava north side face: a full 8/9-height source with
/// nothing else drawn, matching what `mesh_fluids` would emit for a lava cell
/// whose only visible face is its north side. `include_back` selects whether
/// the second (reversed-winding) quad `bake_fluid` now produces is kept —
/// `false` reproduces exactly what this code emitted before gap 4 was closed.
fn lava_side_quads(include_back: bool) -> Vec<lodestone_assets::BakedQuad> {
    const SOURCE: f32 = 8.0 / 9.0;
    let unit = SpriteUv {
        min: [0.0, 0.0],
        max: [1.0, 1.0],
        anim: 0,
    };
    let geom = FluidGeometry {
        corners: [SOURCE; 4],
        flow: [0.0, 0.0],
        faces: FaceSet {
            up: false,
            down: false,
            north: true,
            south: false,
            east: false,
            west: false,
        },
        tint_index: None, // lava is untinted
        back_up_face: false,
        side_overlay: SideOverlay::default(), // lava has no overlay material
    };
    let mut quads = bake_fluid(&geom, unit, unit, None);
    assert_eq!(
        quads.len(),
        2,
        "one lava side face open to air must bake to front + back"
    );
    if !include_back {
        quads.truncate(1);
    }
    quads
}

/// Map `bake_fluid`'s block-local quads (`0..=1`) straight to clip space (like
/// `fluid_gate.rs`'s hand-built `water_quad`, bypassing camera math with an
/// identity camera) and flatten into a full-bright, untinted `ModelMesh` the
/// way `mesh_fluids` itself does for lava (`light = 0xFF`, `tint = 255`).
fn to_clip_space_mesh(quads: &[lodestone_assets::BakedQuad]) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    for quad in quads {
        let base = mesh.vertices.len() as u32;
        for i in 0..4 {
            let p = quad.positions[i];
            mesh.vertices.push(ModelVertex {
                position: [p[0] * 2.0 - 1.0, p[1] * 2.0 - 1.0, 0.5],
                uv: quad.uvs[i],
                ao: 1.0,
                light: 0xFF,
                tint: 255,
                anim: quad.anim,
                cutout_bypass: 0,
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh
}

/// Render `quads` through the real opaque (`RenderLayer::Solid`) pipeline —
/// the one lava's merged mesh actually draws through — over an orange lava
/// texture, and read back the centre pixel.
fn render_center(gpu: &Gpu, quads: &[lodestone_assets::BakedQuad]) -> (u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    // Opaque orange "lava" texture, alpha 255 — the opaque pass never blends.
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[255, 140, 0, 255].repeat(16), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);
    // The opaque model pipeline (unlike the fluid one) has a palette bind
    // group at group 2 and the anim group at 3. The shader's `Palette` is a
    // fixed `array<vec4<f32>, 256>`; lava is untinted (`tint = 255`), so every
    // slot can just be white.
    let palette_buffer = model_palette_buffer(device, &[[1.0, 1.0, 1.0, 1.0]; 256]);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    let mesh = GpuModelMesh::upload(device, &to_clip_space_mesh(quads)).expect("non-empty mesh");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lava backface target"),
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
            label: Some("lava backface gate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // A distinctive "sky" clear colour, nothing like the lava
                    // texture, so a culled draw is unambiguous.
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
fn lava_side_face_back_copy_is_visible_through_the_opaque_cull_back_pipeline() {
    let Some(gpu) = setup() else {
        panic!(
            "fluid_lava_backface_gate: no GPU adapter. This test is #[ignore]d, so running it \
             is an explicit request for a real GPU frame."
        );
    };

    // Fixed geometry (front quad's winding is fixed by bake_fluid), only the
    // presence of the back copy varies — isolating exactly the bit gap 4 adds.
    let front_only = lava_side_quads(false);
    let (cr, cg, cb) = render_center(&gpu, &front_only);
    println!(
        "pre-fix (front quad only, opaque cull_mode=Back): rgb=({cr},{cg},{cb}) \
         <-- expected to be the clear colour: culled, invisible"
    );

    let with_back = lava_side_quads(true);
    let (fr, fg, fb) = render_center(&gpu, &with_back);
    println!(
        "fixed (front + back copy): rgb=({fr},{fg},{fb}) \
         <-- expected to be the lava texture colour: the back copy is what's visible here"
    );

    // Negative control, executed: a single quad's winding is fixed by
    // `bake_fluid`'s vertex order (matching vanilla `FluidRenderer`'s
    // `positions` array) — it cannot be "front-facing" from both directions at
    // once, so *one* of the two draws above must read as the clear colour if
    // culling is doing anything at all. Whichever one it is, this proves the
    // pipeline's `cull_mode: Some(Face::Back)` really culls a single-sided
    // fluid quad — the exact mechanism that made lava's back faces a real, not
    // cosmetic, gap.
    let front_only_culled = cr < 20 && cg < 20;
    let with_back_visible = fr > 100 && fg > 60;
    assert!(
        front_only_culled || (!with_back_visible),
        "control premise: opaque cull_mode=Back must in fact cull one winding of a \
         single quad (front_only={front_only_culled}, with_back_visible={with_back_visible}); \
         if neither read as culled, this gate cannot see the defect it exists to catch"
    );
    assert!(
        with_back_visible,
        "front + back copy must render the lava texture through the opaque, \
         back-face-culling pipeline lava's merged mesh actually uses: got rgb=({fr},{fg},{fb})"
    );
    assert!(
        front_only_culled,
        "the pre-fix geometry (front quad only) must be invisible from this side \
         through the same pipeline — that invisibility is the reported gap: got rgb=({cr},{cg},{cb})"
    );
}
