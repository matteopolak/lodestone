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
//! `GiantTrunkPlacer`/`JungleFoliagePlacer`, dark oak's `FancyTrunkPlacer`,
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
//! jungle/dark-oak/mangrove/cherry all have). Jungle (`GiantTrunkPlacer`+
//! `MegaJungleFoliagePlacer`), dark oak (`DarkOakTrunkPlacer`+
//! `DarkOakFoliagePlacer`, a real 2×2 trunk with branches), mangrove
//! (`UpwardsBranchingTrunkPlacer` — has real above-water roots) and cherry
//! (`CherryTrunkPlacer`+`CherryFoliagePlacer`) remain
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

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::density::Resolver;
use crate::feature::{BlockPos, IntProvider, STEP_VEGETAL_DECORATION};
use crate::rng::{RandomSource, WorldgenRandom};

fn base_id(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// `Heightmap.Types` (the subset vegetal decoration references). See this
/// module's doc "Approximations, named" for why only two scans back all
/// five.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightmapKind {
    OceanFloor,
    OceanFloorWg,
    WorldSurface,
    WorldSurfaceWg,
    MotionBlocking,
}

impl HeightmapKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "OCEAN_FLOOR" => Some(Self::OceanFloor),
            "OCEAN_FLOOR_WG" => Some(Self::OceanFloorWg),
            "WORLD_SURFACE" => Some(Self::WorldSurface),
            "WORLD_SURFACE_WG" => Some(Self::WorldSurfaceWg),
            "MOTION_BLOCKING" | "MOTION_BLOCKING_NO_LEAVES" => Some(Self::MotionBlocking),
            _ => None,
        }
    }

    fn scan(self, grid: &VegGrid, x: i32, z: i32) -> i32 {
        match self {
            Self::OceanFloor | Self::OceanFloorWg => grid.height_ocean_floor(x, z),
            Self::WorldSurface | Self::WorldSurfaceWg | Self::MotionBlocking => {
                grid.height_world_surface(x, z)
            }
        }
    }
}

/// `net.minecraft.world.level.levelgen.blockpredicates.BlockPredicate` (the
/// subset grass/flower/tree placement and `RuleBasedStateProvider` use).
/// Unknown predicate types degrade to [`BlockPredicate::True`] (this
/// module's blanket "unsupported degrades, never panics" rule) — see this
/// module's doc comment for why that must never be a panic.
#[derive(Clone, Debug)]
pub enum BlockPredicate {
    True,
    Not(Box<BlockPredicate>),
    /// `AllOfPredicate`/`AnyOfPredicate` — added for `patch_sugar_cane*`'s
    /// `block_predicate_filter`, which nests a `matching_block_tag` +
    /// `would_survive` + `any_of(matching_fluids)` combinator. Before these
    /// two variants existed, every combinator type fell through to
    /// [`BlockPredicate::True`] — harmless while nothing in scope used one,
    /// but it would have made sugar cane's water-adjacency requirement a
    /// silent no-op *in the wrong direction* (always-pass instead of
    /// always-fail) the moment `BlockColumnFeature` support let sugar cane's
    /// placed feature actually run — named here because that direction of
    /// bug is the more dangerous one this module's "degrade, don't panic"
    /// convention can produce.
    AllOf(Vec<BlockPredicate>),
    AnyOf(Vec<BlockPredicate>),
    MatchingBlockTag(String),
    /// `MatchingFluidPredicate` — `fluids` is the JSON's raw
    /// `minecraft:water`/`minecraft:flowing_water`/`minecraft:lava`/
    /// `minecraft:flowing_lava` id list; `offset` is `(dx, dy, dz)` added to
    /// the tested position. Matched via [`fluid_base_matches`] because this
    /// engine's grid never distinguishes a fluid's source/flowing variant
    /// (the same "known representation gap: fluid `level`"
    /// `docs/worldgen-parity.md` already names) — both JSON ids for one
    /// fluid collapse onto the one base id our grid can ever hold.
    MatchingFluid {
        fluids: Vec<String>,
        offset: (i32, i32, i32),
    },
    /// Approximates every `would_survive` check this module reaches as
    /// `VegetationBlock.mayPlaceOn` — see module doc. The default for any
    /// `would_survive` whose tested state isn't one of the two special-cased
    /// below.
    WouldSurviveOnSupportsVegetation,
    /// `would_survive` on a `minecraft:cactus` state — `CactusBlock
    /// .canSurvive`: below is cactus itself or `#minecraft:supports_cactus`,
    /// all 4 horizontal neighbours non-solid, block above not a fluid.
    /// "Non-solid" is approximated as "air" (see [`BlockPredicate::test`]'s
    /// own doc on this one) — a named narrowing, not the full vanilla
    /// solidity table, which this crate has no other reason to carry.
    WouldSurviveCactus,
    /// `would_survive` on a `minecraft:sugar_cane` state — deliberately
    /// **omits** `SugarCaneBlock.canSurvive`'s water-adjacency half: every
    /// `patch_sugar_cane*` placed feature already re-checks that adjacency
    /// explicitly via a sibling `any_of(matching_fluids)` predicate in the
    /// same `all_of`, so modelling it twice would be redundant, not more
    /// correct.
    WouldSurviveSugarCane,
}

fn parse_predicate_list(v: &Value) -> Vec<BlockPredicate> {
    v["predicates"]
        .as_array()
        .map(|arr| arr.iter().map(BlockPredicate::parse).collect())
        .unwrap_or_default()
}

fn parse_offset(v: &Value) -> (i32, i32, i32) {
    let Some(arr) = v.as_array() else {
        return (0, 0, 0);
    };
    let get = |i: usize| arr.get(i).and_then(Value::as_i64).unwrap_or(0) as i32;
    (get(0), get(1), get(2))
}

/// See [`BlockPredicate::MatchingFluid`]'s doc: both the source and flowing
/// JSON ids for one fluid collapse onto this engine's single base id.
fn fluid_base_matches(fluid_id: &str, base: &str) -> bool {
    match fluid_id {
        "minecraft:water" | "minecraft:flowing_water" => base == "minecraft:water",
        "minecraft:lava" | "minecraft:flowing_lava" => base == "minecraft:lava",
        _ => false,
    }
}

impl BlockPredicate {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().unwrap_or("minecraft:true");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "not" => BlockPredicate::Not(Box::new(BlockPredicate::parse(&v["predicate"]))),
            "all_of" => BlockPredicate::AllOf(parse_predicate_list(v)),
            "any_of" => BlockPredicate::AnyOf(parse_predicate_list(v)),
            "matching_block_tag" => {
                BlockPredicate::MatchingBlockTag(v["tag"].as_str().unwrap_or_default().to_string())
            }
            "matching_fluids" => BlockPredicate::MatchingFluid {
                fluids: v["fluids"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                offset: parse_offset(&v["offset"]),
            },
            "would_survive" => match v["state"]["Name"].as_str().unwrap_or("") {
                "minecraft:cactus" => BlockPredicate::WouldSurviveCactus,
                "minecraft:sugar_cane" => BlockPredicate::WouldSurviveSugarCane,
                _ => BlockPredicate::WouldSurviveOnSupportsVegetation,
            },
            _ => BlockPredicate::True,
        }
    }

    fn test(&self, grid: &VegGrid, tags: &VegTags, pos: BlockPos) -> bool {
        match self {
            BlockPredicate::True => true,
            BlockPredicate::Not(inner) => !inner.test(grid, tags, pos),
            BlockPredicate::AllOf(list) => list.iter().all(|p| p.test(grid, tags, pos)),
            BlockPredicate::AnyOf(list) => list.iter().any(|p| p.test(grid, tags, pos)),
            BlockPredicate::MatchingBlockTag(tag) => {
                let base = base_id(grid.get(pos.x, pos.y, pos.z));
                if tag == "minecraft:air" {
                    is_air(base)
                } else if tag == "minecraft:cannot_replace_below_tree_trunk" {
                    tags.cannot_replace_below_tree_trunk.contains(base)
                } else {
                    false
                }
            }
            BlockPredicate::MatchingFluid { fluids, offset } => {
                let (dx, dy, dz) = *offset;
                let base = base_id(grid.get(pos.x + dx, pos.y + dy, pos.z + dz));
                fluids.iter().any(|f| fluid_base_matches(f, base))
            }
            BlockPredicate::WouldSurviveOnSupportsVegetation => {
                let below = base_id(grid.get(pos.x, pos.y - 1, pos.z));
                tags.supports_vegetation.contains(below)
            }
            BlockPredicate::WouldSurviveCactus => {
                let below = base_id(grid.get(pos.x, pos.y - 1, pos.z));
                if below != "minecraft:cactus" && !tags.supports_cactus.contains(below) {
                    return false;
                }
                let neighbours_ok = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().all(|&(dx, dz)| {
                    is_air(base_id(grid.get(pos.x + dx, pos.y, pos.z + dz)))
                });
                if !neighbours_ok {
                    return false;
                }
                !is_fluid(base_id(grid.get(pos.x, pos.y + 1, pos.z)))
            }
            BlockPredicate::WouldSurviveSugarCane => {
                let below = base_id(grid.get(pos.x, pos.y - 1, pos.z));
                below == "minecraft:sugar_cane" || tags.supports_sugar_cane.contains(below)
            }
        }
    }
}

fn is_air(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

fn is_fluid(base: &str) -> bool {
    matches!(base, "minecraft:water" | "minecraft:lava")
}

/// `net.minecraft.world.level.levelgen.feature.stateproviders.BlockStateProvider`
/// (the subset grass/flower/tree configs use). Parsing degrades to `None`
/// on an unsupported provider type or a sub-provider that itself failed to
/// parse — see module doc.
#[derive(Clone, Debug)]
pub enum BlockStateProvider {
    Simple(String),
    /// `(weight, state)` pairs, declaration order (matches
    /// [`IntProvider::WeightedList`]'s own walk).
    Weighted(Vec<(i32, String)>),
    NoiseThreshold {
        seed: i64,
        first_octave: i32,
        amplitudes: Vec<f64>,
        scale: f64,
        threshold: f64,
        high_chance: f32,
        default_state: String,
        low_states: Vec<String>,
        high_states: Vec<String>,
    },
    RuleBased {
        rules: Vec<(BlockPredicate, Box<BlockStateProvider>)>,
        fallback: Option<Box<BlockStateProvider>>,
    },
}

fn canon_state(v: &Value) -> String {
    crate::feature::canon_state(v)
}

impl BlockStateProvider {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "simple_state_provider" => Some(BlockStateProvider::Simple(canon_state(&v["state"]))),
            "weighted_state_provider" => {
                let entries = v["entries"].as_array()?;
                let parsed = entries
                    .iter()
                    .map(|e| {
                        let weight = e["weight"].as_i64().unwrap_or(1) as i32;
                        (weight, canon_state(&e["data"]))
                    })
                    .collect();
                Some(BlockStateProvider::Weighted(parsed))
            }
            "noise_threshold_provider" => Some(BlockStateProvider::NoiseThreshold {
                seed: v["seed"].as_i64()?,
                first_octave: v["noise"]["firstOctave"].as_i64().unwrap_or(0) as i32,
                amplitudes: v["noise"]["amplitudes"]
                    .as_array()?
                    .iter()
                    .map(|a| a.as_f64().unwrap_or(0.0))
                    .collect(),
                scale: v["scale"].as_f64()?,
                threshold: v["threshold"].as_f64()?,
                high_chance: v["high_chance"].as_f64().unwrap_or(0.0) as f32,
                default_state: canon_state(&v["default_state"]),
                low_states: v["low_states"]
                    .as_array()?
                    .iter()
                    .map(canon_state)
                    .collect(),
                high_states: v["high_states"]
                    .as_array()?
                    .iter()
                    .map(canon_state)
                    .collect(),
            }),
            "rule_based_state_provider" => {
                let raw_rules = v["rules"].as_array()?;
                let mut rules = Vec::with_capacity(raw_rules.len());
                for rule in raw_rules {
                    let then = BlockStateProvider::try_parse(&rule["then"])?;
                    rules.push((BlockPredicate::parse(&rule["if_true"]), Box::new(then)));
                }
                let fallback = match v.get("fallback") {
                    Some(f) if !f.is_null() => Some(Box::new(BlockStateProvider::try_parse(f)?)),
                    _ => None,
                };
                Some(BlockStateProvider::RuleBased { rules, fallback })
            }
            _ => None,
        }
    }

    fn get_state<R: RandomSource>(
        &self,
        grid: &VegGrid,
        tags: &VegTags,
        random: &mut R,
        pos: BlockPos,
    ) -> Option<String> {
        match self {
            BlockStateProvider::Simple(state) => Some(state.clone()),
            BlockStateProvider::Weighted(entries) => {
                let total: i32 = entries.iter().map(|(w, _)| *w).sum();
                let mut roll = random.next_int_bounded(total.max(1));
                for (weight, state) in entries {
                    roll -= *weight;
                    if roll < 0 {
                        return Some(state.clone());
                    }
                }
                entries.last().map(|(_, s)| s.clone())
            }
            BlockStateProvider::NoiseThreshold {
                seed,
                first_octave,
                amplitudes,
                scale,
                threshold,
                high_chance,
                default_state,
                low_states,
                high_states,
            } => {
                let mut legacy = crate::rng::LegacyRandomSource::new(*seed);
                let noise =
                    crate::noise::NormalNoise::create(&mut legacy, *first_octave, amplitudes);
                let value = noise.get_value(
                    f64::from(pos.x) * scale,
                    f64::from(pos.y) * scale,
                    f64::from(pos.z) * scale,
                );
                if value < *threshold {
                    let idx = random.next_int_bounded(low_states.len().max(1) as i32) as usize;
                    low_states.get(idx).cloned().or_else(|| Some(default_state.clone()))
                } else if random.next_float() < *high_chance {
                    let idx = random.next_int_bounded(high_states.len().max(1) as i32) as usize;
                    high_states.get(idx).cloned().or_else(|| Some(default_state.clone()))
                } else {
                    Some(default_state.clone())
                }
            }
            BlockStateProvider::RuleBased { rules, fallback } => {
                for (predicate, then) in rules {
                    if predicate.test(grid, tags, pos) {
                        return then.get_state(grid, tags, random, pos);
                    }
                }
                fallback
                    .as_ref()
                    .and_then(|f| f.get_state(grid, tags, random, pos))
            }
        }
    }
}

