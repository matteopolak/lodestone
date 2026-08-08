//! Vegetal decoration (issue #406): grass, flowers and trees (oak, birch,
//! spruce, plus spruce's own `pine` sibling since it shares the same
//! trunk/foliage engine at near-zero extra cost — see "Scope" below).
//! `GenerationStep.Decoration.VEGETAL_DECORATION` ([`super::STEP_VEGETAL_DECORATION`]),
//! one step after [`super`]'s `UNDERGROUND_ORES` engine, reusing the same
//! shape: vanilla's placement-modifier pipeline is data, walked by an engine
//! that reproduces its exact RNG-consumption order (see [`super`]'s own
//! module doc for the depth-first `flatMap` semantics every placed feature
//! composes with — this module's [`resolve_placed_feature_ref`]/
//! [`place_placed_feature`] recursion is the same shape applied to
//! vegetation instead of ores).
//!
//! # Scope, named plainly
//!
//! **Cross-chunk spill (issue #427): closed.** Vanilla's
//! `blockStateWriteRadius(1)` at the FEATURES generation stage
//! (`ChunkPyramid.java:32-35`, the same limit `docs/worldgen-parity.md`
//! documents for the ore 3×3 driver) applies to `VEGETAL_DECORATION` too —
//! a tree placed near a chunk edge can legitimately spill canopy into a
//! neighbour, and a neighbour's own pass can spill grass/leaves into this
//! chunk. [`apply_vegetal_decoration_step_3x3_per_source`] is the real
//! vanilla driver: each of the 9 chunks in `center ± 1` gets its own full
//! decoration pass (its own origin, its own decoration seed, its own
//! biome-resolved feature list), all writing into one shared
//! [`VegGrid::with_footprint`] region spanning
//! [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`] — the exact
//! shape [`crate::feature::apply_ore_step_3x3_per_source`] already
//! established for the ore engine, reused rather than reinvented. See
//! `docs/worldgen-parity.md` for the measured residual against the real JVM
//! oracle's `FULL3X3` mode.
//!
//! [`apply_vegetal_decoration_step`] (the single-source primitive this
//! module shipped with originally, issue #406) still exists and is still
//! correct on its own terms — it is simply no longer what
//! `crate::overworld::OverworldGenerator::vegetation_stage` calls in
//! production. A write whose final position lands outside whatever
//! footprint the caller's [`VegGrid`] covers is silently dropped (never
//! written anywhere else), and a read (heightmap probe, air/tag check)
//! clamps into the nearest in-bounds column — for the single-source
//! primitive that means the nearest column *within the one chunk*; for the
//! 3×3 driver it means the nearest column within the driven 48×48 region,
//! narrower than vanilla's genuinely unbounded read (see
//! `docs/worldgen-parity.md`'s "known gap: the 3×3 ore driver's residual
//! beyond its own neighbourhood" for the ore engine's identical, already-
//! named instance of this same shape).
//!
//! ## Evidence: an oracle *does* validate this now (this paragraph used to say
//! the opposite)
//!
//! Up to `074b5e9` this doc opened with "**No oracle validates this against a
//! real vanilla dump**", and that was true when written — but
//! `scripts/worldgen-oracle/VegetationOracle.java` and
//! `crates/lodestone-worldgen/tests/vegetation_parity.rs` both exist now, and
//! the latter diffs this module block-for-block against a real 26.2 server dump
//! at four fixtures (two plains, two savanna). Issue #478's investigation found
//! the stale paragraph still here, steering readers away from the evidence that
//! had already landed. Corrected rather than deleted, because *which* claim went
//! stale is itself the useful record — CLAUDE.md's rule 2.
//!
//! Two live limits on that evidence, both real:
//!
//! * `vegetation_parity.rs` resolves against a **fixture directory**
//!   (`tests/support/worldgen_data`), not the bundled server assets, so it
//!   validates the *engine* and not the shipped data path. The production seam is
//!   covered separately, by
//!   `lodestone_server::worldgen_data::tests::vegetation_reaches_real_blocks_over_a_production_sweep`.
//! * `VegetationOracle.java` is self-authored, so agreement with it is weaker
//!   evidence than a captured vanilla byte stream — and it has already been wrong
//!   in a way that produced *plausible* output: see that test file's own
//!   "A real bug in the oracle itself" section, where a missing
//!   `isStateAtPosition` case made `TreeFeature.validTreePos` always false, so no
//!   trunk placer had ever written a block through it.
//!
//! Counts asserted *inside* this crate remain derived **from the embedded
//! placement-modifier JSON itself** (`expected_value()` on the outer `count`
//! provider, `noise_threshold_count`'s two constants, etc.) — an
//! internal-consistency check. That is not parity evidence and is not claimed as
//! any; the parity claim rests on the two files named above.
//!
//! **Unsupported feature/trunk/foliage/state-provider kinds degrade to a
//! silent no-op, never a panic** — [`ConfiguredFeature::Unsupported`],
//! [`TreeConfig::try_parse`] returning `None`, [`BlockStateProvider::try_parse`]
//! returning `None`. This matters beyond the three named species:
//! [`super::compose::build_biome_vegetation`] (this module is reached from)
//! resolves **every** biome's `VEGETAL_DECORATION` step at generator
//! construction time, including biomes this issue never asked for (jungle's
//! `GiantTrunkPlacer`/`JungleFoliagePlacer`, fancy oak's `FancyTrunkPlacer`,
//! acacia's `RotatedBlockProvider` trunk, azalea's `EnvironmentScanPlacement`,
//! `FallenTreeFeature`, and more) — a `panic!` on any one of those would
//! break world generation for **every** biome table, not just the one that
//! triggered it. So every parse path in this module returns `Option`/degrades
//! rather than panics, and a biome whose vegetation this module can't yet
//! model simply grows nothing there (a real, visible gap for those biomes,
//! not a crash) — see "Named per-branch gaps" below for exactly which real
//! oak/birch/taiga branches this affects even within the three requested
//! species.
//!
//! # Named per-branch gaps within oak/birch/spruce themselves
//!
//! `configured_feature/trees_plains.json` etc. are [`ConfiguredFeature::RandomSelector`]s,
//! not a single tree — real vanilla plains rolls a fancy-oak / fallen-oak /
//! plain-oak branch per attempt. This module implements straight-trunk +
//! blob/spruce/pine foliage only, so:
//!
//! - **oak** (`trees_plains`/`trees_flower_forest` etc.): `fancy_oak_bees_*`
//!   (33.3% chance, `FancyTrunkPlacer`+`FancyFoliagePlacer`) and
//!   `fallen_oak_tree` (1.25%, [`FallenTreeFeature`]) are both
//!   [`ConfiguredFeature::Unsupported`] — roughly **1/3 of oak attempts
//!   produce no tree** in this landing. The default/`oak_bees_*` branch
//!   (~65.4%) is fully supported, beehive decorator included.
//! - **birch** (`trees_birch`): only `fallen_birch_tree` (1.25%) is
//!   unsupported — the default `birch_bees_0002` branch (~98.75%) is fully
//!   supported. Birch is the most complete of the three.
//! - **spruce/taiga** (`trees_taiga`): `pine_checked` (33.3%,
//!   `PineFoliagePlacer` — **implemented**, see below) and
//!   `fallen_spruce_tree` (0.83%) — with pine supported, only the fallen
//!   branch is a gap, so taiga is ~99.2% supported.
//! - **acacia/savanna** (`trees_savanna`, issue #428): `acacia_checked`
//!   (80%, [`TrunkPlacerCfg::Forking`]+[`FoliagePlacerCfg::Acacia`] —
//!   **implemented**) and the default `oak_checked` branch (~19.75%, the
//!   same straight-trunk oak every other biome already supports) leave only
//!   `fallen_oak_tree` (1.25%) unsupported — savanna/savanna_plateau/
//!   windswept_savanna (all three resolve through this same configured
//!   feature) are ~98.75% supported.
//!
//! `pine`/`PineFoliagePlacer` was added beyond the issue's literal "oak,
//! birch, spruce" minimum because it shares [`TrunkPlacerCfg`] entirely and
//! is a small, self-contained addition ([`FoliagePlacerCfg::Pine`]) that
//! turns taiga's honest coverage from ~66% to ~99%, in contrast to oak's
//! `fancy_oak`/`FallenTreeFeature`, which are structurally different
//! trunk/foliage/feature families and were out of scope for issue #406's
//! landing. Acacia (`TrunkPlacerCfg::Forking`) is issue #428's own addition
//! in that same spirit — a real, separate trunk/foliage family (leaning
//! column + branch, not oak's straight-trunk-plus-variant shape), landed
//! because savanna is a common, visible biome and `ForkingTrunkPlacer` is
//! self-contained (no multi-block-wide "giant" trunk footprint the way
//! jungle/mangrove/cherry all have). Dark oak
//! (`TrunkPlacerCfg::DarkOak`+`FoliagePlacerCfg::DarkOak`, a real 2×2 trunk
//! with hanging branches — plus the `ThreeLayersFeatureSize` it pairs with)
//! landed later in the same issue, because it gates dark_forest's defining
//! tree at 66.7% weight in `dark_forest_vegetation` (the acacia-style
//! argument, applied to the biome where the gap was loudest); it also
//! carries pale oak for free — `pale_oak`/`pale_oak_creaking` reuse the same
//! trunk/foliage placer types with their own providers, closing pale_garden's
//! tree gap alongside. Jungle (`GiantTrunkPlacer`+`MegaJungleFoliagePlacer`),
//! mangrove (`UpwardsBranchingTrunkPlacer` — has real above-water roots) and
//! cherry (`CherryTrunkPlacer`+`CherryFoliagePlacer`) remain
//! [`ConfiguredFeature::Unsupported`] — each is a structurally distinct
//! trunk/foliage shape, not a small extension of `Straight`/`Forking`, and
//! none was attempted this session; see
//! `lodestone_server::worldgen_data::KNOWN_VEGETATION_GAPS` for exactly
//! which biomes still carry `"tree: unsupported trunk/foliage/size/provider"`
//! because of them. `FallenTreeFeature` (a decorator-like feature reachable
//! from MANY biomes' `RandomSelector`s at a small, consistent ~1-1.25%
//! chance each — plains, birch, taiga, savanna, and more) is a different,
//! separately-named gap (`"fallen_tree"`) for the same reason: a real,
//! distinct feature type, not a variant of `ConfiguredFeature::Tree`, and
//! also not attempted this session.
//!
//! # Approximations, named
//!
//! - **Heightmap types are collapsed to two scans**, not vanilla's five
//!   distinct incremental heightmaps: [`HeightmapKind::WorldSurfaceWg`] and
//!   `MotionBlocking` both scan for "topmost non-air" ([`VegGrid::height_world_surface`]);
//!   [`HeightmapKind::OceanFloorWg`]/`OceanFloor` both scan for "topmost
//!   non-air, non-fluid" ([`VegGrid::height_ocean_floor`]). The
//!   `MOTION_BLOCKING` vs `WORLD_SURFACE_WG` difference (whether a
//!   non-solid decorative block like an already-placed `short_grass` counts)
//!   is real but narrow: [`VegPlacement::BlockPredicateFilter`]'s own
//!   final air-check downstream rejects most of the cases where it would
//!   have mattered anyway. Both scans are recomputed live against the
//!   *current* (mutating, this-step-inclusive) grid on every query — not a
//!   separately-maintained incremental heightmap — so a later feature in
//!   the same step correctly sees an earlier feature's writes.
//! - **`canSurvive` is modelled uniformly as `VegetationBlock`'s rule**
//!   (the block below the target must be in `#minecraft:supports_vegetation`)
//!   for every [`ConfiguredFeature::SimpleBlock`] placement. Every state this
//!   module actually places (`short_grass`, `dandelion`, `poppy`, and their
//!   siblings) really is a `VegetationBlock` subclass, so this is exact
//!   within scope — it would be wrong for a double-plant
//!   (`DoublePlantBlock`, e.g. `lilac`/`sunflower`/`tall_grass`) or
//!   `MossyCarpetBlock`, which this module does not special-case (their
//!   `SimpleBlockConfiguration` still parses; placement silently treats them
//!   like a single-block state, which is wrong for anything taller than one
//!   block — named here rather than hidden).
//! - **`would_survive`'s tested `state` is ignored**; the predicate always
//!   means "the block below the target is `#minecraft:supports_vegetation`"
//!   regardless of which sapling it names. Exact for every `would_survive`
//!   check this module's own configured/placed features actually use (all
//!   name a sapling, all resolve to the same `VegetationBlock` rule) —
//!   would be wrong for a `would_survive` check on a non-`VegetationBlock`
//!   state, which does not occur in this module's own scope.
//! - **`BeehiveDecorator`'s hive-row selection (`leaves.getFirst()`) is
//!   approximated as the canopy's topmost row**, not vanilla's true
//!   (JVM-`HashSet`-iteration-order-dependent, and therefore not portably
//!   reproducible) "first" leaf. The **log**-row half of the same decorator
//!   (`logs.getFirst()`/`getLast()`) is exact, not approximate: this
//!   engine's straight trunks place exactly one log per Y level, so
//!   "lowest"/"highest" log is unambiguous regardless of iteration order.
//!   Gated behind a ≤5% probability roll to begin with; see
//!   [`place_beehive_decorator`]'s own doc comment.
//! - **Waterlogged-leaf detection reads the current block id directly**
//!   (`"minecraft:water"` with no separate fluid layer) rather than a real
//!   fluid-level model — the same simplification `docs/worldgen-parity.md`'s
//!   "known representation gap: fluid `level`" already names for this
//!   engine generally.

