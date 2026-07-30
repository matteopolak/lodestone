//! Prove that a **thrown projectile** and a **first-person held item** each reach
//! pixels.
//!
//! ## What was actually broken, and what was not
//!
//! Four issues were filed against one suspected root cause — "the sprite icon
//! stream is collected but never exposed, so flat items are invisible as drops, in
//! the hand, in containers, and as projectiles". That diagnosis was **stale in
//! three of the four**:
//!
//! * the sprite stream is collected *and* baked *and* inserted into the very same
//!   `BlockModels::items` map the 3-D items use (`block_models.rs`'s
//!   `extruded_sprite_geometry` loop), so `items()` has yielded flat items since
//!   `9980a96`, and `sprite_drop_pixels` already passes;
//! * the first-person hand had every piece **except** a way to learn what the
//!   local player was holding — `RenderState::render` takes only `&[EntityDraw]`,
//!   which never contains the local player;
//! * no projectile had a renderer of any kind.
//!
//! So this gate covers the two consumers that genuinely had no path to a pixel,
//! and deliberately does **not** re-test the sprite stream: `sprite_drop_pixels`
//! owns that, and duplicating it here would make a stream regression look like two
//! independent failures.
//!
//! ## The projectile assertions, and why each is not vacuous
//!
//! 1. **A silhouette exists** inside the projectile's own projected bounding box.
//! 2. **The billboard is what puts it there.** The negative control renders the
//!    *identical* mesh through the *identical* pass with
//!    [`glam::Mat4::IDENTITY`] in place of [`camera_orientation`], viewed from a
//!    camera 90° off the slab's own facing. A 1/16-block-thick slab seen edge-on
//!    is a sliver, so the control must draw an order of magnitude fewer pixels —
//!    and it is *executed*, not described. Without it, "the projectile drew" is
//!    equally satisfied by a renderer that ignores the camera entirely and happens
//!    to be pointed the right way in the one scene the gate builds.
//! 3. **It is a cutout, not a box.** Strictly fewer lit pixels than its bounding
//!    box, because a sprite has transparent corners. A ">0 pixels" assertion
//!    passes when the alpha discard or the UVs are wrong; this band does not.
//! 4. **It is localised.** The opposite corner of the frame is untouched, so a
//!    full-screen wash cannot pass as a snowball.
//! 5. **It stays upright**, checked on the matrix rather than the frame:
//!    [`camera_orientation`] must map item-local `+Y` to world `+Y` for a level
//!    camera at any yaw. An upside-down billboard is invisible to a pixel count
//!    and, for a near-symmetric sprite like a snowball, nearly invisible to a row
//!    profile too.
//!
//! ## The held-item assertions
//!
//! 1. **The item lands in the bottom-right quadrant** of the hand projection,
//!    where vanilla's `applyItemArmTransform` puts it (`+0.56` right, `-0.52`
//!    down, `-0.72` forward of the eye). A chain with a sign error still "draws
//!    something"; only a quadrant floor separates "the item is in hand" from "the
//!    item is somewhere".
//! 2. **The winding is positive**, derived from a real camera rather than
//!    asserted: `hand_projection` is built by the same constructor
//!    `Camera::projection_matrix` uses, and a view matrix has `det = +1`, so the
//!    hand pose obeys the *world* rule (positive) and not the GUI one (negative).
//!    Coding to the GUI rule here ships an inside-out item that still looks
//!    plausible in a screenshot.
//! 3. **A handedness-flipped pose does not reproduce the frame** — the executed
//!    control for assertion 2.
//! 4. **`attack_anim = 0` is exactly the rest pose**, because
//!    `applyItemArmAttackTransform`'s leading `Ry(i·45°)` is cancelled by its
//!    trailing `Ry(i·-45°)` only when both shaping terms vanish. Dropping either
//!    rotation leaves a permanent 45° twist that no swing test would notice.
//!
//! `#[ignore]`d and **fail-closed**: needs a GPU adapter *and* a fetched
//! `client.jar`. Once opted in, a missing adapter is a failure, never a skip:
//!
//! ```text
//! cargo test -p lodestone-render --test thrown_and_held_item_pixels -- --ignored --nocapture
//! ```

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_render::entity::{
    Arm, camera_orientation, first_person_item_chain, first_person_item_matrix,
    first_person_item_mesh, ground_transform, hand_projection, hand_transform, thrown_item_for,
    thrown_item_mesh,
};
use lodestone_render::{
    BlockModels, Camera, CameraUniform, GpuAtlas, GpuModelMesh, ItemGeometry, ModelMesh,
    ModelPipeline, blocks_json_registry, model_anim_buffer, model_camera_buffer,
    model_palette_buffer,
};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// A **wide** frame, and that is load-bearing rather than cosmetic.
///
/// `applyItemArmTransform` puts the held item `0.56` blocks right of the eye and
/// `0.72` forward, so at `hand_projection`'s fixed 70° vertical FOV it sits outside
/// a *square* viewport's right edge entirely: measured, a 256×256 target draws
/// **zero** item pixels while a 1.5-aspect one draws 2722 and a 16:9 one 4191.
/// Nothing about the pose is wrong in that square frame — vanilla's window is never
/// square, and its `hudFov` is vertical, so the horizontal half-angle grows with
/// aspect. A gate on a square target would have read as "the held item does not
/// render" and sent the next reader hunting a nonexistent chain bug.
const W: u32 = 448;
const H: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The projectile under test. `minecraft:snowball` is registered to
/// `ThrownItemRenderer` at scale `1.0`, and its icon is `item/snowball` through
/// `builtin/generated` — i.e. the flat-sprite path, which is the whole reason a
/// projectile could not have drawn before the extrusion landed.
const PROJECTILE_TYPE: &str = "snowball";