/// `#minecraft:cannot_replace_below_tree_trunk`/`#minecraft:supports_vegetation`/
/// `#minecraft:replaceable_by_trees`/`#minecraft:logs`, resolved once at
/// generator construction via [`crate::compose::resolve_block_tag`] — the
/// same tag-closure machinery [`crate::compose::build_ore_tag_map`] already
/// uses for ore `RuleTest::TagMatch`, applied here to the four tags this
/// module's own predicates/checks reference.
#[derive(Debug, Default, Clone)]
pub struct VegTags {
    pub cannot_replace_below_tree_trunk: HashSet<String>,
    pub supports_vegetation: HashSet<String>,
    pub replaceable_by_trees: HashSet<String>,
    pub logs: HashSet<String>,
    /// `#minecraft:supports_cactus` — `CactusBlock.canSurvive`'s below-block
    /// check (cactus/`BlockColumnFeature`, added alongside sugar cane).
    pub supports_cactus: HashSet<String>,
    /// `#minecraft:supports_sugar_cane` — `SugarCaneBlock.canSurvive`'s
    /// below-block check. The adjacency-to-water half of that same method
    /// is *not* modelled here; it doesn't need to be, because every biome's
    /// own `patch_sugar_cane*` placed-feature JSON already encodes it as an
    /// explicit sibling `any_of`/`matching_fluids` predicate — see
    /// [`BlockPredicate::MatchingFluid`].
    pub supports_sugar_cane: HashSet<String>,
}

/// Resolves [`VegTags`] from a [`Resolver`]. Empty sets (never a panic) if
/// the resolver has no data for a given tag id — matches every other #295/
/// #406 resolver method's "no data supplied" convention.
#[must_use]
pub fn build_veg_tags(resolver: &dyn Resolver) -> VegTags {
    let resolve = |id: &str| {
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        crate::compose::resolve_block_tag(resolver, id, &mut out, &mut seen);
        out
    };
    VegTags {
        cannot_replace_below_tree_trunk: resolve("minecraft:cannot_replace_below_tree_trunk"),
        supports_vegetation: resolve("minecraft:supports_vegetation"),
        replaceable_by_trees: resolve("minecraft:replaceable_by_trees"),
        logs: resolve("minecraft:logs"),
        supports_cactus: resolve("minecraft:supports_cactus"),
        supports_sugar_cane: resolve("minecraft:supports_sugar_cane"),
    }
}

/// `net.minecraft.world.level.levelgen.placement.PlacementModifier` (the
/// vegetal-decoration subset). A separate type from [`super::Placement`]
/// (the ore engine's) rather than an extension of it — the two engines share
/// no placement instances and vegetal decoration needs live grid reads
/// (heightmap, air/tag checks) the ore engine's modifiers never did, so
/// giving them their own `get_positions` signature avoids retrofitting a
/// grid parameter onto ore's already-proven, already-tested type.
#[derive(Clone, Debug)]
pub enum VegPlacement {
    Count(IntProvider),
    InSquare,
    Heightmap(HeightmapKind),
    Biome,
    RarityFilter(i32),
    SurfaceWaterDepthFilter(i32),
    /// `NoiseThresholdCountPlacement` — `Biome.BIOME_INFO_NOISE` gated count.
    NoiseThresholdCount {
        noise_level: f64,
        below: i32,
        above: i32,
    },
    RandomOffset {
        xz: IntProvider,
        y: IntProvider,
    },
    BlockPredicateFilter(BlockPredicate),
}