//! # File layout (U16 Phase B)
//!
//! One 3,661-line file until the decomposition unit split it: this file keeps the
//! `VEGETAL_DECORATION` driver and its seeding, and the interior moved out unchanged.
//!
//! * this file — the step drivers plus `place_placed_feature`/`place_configured_feature`;
//! * [`config`] — the JSON/predicate/provider layer every feature is parsed into;
//! * [`tree`] — trunk placers, foliage placers and leaf-distance propagation;
//! * [`place`] — the per-feature placement bodies (simple block, block column, tree, beehive);
//! * [`grid`] — [`VegGrid`] and the [`census`] counters.
//!
//! Every path this module used to expose (`crate::feature::vegetation::X`) still
//! resolves: the submodules are private and glob-re-exported here.

mod config;
mod grid;
pub mod ids;
mod place;
mod tree;

pub use self::config::*;
pub use self::grid::*;
use self::place::*;
pub use self::tree::*;

use crate::feature::{BlockPos, STEP_VEGETAL_DECORATION};
use crate::rng::{RandomSource, WorldgenRandom};

use self::grid::census::bump as census_bump;

fn base_id(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

pub fn apply_vegetal_decoration_step<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    chunk_x: i32,
    chunk_z: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    features: &[(usize, PlacedRef)],
) {
    // U8: bring `tags`' id bitsets up to date with this grid's interner, once per
    // pass. This is the only place `StateInterner::len`'s lock is taken on the
    // decoration path — see [`ids`] for why per-query binding would defeat the
    // whole mechanism, and why skipping it entirely is merely slow, not wrong.
    tags.bind(grid.interner());
    let origin = BlockPos {
        x: chunk_x * 16,
        y: grid.min_y,
        z: chunk_z * 16,
    };
    let decoration_seed = random.set_decoration_seed(seed, origin.x, origin.z);
    for (index, placed) in features {
        random.set_feature_seed(decoration_seed, *index as i32, STEP_VEGETAL_DECORATION);
        place_placed_feature(random, origin, placed, grid, tags);
    }
}

