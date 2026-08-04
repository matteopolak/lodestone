//! Version-free **feature / placement** interpreter — the stage that populates
//! carved terrain with ores (and, later, vegetation and trees).
//!
//! Like the density router and surface rules, features are *data*: vanilla ships
//! `configured_feature/*.json`, `placed_feature/*.json` and per-biome step lists
//! as datapack files. This module is the engine that walks those definitions and
//! reproduces vanilla's exact RNG-consumption order; the per-version JSON lives
//! in the version crate (mirrored into the worldgen test fixtures here).
//!
//! ## What vanilla does (and this reproduces)
//!
//! `ChunkGenerator.applyBiomeDecoration` derives a per-chunk `decorationSeed`
//! (`WorldgenRandom.setDecorationSeed`), then for each generation *step* and each
//! feature within it (in a globally-sorted order that, for a single biome, is the
//! biome's own list order) reseeds the RNG with `setFeatureSeed(decorationSeed,
//! featureIndexInStep, stepIndex)` and places that feature. **Each feature
//! reseeds independently**, so features are RNG-isolated from one another — a bug
//! in one cannot desynchronise the next.
//!
//! Within one placed feature, vanilla composes *placement modifiers* as a lazy
//! `Stream` pipeline: `Stream.of(origin)` flat-mapped through each modifier in
//! turn, terminating in `feature.place` per surviving position. Java's `flatMap`
//! is depth-first, so the draw order is: modifier 0 emits its positions (drawing
//! as it goes), then for *each* of those, modifier 1 runs fully (including the
//! eventual `place`), and so on. [`place_ore_feature`] reproduces that exact
//! nesting with a recursion, keeping every modifier a separate composable unit
//! (no fused loops).
//!
//! ## Ore placement RNG order (per emitted position)
//!
//! `OreFeature.place` draws, in order: `nextFloat()` (blob direction),
//! `nextInt(3)`, `nextInt(3)` (the two y-endpoints). It then reads the
//! `OCEAN_FLOOR_WG` heightmap around the origin (no draws); **only if** some probe
//! is at/below the blob does it proceed to `doPlace`, which draws `nextDouble()`
//! once per `i in 0..size` (the blob radii). `canPlaceOre` draws nothing for a
//! *non-buried* ore (`discard_chance_on_air_exposure == 0` makes
//! `shouldSkipAirCheck` short-circuit true), but a **buried** ore
//! (`0 < discard < 1`, e.g. `ore_gold_buried`, `ore_coal_buried`) draws a single
//! `nextFloat()` in `shouldSkipAirCheck` for *each candidate position whose block
//! matches a target tag* — so its per-feature draw count is position-dependent.
//! `discard >= 1` (e.g. `ore_diamond_buried`) draws nothing and places only when
//! fully enclosed. The `tag_match` `RuleTest` itself never draws.
//!
//! Note the deliberate float typing: the blob *centres* use `java.lang.Math.sin`
//! /`cos` (real `f64` transcendentals), while the per-step *radius* uses
//! `Mth.sin` (the 65536-entry table, [`crate::math::sin`]). Getting these
//! swapped produces a plausible-but-wrong world, which is why the oracle compares
//! whole-chunk block output rather than sampled positions.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::math;
use crate::rng::{RandomSource, WorldgenRandom};

/// Grass/flower/tree placement (issue #406) — a separate engine from the
/// rest of this module (ores), sharing only [`BlockPos`]/[`IntProvider`]/
/// [`canon_state`] and the [`STEP_VEGETAL_DECORATION`] constant below. See
/// its own module doc for scope and named gaps.
pub mod vegetation;

/// `TOP_LAYER_MODIFICATION` — snow layers and surface ice (issue #404's U2),
/// vanilla's `freeze_top_layer`. A third engine again: it consumes no RNG, never
/// writes outside its own chunk, and reads per-block-state facts (collision
/// UP-face fullness, fluid presence) that neither the ore nor the vegetation
/// engine needs. See its own module doc.
pub mod top_layer;

/// `GenerationStep.Decoration.UNDERGROUND_ORES.ordinal()`.
pub const STEP_UNDERGROUND_ORES: i32 = 6;

/// `GenerationStep.Decoration.VEGETAL_DECORATION.ordinal()` — grass, flowers
/// and trees (issue #406). One past `UNDERGROUND_DECORATION`/`FLUID_SPRINGS`
/// (7, 8), which this engine does not compose; see
/// `net.minecraft.world.level.levelgen.GenerationStep.Decoration`'s own
/// declaration order (`RAW_GENERATION, LAKES, LOCAL_MODIFICATIONS,
/// UNDERGROUND_STRUCTURES, SURFACE_STRUCTURES, STRONGHOLDS, UNDERGROUND_ORES,
/// UNDERGROUND_DECORATION, FLUID_SPRINGS, VEGETAL_DECORATION,
/// TOP_LAYER_MODIFICATION`).
pub const STEP_VEGETAL_DECORATION: i32 = 9;

/// A block position with `i32` components (`net.minecraft.core.BlockPos`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// `net.minecraft.world.level.levelgen.VerticalAnchor`.
#[derive(Clone, Copy, Debug)]
pub enum VerticalAnchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
}

impl VerticalAnchor {
    #[must_use]
    pub fn resolve_y(self, min_gen_y: i32, gen_depth: i32) -> i32 {
        match self {
            VerticalAnchor::Absolute(y) => y,
            VerticalAnchor::AboveBottom(o) => min_gen_y + o,
            VerticalAnchor::BelowTop(o) => gen_depth - 1 + min_gen_y - o,
        }
    }