/// The held item under test: a flat sprite again, and the item a player actually
/// holds. A block item would exercise a *different* `display` chain
/// (`block/block`'s `firstperson_righthand` versus `item/handheld`'s), so the two
/// are not interchangeable — see the note on `hand_transform`'s `first_person`
/// flag.
const HELD_ITEM: &str = "minecraft:diamond_pickaxe";

/// The projectile's world position, on a block centre.
const AT: glam::Vec3 = glam::Vec3::new(0.5, 1.0, 0.5);

/// Camera distance from the projectile.
const CAM_DISTANCE: f32 = 1.0;

/// Full daylight, so the shader's `0.2 + 0.8 * max(sky, block)` is `1.0` and a
/// dark frame cannot be mistaken for an empty one.
const LIGHT: u8 = 0xF0;

/// A clear colour no item texel can coincide with; fully saturated magenta is
/// absent from the vanilla item atlas.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

/// Minimum billboard silhouette, in pixels. Far above a handful of stray
/// fragments and far below the ~2500 a 1.0-scale snowball covers at this
/// distance.
const MIN_SILHOUETTE_PX: usize = 250;

/// The edge-on control must draw at most this fraction of the subject's area.
///
/// **Measured, not guessed**: 494 against the subject's 3788, i.e. 13%. Not the
/// ~6% a 1/16-thick slab's *face-to-edge* ratio predicts, because
/// `ItemModelGenerator` fans one edge quad per boundary texel of the sprite's alpha
/// outline — seen side-on those quads are the widest thing left, so the sliver is
/// twice as bright as the naive ratio. A 10% ceiling failed on a working build; 20%
/// keeps a 7.7x separation while leaving the control able to fail.
const MAX_CONTROL_FRACTION: f32 = 0.20;

/// The silhouette must leave at least this fraction of its own bounding box
/// unlit — a sprite is a cutout, not a box.
const MIN_BOX_SLACK: f32 = 0.05;

