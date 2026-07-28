//! Prove that a dropped **flat sprite item** reaches pixels.
//!
//! ## The defect
//!
//! `collect_item_model_parts` kept only [`IconPart::Model`] parts, so an
//! `item/generated` icon never entered [`BlockModels::items`] and the shell's
//! dropped-item pass skipped it outright (`prepare_item_drops` does
//! `let Some(geometry) = … model.items.get(id) else { continue }`). That is not a
//! corner: it is **most items in the game** — every tool, ingot, gem and food. A
//! dropped `minecraft:diamond` resolved its identity correctly off the wire and
//! then drew **zero pixels**.
//!
//! The fix synthesises vanilla's geometry for those items:
//! `ItemModelGenerator` (26.2,
//! `net.minecraft.client.resources.model.cuboid`) extrudes the sprite into a
//! 1/16-block slab — a `SOUTH` face, a `NORTH` face with reversed `u`, and one
//! edge quad per boundary texel of the sprite's **alpha outline**.
//!
//! ## What this gate measures, and why each piece is not vacuous
//!
//! 1. **A silhouette exists.** Lit pixels inside the item's own projected
//!    bounding box. The unfixed build's value is not "small", it is **exactly
//!    zero**, and [`the_pre_fix_build_draws_exactly_nothing`] renders that frame
//!    and counts it rather than asserting it from memory.
//! 2. **It is a silhouette, not a slab.** The count must be *strictly less* than
//!    the bounding box, because a sprite is a cutout: if the alpha discard or the
//!    UVs were wrong the quad would fill its whole box. A ">0 pixels" assertion
//!    passes under that bug; this band does not.
//! 3. **It is the right sprite, right way up.** The rendered silhouette's
//!    per-row profile is correlated against the **sprite's own alpha row profile
//!    read out of the atlas** — an expected value that originates outside the
//!    geometry code entirely. The vertically reversed profile is scored too, and
//!    must score worse. An upside-down item is the exact visible signature of
//!    getting the world-pose determinant backwards, and it is invisible to a
//!    pixel *count*.
//! 4. **It is localised.** The opposite corner of the frame must be untouched, so
//!    a full-screen wash cannot pass as an item.
//!
//! ## The determinant, verified rather than assumed
//!
//! `sign(det(...))` inverts by context and the two cases are easy to swap:
//!
//! * the composed **GUI** matrix `gui_ortho * gui_item_pose` must be **negative**
//!   (it matches `Camera::view_projection`, which is itself negative);
//! * a **world-space** drop pose must be **positive**, because it is
//!   *left*-multiplied by that same negative `view_projection`.
//!
//! [`a_world_drop_pose_is_positive_and_the_composition_is_negative`] derives the
//! front-facing sign from a real camera rather than hardcoding either answer, and
//! then checks the claim survives contact with the rasteriser: the same slab
//! rendered under a deliberately handedness-flipped pose must not reproduce the
//! correct frame.
//!
//! `#[ignore]`d and **fail-closed**: needs a GPU adapter *and* a fetched
//! `client.jar`. Once opted in, a missing adapter is a failure, never a skip. Run
//! with
//! `cargo test -p lodestone-render --test sprite_drop_pixels -- --ignored --nocapture`.

use lodestone_assets::{Atlas, AtlasSprite, GuiLight, ResourceManager, ZipSource};
use lodestone_render::entity::{
    dropped_item_matrix, dropped_item_mesh, ground_transform_for, item_hover_lift,
};
use lodestone_render::{
    BlockModels, Camera, CameraUniform, GpuAtlas, GpuModelMesh, ItemGeometry, ModelMesh,
    ModelPipeline, blocks_json_registry, model_anim_buffer, model_camera_buffer,
    model_palette_buffer,
};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

const W: u32 = 256;
const H: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The item under test: exactly the one asserted live to draw nothing. Its icon
/// is `item/diamond` through `builtin/generated`, i.e. the flat-sprite path.
const ITEM: &str = "minecraft:diamond";

/// World position of the drop. `x`/`z` on a block centre so the pose's rotation
/// pivot is the frame centre.
const DROP: glam::Vec3 = glam::Vec3::new(0.5, 0.0, 0.5);