    fn parse(v: &Value) -> Self {
        if let Some(y) = v.get("absolute") {
            VerticalAnchor::Absolute(y.as_i64().expect("absolute anchor") as i32)
        } else if let Some(o) = v.get("above_bottom") {
            VerticalAnchor::AboveBottom(o.as_i64().expect("above_bottom anchor") as i32)
        } else if let Some(o) = v.get("below_top") {
            VerticalAnchor::BelowTop(o.as_i64().expect("below_top anchor") as i32)
        } else {
            panic!("unknown vertical anchor: {v}");
        }
    }
}

/// `net.minecraft.util.valueproviders.IntProvider` (the subset features use).
///
/// [`IntProvider::WeightedList`] (issue #406's `trees_plains`/`trees_birch`/
/// `trees_taiga` outer `count` — e.g. `{data: 0, weight: 19}, {data: 1,
/// weight: 1}`) is additive: nothing in the ore engine (issue #295)
/// constructs it, so this is a strict superset, not a behaviour change to
/// any existing caller.
#[derive(Clone, Debug)]
pub enum IntProvider {
    Constant(i32),
    Uniform { min: i32, max: i32 },
    /// `net.minecraft.util.valueproviders.WeightedListInt` — a `WeightedList<Integer>`.
    /// `(value, weight)` pairs, in JSON declaration order (order doesn't
    /// affect the *distribution*, but does affect [`WeightedList::sample`]'s
    /// draw semantics, which walks the list in order — see that fn's doc).
    WeightedList(Vec<(i32, i32)>),
    /// `net.minecraft.util.valueproviders.BiasedToBottomInt` — used by
    /// `cactus`/`sugar_cane`'s `BlockColumnFeature` layer heights (issue
    /// #406's cacti/sugar-cane increment). Additive: nothing before that
    /// increment constructs this variant.
    BiasedToBottom { min: i32, max: i32 },
    /// `net.minecraft.util.valueproviders.TrapezoidInt`, the REAL
    /// (two-draw, triangular) sample — not the `Uniform` approximation
    /// `crate::feature::vegetation::try_parse_int_provider` used to fold
    /// this into. That approximation preserved mean and support but not
    /// **draw count**: vanilla's symmetric case
    /// (`min == -max, plateau == 0`, which is every `random_offset` this
    /// crate's vegetation engine actually uses) draws `nextInt` TWICE and
    /// subtracts, while `Uniform` draws once — every RNG call after the
    /// first desyncs completely from vanilla's own stream. Found via
    /// `tests/vegetation_parity.rs` (issue #406's real-oracle evidence
    /// gap): `patch_grass_plain`'s placed positions were disjoint,
    /// bit-for-bit, from the real JVM's — not "close but off by a block",
    /// a full stream desync — because `random_offset`'s `xz_spread`/
    /// `y_spread` are exactly this symmetric trapezoid shape. See
    /// [`IntProvider::sample`] for the exact vanilla formula, ported.
    Trapezoid { min: i32, max: i32, plateau: i32 },
}

impl IntProvider {
    fn parse(v: &Value) -> Self {
        match v {
            Value::Number(n) => IntProvider::Constant(n.as_i64().expect("int") as i32),
            Value::Object(_) => {
                let ty = v["type"].as_str().unwrap_or("minecraft:constant");
                match ty.strip_prefix("minecraft:").unwrap_or(ty) {
                    "constant" => {
                        IntProvider::Constant(v["value"].as_i64().expect("constant value") as i32)
                    }
                    "uniform" => IntProvider::Uniform {
                        min: v["min_inclusive"].as_i64().expect("min_inclusive") as i32,
                        max: v["max_inclusive"].as_i64().expect("max_inclusive") as i32,
                    },
                    "weighted_list" => {
                        let entries = v["distribution"]
                            .as_array()
                            .expect("weighted_list distribution")
                            .iter()
                            .map(|e| {
                                (
                                    e["data"].as_i64().expect("weighted_list data") as i32,
                                    e["weight"].as_i64().expect("weighted_list weight") as i32,
                                )
                            })
                            .collect();
                        IntProvider::WeightedList(entries)
                    }
                    other => panic!("unsupported int provider: {other}"),
                }
            }
            other => panic!("unexpected int provider json: {other}"),
        }
    }

    /// Expected value, for count-prediction tests — not consulted by
    /// [`IntProvider::sample`] itself.
    #[must_use]
    pub fn expected_value(&self) -> f64 {
        match self {
            IntProvider::Constant(v) => f64::from(*v),
            IntProvider::Uniform { min, max } => f64::from(min + max) / 2.0,
            IntProvider::WeightedList(entries) => {
                let total: i64 = entries.iter().map(|(_, w)| i64::from(*w)).sum();
                entries
                    .iter()
                    .map(|(v, w)| f64::from(*v) * f64::from(*w) / total as f64)
                    .sum()
            }
            // `BiasedToBottomInt.sample` is `min + nextInt(nextInt(n)+1)`
            // with `n = max-min+1`. Closed form: `Y = nextInt(nextInt(n))`
            // (0-based) has `E[Y] = (n-1)/4` (for fixed `Z ~ Uniform[0,n-1]`,
            // `E[Y|Z] = Z/2`, so `E[Y] = E[Z]/2 = ((n-1)/2)/2`).
            IntProvider::BiasedToBottom { min, max } => {
                let n = f64::from(max - min + 1);
                f64::from(*min) + (n - 1.0) / 4.0
            }
            // Symmetric (difference-of-uniforms) or plateau'd trapezoid,
            // both mean `(min+max)/2` regardless of shape.
            IntProvider::Trapezoid { min, max, .. } => f64::from(min + max) / 2.0,
        }
    }

