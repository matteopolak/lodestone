//! Pixel gate: the boat water-clip mask (`92558b73`, "placing down a boat
//! still shows water through the bottom") actually reaches pixels.
//!
//! # Why this exists
//!
//! `92558b73` ported vanilla's `BoatModel.createWaterPatch` — an invisible,
//! depth-only mask drawn through `EntityPipeline::water_mask_pipeline`
//! (colour writes disabled, depth writes on) that fills the boat hull's
//! hollow interior so the translucent water pass's depth test fails there
//! instead of drawing straight through the gap between the five thin planks.
//! Its own author verified only at the CPU level: geometry against vanilla's
//! constants, corpus-loader resolution, placement-transform identity. A
//! depth-only, colour-masked draw is exactly the shape that can be
//! structurally correct and still do nothing — wrong pipeline bound, wrong
//! pass order, an inverted depth comparison (this backend's `[0,1]`
//! DirectX-style depth is not vanilla's reversed-Z, so a ported comparison
//! can silently flip) — so this is the live check: a boat sitting in real
//! water, rendered through the real production path, read back as rasterised
//! pixels.
//!
//! # The clean A/B
//!
//! `gpu/entity_passes.rs`'s `prepare_entities` adds the water-patch instance
//! only when `EntityDraw::type_path` **ends with `"_boat"`**
//! (`"oak_boat".ends_with("_boat")`). `lodestone_render::entity`'s own
//! `a_literal_corpus_rig_name_still_resolves_to_itself` test establishes that
//! the bare corpus name `"boat"` is *not* a real registry entity type and
//! resolves straight to the same `boat` rig **without** going through that
//! suffix rule (`canonical_model_name("boat") == Some("boat")`, and
//! `"boat".ends_with("_boat")` is `false` — four characters cannot end with a
//! five-character suffix). So `"oak_boat"` and `"boat"` draw the **identical
//! hull mesh**, at the same placement, and differ *only* in whether the
//! water-mask instance is submitted — the cleanest possible A/B, needing no
//! second geometry family and no guessed "with vs without" flag threaded
//! through test-only code.
//!
//! # The metric
//!
//! Six renders through one real world and one real camera. Two are
//! **entity-free references** — `bg` (no entity, no terrain: the background
//! this scene actually has, rendered rather than restated as a constant) and
//! `water_only` (no entity, puddle uploaded) — and four are the A/B:
//! `masked_water` (`"oak_boat"`, puddle), `masked_nowater`, `unmasked_water`
//! (`"boat"`, puddle), `unmasked_nowater`.
//!
//! Everything is scoped to two derived probes rather than to the frame. The
//! coarse one is [`patch_screen_rect`], the water patch's **own baked
//! vertices** projected through the **same** `part_transforms` the draw uses.
//! The fine one is [`silhouette`]: the pixels the hull covers, measured
//! against `bg`. The coarse rect alone is not enough — it also contains open
//! water beside the hull, which legitimately changes with the puddle and
//! which the mask correctly does not hide.
//!
//! Three separate claims come out of that, and each has failed for a
//! different reason during this gate's life:
//!
//! 1. **The mask must not occlude visible entity geometry.** With no water
//!    anywhere, a depth-only draw has nothing behind it to hide, so adding the
//!    mask instance must change **zero** boat pixels. Measured at 5,492 px when
//!    `prepare_entities` pushed the patch *before* the boat's own instance:
//!    `plan_entities` batches in first-appearance order, so the patch's depth
//!    write depth-rejected the hull's own below-waterline planks and, having
//!    no colour to write, left background showing through them. The rider A/B
//!    below measured the subtler form from the owner report: 798 player pixels
//!    vanished when a later player batch followed the mask.
//! 2. **Without the mask, water paints inside the hull.** Measured: 4,578 of
//!    the hull's 11,124 silhouette pixels.
//! 3. **With the mask, it does not.** Measured: 645 — the residue being the
//!    submerged *outside* of the hull, which vanilla's patch does not cover
//!    either. The 3,933-pixel difference is then checked per pixel, and all
//!    3,933 satisfy both halves at once (masked stays put when water is added
//!    or removed, unmasked visibly reveals water): `confirmed=3933
//!    mask-only=0 reveal-only=0`.
//!
//! # The negative control
//!
//! See [`a_raft_gets_no_mask_and_still_shows_real_water_change`]. It is not
//! the claim its name suggests, and the reason is worth reading before
//! trusting the number: a raft has no cavity, so water paints over only 9.2%
//! of its silhouette against a *masked* boat's 5.8% and an unmasked boat's
//! 41.2%. The raft sits nearer the masked boat, so any threshold separating
//! the two on that axis would be fitted. What it asserts instead is exact:
//! `"oak_raft"` and `"raft"` render byte-identically (the suffix rule does not
//! fire), with `"oak_boat"` versus `"boat"` as the positive control that the
//! comparison can see a mask at all.
//!
//! Fail-closed like every gate in this crate: no GPU adapter or no
//! `client.jar` is a failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test boat_water_mask_pixels -- --ignored --nocapture
//! ```

use std::sync::Arc;

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, ThirdPersonBodyState};
use lodestone::mesher::{SectionGeometry, SectionKey, mesh_snapshot_fluids, mesh_snapshot_models, snapshot_section, snapshot_visibility};
use lodestone::resources::BlockResources;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, entity_anim::AnimInput};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