/// Camera distance from the drop, chosen so the 0.5-block posed sprite covers
/// roughly a third of the frame height at the default 70° vertical FOV: big
/// enough that a silhouette is many hundreds of pixels, small enough that "the
/// item" and "the whole screen" are far apart.
const CAM_DISTANCE: f32 = 1.2;

/// Age and bob phase are pinned to zero so `item_spin_radians` is `0` and the
/// slab faces `+Z` — square to the camera. The spin is vanilla behaviour and is
/// covered by `entity::dropped_item_pose_preserves_winding` across several ages;
/// varying it here would only make the silhouette oracle angle-dependent.
const AGE_TICKS: f32 = 0.0;
const BOB_OFFSET: f32 = 0.0;

/// Full daylight, so the shader's `0.2 + 0.8 * max(sky, block)` is `1.0` and a
/// dark frame cannot be mistaken for an empty one.
const LIGHT: u8 = 0xF0;

/// A clear colour no item texel can coincide with: fully saturated magenta is
/// absent from the vanilla item atlas, so "differs from the clear" is a sound
/// definition of "the item drew here".
const CLEAR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

/// Minimum silhouette area, in pixels. Well above zero (the unfixed build) and
/// above any plausible handful of stray fragments, and far below the predicted
/// ~2000.
const MIN_SILHOUETTE_PX: usize = 300;

/// The silhouette must leave at least this fraction of its own bounding box
/// unlit. A sprite is a cutout; a solid box means the alpha discard or the UVs
/// are wrong.
const MIN_BOX_SLACK: f32 = 0.10;

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
                label: Some("sprite_drop_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    });
    // Fail closed: this test is `#[ignore]`d, so running it is an explicit
    // request for a real GPU frame.
    gpu.expect("sprite_drop_pixels: no GPU adapter, and this gate must not skip")
}

fn build_models() -> BlockModels {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

fn item_geometry(models: &BlockModels) -> &ItemGeometry {
    let id = ITEM.parse().expect("valid resource location");
    models.item(&id).unwrap_or_else(|| {
        panic!(
            "{ITEM} has no baked geometry. This is the defect itself: a flat `builtin/generated` \
             icon never entered `BlockModels::items`, so the dropped-item pass skipped it and the \
             item drew zero pixels"
        )
    })
}

/// The camera used by every frame here: on `+Z`, looking down `-Z` at the posed
/// item's centre. Square aspect so the two axes are directly comparable.
fn camera(centre: glam::Vec3) -> Camera {
    Camera {
        position: glam::Vec3::new(DROP.x, centre.y, DROP.z + CAM_DISTANCE),
        yaw: 180.0, // forward = (0, 0, -1)
        pitch: 0.0,
        aspect: 1.0,
        ..Camera::default()
    }
}

/// Render one already-built world-space mesh through the real
/// [`ModelPipeline`] against the real stitched atlas, and read the frame back
/// row-major.
#[allow(clippy::too_many_lines)]
fn render(gpu: &Gpu, models: &BlockModels, mesh: &ModelMesh, cam: &Camera) -> Vec<(u8, u8, u8)> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_atlas(device, queue, models.atlas());
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);
    let palette_buffer = model_palette_buffer(device, models.tint_palette());
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &models.anim_slot_uniforms(0));
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    // A *world* camera with a zero section origin — exactly what
    // `prepare_item_drops` writes into `drop_cam_buffer`.
    let cam_buffer = model_camera_buffer(
        device,
        CameraUniform {
            view_proj: cam.view_projection().to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer);

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sprite drop target"),
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
            label: Some("sprite drop pixels"),
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
        // An empty mesh still clears the target — that is the pre-fix frame, and
        // it has to go through the identical path to be a fair control.
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

    let mut out = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        for x in 0..W {
            let i = (y * padded + x * 4) as usize;
            out.push((data[i], data[i + 1], data[i + 2]));
        }
    }
    out
}

/// The clear colour as it reads back off an `Rgba8UnormSrgb` target.
fn clear_rgb() -> (u8, u8, u8) {
    (255, 0, 255)
}

/// Whether a pixel was written by geometry rather than left at the clear.
fn lit(px: (u8, u8, u8)) -> bool {
    px != clear_rgb()
}