    pub(crate) fn sample<R: RandomSource>(&self, random: &mut R) -> i32 {
        match self {
            IntProvider::Constant(v) => *v,
            IntProvider::Uniform { min, max } => {
                math::random_between_inclusive(random, *min, *max)
            }
            // `WeightedList.getRandom`: walk in declared order, subtracting a
            // `nextInt(totalWeight)` draw until it goes negative — the entry
            // it goes negative on is the pick. Matches
            // `net.minecraft.util.random.SimpleWeightedRandomList`'s walk
            // order exactly (declaration order, not sorted).
            IntProvider::WeightedList(entries) => {
                let total: i32 = entries.iter().map(|(_, w)| *w).sum();
                let mut roll = random.next_int_bounded(total.max(1));
                for (value, weight) in entries {
                    roll -= *weight;
                    if roll < 0 {
                        return *value;
                    }
                }
                entries.last().map_or(0, |(v, _)| *v)
            }
            // `BiasedToBottomInt.sample`: `minInclusive + nextInt(nextInt(maxInclusive
            // - minInclusive + 1) + 1)` — two nested, dependent draws, not one.
            IntProvider::BiasedToBottom { min, max } => {
                let n = *max - *min + 1;
                let inner = random.next_int_bounded(n);
                min + random.next_int_bounded(inner + 1)
            }
            // `TrapezoidInt.sample`, ported exactly (see this variant's own
            // doc comment for why the draw COUNT matters, not just the
            // resulting distribution's shape).
            IntProvider::Trapezoid { min, max, plateau } => {
                if *plateau == 0 && *max == -*min {
                    random.next_int_bounded(max + 1) - random.next_int_bounded(max + 1)
                } else {
                    let range = max - min;
                    if *plateau == range {
                        math::random_between_inclusive(random, *min, *max)
                    } else {
                        let plateau_start = (range - plateau) / 2;
                        let plateau_end = range - plateau_start;
                        min + math::random_between_inclusive(random, 0, plateau_end)
                            + math::random_between_inclusive(random, 0, plateau_start)
                    }
                }
            }
        }
    }
}

/// `net.minecraft.world.level.levelgen.heightproviders.HeightProvider`
/// (uniform + trapezoid — the two the overworld ores use).
#[derive(Clone, Copy, Debug)]
pub enum HeightProvider {
    Uniform {
        min: VerticalAnchor,
        max: VerticalAnchor,
    },
    Trapezoid {
        min: VerticalAnchor,
        max: VerticalAnchor,
        plateau: i32,
    },
}

impl HeightProvider {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().expect("height provider type");
        let min = VerticalAnchor::parse(&v["min_inclusive"]);
        let max = VerticalAnchor::parse(&v["max_inclusive"]);
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "uniform" => HeightProvider::Uniform { min, max },
            "trapezoid" => HeightProvider::Trapezoid {
                min,
                max,
                plateau: v["plateau"].as_i64().unwrap_or(0) as i32,
            },
            other => panic!("unsupported height provider: {other}"),
        }
    }

    /// Mirrors `UniformHeight.sample` / `TrapezoidHeight.sample`, including their
    /// exact `Mth.randomBetweenInclusive` draw counts.
    fn sample<R: RandomSource>(&self, random: &mut R, min_gen_y: i32, gen_depth: i32) -> i32 {
        match *self {
            HeightProvider::Uniform { min, max } => {
                let lo = min.resolve_y(min_gen_y, gen_depth);
                let hi = max.resolve_y(min_gen_y, gen_depth);
                if lo > hi {
                    lo
                } else {
                    math::random_between_inclusive(random, lo, hi)
                }
            }
            HeightProvider::Trapezoid { min, max, plateau } => {
                let lo = min.resolve_y(min_gen_y, gen_depth);
                let hi = max.resolve_y(min_gen_y, gen_depth);
                if lo > hi {
                    return lo;
                }
                let range = hi - lo;
                if plateau >= range {
                    return math::random_between_inclusive(random, lo, hi);
                }
                let plateau_start = (range - plateau) / 2;
                let plateau_end = range - plateau_start;
                lo + math::random_between_inclusive(random, 0, plateau_end)
                    + math::random_between_inclusive(random, 0, plateau_start)
            }
        }
    }
}

/// `net.minecraft.world.level.levelgen.placement.PlacementModifier` (ore subset).
/// Kept as separate composable variants — vanilla's structure, not a fused loop.
#[derive(Clone, Debug)]
pub enum Placement {
    Count(IntProvider),
    RarityFilter(i32),
    InSquare,
    HeightRange(HeightProvider),
    Biome,
}