/// Minimum share of the bottom-right quadrant the held item must cover.
const MIN_HAND_QUADRANT_FRACTION: f32 = 0.01;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn setup() -> Gpu {
    let gpu = pollster::block_on(async {
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
                label: Some("thrown_and_held_item_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    });
    gpu.expect("thrown_and_held_item_pixels: no GPU adapter, and this gate must not skip")
}

fn build_models() -> BlockModels {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

fn geometry_of<'a>(models: &'a BlockModels, item: &str) -> &'a ItemGeometry {
    let id = item.parse().expect("valid resource location");
    models.item(&id).unwrap_or_else(|| {
        panic!(
            "{item} has no baked geometry. For a flat `builtin/generated` icon this is the \
             `extruded_sprite_geometry` loop in `BlockModels::build` having failed to stitch its \
             layers, not a consumer bug — see `sprite_drop_pixels`"
        )
    })
}

/// A camera looking at `AT` from `yaw`, level.
///
/// `yaw = 180` puts the camera on `+Z` looking down `-Z`; `yaw = 90` puts it on
/// `+X` looking down `-X`, which is the 90°-off view the edge-on control needs.
fn camera(yaw: f32) -> Camera {
    // Forward is Minecraft's convention: `(-sin y, 0, cos y)` at zero pitch. The
    // eye therefore sits at `AT - CAM_DISTANCE * forward`.
    let f = glam::Vec3::new(-yaw.to_radians().sin(), 0.0, yaw.to_radians().cos());
    Camera {
        position: AT - f * CAM_DISTANCE,
        yaw,
        pitch: 0.0,
        aspect: W as f32 / H as f32,
        ..Camera::default()
    }
}

/// Render one already-posed mesh through the real [`ModelPipeline`] against the
/// real stitched atlas, with `view_proj` supplied by the caller so both the world
/// pass (a projectile) and the hand pass (a held item, whose `view_proj` is
/// `hand_projection` **alone**) go through one implementation.
#[allow(clippy::too_many_lines)]
fn render(gpu: &Gpu, models: &BlockModels, mesh: &ModelMesh, view_proj: glam::Mat4) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_atlas(device, queue, models.atlas());
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);
    let palette_buffer = model_palette_buffer(device, models.tint_palette());
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &models.anim_slot_uniforms(0));
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    let cam_buffer = model_camera_buffer(
        device,
        CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer);

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("thrown/held target"),
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

    let gpu_mesh = GpuModelMesh::upload(device, mesh);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("thrown/held item pixels"),
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
        // An empty mesh still clears the target, so a control that legitimately
        // meshes nothing goes through the identical path.
        if let Some(gpu_mesh) = &gpu_mesh {
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &cam_bg, &[]);
            pass.set_bind_group(1, &atlas_bg, &[]);
            pass.set_bind_group(2, &palette_bg, &[]);
            pass.set_bind_group(3, &anim_bg, &[]);
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
        }
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

    let mut out = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            let i = (y * padded + x * 4) as usize;
            out.extend_from_slice(&[data[i], data[i + 1], data[i + 2]]);
        }
    }
    out
}

/// Whether pixel `i` was written by geometry rather than left at the clear.
fn lit(frame: &[u8], i: usize) -> bool {
    let px = (frame[i * 3], frame[i * 3 + 1], frame[i * 3 + 2]);
    px != (255, 0, 255)
}

fn lit_count(frame: &[u8]) -> usize {
    (0..(W * H) as usize).filter(|&i| lit(frame, i)).count()
}

/// Lit pixels whose `(x, y)` falls in `[x0, x1) × [y0, y1)`.
fn lit_in(frame: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if lit(frame, (y * W + x) as usize) {
                n += 1;
            }
        }
    }
    n
}