/// Every lit pixel's `(x, y)`.
fn lit_pixels(frame: &[(u8, u8, u8)]) -> Vec<(u32, u32)> {
    frame
        .iter()
        .enumerate()
        .filter(|(_, px)| lit(**px))
        .map(|(i, _)| {
            let i = u32::try_from(i).expect("frame fits in u32");
            (i % W, i / W)
        })
        .collect()
}

/// The world-space mesh for the drop, optionally under a caller-supplied pose
/// override so a handedness-flipped variant can go through the same path.
fn drop_mesh(geometry: &ItemGeometry, flip: Option<glam::Mat4>) -> ModelMesh {
    let ground = ground_transform_for(geometry.gui_light);
    let mut mesh = dropped_item_mesh(
        &geometry.quads,
        geometry.gui_light,
        &ground,
        DROP,
        AGE_TICKS,
        BOB_OFFSET,
        LIGHT,
    );
    if let Some(extra) = flip {
        // Re-pose the already-world-space vertices about the item's centre.
        for v in &mut mesh.vertices {
            v.position = extra.transform_point3(glam::Vec3::from(v.position)).into();
        }
    }
    mesh
}

/// The world matrix `prepare_item_drops` builds for this drop.
fn drop_matrix(geometry: &ItemGeometry) -> glam::Mat4 {
    let ground = ground_transform_for(geometry.gui_light);
    let lift = item_hover_lift(&geometry.quads, &ground);
    dropped_item_matrix(DROP, AGE_TICKS, BOB_OFFSET, &ground, lift)
}

/// World-space axis-aligned bounds of a meshed drop.
fn mesh_bounds(mesh: &ModelMesh) -> (glam::Vec3, glam::Vec3) {
    let mut lo = glam::Vec3::splat(f32::MAX);
    let mut hi = glam::Vec3::splat(f32::MIN);
    for v in &mesh.vertices {
        let p = glam::Vec3::from(v.position);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo, hi)
}