impl Placement {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().expect("placement type");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "count" => Placement::Count(IntProvider::parse(&v["count"])),
            "rarity_filter" => {
                Placement::RarityFilter(v["chance"].as_i64().expect("rarity chance") as i32)
            }
            "in_square" => Placement::InSquare,
            "height_range" => Placement::HeightRange(HeightProvider::parse(&v["height"])),
            "biome" => Placement::Biome,
            other => panic!("unsupported placement modifier: {other}"),
        }
    }

    /// `PlacementModifier.getPositions`: emit the positions this modifier
    /// produces for one incoming position, drawing RNG exactly as vanilla does.
    fn get_positions<R: RandomSource>(
        &self,
        random: &mut R,
        pos: BlockPos,
        ctx: &Ctx,
    ) -> Vec<BlockPos> {
        match self {
            Placement::Count(count) => {
                let n = count.sample(random);
                vec![pos; n.max(0) as usize]
            }
            Placement::RarityFilter(chance) => {
                // RarityFilter.shouldPlace: nextFloat() < 1/chance.
                if random.next_float() < 1.0 / *chance as f32 {
                    vec![pos]
                } else {
                    Vec::new()
                }
            }
            Placement::InSquare => {
                let x = random.next_int_bounded(16) + pos.x;
                let z = random.next_int_bounded(16) + pos.z;
                vec![BlockPos { x, y: pos.y, z }]
            }
            Placement::HeightRange(h) => {
                let y = h.sample(random, ctx.min_gen_y, ctx.gen_depth);
                vec![BlockPos {
                    x: pos.x,
                    y,
                    z: pos.z,
                }]
            }
            // BiomeFilter for a single-biome world always keeps the position and
            // draws nothing (topFeature is in the biome by construction).
            Placement::Biome => vec![pos],
        }
    }
}

/// `net.minecraft.world.level.levelgen.structure.templatesystem.RuleTest` (the
/// two predicates overworld ores use).
#[derive(Clone, Debug)]
pub enum RuleTest {
    TagMatch(String),
    BlockMatch(String),
}

impl RuleTest {
    fn parse(v: &Value) -> Self {
        let ty = v["predicate_type"].as_str().expect("predicate_type");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "tag_match" => RuleTest::TagMatch(v["tag"].as_str().expect("tag").to_string()),
            "block_match" => RuleTest::BlockMatch(v["block"].as_str().expect("block").to_string()),
            other => panic!("unsupported rule test: {other}"),
        }
    }
}

/// One `OreConfiguration.TargetBlockState`: the block to place and the test the
/// existing block must pass.
#[derive(Clone, Debug)]
pub struct OreTarget {
    /// Canonicalised placed state (e.g. `minecraft:redstone_ore[lit=false]`).
    pub state: String,
    pub target: RuleTest,
}

/// `net.minecraft.world.level.levelgen.feature.configurations.OreConfiguration`.
#[derive(Clone, Debug)]
pub struct OreConfig {
    pub size: i32,
    pub discard_chance_on_air_exposure: f32,
    pub targets: Vec<OreTarget>,
}

/// A resolved, placeable ore feature: its index within the generation step (for
/// `setFeatureSeed`), its ordered placement modifiers, and its ore config.
#[derive(Clone, Debug)]
pub struct PlacedOre {
    pub index: usize,
    pub placements: Vec<Placement>,
    pub config: OreConfig,
}

struct Ctx {
    min_gen_y: i32,
    gen_depth: i32,
}

/// Canonicalise an ore JSON `state` object (`{Name, Properties?}`) the same way
/// the oracle canonicalises a `BlockState`: name plus alphabetically-sorted
/// `key=value` properties.
#[must_use]
pub fn canon_state(state: &Value) -> String {
    let name = state["Name"].as_str().expect("state Name");
    let mut out = name.to_string();
    if let Some(props) = state.get("Properties").and_then(Value::as_object) {
        let mut kv: Vec<(&String, String)> = props
            .iter()
            .map(|(k, v)| (k, v.as_str().unwrap_or_default().to_string()))
            .collect();
        kv.sort_by(|a, b| a.0.cmp(b.0));
        out.push('[');
        for (i, (k, v)) in kv.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push('=');
            out.push_str(v);
        }
        out.push(']');
    }
    out
}

/// Parse an `OreConfiguration` from a `configured_feature` JSON's `config` object.
#[must_use]
pub fn parse_ore_config(config: &Value) -> OreConfig {
    let targets = config["targets"]
        .as_array()
        .expect("ore targets")
        .iter()
        .map(|t| OreTarget {
            state: canon_state(&t["state"]),
            target: RuleTest::parse(&t["target"]),
        })
        .collect();
    OreConfig {
        size: config["size"].as_i64().expect("ore size") as i32,
        discard_chance_on_air_exposure: config["discard_chance_on_air_exposure"]
            .as_f64()
            .unwrap_or(0.0) as f32,
        targets,
    }
}

/// Parse the ordered placement modifiers from a `placed_feature` JSON.
#[must_use]
pub fn parse_placements(placed: &Value) -> Vec<Placement> {
    placed["placement"]
        .as_array()
        .expect("placement list")
        .iter()
        .map(Placement::parse)
        .collect()
}

/// The mutable block field the ore stage reads and writes, over the whole
/// driven 3×3 neighbourhood. Addressed by `(local_x, y, local_z)`,
/// centre-relative, with `local_x, local_z ∈ [`[`REGION_MIN`]`,`[`REGION_MAX`]`)`
/// and `y` absolute. See [`OreInput::region_local`] for why this is wider
/// than one chunk.
///
/// A [`crate::dense_grid::DenseBlockGrid`] (issue #106's ore-composition
/// perf pass), not a `HashMap<(i32,i32,i32), String>` — this was, per
/// `crate::overworld`'s own module doc "Performance" section, "the one
/// remaining `HashMap<(i32,i32,i32), String>` in the hot path", left there
/// deliberately by issue #295's Job 2 as further work. `stitch_region`
/// (`crate::overworld::OverworldGenerator`) populates one of these for all 9
/// source chunks in the driven neighbourhood on every single `column()`
/// call, so every cell used to cost a fresh heap-allocated `String` even
/// though real terrain repeats a small palette overwhelmingly — a
/// `DenseBlockGrid` interns each distinct state once and stores a `u16`
/// per cell instead. See `crate::overworld`'s module doc for the measured
/// before/after.
pub type RegionGrid = crate::dense_grid::DenseBlockGrid;

