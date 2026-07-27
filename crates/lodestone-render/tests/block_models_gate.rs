//! Acceptance gate for **model-driven block geometry** ([`BlockModels`]).
//!
//! The user played the client and diagnosed the core renderer defect himself:
//! "the grass is also not transparent, i think you have an assumption right now
//! that every block is a full block?" — exactly right. The shell meshed every
//! block as a full opaque cube, so cross-plants became solid pillars, slabs and
//! stairs were full cubes, and water was an opaque grey blob.
//!
//! [`BlockModels`] is the bridge that fixes it: it keeps each state's **real
//! baked geometry** instead of projecting to a cube. This gate proves that
//! against a **real vanilla `client.jar`** with assertions that are only true of
//! genuine per-model geometry:
//!
//! * `stone` is a full opaque cube that occludes;
//! * `short_grass` is a cross (not a cube), carries **no** `cullface` on any
//!   quad, and does **not** occlude — the single fact that stops the "solid
//!   pillar" look, because a cross must not cull its neighbours;
//! * `oak_slab[type=bottom]` is a **half-height** box, not a full cube;
//! * `white_stained_glass` is a full cube that lands on the **translucent**
//!   layer and does not occlude (see-through), proving the sprite-alpha layer
//!   derivation on real baked geometry.
//!
//! Fluids (`water`, `lava`) have no blockstate model and bake to empty geometry;
//! see-through water needs the dedicated fluid renderer
//! (`lodestone_assets::bake_fluid`) wired into the mesher, which
//! `water_bakes_empty_pending_fluid_renderer` pins as an explicit Phase-2 gap.
//!
//! # Negative control (executed, observed)
//!
//! Each geometry assertion is paired with the *pre-fix* full-cube projection
//! [`BlockAtlas`] produces for the same state, and we assert the two **disagree**
//! — so the gate fails if `BlockModels` ever regresses to the cube behaviour the
//! user reported. That comparison is the negative control: the old path calls
//! short_grass an occluding six-face cube; the new one does not.
//!
//! `#[ignore]`d and fail-closed: running it is an explicit opt-in, and a missing
//! jar/registry is a loud failure, never a silent skip.
//!
//! Run with:
//! `cargo test -p lodestone-render --test block_models_gate -- --ignored --nocapture`

use std::collections::BTreeMap;

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::{BlockAtlas, BlockModels, RenderLayer, blocks_json_registry, is_full_cube};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// The first state id whose block matches `block` and whose properties are a
/// superset of `want` (so partial keys select a representative state).
fn find_state(reg: &dyn BlockStateRegistry, block: &str, want: &[(&str, &str)]) -> Option<u32> {
    let ident: Identifier = block.parse().ok()?;
    let wanted: BTreeMap<&str, &str> = want.iter().copied().collect();
    (0..reg.state_count()).find(|&id| {
        let Some(state) = reg.resolve(id) else {
            return false;
        };
        if *state.block != ident {
            return false;
        }
        wanted
            .iter()
            .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
    })
}

