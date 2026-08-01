//! Proves `ChunkWorld` reads the real 26.2 path-type/collision census
//! (`lodestone_data::path_types` + `collision_shapes`, issue #204) instead of
//! guessing solid/air, and — per this project's evidence standard — that the
//! difference actually changes *where a mob paths*, not just what a lookup
//! returns.
//!
//! `ChunkColumn` (this crate's terrain storage, `src/chunk.rs`) already
//! carries full canonical vanilla block-state strings, not a solid/air bit —
//! `ChunkWorld` was the one place still throwing that fidelity away by asking
//! only "are you solid?". The scene below is deliberately **not**
//! stone-and-air: per CLAUDE.md's "world species" trap, a scene built only
//! from those two states cannot distinguish the old mapping from the new one
//! no matter how green the test looks, because they agree on both. It
//! contains real lava (`minecraft:lava[level=0]`), which the two mappings
//! read as opposite polarities: the census says `PathType::Lava` (malus
//! `-1`, impassable to a land mob), the old solid/air view says `Open`
//! (lava is a fluid, so `ChunkColumn::is_solid` reads it as "not solid").
//!
//! The control is a hand-built [`PathWorld`] ([`SolidAirOnly`]) that
//! reproduces `ChunkWorld`'s **pre-#204** mapping exactly (full-cell solid ⇒
//! `Blocked`/`1.0`, everything else ⇒ `Open`/`0.0`) over the *same* terrain.
//! Re-running the identical search through it and asserting a *different*
//! path is the "assertions of an absence need a control proving the detector
//! would have fired" standard applied directly: without it, a green
//! "detours around lava" test would only prove the pathfinder ran, not that
//! the census changed its answer.

use lodestone_entity::pathfinding::{
    Aabb, MobShape, PathFinder, PathParams, PathStart, PathType, PathWorld,
};
use lodestone_model::BlockPos;
use lodestone_server::ChunkWorld;

/// Canonical 26.2 block-state strings for cells the census classifies
/// distinctly from a solid/air guess. Verified directly against each block's
/// default state in `.cache/mc/26.2/generated/reports/blocks.json` (Mojang's
/// own report — the authoritative source per `CLAUDE.md`), and cross-checked
/// against `lodestone_data::path_types`/`collision_shapes` at the id each
/// resolves to:
///
/// | state | id | `path_type` | collision top |
/// |---|---|---|---|
/// | `lava[level=0]` | 102 | `Lava` (malus `-1`) | `0.0` |
/// | `water[level=0]` | 86 | `Water` (malus `8`) | `0.0` |
/// | `oak_fence[...]` (unconnected) | 6996 | `Fence` (malus `-1`) | `1.5` |
/// | `oak_slab[type=bottom,...]` | 13333 | `Blocked` | `0.5` |
/// | `stone` | 1 | `Blocked` | `1.0` |
/// | `air` | 0 | `Open` | `0.0` |
const LAVA: &str = "minecraft:lava[level=0]";
const WATER: &str = "minecraft:water[level=0]";
const OAK_FENCE: &str =
    "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]";
const OAK_SLAB_BOTTOM: &str = "minecraft:oak_slab[type=bottom,waterlogged=false]";

/// Reproduces `ChunkWorld`'s **pre-#204** `PathWorld` impl exactly: every
/// solid (non-air, non-fluid) cell is `PathType::Blocked` with a full-cell
/// (`1.0`) collision top, everything else is `PathType::Open`/`0.0` — byte
/// for byte what `mobs.rs` did before this fix. `collides` is untouched by
/// #204 either way (still `ChunkWorld::is_solid`-based), so it delegates
/// straight through.
struct SolidAirOnly<'w>(&'w ChunkWorld);

impl PathWorld for SolidAirOnly<'_> {
    fn min_y(&self) -> i32 {
        self.0.min_y()
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if self.0.is_solid(x, y, z) {
            PathType::Blocked
        } else {
            PathType::Open
        }
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if self.0.is_solid(x, y, z) { 1.0 } else { 0.0 }
    }

    fn collides(&self, aabb: Aabb) -> bool {
        self.0.collides(aabb)
    }

    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// Floor plus a lava band from `x=-4..=4` at `z=3`, open ground beyond both
/// ends — the exact shape issue #204 names as its own suggested case: "a mob
/// routing around a lava pool it could otherwise walk through." One row of
/// lava is enough to fully block lateral crossing (unlike a *solid* wall,
/// which needs two rows to stop a mob jumping onto its top — see
/// `tests/mob_sim.rs`'s `walled_world` — lava has no collision top to land
/// on at all, and the search graph only steps one block at a time, so it
/// cannot skip over an adjacent impassable cell in a single move).
fn lava_band_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -8..=8 {
        for z in -2..=12 {
            world.set_solid(x, -1, z, true); // floor, surface at y=0
        }
    }
    for x in -4..=4 {
        world.set_block(x, 0, 3, LAVA);
    }
    world
}

fn run_search(world: &dyn PathWorld, target: BlockPos) -> lodestone_entity::pathfinding::Path {
    let mob = MobShape::land(0.6, 1.95);
    let start = PathStart::grounded(0.5, 0.0, 0.5);
    let params = PathParams {
        max_path_length: 200.0,
        reach_range: 1,
        visited_multiplier: 1.0,
    };
    PathFinder::new(20_000)
        .find_path(world, &mob, start, &[target], params)
        .expect("a start node exists over this terrain — the search must return something")
}

