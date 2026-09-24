//! **Defect 2 — "mobs are drawn on top of water even if they're submerged."**
//!
//! The report is one symptom with *two independent causes*, and this file gates
//! them separately because fixing either alone leaves the player's complaint
//! partly standing:
//!
//! 1. **The entity shader applied no fog.** Only the model and fluid shaders
//!    read the fog block, so a mob at any depth (or at the render-distance edge)
//!    rendered at full contrast against terrain that had already faded. When the
//!    eye is submerged the shell swaps in a short, biome-coloured water fog;
//!    terrain dissolved into it and mobs did not.
//! 2. **Entities were drawn after the translucent water pass.** Water is
//!    alpha-blended with depth *write* disabled, so it leaves nothing in the
//!    depth buffer. A mob drawn afterwards passes the depth test against the sea
//!    floor and writes opaque colour straight over the water surface — it is
//!    painted on top of the water however deep it is.
//!
//! **These do not substitute for each other.** Fog tints a mob by distance; it
//! cannot put a water surface in front of it. Draw order puts water in front;
//! it cannot make a distant mob fade. `entity_fog_darkens_with_depth` gates (1);
//! `water_surface_covers_a_mob_behind_it` gates (2), and would still fail with
//! fog perfectly correct.
//!
//! `#[ignore]`d: needs a real GPU adapter, and once opted in a missing adapter
//! is a failure, never a skip.

use glam::Vec3;
use lodestone_assets::entity_models::zombie_model;
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{ENTITY_FULLBRIGHT, EntityInstance, EntityMesh, plan_entities};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};
use lodestone_render::fog::{FogSettings, FogUniform};

const W: u32 = 192;
const H: u32 = 192;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A deep red fog, chosen to be nothing like the mob's grey sheet or the sky, so
/// "did this fragment move toward the fog colour?" is unambiguous in the red
/// channel alone.
const FOG_COLOR: [f32; 3] = [0.9, 0.05, 0.05];

/// The fade band. The two depths below sit well inside it — near the bottom and
/// near the top — rather than sharing a plateau at either end, so the fog factor
/// genuinely differs between them (0.11 vs 0.78).
const FOG_START: f32 = 2.0;
const FOG_END: f32 = 20.0;

/// The two depths the mob is measured at.
const NEAR_Z: f32 = 4.0;
const FAR_Z: f32 = 16.0;

/// Pure black, so the mob-pixel test below cannot pick up background.
///
/// A merely *dark* clear does not work on an `_srgb` target: linear `0.02`
/// encodes to byte 39, which sails past any "is this pixel lit?" threshold and
/// silently folds the whole background into the mean. Black encodes to 0.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
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
                label: Some("entity_fog_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// The mob is scaled in proportion to its distance and the eye is raised to
/// match, so its **silhouette stays the same size on screen** at both depths.
///
/// Without this the far mob is a fraction of the near one's pixel count, and its
/// mean is dominated by different parts of the model — a difference that has
/// nothing to do with fog but would show up in the reading as though it did.
/// `disabled_fog_leaves_the_two_depths_identical` is what proves this control
/// works: with fog off the two depths must read the same number.
fn scale_for(z_back: f32) -> f32 {
    z_back / NEAR_Z
}

fn camera_at(z_back: f32) -> Camera {
    Camera {
        position: Vec3::new(0.0, 0.9 * scale_for(z_back), -z_back),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 128.0,
    }
}

fn upload_grey_sheet(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::Texture, wgpu::Sampler) {
    const N: u32 = 16;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fog-sheet"),
        size: wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let rgba: Vec<u8> = [160u8, 160, 160, 255].repeat((N * N) as usize);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(N * 4),
            rows_per_image: Some(N),
        },
        wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
    );
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("fog-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (texture, sampler)
}