/// The item's projected screen bounding box in pixels, as
/// `(x0, y0, x1, y1)` inclusive-exclusive.
fn projected_box(mesh: &ModelMesh, cam: &Camera) -> (u32, u32, u32, u32) {
    let vp = cam.view_projection();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for v in &mesh.vertices {
        let p = vp.project_point3(glam::Vec3::from(v.position));
        let sx = (p.x * 0.5 + 0.5) * W as f32;
        let sy = (0.5 - p.y * 0.5) * H as f32;
        x0 = x0.min(sx);
        y0 = y0.min(sy);
        x1 = x1.max(sx);
        y1 = y1.max(sy);
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    (
        x0.floor().max(0.0) as u32,
        y0.floor().max(0.0) as u32,
        (x1.ceil().min(W as f32)) as u32,
        (y1.ceil().min(H as f32)) as u32,
    )
}

/// The sprite's own **alpha** row profile, read straight out of the stitched
/// atlas: `rows[i]` is the number of non-transparent texels in image row `i`
/// (row `0` is the top of the PNG).
///
/// This is the oracle. It comes from the texture the artist authored, not from
/// any code under test, so agreeing with it cannot be arranged by two symmetric
/// misunderstandings in the generator and the renderer.
fn sprite_alpha_rows(atlas: &Atlas, sprite: &AtlasSprite) -> Vec<u32> {
    let [fx, fy, fw, fh] = sprite
        .frame_pixel_rect(0)
        .expect("sprite has a frame 0 in the atlas");
    (0..fh)
        .map(|row| {
            (0..fw)
                .filter(|col| {
                    let i = (((fy + row) * atlas.width + fx + col) * 4) as usize;
                    atlas.rgba.get(i + 3).copied().unwrap_or(0) != 0
                })
                .count() as u32
        })
        .collect()
}

/// Lit pixels bucketed into `bands` horizontal bands across `(y0, y1)`, band `0`
/// topmost — the screen-space counterpart of [`sprite_alpha_rows`].
fn screen_row_profile(lit: &[(u32, u32)], y0: u32, y1: u32, bands: usize) -> Vec<u32> {
    let mut out = vec![0u32; bands];
    let span = (y1 - y0).max(1) as f32;
    for &(_, y) in lit {
        let t = (f32::from(u16::try_from(y.saturating_sub(y0)).unwrap_or(0)) / span).clamp(0.0, 1.0);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let band = ((t * bands as f32) as usize).min(bands - 1);
        out[band] += 1;
    }
    out
}

/// Pearson correlation of two equal-length profiles.
fn correlation(a: &[u32], b: &[u32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let n = a.len() as f32;
    let fa: Vec<f32> = a.iter().map(|v| *v as f32).collect();
    let fb: Vec<f32> = b.iter().map(|v| *v as f32).collect();
    let ma = fa.iter().sum::<f32>() / n;
    let mb = fb.iter().sum::<f32>() / n;
    let num: f32 = fa.iter().zip(&fb).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let da = fa.iter().map(|x| (x - ma).powi(2)).sum::<f32>().sqrt();
    let db = fb.iter().map(|y| (y - mb).powi(2)).sum::<f32>().sqrt();
    if da == 0.0 || db == 0.0 { 0.0 } else { num / (da * db) }
}

// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn a_dropped_sprite_item_draws_a_silhouette() {
    let gpu = setup();
    let models = build_models();
    let geometry = item_geometry(&models);

    // Anti-vacuity on the *geometry*, before any pixel is read. A bare pair of
    // front/back quads would still light pixels, so the count that distinguishes
    // "vanilla's extrusion" from "a billboard" is asserted here: the sprite's
    // alpha outline has to have produced edge quads.
    assert_eq!(
        geometry.gui_light,
        GuiLight::Front,
        "`ItemModelGenerator.guiLight()` is FRONT; the value routes the drop pose to \
         GENERATED_ITEM_GROUND rather than the block items' transform"
    );
    assert!(
        geometry.quads.len() > 20,
        "vanilla extrudes the sprite: a SOUTH face, a NORTH face, and one quad per boundary texel \
         of the alpha outline. {ITEM} baked only {} quads, which is a flat billboard, not a slab",
        geometry.quads.len()
    );

    let mesh = drop_mesh(geometry, None);
    let (lo, hi) = mesh_bounds(&mesh);
    let centre = (lo + hi) * 0.5;
    let cam = camera(centre);

    // The slab really is a slab: 1/16 block thick, scaled 0.5 by
    // `GENERATED_ITEM_GROUND`, so 1/32 block in world z.
    let depth = hi.z - lo.z;
    assert!(
        (depth - 1.0 / 32.0).abs() < 1e-4,
        "vanilla's MIN_Z/MAX_Z of 7.5/8.5 under a 0.5 ground scale is a 1/32-block slab; got {depth}"
    );

    let frame = render(&gpu, &models, &mesh, &cam);
    let lit = lit_pixels(&frame);
    let (bx0, by0, bx1, by1) = projected_box(&mesh, &cam);
    let box_area = ((bx1 - bx0) * (by1 - by0)) as usize;

    println!("=== DROPPED SPRITE ITEM: {ITEM} ===");
    println!("  baked quads          : {}", geometry.quads.len());
    println!("  world bounds         : {lo} .. {hi}");
    println!("  projected box (px)   : x {bx0}..{bx1}  y {by0}..{by1}  area {box_area}");
    println!("  lit pixels           : {}", lit.len());
    println!(
        "  unfixed build        : 0 (see `the_pre_fix_build_draws_exactly_nothing`, which \
         renders it)"
    );

    assert!(
        lit.len() >= MIN_SILHOUETTE_PX,
        "the dropped {ITEM} must draw a measurable silhouette; got {} px. The unfixed build's \
         value here is exactly 0",
        lit.len()
    );
    assert!(
        box_area > 0 && lit.len() < box_area,
        "the silhouette cannot exceed its own projected bounding box ({} px in {box_area})",
        lit.len()
    );
    // A cutout, not a slab: if the alpha discard or the UVs were wrong the front
    // face would fill its box and this is what would catch it.
    let slack = 1.0 - lit.len() as f32 / box_area as f32;
    println!("  box slack            : {slack:.3} (must exceed {MIN_BOX_SLACK})");
    assert!(
        slack > MIN_BOX_SLACK,
        "a sprite item is a cutout, so its silhouette must leave part of its bounding box unlit; \
         slack is only {slack:.3}. A filled box means the alpha discard or the UVs are wrong and \
         the frame is a coloured rectangle, which a pixel *count* alone would accept"
    );

    // Localised: the corner diagonally opposite the item is untouched.
    let far_x = if bx0 > W - bx1 { 0..bx0 / 2 } else { bx1 + (W - bx1) / 2..W };
    let far_y = if by0 > H - by1 { 0..by0 / 2 } else { by1 + (H - by1) / 2..H };
    let corner = lit
        .iter()
        .filter(|(x, y)| far_x.contains(x) && far_y.contains(y))
        .count();
    println!("  opposite-corner px   : {corner} (x {far_x:?}, y {far_y:?})");
    assert_eq!(
        corner, 0,
        "the drop must be a localised silhouette, not a full-frame wash; {corner} px lit in the \
         opposite corner"
    );
}

#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn the_silhouette_matches_the_sprites_own_alpha_and_is_not_upside_down() {
    let gpu = setup();
    let models = build_models();
    let geometry = item_geometry(&models);

    let mesh = drop_mesh(geometry, None);
    let (lo, hi) = mesh_bounds(&mesh);
    let cam = camera((lo + hi) * 0.5);
    let frame = render(&gpu, &models, &mesh, &cam);
    let lit = lit_pixels(&frame);
    let (_, by0, _, by1) = projected_box(&mesh, &cam);

    let sprite_id = format!("minecraft:item/{}", ITEM.rsplit(':').next().unwrap())
        .parse()
        .expect("valid sprite location");
    let sprite = models
        .atlas()
        .sprite(&sprite_id)
        .expect("the item's layer0 sprite must be stitched into the block atlas");
    let expected = sprite_alpha_rows(models.atlas(), sprite);
    let actual = screen_row_profile(&lit, by0, by1, expected.len());
    let reversed: Vec<u32> = actual.iter().copied().rev().collect();

    let upright = correlation(&expected, &actual);
    let flipped = correlation(&expected, &reversed);

    println!("=== SILHOUETTE vs THE SPRITE'S OWN ALPHA ({ITEM}) ===");
    println!("  sprite {}x{} (frame h {})", sprite.width, sprite.height, sprite.frame_height);
    println!("  expected alpha rows  : {expected:?}");
    println!("  rendered row profile : {actual:?}");
    println!("  correlation upright  : {upright:+.3}");
    println!("  correlation flipped  : {flipped:+.3}");

    // Precondition, asserted rather than skipped: the sprite must be vertically
    // asymmetric enough for "upside down" to be distinguishable at all. A
    // symmetric sprite would make the comparison below the *precondition* species
    // of vacuous test.
    let self_flip = correlation(&expected, &expected.iter().copied().rev().collect::<Vec<_>>());
    println!("  sprite self-vs-flipped: {self_flip:+.3} (must be < 0.90 to discriminate)");
    assert!(
        self_flip < 0.90,
        "{ITEM}'s sprite is too vertically symmetric ({self_flip:+.3}) for this gate to tell \
         upright from upside down; choose an item whose alpha profile is asymmetric"
    );

    assert!(
        upright > 0.90,
        "the rendered silhouette must follow the sprite's own alpha profile; correlation is only \
         {upright:+.3}. Either the wrong sprite is sampled or the UVs are wrong"
    );
    assert!(
        upright > flipped + 0.15,
        "the item must be the right way up: upright correlation {upright:+.3} vs flipped \
         {flipped:+.3}. An upside-down item is what a *negative* world-pose determinant produces, \
         and it is invisible to a pixel count"
    );
}

