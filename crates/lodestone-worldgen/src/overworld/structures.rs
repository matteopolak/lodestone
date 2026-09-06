//! Stages 0a/0b of [`OverworldGenerator::column`] — `structure_starts` and
//! `structure_refs`, the two stages that run *before* terrain.
//!
//! # Why these are the topmost stages, not the last ones
//!
//! Vanilla's `ChunkStatus` order is `STRUCTURE_STARTS → STRUCTURE_REFERENCES →
//! BIOMES → NOISE → …`, and the reason is the beardifier: `NoiseChunk`'s fill
//! consults the structure bounds intersecting the chunk to flatten terrain
//! underneath them, so the bounds have to exist before a single density sample is
//! taken. This inverts the intuition that structures are placed *on* terrain,
//! and getting it backwards is not a small error — it is the difference between
//! a village on flat ground and a village draped over a hillside.
//!
//! In this engine the fill lives inside
//! [`OverworldGenerator::pre_ore_stage`], so the two stages here sit *above*
//! `pre_ore` in [`super::ChunkStages`] and `pre_ore` gains exactly one upstream
//! edge: it reads its own chunk's [`StructureRefs`]. The store's stage rule ("add
//! a stage above the ones it consumes") is therefore satisfied, and the
//! reentrancy trap is avoided because neither stage here reads any terrain
//! product — [`StartSampler`] samples a *fresh* noise column, exactly as
//! vanilla's `getBaseColumn` does, which is what makes the layering acyclic
//! rather than merely conventional.
//!
//! # What it costs
//!
//! `structure_starts` is ~20 structure-set placement predicates (two to four
//! legacy RNG draws each) and, only on the rare chunk where a placement fires, a
//! handful of column samples. `structure_refs` is at most 289 store probes over
//! the 17×17 neighbourhood (`ChunkStatus`'s STRUCTURE_REFERENCES radius 8), each
//! an `Arc` clone. Neither touches a block, and for a chunk with no
//! adaptation-bearing start in reach `refs` produces an empty list and
//! `pre_ore`'s beardifier context stays the constant-zero leaf it is today —
//! which is why this unit is bit-identical on output.
//!
//! # How to change it
//!
//! * The beardifier itself is **not** here (S3). What is here is the *seam*:
//!   [`StructureRefs`] is the product S3's evaluator consumes, and it already
//!   filters the way vanilla's `Beardifier.forStructuresInChunk` filters
//!   (adaptation `!= NONE`, within 12 blocks of the chunk). Widen that filter and
//!   you widen the halo the join scheduler has to lead by.
//! * [`StartSampler`]'s height scan builds a whole [`AquiferSystem`] per
//!   candidate chunk. That is deliberate — it is the cheapest thing that is
//!   *exactly* vanilla's column — but it means a structure kind that samples many
//!   scattered columns (mineshaft's mesa arm, ruined portals' corner heights)
//!   wants the sampler to cache per chunk, which it does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, get_seed};

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::structure::{HeightmapKind, PieceRefinement, StartContext, StructureStart, VerticalPlacement};

use super::OverworldGenerator;

/// One chunk's structure references: every start whose *adjusted* bounding box
/// comes within 12 blocks of this chunk, paired with the chunk that owns it.
///
/// This is both halves of vanilla's `structures.References` (which chunks' starts
/// this chunk participates in) and the beardifier's input
/// (`Beardifier.forStructuresInChunk`). Kept as one product because they are the
/// same walk over the same 17×17 neighbourhood, and computing them separately
/// would be two chances to disagree about the reach.
#[derive(Debug, Default)]
pub struct StructureRefs {
    /// `(owning chunk x, owning chunk z, start)`, in neighbourhood scan order.
    pub entries: Vec<(i32, i32, Arc<StructureStart>)>,
}

impl StructureRefs {
    /// The `References` NBT view: structure id → the packed chunk keys of the
    /// chunks whose starts this chunk references, deduplicated and sorted.
    ///
    /// Vanilla writes a `long[]` per structure id; `ChunkPos.pack` is
    /// `(z as u32 as i64) << 32 | (x as u32 as i64)`.
    #[must_use]
    pub fn packed_by_structure(&self) -> std::collections::BTreeMap<String, Vec<i64>> {
        let mut out: std::collections::BTreeMap<String, Vec<i64>> =
            std::collections::BTreeMap::new();
        for (cx, cz, start) in &self.entries {
            let packed =
                (i64::from(*cz as u32) << 32) | i64::from(*cx as u32);
            let list = out.entry(start.structure.clone()).or_default();
            if !list.contains(&packed) {
                list.push(packed);
            }
        }
        for list in out.values_mut() {
            list.sort_unstable();
        }
        out
    }

    /// The starts the beardifier evaluates: adaptation-bearing, piece-complete,
    /// and in reach.
    ///
    /// **Which structures reach this** is the honest measure of how much of S3 is
    /// observable in a generated world: only the seven adaptation-bearing kinds
    /// with a landed piece generator do. See `docs/worldgen-beardifier.md` for the
    /// current list — while every adaptation-bearing kind is still jigsaw (S4) or
    /// coded (S5), this iterator is empty for every chunk and the fill stage takes
    /// its no-beard branch, which is exactly what the negative control asserts.
    #[must_use]
    pub fn adaptation_bearing(&self) -> impl Iterator<Item = &Arc<StructureStart>> {
        self.entries
            .iter()
            .map(|(_, _, start)| start)
            .filter(|start| {
                start.pieces_complete
                    && start.terrain_adaptation != crate::structure::TerrainAdjustment::None
            })
    }
}