/// The mesh's projected screen bounding box in pixels, clamped to the frame.
fn projected_box(mesh: &ModelMesh, view_proj: glam::Mat4) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for v in &mesh.vertices {
        let p = view_proj.project_point3(glam::Vec3::from(v.position));
        let sx = (p.x * 0.5 + 0.5) * W as f32;
        let sy = (0.5 - p.y * 0.5) * H as f32;
        x0 = x0.min(sx);
        y0 = y0.min(sy);
        x1 = x1.max(sx);
        y1 = y1.max(sy);
    }
    (
        x0.floor().clamp(0.0, W as f32) as u32,
        y0.floor().clamp(0.0, H as f32) as u32,
        x1.ceil().clamp(0.0, W as f32) as u32,
        y1.ceil().clamp(0.0, H as f32) as u32,
    )
}

/// Re-pose an already-posed mesh's vertices, so a control variant goes through
/// the identical upload and draw.
fn repose(mesh: &ModelMesh, extra: glam::Mat4) -> ModelMesh {
    let mut out = mesh.clone();
    for v in &mut out.vertices {
        v.position = extra.transform_point3(glam::Vec3::from(v.position)).into();
    }
    out
}

// ---------------------------------------------------------------------------
// Thrown projectiles
// ---------------------------------------------------------------------------

/// The whole registration table, checked against the 26.2 source rather than
/// against itself. Hermetic — no GPU, no jar.
#[test]
fn the_thrown_item_table_matches_the_26_2_registrations() {
    // `EntityRenderers.java` registers exactly nine types to `ThrownItemRenderer`.
    for (type_path, item, scale, full_bright) in [
        ("egg", "minecraft:egg", 1.0, false),
        ("ender_pearl", "minecraft:ender_pearl", 1.0, false),
        (
            "experience_bottle",
            "minecraft:experience_bottle",
            1.0,
            false,
        ),
        // `EyeOfEnder.getDefaultItem()` is `Items.ENDER_EYE` — the item id differs
        // from the entity type name, which is the one entry a name-derived table
        // would get wrong.
        ("eye_of_ender", "minecraft:ender_eye", 1.0, true),
        ("fireball", "minecraft:fire_charge", 3.0, true),
        ("lingering_potion", "minecraft:lingering_potion", 1.0, false),
        ("small_fireball", "minecraft:fire_charge", 0.75, true),
        ("snowball", "minecraft:snowball", 1.0, false),
        ("splash_potion", "minecraft:splash_potion", 1.0, false),
    ] {
        let got = thrown_item_for(type_path)
            .unwrap_or_else(|| panic!("{type_path} is registered to ThrownItemRenderer in 26.2"));
        assert_eq!(got.item, item, "{type_path} default item");
        assert!(
            (got.scale - scale).abs() < 1e-6,
            "{type_path} scale: got {}, want {scale}",
            got.scale
        );
        assert_eq!(got.full_bright, full_bright, "{type_path} fullBright");
    }

    // The three most commonly assumed members that are **not** in the list. Each
    // has a real cuboid renderer in 26.2, so billboarding an item icon for them
    // would draw the wrong thing rather than nothing.
    for absent in [
        "wind_charge",
        "breeze_wind_charge",
        "arrow",
        "spectral_arrow",
        "trident",
        "dragon_fireball",
        "wither_skull",
        "shulker_bullet",
        "llama_spit",
        "firework_rocket",
        "item",
        "pig",
    ] {
        assert!(
            thrown_item_for(absent).is_none(),
            "{absent} is not a ThrownItemRenderer type in 26.2"
        );
    }
}

