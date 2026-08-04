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
//! **Single-chunk only — no cross-chunk feature spill.** Vanilla's
//! `blockStateWriteRadius(1)` at the FEATURES generation stage
//! (`ChunkPyramid.java:32-35`, the same limit `docs/worldgen-parity.md`
//! documents for the ore 3×3 driver) applies to `VEGETAL_DECORATION` too —
//! a tree placed near a chunk edge can legitimately spill canopy into a
//! neighbour, and a neighbour's own pass can spill grass/leaves into this
//! chunk. This module runs **only the centre chunk's own pass**: a write
//! whose final position lands outside the local `0..16` × `0..16` footprint
//! is silently dropped (never written anywhere — there is no neighbour grid
//! to write into), and a read (heightmap probe, air/tag check) clamps into
//! the nearest in-bounds column rather than seeing real neighbour terrain.
//! This is the same shape the ore engine had *before* issue #295's 3×3
//! driver landed (see `docs/worldgen-parity.md`'s "before Job 3" numbers) —
//! an accepted, named intermediate milestone, not a hidden approximation.
//! Extending this to a real 3×3+ driver (canopies are small — 2-3 blocks for
//! all three species here — so the affected fraction of edge columns is
//! bounded but nonzero) is the natural next increment and is **not**
//! attempted in this landing.
//!
//! **No oracle validates this against a real vanilla dump.**
//! `docs/worldgen-parity.md`'s "what could not be isolated" section already
//! recorded, before this issue, that vegetation is "not composed into
//! `ComposedChunkOracle.java` and not built anywhere in this crate's Rust
//! ... no isolated oracle for it exists yet in `scripts/worldgen-oracle/`
//! either." That remains true after this module — building one (an 8-more-
//! real-chunk composed dump, or a single-biome isolated `VegetationOracle
//! .java` analogous to `FeatureOracle.java`) is real, separate work, not
//! attempted here. Every count this module's tests assert is derived
//! **from the embedded placement-modifier JSON itself** (`expected_value()`
//! on the outer `count` provider, `noise_threshold_count`'s two constants,
//! etc.) plus a hand-computation of what the *engine* should produce from
//! those inputs — an internal-consistency check, not a live-vanilla parity
//! number. Per CLAUDE.md's evidence standard: this is **not** parity
//! evidence, and this module's own report does not claim it is.
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
//!
//! `pine`/`PineFoliagePlacer` was added beyond the issue's literal "oak,
//! birch, spruce" minimum because it shares [`TrunkPlacerCfg`] entirely and
//! is a small, self-contained addition ([`FoliagePlacerCfg::Pine`]) that
//! turns taiga's honest coverage from ~66% to ~99%, in contrast to oak's
//! `fancy_oak`/`FallenTreeFeature`, which are structurally different
//! trunk/foliage/feature families and are out of scope for this landing.
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
    MatchingBlockTag(String),
    /// Approximates every `would_survive` check this module reaches as
    /// `VegetationBlock.mayPlaceOn` — see module doc.
    WouldSurviveOnSupportsVegetation,
}