/// Chebyshev chunk radius `structure_refs` reads `structure_starts` over —
/// vanilla's `ChunkGenerator.createReferences`' hardcoded `int range = 8`, i.e.
/// a 17×17 neighbourhood.
pub const REFS_RADIUS: i32 = 8;

/// The reach `Beardifier.forStructuresInChunk` keeps a start at
/// (`isCloseToChunk(chunkPos, 12)`), and the amount
/// `Structure.adjustBoundingBox` inflates an adaptation-bearing box by. Same
/// number twice in vanilla, and it is the same number for the same reason.
pub const BEARD_REACH: i32 = 12;

/// How far a ruined portal's post-template terrain pass can write beyond its
/// frame. Structure references normally need only the piece box (plus the
/// beardifier halo), but this pass must also reach neighbouring grids so each
/// can regenerate and clip its portion of the skirt.
const PORTAL_TERRAIN_REACH: i32 = 14;

/// [`StartContext`] over freshly sampled noise columns.
///
/// Holds no terrain product and reads no store slot, which is what keeps
/// `structure_starts` above `pre_ore` instead of circular. The per-chunk
/// [`AquiferSystem`] cache exists because building one is the expensive part and
/// a structure predicate asks about several columns of the same chunk.
struct StartSampler<'a> {
    generator: &'a OverworldGenerator,
    /// `(cx, cz)` → that chunk's aquifer. `RefCell` because [`StartContext`]
    /// takes `&self` (it is called from a `&dyn` behind the registry) and this is
    /// single-threaded per stage invocation.
    aquifers: RefCell<HashMap<(i32, i32), Arc<AquiferSystem>>>,
}

impl StartSampler<'_> {
    fn aquifer(&self, cx: i32, cz: i32) -> Arc<AquiferSystem> {
        if let Some(existing) = self.aquifers.borrow().get(&(cx, cz)) {
            return Arc::clone(existing);
        }
        // Counted separately from the fill path's aquifers: `build_aquifer` bumps
        // `stage_entered[Aquifer]`, which the calibration bench predicts as the
        // pre-ore closure size, and this one is not part of that closure.
        crate::counters::bump_structure_aquifer();
        let built = Arc::new(self.generator.build_aquifer(cx, cz));
        self.aquifers
            .borrow_mut()
            .insert((cx, cz), Arc::clone(&built));
        built
    }
}

impl StartContext for StartSampler<'_> {
    /// `NoiseBasedChunkGenerator.getFirstOccupiedHeight` — `getBaseHeight - 1`,
    /// i.e. the Y of the topmost block satisfying the heightmap predicate.
    ///
    /// Vanilla scans a 1-cell `NoiseChunk` from the top down and returns
    /// `posY + 1` for the first match, `minY` for none; the `-1` in
    /// `getFirstOccupiedHeight` cancels the `+1`, so the answer is the matching
    /// block's own Y (and `minY - 1` when the column never matches).
    ///
    /// Interpolation cells are 4 blocks wide and globally aligned, so reading
    /// this out of the *chunk*-wide aquifer gives the same value as vanilla's
    /// cell-wide one. The heightmap predicates come from `Heightmap.Types`:
    /// `WORLD_SURFACE_WG` is `NOT_AIR`, `OCEAN_FLOOR_WG` is
    /// `blocksMotion` — which for the fill's four-way [`BlockKind`] means
    /// "stone", water and lava explicitly excluded.
    fn first_occupied_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
        let generator = self.generator;
        let aquifer = self.aquifer(x >> 4, z >> 4);
        let min_y = generator.min_y();
        // `queries` is the scan depth, reported to the counters once at the end
        // rather than per iteration. It is what makes `block_at` predictable
        // again: this probe is the *second* consumer of `AquiferSystem::block_at`
        // (the first is `fill_stage`'s one-per-cell loop), it is data-dependent,
        // and `benches/generation.rs`'s calibration decomposes the total into the
        // two terms rather than asserting a literal that quietly absorbed this
        // one. See `counters::Snapshot::structure_probe_block_at`.
        let mut queries = 0u64;
        for ly in (0..generator.height()).rev() {
            let y = min_y + ly;
            queries += 1;
            let kind = aquifer.block_at(x, y, z);
            let matched = match heightmap {
                HeightmapKind::WorldSurfaceWg => kind != BlockKind::Air,
                HeightmapKind::OceanFloorWg => kind == BlockKind::Stone,
            };
            if matched {
                crate::counters::bump_structure_height_probe(queries);
                return y;
            }
        }
        crate::counters::bump_structure_height_probe(queries);
        min_y - 1
    }

    fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String {
        self.generator.biome_at_quart(qx, qy, qz)
    }

    fn sea_level(&self) -> i32 {
        self.generator.sea_level()
    }

    /// The real dimension bounds, so a jigsaw structure's `above_bottom` /
    /// `below_top` start height and its `dimension_padding` resolve against this
    /// world rather than against the trait's overworld default.
    fn min_y(&self) -> i32 {
        self.generator.min_y()
    }

    fn dimension_height(&self) -> i32 {
        self.generator.height()
    }

    /// `isReplaceableByStructures`: air or fluid. Read out of the same per-chunk
    /// [`AquiferSystem`] the height probe uses, so a coded piece's foundation walk
    /// costs no extra aquifer build.
    fn is_replaceable_at(&self, x: i32, y: i32, z: i32) -> bool {
        let aquifer = self.aquifer(x >> 4, z >> 4);
        !matches!(aquifer.block_at(x, y, z), BlockKind::Stone)
    }

    /// The four-way fill kind itself, for the predicates that must separate water
    /// from lava from air. Same cached aquifer, so a mineshaft's liquid survey adds
    /// [`AquiferSystem::block_at`] calls but no aquifer builds beyond the chunks it
    /// already spans.
    fn block_kind_at(&self, x: i32, y: i32, z: i32) -> BlockKind {
        self.aquifer(x >> 4, z >> 4).block_at(x, y, z)
    }
}