/// Parses an `IntProvider` for a vegetal-decoration placement field without
/// risking [`IntProvider::parse`]'s panic on an unrecognised type — see
/// module doc on why nothing in this file may panic on data it doesn't yet
/// model. A dedicated, duplicated mini-parser (not a new
/// [`IntProvider::try_parse`] on the shared type) so the change carries zero
/// risk to the already-proven ore engine's parsing contract.
fn try_parse_int_provider(v: &Value) -> Option<IntProvider> {
    match v {
        Value::Number(n) => Some(IntProvider::Constant(n.as_i64()? as i32)),
        Value::Object(_) => {
            let ty = v["type"].as_str().unwrap_or("minecraft:constant");
            match ty.strip_prefix("minecraft:").unwrap_or(ty) {
                "constant" => Some(IntProvider::Constant(v["value"].as_i64()? as i32)),
                "uniform" => Some(IntProvider::Uniform {
                    min: v["min_inclusive"].as_i64()? as i32,
                    max: v["max_inclusive"].as_i64()? as i32,
                }),
                // The REAL `TrapezoidInt` sample (two draws, triangular),
                // not a `Uniform` stand-in — see `IntProvider::Trapezoid`'s
                // own doc comment on why the approximation this replaced
                // was a real bug, not just a shape simplification: it
                // changed how many `nextInt` calls this placement consumed,
                // desyncing every RNG draw after the first `random_offset`
                // from vanilla's own stream. Found via a real JVM oracle
                // (`tests/vegetation_parity.rs`), not by inspection.
                "trapezoid" => Some(IntProvider::Trapezoid {
                    min: v["min"].as_i64()? as i32,
                    max: v["max"].as_i64()? as i32,
                    plateau: v["plateau"].as_i64().unwrap_or(0) as i32,
                }),
                "weighted_list" => {
                    let entries = v["distribution"]
                        .as_array()?
                        .iter()
                        .map(|e| {
                            Some((
                                e["data"].as_i64()? as i32,
                                e["weight"].as_i64()? as i32,
                            ))
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(IntProvider::WeightedList(entries))
                }
                "biased_to_bottom" => Some(IntProvider::BiasedToBottom {
                    min: v["min_inclusive"].as_i64()? as i32,
                    max: v["max_inclusive"].as_i64()? as i32,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

impl VegPlacement {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "count" => Some(VegPlacement::Count(try_parse_int_provider(&v["count"])?)),
            "in_square" => Some(VegPlacement::InSquare),
            "heightmap" => Some(VegPlacement::Heightmap(HeightmapKind::parse(
                v["heightmap"].as_str()?,
            )?)),
            "biome" => Some(VegPlacement::Biome),
            "rarity_filter" => Some(VegPlacement::RarityFilter(v["chance"].as_i64()? as i32)),
            "surface_water_depth_filter" => Some(VegPlacement::SurfaceWaterDepthFilter(
                v["max_water_depth"].as_i64()? as i32,
            )),
            "noise_threshold_count" => Some(VegPlacement::NoiseThresholdCount {
                noise_level: v["noise_level"].as_f64()?,
                below: v["below_noise"].as_i64()? as i32,
                above: v["above_noise"].as_i64()? as i32,
            }),
            "random_offset" => Some(VegPlacement::RandomOffset {
                xz: try_parse_int_provider(&v["xz_spread"])?,
                y: try_parse_int_provider(&v["y_spread"])?,
            }),
            "block_predicate_filter" => Some(VegPlacement::BlockPredicateFilter(
                BlockPredicate::parse(&v["predicate"]),
            )),
            _ => None,
        }
    }

    fn get_positions<R: RandomSource>(
        &self,
        random: &mut R,
        pos: BlockPos,
        grid: &VegGrid,
        tags: &VegTags,
    ) -> Vec<BlockPos> {
        match self {
            VegPlacement::Count(ip) => {
                let n = ip.sample(random);
                vec![pos; n.max(0) as usize]
            }
            VegPlacement::InSquare => {
                let x = pos.x + random.next_int_bounded(16);
                let z = pos.z + random.next_int_bounded(16);
                vec![BlockPos { x, y: pos.y, z }]
            }
            VegPlacement::Heightmap(kind) => {
                let height = kind.scan(grid, pos.x, pos.z);
                if height > grid.min_y {
                    vec![BlockPos {
                        x: pos.x,
                        y: height,
                        z: pos.z,
                    }]
                } else {
                    Vec::new()
                }
            }
            VegPlacement::Biome => vec![pos],
            VegPlacement::RarityFilter(chance) => {
                if random.next_float() < 1.0 / *chance as f32 {
                    vec![pos]
                } else {
                    Vec::new()
                }
            }
            VegPlacement::SurfaceWaterDepthFilter(max_depth) => {
                let ocean = grid.height_ocean_floor(pos.x, pos.z);
                let surface = grid.height_world_surface(pos.x, pos.z);
                if surface - ocean <= *max_depth {
                    vec![pos]
                } else {
                    Vec::new()
                }
            }
            VegPlacement::NoiseThresholdCount {
                noise_level,
                below,
                above,
            } => {
                let noise = crate::noise::biome_info_noise_value(
                    f64::from(pos.x) / 200.0,
                    f64::from(pos.z) / 200.0,
                );
                let n = if noise < *noise_level { *below } else { *above };
                vec![pos; n.max(0) as usize]
            }
            VegPlacement::RandomOffset { xz, y } => {
                // Two INDEPENDENT samples of `xz` (x, then z) — matches
                // `RandomOffsetPlacement.getPositions`'s two separate
                // `this.xzSpread.sample(random)` calls, not one shared draw.
                let scatter_x = pos.x + xz.sample(random);
                let scatter_y = pos.y + y.sample(random);
                let scatter_z = pos.z + xz.sample(random);
                vec![BlockPos {
                    x: scatter_x,
                    y: scatter_y,
                    z: scatter_z,
                }]
            }
            VegPlacement::BlockPredicateFilter(pred) => {
                census_bump(|c| c.block_predicate_filter_in += 1);
                if pred.test(grid, tags, pos) {
                    census_bump(|c| c.block_predicate_filter_out += 1);
                    vec![pos]
                } else {
                    Vec::new()
                }
            }
        }
    }
}

/// `net.minecraft.world.level.levelgen.feature.treedecorators.TreeDecorator`
/// (the subset reachable from oak/birch's `_bees_*` variants — see module
/// doc). Any other decorator type parses to [`Decorator::Unsupported`] (a
/// silent no-op — see [`place_beehive_decorator`]'s doc on the RNG-continuity
/// cost of skipping one).
#[derive(Clone, Debug)]
pub enum Decorator {
    Beehive { probability: f32 },
    Unsupported,
}

impl Decorator {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().unwrap_or("");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "beehive" => Decorator::Beehive {
                probability: v["probability"].as_f64().unwrap_or(0.0) as f32,
            },
            _ => Decorator::Unsupported,
        }
    }
}

/// `net.minecraft.world.level.levelgen.feature.trunkplacers.TrunkPlacer` (the
/// `Straight`/`Forking` subset — issue #428 adds `Forking`, acacia's real
/// trunk placer, alongside the `Straight` this module shipped with under
/// #406). Both variants carry the identical `(base_height, height_rand_a,
/// height_rand_b)` triple `TrunkPlacer.getTreeHeight` (a base-class method,
/// not overridden by either subclass) draws from — kept as one shared shape
/// rather than duplicating the three fields per variant.
#[derive(Clone, Copy, Debug)]
pub enum TrunkPlacerCfg {
    Straight {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `ForkingTrunkPlacer` — acacia's real trunk (issue #428): a single
    /// leaning column, plus (usually) one branch in a different horizontal
    /// direction. See [`place_trunk`] for the port of `placeTrunk` itself.
    Forking {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
}

impl TrunkPlacerCfg {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        let base_height = v["base_height"].as_i64()? as i32;
        let height_rand_a = v["height_rand_a"].as_i64()? as i32;
        let height_rand_b = v["height_rand_b"].as_i64()? as i32;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "straight_trunk_placer" => Some(Self::Straight { base_height, height_rand_a, height_rand_b }),
            "forking_trunk_placer" => Some(Self::Forking { base_height, height_rand_a, height_rand_b }),
            _ => None,
        }
    }

    fn heights(&self) -> (i32, i32, i32) {
        match *self {
            Self::Straight { base_height, height_rand_a, height_rand_b }
            | Self::Forking { base_height, height_rand_a, height_rand_b } => {
                (base_height, height_rand_a, height_rand_b)
            }
        }
    }

    /// `TrunkPlacer.getTreeHeight` — shared across every subclass (not
    /// overridden by `ForkingTrunkPlacer`, `DarkOakTrunkPlacer`, etc. in
    /// real vanilla either).
    fn get_tree_height<R: RandomSource>(&self, random: &mut R) -> i32 {
        let (base_height, height_rand_a, height_rand_b) = self.heights();
        base_height + random.next_int_bounded(height_rand_a + 1) + random.next_int_bounded(height_rand_b + 1)
    }
}

/// `FoliagePlacer.FoliageAttachment` — one trunk-placement result the
/// foliage placer runs `create_foliage` against. [`TrunkPlacerCfg::Straight`]
/// always produces exactly one; [`TrunkPlacerCfg::Forking`] can produce one
/// or two (the lean column always attaches if it placed any log at all; the
/// branch attaches only if its own direction differs from the lean's AND it
/// placed at least one log — see [`place_trunk`]).
#[derive(Clone, Copy, Debug)]
struct Attachment {
    pos: BlockPos,
    /// `FoliageAttachment.radiusOffset` — nonzero only for
    /// `ForkingTrunkPlacer`'s primary (lean) attachment (`1`); every other
    /// attachment this module produces uses `0`. Consumed by
    /// [`FoliagePlacerCfg::Acacia`]'s `create_foliage`.
    radius_offset: i32,
    /// `FoliageAttachment.doubleTrunk` — always `false` for every trunk
    /// placer this module implements (`Straight`, `Forking`); kept as a
    /// real field rather than assumed, since a future `DarkOakTrunkPlacer`
    /// port would set it `true` for its primary attachment. Not read by
    /// anything yet (no implemented foliage placer branches on it — see
    /// [`FoliagePlacerCfg::should_skip_location`]'s own doc on why even
    /// `Acacia` never needs it), hence the explicit allow rather than
    /// deleting a field the next placer family will need on day one.
    #[allow(dead_code)]
    double_trunk: bool,
}

/// `ForkingTrunkPlacer.placeTrunk` — acacia's real trunk (issue #428).
/// Places `placeBelowTrunkBlock(origin.below())` first (matching
/// `StraightTrunkPlacer`'s own convention, [`place_tree`]'s existing
/// pre-loop call for the `Straight` case), then a single leaning log column
/// (`Direction.Plane.HORIZONTAL.getRandomDirection` = `random.nextInt(4)`
/// indexing `[NORTH, EAST, SOUTH, WEST]`, i.e. step vectors `(0,-1)`,
/// `(1,0)`, `(0,1)`, `(-1,0)` in that exact order — `Direction.java`'s own
/// `Plane.HORIZONTAL` face array), and then, only if a *second*,
/// independently-rolled direction differs from the first, a branch that
/// starts partway up the lean and runs for a few more logs in that second
/// direction. Both attachments are only added if `placeLog` actually placed
/// at least one log along that column (`OptionalInt` in the Java; `Option`
/// here) — an entirely-blocked lean or branch contributes no
/// [`Attachment`], matching vanilla exactly rather than attaching at a
/// position nothing was ever placed at.
/// Returns `(attachments, trunk_positions, placed_any)`. `trunk_positions`
/// is every position `trunkSetter`/`placeBelowTrunkBlock` was actually
/// invoked at (matching vanilla's real `trunks` set in `TreeFeature.place`)
/// — including the below-origin block, which real `placeBelowTrunkBlock`
/// places via the SAME `trunkSetter` (`TrunkPlacer.java`'s own
/// `placeBelowTrunkBlock`), and therefore counts as a real distance-0
/// source for [`update_leaf_distances`], not merely cosmetic soil.
fn place_forking_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
) -> (Vec<Attachment>, Vec<BlockPos>, bool) {
    let mut trunk_positions = Vec::new();
    if let Some(below_provider) = below_trunk_provider {
        let below_pos = BlockPos { x: origin.x, y: origin.y - 1, z: origin.z };
        if let Some(state) = below_provider.get_state(grid, tags, random, below_pos) {
            grid.set_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
            trunk_positions.push(below_pos);
        }
    }

    let mut attachments = Vec::new();
    let mut placed_any = false;

    const STEP: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)]; // NORTH, EAST, SOUTH, WEST
    let lean_direction = STEP[random.next_int_bounded(4) as usize];
    let lean_height = tree_height - random.next_int_bounded(4) - 1;
    let mut lean_steps = 3 - random.next_int_bounded(3);
    let mut tx = origin.x;
    let mut tz = origin.z;
    let mut ey: Option<i32> = None;
    for yo in 0..tree_height {
        let yy = origin.y + yo;
        if yo >= lean_height && lean_steps > 0 {
            tx += lean_direction.0;
            tz += lean_direction.1;
            lean_steps -= 1;
        }
        let pos = BlockPos { x: tx, y: yy, z: tz };
        let base = base_id(grid.get(pos.x, pos.y, pos.z));
        if is_air(base) || tags.replaceable_by_trees.contains(base) {
            if let Some(state) = trunk_provider.get_state(grid, tags, random, pos) {
                grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
                placed_any = true;
                trunk_positions.push(pos);
                ey = Some(yy + 1);
            }
        }
    }
    if let Some(y) = ey {
        attachments.push(Attachment { pos: BlockPos { x: tx, y, z: tz }, radius_offset: 1, double_trunk: false });
    }

    tx = origin.x;
    tz = origin.z;
    let branch_direction = STEP[random.next_int_bounded(4) as usize];
    if branch_direction != lean_direction {
        let branch_pos = lean_height - random.next_int_bounded(2) - 1;
        let mut branch_steps = 1 + random.next_int_bounded(3);
        let mut ey: Option<i32> = None;
        let mut yo = branch_pos;
        while yo < tree_height && branch_steps > 0 {
            if yo >= 1 {
                let yy = origin.y + yo;
                tx += branch_direction.0;
                tz += branch_direction.1;
                let pos = BlockPos { x: tx, y: yy, z: tz };
                let base = base_id(grid.get(pos.x, pos.y, pos.z));
                if is_air(base) || tags.replaceable_by_trees.contains(base) {
                    if let Some(state) = trunk_provider.get_state(grid, tags, random, pos) {
                        grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
                        placed_any = true;
                        trunk_positions.push(pos);
                        ey = Some(yy + 1);
                    }
                }
            }
            branch_steps -= 1;
            yo += 1;
        }
        if let Some(y) = ey {
            attachments.push(Attachment { pos: BlockPos { x: tx, y, z: tz }, radius_offset: 0, double_trunk: false });
        }
    }

    (attachments, trunk_positions, placed_any)
}

/// `TreeFeature.updateLeaves` — the real post-processing pass vanilla runs
/// after a tree's trunk, foliage AND decorators have all been placed: a
/// multi-source BFS from every position in `trunk_positions` (bucket 0),
/// lowering every reachable `distance`-carrying block's `distance` property
/// to the true shortest distance-to-a-log, capped at 7 (never written past
/// that cap, matching `LeavesBlock.DECAY_DISTANCE`). This is why every
/// configured leaves state's own JSON-literal `distance` (always `7`, the
/// "fresh, undecayed" default) is not what real vanilla ever actually
/// serves near a trunk — before this function existed, this engine placed
/// every leaf at the JSON's literal `distance=7` and never corrected it, a
/// real, measured mismatch found by issue #428's savanna oracle fixtures
/// (real oak/acacia canopies are NOT reachable at plains' ~5%-per-chunk tree
/// rate with the two originally-committed fixtures, which is why this
/// gap was invisible until now — see this module's own parity test's doc
/// comment "A real bug in the oracle itself" for the reason trees were
/// never actually exercised before).
///
/// **Not a literal line-for-line port of the bucket/queue mechanics** — the
/// real Java keeps `toCheck`'s buckets as `Set`s and only guards re-adding
/// a position via a separately-tracked `DiscreteVoxelShape` "filled" bitset
/// checked at *dequeue* time (`shape.fill`)/*enqueue* time
/// (`shape.isFull`). A first attempt at translating that literally with
/// per-bucket `VecDeque`s and no cross-bucket dedup **hung indefinitely**:
/// a log's neighbour (a leaf) enqueues the log's own position back into
/// bucket 0 every time it is visited (the log always answers distance `0`,
/// so `min(smallest+1, 0)` is always `0`), and with no de-duplication nothing
/// ever stops that log from being re-popped and re-expanding the exact same
/// leaf forever. This function instead tracks one `visited: HashSet` and
/// marks a position the moment it is *enqueued* (not when it is later
/// popped) — a standard, well-known equivalent formulation of a uniform
/// (all-edge-weight-`1`) multi-source BFS via a bucket queue: the first
/// discovery of any position, under a discipline that always drains the
/// current-nearest bucket completely before advancing, **is** its true
/// shortest distance, so marking on first discovery cannot produce a
/// different final value than marking on completion — it only prevents the
/// redundant re-enqueues that made the literal port hang. This changes
/// nothing about *which* `distance` value ultimately gets written to any
/// cell, only how many times an already-settled cell gets looked at again.
/// No RNG is consumed anywhere in this function (a pure grid post-process),
/// so none of this affects the decoration RNG stream either way.
///
/// `#minecraft:prevents_nearby_leaf_decay` is, in the real registry, defined
/// as exactly `["#minecraft:logs"]` (`prevents_nearby_leaf_decay.json`) —
/// not an approximation, so this reuses [`VegTags::logs`] rather than
/// resolving a second, redundant tag.
///
/// **`bbox` is load-bearing, not a perf bound.** Real
/// `TreeFeature.place`/`updateLeaves` scopes its own BFS to
/// `BoundingBox.encapsulatingPositions(trunks ∪ foliage ∪ decorations ∪
/// roots)` — the bounding box of THIS ONE TREE's own placed blocks, not the
/// whole world — and gates BOTH the write and the neighbour-expansion step
/// on `bounds.isInside(pos)`. A first version of this port had no such
/// bound at all (any in-grid position was fair game), and measured wrong
/// against real savanna oracle fixtures: it found a *closer* neighbouring
/// tree's log through gaps between two adjacent canopies, giving every
/// affected leaf a lower `distance` than vanilla's own bbox-scoped BFS ever
/// would (vanilla's version, confined to one tree's own extent, simply
/// cannot see a different tree's logs at all, no matter how close). `bbox`
/// is `(min_x, min_y, min_z, max_x, max_y, max_z)`, inclusive, computed by
/// the caller from exactly the positions this one [`place_tree`] call wrote
/// (see that function's own call site for how).
fn update_leaf_distances(
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_positions: &[BlockPos],
    bbox: (i32, i32, i32, i32, i32, i32),
) {
    const MAX_DISTANCE: i32 = 7;
    const NEIGHBOR_OFFSETS: [(i32, i32, i32); 6] =
        [(0, -1, 0), (0, 1, 0), (-1, 0, 0), (1, 0, 0), (0, 0, -1), (0, 0, 1)];
    let (min_x, min_y, min_z, max_x, max_y, max_z) = bbox;
    let inside = |x: i32, y: i32, z: i32| {
        (min_x..=max_x).contains(&x) && (min_y..=max_y).contains(&y) && (min_z..=max_z).contains(&z)
    };

    let mut buckets: Vec<std::collections::VecDeque<(i32, i32, i32)>> =
        (0..MAX_DISTANCE).map(|_| std::collections::VecDeque::new()).collect();
    let mut visited: HashSet<(i32, i32, i32)> = HashSet::new();
    // Every trunk position is, by construction, inside `bbox` (the caller
    // derives `bbox` to encapsulate them) — matching real vanilla, where
    // `bounds` is built FROM `trunks`, so a log is trivially always its own
    // bbox member. No `inside` check needed here.
    for p in trunk_positions {
        let key = (p.x, p.y, p.z);
        if visited.insert(key) {
            buckets[0].push_back(key);
        }
    }

    let mut smallest: i32 = 0;
    loop {
        loop {
            if smallest >= MAX_DISTANCE {
                return;
            }
            let Some((x, y, z)) = buckets[smallest as usize].pop_front() else {
                break;
            };
            if smallest != 0 {
                let state = grid.get(x, y, z).to_string();
                if let Some(new_state) = set_distance_property(&state, smallest) {
                    grid.set_if_in_bounds(x, y, z, new_state);
                }
            }
            for (dx, dy, dz) in NEIGHBOR_OFFSETS {
                let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                let neighbor_key = (nx, ny, nz);
                if visited.contains(&neighbor_key) {
                    continue;
                }
                // The real `bounds.isInside(neighborPos)` gate — see this
                // function's own doc comment on why this must be the
                // tree's own bbox, not the grid's whole footprint.
                if !inside(nx, ny, nz) {
                    continue;
                }
                let neighbor_state = grid.get(nx, ny, nz);
                let base = base_id(neighbor_state);
                let current_distance = if tags.logs.contains(base) {
                    Some(0)
                } else {
                    distance_property(neighbor_state)
                };
                if let Some(current_distance) = current_distance {
                    let new_distance = (smallest + 1).min(current_distance);
                    if new_distance < MAX_DISTANCE {
                        visited.insert(neighbor_key);
                        buckets[new_distance as usize].push_back(neighbor_key);
                        smallest = smallest.min(new_distance);
                    }
                }
            }
        }
        smallest += 1;
    }
}

/// `LeavesBlock.getOptionalDistanceAt`'s non-tag half:
/// `state.hasProperty(DISTANCE) ? OptionalInt.of(state.getValue(DISTANCE)) :
/// OptionalInt.empty()`. The `#prevents_nearby_leaf_decay` half is handled
/// by the caller ([`update_leaf_distances`]) via [`VegTags::logs`] directly.
fn distance_property(state: &str) -> Option<i32> {
    let idx = state.find("distance=")?;
    let start = idx + "distance=".len();
    let end = state[start..].find([',', ']']).map_or(state.len(), |o| start + o);
    state[start..end].parse().ok()
}

/// Rewrites an existing `distance=N` property in place, preserving every
/// other property and bracket — the same `replace_range` idiom
/// [`try_place_leaf`]'s waterlogged fix-up already uses for the identical
/// shape of edit. `None` if `state` has no `distance` property at all
/// (never actually called on such a state — [`update_leaf_distances`] only
/// calls this after confirming [`distance_property`] returned `Some` for
/// the same position — but returning `Option` rather than panicking keeps
/// this fn safe to call standalone, e.g. from a future caller or a test).
fn set_distance_property(state: &str, new_distance: i32) -> Option<String> {
    let idx = state.find("distance=")?;
    let start = idx + "distance=".len();
    let end = state[start..].find([',', ']']).map_or(state.len(), |o| start + o);
    let mut s = state.to_string();
    s.replace_range(start..end, &new_distance.to_string());
    Some(s)
}

/// `net.minecraft.world.level.levelgen.feature.foliageplacers.FoliagePlacer`
/// (the `Blob`/`Spruce`/`Pine`/`Acacia` subset — see module doc's "Named
/// per-branch gaps" for why `Pine` is here despite not being one of issue
/// #406's three named species; `Acacia` is issue #428's addition, paired
/// with [`TrunkPlacerCfg::Forking`]).
#[derive(Clone, Debug)]
pub enum FoliagePlacerCfg {
    Blob {
        height: i32,
        radius: IntProvider,
        offset: IntProvider,
    },
    Spruce {
        radius: IntProvider,
        offset: IntProvider,
        trunk_height: IntProvider,
    },
    Pine {
        height: IntProvider,
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `AcaciaFoliagePlacer` — acacia's real foliage (issue #428). Its
    /// `foliageHeight` override always returns the constant `0`, drawing no
    /// RNG at all (unlike `Blob`'s config-constant `height` field or
    /// `Pine`'s sampled one) — see [`Self::foliage_height`]'s own arm.
    Acacia {
        radius: IntProvider,
        offset: IntProvider,
    },
}

impl FoliagePlacerCfg {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        let radius = try_parse_int_provider(&v["radius"])?;
        let offset = try_parse_int_provider(&v["offset"])?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "blob_foliage_placer" => Some(FoliagePlacerCfg::Blob {
                height: v["height"].as_i64()? as i32,
                radius,
                offset,
            }),
            "spruce_foliage_placer" => Some(FoliagePlacerCfg::Spruce {
                radius,
                offset,
                trunk_height: try_parse_int_provider(&v["trunk_height"])?,
            }),
            "pine_foliage_placer" => Some(FoliagePlacerCfg::Pine {
                height: try_parse_int_provider(&v["height"])?,
                radius,
                offset,
            }),
            "acacia_foliage_placer" => Some(FoliagePlacerCfg::Acacia { radius, offset }),
            _ => None,
        }
    }

    fn foliage_height<R: RandomSource>(&self, random: &mut R, tree_height: i32) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { height, .. } => *height,
            FoliagePlacerCfg::Spruce { trunk_height, .. } => {
                (tree_height - trunk_height.sample(random)).max(4)
            }
            FoliagePlacerCfg::Pine { height, .. } => height.sample(random),
            // `AcaciaFoliagePlacer.foliageHeight` ignores every one of its
            // own arguments and returns the constant `0` — no RNG draw.
            FoliagePlacerCfg::Acacia { .. } => 0,
        }
    }

    fn foliage_radius<R: RandomSource>(&self, random: &mut R, trunk_len: i32) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { radius, .. }
            | FoliagePlacerCfg::Spruce { radius, .. }
            | FoliagePlacerCfg::Acacia { radius, .. } => radius.sample(random),
            FoliagePlacerCfg::Pine { radius, .. } => {
                radius.sample(random) + random.next_int_bounded(trunk_len.max(0) + 1)
            }
        }
    }

    fn sample_offset<R: RandomSource>(&self, random: &mut R) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { offset, .. }
            | FoliagePlacerCfg::Spruce { offset, .. }
            | FoliagePlacerCfg::Pine { offset, .. }
            | FoliagePlacerCfg::Acacia { offset, .. } => offset.sample(random),
        }
    }

    /// `FoliagePlacer.shouldSkipLocation`/`shouldSkipLocationSigned`.
    /// `double_trunk` is always `false` for every trunk placer this module
    /// implements (`Straight`, `Forking`), so the signed wrapper's default
    /// implementation (`shouldSkipLocationSigned`, not overridden by
    /// `AcaciaFoliagePlacer`) reduces to calling `shouldSkipLocation` with
    /// `dx.abs()`/`dz.abs()` — exactly what every call site already passes,
    /// so this stays a plain (non-signed) check rather than adding a second
    /// entry point that only `DarkOakFoliagePlacer` (not implemented) would
    /// ever need to override.
    fn should_skip_location<R: RandomSource>(
        &self,
        random: &mut R,
        dx: i32,
        y: i32,
        dz: i32,
        current_radius: i32,
    ) -> bool {
        match self {
            FoliagePlacerCfg::Blob { .. } => {
                if dx == current_radius && dz == current_radius {
                    // Always drawn when at the corner, regardless of `y` —
                    // `nextInt(2)` is the *left* operand of `||`, and Java
                    // never short-circuits away from evaluating it first.
                    let coin = random.next_int_bounded(2) == 0;
                    coin || y == 0
                } else {
                    false
                }
            }
            FoliagePlacerCfg::Spruce { .. } | FoliagePlacerCfg::Pine { .. } => {
                dx == current_radius && dz == current_radius && current_radius > 0
            }
            // `AcaciaFoliagePlacer.shouldSkipLocation` — pure geometry, no
            // RNG draw (unlike `Blob`'s corner coin flip above).
            FoliagePlacerCfg::Acacia { .. } => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == current_radius && dz == current_radius && current_radius > 0
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn create_foliage<R: RandomSource>(
        &self,
        random: &mut R,
        attachment: BlockPos,
        foliage_height: i32,
        leaf_radius: i32,
        offset: i32,
        radius_offset: i32,
        grid: &mut VegGrid,
        tags: &VegTags,
        provider: &BlockStateProvider,
        placed_any: &mut bool,
    ) {
        match self {
            FoliagePlacerCfg::Blob { .. } => {
                // `yo / 2` in both Java and Rust truncates toward zero, so
                // no special-casing is needed for negative `yo` here.
                for yo in (offset - foliage_height..=offset).rev() {
                    let radius = (leaf_radius - 1 - yo / 2).max(0);
                    place_leaves_row(
                        random, attachment, radius, yo, grid, tags, self, provider, placed_any,
                    );
                }
            }
            FoliagePlacerCfg::Spruce { .. } => {
                let mut current_radius = random.next_int_bounded(2);
                let mut max_radius = 1;
                let mut min_radius = 0;
                let mut yo = offset;
                while yo >= -foliage_height {
                    place_leaves_row(
                        random,
                        attachment,
                        current_radius,
                        yo,
                        grid,
                        tags,
                        self,
                        provider,
                        placed_any,
                    );
                    if current_radius >= max_radius {
                        current_radius = min_radius;
                        min_radius = 1;
                        max_radius = (max_radius + 1).min(leaf_radius);
                    } else {
                        current_radius += 1;
                    }
                    yo -= 1;
                }
            }
            FoliagePlacerCfg::Pine { .. } => {
                let mut current_radius = 0;
                let lower = offset - foliage_height;
                let mut yo = offset;
                while yo >= lower {
                    place_leaves_row(
                        random,
                        attachment,
                        current_radius,
                        yo,
                        grid,
                        tags,
                        self,
                        provider,
                        placed_any,
                    );
                    if current_radius >= 1 && yo == lower + 1 {
                        current_radius -= 1;
                    } else if current_radius < leaf_radius {
                        current_radius += 1;
                    }
                    yo -= 1;
                }
            }
            // `AcaciaFoliagePlacer.createFoliage` — exactly three explicit
            // `placeLeavesRow` calls (not a scanning loop like `Blob`/
            // `Spruce`/`Pine` above), at `y = -1 - foliageHeight`,
            // `-foliageHeight`, `0` — with `foliageHeight` always `0` (see
            // `Self::foliage_height`), that's `y = -1, 0, 0`. The two `y =
            // 0` rows use DIFFERENT radii (`leaf_radius - 1` then
            // `leaf_radius + radius_offset - 1`) and are both real, in that
            // exact order — the second simply overwrites part of what the
            // first already wrote, matching vanilla's own redundancy rather
            // than an engine bug.
            FoliagePlacerCfg::Acacia { .. } => {
                place_leaves_row(
                    random, attachment, leaf_radius + radius_offset, -1 - foliage_height, grid, tags, self, provider, placed_any,
                );
                place_leaves_row(random, attachment, leaf_radius - 1, -foliage_height, grid, tags, self, provider, placed_any);
                place_leaves_row(
                    random, attachment, leaf_radius + radius_offset - 1, 0, grid, tags, self, provider, placed_any,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn place_leaves_row<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    current_radius: i32,
    y: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    placer: &FoliagePlacerCfg,
    provider: &BlockStateProvider,
    placed_any: &mut bool,
) {
    for dx in -current_radius..=current_radius {
        for dz in -current_radius..=current_radius {
            if !placer.should_skip_location(random, dx.abs(), y, dz.abs(), current_radius) {
                let pos = BlockPos {
                    x: origin.x + dx,
                    y: origin.y + y,
                    z: origin.z + dz,
                };
                try_place_leaf(random, pos, grid, tags, provider, placed_any);
            }
        }
    }
}

fn try_place_leaf<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    grid: &mut VegGrid,
    tags: &VegTags,
    provider: &BlockStateProvider,
    placed_any: &mut bool,
) {
    let base = base_id(grid.get(pos.x, pos.y, pos.z));
    // `!isPersistent && validTreePos`: nothing this engine ever places
    // during worldgen carries `persistent=true` (only a player placing a
    // leaf block by hand can set it), so the persistence half of the check
    // is unconditionally true here — not modelled as a separate branch.
    let valid = is_air(base) || tags.replaceable_by_trees.contains(base);
    if !valid {
        return;
    }
    let Some(mut state) = provider.get_state(grid, tags, random, pos) else {
        return;
    };
    if let Some(idx) = state.find("waterlogged=") {
        let is_water_source = base == "minecraft:water";
        let start = idx + "waterlogged=".len();
        let end = state[start..]
            .find([',', ']'])
            .map_or(state.len(), |o| start + o);
        state.replace_range(start..end, if is_water_source { "true" } else { "false" });
    }
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
    *placed_any = true;
}

/// `net.minecraft.world.level.levelgen.feature.featuresize.TwoLayersFeatureSize`
/// — the only `FeatureSize` this module implements (and, per the decompiled
/// source, the only one vanilla itself ships).
#[derive(Clone, Copy, Debug)]
pub struct FeatureSizeCfg {
    limit: i32,
    lower_size: i32,
    upper_size: i32,
}

impl FeatureSizeCfg {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        if ty.strip_prefix("minecraft:").unwrap_or(ty) != "two_layers_feature_size" {
            return None;
        }
        Some(Self {
            limit: v.get("limit").and_then(Value::as_i64).unwrap_or(1) as i32,
            lower_size: v.get("lower_size").and_then(Value::as_i64).unwrap_or(0) as i32,
            upper_size: v.get("upper_size").and_then(Value::as_i64).unwrap_or(1) as i32,
        })
    }

    fn size_at_height(&self, y: i32) -> i32 {
        if y < self.limit {
            self.lower_size
        } else {
            self.upper_size
        }
    }
}

/// `net.minecraft.world.level.levelgen.feature.configurations.TreeConfiguration`.
#[derive(Clone, Debug)]
pub struct TreeConfig {
    below_trunk_provider: Option<BlockStateProvider>,
    trunk_provider: BlockStateProvider,
    foliage_provider: BlockStateProvider,
    trunk_placer: TrunkPlacerCfg,
    foliage_placer: FoliagePlacerCfg,
    feature_size: FeatureSizeCfg,
    decorators: Vec<Decorator>,
}

impl TreeConfig {
    /// `None` if any required sub-part (trunk placer, foliage placer,
    /// feature size, trunk/foliage provider) is a kind this module doesn't
    /// implement — see module doc on why that must degrade rather than
    /// panic. `below_trunk_provider`/`decorators` degrade individually
    /// instead (a missing/unsupported one just does less, it doesn't sink
    /// the whole tree).
    fn try_parse(cfg: &Value) -> Option<Self> {
        let trunk_provider = BlockStateProvider::try_parse(&cfg["trunk_provider"])?;
        let foliage_provider = BlockStateProvider::try_parse(&cfg["foliage_provider"])?;
        let trunk_placer = TrunkPlacerCfg::try_parse(&cfg["trunk_placer"])?;
        let foliage_placer = FoliagePlacerCfg::try_parse(&cfg["foliage_placer"])?;
        let feature_size = FeatureSizeCfg::try_parse(&cfg["minimum_size"])?;
        let below_trunk_provider = cfg
            .get("below_trunk_provider")
            .and_then(BlockStateProvider::try_parse);
        let decorators = cfg
            .get("decorators")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(Decorator::parse).collect())
            .unwrap_or_default();
        Some(Self {
            below_trunk_provider,
            trunk_provider,
            foliage_provider,
            trunk_placer,
            foliage_placer,
            feature_size,
            decorators,
        })
    }
}

/// `net.minecraft.world.level.levelgen.feature.configurations.BlockColumnConfiguration`
/// + `BlockColumnFeature` — issue #406's cacti/sugar-cane increment. Used by
/// `cactus` (desert) and `sugar_cane` (desert/swamp/badlands/beach), both
/// previously a silent no-op under [`ConfiguredFeature::Unsupported`].
/// `direction` is `(dx, dy, dz)`; only `up`/`down` parse (every configured
/// feature in this crate's embedded data uses one of those two — see
/// [`BlockColumnConfig::try_parse`]'s doc), matching this module's blanket
/// "unsupported degrades, never panics" rule for anything else.
#[derive(Clone, Debug)]
pub struct BlockColumnConfig {
    layers: Vec<(IntProvider, BlockStateProvider)>,
    direction: (i32, i32, i32),
    allowed_placement: BlockPredicate,
    prioritize_tip: bool,
}

impl BlockColumnConfig {
    /// `direction` is a JSON string (`"up"`/`"down"`/four horizontal names);
    /// only the two vertical directions parse — the only two any
    /// `block_column` configured feature in `crates/lodestone-server/assets/worldgen`
    /// actually uses (`cactus.json`, `sugar_cane.json`, `cave_vine*.json`,
    /// `dripleaf.json`), checked at the time this was written. A horizontal
    /// direction degrades the whole feature to [`ConfiguredFeature::Unsupported`]
    /// rather than guessing.
    fn try_parse(v: &Value) -> Option<Self> {
        let layers = v["layers"]
            .as_array()?
            .iter()
            .map(|l| {
                let height = try_parse_int_provider(&l["height"])?;
                let provider = BlockStateProvider::try_parse(&l["provider"])?;
                Some((height, provider))
            })
            .collect::<Option<Vec<_>>>()?;
        let direction = match v["direction"].as_str()? {
            "up" => (0, 1, 0),
            "down" => (0, -1, 0),
            _ => return None,
        };
        Some(Self {
            layers,
            direction,
            allowed_placement: BlockPredicate::parse(&v["allowed_placement"]),
            prioritize_tip: v["prioritize_tip"].as_bool().unwrap_or(false),
        })
    }
}

/// `net.minecraft.world.level.levelgen.feature.ConfiguredFeature` (the
/// subset reached from grass/flower/tree biome steps). [`Unsupported`]
/// carries the vanilla type string purely for diagnostics — placing it is
/// always a no-op.
#[derive(Clone, Debug)]
pub enum ConfiguredFeature {
    SimpleBlock(BlockStateProvider),
    Tree(Box<TreeConfig>),
    BlockColumn(Box<BlockColumnConfig>),
    RandomSelector {
        default: Box<PlacedRef>,
        options: Vec<(f32, PlacedRef)>,
    },
    SimpleRandomSelector(Vec<PlacedRef>),
    Unsupported(String),
}

/// `net.minecraft.world.level.levelgen.placement.PlacedFeature` — an ordered
/// [`VegPlacement`] pipeline plus the [`ConfiguredFeature`] it terminates in.
/// Every reference to a placed feature (top-level biome step entry, or a
/// nested option inside a selector) resolves to one of these — vanilla's
/// `PlacedFeature.place` runs its *own* placement pipeline even when reached
/// as a selector's branch, and [`place_placed_feature`] reproduces that
/// uniformly rather than special-casing "top level" vs "nested".
#[derive(Clone, Debug)]
pub struct PlacedRef {
    pub placements: Vec<VegPlacement>,
    pub feature: Box<ConfiguredFeature>,
}

fn unsupported_placed_ref(why: &str) -> PlacedRef {
    PlacedRef {
        placements: Vec::new(),
        feature: Box::new(ConfiguredFeature::Unsupported(why.to_string())),
    }
}

/// Resolves a `Holder<PlacedFeature>`-shaped JSON value — either a plain
/// string (a `placed_feature` registry id) or an inline `{feature,
/// placement}` object — into a [`PlacedRef`]. Never panics: any parse
/// failure anywhere in the (possibly deeply nested, selector-within-
/// selector) tree degrades the *innermost* failing node to
/// [`ConfiguredFeature::Unsupported`], per this module's blanket
/// "degrade, don't crash" rule.
#[must_use]
pub fn resolve_placed_feature_ref(resolver: &dyn Resolver, value: &Value) -> PlacedRef {
    match value {
        Value::String(id) => {
            let doc = resolver.placed_feature(id);
            if doc.is_null() {
                return unsupported_placed_ref("missing placed_feature data");
            }
            parse_placed_feature_doc(resolver, &doc)
        }
        Value::Object(_) => parse_placed_feature_doc(resolver, value),
        _ => unsupported_placed_ref("unexpected placed-feature ref shape"),
    }
}

fn parse_placed_feature_doc(resolver: &dyn Resolver, doc: &Value) -> PlacedRef {
    let placements = doc
        .get("placement")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(VegPlacement::try_parse).collect())
        .unwrap_or_default();
    let Some(feature_ref) = doc.get("feature") else {
        return unsupported_placed_ref("placed-feature doc missing 'feature'");
    };
    let feature = resolve_configured_feature_ref(resolver, feature_ref);
    PlacedRef {
        placements,
        feature: Box::new(feature),
    }
}

/// Resolves a `Holder<ConfiguredFeature>`-shaped JSON value the same way
/// [`resolve_placed_feature_ref`] resolves a placed-feature one.
#[must_use]
pub fn resolve_configured_feature_ref(resolver: &dyn Resolver, value: &Value) -> ConfiguredFeature {
    match value {
        Value::String(id) => {
            let doc = resolver.configured_feature(id);
            if doc.is_null() {
                return ConfiguredFeature::Unsupported("missing configured_feature data".into());
            }
            parse_configured_feature_doc(resolver, &doc)
        }
        Value::Object(_) => parse_configured_feature_doc(resolver, value),
        _ => ConfiguredFeature::Unsupported("unexpected configured-feature ref shape".into()),
    }
}

fn parse_configured_feature_doc(resolver: &dyn Resolver, doc: &Value) -> ConfiguredFeature {
    let ty = doc["type"].as_str().unwrap_or("");
    let short = ty.strip_prefix("minecraft:").unwrap_or(ty);
    match short {
        "simple_block" => match BlockStateProvider::try_parse(&doc["config"]["to_place"]) {
            Some(p) => ConfiguredFeature::SimpleBlock(p),
            None => ConfiguredFeature::Unsupported("simple_block: unsupported to_place".into()),
        },
        "tree" => match TreeConfig::try_parse(&doc["config"]) {
            Some(cfg) => ConfiguredFeature::Tree(Box::new(cfg)),
            None => ConfiguredFeature::Unsupported(
                "tree: unsupported trunk/foliage/size/provider".into(),
            ),
        },
        "block_column" => match BlockColumnConfig::try_parse(&doc["config"]) {
            Some(cfg) => ConfiguredFeature::BlockColumn(Box::new(cfg)),
            None => ConfiguredFeature::Unsupported(
                "block_column: unsupported layer/direction/predicate".into(),
            ),
        },
        "random_selector" => {
            let cfg = &doc["config"];
            let default = resolve_placed_feature_ref(resolver, &cfg["default"]);
            let options = cfg["features"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| {
                            let chance = e["chance"].as_f64().unwrap_or(0.0) as f32;
                            (chance, resolve_placed_feature_ref(resolver, &e["feature"]))
                        })
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::RandomSelector {
                default: Box::new(default),
                options,
            }
        }
        "simple_random_selector" => {
            let list = doc["config"]["features"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| resolve_placed_feature_ref(resolver, e))
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::SimpleRandomSelector(list)
        }
        other => ConfiguredFeature::Unsupported(other.to_string()),
    }
}

/// Walks a resolved vegetal-decoration tree — through `RandomSelector`'s
/// `default`/`options` and `SimpleRandomSelector`'s list, the only two ways
/// this module's own [`ConfiguredFeature`] nests — collecting every
/// [`ConfiguredFeature::Unsupported`] reason string actually reachable from
/// `placed`. This is the read side of this module's "unsupported degrades to
/// a silent no-op" rule: a caller that wants that silence to be **loud**
/// (issue #406's "does this biome's declared vegetation include a placer we
/// don't implement" gate, in `lodestone_server::worldgen_data`) diffs this
/// against a maintained allow-list instead of trusting the resolved tree to
/// run and simply place fewer blocks than vanilla. Reasons are **not**
/// deduplicated here — the caller decides whether it wants a set or a count.
#[must_use]
pub fn collect_unsupported(placed: &PlacedRef) -> Vec<String> {
    fn walk(feature: &ConfiguredFeature, out: &mut Vec<String>) {
        match feature {
            ConfiguredFeature::Unsupported(reason) => out.push(reason.clone()),
            ConfiguredFeature::RandomSelector { default, options } => {
                walk(&default.feature, out);
                for (_, opt) in options {
                    walk(&opt.feature, out);
                }
            }
            ConfiguredFeature::SimpleRandomSelector(list) => {
                for opt in list {
                    walk(&opt.feature, out);
                }
            }
            ConfiguredFeature::SimpleBlock(_) | ConfiguredFeature::Tree(_) | ConfiguredFeature::BlockColumn(_) => {}
        }
    }
    let mut out = Vec::new();
    walk(&placed.feature, &mut out);
    out
}

/// The mutable block field vegetal decoration reads and writes. Defaults to
/// chunk-local (`0..16` × `0..16`, absolute `y`) via [`VegGrid::new`] — see
/// module doc's "Scope" section for why single-chunk was this module's
/// original footprint. [`VegGrid::with_footprint`] widens the local bound to
/// an arbitrary `[lo, hi)` on both axes (issue #427: the real vanilla 3×3
/// `blockStateWriteRadius(1)` driver uses [`crate::feature::REGION_MIN`]/
/// [`crate::feature::REGION_MAX`], the exact constants
/// [`crate::feature::OreInput::region_local`] already established for the
/// ore engine's own 3×3 driver — reused here rather than duplicated, per
/// CLAUDE.md's instruction to follow that precedent) with `origin_x`/
/// `origin_z` fixed at the **centre** chunk's own absolute origin, so every
/// one of the 9 sources' absolute-coordinate writes translates through the
/// same origin and lands (or is dropped) relative to the centre — exactly
/// [`crate::feature::OreInput`]'s `chunk_x`/`chunk_z` (varies per source) vs
/// `center_x`/`center_z` (fixed) split, applied to this module's own grid
/// type instead of introducing a second region-grid mechanism.
#[derive(Debug)]
pub struct VegGrid {
    /// Keyed by **local** `(0..16, y, 0..16)` — every public accessor takes
    /// **absolute world** coordinates (matching every `BlockPos` this
    /// engine's placement modifiers compute — noise sampling, the decoration
    /// seed, `RandomOffset`'s scatter, all of it is absolute-coordinate
    /// arithmetic, not local) and converts via `origin_x`/`origin_z`
    /// internally. Getting this translation wrong is exactly the bug this
    /// comment exists to prevent from recurring: an earlier version of this
    /// struct stored *and exposed* local coordinates, silently accepting the
    /// engine's absolute-coordinate `BlockPos`es and comparing them against
    /// a `0..16` bound that was almost always false — every placement
    /// attempt for any chunk other than `(0, 0)` failed `in_bounds`/`get`'s
    /// implicit "must already be local" assumption, so vegetation composed,
    /// ran, and reached zero blocks in every real served chunk. Caught by a
    /// sweep gate measuring **zero** grass/flowers/logs/leaves over a plains
    /// neighbourhood — this module's own hermetic unit tests never caught it
    /// because every one of them happened to place at `origin = BlockPos { x: 8,
    /// ... z: 8 }`, which is coincidentally already "local" (chunk (0,0)'s own
    /// footprint), the exact island CLAUDE.md's rule 1 describes: a unit
    /// test can be green while the real integration seam is broken.
    ///
    /// **That gate was then deleted, and this comment kept naming it** — it read
    /// `lodestone_server::worldgen_data::tests::diagnostic_vegetation_counts_over_plains_sweep`
    /// up to `074b5e9`, by which point no such test existed anywhere in the tree
    /// (issue #478). So for an unknown span the repo held a written record of a
    /// regression with nothing watching for its return, and the reference read as
    /// coverage on inspection. The live gate is now
    /// `lodestone_server::worldgen_data::tests::vegetation_reaches_real_blocks_over_a_production_sweep`,
    /// with `plains_grass_patch_attempt_count_matches_the_placement_json` carrying
    /// the predicted magnitude — but treat *this sentence* as a claim like any
    /// other and grep for the name before trusting it.
    blocks: HashMap<(i32, i32, i32), String>,
    /// Positions actually written by `set_if_in_bounds`, **local** (see
    /// `blocks`' doc), in write order — a `Vec`, not a re-iterated
    /// `HashMap`, specifically so a caller folding this back into a dense
    /// grid (`OverworldGenerator`'s vegetation stage) has a *deterministic*
    /// order to replay, the same discipline `docs/worldgen-parity.md`'s
    /// "Performance" section describes fixing for `surface_diff` (point
    /// lookups inside a fixed loop, never a raw `HashMap` iteration) — here
    /// achieved even more directly, since insertion order into a `Vec`
    /// carries no ambiguity to begin with. Lets the fold-back touch only the
    /// (typically small) written subset instead of rewriting all
    /// `16 × height × 16` cells.
    dirty: Vec<(i32, i32, i32)>,
    origin_x: i32,
    origin_z: i32,
    min_y: i32,
    height: i32,
    /// Local-coordinate bound `[local_lo, local_hi)` on both `lx` and `lz` —
    /// `(0, 16)` for the single-chunk case ([`VegGrid::new`]), widened to
    /// [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`] for the
    /// 3×3 driver ([`VegGrid::with_footprint`],
    /// [`apply_vegetal_decoration_step_3x3_per_source`]).
    local_lo: i32,
    local_hi: i32,
}

impl VegGrid {
    /// `origin_x`/`origin_z` are the chunk's own **absolute** block origin
    /// (`chunk_x * 16`, `chunk_z * 16`) — every other method on this type
    /// takes absolute world coordinates and translates through these.
    /// Single-chunk footprint (`0..16` on both axes) — see
    /// [`VegGrid::with_footprint`] for the 3×3 driver's widened case.
    #[must_use]
    pub fn new(min_y: i32, height: i32, origin_x: i32, origin_z: i32) -> Self {
        Self::with_footprint(min_y, height, origin_x, origin_z, 0, 16)
    }

    /// Like [`VegGrid::new`], but with an explicit local-coordinate bound
    /// `[local_lo, local_hi)` on both `lx` and `lz` instead of the hardcoded
    /// `0..16` — the real vanilla 3×3 `blockStateWriteRadius(1)` driver
    /// passes [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`]
    /// here with `origin_x`/`origin_z` fixed at the **centre** chunk's own
    /// origin (see this struct's own doc comment).
    #[must_use]
    pub fn with_footprint(min_y: i32, height: i32, origin_x: i32, origin_z: i32, local_lo: i32, local_hi: i32) -> Self {
        Self {
            blocks: HashMap::new(),
            dirty: Vec::new(),
            origin_x,
            origin_z,
            min_y,
            height,
            local_lo,
            local_hi,
        }
    }

    /// Positions written by `set_if_in_bounds` since construction, in write
    /// order, **as absolute world coordinates**, each paired with the state
    /// currently at that position (i.e. the *final* state if the same cell
    /// was written more than once, not an intermediate one) — what a caller
    /// should fold back into a wider grid, with no further translation
    /// needed.
    pub fn dirty_cells(&self) -> impl Iterator<Item = (i32, i32, i32, &str)> {
        self.dirty.iter().map(|&(lx, y, lz)| {
            (
                self.origin_x + lx,
                y,
                self.origin_z + lz,
                self.blocks.get(&(lx, y, lz)).map_or("minecraft:air", String::as_str),
            )
        })
    }

    /// The number of writes recorded so far — a caller (currently only
    /// [`place_tree`]) that brackets a `dirty_len()` call before and after a
    /// span of writes and then reads `dirty_cells().skip(before)` gets
    /// exactly the absolute-coordinate positions written in that span, in
    /// order. Used to compute one tree's own `trunks ∪ foliage ∪
    /// decorations` bounding box for [`update_leaf_distances`] — see that
    /// function's own doc comment for why the bound matters.
    #[must_use]
    pub fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    fn in_bounds_local(&self, lx: i32, lz: i32) -> bool {
        (self.local_lo..self.local_hi).contains(&lx) && (self.local_lo..self.local_hi).contains(&lz)
    }

    /// Absolute world `(x, z)` -> local `[local_lo, local_hi)`, **clamped**
    /// into range — used only by read paths, which must always answer
    /// something.
    fn to_local_clamped(&self, x: i32, z: i32) -> (i32, i32) {
        (
            (x - self.origin_x).clamp(self.local_lo, self.local_hi - 1),
            (z - self.origin_z).clamp(self.local_lo, self.local_hi - 1),
        )
    }

    /// Absolute world `(x, z)` -> local, **unclamped** — used only by the
    /// write path, which must know whether the position genuinely falls
    /// inside this chunk's own footprint rather than silently relocating a
    /// write to the nearest edge.
    fn to_local_exact(&self, x: i32, z: i32) -> (i32, i32) {
        (x - self.origin_x, z - self.origin_z)
    }

    /// Seeds one column position (absolute world coordinates) from the
    /// post-ore composed grid. Callers fill every `(x, y, z)` in this
    /// chunk's own `16 × height × 16` footprint before running vegetal
    /// decoration.
    pub fn seed(&mut self, x: i32, y: i32, z: i32, state: String) {
        let (lx, lz) = self.to_local_exact(x, z);
        self.blocks.insert((lx, y, lz), state);
    }

    fn get_local(&self, lx: i32, y: i32, lz: i32) -> &str {
        if y < self.min_y || y >= self.min_y + self.height {
            return "minecraft:air";
        }
        self.blocks
            .get(&(lx, y, lz))
            .map_or("minecraft:air", String::as_str)
    }

    /// Reads always succeed (clamped into bounds) — a read past the local
    /// footprint approximates the nearest in-bounds column rather than
    /// panicking or returning a sentinel the caller has to special-case.
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> &str {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.get_local(lx, y, lz)
    }

    /// Writes past the local footprint (`0..16` for [`VegGrid::new`], wider
    /// for [`VegGrid::with_footprint`]) or outside the vertical build range
    /// are dropped, not clamped — see module doc's "Scope" section; a write
    /// past whatever footprint this grid covers would fabricate a block on
    /// the wrong column. Returns whether the write actually landed.
    pub fn set_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: String) -> bool {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            census_bump(|c| c.writes += 1);
            self.blocks.insert((lx, y, lz), state);
            self.dirty.push((lx, y, lz));
            true
        } else {
            census_bump(|c| c.writes_rejected += 1);
            false
        }
    }

    /// `Heightmap.Types.WORLD_SURFACE`/`WORLD_SURFACE_WG` — topmost non-air,
    /// scanned live against the current (possibly already-modified-this-step)
    /// grid. `x`/`z` are absolute world coordinates. Returns `min_y` (not
    /// `min_y - 1`) for an all-air column, matching vanilla's `y + 1`
    /// convention with `y` floored at one below the lowest placeable block.
    #[must_use]
    pub fn height_world_surface(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        for y in (self.min_y..self.min_y + self.height).rev() {
            let base = base_id(self.get_local(lx, y, lz));
            if !is_air(base) {
                return y + 1;
            }
        }
        self.min_y
    }

    /// `Heightmap.Types.OCEAN_FLOOR`/`OCEAN_FLOOR_WG` — topmost non-air,
    /// non-fluid. `x`/`z` are absolute world coordinates.
    #[must_use]
    pub fn height_ocean_floor(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        for y in (self.min_y..self.min_y + self.height).rev() {
            let base = base_id(self.get_local(lx, y, lz));
            if !is_air(base) && !is_fluid(base) {
                return y + 1;
            }
        }
        self.min_y
    }
}