/// [`camera_orientation`] must keep the billboard upright for a level camera at
/// every yaw, and must be a pure rotation (`det = +1`) so the composed world pose
/// keeps terrain's winding.
///
/// This is the assertion a pixel count cannot make: an upside-down snowball covers
/// exactly as many pixels as an upright one.
#[test]
fn the_billboard_is_a_pure_rotation_that_stays_upright() {
    for yaw in [0.0_f32, 45.0, 90.0, 180.0, 270.0, -37.5] {
        let cam = camera(yaw);
        let orientation = camera_orientation(cam.view_matrix());
        assert!(
            (orientation.determinant() - 1.0).abs() < 1e-4,
            "yaw {yaw}: camera_orientation must be a pure rotation, det = {}",
            orientation.determinant()
        );
        let up = orientation.transform_vector3(glam::Vec3::Y);
        assert!(
            (up - glam::Vec3::Y).length() < 1e-4,
            "yaw {yaw}: a level camera must leave the billboard upright, up = {up:?}"
        );
        // And the item's own front (`+Z` in model space, the SOUTH face) must end
        // up pointing back at the eye, not away from it.
        let front = orientation.transform_vector3(glam::Vec3::Z);
        let to_eye = (cam.position - AT).normalize();
        assert!(
            front.dot(to_eye) > 0.99,
            "yaw {yaw}: the billboard's front must face the eye; front = {front:?}, \
             to_eye = {to_eye:?}"
        );
    }
}

/// The pixel gate: a thrown snowball draws a localised cutout silhouette, and the
/// un-billboarded control seen from the same 90°-off camera does not.
#[test]
#[ignore = "requires a GPU adapter and a fetched client.jar"]
fn a_thrown_snowball_draws_a_silhouette_and_the_edge_on_control_does_not() {
    let gpu = setup();
    let models = build_models();
    let thrown = thrown_item_for(PROJECTILE_TYPE).expect("snowball is a thrown item");
    let geometry = geometry_of(&models, thrown.item);
    let ground = ground_transform(&geometry.display, geometry.gui_light);

    // Viewed from `+X`, 90° off the slab's own `+Z` facing: the pose the drop path
    // would use is edge-on here, and only the billboard rotation saves it.
    let cam = camera(90.0);
    let orientation = camera_orientation(cam.view_matrix());
    let vp = cam.view_projection();

    let subject_mesh = thrown_item_mesh(
        &geometry.quads,
        geometry.gui_light,
        &ground,
        AT,
        orientation,
        thrown.scale,
        LIGHT,
    );
    let subject = render(&gpu, &models, &subject_mesh, vp);
    let subject_px = lit_count(&subject);

    // The control: identical everything, orientation replaced by the identity.
    let control_mesh = thrown_item_mesh(
        &geometry.quads,
        geometry.gui_light,
        &ground,
        AT,
        glam::Mat4::IDENTITY,
        thrown.scale,
        LIGHT,
    );
    let control = render(&gpu, &models, &control_mesh, vp);
    let control_px = lit_count(&control);

    let (x0, y0, x1, y1) = projected_box(&subject_mesh, vp);
    let box_area = ((x1 - x0) as usize) * ((y1 - y0) as usize);
    let in_box = lit_in(&subject, x0, y0, x1, y1);

    eprintln!("projected box   = ({x0},{y0})..({x1},{y1}), area {box_area}");
    eprintln!("subject lit     = {subject_px} ({in_box} inside the box)");
    eprintln!("control lit     = {control_px} (edge-on, no billboard)");

    assert!(
        subject_px >= MIN_SILHOUETTE_PX,
        "a thrown snowball drew {subject_px} pixels, below the {MIN_SILHOUETTE_PX} floor. \
         Zero means no projectile renderer ran at all"
    );
    assert_eq!(
        in_box,
        subject_px,
        "every lit pixel must be inside the projectile's own projected box; \
         {} strayed outside it",
        subject_px - in_box
    );
    assert!(
        in_box < box_area,
        "the silhouette filled its whole {box_area}px box, so it is a slab and not a \
         cutout — suspect the alpha discard or the UVs"
    );
    let slack = 1.0 - in_box as f32 / box_area as f32;
    assert!(
        slack >= MIN_BOX_SLACK,
        "only {:.1}% of the bounding box is unlit; a sprite cutout should leave at least \
         {:.0}%",
        slack * 100.0,
        MIN_BOX_SLACK * 100.0
    );
    // The executed negative control.
    #[allow(clippy::cast_sign_loss)]
    let ceiling = (subject_px as f32 * MAX_CONTROL_FRACTION) as usize;
    assert!(
        control_px <= ceiling,
        "the un-billboarded control drew {control_px} pixels (ceiling {ceiling}), so this \
         frame does not actually depend on the camera-facing rotation and the gate is \
         vacuous"
    );
    // And a far corner is untouched, so a full-screen wash cannot pass.
    assert_eq!(
        lit_in(&subject, 0, 0, 16, 16),
        0,
        "the top-left corner is lit; the frame is a wash, not a projectile"
    );
}