impl BlockPredicate {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().unwrap_or("minecraft:true");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "not" => BlockPredicate::Not(Box::new(BlockPredicate::parse(&v["predicate"]))),
            "matching_block_tag" => {
                BlockPredicate::MatchingBlockTag(v["tag"].as_str().unwrap_or_default().to_string())
            }
            "would_survive" => BlockPredicate::WouldSurviveOnSupportsVegetation,
            _ => BlockPredicate::True,
        }
    }

    fn test(&self, grid: &VegGrid, tags: &VegTags, pos: BlockPos) -> bool {
        match self {
            BlockPredicate::True => true,
            BlockPredicate::Not(inner) => !inner.test(grid, tags, pos),
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
            BlockPredicate::WouldSurviveOnSupportsVegetation => {
                let below = base_id(grid.get(pos.x, pos.y - 1, pos.z));
                tags.supports_vegetation.contains(below)
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
                "trapezoid" => {
                    // TrapezoidInt is not exactly Uniform, but every
                    // xz_spread/y_spread in this module's own scope
                    // (patch_grass_*/flower_*'s random_offset) is symmetric
                    // (min == -max, plateau == 0); approximating it as
                    // Uniform over the same [min, max] preserves the mean
                    // and support exactly, only the interior shape (triangular
                    // vs flat) differs — named, not hidden.
                    Some(IntProvider::Uniform {
                        min: v["min"].as_i64()? as i32,
                        max: v["max"].as_i64()? as i32,
                    })
                }
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
                if pred.test(grid, tags, pos) {
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

/// `net.minecraft.world.level.levelgen.feature.trunkplacers.StraightTrunkPlacer`
/// — the only trunk placer this module implements (see module doc).
#[derive(Clone, Copy, Debug)]
pub struct TrunkPlacerCfg {
    base_height: i32,
    height_rand_a: i32,
    height_rand_b: i32,
}

impl TrunkPlacerCfg {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        if ty.strip_prefix("minecraft:").unwrap_or(ty) != "straight_trunk_placer" {
            return None;
        }
        Some(Self {
            base_height: v["base_height"].as_i64()? as i32,
            height_rand_a: v["height_rand_a"].as_i64()? as i32,
            height_rand_b: v["height_rand_b"].as_i64()? as i32,
        })
    }

    /// `TrunkPlacer.getTreeHeight`.
    fn get_tree_height<R: RandomSource>(&self, random: &mut R) -> i32 {
        self.base_height
            + random.next_int_bounded(self.height_rand_a + 1)
            + random.next_int_bounded(self.height_rand_b + 1)
    }
}

/// `net.minecraft.world.level.levelgen.feature.foliageplacers.FoliagePlacer`
/// (the `Blob`/`Spruce`/`Pine` subset — see module doc's "Named per-branch
/// gaps" for why `Pine` is here despite not being one of the issue's three
/// named species).
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
        }
    }

    fn foliage_radius<R: RandomSource>(&self, random: &mut R, trunk_len: i32) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { radius, .. } | FoliagePlacerCfg::Spruce { radius, .. } => {
                radius.sample(random)
            }
            FoliagePlacerCfg::Pine { radius, .. } => {
                radius.sample(random) + random.next_int_bounded(trunk_len.max(0) + 1)
            }
        }
    }

    fn sample_offset<R: RandomSource>(&self, random: &mut R) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { offset, .. }
            | FoliagePlacerCfg::Spruce { offset, .. }
            | FoliagePlacerCfg::Pine { offset, .. } => offset.sample(random),
        }
    }

    /// `FoliagePlacer.shouldSkipLocation` (double-trunk always `false` for
    /// every species this module implements — none use a fancy/giant trunk).
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
        }
    }

    fn create_foliage<R: RandomSource>(
        &self,
        random: &mut R,
        attachment: BlockPos,
        foliage_height: i32,
        leaf_radius: i32,
        offset: i32,
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

/// `net.minecraft.world.level.levelgen.feature.ConfiguredFeature` (the
/// subset reached from grass/flower/tree biome steps). [`Unsupported`]
/// carries the vanilla type string purely for diagnostics — placing it is
/// always a no-op.
#[derive(Clone, Debug)]
pub enum ConfiguredFeature {
    SimpleBlock(BlockStateProvider),
    Tree(Box<TreeConfig>),
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

/// The mutable, chunk-local (`0..16` × `0..16`, absolute `y`) block field
/// vegetal decoration reads and writes. See module doc's "Scope" section for
/// why this is chunk-local rather than the ore engine's centre-±1 region.
#[derive(Debug)]
pub struct VegGrid {
    blocks: HashMap<(i32, i32, i32), String>,
    min_y: i32,
    height: i32,
}

impl VegGrid {
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        Self {
            blocks: HashMap::new(),
            min_y,
            height,
        }
    }

    fn in_bounds(x: i32, z: i32) -> bool {
        (0..16).contains(&x) && (0..16).contains(&z)
    }

    fn clamp(&self, x: i32, z: i32) -> (i32, i32) {
        (x.clamp(0, 15), z.clamp(0, 15))
    }

    /// Seeds one column position from the post-ore composed grid. Callers
    /// fill every `(x, y, z)` in `0..16 × min_y..min_y+height × 0..16`
    /// before running vegetal decoration.
    pub fn seed(&mut self, x: i32, y: i32, z: i32, state: String) {
        self.blocks.insert((x, y, z), state);
    }

    /// Reads always succeed (clamped into bounds) — a read past the local
    /// footprint approximates the nearest in-bounds column rather than
    /// panicking or returning a sentinel the caller has to special-case.
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> &str {
        if y < self.min_y || y >= self.min_y + self.height {
            return "minecraft:air";
        }
        let (lx, lz) = self.clamp(x, z);
        self.blocks
            .get(&(lx, y, lz))
            .map_or("minecraft:air", String::as_str)
    }

    /// Writes past the local `0..16` footprint (or outside the vertical
    /// build range) are dropped, not clamped — see module doc's "Scope"
    /// section; there is no neighbour-chunk grid here to write into, and
    /// clamping a write would fabricate a block on the wrong column.
    /// Returns whether the write actually landed.
    pub fn set_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: String) -> bool {
        if Self::in_bounds(x, z) && y >= self.min_y && y < self.min_y + self.height {
            self.blocks.insert((x, y, z), state);
            true
        } else {
            false
        }
    }

    /// `Heightmap.Types.WORLD_SURFACE`/`WORLD_SURFACE_WG` — topmost non-air,
    /// scanned live against the current (possibly already-modified-this-step)
    /// grid. Returns `min_y` (not `min_y - 1`) for an all-air column, matching
    /// vanilla's `y + 1` convention with `y` floored at one below the lowest
    /// placeable block.
    #[must_use]
    pub fn height_world_surface(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.clamp(x, z);
        for y in (self.min_y..self.min_y + self.height).rev() {
            let base = base_id(self.get(lx, y, lz));
            if !is_air(base) {
                return y + 1;
            }
        }
        self.min_y
    }

    /// `Heightmap.Types.OCEAN_FLOOR`/`OCEAN_FLOOR_WG` — topmost non-air,
    /// non-fluid.
    #[must_use]
    pub fn height_ocean_floor(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.clamp(x, z);
        for y in (self.min_y..self.min_y + self.height).rev() {
            let base = base_id(self.get(lx, y, lz));
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
        ConfiguredFeature::SimpleBlock(provider) => place_simple_block(random, pos, provider, grid, tags),
        ConfiguredFeature::Tree(cfg) => place_tree(random, pos, cfg, grid, tags),
        ConfiguredFeature::RandomSelector { default, options } => {
            for (chance, option) in options {
                if random.next_float() < *chance {
                    place_placed_feature(random, pos, option, grid, tags);
                    return;
                }
            }
            place_placed_feature(random, pos, default, grid, tags);
        }
        ConfiguredFeature::SimpleRandomSelector(list) => {
            if list.is_empty() {
                return;
            }
            let idx = random.next_int_bounded(list.len() as i32) as usize;
            place_placed_feature(random, pos, &list[idx], grid, tags);
        }
        ConfiguredFeature::Unsupported(_) => {}
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
        return;
    };
    // `VegetationBlock.canSurvive`: the block below must support vegetation
    // — see module doc on why this is applied uniformly.
    let below = base_id(grid.get(pos.x, pos.y - 1, pos.z));
    if !tags.supports_vegetation.contains(below) {
        return;
    }
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
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

    if let Some(below_provider) = &cfg.below_trunk_provider {
        let below_pos = BlockPos {
            x: origin.x,
            y: origin.y - 1,
            z: origin.z,
        };
        if let Some(state) = below_provider.get_state(grid, tags, random, below_pos) {
            grid.set_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
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
            }
        }
    }
    let attachment = BlockPos {
        x: origin.x,
        y: origin.y + tree_height,
        z: origin.z,
    };

    let offset = cfg.foliage_placer.sample_offset(random);
    let mut placed_leaf = false;
    cfg.foliage_placer.create_foliage(
        random,
        attachment,
        foliage_height,
        leaf_radius,
        offset,
        grid,
        tags,
        &cfg.foliage_provider,
        &mut placed_leaf,
    );

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
        let mut grid = VegGrid::new(min_y, height);
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
        let mut grid = VegGrid::new(-64, 384);
        assert!(!grid.set_if_in_bounds(-1, 70, 5, "minecraft:oak_log".to_string()));
        assert!(!grid.set_if_in_bounds(16, 70, 5, "minecraft:oak_log".to_string()));
        assert!(grid.set_if_in_bounds(0, 70, 5, "minecraft:oak_log".to_string()));
        assert!(grid.set_if_in_bounds(15, 70, 5, "minecraft:oak_log".to_string()));
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
            trunk_placer: TrunkPlacerCfg {
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
            trunk_placer: TrunkPlacerCfg {
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
            trunk_placer: TrunkPlacerCfg {
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
        let mut grid = VegGrid::new(-64, 384);
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
}