/// **The negative control, executed.** The unfixed build's frame, produced by the
/// identical render path with the geometry the unfixed build supplied for this
/// item: none. `prepare_item_drops` skipped every sprite item, so this is not an
/// approximation of the old behaviour, it *is* it.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn the_pre_fix_build_draws_exactly_nothing() {
    let gpu = setup();
    let models = build_models();
    let geometry = item_geometry(&models);

    let mesh = drop_mesh(geometry, None);
    let (lo, hi) = mesh_bounds(&mesh);
    let cam = camera((lo + hi) * 0.5);

    let empty = ModelMesh::default();
    let frame = render(&gpu, &models, &empty, &cam);
    let lit = lit_pixels(&frame);

    println!("=== NEGATIVE CONTROL: the pre-fix build's frame ===");
    println!("  geometry supplied    : none (the item was absent from BlockModels::items)");
    println!("  lit pixels           : {}", lit.len());
    println!("  the positive gate's floor is {MIN_SILHOUETTE_PX} px, which this cannot reach");

    assert_eq!(
        lit.len(),
        0,
        "the detector must report zero for a frame with no geometry, or every count it reports is \
         confounded; got {} px",
        lit.len()
    );
    assert!(
        MIN_SILHOUETTE_PX > 0,
        "the positive gate's acceptance band must exclude the control's value"
    );
}

