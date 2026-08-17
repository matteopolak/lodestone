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
//! Four renders through one real water-filled world and one real camera:
//! `masked_water` (`"oak_boat"`, water uploaded), `masked_nowater` (same
//! entity, terrain skipped), `unmasked_water` (`"boat"`, water uploaded),
//! `unmasked_nowater` (same, terrain skipped). The **moved set** is every
//! pixel differing between `masked_water` and `unmasked_water` — by
//! construction (identical camera, identical terrain, identical hull mesh)
//! this can only be the hollow-interior region the mask does or does not
//! occlude. Within that set, a working mask predicts two things at once,
//! checked per pixel: `masked_water` must stay close to `masked_nowater`
//! (masking hides the gap regardless of whether water exists to hide) and
//! `unmasked_water` must move far from `unmasked_nowater` (revealing water
//! is what a *missing* mask does). Both halves are asserted, mirroring
//! `first_person_banner_hand_pixels.rs`'s moved/unmoved split — a mask that
//! merely dims everything, or one bound to the wrong pipeline entirely,
//! cannot satisfy both.
//!
//! # The negative control
//!
//! `RaftRenderer` has no `submitTypeAdditions` override, so
//! `boat_water_patch_model`'s own doc records rafts get no mask at all —
//! already checked at the CPU level
//! (`entity_passes.rs`'s `ends_with("_boat")` never matches `"_raft"`). This
//! file makes it a pixel one too: an `"oak_raft"` floating on the same water,
//! same camera, must show a real, non-trivial pixel change between water
//! present and absent, exactly like the unmasked boat.
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
/// gate needs a lake big enough to fill the frame around one boat, not a
/// realistic view distance.
const RD_CHUNKS: i32 = 3;
const MIN_Y: i32 = 0;
/// Water fills `[MIN_Y, SURFACE_Y)`; the boat floats with its feet exactly at
/// the surface, matching how `distant_flat_terrain_holes.rs` stands a player
/// on `SURFACE_Y`. One section of water plus one of headroom for the boat and
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