/// Render one mob `z_back` blocks in front of the eye under `fog`, and return
/// the mean red channel over its silhouette. The sheet is neutral grey and the
/// fog is red, so red rises monotonically with the fog amount.
fn mob_red(gpu: &Gpu, z_back: f32, fog: FogSettings) -> f32 {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = camera_at(z_back);
    let def = zombie_model();
    let mesh = EntityMesh::from_named_model("zombie", &def);

    // Full-bright so the *only* thing separating the two readings is fog.
    let inst = EntityInstance::new(
        "zombie",
        &mesh,
        Vec3::ZERO,
        90.0,
        scale_for(z_back),
        &AnimInput::REST,
    )
    .with_light(ENTITY_FULLBRIGHT);
    let frame = plan_entities(std::slice::from_ref(&inst), &camera.frustum());
    assert_eq!(frame.instance_count(), 1, "the mob must be on screen");

    let pipeline = EntityPipeline::new(device, FORMAT);
    let (tex, sampler) = upload_grey_sheet(device, queue);
    let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let eye = camera.position;
    let cam_buf = pipeline.camera_buffer_with_fog(
        device,
        &camera,
        FogUniform::new(&fog, [eye.x, eye.y, eye.z]),
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_mesh = GpuEntityModel::upload(device, &mesh).expect("non-empty mesh");

    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_mesh.parts.iter().zip(&batch.parts) {
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
    assert!(!per_part.is_empty(), "nothing would be drawn");

    let (color, color_view) = color_target(device);
    let depth = DepthBuffer::new(device, W, H);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("entity fog pass"),
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
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }
    }
    let frame = readback(device, queue, enc, &color);

    // The clear is pure black (byte 0) and every mob texel is grey or fogged
    // red, so any lit pixel is mob.
    let (mut sum, mut n) = (0u64, 0u32);
    for i in 0..(W * H) as usize {
        let px = &frame[i * 4..i * 4 + 3];
        if u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]) > 60 {
            sum += u64::from(px[0]);
            n += 1;
        }
    }
    assert!(
        n > 1500,
        "only {n} mob pixels at z={z_back} — the reading would be a sliver (the \
         distance-compensating scale should keep this near-constant across depths)"
    );
    sum as f32 / n as f32
}

fn color_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fog-color"),
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
    let view = color.create_view(&wgpu::TextureViewDescriptor::default());
    (color, view)
}

fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut enc: wgpu::CommandEncoder,
    color: &wgpu::Texture,
) -> Vec<u8> {
    let padded = (W * 4).next_multiple_of(256);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fog-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
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
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
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
    buf.unmap();
    out
}