// ---------------------------------------------------------------------------
// The item in the first-person hand
// ---------------------------------------------------------------------------

/// `applyItemArmAttackTransform` must be exactly the identity at rest, so the
/// resting pose is independent of the swing. Hermetic.
#[test]
fn the_first_person_item_chain_is_a_plain_translation_at_rest() {
    let chain = first_person_item_chain(Arm::Right, 0.0, 0.0);
    let expected = glam::Mat4::from_translation(glam::Vec3::new(0.56, -0.52, -0.72));
    let diff = (chain - expected)
        .to_cols_array()
        .iter()
        .fold(0.0_f32, |m, v| m.max(v.abs()));
    assert!(
        diff < 1e-5,
        "at attack_anim = 0 the chain must reduce to applyItemArmTransform's translation \
         alone (the Ry(45)/Ry(-45) pair cancels); max element error {diff}"
    );
    // Mirrored for the off hand, and nothing else changes.
    let left = first_person_item_chain(Arm::Left, 0.0, 0.0);
    assert!((left.w_axis.x + 0.56).abs() < 1e-5, "left hand mirrors x");
    assert!((left.w_axis.y + 0.52).abs() < 1e-5, "left hand keeps y");
    assert!((left.w_axis.z + 0.72).abs() < 1e-5, "left hand keeps z");
}

/// The winding rule, derived from a real camera rather than asserted.
///
/// `hand_projection` uses the same constructor as `Camera::projection_matrix`, and
/// a view matrix is orthonormal-plus-translation (`det = +1`), so the hand pose
/// must have a **positive** determinant — the world rule, not the GUI one.
#[test]
fn the_first_person_item_pose_takes_the_world_winding_rule() {
    let cam = camera(180.0);
    let reference = cam.view_projection().determinant().signum();
    assert!(
        hand_projection(cam.aspect).determinant().signum() == reference,
        "hand_projection must share Camera::view_projection's determinant sign; deriving \
         it here rather than hardcoding is the whole point"
    );
    // A `display` transform with a positive uniform scale, which every real one
    // has; the pose's sign must not depend on the item.
    let transform = lodestone_assets::DisplayTransform {
        rotation: [0.0, 45.0, 0.0],
        translation: [0.0, 3.0, 1.0],
        scale: [0.68, 0.68, 0.68],
    };
    for &arm in &[Arm::Right, Arm::Left] {
        for attack in [0.0_f32, 0.25, 0.5, 0.9, 1.0] {
            let pose = first_person_item_matrix(arm, attack, 0.0, &transform);
            assert!(
                pose.determinant() > 0.0,
                "{arm:?} at attack {attack}: the hand pose must be right-handed \
                 (det = {}), or the item is drawn inside out",
                pose.determinant()
            );
            let composed = hand_projection(cam.aspect) * pose;
            assert_eq!(
                composed.determinant().signum(),
                reference,
                "{arm:?} at attack {attack}: the composed hand matrix must match the \
                 camera's sign"
            );
        }
    }
}