/// The real vanilla 3×3 neighbourhood driver for `VEGETAL_DECORATION`
/// (issue #427) — the same `blockStateWriteRadius(1)` limit
/// `docs/worldgen-parity.md` already documents for the ore engine's own
/// [`crate::feature::apply_ore_step_3x3_per_source`], applied to this
/// module's own placement pipeline instead of introducing a second
/// mechanism.
///
/// Runs [`apply_vegetal_decoration_step`] for each of the 9 chunks in
/// `center ± 1` in turn (`dx` outer `-1..=1`, `dz` inner `-1..=1`, matching
/// the ore driver's and `crate::carver::apply_carvers`'s own fixed,
/// documented iteration order — not a claim this matches real-world chunk
/// *load* order, which vanilla itself does not guarantee at boundaries),
/// against one shared `grid`. `grid` must already be able to *read* every one of
/// the 9 sources' own post-ore terrain **and the 16-chunk rim around them** — in
/// production via [`VegGrid::with_sources`] over
/// [`crate::feature::region_view::WIDE_RADIUS`] = 2, in a fixture via
/// [`VegGrid::with_footprint`] plus `seed`, with `origin_x`/`origin_z` fixed at
/// `(center_x * 16, center_z * 16)` either way. This function does no stitching of
/// its own, mirroring [`crate::feature::apply_ore_step_3x3_per_source`]'s own
/// "caller stitches, driver only places" split.
///
/// **The rim is not margin.** Each of these nine can write into the centre, so each
/// one's pass has to be a function of that source alone or the two chunks either
/// side of a seam produce different versions of the same tree and the served world
/// keeps one half. A source at offset `(±1, ±1)` reads
/// [`crate::feature::VEG_PADDING`] blocks past its own edge; if those columns answer
/// air, *where* they start answering air moves with the centre. Measured: 94
/// truncated seam rows over the 66 bundled biomes, 50 removed by the rim. See
/// `docs/worldgen-seam-consistency.md`.
///
/// Each source's own pass mutates `grid` **in place**, so a later source in
/// the fixed iteration order sees an earlier source's writes — this is a
/// real, intentional match to `VegetationOracle.java`'s `runStep`, which
/// mutates one shared, live `WorldGenLevel` across all 9 sources in the same
/// order, not 9 independent snapshots merged afterward. See that oracle's
/// own doc comment on `runStep` for the vanilla behaviour this reproduces.
///
/// **That shared mutation is also the remaining, unclosed violation of the
/// invariant above, and it cannot be fixed here without a parity trade.** Which
/// other sources have already written — and in what order — is decided by the
/// centre, so a source's reads still differ between the two drives that recompute
/// it. Giving each source its own overlay takes the truncation count to **0** and
/// simultaneously pushes JVM FULL3X3 identity mismatches from 1 to 7 at
/// `vegetation_savanna_neg30_15_jvm.txt`, past `tests/vegetation_parity.rs`'s own
/// measured bound of 3 — vanilla genuinely is order-dependent here, because it runs
/// each chunk's FEATURES stage exactly once and persists the spill. Do not "fix"
/// this by isolating the overlay without re-baselining that gate and reading
/// `docs/worldgen-seam-consistency.md` first.
///
/// `features_for_source(source_x, source_z)` is called once per source
/// (their own chunk coordinates, not centre-relative) and must return that
/// source's own biome's `VEGETAL_DECORATION` list — vanilla resolves the
/// decorating biome per chunk, so a neighbour in a different biome to the
/// centre places (and RNG-consumes) a different feature list, matching
/// [`crate::feature::apply_ore_step_3x3_per_source`]'s `ores_for_source`
/// convention exactly.
pub fn apply_vegetal_decoration_step_3x3_per_source<'a, R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    center_x: i32,
    center_z: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    features_for_source: &dyn Fn(i32, i32) -> &'a [(usize, PlacedRef)],
) {
    for dx in -1..=1i32 {
        for dz in -1..=1i32 {
            let source_x = center_x + dx;
            let source_z = center_z + dz;
            let features = features_for_source(source_x, source_z);
            apply_vegetal_decoration_step(random, seed, source_x, source_z, grid, tags, features);
        }
    }
}