/// **Cause 1.** The same mob at two depths under the same fog must render
/// measurably differently, and the difference must be *toward the fog colour*.
///
/// # Both predictions
///
/// The wrong implementation (the shipped entity shader, which reads no fog
/// block at all) renders the two depths identically: the mob's size on screen
/// changes but its per-pixel colour does not, so the red delta is
/// [`UNFOGGED_DELTA`] = 0. The correct implementation fogs the far mob much
/// further toward red than the near one; with a 0.9-red fog over a grey sheet
/// the gap is tens of bytes, so the band starts an order of magnitude above the
/// broken value.
///
/// The **equal-depths control** runs in the same test: two renders at the same
/// distance must read 0 apart. That is what separates "fog responds to depth"
/// from "these two renders happen to differ for some other reason" — a shader
/// that returned, say, a random or position-dependent tint would pass a
/// naive "the frames differ" assertion and fails this one.
const UNFOGGED_DELTA: f32 = 0.0;
/// Floor on the near→far red rise for a fog this saturated across this fade
/// band. Comfortably above the broken value and above readback noise.
const MIN_FOGGED_DELTA: f32 = 25.0;

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fire"]
fn entity_fog_darkens_with_depth() {
    let Some(gpu) = setup() else {
        panic!("entity_fog_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a failure")
    };
    let fog = FogSettings {
        color: FOG_COLOR,
        // Entity pass only; no sky disc is drawn, so this tracks the fog colour
        // the way every `FogSettings` constructor defaults it.
        sky_color: FOG_COLOR,
        start: FOG_START,
        end: FOG_END,
        // Environmental term disabled: this gate is specifically about the
        // render-distance term's response to depth, and leaving the
        // environmental pair degenerate keeps it a single-term measurement
        // exactly as before F2/F3.
        environmental_start: 0.0,
        environmental_end: 0.0,
    };

    let near = mob_red(&gpu, NEAR_Z, fog);
    let far = mob_red(&gpu, FAR_Z, fog);
    let delta = far - near;

    // The discriminating control: same fog, same depth twice. Anything other
    // than exactly 0 means the two readings above cannot be attributed to depth.
    let control = mob_red(&gpu, NEAR_Z, fog) - near;

    println!("=== ENTITY FOG DEPTH GATE ===");
    println!("mob red @ z={NEAR_Z}  = {near:.1}");
    println!("mob red @ z={FAR_Z} = {far:.1}");
    println!("depth delta            = {delta:.1}");
    println!("equal-depth control    = {control:.1} (must be exactly 0)");
    println!("fogged prediction (fix) >= {MIN_FOGGED_DELTA:.1}");
    println!("unfogged prediction (bug) = {UNFOGGED_DELTA:.1}");
    println!(
        "negative control: the shipped entity shader contains no fog term — `grep fog \
         entity_pipeline.rs` returned nothing — so both depths render the identical colour and \
         this delta is exactly {UNFOGGED_DELTA:.1}."
    );

    assert_eq!(
        control, 0.0,
        "two renders at the same depth must be identical ({control:.3} apart); if they are not, \
         the depth reading below is measuring something other than depth"
    );
    assert!(
        delta >= MIN_FOGGED_DELTA,
        "a mob at z={FAR_Z} must be pulled at least {MIN_FOGGED_DELTA:.1} further toward the fog \
         colour than one at z={NEAR_Z}, got {delta:.1} (near {near:.1}, far {far:.1}); a delta \
         near {UNFOGGED_DELTA:.1} means the entity shader is ignoring the fog block, which is the \
         reported defect"
    );
    assert!(
        far > near,
        "the far mob must move *toward* the fog colour (red {FOG_COLOR:?}), not away: near \
         {near:.1}, far {far:.1}"
    );
}

/// Disabling fog must collapse the depth response to nothing — proof that the
/// delta above comes from the fog block and not from the mob's changing size or
/// sampling footprint at two distances.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn disabled_fog_leaves_the_two_depths_identical() {
    let Some(gpu) = setup() else {
        panic!("entity_fog_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a failure")
    };
    let near = mob_red(&gpu, NEAR_Z, FogSettings::disabled());
    let far = mob_red(&gpu, FAR_Z, FogSettings::disabled());

    println!("=== FOG-DISABLED CONTROL ===");
    println!("mob red @ z={NEAR_Z} = {near:.1}, @ z={FAR_Z} = {far:.1}");

    assert!(
        (far - near).abs() < 1.0,
        "with fog disabled the mob's colour must not depend on distance at all, but the two \
         depths read {near:.1} and {far:.1}; a difference here would mean the depth response in \
         `entity_fog_darkens_with_depth` is partly an artefact of distance rather than of fog"
    );
}

// ---------------------------------------------------------------- draw order

use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex, model_anim_buffer,
    model_shared_camera_buffer_with_fog, section_origin_buffer,
};

/// Where the water plane sits between the eye and the mob.
const WATER_Z: f32 = -1.0;

/// Predicted mob-pixel blue channel when the water surface is drawn **after**
/// the mob (correct, vanilla's order): the translucent blue quad alpha-blends
/// over the already-written mob, so the mob's pixels pick up the water's blue.
///
/// Predicted value when the mob is drawn **after** the water (the shipped bug):
/// the fluid pipeline has `depth_write_enabled: false`, so the water leaves
/// nothing in the depth buffer; the mob then passes the depth test and writes
/// **opaque** colour straight over the surface. Its pixels are then exactly the
/// no-water rendering — the mob is painted on top of the water however deep it
/// is. That is the reference the gate compares against, so "the mob changed at
/// all" is what is being asserted, and a wrong order reproduces the reference
/// byte-for-byte.
const MIN_WATER_BLUE_SHIFT: f32 = 20.0;