#[test]
fn real_census_forces_a_lava_detour_the_old_solid_air_model_would_walk_straight_through() {
    let world = lava_band_world();
    let target = BlockPos::new(0, 0, 8);

    // Ground truth first: ChunkColumn really does carry real block states here
    // (not a solid/air-only scene, which cannot distinguish the two mappings
    // no matter how green the test looks — CLAUDE.md's "world species" trap).
    assert_eq!(
        world.base_path_type(0, 0, 3),
        PathType::Lava,
        "expected the real census to classify the trench cell as Lava"
    );
    let old = SolidAirOnly(&world);
    assert_eq!(
        old.base_path_type(0, 0, 3),
        PathType::Open,
        "the pre-#204 solid/air model reads lava as a fluid == not solid == \
         Open — this is the exact miscount issue #204 reports, reproduced here \
         as the test's control premise"
    );

    // Real census: lava carries malus -1 (impassable to a land mob), so the
    // search must detour around the band's end, past x=4 or x=-4.
    let real_path = run_search(&world, target);
    let real_max_abs_x = real_path.nodes().iter().map(|n| n.x.abs()).max().unwrap_or(0);
    assert!(real_path.reached(), "real-census path did not reach the target");
    assert!(
        real_max_abs_x > 4,
        "real-census path never detoured around the lava band (max |x| = \
         {real_max_abs_x}) — did it walk through the lava?"
    );

    // The control that matters: the identical search, over the identical
    // terrain, through the pre-fix solid/air mapping, must find a *different*
    // (undetoured, straight-through) path — proving the census genuinely
    // changes pathfinder behaviour rather than merely a lookup's return value.
    let old_path = run_search(&old, target);
    let old_max_abs_x = old_path.nodes().iter().map(|n| n.x.abs()).max().unwrap_or(0);
    assert!(
        old_path.reached(),
        "solid/air-model control path did not reach the target"
    );
    assert!(
        old_max_abs_x <= 1,
        "expected the solid/air control to walk straight through the lava \
         (max |x| = {old_max_abs_x}) — if it also detours, the control's own \
         premise is false and this test proves nothing about the fix"
    );
    assert!(
        real_path.nodes().len() > old_path.nodes().len(),
        "the detour should also be the longer route: real={} nodes, old={} nodes",
        real_path.nodes().len(),
        old_path.nodes().len()
    );
}

/// `ChunkWorld::collision_top` used to be a hardcoded `1.0`/`0.0` regardless
/// of what was actually there. This pins it to the real per-state shape max
/// (`WalkNodeEvaluator.getFloorLevel`,
/// `.cache/mc/26.2/src/net/minecraft/world/level/pathfinder/WalkNodeEvaluator.java:219-222`:
/// `shape.isEmpty() ? 0.0 : shape.max(Direction.Axis.Y)`) for four states a
/// full-cell assumption gets wrong in three different directions: a slab
/// (shorter than a full cell), a fence (taller), and a fluid (no collision at
/// all despite not being air).
#[test]
fn collision_top_reads_the_real_per_state_shape_not_a_hardcoded_full_cell() {
    let mut world = ChunkWorld::new(-4, 24);
    world.set_solid(0, 0, 0, true); // stone: full cube
    world.set_block(1, 0, 0, OAK_SLAB_BOTTOM);
    world.set_block(2, 0, 0, OAK_FENCE);
    world.set_block(3, 0, 0, WATER);
    // (4, 0, 0) is left air by construction.

    assert_eq!(world.collision_top(0, 0, 0), 1.0, "full cube must stay 1.0");
    assert_eq!(
        world.collision_top(1, 0, 0),
        0.5,
        "a bottom slab is half a cell, not the old hardcoded full cell"
    );
    assert_eq!(
        world.collision_top(2, 0, 0),
        1.5,
        "a fence reaches 1.5 — the reason a 0.6 step height cannot mount it; \
         clamping this to 1.0 would silently make fences step-able"
    );
    assert_eq!(
        world.collision_top(3, 0, 0),
        0.0,
        "water has no collision shape at all, despite not being air"
    );
    assert_eq!(world.collision_top(4, 0, 0), 0.0, "air stays 0.0");

    // And the base_path_type census differentiates the same four cells --
    // the two questions issue #204 asked ChunkWorld to answer for real.
    assert_eq!(world.base_path_type(0, 0, 0), PathType::Blocked);
    assert_eq!(
        world.base_path_type(1, 0, 0),
        PathType::Blocked,
        "vanilla's WalkNodeEvaluator classifies a plain slab as Blocked too -- \
         it is the *collision top*, not the path type, where a slab differs \
         from a full cube"
    );
    assert_eq!(world.base_path_type(2, 0, 0), PathType::Fence);
    assert_eq!(world.base_path_type(3, 0, 0), PathType::Water);
    assert_eq!(world.base_path_type(4, 0, 0), PathType::Open);

    // `is_water` (no longer hardcoded false) agrees with the census.
    assert!(world.is_water(3, 0, 0));
    assert!(!world.is_water(0, 0, 0));
    assert!(!world.is_water(4, 0, 0));
}