/// `BuriedTreasurePieces.BuriedTreasurePiece.postProcess` — walk a cursor down
/// from the ocean-floor height at `(origin.x, origin.z)` until the block
/// *below* it is one of the five stone-family materials, fill the walk
/// position's six air/liquid neighbours (stone-family straight down, the
/// pre-walk block or sand everywhere else) and place an empty chest.
///
/// Runs against the **real** per-chunk grid at placement time (see
/// [`crate::structure::PieceRefinement::BuriedTreasureChest`]'s own doc for
/// why this cannot be an eager, start-time list like every other coded piece).
/// Draws no random: vanilla's own `random` argument is spent only inside
/// `createChest` on the loot-table roll seed, which is out of scope here the
/// same way every other structure's container loot is (see the
/// `template:block_entity_nbt`/`coded:chests` ledger rows) — the **block** is
/// what this places.
fn place_buried_treasure_chest(world: &mut crate::dense_grid::DenseBlockGrid, origin: [i32; 3]) {
    let (min_x, min_y, _min_z, size_x, size_y, _size_z) = world.bounds();
    let (x, z) = (origin[0], origin[2]);
    if x < min_x || x >= min_x + size_x {
        // The piece's own column is always inside its origin chunk
        // (`chunkBlockX(9)`), so this never fires in practice — a defensive
        // bound rather than a reachable one.
        return;
    }
    let top = min_y + size_y - 1;
    // `level.getHeight(OCEAN_FLOOR_WG, x, z)`: one above the topmost block that
    // is neither air nor a fluid, scanned against the *real* grid — sand,
    // sandstone and every surface-rule product are visible here, unlike at
    // structure-start time.
    let Some(ground) = (min_y..=top).rev().find(|&y| !is_air_or_liquid(world.get(x, y, z))) else {
        return;
    };
    let mut y = ground + 1;
    while y > min_y {
        let below = world.get(x, y - 1, z).to_string();
        if is_stone_family(base_name(&below)) {
            let current = world.get(x, y, z);
            let soft = if !is_air_or_liquid(current) {
                current.to_string()
            } else {
                "minecraft:sand".to_string()
            };
            const NEIGHBOURS: [[i32; 3]; 6] = [
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
                [-1, 0, 0],
                [1, 0, 0],
            ];
            for delta in NEIGHBOURS {
                let rel = [x + delta[0], y + delta[1], z + delta[2]];
                if !is_air_or_liquid(world.get(rel[0], rel[1], rel[2])) {
                    continue;
                }
                let below_rel = world.get(rel[0], rel[1] - 1, rel[2]);
                let is_up = delta == [0, 1, 0];
                if is_air_or_liquid(below_rel) && !is_up {
                    world.set(rel[0], rel[1], rel[2], &below);
                } else {
                    world.set(rel[0], rel[1], rel[2], &soft);
                }
            }
            // `StructurePiece.reorient` picks the facing from the four
            // horizontal neighbours' render-solidity *as just written above*;
            // every one of them is now solid by construction (each was either
            // already solid or just filled), so vanilla's own fallback branch
            // always fires here and lands on a fixed facing — see
            // `coded:chest_reorient` on the ledger, the same simplification
            // every other coded chest in this crate already makes.
            world.set(x, y, z, "minecraft:chest[facing=north,type=single,waterlogged=false]");
            return;
        }
        y -= 1;
    }
}

fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn is_air_or_liquid(state: &str) -> bool {
    let name = base_name(state);
    name == "minecraft:air" || name == "minecraft:water" || name == "minecraft:lava"
}

/// `belowState.is(SANDSTONE) || .is(STONE) || .is(ANDESITE) || .is(GRANITE) ||
/// .is(DIORITE)`.
fn is_stone_family(name: &str) -> bool {
    matches!(
        name,
        "minecraft:sandstone"
            | "minecraft:stone"
            | "minecraft:andesite"
            | "minecraft:granite"
            | "minecraft:diorite"
    )
}