const W: u32 = 480;
const H: u32 = 360;
const FOV_Y_DEGREES: f32 = 50.0;

/// Render distance for the water world, in chunks — small on purpose: this
/// gate needs a few loaded columns around one boat, not a realistic view
/// distance. The water itself is a 3x3 puddle; see [`water_world`].
const RD_CHUNKS: i32 = 3;
const MIN_Y: i32 = 0;
/// The water surface: the puddle occupies the single layer `SURFACE_Y - 1`, so
/// its top face is at `SURFACE_Y`. The boat does **not** stand on it — see
/// [`BOAT_FEET_Y`] for the draft that makes this gate able to measure
/// anything. One section for the water plus one of headroom for the boat and
/// camera.
const SURFACE_Y: i32 = 16;
const SECTION_COUNT: usize = 2;

/// `20` ticks/s * `10` s, clearing the section fade window
/// (`SECTION_FADE_DURATION_SECS = 0.75`) with a wide margin — see
/// `distant_flat_terrain_holes.rs::render_frame`'s doc for why skipping this
/// makes every section render as pure fog colour regardless of what was
/// uploaded.
const FADE_COMPLETE_TICK: u64 = 200;

/// Manhattan RGB distance above which two pixels count as different.
const DIFFERS: i32 = 24;

fn differs(a: &[u8], b: &[u8]) -> bool {
    let d = (i32::from(a[0]) - i32::from(b[0])).abs()
        + (i32::from(a[1]) - i32::from(b[1])).abs()
        + (i32::from(a[2]) - i32::from(b[2])).abs();
    d > DIFFERS
}

fn diff_count(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(x, y)| differs(x, y)).count()
}

/// `minecraft:water`'s **source** state (`level=0`) — a lake, not a tilted
/// flowing surface, matching `water_seam_convergence.rs`'s own reasoning for
/// picking it: a flowing level would put a second variable in the
/// measurement.
fn water_source_state() -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| {
            lodestone_data::block_states::block_name(id) == Some("minecraft:water")
                && lodestone_data::block_states::properties(id)
                    .is_some_and(|props| props.iter().any(|&(k, v)| k == "level" && v == "0"))
        })
        .expect("minecraft:water[level=0] must exist in the 26.2 block-state table")
}

fn air_state() -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some("minecraft:air"))
        .expect("minecraft:air must exist in the 26.2 block-state table")
}

/// Half-width, in blocks, of the water the boat floats on — a **puddle**, not
/// a lake. See [`water_world`]'s doc for why the size is the whole point. The
/// value is not free: the puddle must contain the hull's whole world-space
/// footprint (asserted, from the resolved instance's own AABB) and no more.
const PUDDLE_RADIUS: i32 = 1;

/// A 3×3×1 puddle of water directly under the boat, in an otherwise empty
/// air world, fully loaded before any section is meshed, under open daylight.
///
/// # Why this is a puddle and not a lake
///
/// The first version of this fixture filled every block of a 7×7-chunk region
/// below `SURFACE_Y` with water. That made the gate unable to answer its own
/// question, in the way `CLAUDE.md` calls the *world* species: the raft
/// negative control measured **159,068** changed pixels between water present
/// and water absent — 92% of a 480×360 frame — because it was measuring the
/// background ocean, not the raft's hollow, and the boat arm's whole-frame
/// numbers (157,812 either way) were the same quantity. Neither figure had
/// anything to do with the hull.
///
/// Water only under the boat makes every water/no-water difference a *local*
/// one, and [`patch_screen_rect`] then narrows it further to the exact region
/// the mask covers. `PUDDLE_RADIUS` is the smallest value whose world extent
/// still contains the hull's own footprint, and the gate asserts that
/// containment from the resolved instance's AABB rather than trusting the
/// constant.
///
/// See `distant_flat_terrain_holes.rs::flat_world`'s doc for why the light
/// step is not optional: a column's light defaults to `LightData::Missing`,
/// which resolves to `0` (full dark), and unlit-but-present geometry renders
/// dark enough to be indistinguishable from this scene's own background —
/// not "hard to see", byte-identical.
fn water_world() -> World {
    let water = water_source_state();
    let air = air_state();
    let mut world = World::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            let mut light = ColumnLight::new(SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let written = world.fill_region(
        [-PUDDLE_RADIUS, SURFACE_Y - 1, -PUDDLE_RADIUS],
        [PUDDLE_RADIUS, SURFACE_Y - 1, PUDDLE_RADIUS],
        water,
    );
    let expected = (2 * PUDDLE_RADIUS + 1).pow(2) as usize;
    assert_eq!(
        written, expected,
        "fixture: the puddle must be exactly {expected} water blocks, one layer deep"
    );
    world
}

/// A screen-space rectangle, half-open on neither edge (both bounds
/// inclusive), in target pixels.
#[derive(Debug, Clone, Copy)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    fn area(self) -> usize {
        ((self.x1 - self.x0 + 1) as usize) * ((self.y1 - self.y0 + 1) as usize)
    }
}

/// World point → target pixel, the same expression every other pixel gate in
/// this crate uses (`bell_block_entity_pixels.rs`'s `project`).
fn project(view_proj: glam::Mat4, world: glam::Vec3) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    (
        (clip.x / clip.w * 0.5 + 0.5) * W as f32,
        (1.0 - (clip.y / clip.w * 0.5 + 0.5)) * H as f32,
    )
}

