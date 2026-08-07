//! The data layer vegetal decoration is parsed into: heightmap kinds, block
//! predicates, block-state providers, placement modifiers, decorators, and the
//! `PlacedFeature`/`ConfiguredFeature` resolution that turns a registry document into
//! one of them.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B; see [`super`]'s own
//! module doc for the scope and the named approximations.

use std::collections::HashSet;

use serde_json::Value;

use crate::density::Resolver;
use crate::feature::{BlockPos, IntProvider};
use crate::rng::RandomSource;

use super::base_id;
use super::grid::VegGrid;
use super::grid::census::bump as census_bump;
use super::tree::{FoliagePlacerCfg, TrunkPlacerCfg};

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

pub(super) fn parse_predicate_list(v: &Value) -> Vec<BlockPredicate> {
    v["predicates"]
        .as_array()
        .map(|arr| arr.iter().map(BlockPredicate::parse).collect())
        .unwrap_or_default()
}

pub(super) fn parse_offset(v: &Value) -> (i32, i32, i32) {
    let Some(arr) = v.as_array() else {
        return (0, 0, 0);
    };
    let get = |i: usize| arr.get(i).and_then(Value::as_i64).unwrap_or(0) as i32;
    (get(0), get(1), get(2))
}

/// See [`BlockPredicate::MatchingFluid`]'s doc: both the source and flowing
/// JSON ids for one fluid collapse onto this engine's single base id.
pub(super) fn fluid_base_matches(fluid_id: &str, base: &str) -> bool {
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

pub(super)     fn test(&self, grid: &VegGrid, tags: &VegTags, pos: BlockPos) -> bool {
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

pub(super) fn is_air(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

pub(super) fn is_fluid(base: &str) -> bool {
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

pub(super) fn canon_state(v: &Value) -> String {
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

pub(super)     fn get_state<R: RandomSource>(
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
    /// `#minecraft:leaves` — `TreeFeature.isAirOrLeaves`, the anchor gate
    /// [`place_dark_oak_trunk`] checks before attempting each 2×2 log layer
    /// (a dark oak trunk can grow up through a neighbour's already-placed
    /// canopy; dense dark forests depend on that).
    pub leaves: HashSet<String>,
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
        leaves: resolve("minecraft:leaves"),
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
pub(super) fn try_parse_int_provider(v: &Value) -> Option<IntProvider> {
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

pub(super)     fn get_positions<R: RandomSource>(
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

/// `net.minecraft.world.level.levelgen.feature.featuresize.FeatureSize` — both
/// subclasses vanilla ships, each reachable from tree configs this module
/// implements: `TwoLayersFeatureSize` (oak, birch, spruce, pine, acacia) and
/// `ThreeLayersFeatureSize` (dark oak, pale oak — the 2×2-trunk species,
/// issue #428). The two share the `getSizeAtHeight(treeHeight, yo)` shape but
/// answer it differently: `TwoLayers` splits at `limit`; `ThreeLayers` splits
/// into lower/middle/upper bands using `upper_limit` measured down from the
/// tree's own height, which is why the caller must pass `tree_height`.
#[derive(Clone, Copy, Debug)]
pub enum FeatureSizeCfg {
    TwoLayers {
        limit: i32,
        lower_size: i32,
        upper_size: i32,
    },
    ThreeLayers {
        limit: i32,
        upper_limit: i32,
        lower_size: i32,
        middle_size: i32,
        upper_size: i32,
    },
}

impl FeatureSizeCfg {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "two_layers_feature_size" => Some(Self::TwoLayers {
                limit: v.get("limit").and_then(Value::as_i64).unwrap_or(1) as i32,
                lower_size: v.get("lower_size").and_then(Value::as_i64).unwrap_or(0) as i32,
                upper_size: v.get("upper_size").and_then(Value::as_i64).unwrap_or(1) as i32,
            }),
            "three_layers_feature_size" => Some(Self::ThreeLayers {
                limit: v.get("limit").and_then(Value::as_i64).unwrap_or(1) as i32,
                upper_limit: v.get("upper_limit").and_then(Value::as_i64).unwrap_or(1) as i32,
                lower_size: v.get("lower_size").and_then(Value::as_i64).unwrap_or(0) as i32,
                middle_size: v.get("middle_size").and_then(Value::as_i64).unwrap_or(1) as i32,
                upper_size: v.get("upper_size").and_then(Value::as_i64).unwrap_or(1) as i32,
            }),
            _ => None,
        }
    }

    /// `FeatureSize.getSizeAtHeight(treeHeight, yo)`. The `tree_height`
    /// argument only matters for `ThreeLayers` (the upper band is `yo >=
    /// treeHeight - upperLimit`); `TwoLayers` ignores it.
pub(super)     fn size_at_height(&self, tree_height: i32, y: i32) -> i32 {
        match *self {
            Self::TwoLayers { limit, lower_size, upper_size } => {
                if y < limit {
                    lower_size
                } else {
                    upper_size
                }
            }
            Self::ThreeLayers { limit, upper_limit, lower_size, middle_size, upper_size } => {
                if y < limit {
                    lower_size
                } else if y >= tree_height - upper_limit {
                    upper_size
                } else {
                    middle_size
                }
            }
        }
    }
}

/// `net.minecraft.world.level.levelgen.feature.configurations.TreeConfiguration`.
#[derive(Clone, Debug)]
pub struct TreeConfig {
pub(super)     below_trunk_provider: Option<BlockStateProvider>,
pub(super)     trunk_provider: BlockStateProvider,
pub(super)     foliage_provider: BlockStateProvider,
pub(super)     trunk_placer: TrunkPlacerCfg,
pub(super)     foliage_placer: FoliagePlacerCfg,
pub(super)     feature_size: FeatureSizeCfg,
pub(super)     decorators: Vec<Decorator>,
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
pub(super)     layers: Vec<(IntProvider, BlockStateProvider)>,
pub(super)     direction: (i32, i32, i32),
pub(super)     allowed_placement: BlockPredicate,
pub(super)     prioritize_tip: bool,
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

pub(super) fn unsupported_placed_ref(why: &str) -> PlacedRef {
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

pub(super) fn parse_placed_feature_doc(resolver: &dyn Resolver, doc: &Value) -> PlacedRef {
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

pub(super) fn parse_configured_feature_doc(resolver: &dyn Resolver, doc: &Value) -> ConfiguredFeature {
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