/// Runs the whole `VEGETAL_DECORATION` step for one chunk against its own
/// grid — single-source only, see module doc's "Scope" section.
/// `features` is `(raw step index, resolved PlacedRef)`, matching
/// [`super::compose::build_biome_ores`]'s "preserve raw position" convention
/// so `setFeatureSeed`'s index is the JSON array position, not a filtered
/// count.
/// Per-thread census of what the vegetal-decoration placer actually *did* —
/// issue #478's "make absence loud" half.
///
/// # Why this exists, and why the existing gate was not enough
///
/// This module's blanket rule is "an unmodelled feature/trunk/foliage/provider
/// kind degrades to a silent no-op, never a panic" (see the module doc). That
/// rule is right — a datapack naming a feature we don't implement must still
/// produce a world — but on its own it makes *every* quantity of vegetation,
/// including zero, look identical from the outside. Issue #478 was filed
/// against exactly that shape, and the previous instance of the same shape
/// (the absolute-vs-local `VegGrid` coordinate bug recorded in
/// [`VegGrid`]'s own doc comment) reached **zero** blocks in every served
/// chunk with the whole suite green.
///
/// [`collect_unsupported`] plus `lodestone_server::worldgen_data`'s
/// `KNOWN_VEGETATION_GAPS` already make absence loud at **resolve** time: they
/// answer "does this biome's declared step name a placer we don't implement?"
/// They structurally cannot answer "did the placer that *is* implemented reach
/// a block?", because they never run it. This census answers the second
/// question — the one that separates a fully-connected wire carrying real
/// blocks from a fully-connected wire carrying nothing.
///
/// # Thread-local, not global
///
/// `OverworldGenerator` is shared across threads by
/// `lodestone_server::chunk::generate_columns_parallel`, and `cargo test` runs
/// test binaries multi-threaded. A process-global counter would make any gate
/// built on it read another test's work, which is the *duration* species of
/// vacuous test (a counter accumulating past the gate's own lifetime). Each
/// thread sees only its own placements, so a gate resets, generates, and reads
/// back on one thread and measures exactly what it caused.
pub mod census {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Terminal-dispatch and write tallies for one thread's placements.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct VegCensus {
        /// [`super::ConfiguredFeature::SimpleBlock`] terminal dispatches — one
        /// per position that survived the whole placement pipeline.
        pub simple_block: usize,
        /// [`super::ConfiguredFeature::Tree`] terminal dispatches.
        pub tree: usize,
        /// [`super::ConfiguredFeature::BlockColumn`] terminal dispatches.
        pub block_column: usize,
        /// [`super::ConfiguredFeature::RandomSelector`] traversals (not
        /// terminals — each recurses into a branch).
        pub random_selector: usize,
        /// [`super::ConfiguredFeature::SimpleRandomSelector`] traversals.
        pub simple_random_selector: usize,
        /// Unmodelled terminal dispatches, **keyed by the reason string**
        /// [`super::ConfiguredFeature::Unsupported`] carries. This is the loud
        /// part: a new unimplemented feature type shows up here as a named,
        /// counted row instead of as a slightly emptier world.
        pub unsupported: BTreeMap<String, usize>,
        /// `SimpleBlock` dispatches dropped because the state provider
        /// produced nothing.
        pub simple_block_no_state: usize,
        /// `SimpleBlock` dispatches dropped because the block below is not in
        /// `#minecraft:supports_vegetation` (`VegetationBlock.canSurvive`).
        /// Legitimately the majority — `random_offset` scatters positions off
        /// the heightmap column — so this is a diagnostic, not a defect count.
        pub simple_block_unsupported_ground: usize,
        /// Positions handed to a [`super::VegPlacement::BlockPredicateFilter`].
        ///
        /// This is the **last exactly-predictable boundary** in a vanilla
        /// vegetal-decoration pipeline, and the reason it is counted separately
        /// from everything else here. Every 26.2 overworld vegetation
        /// `placed_feature` ends in at least one filter whose outcome depends on
        /// terrain (measured: of 262 bundled placed features, the only three
        /// with no filter at all are `end_spike`, `freeze_top_layer` and
        /// `void_start_platform`), so no *terminal* count can be predicted from
        /// the JSON alone. Everything upstream of the filter can:
        /// `count`/`noise_threshold_count` multiply by a JSON constant,
        /// `in_square`/`biome`/`random_offset` are each exactly
        /// position-preserving, and `heightmap` yields exactly one position for
        /// any column that is not entirely air. So for a single-source run of a
        /// single placed feature this number is a product of JSON constants —
        /// which is what lets a gate *predict* it instead of asserting a sign.
        /// See `lodestone_server::worldgen_data`'s
        /// `plains_grass_patch_attempt_count_matches_the_placement_json`.
        pub block_predicate_filter_in: usize,
        /// Positions that passed a [`super::VegPlacement::BlockPredicateFilter`].
        pub block_predicate_filter_out: usize,
        /// Grid writes that landed.
        pub writes: usize,
        /// Grid writes dropped as outside the grid's own footprint (spill into
        /// a chunk this grid does not cover — expected, see
        /// [`super::VegGrid::set_if_in_bounds`]).
        pub writes_rejected: usize,
    }

    impl VegCensus {
        /// Total unmodelled terminal dispatches across every reason.
        #[must_use]
        pub fn unsupported_total(&self) -> usize {
            self.unsupported.values().sum()
        }

        /// Terminal dispatches that reached a placer this engine implements.
        #[must_use]
        pub fn modelled_terminals(&self) -> usize {
            self.simple_block + self.tree + self.block_column
        }
    }

    thread_local! {
        static CENSUS: RefCell<VegCensus> = RefCell::new(VegCensus::default());
    }

    /// Whether an unmodelled terminal dispatch should panic instead of being
    /// counted — `LODESTONE_VEG_STRICT=1`. Read once per process.
    ///
    /// Off by default on purpose: the module's degrade-don't-crash rule is what
    /// lets a trimmed datapack generate at all, and 26.2's own vanilla data
    /// reaches unmodelled types in nearly every biome (`multiface_growth` alone
    /// is in 55 of them), so strict mode is a *debugging* switch for "which
    /// type am I missing here", not a mode anything ships in.
    #[must_use]
    pub fn strict() -> bool {
        static STRICT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *STRICT.get_or_init(|| {
            std::env::var("LODESTONE_VEG_STRICT").is_ok_and(|v| v != "0" && !v.is_empty())
        })
    }

    /// Zeroes this thread's census. Call immediately before the generation a
    /// gate intends to measure.
    pub fn reset() {
        CENSUS.with(|c| *c.borrow_mut() = VegCensus::default());
    }

    /// This thread's census so far.
    #[must_use]
    pub fn snapshot() -> VegCensus {
        CENSUS.with(|c| c.borrow().clone())
    }

    pub(super) fn bump(f: impl FnOnce(&mut VegCensus)) {
        CENSUS.with(|c| f(&mut c.borrow_mut()));
    }
}