/// The screen rect the water-clip mask actually covers, projected from the
/// `boat_water_patch` mesh's **own baked vertices** through the **same**
/// `part_transforms` the draw uses — never a hand-guessed rect, and never the
/// bounding box of a measurement this gate then asserts on.
///
/// This is the probe the old whole-frame diffs lacked. `mesh.local_min`/
/// `local_max` would do as a coarser version, but the patch is a plate posed
/// by a 90° X rotation under its root, so its rest AABB and its posed extent
/// are different rectangles and only the posed one is where pixels land.
fn patch_screen_rect(
    models: &lodestone_render::EntityModelSet,
    feet: glam::Vec3,
    yaw: f32,
    view_proj: glam::Mat4,
) -> Rect {
    let anim = AnimInput::REST;
    let patch = models
        .resolve_animated("boat_water_patch", feet, yaw, 0.0, 1.0, &anim, 0.0, 0.0)
        .expect("\"boat_water_patch\" must resolve through the real corpus loader");
    let mesh = models
        .get("boat_water_patch")
        .expect("the corpus must carry the water-patch mesh");
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for (index, range) in mesh.parts.iter().enumerate() {
        let start = range.vertex_start as usize;
        let end = start + range.vertex_count as usize;
        for vertex in &mesh.vertices[start..end] {
            let world =
                patch.part_transforms[index].transform_point3(glam::Vec3::from(vertex.position));
            let (sx, sy) = project(view_proj, world);
            min = (min.0.min(sx), min.1.min(sy));
            max = (max.0.max(sx), max.1.max(sy));
        }
    }
    assert!(
        min.0 < max.0 && min.1 < max.1,
        "the water patch projected to a degenerate rect ({min:?}..{max:?}) — the camera does \
         not see it at all and every measurement below would be of empty background"
    );
    Rect {
        x0: min.0.max(0.0).floor() as u32,
        y0: min.1.max(0.0).floor() as u32,
        x1: max.0.min((W - 1) as f32).ceil() as u32,
        y1: max.1.min((H - 1) as f32).ceil() as u32,
    }
}

/// The screen rect the puddle itself occupies, projected from the world AABB
/// [`water_world`] fills — the "and not everywhere" half of this fixture,
/// expressed as a containment claim a whole-frame *fraction* cannot make.
fn puddle_screen_rect(view_proj: glam::Mat4) -> Rect {
    let lo = -PUDDLE_RADIUS as f32;
    let hi = (PUDDLE_RADIUS + 1) as f32;
    let (mut min, mut max) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
    for &x in &[lo, hi] {
        for &y in &[(SURFACE_Y - 1) as f32, SURFACE_Y as f32] {
            for &z in &[lo, hi] {
                let (sx, sy) = project(view_proj, glam::Vec3::new(x, y, z));
                min = (min.0.min(sx), min.1.min(sy));
                max = (max.0.max(sx), max.1.max(sy));
            }
        }
    }
    Rect {
        x0: min.0.max(0.0).floor() as u32,
        y0: min.1.max(0.0).floor() as u32,
        x1: max.0.min((W - 1) as f32).ceil() as u32,
        y1: max.1.min((H - 1) as f32).ceil() as u32,
    }
}

/// Pixels differing between `a` and `b` that lie **outside** `rect`.
fn diff_outside(a: &[u8], b: &[u8], rect: Rect) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(i, _)| !rect.contains((*i as u32) % W, (*i as u32) / W))
        .filter(|(_, (x, y))| differs(x, y))
        .count()
}

/// [`diff_count`], restricted to `rect`.
fn diff_in(a: &[u8], b: &[u8], rect: Rect) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(i, _)| rect.contains((*i as u32) % W, (*i as u32) / W))
        .filter(|(_, (x, y))| differs(x, y))
        .count()
}

/// Mesh and upload every section through the real live-vanilla path
/// (`mesh_snapshot_models` for opaque, `mesh_snapshot_fluids` for the
/// translucent water pass) — the same functions `mesh_one` calls in
/// production, just without the worker-pool indirection. Returns 0 and
/// uploads nothing when `upload` is `false`, for the no-terrain reference
/// renders.
fn upload_all(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    upload: bool,
) -> usize {
    if !upload {
        return 0;
    }
    let mut uploaded = 0usize;
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                let fluids = mesh_snapshot_fluids(&snap, models);
                let visibility = snapshot_visibility(&snap, models);
                let geometry = SectionGeometry::Model {
                    opaque,
                    water: fluids.water,
                    translucent_blocks: ModelMesh::default(),
                    visibility,
                };
                state.upload_section(device, queue, key, &geometry);
                uploaded += 1;
            }
        }
    }
    uploaded
}

/// Suppress `RenderState`'s unconditional first-person bare-arm pass — see
/// `distant_flat_terrain_holes.rs`'s identical helper. Measured there too:
/// without this, every config reported an identical fixed-rect "hole"
/// regardless of camera angle, the tell of a screen-space artefact rather
/// than a world-space one.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            // No skin: this fixture installs a body to suppress the first-person
            // arm, not to assert a sheet. The draw falls back to the model's own
            // texture, exactly as it did before this field existed.
            player_skin: None,
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
        })
    });
}

const BOAT_FEET_X: f32 = 0.5;
const BOAT_FEET_Z: f32 = 0.5;
const BOAT_YAW: f32 = 35.0;