fn place_placed_feature<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    placed: &PlacedRef,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    fn recurse<R: RandomSource>(
        random: &mut R,
        mods: &[VegPlacement],
        i: usize,
        pos: BlockPos,
        grid: &mut VegGrid,
        tags: &VegTags,
        feature: &ConfiguredFeature,
    ) {
        if i == mods.len() {
            place_configured_feature(random, pos, feature, grid, tags);
            return;
        }
        // U8: `get_positions` returns [`Positions`] instead of a freshly
        // allocated `Vec<BlockPos>`. The walk below is the same depth-first
        // recursion in the same order — `Repeat(p, n)` recurses `n` times on the
        // same position, exactly as `for next in vec![p; n]` did. See
        // [`Positions`]'s own doc for why three shapes are exhaustive here.
        match mods[i].get_positions(random, pos, grid, tags) {
            Positions::None => {}
            Positions::One(next) => recurse(random, mods, i + 1, next, grid, tags, feature),
            Positions::Repeat(next, n) => {
                for _ in 0..n {
                    recurse(random, mods, i + 1, next, grid, tags, feature);
                }
            }
        }
    }
    recurse(
        random,
        &placed.placements,
        0,
        origin,
        grid,
        tags,
        &placed.feature,
    );
}

fn place_configured_feature<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    feature: &ConfiguredFeature,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    match feature {
        ConfiguredFeature::SimpleBlock(provider) => {
            census_bump(|c| c.simple_block += 1);
            place_simple_block(random, pos, provider, grid, tags)
        }
        ConfiguredFeature::Tree(cfg) => {
            census_bump(|c| c.tree += 1);
            place_tree(random, pos, cfg, grid, tags)
        }
        ConfiguredFeature::BlockColumn(cfg) => {
            census_bump(|c| c.block_column += 1);
            place_block_column(random, pos, cfg, grid, tags)
        }
        ConfiguredFeature::RandomSelector { default, options } => {
            census_bump(|c| c.random_selector += 1);
            for (chance, option) in options {
                if random.next_float() < *chance {
                    place_placed_feature(random, pos, option, grid, tags);
                    return;
                }
            }
            place_placed_feature(random, pos, default, grid, tags);
        }
        ConfiguredFeature::SimpleRandomSelector(list) => {
            census_bump(|c| c.simple_random_selector += 1);
            if list.is_empty() {
                return;
            }
            let idx = random.next_int_bounded(list.len() as i32) as usize;
            place_placed_feature(random, pos, &list[idx], grid, tags);
        }
        // Issue #478: still a no-op — the module's degrade-don't-crash rule —
        // but a *counted, named* one. `LODESTONE_VEG_STRICT=1` turns it into a
        // panic naming the reason, for answering "which type is missing here"
        // without adding a print to a hot loop.
        ConfiguredFeature::Unsupported(reason) => {
            assert!(
                !census::strict(),
                "LODESTONE_VEG_STRICT: unmodelled vegetal-decoration feature reached a \
                 placement at {pos:?}: {reason}"
            );
            // U8: `entry(reason.clone())` allocated a `String` on EVERY unmodelled
            // dispatch, not just the first — `Entry` has to own the key before it
            // knows whether it needs it. Unmodelled dispatches are not rare
            // (`multiface_growth` alone is in 55 biomes, and ~1/3 of oak attempts
            // roll a fancy/fallen branch), so this one line was the largest single
            // remaining allocation source in the steady-state serve path once the
            // placement engine itself stopped allocating. Probe first, clone only
            // to insert; a `BTreeMap<String, _>` looks up by `&str` via `Borrow`.
            census_bump(|c| match c.unsupported.get_mut(reason.as_str()) {
                Some(n) => *n += 1,
                None => {
                    c.unsupported.insert(reason.clone(), 1);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::density::Resolver;
    use crate::feature::IntProvider;
    use serde_json::Value;
    use crate::rng::{LegacyRandomSource, XoroshiroRandomSource};

    fn grid_with_flat_ground(min_y: i32, height: i32, ground_y: i32) -> VegGrid {
        let mut grid = VegGrid::new(min_y, height, 0, 0);
        for x in 0..16 {
            for z in 0..16 {
                for y in min_y..=ground_y {
                    grid.seed(x, y, z, "minecraft:grass_block".to_string());
                }
                for y in ground_y + 1..min_y + height {
                    grid.seed(x, y, z, "minecraft:air".to_string());
                }
            }
        }
        grid
    }

    #[test]
    fn height_world_surface_matches_flat_ground_plus_one() {
        let grid = grid_with_flat_ground(-64, 384, 70);
        assert_eq!(grid.height_world_surface(5, 5), 71);
    }

    #[test]
    fn height_ocean_floor_skips_water() {
        let mut grid = grid_with_flat_ground(-64, 384, 70);
        grid.seed(5, 71, 5, "minecraft:water".to_string());
        grid.seed(5, 72, 5, "minecraft:water".to_string());
        assert_eq!(grid.height_world_surface(5, 5), 73, "world surface counts water as non-air");
        assert_eq!(grid.height_ocean_floor(5, 5), 71, "ocean floor skips water down to solid ground");
    }

    #[test]
    fn writes_outside_chunk_footprint_are_dropped_not_clamped() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        assert!(!grid.set_if_in_bounds(-1, 70, 5, "minecraft:oak_log".to_string()));
        assert!(!grid.set_if_in_bounds(16, 70, 5, "minecraft:oak_log".to_string()));
        assert!(grid.set_if_in_bounds(0, 70, 5, "minecraft:oak_log".to_string()));
        assert!(grid.set_if_in_bounds(15, 70, 5, "minecraft:oak_log".to_string()));
    }

    #[test]
    fn dirty_cells_only_reports_in_bounds_writes_in_write_order() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        assert!(!grid.set_if_in_bounds(-1, 70, 5, "minecraft:oak_log".to_string()));
        assert!(grid.set_if_in_bounds(3, 70, 5, "minecraft:oak_log".to_string()));
        assert!(grid.set_if_in_bounds(4, 71, 5, "minecraft:oak_leaves".to_string()));
        let cells: Vec<(i32, i32, i32, String)> = grid
            .dirty_cells()
            .map(|(x, y, z, s)| (x, y, z, s.to_string()))
            .collect();
        assert_eq!(
            cells,
            vec![
                (3, 70, 5, "minecraft:oak_log".to_string()),
                (4, 71, 5, "minecraft:oak_leaves".to_string()),
            ],
            "the out-of-bounds attempt must not appear, and order must match write order"
        );
    }

    #[test]
    fn int_provider_weighted_list_parses_from_placed_feature_shape() {
        let v = serde_json::json!({
            "type": "minecraft:weighted_list",
            "distribution": [{"data": 0, "weight": 19}, {"data": 1, "weight": 1}]
        });
        let parsed = try_parse_int_provider(&v).expect("weighted_list must parse");
        match parsed {
            IntProvider::WeightedList(entries) => {
                assert_eq!(entries, vec![(0, 19), (1, 1)]);
            }
            other => panic!("expected WeightedList, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_configured_feature_type_degrades_not_panics() {
        struct EmptyResolver;
        impl Resolver for EmptyResolver {
            fn density_function(&self, _id: &str) -> Value {
                Value::Null
            }
            fn noise(&self, _id: &str) -> crate::density::NoiseParams {
                unimplemented!()
            }
        }
        let doc = serde_json::json!({"type": "minecraft:fallen_tree", "config": {}});
        let feature = parse_configured_feature_doc(&EmptyResolver, &doc);
        assert!(matches!(feature, ConfiguredFeature::Unsupported(_)));
    }

    /// A straight oak trunk over open air must place exactly `tree_height`
    /// logs directly above the origin, and canopy leaves only above that —
    /// the minimal "does the engine place a recognisable tree at all" gate.
    #[test]
    fn straight_trunk_places_exactly_tree_height_logs() {
        let cfg = TreeConfig {
            below_trunk_provider: None,
            trunk_provider: BlockStateProvider::Simple("minecraft:oak_log[axis=y]".to_string()),
            foliage_provider: BlockStateProvider::Simple(
                "minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]".to_string(),
            ),
            trunk_placer: TrunkPlacerCfg::Straight {
                base_height: 5,
                height_rand_a: 0,
                height_rand_b: 0,
            },
            foliage_placer: FoliagePlacerCfg::Blob {
                height: 3,
                radius: IntProvider::Constant(2),
                offset: IntProvider::Constant(0),
            },
            feature_size: FeatureSizeCfg::TwoLayers {
                limit: 1,
                lower_size: 0,
                upper_size: 1,
            },
            decorators: Vec::new(),
        };
        let mut grid = grid_with_flat_ground(-64, 384, 69);
        let tags = VegTags::default();
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(42));
        let origin = BlockPos { x: 8, y: 70, z: 8 };
        place_tree(&mut random, origin, &cfg, &mut grid, &tags);

        let mut log_count = 0;
        for y in 70..70 + 20 {
            if base_id(grid.get(8, y, 8)) == "minecraft:oak_log" {
                log_count += 1;
            }
        }
        assert_eq!(
            log_count, 5,
            "base_height=5, height_rand_a=height_rand_b=0 must always yield exactly 5 logs"
        );

        let mut leaf_count = 0;
        for y in 70..70 + 20 {
            for x in 5..12 {
                for z in 5..12 {
                    if base_id(grid.get(x, y, z)) == "minecraft:oak_leaves" {
                        leaf_count += 1;
                    }
                }
            }
        }
        assert!(leaf_count > 0, "a placed tree must carry at least one leaf block");
    }

    #[test]
    fn tree_over_solid_ceiling_does_not_place_anything() {
        // Control for the space-check gate: an unobstructed tree places
        // logs (proven above); the SAME config with a solid block directly
        // above the origin must place NOTHING — the "not enough room"
        // early-return actually fires, not merely never gets exercised.
        let cfg = TreeConfig {
            below_trunk_provider: None,
            trunk_provider: BlockStateProvider::Simple("minecraft:oak_log[axis=y]".to_string()),
            foliage_provider: BlockStateProvider::Simple(
                "minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]".to_string(),
            ),
            trunk_placer: TrunkPlacerCfg::Straight {
                base_height: 5,
                height_rand_a: 0,
                height_rand_b: 0,
            },
            foliage_placer: FoliagePlacerCfg::Blob {
                height: 3,
                radius: IntProvider::Constant(2),
                offset: IntProvider::Constant(0),
            },
            feature_size: FeatureSizeCfg::TwoLayers {
                limit: 1,
                lower_size: 0,
                upper_size: 1,
            },
            decorators: Vec::new(),
        };
        let mut grid = grid_with_flat_ground(-64, 384, 69);
        // Block the space directly above the trunk's base with stone.
        grid.seed(8, 71, 8, "minecraft:stone".to_string());
        let tags = VegTags::default();
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(42));
        let origin = BlockPos { x: 8, y: 70, z: 8 };
        place_tree(&mut random, origin, &cfg, &mut grid, &tags);

        for y in 70..90 {
            assert_ne!(
                base_id(grid.get(8, y, 8)),
                "minecraft:oak_log",
                "a blocked tree must place zero logs, not a truncated trunk"
            );
        }
    }

    /// Two independently-constructed random sources placing the same tree
    /// at the same seed/position must agree exactly — CLAUDE.md's
    /// determinism rule (never reuse/clone a single generator across the
    /// two sides of a determinism comparison).
    #[test]
    fn tree_placement_is_deterministic_across_two_independent_generators() {
        let cfg = TreeConfig {
            below_trunk_provider: None,
            trunk_provider: BlockStateProvider::Simple("minecraft:spruce_log[axis=y]".to_string()),
            foliage_provider: BlockStateProvider::Simple(
                "minecraft:spruce_leaves[distance=7,persistent=false,waterlogged=false]"
                    .to_string(),
            ),
            trunk_placer: TrunkPlacerCfg::Straight {
                base_height: 5,
                height_rand_a: 2,
                height_rand_b: 1,
            },
            foliage_placer: FoliagePlacerCfg::Spruce {
                radius: IntProvider::Uniform { min: 2, max: 3 },
                offset: IntProvider::Uniform { min: 0, max: 2 },
                trunk_height: IntProvider::Uniform { min: 1, max: 2 },
            },
            feature_size: FeatureSizeCfg::TwoLayers {
                limit: 2,
                lower_size: 0,
                upper_size: 2,
            },
            decorators: Vec::new(),
        };
        let tags = VegTags::default();
        let origin = BlockPos { x: 8, y: 70, z: 8 };

        let mut grid_a = grid_with_flat_ground(-64, 384, 69);
        let mut random_a = WorldgenRandom::new(XoroshiroRandomSource::new(1234));
        place_tree(&mut random_a, origin, &cfg, &mut grid_a, &tags);

        let mut grid_b = grid_with_flat_ground(-64, 384, 69);
        let mut random_b = WorldgenRandom::new(XoroshiroRandomSource::new(1234));
        place_tree(&mut random_b, origin, &cfg, &mut grid_b, &tags);

        for y in -64..320 {
            for x in 0..16 {
                for z in 0..16 {
                    assert_eq!(
                        grid_a.get(x, y, z),
                        grid_b.get(x, y, z),
                        "mismatch at ({x},{y},{z})"
                    );
                }
            }
        }
    }

    /// `DarkOakTrunkPlacer`+`DarkOakFoliagePlacer` over flat open ground must
    /// place a 2×2 trunk: at the base level (before any lean can start — a
    /// 5-tall tree's `leanHeight = 5 - nextInt(4)` is always ≥ 2, so `dy=0`
    /// can never lean) exactly four logs occupy the origin's 2×2 footprint,
    /// and the canopy must reach at least one leaf block. The 2×2 base is the
    /// dark oak signature `Straight`/`Forking` structurally cannot produce,
    /// so this is the gate that would catch a placer that silently placed a
    /// 1-wide trunk.
    #[test]
    fn dark_oak_trunk_places_a_two_by_two_base_and_canopy() {
        let cfg = TreeConfig {
            below_trunk_provider: None,
            trunk_provider: BlockStateProvider::Simple("minecraft:dark_oak_log[axis=y]".to_string()),
            foliage_provider: BlockStateProvider::Simple(
                "minecraft:dark_oak_leaves[distance=7,persistent=false,waterlogged=false]".to_string(),
            ),
            trunk_placer: TrunkPlacerCfg::DarkOak {
                base_height: 5,
                height_rand_a: 0,
                height_rand_b: 0,
            },
            foliage_placer: FoliagePlacerCfg::DarkOak {
                radius: IntProvider::Constant(0),
                offset: IntProvider::Constant(0),
            },
            feature_size: FeatureSizeCfg::ThreeLayers {
                limit: 1,
                upper_limit: 1,
                lower_size: 0,
                middle_size: 1,
                upper_size: 2,
            },
            decorators: Vec::new(),
        };
        let mut grid = grid_with_flat_ground(-64, 384, 69);
        let mut tags = VegTags::default();
        // `place_dark_oak_trunk`'s `isAirOrLeaves` anchor gate needs the
        // leaves tag populated (real vanilla's `#minecraft:leaves`).
        tags.leaves.insert("minecraft:dark_oak_leaves".to_string());
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(99));
        let origin = BlockPos { x: 8, y: 70, z: 8 };
        place_tree(&mut random, origin, &cfg, &mut grid, &tags);

        let mut base_logs = 0;
        for dx in 0..2 {
            for dz in 0..2 {
                if base_id(grid.get(8 + dx, 70, 8 + dz)) == "minecraft:dark_oak_log" {
                    base_logs += 1;
                }
            }
        }
        assert_eq!(
            base_logs, 4,
            "the y=0 level must be a full 2×2 log footprint (no lean has started yet)"
        );

        let mut leaf_count = 0;
        for y in 70..95 {
            for x in 0..16 {
                for z in 0..16 {
                    if base_id(grid.get(x, y, z)) == "minecraft:dark_oak_leaves" {
                        leaf_count += 1;
                    }
                }
            }
        }
        assert!(leaf_count > 0, "a placed dark oak must carry at least one leaf block");
    }

    /// The real `configured_feature/dark_oak.json` (`crates/lodestone-server
    /// /assets/worldgen/configured_feature/dark_oak.json`, transcribed here —
    /// the same convention as the cactus test above) must parse to
    /// [`ConfiguredFeature::Tree`], not [`ConfiguredFeature::Unsupported`] —
    /// the regression control for this module's dark oak increment: before
    /// it, this exact JSON degraded to `"tree: unsupported
    /// trunk/foliage/size/provider"` (the `dark_oak_trunk_placer`/
    /// `dark_oak_foliage_placer`/`three_layers_feature_size` kinds were all
    /// unmodelled), leaving dark_forest's 66.7%-weight branch a silent no-op.
    #[test]
    fn real_dark_oak_configured_feature_parses_as_tree() {
        struct EmptyResolver;
        impl Resolver for EmptyResolver {
            fn density_function(&self, _id: &str) -> Value {
                Value::Null
            }
            fn noise(&self, _id: &str) -> crate::density::NoiseParams {
                unimplemented!()
            }
        }
        let doc = serde_json::json!({
            "type": "minecraft:tree",
            "config": {
                "below_trunk_provider": {
                    "type": "minecraft:rule_based_state_provider",
                    "rules": [{
                        "if_true": {
                            "type": "minecraft:not",
                            "predicate": {
                                "type": "minecraft:matching_block_tag",
                                "tag": "minecraft:cannot_replace_below_tree_trunk"
                            }
                        },
                        "then": {
                            "type": "minecraft:simple_state_provider",
                            "state": {"Name": "minecraft:dirt"}
                        }
                    }]
                },
                "decorators": [],
                "foliage_placer": {
                    "type": "minecraft:dark_oak_foliage_placer",
                    "offset": 0,
                    "radius": 0
                },
                "foliage_provider": {
                    "type": "minecraft:simple_state_provider",
                    "state": {
                        "Name": "minecraft:dark_oak_leaves",
                        "Properties": {"distance": "7", "persistent": "false", "waterlogged": "false"}
                    }
                },
                "minimum_size": {
                    "type": "minecraft:three_layers_feature_size",
                    "upper_size": 2
                },
                "trunk_placer": {
                    "type": "minecraft:dark_oak_trunk_placer",
                    "base_height": 6,
                    "height_rand_a": 2,
                    "height_rand_b": 1
                },
                "trunk_provider": {
                    "type": "minecraft:simple_state_provider",
                    "state": {
                        "Name": "minecraft:dark_oak_log",
                        "Properties": {"axis": "y"}
                    }
                }
            }
        });
        let feature = parse_configured_feature_doc(&EmptyResolver, &doc);
        assert!(
            matches!(feature, ConfiguredFeature::Tree(_)),
            "expected Tree, got {feature:?}"
        );
    }

    #[test]
    fn grass_patch_survives_only_on_supports_vegetation() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(5, 69, 5, "minecraft:grass_block".to_string());
        grid.seed(5, 70, 5, "minecraft:air".to_string());
        grid.seed(5, 69, 6, "minecraft:stone".to_string());
        grid.seed(5, 70, 6, "minecraft:air".to_string());
        let mut tags = VegTags::default();
        tags.supports_vegetation.insert("minecraft:grass_block".to_string());
        let provider = BlockStateProvider::Simple("minecraft:short_grass".to_string());
        let mut random = LegacyRandomSource::new(1);

        place_simple_block(&mut random, BlockPos { x: 5, y: 70, z: 5 }, &provider, &mut grid, &tags);
        assert_eq!(grid.get(5, 70, 5), "minecraft:short_grass");

        place_simple_block(&mut random, BlockPos { x: 5, y: 70, z: 6 }, &provider, &mut grid, &tags);
        assert_eq!(
            grid.get(5, 70, 6),
            "minecraft:air",
            "grass must not survive on a non-supports_vegetation block (stone)"
        );
    }

    /// Real `configured_feature/cactus.json` (see `crates/lodestone-server
    /// /assets/worldgen/configured_feature/cactus.json`, transcribed here)
    /// must parse to [`ConfiguredFeature::BlockColumn`], not
    /// [`ConfiguredFeature::Unsupported`] — the regression control for this
    /// module's cacti increment: before it, this exact JSON degraded
    /// silently.
    #[test]
    fn real_cactus_configured_feature_parses_as_block_column() {
        struct EmptyResolver;
        impl Resolver for EmptyResolver {
            fn density_function(&self, _id: &str) -> Value {
                Value::Null
            }
            fn noise(&self, _id: &str) -> crate::density::NoiseParams {
                unimplemented!()
            }
        }
        let doc = serde_json::json!({
            "type": "minecraft:block_column",
            "config": {
                "allowed_placement": {"type": "minecraft:matching_block_tag", "tag": "minecraft:air"},
                "direction": "up",
                "layers": [
                    {
                        "height": {"type": "minecraft:biased_to_bottom", "max_inclusive": 3, "min_inclusive": 1},
                        "provider": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:cactus", "Properties": {"age": "0"}}}
                    },
                    {
                        "height": {"type": "minecraft:weighted_list", "distribution": [{"data": 0, "weight": 3}, {"data": 1, "weight": 1}]},
                        "provider": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:cactus_flower"}}
                    }
                ],
                "prioritize_tip": false
            }
        });
        let feature = parse_configured_feature_doc(&EmptyResolver, &doc);
        assert!(
            matches!(feature, ConfiguredFeature::BlockColumn(_)),
            "expected BlockColumn, got {feature:?}"
        );
    }

    #[test]
    fn block_column_places_full_sampled_height_when_unobstructed() {
        let cfg = BlockColumnConfig {
            layers: vec![(
                IntProvider::Constant(3),
                BlockStateProvider::Simple("minecraft:cactus[age=0]".to_string()),
            )],
            direction: (0, 1, 0),
            allowed_placement: BlockPredicate::MatchingBlockTag("minecraft:air".to_string()),
            prioritize_tip: false,
        };
        let mut grid = grid_with_flat_ground(-64, 384, 69);
        let tags = VegTags::default();
        let mut random = LegacyRandomSource::new(7);
        let origin = BlockPos { x: 8, y: 70, z: 8 };
        place_block_column(&mut random, origin, &cfg, &mut grid, &tags);

        let mut placed = 0;
        for y in 70..90 {
            if base_id(grid.get(8, y, 8)) == "minecraft:cactus" {
                placed += 1;
            }
        }
        assert_eq!(placed, 3, "an unobstructed constant-height-3 column must place exactly 3 blocks");
    }

    #[test]
    fn block_column_truncates_at_the_first_blocked_probe() {
        // Same config as above, but with stone 2 blocks above the origin —
        // the probe walk starts at origin+direction, so this must be caught
        // on the SECOND probe (y=71 is clear, y=72 is stone), truncating the
        // single layer from 3 down to 1. Control for the "does the truncate
        // path actually fire" half of this feature, not merely the
        // unobstructed happy path above.
        let cfg = BlockColumnConfig {
            layers: vec![(
                IntProvider::Constant(3),
                BlockStateProvider::Simple("minecraft:cactus[age=0]".to_string()),
            )],
            direction: (0, 1, 0),
            allowed_placement: BlockPredicate::MatchingBlockTag("minecraft:air".to_string()),
            prioritize_tip: false,
        };
        let mut grid = grid_with_flat_ground(-64, 384, 69);
        grid.seed(8, 72, 8, "minecraft:stone".to_string());
        let tags = VegTags::default();
        let mut random = LegacyRandomSource::new(7);
        let origin = BlockPos { x: 8, y: 70, z: 8 };
        place_block_column(&mut random, origin, &cfg, &mut grid, &tags);

        assert_eq!(base_id(grid.get(8, 70, 8)), "minecraft:cactus", "the origin block itself is never probe-checked");
        assert_ne!(base_id(grid.get(8, 71, 8)), "minecraft:cactus", "truncated to height 1: only the origin gets a block");
    }

    #[test]
    fn would_survive_cactus_requires_supports_cactus_below_and_clear_sides() {
        let mut tags = VegTags::default();
        tags.supports_cactus.insert("minecraft:sand".to_string());
        let pred = BlockPredicate::WouldSurviveCactus;

        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(5, 69, 5, "minecraft:sand".to_string());
        grid.seed(5, 70, 5, "minecraft:air".to_string());
        grid.seed(6, 70, 5, "minecraft:air".to_string());
        grid.seed(4, 70, 5, "minecraft:air".to_string());
        grid.seed(5, 70, 6, "minecraft:air".to_string());
        grid.seed(5, 70, 4, "minecraft:air".to_string());
        grid.seed(5, 71, 5, "minecraft:air".to_string());
        assert!(
            pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "sand below, all 4 horizontal neighbours air: must survive"
        );

        // Control: a solid neighbour must fail the check that just passed.
        grid.seed(6, 70, 5, "minecraft:stone".to_string());
        assert!(
            !pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "a solid horizontal neighbour must block cactus survival"
        );

        // Control: a non-supports_cactus block below must also fail.
        grid.seed(6, 70, 5, "minecraft:air".to_string());
        grid.seed(5, 69, 5, "minecraft:stone".to_string());
        assert!(
            !pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "stone below (not in supports_cactus) must block cactus survival"
        );
    }

    #[test]
    fn would_survive_sugar_cane_ignores_adjacency_by_design() {
        // See BlockPredicate::WouldSurviveSugarCane's own doc: the
        // water-adjacency half of CactusBlock's real-vanilla sibling
        // (SugarCaneBlock.canSurvive) is deliberately NOT modelled here —
        // every patch_sugar_cane* placed feature re-checks it via an
        // explicit sibling `any_of(matching_fluids)`. This predicate alone
        // must therefore pass on bare sand with NO adjacent water.
        let mut tags = VegTags::default();
        tags.supports_sugar_cane.insert("minecraft:sand".to_string());
        let pred = BlockPredicate::WouldSurviveSugarCane;
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(5, 69, 5, "minecraft:sand".to_string());
        grid.seed(5, 70, 5, "minecraft:air".to_string());
        assert!(pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }));

        // Control: stone below (not in supports_sugar_cane) must fail.
        grid.seed(5, 69, 5, "minecraft:stone".to_string());
        assert!(!pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }));
    }

    #[test]
    fn matching_fluids_any_of_is_the_real_gate_sugar_cane_relies_on() {
        // The explicit sibling predicate patch_sugar_cane*'s own JSON uses
        // instead of adjacency-in-would_survive (see the test above). This
        // is the control that proves `AnyOf`/`MatchingFluid` actually gate
        // placement rather than defaulting to `True` the way every
        // unrecognised combinator used to (see BlockPredicate::AllOf's doc).
        let pred = BlockPredicate::AnyOf(vec![
            BlockPredicate::MatchingFluid {
                fluids: vec!["minecraft:water".to_string(), "minecraft:flowing_water".to_string()],
                offset: (1, -1, 0),
            },
            BlockPredicate::MatchingFluid {
                fluids: vec!["minecraft:water".to_string(), "minecraft:flowing_water".to_string()],
                offset: (-1, -1, 0),
            },
        ]);
        let tags = VegTags::default();
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(5, 69, 5, "minecraft:sand".to_string());
        grid.seed(5, 70, 5, "minecraft:air".to_string());
        grid.seed(6, 69, 5, "minecraft:sand".to_string());
        assert!(
            !pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "no adjacent water: must fail"
        );

        grid.seed(6, 69, 5, "minecraft:water".to_string());
        assert!(
            pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "water at offset (1,-1,0): must pass"
        );
    }
}
