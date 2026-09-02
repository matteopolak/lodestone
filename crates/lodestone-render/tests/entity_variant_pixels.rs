//! Prove the two *pose and identity* entity defects reach pixels.
//!
//! [`entity_anim_pixels`](./entity_anim_pixels.rs) proves that changing
//! [`AnimInput`] changes the frame. It says nothing about **which mob** is drawn
//! or **what pose it rests in**, and both were wrong in the same build:
//!
//! 1. A **drowned rendered as an ordinary zombie**. Its mesh (`drowned_model`)
//!    and its sheet (`entity/zombie/drowned.png`) were both already in the
//!    corpus; a stale alias in `entity::canonical_model_name` routed the type
//!    path `drowned` to the zombie entry, so the wrong geometry *and* the wrong
//!    texture reached the GPU.
//! 2. A **zombie's arms hung at its sides**. Vanilla's
//!    `AnimationUtils.animateZombieArms` holds them out in front
//!    unconditionally; the port had only `HumanoidModel.setupAnim`.
//!
//! Both are silent under every mesh and matrix test that existed, because both
//! produce a perfectly well-formed mob — just the wrong one. Only pixels
//! separate "a drowned" from "a zombie".
//!
//! # The controls
//!
//! Every assertion here is paired with the **pre-fix build as its own negative
//! control**, which is the strongest available form: the arms-down rig is
//! exactly what `EntityMesh::from_model` still produces, and the zombie
//! mesh-plus-sheet is exactly what the old alias resolved to. A change that
//! reverted either fix would drive the corresponding reading back to the
//! control's value. On top of that, each test renders one configuration twice
//! and requires **zero** differing pixels, so a non-deterministic pipeline or a
//! drifting camera cannot masquerade as a fix.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is an explicit opt-in; once opted in, a missing
//! adapter or a missing `client.jar` is a **failure**, never a skip.

mod gate_harness;

use glam::Vec3;
use lodestone_assets::entity_models::zombie_model;
use lodestone_assets::{Image, ResourceManager, ResourceSource, ZipSource};
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{
    EntityInstance, EntityMesh, EntityModelSet, entity_texture_candidates, plan_entities,
};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};

const W: u32 = 256;
const H: u32 = 256;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Turn the mob broadside to the camera, as `entity_anim_pixels` does. The
/// zombie arm pose points along the mob's *facing*, so side-on it is horizontal
/// screen motion; head-on it would be almost pure depth and the silhouette would
/// barely change.
const BODY_YAW: f32 = 90.0;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
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
                label: Some("entity_variant_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// The real 26.2 `client.jar`, opened once. Fails closed: this test is
/// `#[ignore]`d, so a missing jar is an environment failure, not a skip.
fn jar() -> ResourceManager {
    let path = gate_harness::require_client_jar();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let zip = ZipSource::from_bytes(bytes).unwrap_or_else(|e| panic!("open jar: {e}"));
    ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>])
}

/// Decode a vanilla entity sheet out of the jar.
fn sheet(jar: &ResourceManager, path: &str) -> Image {
    let png = jar
        .read(path)
        .unwrap_or_else(|| panic!("{path} missing from client.jar"));
    Image::decode_png(&png).unwrap_or_else(|e| panic!("decode {path}: {e}"))
}

/// Resolve an entity **type path** the way the shell does — through
/// [`EntityModelSet::resolve`] for the mesh and [`entity_texture_candidates`]
/// for the sheet — and return the model name it landed on plus its decoded
/// texture.
///
/// Going through the public seam rather than naming `drowned_model()` directly
/// is what puts `canonical_model_name` **inside** the gate. The defect was an
/// alias in that function; a test that reached past it into the corpus would
/// have stayed green for the entire time the bug was on screen.
fn resolve_mob<'a>(
    set: &'a EntityModelSet,
    jar: &ResourceManager,
    type_path: &str,
) -> (&'static str, &'a EntityMesh, Image) {
    let inst = set
        .resolve(type_path, Vec3::ZERO, BODY_YAW, 1.0, &AnimInput::REST)
        .unwrap_or_else(|| panic!("{type_path} must resolve to a model"));
    let mesh = set.get(inst.model).expect("resolved model is in the set");
    let path = entity_texture_candidates(inst.model)
        .first()
        .unwrap_or_else(|| panic!("{} has no texture candidate", inst.model));
    (inst.model, mesh, sheet(jar, path))
}