use census::bump as census_bump;

pub fn apply_vegetal_decoration_step<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    chunk_x: i32,
    chunk_z: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    features: &[(usize, PlacedRef)],
) {
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
/// against one shared `grid`. `grid` must already be seeded with every one
/// of the 9 sources' own post-ore terrain (its footprint should span
/// [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`], built via
/// [`VegGrid::with_footprint`] with `origin_x`/`origin_z` fixed at
/// `(center_x * 16, center_z * 16)`) — this function does no stitching of
/// its own, mirroring [`crate::feature::apply_ore_step_3x3_per_source`]'s
/// own "caller stitches, driver only places" split.
///
/// Each source's own pass mutates `grid` **in place**, so a later source in
/// the fixed iteration order sees an earlier source's writes — this is a
/// real, intentional match to `VegetationOracle.java`'s `runStep`, which
/// mutates one shared, live `WorldGenLevel` across all 9 sources in the same
/// order, not 9 independent snapshots merged afterward. See that oracle's
/// own doc comment on `runStep` for the vanilla behaviour this reproduces.
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
        for next in mods[i].get_positions(random, pos, grid, tags) {
            recurse(random, mods, i + 1, next, grid, tags, feature);
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
            census_bump(|c| *c.unsupported.entry(reason.clone()).or_default() += 1);
        }
    }
}