/// Centre-relative local coordinate lower/upper (exclusive) bound the 3×3
/// driver ([`apply_ore_step_3x3`]) reads and writes over: one 16-wide band
/// per row/column of the 3×3 chunk grid (`-16..0` the west/north neighbour,
/// `0..16` the centre, `16..32` the east/south neighbour).
pub const REGION_MIN: i32 = -16;
pub const REGION_MAX: i32 = 32;

/// Inputs the ore driver needs beyond the RNG.
pub struct OreInput<'a> {
    /// The chunk currently placing features — its own origin/seed
    /// ([`OreInput::origin`]). The [`apply_ore_step_3x3`] driver varies this
    /// across the 9 source chunks while holding `center_x`/`center_z` fixed.
    pub chunk_x: i32,
    pub chunk_z: i32,
    /// The chunk whose output is being dumped/compared. [`OreInput::in_center`]
    /// tests writes against this pair, not `chunk_x`/`chunk_z` — the whole
    /// point of the 3×3 driver is that a source chunk other than the centre
    /// can still produce a write that lands in the centre (vanilla's real
    /// `blockStateWriteRadius(1)` at the FEATURES stage, `ChunkPyramid.java:32-35`).
    pub center_x: i32,
    pub center_z: i32,
    pub min_y: i32,
    pub height: i32,
    /// `getMinGenY` / `getGenDepth` for `VerticalAnchor` resolution.
    pub min_gen_y: i32,
    pub gen_depth: i32,
    /// `OCEAN_FLOOR_WG` heightmap across the whole driven 3×3 region, as
    /// `level.getHeight` returns it, keyed by centre-relative local
    /// `(x, z) ∈ [`[`REGION_MIN`]`,`[`REGION_MAX`]`)` (see
    /// [`OreInput::region_local`] for probes landing outside that range).
    pub ocean_floor_wg: &'a HashMap<(i32, i32), i32>,
    /// `true` iff the given block base name is in the given tag (closure already
    /// resolved by the caller).
    pub in_tag: &'a dyn Fn(&str, &str) -> bool,
}

impl std::fmt::Debug for OreInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OreInput")
            .field("chunk_x", &self.chunk_x)
            .field("chunk_z", &self.chunk_z)
            .field("center_x", &self.center_x)
            .field("center_z", &self.center_z)
            .field("min_y", &self.min_y)
            .field("height", &self.height)
            .field("min_gen_y", &self.min_gen_y)
            .field("gen_depth", &self.gen_depth)
            .field("ocean_floor_wg", &self.ocean_floor_wg.len())
            .finish_non_exhaustive()
    }
}

impl OreInput<'_> {
    fn origin(&self) -> BlockPos {
        BlockPos {
            x: self.chunk_x * 16,
            y: self.min_y, // section origin y = minSectionY*16 = min_y
            z: self.chunk_z * 16,
        }
    }

    /// Exact (unclamped) local coordinate within the CENTRE chunk only —
    /// `None` for anything else, including a position inside one of the 8
    /// neighbour chunks. This is the boundary the fixture's `ore.*`/`in.*`
    /// data is scoped to: only writes landing in the centre are reported,
    /// even though every one of the 9 source passes can produce them.
    #[must_use]
    pub fn in_center(&self, x: i32, z: i32) -> Option<(i32, i32)> {
        let lx = x - self.center_x * 16;
        let lz = z - self.center_z * 16;
        if (0..16).contains(&lx) && (0..16).contains(&lz) {
            Some((lx, lz))
        } else {
            None
        }
    }

    /// Centre-relative local coordinates, **clamped** into the driven 3×3
    /// region (`[`[`REGION_MIN`]`,`[`REGION_MAX`]`)` each axis).
    ///
    /// Vanilla's real `blockStateWriteRadius(1)` reach is nominally one
    /// chunk, but the largest overworld blob ores (`size=64`:
    /// andesite/diorite/granite/tuff) can, in rare boundary-adjacent cases,
    /// probe or write up to ~13 blocks beyond the chunk they originate in —
    /// enough to exceed this 3×3 footprint by a further ~13 blocks in the
    /// extreme. Rather than drive an unbounded neighbourhood (which has no
    /// natural stopping point and no oracle counterpart), reads/writes past
    /// the region are clamped to the nearest column this driver actually
    /// modelled — a bounded, honest approximation, not the old proxy's "wrap
    /// into centre" (which was wrong for effectively every out-of-chunk
    /// probe, not just this residual). See `docs/worldgen-parity.md`'s
    /// "known gap" section for the measured scope of this.
    fn region_local(&self, x: i32, z: i32) -> (i32, i32) {
        let lx = (x - self.center_x * 16).clamp(REGION_MIN, REGION_MAX - 1);
        let lz = (z - self.center_z * 16).clamp(REGION_MIN, REGION_MAX - 1);
        (lx, lz)
    }

    fn get_height(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.region_local(x, z);
        *self.ocean_floor_wg.get(&(lx, lz)).unwrap_or_else(|| {
            panic!("OreInput::get_height: no heightmap entry for region-local ({lx},{lz})")
        })
    }
}