/// How far above `feet` the water-patch plate's **centre** sits, in blocks,
/// composed from the two offsets that put it there:
///
/// * `non_living_vehicle_matrix`'s `0.375` bob (vanilla's
///   `poseStack.translate(0, 0.375F, 0)` in `AbstractBoatRenderer.submit`), and
/// * the plate's own `PartPose::offset(0, -3, 1)` — `3/16 = 0.1875` blocks,
///   sign-flipped by the placement's `scale(-1, -1, 1)` — and its `3/16`
///   thickness, whose midpoint is therefore `0.375 + 0.1875/2`.
///
/// Measured against the resolved instance rather than trusted: with `feet.y`
/// at `16.0` the patch's world AABB is `16.3750..16.5625`, centre `16.46875`.
const PATCH_CENTRE_ABOVE_FEET: f32 = 0.375 + 0.1875 / 2.0;

/// The boat's floating height: the one placement at which the world's water
/// **surface** lies inside the water patch.
///
/// # Why this is the whole fixture
///
/// The first version of this gate put `feet.y` at `SURFACE_Y` exactly, which
/// stands the boat *on top of* the water like a bathtub on a floor: measured,
/// the hull spanned `16.0000..16.8836` and the patch `16.3750..16.5625` with
/// the water surface at `16.0`, so every part of the boat was above every part
/// of the water. The mask then has nothing to occlude — its own opaque hull
/// already hides the surface — and the gate measured `reveal == hidden ==
/// 3016` with the masked and unmasked renders **pixel-identical** inside the
/// mask's own rect. That is `CLAUDE.md`'s *world* species exactly: the input
/// does not contain the structure the code under test exists to handle.
///
/// What the mask is *for* is a **partially submerged** boat, where the water
/// surface plane passes through the open hull and the translucent water pass
/// (depth-write off, drawn after entities) paints it inside the boat's cavity.
/// Vanilla authors the patch at the boat's own waterline, so aligning the
/// surface with the patch plane is not a tuned number — it is the definition
/// of floating, read off the model. The resulting draft (`0.53` blocks
/// submerged, `0.35` proud) is asserted at runtime rather than assumed.
const BOAT_FEET_Y: f32 = SURFACE_Y as f32 - PATCH_CENTRE_ABOVE_FEET;

fn boat_draw(type_path: &str) -> EntityDraw {
    EntityDraw {
        hurt: false,
        id: 1,
        type_path: Arc::from(type_path),
        item: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        feet: glam::Vec3::new(BOAT_FEET_X, BOAT_FEET_Y, BOAT_FEET_Z),
        yaw: BOAT_YAW,
        head_yaw: BOAT_YAW,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    }
}

/// A remote-player draw at the boat's real first passenger attachment height.
/// The seated pose puts the legs and lower arms through the hull volume, which
/// is exactly where a prematurely submitted depth-only water patch can erase
/// only the intersecting fragments.
fn rider_draw() -> EntityDraw {
    let mut rider = boat_draw("player");
    rider.id = 2;
    rider.feet.y = BOAT_FEET_Y + 0.5625 / 3.0 - 0.6;
    rider.yaw = BOAT_YAW;
    rider.head_yaw = BOAT_YAW;
    rider.anim = AnimInput {
        is_passenger: true,
        ..AnimInput::REST
    };
    rider
}

/// Looking down and slightly across into the boat's open top, close enough
/// that the hull fills a real fraction of the frame — the "grazing angle
/// into an occupied or empty boat" `boat_water_patch_model`'s own doc names
/// as the gap's visibility condition.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(BOAT_FEET_X, SURFACE_Y as f32 + 3.2, BOAT_FEET_Z - 4.2),
        yaw: 0.0,
        pitch: 40.0,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    }
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