#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn a_world_drop_pose_is_positive_and_the_composition_is_negative() {
    let gpu = setup();
    let models = build_models();
    let geometry = item_geometry(&models);

    let mesh = drop_mesh(geometry, None);
    let (lo, hi) = mesh_bounds(&mesh);
    let cam = camera((lo + hi) * 0.5);
    let vp = cam.view_projection();
    let pose = drop_matrix(geometry);

    println!("=== THE DETERMINANT, DERIVED NOT ASSUMED ===");
    println!("  det(view_projection)      = {:+.6e}", vp.determinant());
    println!("  det(drop pose)            = {:+.6e}", pose.determinant());
    println!("  det(view_projection*pose) = {:+.6e}", (vp * pose).determinant());

    // The camera's own sign is a *measurement*, not an assumption: glam's DirectX
    // right-handed perspective happens to be orientation-reversing, and the whole
    // trap is that "positive" sounds like the safe answer.
    assert!(
        vp.determinant() < 0.0,
        "the reference camera is expected to be orientation-reversing; got {:+e}",
        vp.determinant()
    );
    // A world pose is *left*-multiplied by that camera, exactly like a terrain
    // section's model matrix, so it must not flip anything.
    assert!(
        pose.determinant() > 0.0,
        "a world-space drop pose must be POSITIVE (translation, Y rotation, positive uniform \
         scale); got {:+e}. The GUI rule — that `gui_ortho * gui_item_pose` is NEGATIVE — is a \
         statement about the composed matrix and inverts here",
        pose.determinant()
    );
    assert_eq!(
        (vp * pose).determinant().signum(),
        vp.determinant().signum(),
        "the composition must inherit the camera's winding, as terrain does"
    );

    // ...and the claim has to survive the rasteriser, not just the algebra. The
    // same slab under a handedness-flipped pose — the geometry a negative world
    // pose produces — must not reproduce the correct frame.
    let good = render(&gpu, &models, &mesh, &cam);
    let centre = (lo + hi) * 0.5;
    let flip = glam::Mat4::from_translation(centre)
        * glam::Mat4::from_scale(glam::Vec3::new(1.0, -1.0, 1.0))
        * glam::Mat4::from_translation(-centre);
    assert!(
        flip.determinant() < 0.0,
        "the control transform must actually reverse handedness"
    );
    let flipped_mesh = drop_mesh(geometry, Some(flip));
    let bad = render(&gpu, &models, &flipped_mesh, &cam);

    let differing = good
        .iter()
        .zip(&bad)
        .filter(|(a, b)| a != b)
        .count();
    let good_lit = lit_pixels(&good).len();
    let bad_lit = lit_pixels(&bad).len();
    println!("  correct frame lit px      = {good_lit}");
    println!("  flipped frame lit px      = {bad_lit}");
    println!("  pixels differing          = {differing}");
    assert!(
        differing > good_lit / 4,
        "a handedness-flipped pose must be visibly a different frame, or this gate cannot see the \
         inside-out bug at all: only {differing} of {} px differ",
        good.len()
    );

    // Determinism: the same input twice must be byte-identical, so a flaky
    // adapter cannot masquerade as either result.
    let again = render(&gpu, &models, &mesh, &cam);
    assert_eq!(
        good.iter().zip(&again).filter(|(a, b)| a != b).count(),
        0,
        "the same mesh and camera must render byte-identically"
    );
}