/// Runs ONE source chunk's own `UNDERGROUND_ORES` step (`input.chunk_x`/
/// `chunk_z`'s own origin and decoration seed), writing into `working`
/// in-place (region-local, centre-relative coordinates — see
/// [`OreInput::region_local`]). Returns the derived decoration seed, mostly
/// so callers can cross-check the centre pass's own seed against an oracle.
fn apply_one_source<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    input: &OreInput<'_>,
    ores: &[PlacedOre],
    working: &mut RegionGrid,
) -> i64 {
    let origin = input.origin();
    let decoration_seed = random.set_decoration_seed(seed, origin.x, origin.z);
    let ctx = Ctx {
        min_gen_y: input.min_gen_y,
        gen_depth: input.gen_depth,
    };
    for ore in ores {
        random.set_feature_seed(decoration_seed, ore.index as i32, STEP_UNDERGROUND_ORES);
        place_placed_feature(random, origin, ore, input, &ctx, working);
    }
    decoration_seed
}

/// Run the whole `UNDERGROUND_ORES` decoration step for a single chunk (its
/// own origin doubling as the centre — i.e. `input.chunk_x/chunk_z` must
/// equal `input.center_x/center_z`) over an identical post-carve input,
/// returning the region field after placement. `ores` must be in step order
/// with each entry's `index` set to its position within the step's feature
/// list (matching vanilla's `setFeatureSeed` index).
///
/// This is the single-source primitive; [`apply_ore_step_3x3`] is the real
/// vanilla driver (9 of these, one per source chunk in the 3×3 neighbourhood,
/// sharing one region grid) and is what a whole-chunk parity comparison
/// should use — see this module's doc comment for why a single-source-only
/// driver under-models vanilla's `blockStateWriteRadius(1)` spill.
///
/// The returned grid is the input `grid` with ore writes applied; the caller
/// diffs it against the original to obtain the placed ores.
pub fn apply_ore_step<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    input: &OreInput<'_>,
    grid: &RegionGrid,
    ores: &[PlacedOre],
) -> RegionGrid {
    let mut working = grid.clone();
    apply_one_source(random, seed, input, ores, &mut working);
    working
}

/// The real vanilla 3×3 neighbourhood driver for one CENTRE chunk.
///
/// Vanilla's `blockStateWriteRadius(1)` at the FEATURES generation stage
/// (`ChunkPyramid.java:32-35`) means a NEIGHBOUR chunk's own ore decoration
/// (its own origin, its own `decorationSeed` — `ChunkGenerator
/// .applyBiomeDecoration` is called once per chunk, using that chunk's own
/// seed) can legitimately spill blocks into the centre. This runs the full
/// `UNDERGROUND_ORES` step for each of the 9 chunks in `center ± 1`, in turn
/// (`dx` outer `-1..=1`, `dz` inner `-1..=1`, matching
/// `crate::carver::apply_carvers`'s own source-chunk loop convention — a
/// fixed, documented iteration order, not a claim this matches real-world
/// chunk *load* order, which vanilla itself does not guarantee is
/// deterministic at boundaries), writing every result into one shared region
/// grid keyed by centre-relative local coordinates.
///
/// `ores` is the SAME list for all 9 passes — correct whenever biome does not
/// vary across the neighbourhood (true for a fixed single-biome fixture).
/// A thin wrapper over [`apply_ore_step_3x3_per_source`] for that fixed-list
/// case; composing against real per-quart biome variety (issue #295) uses
/// that function directly with a per-source ore-list closure instead.
///
/// Returns the region grid after all 9 passes; the caller diffs the CENTRE
/// 16×16 slice (`in_center`) against the original to obtain the fixture-
/// comparable `ore.*` output, and can separately read `working` at any
/// region-local coordinate to see spill into (or within) a neighbour.
#[allow(clippy::too_many_arguments)]
pub fn apply_ore_step_3x3<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    center_x: i32,
    center_z: i32,
    min_y: i32,
    height: i32,
    min_gen_y: i32,
    gen_depth: i32,
    ocean_floor_wg: &HashMap<(i32, i32), i32>,
    in_tag: &dyn Fn(&str, &str) -> bool,
    grid: &RegionGrid,
    ores: &[PlacedOre],
) -> (RegionGrid, i64) {
    apply_ore_step_3x3_per_source(
        random,
        seed,
        center_x,
        center_z,
        min_y,
        height,
        min_gen_y,
        gen_depth,
        ocean_floor_wg,
        in_tag,
        grid,
        &|_source_x, _source_z| ores,
    )
}

/// The real vanilla 3×3 neighbourhood driver, generalised to a **per-source**
/// ore list (issue #295's ore-composition increment): `ores_for_source(x, z)`
/// is called once per of the 9 source chunks (their own chunk coordinates,
/// not centre-relative) and must return that source's own biome's
/// `UNDERGROUND_ORES` list — vanilla's `ChunkGenerator.applyBiomeDecoration`
/// resolves the decorating biome per chunk, so a neighbour in a different
/// biome to the centre places (and RNG-consumes) a different feature list,
/// not the centre's own. [`apply_ore_step_3x3`] is the fixed-list special
/// case of this for a single-biome fixture.
#[allow(clippy::too_many_arguments)]
pub fn apply_ore_step_3x3_per_source<'a, R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    center_x: i32,
    center_z: i32,
    min_y: i32,
    height: i32,
    min_gen_y: i32,
    gen_depth: i32,
    ocean_floor_wg: &HashMap<(i32, i32), i32>,
    in_tag: &dyn Fn(&str, &str) -> bool,
    grid: &RegionGrid,
    ores_for_source: &dyn Fn(i32, i32) -> &'a [PlacedOre],
) -> (RegionGrid, i64) {
    let mut working = grid.clone();
    let mut center_decoration_seed = 0;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let source_x = center_x + dx;
            let source_z = center_z + dz;
            let input = OreInput {
                chunk_x: source_x,
                chunk_z: source_z,
                center_x,
                center_z,
                min_y,
                height,
                min_gen_y,
                gen_depth,
                ocean_floor_wg,
                in_tag,
            };
            let ores = ores_for_source(source_x, source_z);
            let ds = apply_one_source(random, seed, &input, ores, &mut working);
            if dx == 0 && dz == 0 {
                center_decoration_seed = ds;
            }
        }
    }
    (working, center_decoration_seed)
}