/// The ruined-portal post-template pass: terrain growth, downward columns and
/// optional overgrowth. It runs against a fully surfaced chunk, after the
/// template itself wrote its frame.
///
/// The reference uses one mutable decoration stream per decorating chunk. This
/// engine cannot make a structure's neighbouring chunks depend on whichever
/// chunk happened to generate first, so every local choice is instead forked
/// from the world seed and its block position. Each chunk can then regenerate
/// the whole portal pass and clip writes to itself without a seam at a border.
/// The registry keeps that intentional random-stream deviation visible.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_ruined_portal_terrain(
    world: &mut crate::dense_grid::DenseBlockGrid,
    box_: crate::structure::BoundingBox,
    seed: i64,
    placement: VerticalPlacement,
    cold: bool,
    overgrown: bool,
    vines: bool,
    features_cannot_replace: &std::collections::HashSet<String>,
) {
    let centre = [
        box_.min[0] + (box_.max[0] - box_.min[0] + 1) / 2,
        box_.min[1] + (box_.max[1] - box_.min[1] + 1) / 2,
        box_.min[2] + (box_.max[2] - box_.min[2] + 1) / 2,
    ];
    let average_width = (box_.max[0] - box_.min[0] + 1 + box_.max[2] - box_.min[2] + 1) / 2;
    let mut radius_random = portal_random(seed, centre, 0);
    let distance_adjustment = radius_random.next_int_bounded((8 - average_width / 2).max(1));
    const CHANCE_BY_DISTANCE: [f32; 14] = [
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2,
    ];
    let follows_surface = matches!(placement, VerticalPlacement::OnLandSurface | VerticalPlacement::OnOceanFloor);
    for x in (centre[0] - CHANCE_BY_DISTANCE.len() as i32)..=(centre[0] + CHANCE_BY_DISTANCE.len() as i32) {
        for z in (centre[2] - CHANCE_BY_DISTANCE.len() as i32)..=(centre[2] + CHANCE_BY_DISTANCE.len() as i32) {
            let distance = (x - centre[0]).abs() + (z - centre[2]).abs();
            let adjusted = (distance + distance_adjustment).max(0) as usize;
            let Some(&chance) = CHANCE_BY_DISTANCE.get(adjusted) else {
                continue;
            };
            if portal_float(seed, [x, box_.min[1], z], 1) >= chance {
                continue;
            }
            let Some(surface) = portal_surface_y(world, x, z, placement) else {
                continue;
            };
            let y = if follows_surface { surface } else { box_.min[1].min(surface) };
            if (y - box_.min[1]).abs() > 3
                || !portal_replaceable(world.get(x, y, z), placement, features_cannot_replace)
            {
                continue;
            }
            place_portal_netherrack_or_magma(world, seed, [x, y, z], cold, 2);
            if overgrown {
                maybe_add_portal_leaves(world, seed, [x, y, z], 3);
            }
            add_portal_drip_column(world, seed, [x, y - 1, z], cold, 4);
        }
    }
    for x in (box_.min[0] + 1)..box_.max[0] {
        for z in (box_.min[2] + 1)..box_.max[2] {
            if base_name(world.get(x, box_.min[1], z)) == "minecraft:netherrack" {
                add_portal_drip_column(world, seed, [x, box_.min[1] - 1, z], cold, 5);
            }
        }
    }
    if vines || overgrown {
        for x in box_.min[0]..=box_.max[0] {
            for y in box_.min[1]..=box_.max[1] {
                for z in box_.min[2]..=box_.max[2] {
                    if vines {
                        maybe_add_portal_vine(world, seed, [x, y, z], 6);
                    }
                    if overgrown {
                        maybe_add_portal_leaves(world, seed, [x, y, z], 7);
                    }
                }
            }
        }
    }
}

fn portal_random(seed: i64, pos: [i32; 3], salt: i64) -> LegacyRandomSource {
    LegacyRandomSource::new(seed ^ get_seed(pos[0], pos[1], pos[2]) ^ salt)
}

fn portal_float(seed: i64, pos: [i32; 3], salt: i64) -> f32 {
    portal_random(seed, pos, salt).next_float()
}

fn portal_surface_y(
    world: &crate::dense_grid::DenseBlockGrid,
    x: i32,
    z: i32,
    placement: VerticalPlacement,
) -> Option<i32> {
    let (_, min_y, _, _, size_y, _) = world.bounds();
    let ocean_floor = placement == VerticalPlacement::OnOceanFloor;
    (min_y..(min_y + size_y)).rev().find(|&y| {
        if ocean_floor {
            !is_air_or_liquid(world.get(x, y, z))
        } else {
            base_name(world.get(x, y, z)) != "minecraft:air"
        }
    })
}

fn portal_replaceable(
    state: &str,
    placement: VerticalPlacement,
    features_cannot_replace: &std::collections::HashSet<String>,
) -> bool {
    let name = base_name(state);
    name != "minecraft:air"
        && name != "minecraft:obsidian"
        && !features_cannot_replace.contains(name)
        && (placement == VerticalPlacement::InNether || name != "minecraft:lava")
}

fn place_portal_netherrack_or_magma(
    world: &mut crate::dense_grid::DenseBlockGrid,
    seed: i64,
    pos: [i32; 3],
    cold: bool,
    salt: i64,
) {
    let state = if !cold && portal_float(seed, pos, salt) < 0.07 {
        "minecraft:magma_block"
    } else {
        "minecraft:netherrack"
    };
    world.set(pos[0], pos[1], pos[2], state);
}

fn add_portal_drip_column(
    world: &mut crate::dense_grid::DenseBlockGrid,
    seed: i64,
    mut pos: [i32; 3],
    cold: bool,
    salt: i64,
) {
    place_portal_netherrack_or_magma(world, seed, pos, cold, salt);
    for remaining in 0..8 {
        if portal_float(seed, pos, salt + 1 + i64::from(remaining)) >= 0.5 {
            break;
        }
        pos[1] -= 1;
        place_portal_netherrack_or_magma(world, seed, pos, cold, salt + 10 + i64::from(remaining));
    }
}

fn maybe_add_portal_leaves(
    world: &mut crate::dense_grid::DenseBlockGrid,
    seed: i64,
    pos: [i32; 3],
    salt: i64,
) {
    if portal_float(seed, pos, salt) < 0.5
        && base_name(world.get(pos[0], pos[1], pos[2])) == "minecraft:netherrack"
        && base_name(world.get(pos[0], pos[1] + 1, pos[2])) == "minecraft:air"
    {
        world.set(
            pos[0],
            pos[1] + 1,
            pos[2],
            "minecraft:jungle_leaves[distance=7,persistent=true,waterlogged=false]",
        );
    }
}