/// A flat opaque magenta sheet: every mob texel is one colour, so a differing
/// pixel can only mean the *silhouette* moved, never that a UV shifted. Used by
/// the arm-pose test, where the question is purely about shape.
fn flat_sheet() -> Image {
    const N: u32 = 64;
    Image {
        width: N,
        height: N,
        rgba: (0..N * N).flat_map(|_| [230u8, 30, 200, 255]).collect(),
    }
}

fn upload_sheet(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &Image,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("variant-sheet"),
        size: wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &img.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(img.width * 4),
            rows_per_image: Some(img.height),
        },
        wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("variant-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// The same framing `entity_anim_pixels` uses, which is known to sit a humanoid
/// in the middle of the frame.
fn side_camera() -> Camera {
    Camera {
        position: Vec3::new(0.0, 0.9, -2.2),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// Render one mob — a specific mesh with a specific sheet — and return the RGBA
/// frame, row-major, tightly packed.
///
/// Mesh and sheet are supplied separately on purpose. The drowned defect had two
/// halves (wrong geometry, wrong texture) and a test that could only vary them
/// together could not say which half it had proved.
fn render_mob(gpu: &Gpu, mesh: &EntityMesh, img: &Image) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = side_camera();

    let inst = EntityInstance::new(
        "mob",
        mesh,
        Vec3::ZERO,
        BODY_YAW,
        1.0,
        &AnimInput::REST,
    );
    let frame = plan_entities(std::slice::from_ref(&inst), &camera.frustum());
    assert_eq!(
        frame.instance_count(),
        1,
        "the mob was culled — this gate measures what is drawn, so it must be on screen"
    );

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = upload_sheet(device, queue, img);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_mesh = GpuEntityModel::upload(device, mesh).expect("mesh is non-empty");

    // One instance buffer per part: vertices are part-local, so each part must be
    // drawn against its own matrices.
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
    assert!(
        !per_part.is_empty(),
        "no part produced an instance buffer — nothing would be drawn"
    );

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("variant-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("variant-pass"),
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

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("variant-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
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
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
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
    readback.unmap();
    out
}

/// Pixels that differ between two frames, anywhere.
fn differing(a: &[u8], b: &[u8]) -> u32 {
    (0..(W * H) as usize)
        .filter(|i| a[i * 4..i * 4 + 3] != b[i * 4..i * 4 + 3])
        .count() as u32
}

/// Is this pixel the sky? Anything else in the frame is the mob, which lets the
/// silhouette be measured without knowing what colour the sheet painted it —
/// necessary here, because two of these frames use real multi-hued jar sheets.
fn is_mob(frame: &[u8], i: usize) -> bool {
    let clear = [
        (CLEAR.r * 255.0).round() as u8,
        (CLEAR.g * 255.0).round() as u8,
        (CLEAR.b * 255.0).round() as u8,
    ];
    frame[i..i + 3]
        .iter()
        .zip(clear)
        .any(|(got, want)| got.abs_diff(want) > 8)
}

/// The mob silhouette's `(min_x, max_x, area)` in pixels.
fn silhouette(frame: &[u8]) -> (u32, u32, u32) {
    let (mut lo, mut hi, mut area) = (W, 0u32, 0u32);
    for y in 0..H {
        for x in 0..W {
            if is_mob(frame, ((y * W + x) * 4) as usize) {
                lo = lo.min(x);
                hi = hi.max(x);
                area += 1;
            }
        }
    }
    assert!(
        area > 3000,
        "only {area} px of mob found — the silhouette readings below would be slivers"
    );
    (lo, hi, area)
}

/// **Defect 1.** A drowned must render as a drowned: its own mesh *and* its own
/// sheet, both of which the corpus already had.
#[test]
#[ignore = "requires a GPU adapter and .cache/mc/26.2/client.jar; run explicitly"]
fn a_drowned_renders_as_a_drowned_not_as_a_zombie() {
    let Some(gpu) = setup() else {
        panic!(
            "entity_variant_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let jar = jar();
    let set = EntityModelSet::load();

    // Both resolved through the same seam the shell uses. Under the stale alias
    // these two calls returned the *same* model and the same sheet.
    let (drowned_name, drowned_mesh, drowned_png) = resolve_mob(&set, &jar, "drowned");
    let (zombie_name, zombie_mesh, zombie_png) = resolve_mob(&set, &jar, "zombie");
    assert_ne!(
        drowned_name, zombie_name,
        "the type path `drowned` resolved to the {zombie_name} model — the alias is back, and \
         every pixel reading below would be comparing a zombie with itself"
    );

    // What the player should see, and what the stale alias made them see.
    let fixed = render_mob(&gpu, drowned_mesh, &drowned_png);
    let regressed = render_mob(&gpu, zombie_mesh, &zombie_png);
    // The mesh half on its own: same sheet, different geometry. `drowned_model`
    // gives the left arm and left leg their own tex_offs instead of mirroring
    // the right, so the *UVs* differ even where the silhouette does not.
    let mesh_only = render_mob(&gpu, drowned_mesh, &zombie_png);
    // The control: the identical configuration rendered twice.
    let control = render_mob(&gpu, drowned_mesh, &drowned_png);

    let (_, _, area) = silhouette(&fixed);
    let whole = differing(&fixed, &regressed);
    let mesh_half = differing(&mesh_only, &regressed);
    let repeat = differing(&fixed, &control);

    println!("=== DROWNED IDENTITY PIXEL GATE ===");
    println!("resolved model          : {drowned_name} (zombie resolves to {zombie_name})");
    println!("mob silhouette          : {area} px");
    println!("drowned vs zombie       : {whole} px differ");
    println!("mesh half (same sheet)  : {mesh_half} px differ");
    println!("control (drowned x2)    : {repeat} px differ  (must be 0)");

    assert_eq!(
        repeat, 0,
        "two renders of the same mob differ by {repeat} px — the pipeline is not deterministic, \
         so the readings above prove nothing"
    );
    assert!(
        whole > area / 4,
        "a drowned and a zombie differ in only {whole} of {area} mob pixels. They are supposed to \
         be visibly different mobs; this is the reported defect"
    );
    assert!(
        mesh_half >= 200,
        "with the same sheet on both, the drowned mesh differs from the zombie mesh by only \
         {mesh_half} px. `drowned_model`'s un-mirrored left arm and leg sample a different part \
         of the sheet, so this must be non-trivial — otherwise only the texture lookup was fixed \
         and the mesh alias is still live"
    );
}

/// **Defect 2.** A zombie holds its arms out in front. The negative control is
/// the pre-fix rig, which `EntityMesh::from_model` still builds.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the zombie arm pose reaches pixels"]
fn a_zombie_holds_its_arms_out_in_front() {
    let Some(gpu) = setup() else {
        panic!(
            "entity_variant_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let flat = flat_sheet();
    // `from_named_model` picks the `AbstractZombieModel` rig; `from_model` is the
    // plain `HumanoidModel` one, i.e. exactly the arms-down build being fixed.
    let arms_out = EntityMesh::from_named_model("zombie", &zombie_model());
    let arms_down = EntityMesh::from_model(&zombie_model());

    let posed = render_mob(&gpu, &arms_out, &flat);
    let control_rig = render_mob(&gpu, &arms_down, &flat);
    let repeat = render_mob(&gpu, &arms_out, &flat);

    let (posed_lo, posed_hi, posed_area) = silhouette(&posed);
    let (down_lo, down_hi, down_area) = silhouette(&control_rig);
    let posed_width = posed_hi - posed_lo;
    let down_width = down_hi - down_lo;
    let determinism = differing(&posed, &repeat);

    println!("=== ZOMBIE ARM POSE PIXEL GATE ===");
    println!("arms out : width {posed_width} px, area {posed_area} px");
    println!("arms down: width {down_width} px, area {down_area} px  (the pre-fix build)");
    println!("control (arms-out x2) : {determinism} px differ  (must be 0)");

    assert_eq!(
        determinism, 0,
        "two renders of the same rig differ by {determinism} px — the pipeline is not \
         deterministic, so the widths above prove nothing"
    );
    // Side-on, the mob's facing is horizontal on screen, so arms held out in
    // front widen the silhouette. The arm cube reaches ~0.63 blocks past the
    // pivot; at this framing a block is ~100 px, and the arms-down torso is only
    // ~0.56 blocks deep, so the widening is large and unambiguous.
    assert!(
        posed_width > down_width + 20,
        "the zombie's silhouette is {posed_width} px wide against the arms-down build's \
         {down_width} px. Arms held out in front must extend the silhouette forward; this reads \
         as arms still hanging at its sides"
    );
    // ...and it must be the *arms* moving, not the whole mob getting bigger:
    // rotating two limbs cannot change how many texels the mesh has.
    assert!(
        posed_area < down_area * 3 / 2,
        "the posed mob covers {posed_area} px against {down_area} — that is a scale change, not \
         an arm rotation"
    );
}