/// The `decorationSeed` for a chunk origin — exposed so tests can cross-check it
/// against the oracle's `meta.decorationSeed`.
pub fn decoration_seed<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    origin_x: i32,
    origin_z: i32,
) -> i64 {
    random.set_decoration_seed(seed, origin_x, origin_z)
}

/// Reproduce `PlacedFeature.placeWithContext`'s depth-first modifier pipeline for
/// one ore feature, calling [`place_ore_feature`] at each surviving position.
fn place_placed_feature<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    ore: &PlacedOre,
    input: &OreInput<'_>,
    ctx: &Ctx,
    working: &mut RegionGrid,
) {
    fn recurse<R: RandomSource>(
        random: &mut R,
        modifiers: &[Placement],
        i: usize,
        pos: BlockPos,
        ctx: &Ctx,
        config: &OreConfig,
        input: &OreInput<'_>,
        working: &mut RegionGrid,
    ) {
        if i == modifiers.len() {
            place_ore_feature(random, pos, config, input, working);
            return;
        }
        for next in modifiers[i].get_positions(random, pos, ctx) {
            recurse(random, modifiers, i + 1, next, ctx, config, input, working);
        }
    }
    recurse(
        random,
        &ore.placements,
        0,
        origin,
        ctx,
        &ore.config,
        input,
        working,
    );
}

/// `OreFeature.place` + `doPlace` for a single origin. Writes into `working`
/// at region-local coordinates (centre-relative, clamped beyond the driven
/// 3×3 neighbourhood — see [`OreInput::region_local`]), so a write can land
/// in a neighbour chunk exactly as vanilla's real `blockStateWriteRadius(1)`
/// spill does; the caller decides which of those matter (only the CENTRE's
/// own 16×16 is fixture-comparable — see [`OreInput::in_center`]).
pub fn place_ore_feature<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    config: &OreConfig,
    input: &OreInput<'_>,
    working: &mut RegionGrid,
) {
    let size = config.size;
    let dir = random.next_float() * std::f32::consts::PI;
    let spread_xy = size as f32 / 8.0;
    let max_radius = math::ceil(((size as f32 / 16.0 * 2.0 + 1.0) / 2.0) as f64);
    // Blob centres use real (libm) transcendentals — java.lang.Math.sin/cos.
    let x0 = origin.x as f64 + (dir as f64).sin() * spread_xy as f64;
    let x1 = origin.x as f64 - (dir as f64).sin() * spread_xy as f64;
    let z0 = origin.z as f64 + (dir as f64).cos() * spread_xy as f64;
    let z1 = origin.z as f64 - (dir as f64).cos() * spread_xy as f64;
    let y0 = (origin.y + random.next_int_bounded(3) - 2) as f64;
    let y1 = (origin.y + random.next_int_bounded(3) - 2) as f64;

    let x_start = origin.x - math::ceil(spread_xy as f64) - max_radius;
    let y_start = origin.y - 2 - max_radius;
    let z_start = origin.z - math::ceil(spread_xy as f64) - max_radius;
    let size_xz = 2 * (math::ceil(spread_xy as f64) + max_radius);

    let mut proceed = false;
    'probe: for xprobe in x_start..=x_start + size_xz {
        for zprobe in z_start..=z_start + size_xz {
            if y_start <= input.get_height(xprobe, zprobe) {
                proceed = true;
                break 'probe;
            }
        }
    }
    if !proceed {
        return;
    }
    do_place(
        random, config, input, working, x0, x1, z0, z1, y0, y1, x_start, y_start, z_start,
    );
}