/// Build a single axis-aligned quad facing the camera at `z`, spanning `half`
/// blocks either side of the origin, with the given packed light and tint slot.
fn plane_mesh(z: f32, half: f32, y_centre: f32, tint: u8) -> ModelMesh {
    let v = |x: f32, y: f32| ModelVertex {
        position: [x, y, z],
        uv: [0.5, 0.5],
        ao: 1.0,
        light: 15 << 4,
        tint,
        anim: 0,
        cutout_bypass: 0,
        tint_rgb_override: [0, 0, 0, 0],
    };
    ModelMesh {
        vertices: vec![
            v(-half, y_centre - half),
            v(half, y_centre - half),
            v(half, y_centre + half),
            v(-half, y_centre + half),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// Render the mob and a translucent water plane in front of it, in the given
/// order, and return the mean **blue** channel over the mob's screen region.
///
/// `entities_first` is the whole experiment: everything else is identical.
fn mob_blue_with_water(gpu: &Gpu, entities_first: bool, with_water: bool) -> f32 {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = camera_at(NEAR_Z);
    let fog = FogUniform::disabled();

    // --- the mob (opaque, depth write on) -----------------------------------
    let def = zombie_model();
    let mesh = EntityMesh::from_named_model("zombie", &def);
    let inst = EntityInstance::new("zombie", &mesh, Vec3::ZERO, 90.0, 1.0, &AnimInput::REST)
        .with_light(ENTITY_FULLBRIGHT);
    let frame = plan_entities(std::slice::from_ref(&inst), &camera.frustum());
    assert_eq!(frame.instance_count(), 1, "the mob must be on screen");

    let ent_pipeline = EntityPipeline::new(device, FORMAT);
    let (tex, sampler) = upload_grey_sheet(device, queue);
    let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let ent_cam = ent_pipeline.camera_buffer_with_fog(device, &camera, fog);
    let ent_cam_bg = ent_pipeline.camera_bind_group(device, &ent_cam);
    let ent_tex_bg = ent_pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_mesh = GpuEntityModel::upload(device, &mesh).expect("non-empty mesh");

    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_mesh.parts.iter().zip(&batch.parts) {
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

    // --- the water plane (the real fluid pipeline: alpha blend, no depth write)
    let fluid = ModelPipeline::for_fluid(device, FORMAT);
    // A half-transparent white texel; the shader's own water tint (#3F76E4)
    // supplies the colour, exactly as it does for real water.
    let water_atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[255, 255, 255, 140].repeat(16), &[]);
    let water_atlas_bg = fluid.atlas_bind_group(device, &water_atlas);
    let water_anim = model_anim_buffer(device, &[]);
    let water_anim_bg = fluid.anim_bind_group(device, &water_anim);
    let water_cam = model_shared_camera_buffer_with_fog(
        device,
        camera.view_projection().to_cols_array_2d(),
        fog,
    );
    let water_origin = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let water_cam_bg = fluid.camera_bind_group(device, &water_cam, &water_origin);
    // tint != 255 so the shader applies its water colour.
    let water_mesh = plane_mesh(WATER_Z, 4.0, 0.9, 0);
    let water_gpu = GpuModelMesh::upload(device, &water_mesh).expect("non-empty water");

    let (color, color_view) = color_target(device);
    let depth = DepthBuffer::new(device, W, H);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("draw order pass"),
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

        let draw_mob = |pass: &mut wgpu::RenderPass| {
            pass.set_pipeline(&ent_pipeline.pipeline);
            pass.set_bind_group(0, &ent_cam_bg, &[]);
            pass.set_bind_group(1, &ent_tex_bg, &[]);
            for (count, range, buf) in &per_part {
                pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
                pass.set_vertex_buffer(1, buf.slice(..));
                pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(range.clone(), 0, 0..*count);
            }
        };
        let draw_water = |pass: &mut wgpu::RenderPass| {
            if !with_water {
                return;
            }
            pass.set_pipeline(&fluid.pipeline);
            pass.set_bind_group(0, &water_cam_bg, &[0]);
            pass.set_bind_group(1, &water_atlas_bg, &[]);
            pass.set_bind_group(2, &water_anim_bg, &[]);
            pass.set_vertex_buffer(0, water_gpu.vertices.slice(..));
            pass.set_index_buffer(water_gpu.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..water_gpu.index_count, 0, 0..1);
        };

        if entities_first {
            draw_mob(&mut pass);
            draw_water(&mut pass);
        } else {
            draw_water(&mut pass);
            draw_mob(&mut pass);
        }
    }
    let out = readback(device, queue, enc, &color);

    // Measure over the *mob's* region only. The mob is opaque grey in the
    // no-water reference, so its pixels are exactly those brighter than the
    // water plane alone — take the reference silhouette from a water-free
    // render at the same framing by thresholding on a high total.
    let (mut sum, mut n) = (0u64, 0u32);
    for i in 0..(W * H) as usize {
        let px = &out[i * 4..i * 4 + 3];
        // The mob is grey ~64..160 per channel; the bare water plane over black
        // is much darker. A high red channel therefore selects mob pixels, and
        // red is the channel the water tint moves *least*, so using red to
        // select and blue to measure keeps the two independent.
        if px[0] > 70 {
            sum += u64::from(px[2]);
            n += 1;
        }
    }
    assert!(
        n > 1500,
        "only {n} mob pixels selected (entities_first={entities_first}, water={with_water}) — \
         the reading would be a sliver"
    );
    sum as f32 / n as f32
}

/// **Cause 2, and it is not the fog term.** A translucent water surface between
/// the eye and a mob must actually cover the mob.
///
/// # Both predictions, and why a naive assertion would pass under the bug
///
/// The fluid pipeline runs with `depth_write_enabled: false`. Drawing the mob
/// after it therefore reproduces the **no-water render exactly**: the mob passes
/// the depth test (nothing wrote depth in front of it) and writes opaque colour
/// over the surface. So:
///
/// * wrong order (water, then mob) → mob blue == the no-water reference, shift 0
/// * right order (mob, then water) → mob blue shifted toward the water colour by
///   at least [`MIN_WATER_BLUE_SHIFT`]
///
/// Asserting merely "the two orders differ" would be weak; asserting "the mob
/// got bluer" would still pass if the mob were *tinted* by something else. The
/// discriminating shape is the no-water reference: the buggy order must equal it
/// bit for bit, and the correct order must not.
#[test]
#[ignore = "requires a GPU adapter; run explicitly — the wrong order is the built-in control"]
fn water_surface_covers_a_mob_behind_it() {
    let Some(gpu) = setup() else {
        panic!("entity_fog_pixels: no GPU adapter; this test is #[ignore]d so a missing one is a failure")
    };

    let no_water = mob_blue_with_water(&gpu, true, false);
    let correct = mob_blue_with_water(&gpu, true, true); // mob, then water
    let buggy = mob_blue_with_water(&gpu, false, true); // water, then mob

    println!("=== ENTITY vs WATER DRAW ORDER ===");
    println!("mob blue, no water at all          = {no_water:.1}");
    println!("mob blue, entities THEN water (fix)= {correct:.1}");
    println!("mob blue, water THEN entities (bug)= {buggy:.1}");
    println!("required shift from the reference  >= {MIN_WATER_BLUE_SHIFT:.1}");
    println!(
        "negative control (run above, not described): drawing the mob after the water — the \
         shipped order — reproduces the no-water reference exactly, because the fluid pipeline \
         writes no depth and the opaque mob simply overwrites the surface."
    );

    assert_eq!(
        buggy, no_water,
        "the shipped order must reproduce the no-water render exactly ({buggy:.3} vs \
         {no_water:.3}); if it does not, this test is not measuring what it claims and the \
         'fixed' reading below proves nothing"
    );
    assert!(
        correct - no_water >= MIN_WATER_BLUE_SHIFT,
        "with entities drawn before the translucent pass, the water surface must blend over the \
         mob and move it at least {MIN_WATER_BLUE_SHIFT:.1} toward the water colour, got \
         {:.1} (reference {no_water:.1}, with water {correct:.1})",
        correct - no_water
    );
}