fn place_simple_block<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let Some(state) = provider.get_state(grid, tags, random, pos) else {
        census_bump(|c| c.simple_block_no_state += 1);
        return;
    };
    // `VegetationBlock.canSurvive`: the block below must support vegetation
    // — see module doc on why this is applied uniformly.
    let below = base_id(grid.get(pos.x, pos.y - 1, pos.z));
    if !tags.supports_vegetation.contains(below) {
        census_bump(|c| c.simple_block_unsupported_ground += 1);
        return;
    }
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
}

/// `BlockColumnFeature.place`: samples every layer's height up front (so the
/// RNG draw order is fixed regardless of how far the column actually
/// reaches), then walks `direction` from `origin` checking `allowed_placement`
/// at each *next* position (`origin` itself is never checked — only used as
/// the first placement slot) for up to the sampled total height, truncating
/// via [`truncate_layers`] the moment a check fails, then places each layer's
/// blocks in declared order.
fn place_block_column<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &BlockColumnConfig,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let mut layer_heights: Vec<i32> = cfg.layers.iter().map(|(h, _)| h.sample(random)).collect();
    let total_height: i32 = layer_heights.iter().sum();
    if total_height == 0 {
        return;
    }
    let (dx, dy, dz) = cfg.direction;
    let mut probe = BlockPos {
        x: origin.x + dx,
        y: origin.y + dy,
        z: origin.z + dz,
    };
    let mut new_height = total_height;
    for y in 0..total_height {
        if !cfg.allowed_placement.test(grid, tags, probe) {
            new_height = y;
            break;
        }
        probe = BlockPos {
            x: probe.x + dx,
            y: probe.y + dy,
            z: probe.z + dz,
        };
    }
    if new_height < total_height {
        truncate_layers(&mut layer_heights, total_height, new_height, cfg.prioritize_tip);
    }
    let mut place_pos = origin;
    for (i, (_, provider)) in cfg.layers.iter().enumerate() {
        for _ in 0..layer_heights[i] {
            if let Some(state) = provider.get_state(grid, tags, random, place_pos) {
                grid.set_if_in_bounds(place_pos.x, place_pos.y, place_pos.z, state);
            }
            place_pos = BlockPos {
                x: place_pos.x + dx,
                y: place_pos.y + dy,
                z: place_pos.z + dz,
            };
        }
    }
}