#[allow(clippy::too_many_arguments)]
fn do_place<R: RandomSource>(
    random: &mut R,
    config: &OreConfig,
    input: &OreInput<'_>,
    working: &mut RegionGrid,
    x0: f64,
    x1: f64,
    z0: f64,
    z1: f64,
    y0: f64,
    y1: f64,
    x_start: i32,
    y_start: i32,
    z_start: i32,
) {
    let size = config.size;
    let mut data = vec![0.0_f64; (size * 4) as usize];
    for i in 0..size {
        let step = i as f32 / size as f32;
        let xx = math::lerp(step as f64, x0, x1);
        let yy = math::lerp(step as f64, y0, y1);
        let zz = math::lerp(step as f64, z0, z1);
        let ss = random.next_double() * size as f64 / 16.0;
        // Radius uses the Mth.sin *table* (float arg widened to double).
        let r = ((math::sin((std::f32::consts::PI * step) as f64) as f64 + 1.0) * ss + 1.0) / 2.0;
        let b = (i * 4) as usize;
        data[b] = xx;
        data[b + 1] = yy;
        data[b + 2] = zz;
        data[b + 3] = r;
    }

    for i1 in 0..size - 1 {
        let b1 = (i1 * 4) as usize;
        if data[b1 + 3] <= 0.0 {
            continue;
        }
        for i2 in i1 + 1..size {
            let b2 = (i2 * 4) as usize;
            if data[b2 + 3] <= 0.0 {
                continue;
            }
            let dx = data[b1] - data[b2];
            let dy = data[b1 + 1] - data[b2 + 1];
            let dz = data[b1 + 2] - data[b2 + 2];
            let dr = data[b1 + 3] - data[b2 + 3];
            if dr * dr > dx * dx + dy * dy + dz * dz {
                if dr > 0.0 {
                    data[b2 + 3] = -1.0;
                } else {
                    data[b1 + 3] = -1.0;
                }
            }
        }
    }

    let mut tested: HashSet<(i32, i32, i32)> = HashSet::new();
    for i in 0..size {
        let b = (i * 4) as usize;
        let r = data[b + 3];
        if r < 0.0 {
            continue;
        }
        let xx = data[b];
        let yy = data[b + 1];
        let zz = data[b + 2];
        let x_min = math::floor(xx - r).max(x_start);
        let y_min = math::floor(yy - r).max(y_start);
        let z_min = math::floor(zz - r).max(z_start);
        let x_max = math::floor(xx + r).max(x_min);
        let y_max = math::floor(yy + r).max(y_min);
        let z_max = math::floor(zz + r).max(z_min);

        for x in x_min..=x_max {
            let xd = (x as f64 + 0.5 - xx) / r;
            if xd * xd >= 1.0 {
                continue;
            }
            for y in y_min..=y_max {
                let yd = (y as f64 + 0.5 - yy) / r;
                if xd * xd + yd * yd >= 1.0 {
                    continue;
                }
                for z in z_min..=z_max {
                    let zd = (z as f64 + 0.5 - zz) / r;
                    if xd * xd + yd * yd + zd * zd >= 1.0 {
                        continue;
                    }
                    if is_outside_build_height(y, input.min_y, input.height) {
                        continue;
                    }
                    if !tested.insert((x, y, z)) {
                        continue;
                    }
                    try_place_ore(random, config, input, working, x, y, z);
                }
            }
        }
    }
}

fn is_outside_build_height(y: i32, min_y: i32, height: i32) -> bool {
    y < min_y || y >= min_y + height
}

fn try_place_ore<R: RandomSource>(
    random: &mut R,
    config: &OreConfig,
    input: &OreInput<'_>,
    working: &mut RegionGrid,
    x: i32,
    y: i32,
    z: i32,
) {
    // Writes always land somewhere in the driven 3×3 region (clamped beyond
    // it — see `OreInput::region_local`); whether this particular write is
    // in the CENTRE (and therefore fixture-comparable) is decided later by
    // the caller via `in_center`, not here — a neighbour-chunk write still
    // has to happen so later reads (isAdjacentToAir, a later source's own
    // placement) see it, exactly as vanilla's real, shared block field would.
    let (lx, lz) = input.region_local(x, z);
    let current = working.get(lx, y, lz);
    let base = current.split('[').next().unwrap_or(current).to_string();
    for target in &config.targets {
        let matches = match &target.target {
            RuleTest::TagMatch(tag) => (input.in_tag)(&base, tag),
            RuleTest::BlockMatch(block) => base == block.as_str(),
        };
        // `TargetBlockState.target.test` never draws for tag/block tests, so a
        // non-match costs nothing — exactly as vanilla loops to the next target.
        if !matches {
            continue;
        }
        // canPlaceOre: shouldSkipAirCheck (may draw a nextFloat) ? true
        // : !isAdjacentToAir.
        let place = should_skip_air_check(random, config.discard_chance_on_air_exposure)
            || !is_adjacent_to_air(input, working, x, y, z);
        if place {
            working.set(lx, y, lz, &target.state);
            return;
        }
        // canPlaceOre returned false; vanilla continues to the next target.
    }
}

/// `OreFeature.shouldSkipAirCheck`. Draws a single `nextFloat` iff
/// `0 < chance < 1`; the endpoints short-circuit with no draw.
fn should_skip_air_check<R: RandomSource>(random: &mut R, chance: f32) -> bool {
    if chance <= 0.0 {
        true
    } else if chance >= 1.0 {
        false
    } else {
        random.next_float() >= chance
    }
}

/// `Feature.isAdjacentToAir` over the six `Direction` neighbours. Reads the
/// live working grid across the whole driven 3×3 region (clamped beyond it —
/// see `OreInput::region_local`), so a read just outside the centre sees the
/// real neighbour terrain (or an in-flight write from an earlier source
/// pass), not an assumed-empty scratch chunk.
fn is_adjacent_to_air(input: &OreInput<'_>, working: &RegionGrid, x: i32, y: i32, z: i32) -> bool {
    const DIRS: [(i32, i32, i32); 6] = [
        (0, -1, 0),
        (0, 1, 0),
        (0, 0, -1),
        (0, 0, 1),
        (-1, 0, 0),
        (1, 0, 0),
    ];
    DIRS.iter().any(|&(dx, dy, dz)| {
        let block = block_at(input, working, x + dx, y + dy, z + dz);
        let base = block.split('[').next().unwrap_or(block);
        is_air(base)
    })
}

fn block_at<'a>(input: &OreInput<'_>, working: &'a RegionGrid, x: i32, y: i32, z: i32) -> &'a str {
    if is_outside_build_height(y, input.min_y, input.height) {
        return "minecraft:air";
    }
    let (lx, lz) = input.region_local(x, z);
    working.get(lx, y, lz)
}

fn is_air(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}