/// The pixel gate: the held item lands in the bottom-right quadrant of the hand
/// projection, and a handedness-flipped pose does not reproduce that frame.
#[test]
#[ignore = "requires a GPU adapter and a fetched client.jar"]
fn the_first_person_item_lands_in_the_bottom_right_of_frame() {
    let gpu = setup();
    let models = build_models();
    let geometry = geometry_of(&models, HELD_ITEM);
    // `true` — the *first-person* slot. `false` reads `thirdperson_righthand`,
    // which is a different rotation and scale and is the silent failure mode.
    let transform = hand_transform(&geometry.display, Arm::Right, true);
    let cam = camera(180.0);
    let vp = hand_projection(cam.aspect);

    let mesh = first_person_item_mesh(
        &geometry.quads,
        geometry.gui_light,
        Arm::Right,
        0.0,
        0.0,
        &transform,
        LIGHT,
    );
    let subject = render(&gpu, &models, &mesh, vp);
    let total = lit_count(&subject);

    let (hw, hh) = (W / 2, H / 2);
    let quadrants = [
        ("top-left", lit_in(&subject, 0, 0, hw, hh)),
        ("top-right", lit_in(&subject, hw, 0, W, hh)),
        ("bottom-left", lit_in(&subject, 0, hh, hw, H)),
        ("bottom-right", lit_in(&subject, hw, hh, W, H)),
    ];
    for (name, n) in quadrants {
        eprintln!("{name:>13} = {n}");
    }
    eprintln!("total         = {total}");

    assert!(
        total > 0,
        "the first-person held item drew nothing at all. This is the defect itself: \
         `prepare_first_person_hand` renders the bare arm unless `MainHandSource` names \
         an item with baked geometry"
    );
    let bottom_right = quadrants[3].1;
    let quadrant_area = (hw * hh) as f32;
    assert!(
        bottom_right as f32 / quadrant_area >= MIN_HAND_QUADRANT_FRACTION,
        "the item covers {:.2}% of the bottom-right quadrant, below the {:.0}% floor — \
         `applyItemArmTransform` puts it right, down and forward of the eye",
        bottom_right as f32 / quadrant_area * 100.0,
        MIN_HAND_QUADRANT_FRACTION * 100.0
    );
    assert!(
        bottom_right * 2 > total,
        "most of the item must be in the bottom-right quadrant; only {bottom_right} of \
         {total} pixels are"
    );

    // The executed negative control: mirror the posed mesh in `x` about the eye,
    // which is what an `Arm::invert` sign error produces. It must **fail the same
    // assertion** — the mirrored item lands in the bottom-*left* quadrant, so the
    // quadrant floor above is doing real work rather than being satisfied by any
    // item anywhere.
    let flipped = render(
        &gpu,
        &models,
        &repose(
            &mesh,
            glam::Mat4::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0)),
        ),
        vp,
    );
    let flipped_br = lit_in(&flipped, hw, hh, W, H);
    let flipped_bl = lit_in(&flipped, 0, hh, hw, H);
    eprintln!("mirrored control: bottom-right {flipped_br}, bottom-left {flipped_bl}");
    assert!(
        flipped_bl > flipped_br,
        "the mirrored control must land on the *left*; it put {flipped_br} pixels \
         bottom-right and {flipped_bl} bottom-left, so mirroring the arm sign is \
         invisible to this gate"
    );
    assert!(
        (flipped_br as f32 / quadrant_area) < MIN_HAND_QUADRANT_FRACTION,
        "the mirrored control passes the subject's own bottom-right floor \
         ({:.2}%), so that floor cannot detect an inverted arm sign",
        flipped_br as f32 / quadrant_area * 100.0
    );
    let differing = (0..(W * H) as usize)
        .filter(|&i| lit(&subject, i) != lit(&flipped, i))
        .count();
    eprintln!("mirrored control differs in {differing} pixels");
    assert!(
        differing > total / 2,
        "a mirrored pose produced nearly the same frame ({differing} pixels differ of \
         {total} lit)"
    );
}