/// `BlockColumnFeature.truncate`: removes `total_height - new_height` blocks
/// total, walking layers tip-first (`prioritize_tip`) or base-first
/// (everything else) — matching vanilla's own iteration-order choice exactly.
fn truncate_layers(layer_heights: &mut [i32], total_height: i32, new_height: i32, prioritize_tip: bool) {
    let mut to_remove = total_height - new_height;
    let n = layer_heights.len();
    let indices: Vec<usize> = if prioritize_tip {
        (0..n).collect()
    } else {
        (0..n).rev().collect()
    };
    for i in indices {
        if to_remove <= 0 {
            break;
        }
        let this_layer = layer_heights[i];
        let removed = this_layer.min(to_remove);
        to_remove -= removed;
        layer_heights[i] -= removed;
    }
}

fn place_tree<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &TreeConfig,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let tree_height = cfg.trunk_placer.get_tree_height(random);
    let foliage_height = cfg.foliage_placer.foliage_height(random, tree_height);
    let trunk_len = tree_height - foliage_height;
    let leaf_radius = cfg.foliage_placer.foliage_radius(random, trunk_len);

    // `rootPlacer` is always absent for every species this module
    // implements, so `trunkOrigin == origin`: `minY == origin.y`,
    // `maxY == origin.y + treeHeight + 1`.
    if origin.y < grid.min_y + 1 || origin.y + tree_height + 1 > grid.min_y + grid.height + 1 {
        return;
    }

    // `getMaxFreeTreeHeight`: scan the tree's own footprint for anything
    // that isn't air/replaceable-by-trees/a log (a log counts as "free" —
    // `TrunkPlacer.isFree` — so an already-placed neighbour trunk doesn't
    // block this one). `ignore_vines` is `true` for every species here, so
    // the vine half of vanilla's check never applies.
    let mut clipped = tree_height;
    'scan: for y in 0..=tree_height + 1 {
        let r = cfg.feature_size.size_at_height(y);
        for dx in -r..=r {
            for dz in -r..=r {
                let base = base_id(grid.get(origin.x + dx, origin.y + y, origin.z + dz));
                let free = is_air(base)
                    || tags.replaceable_by_trees.contains(base)
                    || tags.logs.contains(base);
                if !free {
                    clipped = y - 2;
                    break 'scan;
                }
            }
        }
    }
    if clipped < tree_height {
        return;
    }

    // Marks where this tree's own writes begin, so `update_leaf_distances`
    // can later derive its bbox from exactly this tree's own `trunks ∪
    // foliage ∪ decorations` — see that function's own doc comment on why
    // the bbox must be this narrow (real vanilla's `updateLeaves` is scoped
    // the same way, to one tree at a time, not the whole grid).
    let dirty_start = grid.dirty_len();

    // Dispatch trunk placement by placer kind — `Straight`'s own
    // `placeBelowTrunkBlock` + single-column loop stayed inline here (this
    // module's original #406 shape, unchanged); `Forking` (issue #428)
    // delegates to `place_forking_trunk`, which does its own
    // `placeBelowTrunkBlock` call internally, matching `ForkingTrunkPlacer
    // .placeTrunk`'s own real structure. Both branches produce the same
    // `(Vec<Attachment>, Vec<BlockPos>, placed_log)` shape — the third being
    // every position `trunkSetter` actually fired at (issue #428's
    // `update_leaf_distances` BFS seed, see that function's doc comment) —
    // so the foliage loop below is written once, not once per trunk kind.
    let (attachments, trunk_positions, placed_log) = match &cfg.trunk_placer {
        TrunkPlacerCfg::Straight { .. } => {
            let mut trunk_positions = Vec::new();
            if let Some(below_provider) = &cfg.below_trunk_provider {
                let below_pos = BlockPos {
                    x: origin.x,
                    y: origin.y - 1,
                    z: origin.z,
                };
                if let Some(state) = below_provider.get_state(grid, tags, random, below_pos) {
                    grid.set_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
                    trunk_positions.push(below_pos);
                }
            }
            let mut placed_log = false;
            for y in 0..tree_height {
                let pos = BlockPos {
                    x: origin.x,
                    y: origin.y + y,
                    z: origin.z,
                };
                let base = base_id(grid.get(pos.x, pos.y, pos.z));
                if is_air(base) || tags.replaceable_by_trees.contains(base) {
                    if let Some(state) = cfg.trunk_provider.get_state(grid, tags, random, pos) {
                        grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
                        placed_log = true;
                        trunk_positions.push(pos);
                    }
                }
            }
            let attachment = Attachment {
                pos: BlockPos {
                    x: origin.x,
                    y: origin.y + tree_height,
                    z: origin.z,
                },
                radius_offset: 0,
                double_trunk: false,
            };
            (vec![attachment], trunk_positions, placed_log)
        }
        TrunkPlacerCfg::Forking { .. } => place_forking_trunk(
            random,
            origin,
            tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
        ),
    };

    // `foliageAttachments.forEach(a -> foliagePlacer.createFoliage(...))` —
    // the public per-attachment overload draws `this.offset(random)` FRESH
    // for EACH attachment (not once overall), so the fresh
    // `sample_offset` call must live INSIDE this loop. For `Straight`
    // (always exactly one attachment) this is behaviourally identical to
    // the pre-#428 single call it replaces — no draw-count change for
    // oak/birch/spruce/pine.
    let mut placed_leaf = false;
    for attachment in &attachments {
        let offset = cfg.foliage_placer.sample_offset(random);
        cfg.foliage_placer.create_foliage(
            random,
            attachment.pos,
            foliage_height,
            leaf_radius,
            offset,
            attachment.radius_offset,
            grid,
            tags,
            &cfg.foliage_provider,
            &mut placed_leaf,
        );
    }

    if !placed_log && !placed_leaf {
        return;
    }

    for decorator in &cfg.decorators {
        match decorator {
            Decorator::Beehive { probability } => {
                place_beehive_decorator(random, *probability, origin, tree_height, grid);
            }
            Decorator::Unsupported => {}
        }
    }

    // `TreeFeature.place`'s own final step, AFTER decorators — issue #428's
    // fix for the `distance=7`-forever gap named in
    // `update_leaf_distances`'s own doc comment. Draws no RNG (a pure grid
    // post-process), so it is safe to run unconditionally here regardless
    // of which branch above produced `trunk_positions`. The bbox is exactly
    // `BoundingBox.encapsulatingPositions(trunks ∪ foliage ∪ decorations)`
    // (no `rootPositions` — no root placer implemented) — every absolute
    // position this ONE tree call wrote, from `dirty_start` (captured right
    // before trunk placement began) to now (right after decorators ran).
    let mut bbox: Option<(i32, i32, i32, i32, i32, i32)> = None;
    for (x, y, z, _) in grid.dirty_cells().skip(dirty_start) {
        bbox = Some(match bbox {
            None => (x, y, z, x, y, z),
            Some((min_x, min_y, min_z, max_x, max_y, max_z)) => {
                (min_x.min(x), min_y.min(y), min_z.min(z), max_x.max(x), max_y.max(y), max_z.max(z))
            }
        });
    }
    // `bbox` is `None` only if every write this tree attempted landed
    // outside `grid`'s own footprint (single-chunk mode, a lean/branch that
    // walked entirely off-chunk) — matching `placed_log`/`placed_leaf`
    // above tracking ATTEMPTS, not landed writes. Real vanilla's own bbox
    // is always non-empty here (its world has no footprint to fall outside
    // of), so this is a narrowing specific to this engine's bounded grid,
    // not a case vanilla itself has — nothing to update in that case.
    if let Some(bbox) = bbox {
        update_leaf_distances(grid, tags, &trunk_positions, bbox);
    }
}