/// A flat lake of radius [`RD_CHUNKS`] chunks, fully loaded before any
/// section is meshed, under open daylight.
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
    let lo = -RD_CHUNKS * 16;
    let hi = RD_CHUNKS * 16 + 15;
    let written = world.fill_region([lo, MIN_Y, lo], [hi, SURFACE_Y - 1, hi], water);
    assert!(written > 0, "fixture: fill_region must actually write water");
    world
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
        feet: glam::Vec3::new(BOAT_FEET_X, SURFACE_Y as f32, BOAT_FEET_Z),
        yaw: BOAT_YAW,
        head_yaw: BOAT_YAW,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: None,
        block_state: None,
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
    }
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
    entity_type_path: &str,
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
    let boat = boat_draw(entity_type_path);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, std::slice::from_ref(&boat));
    (target.read_texels(device, queue), stats)
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

    // Premise, asserted directly rather than assumed: `"boat"` and
    // `"oak_boat"` must resolve to the identical rig, or this A/B is not
    // isolating the mask at all.
    {
        let rig = lodestone_render::EntityModelSet::load();
        let anim = AnimInput::REST;
        let feet = glam::Vec3::new(BOAT_FEET_X, SURFACE_Y as f32, BOAT_FEET_Z);
        let bare = rig
            .resolve_animated("boat", feet, BOAT_YAW, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect("\"boat\" must resolve");
        let real = rig
            .resolve_animated("oak_boat", feet, BOAT_YAW, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect("\"oak_boat\" must resolve");
        assert_eq!(
            bare.model, real.model,
            "\"boat\" and \"oak_boat\" must resolve to the same corpus rig for this A/B to \
             isolate only the water-mask instance"
        );
        assert_eq!(
            bare.transform, real.transform,
            "\"boat\" and \"oak_boat\" must place identically for this A/B to isolate only \
             the water-mask instance"
        );
    }

    let world = water_world();
    let cam = camera();

    let (masked_water, masked_water_stats) =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &cam, true, "oak_boat");
    let (masked_nowater, _) =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &cam, false, "oak_boat");
    let (unmasked_water, unmasked_water_stats) =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &cam, true, "boat");
    let (unmasked_nowater, _) =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &cam, false, "boat");

    // The moved set: by construction (identical camera, identical terrain,
    // identical hull mesh) the only geometry difference between
    // `masked_water` and `unmasked_water` is whether the water-clip mask
    // instance was submitted, so any pixel that differs between them is
    // attributable to the mask alone.
    let mut moved: Vec<usize> = Vec::new();
    for (i, (a, b)) in masked_water.chunks_exact(4).zip(unmasked_water.chunks_exact(4)).enumerate() {
        if differs(a, b) {
            moved.push(i);
        }
    }

    let mut confirmed = 0usize;
    let mut mask_holds_but_reveal_fails = 0usize;
    let mut reveal_holds_but_mask_fails = 0usize;
    for &i in &moved {
        let px = i * 4;
        let mw = &masked_water[px..px + 4];
        let mn = &masked_nowater[px..px + 4];
        let uw = &unmasked_water[px..px + 4];
        let un = &unmasked_nowater[px..px + 4];
        let mask_holds = !differs(mw, mn);
        let reveal_holds = differs(uw, un);
        match (mask_holds, reveal_holds) {
            (true, true) => confirmed += 1,
            (true, false) => mask_holds_but_reveal_fails += 1,
            (false, true) => reveal_holds_but_mask_fails += 1,
            (false, false) => {}
        }
    }

    let total_masked_vs_nowater = diff_count(&masked_water, &masked_nowater);
    let total_unmasked_vs_nowater = diff_count(&unmasked_water, &unmasked_nowater);

    eprintln!("=== boat water-clip mask pixel gate ===");
    eprintln!(
        "masked_water: third_person_body_drawn={} entity draw_calls={}",
        masked_water_stats.third_person_body_drawn, masked_water_stats.draw_calls
    );
    eprintln!(
        "unmasked_water: third_person_body_drawn={} entity draw_calls={}",
        unmasked_water_stats.third_person_body_drawn, unmasked_water_stats.draw_calls
    );
    eprintln!(
        "moved (masked_water vs unmasked_water) = {} px, of which confirmed={confirmed} \
         (mask holds + reveal fires), mask-only={mask_holds_but_reveal_fails}, \
         reveal-only={reveal_holds_but_mask_fails}",
        moved.len()
    );
    eprintln!(
        "whole-frame diff masked_water vs masked_nowater   = {total_masked_vs_nowater} px \
         (includes legitimate water beside the hull, not just the interior)"
    );
    eprintln!(
        "whole-frame diff unmasked_water vs unmasked_nowater = {total_unmasked_vs_nowater} px"
    );

    assert!(
        moved.len() > 40,
        "masked_water and unmasked_water (identical hull mesh, identical camera, identical \
         terrain, differing only in whether the water-clip mask instance was submitted) are \
         pixel-identical to within a rounding wobble ({} px moved) — the mask is reaching no \
         pixels at all, which is exactly the gap this gate exists to close",
        moved.len()
    );
    assert!(
        confirmed as f32 > moved.len() as f32 * 0.5,
        "of {} pixels the mask instance moved, only {confirmed} satisfy BOTH halves of the \
         claim (masked stays put when water is added/removed, AND unmasked visibly reveals \
         water) — mask-only (moved but water never actually shows through when unmasked) = \
         {mask_holds_but_reveal_fails}, reveal-only (water shows through even with the mask \
         present) = {reveal_holds_but_mask_fails}. A working mask should dominate this set.",
        moved.len()
    );
    assert!(
        confirmed > 20,
        "fewer than 20 pixels ({confirmed}) fully confirm the mask's effect — too small a \
         sample to trust over rounding noise"
    );
}

/// The negative control: `RaftRenderer` has no `submitTypeAdditions`
/// override (`boat_water_patch_model`'s own doc), so a raft gets none of
/// this and must still show water changing through its own hollow hull —
/// the CPU-level check (`entity_passes.rs`'s `ends_with("_boat")` never
/// matching `"_raft"`) made into a pixel one.
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

    let world = water_world();
    let cam = camera();

    let (raft_water, raft_water_stats) =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &cam, true, "oak_raft");
    let (raft_nowater, _) =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &cam, false, "oak_raft");

    let raft_diff = diff_count(&raft_water, &raft_nowater);

    eprintln!("=== raft negative-control pixel gate ===");
    eprintln!(
        "raft_water: third_person_body_drawn={} entity draw_calls={}",
        raft_water_stats.third_person_body_drawn, raft_water_stats.draw_calls
    );
    eprintln!("diff(raft_water, raft_nowater) = {raft_diff} px");

    assert!(
        raft_diff > 200,
        "a raft floating on real water should show a large, real pixel change between water \
         present and absent (no mask protects any part of it, hull included) — got only \
         {raft_diff} px, which reads as the fixture not actually seeing water at all"
    );
}