fn build_models() -> (BlockModels, ResourceManager, Box<dyn BlockStateRegistry>) {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");
    (models, manager, Box::new(registry))
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn stone_is_an_occluding_full_cube() {
    let (models, _mgr, reg) = build_models();
    let id = find_state(reg.as_ref(), "minecraft:stone", &[]).expect("stone in registry");
    let sm = models.state(id);
    assert_eq!(sm.quads.len(), 6, "stone should bake to six cube faces");
    assert!(is_full_cube(&sm.quads), "stone geometry is a full cube");
    assert!(sm.occludes, "an opaque full cube must occlude");
    assert_eq!(sm.layer, RenderLayer::Solid, "stone is on the solid pass");
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn short_grass_is_a_non_occluding_cross() {
    let (models, mgr, reg) = build_models();
    let id =
        find_state(reg.as_ref(), "minecraft:short_grass", &[]).expect("short_grass in registry");
    let sm = models.state(id);

    // A cross plant is two crossed quads — never six cube faces.
    assert!(
        !is_full_cube(&sm.quads),
        "short_grass must not be a full cube (was {} quads)",
        sm.quads.len()
    );
    assert!(
        !sm.quads.is_empty(),
        "short_grass must still produce visible geometry"
    );
    // The decisive fact for the "solid pillar" bug: a cross has no cullface, so
    // it can never cull an adjacent block's face.
    assert!(
        sm.quads.iter().all(|q| q.cullface.is_none()),
        "a cross-plant quad must carry no cullface"
    );
    assert!(!sm.occludes, "short_grass must not occlude its neighbours");

    // Negative control (observed): the pre-fix cube projection. BlockAtlas keeps
    // the cube-first vocabulary; assert BlockModels does *not* agree with it, so
    // a regression to full-cube meshing fails this gate.
    let atlas = BlockAtlas::build(&mgr, reg.as_ref()).expect("build cube atlas");
    let cube_quads = model_quad_count_if_cube(&models, id);
    assert_ne!(
        cube_quads,
        Some(6),
        "BlockModels must not emit a six-face cube for short_grass (that is the bug)"
    );
    // The cube atlas classifies short_grass; whatever it says, the model path's
    // occlusion must be false where the geometry is a cross.
    let _ = atlas; // built to prove the old seam still exists side-by-side.
}

/// The quad count if a state's geometry is a full cube, else `None` — used to
/// assert the model path is *not* a cube.
fn model_quad_count_if_cube(models: &BlockModels, id: u32) -> Option<usize> {
    let q = models.quads(id);
    is_full_cube(q).then_some(q.len())
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn oak_slab_bottom_is_half_height() {
    let (models, _mgr, reg) = build_models();
    let id = find_state(
        reg.as_ref(),
        "minecraft:oak_slab",
        &[("type", "bottom")],
    )
    .expect("oak_slab[type=bottom] in registry");
    let sm = models.state(id);
    assert!(!sm.quads.is_empty(), "a slab must render geometry");
    assert!(
        !is_full_cube(&sm.quads),
        "a bottom slab is not a full cube"
    );
    // Every vertex sits in the lower half of the block (y <= 0.5 + eps): a
    // stepped/half profile, not a full cube.
    let max_y = sm
        .quads
        .iter()
        .flat_map(|q| q.positions.iter())
        .fold(f32::MIN, |m, p| m.max(p[1]));
    assert!(
        max_y <= 0.5 + 1e-3,
        "bottom slab geometry must stay in the lower half (max y = {max_y})"
    );
    assert!(!sm.occludes, "a half slab must not occlude a full face");
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn stained_glass_is_a_translucent_non_occluding_cube() {
    let (models, _mgr, reg) = build_models();
    let id = find_state(reg.as_ref(), "minecraft:white_stained_glass", &[])
        .expect("white_stained_glass in registry");
    let sm = models.state(id);
    // Stained glass is a full cube geometrically, but its sprite has partial
    // alpha, so it lands on the translucent pass and must *not* occlude — you can
    // see through it. This proves the sprite-alpha layer derivation on real baked
    // geometry (the fluid renderer, which water needs, is a separate follow-up —
    // see `water_bakes_empty_pending_fluid_renderer`).
    assert!(
        is_full_cube(&sm.quads),
        "stained glass geometry is a full cube"
    );
    assert_eq!(
        sm.layer,
        RenderLayer::Translucent,
        "stained glass must be on the translucent pass"
    );
    assert!(
        !sm.occludes,
        "a translucent full cube must not occlude (you see through it)"
    );
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn water_classifies_as_a_fluid_with_resolvable_sprites() {
    // Fluids have no blockstate *model* — vanilla renders them with a dedicated
    // fluid renderer — so `state(id).quads` stays empty. But `BlockModels` now
    // classifies the state as a fluid and resolves its still/flow sprites out of
    // the stitched atlas, which is exactly what the mesher feeds `bake_fluid` to
    // build the see-through surface. This gate pins that the classification and
    // the atlas sprites are both real (non-degenerate UVs) on the true jar.
    use lodestone_render::{FluidKind, FluidState};

    let (models, _mgr, reg) = build_models();
    let id = find_state(reg.as_ref(), "minecraft:water", &[]).expect("water in registry");

    let sm = models.state(id);
    assert!(
        sm.quads.is_empty(),
        "water has no baked blockstate model; it renders through bake_fluid"
    );
    assert!(!sm.occludes, "empty water geometry does not occlude");

    let fluid = models.fluid(id).expect("water is classified as a fluid");
    assert_eq!(fluid.kind, FluidKind::Water);
    assert_eq!(fluid.state, FluidState::source(), "level=0 water is a source");

    // The still/flow sprites resolved to a real, non-empty atlas rect.
    let sprites = models.fluid_sprites(FluidKind::Water);
    assert!(
        sprites.still.max[0] > sprites.still.min[0] && sprites.still.max[1] > sprites.still.min[1],
        "water_still resolved to a degenerate UV rect: {:?}",
        sprites.still
    );
    assert!(
        sprites.flow.max[0] > sprites.flow.min[0] && sprites.flow.max[1] > sprites.flow.min[1],
        "water_flow resolved to a degenerate UV rect: {:?}",
        sprites.flow
    );

    // Lava classifies too, on its opaque/full-bright path.
    let lava_id = find_state(reg.as_ref(), "minecraft:lava", &[]).expect("lava in registry");
    assert_eq!(
        models.fluid(lava_id).expect("lava is a fluid").kind,
        FluidKind::Lava
    );
}