/// `net.minecraft.world.level.levelgen.feature.treedecorators.BeehiveDecorator`,
/// approximated — see module doc's "Approximations, named" section. The
/// **log**-row half (`logs.getFirst()`/`getLast()`, i.e. the lowest/highest
/// log Y) is exact for this engine's straight trunks: exactly one log per Y
/// level, so "lowest"/"highest" is unambiguous regardless of Java `HashSet`
/// iteration order. The **leaf**-row half (`leaves.getFirst()`) has no such
/// invariant in general (a canopy has many leaves per Y row) — approximated
/// here as the canopy's topmost row, matching vanilla's own `hiveY` formula
/// shape (`max(topLeafRow - 1, topLogRow + 1)`) without vanilla's specific
/// (and not portably reproducible) choice of *which* leaf anchors it.
fn place_beehive_decorator<R: RandomSource>(
    random: &mut R,
    probability: f32,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
) {
    // logs is never empty here (a straight trunk always tries to place at
    // least one log at y=0..tree_height, per place_tree above).
    if random.next_float() >= probability {
        return;
    }
    let logs_bottom_y = origin.y;
    let logs_top_y = origin.y + tree_height - 1;
    // Approximate "top leaf row" as the topmost row the foliage placer's own
    // `offset` reaches (the highest possible leaf Y for this tree).
    let leaves_top_y = origin.y + tree_height; // attachment.y, foliage's own highest reachable row (offset >= 0 for every species here)
    let hive_y = (leaves_top_y - 1).max(logs_bottom_y + 1).min(logs_top_y);

    const SPAWN_DIRECTIONS: [(i32, i32); 3] = [(1, 0), (-1, 0), (0, -1)]; // east, west, north — all but south (the worldgen-fixed facing)
    let mut candidates: Vec<(i32, i32, i32)> = SPAWN_DIRECTIONS
        .iter()
        .map(|(dx, dz)| (origin.x + dx, hive_y, origin.z + dz))
        .collect();

    // `Util.shuffle` on a fixed 3-element list — a Fisher-Yates pass draws
    // exactly 2 `nextInt` calls regardless of list contents, so the RNG-draw
    // *count* here is exact even though the resulting order need not match
    // vanilla's own (which starts from a differently-ordered candidate list
    // in the ambiguous-iteration-order case this module already named).
    for i in (1..candidates.len()).rev() {
        let j = random.next_int_bounded(i as i32 + 1) as usize;
        candidates.swap(i, j);
    }

    let Some(&(hx, hy, hz)) = candidates.iter().find(|&&(x, y, z)| {
        is_air(base_id(grid.get(x, y, z))) && is_air(base_id(grid.get(x, y, z + 1)))
    }) else {
        return;
    };

    let state = "minecraft:bee_nest[facing=south,honey_level=0]".to_string();
    grid.set_if_in_bounds(hx, hy, hz, state);
    // Bee-entity storage (2-3 bees) is not modelled — this engine has no
    // block-entity/NBT layer for a freshly generated chunk to carry it in;
    // named here rather than silently pretending the hive is fully stocked.
    let _bee_count = 2 + random.next_int_bounded(2);
}

#[cfg(test)]
mod tests {
    use super::*;
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
            feature_size: FeatureSizeCfg {
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
            feature_size: FeatureSizeCfg {
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
            feature_size: FeatureSizeCfg {
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