fn maybe_add_portal_vine(
    world: &mut crate::dense_grid::DenseBlockGrid,
    seed: i64,
    pos: [i32; 3],
    salt: i64,
) {
    let state = base_name(world.get(pos[0], pos[1], pos[2]));
    if matches!(state, "minecraft:air" | "minecraft:water" | "minecraft:lava" | "minecraft:vine") {
        return;
    }
    let mut random = portal_random(seed, pos, salt);
    let (dx, dz, vine) = match random.next_int_bounded(4) {
        0 => (0, -1, "minecraft:vine[east=false,north=false,south=true,up=false,west=false]"),
        1 => (1, 0, "minecraft:vine[east=false,north=false,south=false,up=false,west=true]"),
        2 => (0, 1, "minecraft:vine[east=false,north=true,south=false,up=false,west=false]"),
        _ => (-1, 0, "minecraft:vine[east=true,north=false,south=false,up=false,west=false]"),
    };
    if base_name(world.get(pos[0] + dx, pos[1], pos[2] + dz)) == "minecraft:air" {
        world.set(pos[0] + dx, pos[1], pos[2] + dz, vine);
    }
}

impl OverworldGenerator {
    /// Stage 0a: this chunk's structure starts, memoised.
    ///
    /// Empty (and allocation-free after the `Vec`'s own zero-capacity
    /// construction) for a generator whose resolver supplied no structure sets,
    /// which is every fixture resolver in this workspace.
    pub(super) fn structure_starts_stage(&self, cx: i32, cz: i32) -> Arc<Vec<Arc<StructureStart>>> {
        self.store
            .entry((cx, cz))
            .structure_starts
            .get_or_compute(drop, || {
                // Inside the once-guard, so this counts chunks whose starts
                // really ran — a cache hit adds nothing. Tagged `Structure` so
                // the bench's allocation binning attributes this work to
                // something narrower than `Other` (which also holds generator
                // construction).
                let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Structure);
                crate::counters::bump_structure_start();
                let Some(registry) = &self.structures else {
                    return Vec::new();
                };
                let sampler = StartSampler {
                    generator: self,
                    aquifers: RefCell::new(HashMap::new()),
                };
                registry
                    .starts_at(cx, cz, &sampler)
                    .into_iter()
                    .map(Arc::new)
                    .collect()
            })
    }

    /// Stage 0b: this chunk's structure references — `createReferences`' 17×17
    /// walk, keeping the starts whose adjusted box comes within
    /// [`BEARD_REACH`] blocks of this chunk.
    ///
    /// Vanilla's `createReferences` uses a plain 16×16 chunk-box intersection;
    /// the extra 12 blocks are the beardifier's own reach
    /// (`Beardifier.forStructuresInChunk`'s `isCloseToChunk(chunkPos, 12)`), and
    /// keeping one product for both consumers is why this is the wider of the
    /// two. [`StructureRefs::packed_by_structure`] re-narrows to the exact
    /// chunk-box test for the persistence view, so the NBT is vanilla's and the
    /// beardifier's input is the beardifier's.
    pub(super) fn structure_refs_stage(&self, cx: i32, cz: i32) -> Arc<StructureRefs> {
        self.store.entry((cx, cz)).structure_refs.get_or_compute(drop, || {
            if self.structures.is_none() {
                return StructureRefs::default();
            }
            let mut entries = Vec::new();
            for sx in (cx - REFS_RADIUS)..=(cx + REFS_RADIUS) {
                for sz in (cz - REFS_RADIUS)..=(cz + REFS_RADIUS) {
                    for start in self.structure_starts_stage(sx, sz).iter() {
                        if start.adjusted_bounding_box().is_close_to_chunk(cx, cz, BEARD_REACH)
                            || start.pieces.iter().any(|piece| {
                                matches!(piece.refine.as_ref(), Some(PieceRefinement::RuinedPortalTerrain { .. }))
                                    && piece.bounding_box.is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH)
                            })
                        {
                            entries.push((sx, sz, Arc::clone(start)));
                        }
                    }
                }
            }
            StructureRefs { entries }
        })
    }

    /// The starts whose origin is `(cx, cz)` and whose piece lists are complete —
    /// the set a save file may legitimately carry.
    ///
    /// A start with an incomplete piece list is deliberately **not** returned:
    /// vanilla reloads a start with no `Children` as `INVALID`, so persisting one
    /// would be worse than persisting nothing. Use
    /// [`Self::structure_starts_including_incomplete`] to see the placement
    /// answer, which is complete today even where the pieces are not.
    #[must_use]
    pub fn structure_starts(&self, cx: i32, cz: i32) -> Vec<Arc<StructureStart>> {
        // Radius 0: computing a chunk's own starts reads no other store slot (that
        // is what keeps stage 0a the topmost stage), so there is nothing wider to
        // pin. `column()`'s pin is the wide one — see `STRUCTURE_CLOSURE_RADIUS`.
        let _view = self.store.open_view((cx, cz), 0);
        self.structure_starts_stage(cx, cz)
            .iter()
            .filter(|start| start.pieces_complete)
            .map(Arc::clone)
            .collect()
    }

    /// Every start whose origin is `(cx, cz)`, including those this engine can
    /// place but not yet build (see
    /// [`StructureStart::pieces_complete`](crate::structure::StructureStart::pieces_complete)
    /// and [`StructureRegistry::unsupported`](crate::structure::StructureRegistry::unsupported)).
    ///
    /// This is the placement answer, and it is the one to compare against a
    /// vanilla save's `structures.starts` keys.
    #[must_use]
    pub fn structure_starts_including_incomplete(
        &self,
        cx: i32,
        cz: i32,
    ) -> Vec<Arc<StructureStart>> {
        let _view = self.store.open_view((cx, cz), 0);
        self.structure_starts_stage(cx, cz).to_vec()
    }

    /// Every start whose pieces this chunk's placement stage will write, in the
    /// order it writes them — the input `structure_place_stage` itself reads.
    ///
    /// Exists so a gate can predict what a chunk must contain **without
    /// re-deriving the 17×17 reach or the `is_close_to_chunk` filter**. A mineshaft
    /// is 160 blocks wide and the oracle world has two starts three chunks apart, so
    /// "the pieces of the start at this chunk" is not the set that lands here, and a
    /// test that assumed it was measured 59 blocks against a true 97.
    #[must_use]
    pub fn structure_starts_placed_in(&self, cx: i32, cz: i32) -> Vec<Arc<StructureStart>> {
        let _view = self.store.open_view((cx, cz), REFS_RADIUS);
        self.structure_refs_stage(cx, cz)
            .entries
            .iter()
            .filter(|(_, _, start)| start.pieces_complete)
            .map(|(_, _, start)| Arc::clone(start))
            .collect()
    }

    /// This chunk's `structures.References`, ready for the NBT writer: structure
    /// id → packed origin-chunk keys, narrowed to vanilla's own 16×16
    /// intersection test.
    #[must_use]
    pub fn structure_references(&self, cx: i32, cz: i32) -> std::collections::BTreeMap<String, Vec<i64>> {
        // `REFS_RADIUS`, not the wider column closure: this reads `structure_refs`
        // for one chunk, whose own walk is exactly the 17×17.
        let _view = self.store.open_view((cx, cz), REFS_RADIUS);
        let refs = self.structure_refs_stage(cx, cz);
        let (bx, bz) = (cx * 16, cz * 16);
        let mut narrowed = StructureRefs::default();
        for (sx, sz, start) in &refs.entries {
            if start.pieces_complete
                && start
                    .bounding_box
                    .intersects_xz(bx, bz, bx + 15, bz + 15)
            {
                narrowed
                    .entries
                    .push((*sx, *sz, Arc::clone(start)));
            }
        }
        narrowed.packed_by_structure()
    }

    /// Stage 4b: writes every template-driven piece that
    /// touches this chunk into `world`.
    ///
    /// # Where this sits, and why
    ///
    /// Vanilla places structures inside `applyBiomeDecoration`, per generation
    /// step, *before* that step's features — and the three kinds wired today are
    /// all `surface_structures` (step 4), which precedes `underground_ores`
    /// (step 6) and `vegetal_decoration` (step 9). So this runs at the end of
    /// [`OverworldGenerator::pre_ore_stage`](super::OverworldGenerator): ore and
    /// vegetation then see the structure's blocks, exactly as they do in vanilla,
    /// and the whole thing is memoised once per chunk with the rest of the pre-ore
    /// product.
    ///
    /// # Clipping is the grid, not a box
    ///
    /// The working grid spans this chunk's 16×16 columns only and
    /// [`DenseBlockGrid::set`](crate::dense_grid::DenseBlockGrid::set) ignores a
    /// write outside it, so a piece that straddles a border writes its own half
    /// here and the other half when the neighbour generates — vanilla's
    /// `placeSettings.setBoundingBox(chunkBB)` for free. That is only sound
    /// because every piece's position is fixed at *start* time and every
    /// processor draw is position-seeded; see
    /// [`StructureKind::generate_pieces`](crate::structure::StructureKind).
    pub(super) fn structure_place_stage(
        &self,
        cx: i32,
        cz: i32,
        mut world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        let Some(registry) = &self.structures else {
            return world;
        };
        // Below the early return, per this file's own rule about stage guards:
        // a guard above it would count a fixture-tree no-op as a run.
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Structure);
        let seed = registry.seed();
        let (bx, bz) = (cx * 16, cz * 16);
        let mineshaft_sampler = StartSampler {
            generator: self,
            aquifers: RefCell::new(HashMap::new()),
        };
        for (_, _, start) in &self.structure_refs_stage(cx, cz).entries {
            if !start.pieces_complete {
                continue;
            }
            if start.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15)
                && let Some(blocks) = registry.mineshaft_blocks_for_chunk(
                    start,
                    cx,
                    cz,
                    &mineshaft_sampler,
                )
            {
                for block in blocks {
                    world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
                }
                continue;
            }
            // `StructureStart.placeInChunk` derives one `referencePos` for the whole
            // start, from its **first** piece's box, before the per-piece loop. It
            // is not a per-piece value and it is not the chunk — an
            // `axis_aligned_linear_pos` rule measures from here.
            let reference = crate::structure::jigsaw::reference_position(&start.pieces);
            for piece in &start.pieces {
                let portal_terrain_reaches = matches!(
                    piece.refine.as_ref(),
                    Some(PieceRefinement::RuinedPortalTerrain { .. })
                ) && piece
                    .bounding_box
                    .is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH);
                if !piece.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) && !portal_terrain_reaches {
                    continue;
                }
                // A coded piece writes a pre-resolved block list; a template piece
                // writes its template. Both are clipped by the grid.
                if let Some(blocks) = &piece.blocks {
                    for block in blocks.iter() {
                        world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
                    }
                }
                if let Some(placement) = &piece.placement {
                    let origin = crate::structure::template::PlaceOrigin {
                        position: placement.position,
                        reference,
                        seed,
                    };
                    placement
                        .template
                        .place(origin, &placement.settings, &mut world);
                    // A `list_pool_element` writes several templates at one position,
                    // in document order — `ListPoolElement.place`'s own loop.
                    for extra in &piece.extra_placements {
                        let origin = crate::structure::template::PlaceOrigin {
                            position: extra.position,
                            reference,
                            seed,
                        };
                        extra.template.place(origin, &extra.settings, &mut world);
                    }
                }
                // Refinements read and write the real post-surface, post-carve grid.
                // Portal terrain runs after the frame, while buried treasure has no
                // template and simply takes this same post-placement hook.
                match piece.refine.as_ref() {
                    Some(PieceRefinement::BuriedTreasureChest) => {
                        place_buried_treasure_chest(&mut world, piece.bounding_box.min);
                    }
                    Some(PieceRefinement::RuinedPortalTerrain {
                        placement,
                        cold,
                        overgrown,
                        vines,
                        features_cannot_replace,
                    }) => place_ruined_portal_terrain(
                        &mut world,
                        piece.bounding_box,
                        seed,
                        *placement,
                        *cold,
                        *overgrown,
                        *vines,
                        features_cannot_replace,
                    ),
                    None => {}
                }
            }
        }
        world
    }

    /// Stage 0c: this chunk's beard term.
    ///
    /// Cheap in the case that matters. A generator with no structure data returns
    /// [`Beardifier::empty`] without touching the store; a generator *with*
    /// structure data reads its already-memoised
    /// [`StructureRefs`] and, for the overwhelming majority of chunks, finds no
    /// adaptation-bearing start and returns empty too. Only a chunk genuinely
    /// within reach of one builds a rigid list, and only then does
    /// [`fill_stage`](OverworldGenerator::fill_stage) take its per-block branch.
    ///
    /// Not memoised in the store, deliberately: it is a pure function of
    /// `structure_refs` (which *is* memoised) and it is consumed exactly once per
    /// chunk, by the fill. A slot for it would be a third stage carrying no work.
    pub(super) fn beardifier_for(&self, cx: i32, cz: i32) -> crate::structure::beardifier::Beardifier {
        use crate::structure::beardifier::Beardifier;
        if self.structures.is_none() {
            return Beardifier::empty();
        }
        let refs = self.structure_refs_stage(cx, cz);
        Beardifier::for_chunk(cx, cz, refs.adaptation_bearing().map(std::convert::AsRef::as_ref))
    }

    /// This chunk's pre-surface shape field (`fillFromNoise`'s output, stage 1)
    /// with an **explicit** beard term — the seam a gate or a JVM comparison
    /// drives the beardifier through.
    ///
    /// Public because S3's evidence needs it and there is no other way in. The
    /// production path derives its beard from [`Self::beardifier_for`], which can
    /// only ever produce the beard the *real* starts imply; every
    /// adaptation-bearing structure in 26.2 is jigsaw (S4) or coded (S5), so until
    /// one of those lands, a real generated chunk cannot exercise a non-empty
    /// beard at all. Passing one in is what lets the terrain change be measured
    /// now rather than asserted later, and it is also the comparison point for a
    /// `Beardifier`-bearing JVM dump.
    ///
    /// Returns `16 × height × 16` [`BlockKind`](crate::aquifer::BlockKind)s;
    /// index it with [`Self::shape_index`] rather than restating the layout.
    /// Calling it does **not** touch the store's `pre_ore` slot: it builds a fresh
    /// aquifer, so it is not a way to poison the memoised pipeline with a
    /// synthetic beard.
    #[must_use]
    pub fn shape_field_with_beard(
        &self,
        cx: i32,
        cz: i32,
        beard: &crate::structure::beardifier::Beardifier,
    ) -> Vec<crate::aquifer::BlockKind> {
        let aquifer = self.build_aquifer(cx, cz);
        self.fill_stage(&aquifer, cx * 16, cz * 16, beard)
    }

    /// Where chunk-local `(lx, ly, lz)` lands in
    /// [`Self::shape_field_with_beard`]'s return.
    ///
    /// Forwards to the *same* private `idx` the fill writes through, rather than
    /// restating `((ly * 16 + lz) * 16 + lx)`: a caller that restated it would
    /// read a transposed field and report a plausible-looking wrong answer, and
    /// the two spellings could then drift apart independently.
    #[must_use]
    pub fn shape_index(&self, lx: i32, ly: i32, lz: i32) -> usize {
        Self::idx(lx, ly, lz, self.height())
    }

    /// The beard term this generator's real starts imply for `(cx, cz)` — the
    /// exact value the production fill uses.
    ///
    /// Public so a gate can assert *which* branch the fill took, rather than
    /// inferring it from the output. See [`Self::shape_field_with_beard`].
    #[must_use]
    pub fn beardifier(&self, cx: i32, cz: i32) -> crate::structure::beardifier::Beardifier {
        let _view = self.store.open_view((cx, cz), REFS_RADIUS);
        self.beardifier_for(cx, cz)
    }

    /// The registry's unsupported ledger, or an empty map for a generator with no
    /// structure data. See
    /// [`StructureRegistry::unsupported`](crate::structure::StructureRegistry::unsupported).
    #[must_use]
    pub fn structure_ledger(&self) -> std::collections::BTreeMap<String, String> {
        self.structures
            .as_ref()
            .map(|r| r.unsupported().clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A column with `depth` blocks of sand over stone, air above — the shape
    /// that makes the walk actually walk (a beach/ocean-floor surface rule
    /// stacking sand over the real stone), rather than terminating on its
    /// first iteration.
    fn sandy_column(depth: i32) -> crate::dense_grid::DenseBlockGrid {
        let mut map = HashMap::new();
        // Stone from -64 up to (but not including) the sand layer.
        let stone_top = 60 - depth;
        for y in -64..=stone_top {
            map.insert((8, y, 8), "minecraft:stone".to_string());
        }
        for y in (stone_top + 1)..=60 {
            map.insert((8, y, 8), "minecraft:sand".to_string());
        }
        crate::dense_grid::DenseBlockGrid::from_hashmap(0, -64, 0, 16, 384, 16, &map)
    }

    /// The chest lands exactly one block above the first **stone-family**
    /// block, not the first solid block — a beach column with sand on top must
    /// be walked *through*, matching vanilla's own multi-layer descent.
    #[test]
    fn the_chest_lands_on_stone_under_a_sand_beach() {
        let mut world = sandy_column(3);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        // Stone top is at 60 - 3 = 57, so the chest sits at 58.
        assert_eq!(world.get(8, 58, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
        // Nothing was placed at the sand layer or below the stone surface.
        assert_ne!(world.get(8, 60, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
    }

    /// A column with **no** sand at all (stone straight to the surface) places
    /// the chest one above bare stone — the degenerate case of the same walk.
    #[test]
    fn the_chest_lands_directly_on_bare_stone() {
        let mut world = sandy_column(0);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        assert_eq!(world.get(8, 61, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
    }

    /// Every air/liquid neighbour of the chest is filled — straight down with
    /// the stone-family block the walk found, everywhere else with the
    /// pre-existing block (here, air, so it falls back to sand).
    #[test]
    fn every_air_neighbour_of_the_chest_is_filled() {
        let mut world = sandy_column(0);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        // Chest at (8, 61, 8), stone at (8, 60, 8) and below.
        assert_eq!(world.get(8, 60, 8), "minecraft:stone", "the ground itself is untouched");
        // The four horizontal neighbours and the one above were air; each
        // must now be something solid (sand, since there was nothing else to
        // reuse) rather than air.
        for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, 1, 0)] {
            let state = world.get(8 + dx, 61 + dy, 8 + dz);
            assert_ne!(state, "minecraft:air", "neighbour ({dx},{dy},{dz}) was left air");
        }
    }

    /// `base_name` strips a bracketed property list; `is_air_or_liquid` and
    /// `is_stone_family` read the five- and three-member sets vanilla's own
    /// `postProcess` names, and nothing else.
    #[test]
    fn the_material_predicates_match_exactly_vanillas_named_sets() {
        assert_eq!(base_name("minecraft:water[level=0]"), "minecraft:water");
        assert!(is_air_or_liquid("minecraft:air"));
        assert!(is_air_or_liquid("minecraft:water[level=3]"));
        assert!(is_air_or_liquid("minecraft:lava"));
        assert!(!is_air_or_liquid("minecraft:stone"));
        for name in [
            "minecraft:sandstone",
            "minecraft:stone",
            "minecraft:andesite",
            "minecraft:granite",
            "minecraft:diorite",
        ] {
            assert!(is_stone_family(name), "{name} should be stone-family");
        }
        for name in ["minecraft:dirt", "minecraft:gravel", "minecraft:sand", "minecraft:deepslate"] {
            assert!(!is_stone_family(name), "{name} should not be stone-family");
        }
    }

    /// The portal refinement grows a real skirt beyond the frame, creates a
    /// downward column from the frame's netherrack, and leaves protected blocks
    /// alone. A bare template-placement test cannot observe any of those three
    /// post-template effects.
    #[test]
    fn ruined_portal_refinement_grows_skirt_and_preserves_protected_blocks() {
        let mut map = HashMap::new();
        for x in 0..16 {
            for z in 0..16 {
                for y in -4..=59 {
                    map.insert((x, y, z), "minecraft:stone".to_string());
                }
            }
        }
        let box_ = crate::structure::BoundingBox {
            min: [5, 60, 5],
            max: [8, 63, 8],
        };
        map.insert((6, 60, 6), "minecraft:netherrack".to_string());
        map.insert((5, 59, 5), "minecraft:obsidian".to_string());
        let mut world = crate::dense_grid::DenseBlockGrid::from_hashmap(0, -4, 0, 16, 96, 16, &map);
        let protected = std::collections::HashSet::new();
        place_ruined_portal_terrain(
            &mut world,
            box_,
            41,
            VerticalPlacement::OnLandSurface,
            true,
            false,
            false,
            &protected,
        );
        let skirt_cells = (0..16)
            .flat_map(|x| (0..16).map(move |z| (x, z)))
            .filter(|&(x, z)| x < box_.min[0] || x > box_.max[0] || z < box_.min[2] || z > box_.max[2])
            .filter(|&(x, z)| base_name(world.get(x, 59, z)) == "minecraft:netherrack")
            .count();
        assert!(skirt_cells > 0, "the portal produced no netherrack outside its frame");
        assert_eq!(
            base_name(world.get(6, 59, 6)),
            "minecraft:netherrack",
            "the frame's netherrack did not grow a drip column"
        );
        assert_eq!(
            base_name(world.get(5, 59, 5)),
            "minecraft:obsidian",
            "terrain growth replaced a protected block"
        );
    }
}