fn load_vanilla() -> (BlockResources, std::sync::Arc<lodestone_render::BlockAtlas>) {
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real client.jar \
             under .cache/mc/26.2 (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    (resources, atlas)
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    target: &mut HeadlessTarget,
    atlas: &lodestone_render::BlockAtlas,
    world: &World,
    models: &lodestone_render::BlockModels,
    camera: &Camera,
    upload_terrain: bool,
    entity_type_path: Option<&str>,
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
    let entity = entity_type_path.map(boat_draw);
    let entities: &[EntityDraw] = match &entity {
        Some(e) => std::slice::from_ref(e),
        None => &[],
    };
    render_entities_frame(
        device,
        queue,
        format,
        target,
        atlas,
        world,
        models,
        camera,
        upload_terrain,
        entities,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_entities_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    target: &mut HeadlessTarget,
    atlas: &lodestone_render::BlockAtlas,
    world: &World,
    models: &lodestone_render::BlockModels,
    camera: &Camera,
    upload_terrain: bool,
    entities: &[EntityDraw],
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    let uploaded = upload_all(world, models, &mut state, device, queue, upload_terrain);
    assert!(
        !upload_terrain || uploaded > 0,
        "fixture: some sections must have uploaded when water was requested"
    );
    // Not optional — see `upload_all`'s and this crate's other terrain gates'
    // doc for why: without an advanced fade clock every freshly-uploaded
    // section renders as pure fog colour, indistinguishable from having no
    // water at all.
    state.update_animation(queue, FADE_COMPLETE_TICK);
    // `None` is the entity-free reference render: the background this scene
    // actually has, rendered rather than restated as a constant. `CLAUDE.md`
    // records the measurement that makes this mandatory — the real background
    // is `SkyFrame::clear_color`, a time-of-day and eye-height resolved fog
    // colour under a sky disc, not any `SKY_COLOR` a test could hardcode.
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, entities);
    (target.read_texels(device, queue), stats)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_water_mask_never_depth_rejects_a_rider_drawn_in_the_hull() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let (_resources, atlas) = load_vanilla();
    let models = atlas
        .models()
        .expect("the vanilla load must attach baked block models");
    let world = water_world();
    let cam = camera();

    let rider = rider_draw();
    let masked = [boat_draw("oak_boat"), rider.clone()];
    let unmasked = [boat_draw("boat"), rider];
    let boat_only = [boat_draw("oak_boat")];

    let (with_mask, _) = render_entities_frame(
        device, queue, format, &mut target, &atlas, &world, models, &cam, false, &masked,
    );
    let (without_mask, _) = render_entities_frame(
        device, queue, format, &mut target, &atlas, &world, models, &cam, false, &unmasked,
    );
    let (without_rider, _) = render_entities_frame(
        device,
        queue,
        format,
        &mut target,
        &atlas,
        &world,
        models,
        &cam,
        false,
        &boat_only,
    );

    let rider_pixels = diff_count(&without_mask, &without_rider);
    let clipped_pixels = diff_count(&with_mask, &without_mask);
    assert!(
        rider_pixels > 100,
        "the seated player changed only {rider_pixels} pixels; the fixture does not visibly draw a rider"
    );
    assert_eq!(
        clipped_pixels, 0,
        "with no water in the scene, the invisible boat mask changed {clipped_pixels} pixels of an otherwise identical boat+rider draw; it can only be depth-rejecting rider fragments submitted after the mask"
    );
}



/// The **exact** world-space XZ footprint of a resolved entity: its baked
/// vertices through its own `part_transforms`, not `EntityInstance::aabb_*`.
///
/// The instance AABB is a *cull* box and is padded — measured, the boat's is
/// `x -1.302..2.302` against a real hull footprint under half that. Using it
/// as a fixture premise would demand a puddle twice the size the boat needs.
fn world_footprint(
    mesh: &lodestone_render::entity::EntityMesh,
    part_transforms: &[glam::Mat4],
) -> (glam::Vec2, glam::Vec2) {
    let mut min = glam::Vec2::splat(f32::MAX);
    let mut max = glam::Vec2::splat(f32::MIN);
    for (index, range) in mesh.parts.iter().enumerate() {
        let start = range.vertex_start as usize;
        let end = start + range.vertex_count as usize;
        for vertex in &mesh.vertices[start..end] {
            let w = part_transforms[index].transform_point3(glam::Vec3::from(vertex.position));
            min = min.min(glam::Vec2::new(w.x, w.z));
            max = max.max(glam::Vec2::new(w.x, w.z));
        }
    }
    (min, max)
}

/// The pixels inside `rect` where `subject` differs from the entity-free
/// `bg` render — the entity's own silhouette, **measured** against a rendered
/// reference rather than derived from a colour constant.
///
/// This is the probe every water claim below is scoped to, and it is a
/// tighter one than [`patch_screen_rect`]'s axis-aligned bounding box. The
/// box is right for "where could the mask possibly act", but it also contains
/// open water beside the hull, which legitimately changes when the puddle is
/// added or removed and which the mask is correctly not hiding. Measured with
/// the box alone: 8,038 px "revealed" against 4,001 px "hidden", of which the
/// great majority on both sides was that open water.
fn silhouette(subject: &[u8], bg: &[u8], rect: Rect) -> Vec<usize> {
    subject
        .chunks_exact(4)
        .zip(bg.chunks_exact(4))
        .enumerate()
        .filter(|(i, _)| rect.contains((*i as u32) % W, (*i as u32) / W))
        .filter(|(_, (s, b))| differs(s, b))
        .map(|(i, _)| i)
        .collect()
}

/// How many of `indices` differ between `a` and `b`.
fn changed_within(indices: &[usize], a: &[u8], b: &[u8]) -> usize {
    indices
        .iter()
        .filter(|&&i| differs(&a[i * 4..i * 4 + 4], &b[i * 4..i * 4 + 4]))
        .count()
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_boat_water_mask_hides_the_hollow_interior_a_bare_boat_rig_does_not() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let (_resources, atlas) = load_vanilla();
    let models: &lodestone_render::BlockModels = atlas
        .models()
        .expect("the vanilla load must attach baked block models");

    let rig = lodestone_render::EntityModelSet::load();
    let feet = glam::Vec3::new(BOAT_FEET_X, BOAT_FEET_Y, BOAT_FEET_Z);

    // Premise, asserted directly rather than assumed: `"boat"` and
    // `"oak_boat"` must resolve to the identical rig, or this A/B is not
    // isolating the mask at all.
    let anim = AnimInput::REST;
    let hull = rig
        .resolve_animated("boat", feet, BOAT_YAW, 0.0, 1.0, &anim, 0.0, 0.0)
        .expect("\"boat\" must resolve");
    let real = rig
        .resolve_animated("oak_boat", feet, BOAT_YAW, 0.0, 1.0, &anim, 0.0, 0.0)
        .expect("\"oak_boat\" must resolve");
    assert_eq!(
        hull.model, real.model,
        "\"boat\" and \"oak_boat\" must resolve to the same corpus rig for this A/B to \
         isolate only the water-mask instance"
    );
    assert_eq!(
        hull.transform, real.transform,
        "\"boat\" and \"oak_boat\" must place identically for this A/B to isolate only the \
         water-mask instance"
    );
    let patch = rig
        .resolve_animated("boat_water_patch", feet, BOAT_YAW, 0.0, 1.0, &anim, 0.0, 0.0)
        .expect("\"boat_water_patch\" must resolve");

    let (foot_min, foot_max) = world_footprint(
        rig.get(hull.model).expect("the hull mesh resolved above"),
        &hull.part_transforms,
    );

    let world = water_world();
    let cam = camera();
    let probe = patch_screen_rect(&rig, feet, BOAT_YAW, cam.view_projection());
    let puddle = puddle_screen_rect(cam.view_projection());

    let shoot = |target: &mut HeadlessTarget, terrain: bool, entity: Option<&str>| {
        render_frame(
            device, queue, format, target, &atlas, &world, models, &cam, terrain, entity,
        )
    };

    // The two entity-free references. `bg` is what this scene's background
    // actually is (rendered, never a hardcoded sky constant — `CLAUDE.md`
    // records the measurement that makes that mandatory), and `water_only` is
    // where the puddle paints with no hull in the way.
    let (bg, _) = shoot(&mut target, false, None);
    let (water_only, _) = shoot(&mut target, true, None);

    let (masked_water, masked_stats) = shoot(&mut target, true, Some("oak_boat"));
    let (masked_nowater, _) = shoot(&mut target, false, Some("oak_boat"));
    let (unmasked_water, unmasked_stats) = shoot(&mut target, true, Some("boat"));
    let (unmasked_nowater, _) = shoot(&mut target, false, Some("boat"));

    // The boat's own silhouette, in the dry scene: every pixel the hull
    // covers. Water appearing on any of these in the wet scene is water
    // painting *over the boat*, which is the whole subject.
    let hull_px = silhouette(&unmasked_nowater, &bg, probe);

    let water_frame = diff_count(&water_only, &bg);
    let water_outside = diff_outside(&water_only, &bg, puddle);
    // The ordering guard: with no water anywhere, a depth-only draw has
    // nothing behind it to hide, so adding the mask must change nothing.
    let mask_eats_hull = diff_count(&masked_nowater, &unmasked_nowater);
    let wet_unmasked = changed_within(&hull_px, &unmasked_water, &unmasked_nowater);
    let wet_masked = changed_within(&hull_px, &masked_water, &masked_nowater);

    // The moved set and its per-pixel confirmation — the original metric this
    // gate was written around, now against a fixture that can produce it.
    let mut moved = 0usize;
    let mut confirmed = 0usize;
    let mut mask_only = 0usize;
    let mut reveal_only = 0usize;
    let (mut bx0, mut by0, mut bx1, mut by1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for i in 0..(W * H) as usize {
        let px = i * 4;
        let (mw, mn) = (&masked_water[px..px + 4], &masked_nowater[px..px + 4]);
        let (uw, un) = (&unmasked_water[px..px + 4], &unmasked_nowater[px..px + 4]);
        if !differs(mw, uw) {
            continue;
        }
        moved += 1;
        let (x, y) = ((i as u32) % W, (i as u32) / W);
        bx0 = bx0.min(x);
        by0 = by0.min(y);
        bx1 = bx1.max(x);
        by1 = by1.max(y);
        match (!differs(mw, mn), differs(uw, un)) {
            (true, true) => confirmed += 1,
            (true, false) => mask_only += 1,
            (false, true) => reveal_only += 1,
            (false, false) => {}
        }
    }

    eprintln!("=== boat water-clip mask pixel gate ===");
    eprintln!(
        "geometry: feet.y={:.4}, water surface y={SURFACE_Y}, hull y {:.4}..{:.4}, \
         patch y {:.4}..{:.4}, hull footprint x {:.3}..{:.3} z {:.3}..{:.3}",
        feet.y,
        hull.aabb_min.y,
        hull.aabb_max.y,
        patch.aabb_min.y,
        patch.aabb_max.y,
        foot_min.x,
        foot_max.x,
        foot_min.y,
        foot_max.y
    );
    eprintln!(
        "probe rect (projected water patch) = x{}..{} y{}..{} ({} px); puddle rect = \
         x{}..{} y{}..{} ({} px); frame {W}x{H}",
        probe.x0,
        probe.x1,
        probe.y0,
        probe.y1,
        probe.area(),
        puddle.x0,
        puddle.x1,
        puddle.y0,
        puddle.y1,
        puddle.area()
    );
    eprintln!(
        "entity draw calls: masked={} unmasked={}",
        masked_stats.draw_calls, unmasked_stats.draw_calls
    );
    eprintln!(
        "puddle vs empty background: {water_frame} px whole frame, {water_outside} px outside \
         the puddle's own projected rect"
    );
    eprintln!("mask with no water in the scene at all = {mask_eats_hull} px");
    eprintln!("hull silhouette inside the probe = {} px", hull_px.len());
    eprintln!("  water paints over the hull, unmasked = {wet_unmasked} px");
    eprintln!("  water paints over the hull, masked   = {wet_masked} px");
    eprintln!(
        "moved (masked vs unmasked, both wet) = {moved} px, bbox x{bx0}..{bx1} y{by0}..{by1}; \
         confirmed={confirmed} mask-only={mask_only} reveal-only={reveal_only}"
    );

    // --- Fixture premises, before any claim about the mask -------------------

    let surface = SURFACE_Y as f32;
    assert!(
        hull.aabb_min.y < surface && hull.aabb_max.y > surface,
        "the hull spans {:.4}..{:.4} against a water surface at {surface} — the boat is not \
         partially submerged, so the water plane never enters its cavity. Measured in that \
         state (feet at SURFACE_Y): the mask changed nothing at all, `reveal == hidden == \
         3016` and the masked and unmasked renders were pixel-identical. See `BOAT_FEET_Y`",
        hull.aabb_min.y,
        hull.aabb_max.y
    );
    assert!(
        patch.aabb_min.y < surface && patch.aabb_max.y > surface,
        "the water patch spans {:.4}..{:.4} against a water surface at {surface} — the mask \
         must straddle the surface it exists to occlude, or its depth write lands behind the \
         water and changes nothing",
        patch.aabb_min.y,
        patch.aabb_max.y
    );
    assert!(
        water_frame > 0,
        "the puddle painted no pixels at all against an empty background — the fixture is not \
         rendering water and everything below would be measuring nothing"
    );
    // A *puddle*, not an ocean — as a containment claim rather than a
    // whole-frame fraction, because a fraction is a threshold and this is an
    // exact property: water may only paint where the region `water_world`
    // fills projects to.
    assert_eq!(
        water_outside, 0,
        "{water_outside} pixels changed when the puddle was added that lie outside the \
         puddle's own projected rect — the fixture is putting water somewhere other than \
         under the boat, which is the background-ocean measurement this gate used to make"
    );
    // …and the containment claim must not be vacuous: it says nothing if the
    // projected rect has been clamped to the whole frame.
    assert!(
        puddle.area() < (W * H) as usize,
        "the puddle's projected rect covers the entire {}x{} frame, so the containment \
         assertion above proves nothing about locality",
        W,
        H
    );
    // The other half of "exactly under the boat": the puddle must contain the
    // hull's whole world footprint, or part of the boat floats over air and
    // the water plane never enters that part of its cavity. Derived from the
    // resolved instance's own AABB and the region `water_world` fills, so
    // neither `PUDDLE_RADIUS` nor `BOAT_YAW` can drift out of agreement
    // silently.
    let (p_lo, p_hi) = (-PUDDLE_RADIUS as f32, (PUDDLE_RADIUS + 1) as f32);
    assert!(
        foot_min.x >= p_lo && foot_max.x <= p_hi && foot_min.y >= p_lo && foot_max.y <= p_hi,
        "the hull's footprint x {:.3}..{:.3} z {:.3}..{:.3} is not inside the puddle's \
         {p_lo}..{p_hi} square — part of the boat is over dry air",
        foot_min.x,
        foot_max.x,
        foot_min.y,
        foot_max.y
    );
    assert!(
        hull_px.len() > 500,
        "the boat's silhouette inside the mask's own rect is only {} px — too small to \
         measure anything through",
        hull_px.len()
    );

    // --- The mask must not occlude the boat it belongs to --------------------

    assert_eq!(
        mask_eats_hull, 0,
        "with no water anywhere in the scene, adding the depth-only water-mask instance \
         changed {mask_eats_hull} pixels — a colour-write-disabled draw can only do that by \
         depth-rejecting geometry drawn after it, i.e. the boat's own hull, which then shows \
         background. Vanilla submits the model first (`AbstractBoatRenderer.submit`: \
         `submitModel(this.model(), ...)` then `this.submitTypeAdditions(...)`), so \
         `prepare_entities` must keep the patch out of visible batches and `gpu/frame.rs` must \
         submit the mask phase only after all visible opaque/cutout geometry. Measured in the \
         wrong order: 5,492 px of hull replaced by sky"
    );

    // --- The mask does its job ----------------------------------------------

    assert!(
        wet_unmasked > 500,
        "without the mask, water painted over only {wet_unmasked} of the boat's {} silhouette \
         pixels — the water plane is not reaching the hull's interior even unmasked, so there \
         is nothing for the mask to hide and every claim below would be vacuous",
        hull_px.len()
    );
    assert!(
        wet_masked * 4 < wet_unmasked,
        "the mask left water painting over {wet_masked} of the boat's silhouette where the \
         unmasked rig shows {wet_unmasked} — the patch covers the hull's whole interior at \
         the waterline, so it must remove the great majority of them. What it correctly does \
         not remove is the submerged *outside* of the hull, which vanilla's patch does not \
         cover either"
    );
    assert!(
        moved > 40,
        "masked and unmasked renders of the same scene (identical hull mesh, identical \
         camera, identical water) differ by only {moved} pixels — the mask is reaching no \
         pixels at all, which is exactly the gap this gate exists to close"
    );
    assert!(
        confirmed as f32 > moved as f32 * 0.5,
        "of {moved} pixels the mask instance moved, only {confirmed} satisfy BOTH halves of \
         the claim (masked stays put when water is added or removed, AND unmasked visibly \
         reveals water) — mask-only={mask_only}, reveal-only={reveal_only}"
    );
    assert!(
        confirmed > 20,
        "fewer than 20 pixels ({confirmed}) fully confirm the mask's effect — too small a \
         sample to trust over rounding noise"
    );
}

/// The negative control: `RaftRenderer` has no `submitTypeAdditions`
/// override, so a raft gets no water mask — `entity_passes.rs`'s
/// `ends_with("_boat")` never matches `"_raft"`.
///
/// # Why this is *not* "a raft must show water changing through its hull"
///
/// That was this test's first form and it is not a property a raft has.
/// Measured on the rebuilt fixture: water paints over **10.5%** of a raft's
/// own silhouette against **43.2%** of an *unmasked boat's* — a raft is a flat
/// slab of logs with no cavity for the water plane to appear inside, so the
/// only water on it is the submerged outside of the hull, which is exactly
/// what a *masked* boat also shows (**6.9%**). The raft's number sits nearer
/// the masked boat's than the unmasked one's, so any threshold separating
/// "masked" from "unmasked" on that axis would have been fitted rather than
/// derived. (The original whole-frame version reported 159,068 px and read as
/// decisive only because it was measuring the background ocean.)
///
/// # What a raft *can* establish, exactly
///
/// The suffix rule fires for `oak_boat` and not for `oak_raft`, in pixels.
/// `"raft"` and `"oak_raft"` resolve to the same corpus rig at the same
/// placement, and neither ends with `"_boat"`, so their renders must be
/// **byte-identical**: zero pixels, not a small number. The `"boat"` /
/// `"oak_boat"` pair through the identical comparison is the positive control
/// that the detector fires at all — without it, "0 px" is what two blank
/// frames also measure.

/// override, so a raft gets no mask — `entity_passes.rs`'s
/// `ends_with("_boat")` never matches `"_raft"`. Made a pixel claim rather
/// than a source one, and scoped to each hull's **own measured silhouette**
/// rather than to the whole frame: the previous whole-frame form reported
/// 159,068 changed pixels, which was the background ocean and would have
/// passed with the raft rendered as one opaque cube.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_raft_gets_no_mask_and_still_shows_real_water_change() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let (_resources, atlas) = load_vanilla();
    let models: &lodestone_render::BlockModels = atlas
        .models()
        .expect("the vanilla load must attach baked block models");

    let rig = lodestone_render::EntityModelSet::load();
    let feet = glam::Vec3::new(BOAT_FEET_X, BOAT_FEET_Y, BOAT_FEET_Z);
    let world = water_world();
    let cam = camera();
    let probe = patch_screen_rect(&rig, feet, BOAT_YAW, cam.view_projection());
    let puddle = puddle_screen_rect(cam.view_projection());

    let shoot = |target: &mut HeadlessTarget, terrain: bool, entity: Option<&str>| {
        render_frame(
            device, queue, format, target, &atlas, &world, models, &cam, terrain, entity,
        )
    };

    let (bg, _) = shoot(&mut target, false, None);
    let (raft_water, raft_stats) = shoot(&mut target, true, Some("oak_raft"));
    let (raft_nowater, _) = shoot(&mut target, false, Some("oak_raft"));
    let (bare_raft_water, _) = shoot(&mut target, true, Some("raft"));
    let (boat_water, _) = shoot(&mut target, true, Some("oak_boat"));
    let (boat_nowater, _) = shoot(&mut target, false, Some("oak_boat"));
    let (bare_boat_water, _) = shoot(&mut target, true, Some("boat"));

    let raft_px = silhouette(&raft_nowater, &bg, probe);
    let boat_px = silhouette(&boat_nowater, &bg, probe);
    let raft_wet = changed_within(&raft_px, &raft_water, &raft_nowater);
    let boat_wet = changed_within(&boat_px, &boat_water, &boat_nowater);
    let raft_outside = diff_outside(&raft_water, &raft_nowater, puddle);

    // The claim: the suffix rule fires for one pair and not the other.
    let raft_pair = diff_count(&raft_water, &bare_raft_water);
    let boat_pair = diff_count(&boat_water, &bare_boat_water);

    eprintln!("=== raft negative-control pixel gate ===");
    eprintln!("raft entity draw calls = {}", raft_stats.draw_calls);
    eprintln!(
        "raft: silhouette {} px, water paints over {raft_wet} px ({:.1}%), {raft_outside} px \
         outside the puddle rect",
        raft_px.len(),
        raft_wet as f32 / raft_px.len().max(1) as f32 * 100.0
    );
    eprintln!(
        "masked boat, same scene: silhouette {} px, water paints over {boat_wet} px ({:.1}%)",
        boat_px.len(),
        boat_wet as f32 / boat_px.len().max(1) as f32 * 100.0
    );
    eprintln!("diff(\"oak_raft\", \"raft\") = {raft_pair} px");
    eprintln!("diff(\"oak_boat\", \"boat\") = {boat_pair} px  (positive control)");

    assert!(
        raft_px.len() > 500,
        "the raft's silhouette is only {} px inside the probe rect — \"identical renders\" \
         is what two blank frames also measure, so nothing below would mean anything",
        raft_px.len()
    );
    assert_eq!(
        raft_outside, 0,
        "{raft_outside} of the raft scene's water-response pixels lie outside the puddle's \
         own projected rect — the fixture has water somewhere other than under the raft, \
         which is exactly the background-ocean measurement this control used to be"
    );
    // The positive control, first: the comparison must be able to see a mask.
    assert!(
        boat_pair > 40,
        "\"oak_boat\" and \"boat\" render within {boat_pair} px of each other in this scene \
         — the suffix rule is not adding a mask for either, so the raft's zero below would \
         prove nothing"
    );
    assert_eq!(
        raft_pair, 0,
        "\"oak_raft\" and \"raft\" differ by {raft_pair} px — they resolve to the same rig at \
         the same placement and neither ends with \"_boat\", so something is submitting a \
         water mask for a raft. `RaftRenderer` has no `submitTypeAdditions` override at all"
    );
}
