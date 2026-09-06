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
//! Vanilla's own biome-decoration application derives a per-chunk `decorationSeed`
//! (its own set-decoration-seed step), then for each generation *step* and each
//! feature within it (in a globally-sorted order that, for a single biome, is the
//! biome's own list order) reseeds the RNG with its own per-feature seed derivation over
//! `(decorationSeed, featureIndexInStep, stepIndex)` and places that feature. **Each feature
//! reseeds independently**, so features are RNG-isolated from one another — a bug
//! in one cannot desynchronise the next.
//!
//! Within one placed feature, vanilla composes *placement modifiers* as a lazy
//! stream pipeline: the origin flat-mapped through each modifier in
//! turn, terminating in a feature-place call per surviving position. Vanilla's own flat-map
//! is depth-first, so the draw order is: modifier 0 emits its positions (drawing
//! as it goes), then for *each* of those, modifier 1 runs fully (including the
//! eventual place call), and so on. [`place_ore_feature`] reproduces that exact
//! nesting with a recursion, keeping every modifier a separate composable unit
//! (no fused loops).
//!
//! ## Ore placement RNG order (per emitted position)
//!
//! Vanilla's own ore-feature place draws, in order: `nextFloat()` (blob direction),
//! `nextInt(3)`, `nextInt(3)` (the two y-endpoints). It then reads the
//! `OCEAN_FLOOR_WG` heightmap around the origin (no draws); **only if** some probe
//! is at/below the blob does it proceed to its own inner place step, which draws `nextDouble()`
//! once per `i in 0..size` (the blob radii). Vanilla's own "can place ore" check draws nothing for a
//! *non-buried* ore (`discard_chance_on_air_exposure == 0` makes
//! its own "should skip air check" short-circuit true), but a **buried** ore
//! (`0 < discard < 1`, e.g. `ore_gold_buried`, `ore_coal_buried`) draws a single
//! `nextFloat()` in that "should skip air check" step for *each candidate position whose block
//! matches a target tag* — so its per-feature draw count is position-dependent.
//! `discard >= 1` (e.g. `ore_diamond_buried`) draws nothing and places only when
//! fully enclosed. The `tag_match` `RuleTest` itself never draws.
//!
//! Note the deliberate float typing: the blob *centres* use the standard
//! library's real `sin`/`cos` (real `f64` transcendentals), while the per-step *radius* uses
//! vanilla's own math-helper sine (the 65536-entry table, [`crate::math::sin`]). Getting these
//! swapped produces a plausible-but-wrong world, which is why the oracle compares
//! whole-chunk block output rather than sampled positions.

use std::cell::Cell;
use std::collections::HashMap;

use serde_json::Value;

use crate::math;
use crate::rng::{RandomSource, WorldgenRandom};

use self::region_view::RegionView;

/// Grass/flower/tree placement — a separate engine from the
/// rest of this module (ores), sharing only [`BlockPos`]/[`IntProvider`]/
/// [`canon_state`] and the [`STEP_VEGETAL_DECORATION`] constant below. See
/// its own module doc for scope and named gaps.
pub mod vegetation;

/// The **in-place** decoration medium (Unit 7 of
/// `docs/plans/worldgen-rewrite.md`): [`region_view::RegionView`], a read/write
/// surface over the 3×3 neighbourhood's own grids that replaced the stitched
/// `48 × height × 48` copies this module's ore driver and
/// [`vegetation`]'s own driver both used to be handed. See its module doc for
/// the coordinate-space trap it inherits.
pub mod region_view;

/// `TOP_LAYER_MODIFICATION` — snow layers and surface ice,
/// vanilla's `freeze_top_layer`. A third engine again: it consumes no RNG, never
/// writes outside its own chunk, and reads per-block-state facts (collision
/// UP-face fullness, fluid presence) that neither the ore nor the vegetation
/// engine needs. See its own module doc.
pub mod top_layer;

/// Event counters for this module's own placement loops — the instrument
/// DESIGN.md §12.143 asked for before anything touched the `ore` stage. Inert
/// unless the `gen-counters` feature is on.
pub mod ore_probe;

/// `GenerationStep.Decoration.UNDERGROUND_ORES.ordinal()`.
pub const STEP_UNDERGROUND_ORES: i32 = 6;

/// `GenerationStep.Decoration.VEGETAL_DECORATION.ordinal()` — grass, flowers
/// and trees. One past `UNDERGROUND_DECORATION`/`FLUID_SPRINGS`
/// (7, 8), which this engine does not compose; see
/// vanilla's own decoration-step enum's
/// declaration order (`RAW_GENERATION, LAKES, LOCAL_MODIFICATIONS,
/// UNDERGROUND_STRUCTURES, SURFACE_STRUCTURES, STRONGHOLDS, UNDERGROUND_ORES,
/// UNDERGROUND_DECORATION, FLUID_SPRINGS, VEGETAL_DECORATION,
/// TOP_LAYER_MODIFICATION`).
pub const STEP_VEGETAL_DECORATION: i32 = 9;

/// A block position with `i32` components (vanilla's own block-position record).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Vanilla's own vertical-anchor type.
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

    /// Non-panicking sibling of [`Self::parse`] — see
    /// [`HeightProvider::try_parse`] for why the decoration engine needs one.
    fn try_parse(v: &Value) -> Option<Self> {
        if let Some(y) = v.get("absolute") {
            Some(VerticalAnchor::Absolute(y.as_i64()? as i32))
        } else if let Some(o) = v.get("above_bottom") {
            Some(VerticalAnchor::AboveBottom(o.as_i64()? as i32))
        } else if let Some(o) = v.get("below_top") {
            Some(VerticalAnchor::BelowTop(o.as_i64()? as i32))
        } else {
            None
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

/// Vanilla's own int-provider type (the subset features use).
///
/// [`IntProvider::WeightedList`] (used by `trees_plains`/`trees_birch`/
/// `trees_taiga` outer `count` — e.g. `{data: 0, weight: 19}, {data: 1,
/// weight: 1}`) is additive: nothing in the ore engine
/// constructs it, so this is a strict superset, not a behaviour change to
/// any existing caller.
#[derive(Clone, Debug)]
pub enum IntProvider {
    Constant(i32),
    Uniform { min: i32, max: i32 },
    /// Vanilla's own weighted-list int provider — a `WeightedList<Integer>`.
    /// `(value, weight)` pairs, in JSON declaration order (order doesn't
    /// affect the *distribution*, but does affect [`WeightedList::sample`]'s
    /// draw semantics, which walks the list in order — see that fn's doc).
    WeightedList(Vec<(i32, i32)>),
    /// A weighted choice among nested integer providers.
    WeightedProviders(Vec<(Box<IntProvider>, i32)>),
    /// Vanilla's own biased-to-bottom int provider — used by
    /// `cactus`/`sugar_cane`'s block-column-feature layer heights (the
    /// cacti/sugar-cane increment). Additive: nothing before that
    /// increment constructs this variant.
    BiasedToBottom { min: i32, max: i32 },
    /// Vanilla's own trapezoid int provider, the REAL
    /// (two-draw, triangular) sample — not the `Uniform` approximation
    /// `crate::feature::vegetation::try_parse_int_provider` used to fold
    /// this into. That approximation preserved mean and support but not
    /// **draw count**: vanilla's symmetric case
    /// (`min == -max, plateau == 0`, which is every `random_offset` this
    /// crate's vegetation engine actually uses) draws `nextInt` TWICE and
    /// subtracts, while `Uniform` draws once — every RNG call after the
    /// first desyncs completely from vanilla's own stream. Found via
    /// `tests/vegetation_parity.rs` (real-oracle evidence that surfaced
    /// this gap): `patch_grass_plain`'s placed positions were disjoint,
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

    pub(crate) fn sample<R: RandomSource>(&self, random: &mut R) -> i32 {
        match self {
            IntProvider::Constant(v) => *v,
            IntProvider::Uniform { min, max } => {
                math::random_between_inclusive(random, *min, *max)
            }
            // Vanilla's own weighted-list random pick: walk in declared order, subtracting a
            // `nextInt(totalWeight)` draw until it goes negative — the entry
            // it goes negative on is the pick. Matches
            // vanilla's own simple-weighted-random-list's walk
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
            IntProvider::WeightedProviders(entries) => {
                let total: i32 = entries.iter().map(|(_, weight)| *weight).sum();
                let mut roll = random.next_int_bounded(total.max(1));
                for (provider, weight) in entries {
                    roll -= *weight;
                    if roll < 0 {
                        return provider.sample(random);
                    }
                }
                entries.last().map_or(0, |(provider, _)| provider.sample(random))
            }
            // Vanilla's own biased-to-bottom int provider's sample: `minInclusive + nextInt(nextInt(maxInclusive
            // - minInclusive + 1) + 1)` — two nested, dependent draws, not one.
            IntProvider::BiasedToBottom { min, max } => {
                let n = *max - *min + 1;
                let inner = random.next_int_bounded(n);
                min + random.next_int_bounded(inner + 1)
            }
            // Vanilla's own trapezoid int provider's sample, ported exactly (see this variant's own
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

/// Vanilla's own height-provider type
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
    /// Vanilla's own very-biased-to-bottom height provider — three chained bounded-int draws, not one.
    /// Added because two bundled placed features use it in a
    /// *decoration* step (nothing in the ore step does, which is why
    /// [`HeightProvider::parse`]'s `panic!` never fired on it).
    VeryBiasedToBottom {
        min: VerticalAnchor,
        max: VerticalAnchor,
        inner: i32,
    },
}

impl HeightProvider {
    /// The non-panicking sibling of [`Self::parse`], for the vegetal/decoration
    /// engine, whose blanket rule is that unparseable data degrades a feature to
    /// `Unsupported` rather than taking down world generation for every biome.
    /// See `crate::feature::vegetation`'s module doc.
    pub(crate) fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        let min = VerticalAnchor::try_parse(&v["min_inclusive"])?;
        let max = VerticalAnchor::try_parse(&v["max_inclusive"])?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "uniform" => Some(HeightProvider::Uniform { min, max }),
            "trapezoid" => Some(HeightProvider::Trapezoid {
                min,
                max,
                plateau: v["plateau"].as_i64().unwrap_or(0) as i32,
            }),
            "very_biased_to_bottom" => Some(HeightProvider::VeryBiasedToBottom {
                min,
                max,
                inner: v["inner"].as_i64().unwrap_or(1) as i32,
            }),
            _ => None,
        }
    }

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

    /// Mirrors vanilla's own uniform-height/trapezoid-height sample methods, including their
    /// exact random-between-inclusive draw counts.
    pub(crate) fn sample<R: RandomSource>(
        &self,
        random: &mut R,
        min_gen_y: i32,
        gen_depth: i32,
    ) -> i32 {
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
            HeightProvider::VeryBiasedToBottom { min, max, inner } => {
                let lo = min.resolve_y(min_gen_y, gen_depth);
                let hi = max.resolve_y(min_gen_y, gen_depth);
                if hi - lo - inner + 1 <= 0 {
                    return lo;
                }
                let upper = math::random_between_inclusive(random, lo + inner, hi);
                let biased_upper = math::random_between_inclusive(random, lo, upper - 1);
                math::random_between_inclusive(random, lo, biased_upper - 1 + inner)
            }
        }
    }
}

/// Vanilla's own placement-modifier type (ore subset).
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

    /// Vanilla's own placement-modifier get-positions: emit the positions this modifier
    /// produces for one incoming position, drawing RNG exactly as vanilla does.
    ///
    /// Returns an [`OrePositions`] rather than a `Vec<BlockPos>`; see that
    /// type's doc for why the three shapes are exhaustive over what this
    /// function could previously produce, and for the measurement.
    fn get_positions<R: RandomSource>(
        &self,
        random: &mut R,
        pos: BlockPos,
        ctx: &Ctx,
    ) -> OrePositions {
        match self {
            Placement::Count(count) => {
                let n = count.sample(random);
                OrePositions::Repeat(pos, n)
            }
            Placement::RarityFilter(chance) => {
                // Vanilla's own rarity-filter "should place": nextFloat() < 1/chance.
                if random.next_float() < 1.0 / *chance as f32 {
                    OrePositions::One(pos)
                } else {
                    OrePositions::None
                }
            }
            Placement::InSquare => {
                let x = random.next_int_bounded(16) + pos.x;
                let z = random.next_int_bounded(16) + pos.z;
                OrePositions::One(BlockPos { x, y: pos.y, z })
            }
            Placement::HeightRange(h) => {
                let y = h.sample(random, ctx.min_gen_y, ctx.gen_depth);
                OrePositions::One(BlockPos {
                    x: pos.x,
                    y,
                    z: pos.z,
                })
            }
            // Vanilla's own biome filter for a single-biome world always keeps the position and
            // draws nothing (topFeature is in the biome by construction).
            Placement::Biome => OrePositions::One(pos),
        }
    }
}

/// What one ore [`Placement`] modifier emits for one incoming position — the
/// allocation-free replacement for the `Vec<BlockPos>` it used to return.
///
/// # Why exactly three shapes, and why that is not a narrowing
///
/// Enumerating every arm of [`Placement::get_positions`] shows the returned
/// `Vec` only ever had one of three shapes, so this enum is exhaustive over what
/// the old code could produce rather than a subset of it:
///
/// | arm | old | new |
/// |---|---|---|
/// | `Count` | `vec![pos; n.max(0)]` | [`OrePositions::Repeat`] |
/// | `InSquare`, `HeightRange`, `Biome` | `vec![one]` | [`OrePositions::One`] |
/// | `RarityFilter` | `vec![pos]` or `Vec::new()` | [`OrePositions::One`] / [`OrePositions::None`] |
///
/// **No modifier in the `UNDERGROUND_ORES` subset returns two *different*
/// positions.** If one is ever added, it does **not** get to smuggle itself in
/// as a `Repeat` — add a variant and handle it in [`place_placed_feature`]'s
/// walk, because `Repeat`'s consumer recurses `n` times on the *same* position,
/// which is precisely what `vec![pos; n]` meant and is not what a fan-out means.
///
/// This is [`vegetation::config::Positions`]' shape, applied to the ore engine.
/// U8 did it for vegetation and took a warm column from 20,678 allocations to
/// 87; the ore engine kept returning a `Vec` per modifier per attempt, and U18
/// measured that as **79.96%** of the ore stage's 207,671 allocations on a 3×3
/// cold sweep — cross-checked by an unsampled size histogram in which **78.62%**
/// of ore allocations are exactly 12 bytes, `size_of::<BlockPos>()`. Nine full
/// passes per chunk multiply it (see [`apply_ore_step_3x3_per_source`]).
///
/// # Why this cannot move an RNG draw
///
/// The draw still happens inside [`Placement::get_positions`], before this value
/// is returned, so the consumption order is byte-identical: the driver's
/// depth-first `recurse` walks `Repeat`'s `n` copies in the same order
/// `for next in vec` did. `docs/plans/worldgen-rewrite.md` names breadth-first
/// "optimisation" of this exact recursion as instant desync — the walk below
/// never touches the recursion's shape, only what it iterates.
///
/// `Repeat`'s `n` is stored **unclamped**, exactly as `IntProvider::sample`
/// returned it, and the consumer's `0..n` range is empty for `n <= 0` — the same
/// set `vec![pos; n.max(0) as usize]` produced. Clamping here instead would be
/// equivalent; not clamping keeps the value the sample actually produced visible
/// to a reader comparing against vanilla.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrePositions {
    /// The modifier filtered this position out.
    None,
    /// Exactly one position (possibly moved from the input).
    One(BlockPos),
    /// `n` copies of one position — `Count`'s `vec![pos; n]`. `n <= 0` means
    /// none.
    Repeat(BlockPos, i32),
}

/// Vanilla's own rule-test type (the
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

/// One vanilla ore-configuration target-block-state: the block to place and the test the
/// existing block must pass.
#[derive(Clone, Debug)]
pub struct OreTarget {
    /// Canonicalised placed state (e.g. `minecraft:redstone_ore[lit=false]`).
    pub state: String,
    pub target: RuleTest,
}

/// Vanilla's own ore-configuration type.
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

/// A single dense grid spanning the whole driven 3×3 region, addressed by
/// `(local_x, y, local_z)` centre-relative with `y` absolute — **the parity
/// fixtures' shape, no longer production's.**
///
/// An earlier change made this a [`crate::dense_grid::DenseBlockGrid`] instead of a
/// `HashMap<(i32,i32,i32), String>`, and Unit 3 gave that grid interned ids. What
/// neither could remove is that materialising the region at all means **copying
/// all nine already-computed source chunks into it** — 884,736 cells per
/// `column()` call for the ore driver, plus another 884,736 for the `clone()` the
/// driver itself performed, every one of them warm. Unit 7 deleted both by
/// routing reads at their source instead: production now drives the ore engine
/// through [`region_view::RegionView`], which borrows the nine grids and holds
/// writes in a sparse overlay.
///
/// This alias survives because a *fixture* is naturally one sparse map over the
/// whole region rather than nine per-chunk fields, and
/// [`region_view::RegionView::over_region_grid`] adapts exactly this shape onto
/// the same read path production uses — so the JVM fixtures still exercise the
/// routing rather than a second implementation of it.
pub type RegionGrid = crate::dense_grid::DenseBlockGrid;

/// Centre-relative local coordinate lower/upper (exclusive) bound the 3×3
/// driver ([`apply_ore_step_3x3`]) reads and writes over: one 16-wide band
/// per row/column of the 3×3 chunk grid (`-16..0` the west/north neighbour,
/// `0..16` the centre, `16..32` the east/south neighbour).
pub const REGION_MIN: i32 = -16;
pub const REGION_MAX: i32 = 32;

/// Centre-relative local bounds of ore placement's **read** context. Ores still
/// run and write only for the inner 3×3 sources, but an outer source can probe
/// terrain one chunk farther out while deciding whether a boundary blob is
/// exposed to air.
pub const ORE_READ_MIN: i32 = REGION_MIN - 16;
pub const ORE_READ_MAX: i32 = REGION_MAX + 16;
pub const ORE_READ_SIDE: i32 = ORE_READ_MAX - ORE_READ_MIN;

/// Additional padding beyond [`REGION_MIN`]/[`REGION_MAX`] for the vegetation
/// grid, so a placed tree spilling past the 3×3 neighbourhood's edge is kept
/// rather than silently dropped. The worst case is a giant jungle tree whose
/// canopy touches 15×15 at the trunk; an 8-block pad on every side of a 48-block
/// region gives a 64×64 footprint — enough for any 26.2 tree, including a 2×2
/// dark oak whose 6×6 canopy at a source-chunk corner reaches 22 blocks from the
/// centre. Ore has its own wider chunk-aligned read context because a boundary
/// blob can probe terrain outside the source chunks it is allowed to decorate.
pub const VEG_PADDING: i32 = 8;

/// Side of the driven 3×3 region in blocks (48) — [`REGION_MAX`] − [`REGION_MIN`].
pub const REGION_SIDE: i32 = REGION_MAX - REGION_MIN;

/// The `OCEAN_FLOOR_WG` heightmap over ore's 5×5 **read** context as a dense
/// array, addressed by a centre-relative local `(lx, lz)` already clamped into
/// `[`[`ORE_READ_MIN`]`, `[`ORE_READ_MAX`]`).
///
/// # Why this is not a `HashMap`
///
/// It was one until U15. `OreInput::get_height` is called once per cell of
/// `place_ore_feature`'s pre-placement probe loop, whose box is
/// `(2·(ceil(size/8) + max_radius) + 1)²` — up to 27 × 27 for the `size = 64`
/// blob ores — for **every** emitted position of **every** ore feature of
/// **all nine** source chunks. Each of those was a SipHash of an `(i32, i32)`
/// tuple against a 6,400-entry map.
///
/// Measured (`samply` 0.13.1, release, `threadCPUDelta`-weighted,
/// `bench_ore_composition_sweep`, seed 42): inside the ore engine's own subtree
/// (`apply_one_source`, 22.85% of process CPU), `hash_one::<&(i32, i32)>` was
/// **7.36% inclusive / 2.78% self** and `get_height` itself a further 2.95%
/// self. `overworld/decorate.rs`'s own doc had already named this fix — "if it
/// ever matters, the win is a dense `[i32; 80 * 80]` array rather than a
/// `HashMap`, and the clamp has to move with it" — and the clamp did move with
/// it: it stays in [`OreInput::region_local`], and this type's accessors assume
/// their caller already applied it.
///
/// # The absence detector is preserved deliberately
///
/// The map version panicked on a probe with no entry, which is a real detector:
/// a driver that forgot to stitch a source's heights would otherwise silently
/// read 0 and change where ores place. So the dense array is filled with
/// [`Self::UNSET`] and [`Self::get`] panics on it, rather than defaulting.
#[derive(Clone)]
pub struct RegionHeights {
    /// `ORE_READ_SIDE × ORE_READ_SIDE`, row-major in `lz`.
    heights: Box<[i32]>,
}

impl std::fmt::Debug for RegionHeights {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self.heights.iter().filter(|&&v| v != Self::UNSET).count();
        f.debug_struct("RegionHeights")
            .field("cells", &self.heights.len())
            .field("set", &set)
            .finish()
    }
}

impl Default for RegionHeights {
    fn default() -> Self {
        Self::unset()
    }
}

impl RegionHeights {
    /// Sentinel for "no source stitched this column", standing in for the
    /// `HashMap`'s absent key. Not a reachable height: every real value is at
    /// least `min_y - 1`.
    pub const UNSET: i32 = i32::MIN;

    /// Number of columns in ore's 5×5 read context (80 × 80 = 6,400).
    pub const AREA: usize = (ORE_READ_SIDE * ORE_READ_SIDE) as usize;

    /// An all-[`Self::UNSET`] map — nothing stitched yet.
    #[must_use]
    pub fn unset() -> Self {
        Self {
            heights: vec![Self::UNSET; Self::AREA].into_boxed_slice(),
        }
    }

    #[inline]
    fn index(lx: i32, lz: i32) -> usize {
        debug_assert!((ORE_READ_MIN..ORE_READ_MAX).contains(&lx), "lx {lx} not clamped");
        debug_assert!((ORE_READ_MIN..ORE_READ_MAX).contains(&lz), "lz {lz} not clamped");
        ((lz - ORE_READ_MIN) * ORE_READ_SIDE + (lx - ORE_READ_MIN)) as usize
    }

    /// Records one column's height. `(lx, lz)` must already be inside the
    /// region; a coordinate outside it is a caller bug, not a silent drop —
    /// the `HashMap` this replaced would have stored it and then never been
    /// asked for it, which is the shape that hides a stitching mistake.
    pub fn set(&mut self, lx: i32, lz: i32, y: i32) {
        self.heights[Self::index(lx, lz)] = y;
    }

    /// Height at a **pre-clamped** region-local column.
    ///
    /// # Panics
    ///
    /// If no source stitched this column — same failure the `HashMap`'s
    /// `unwrap_or_else(|| panic!(…))` produced, kept so the detector survives.
    #[must_use]
    #[inline]
    pub fn get(&self, lx: i32, lz: i32) -> i32 {
        let v = self.heights[Self::index(lx, lz)];
        assert_ne!(
            v,
            Self::UNSET,
            "RegionHeights::get: no heightmap entry for region-local ({lx},{lz})"
        );
        v
    }

    /// Builds one from the `HashMap<(i32, i32), i32>` shape the JVM parity
    /// fixtures assemble (see [`apply_ore_step_3x3`]). Production never goes
    /// through a map at all — `overworld::decorate` fills this type directly.
    ///
    /// Entries outside the driven region are dropped, matching the map version:
    /// every read is clamped into the region first, so an out-of-region entry
    /// was unreachable there too.
    #[must_use]
    pub fn from_map(map: &HashMap<(i32, i32), i32>) -> Self {
        let mut out = Self::unset();
        for (&(lx, lz), &y) in map {
            if (ORE_READ_MIN..ORE_READ_MAX).contains(&lx) && (ORE_READ_MIN..ORE_READ_MAX).contains(&lz) {
                out.set(lx, lz, y);
            }
        }
        out
    }
}

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
    /// one-chunk-into-neighbours write spill at the FEATURES stage).
    pub center_x: i32,
    pub center_z: i32,
    pub min_y: i32,
    pub height: i32,
    /// Vanilla's own min-gen-y / gen-depth accessors for `VerticalAnchor` resolution.
    pub min_gen_y: i32,
    pub gen_depth: i32,
    /// Centre-relative bounds of the terrain and heightmap read context. The
    /// 3×3 fixture driver retains [`REGION_MIN`]..[`REGION_MAX`]; production
    /// widens this to [`ORE_READ_MIN`]..[`ORE_READ_MAX`] for boundary probes.
    pub read_min: i32,
    pub read_max: i32,
    /// `OCEAN_FLOOR_WG` heightmap across the supplied read context.
    pub ocean_floor_wg: &'a RegionHeights,
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
            .field("read_min", &self.read_min)
            .field("read_max", &self.read_max)
            .field("ocean_floor_wg", &self.ocean_floor_wg)
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

    /// Centre-relative local coordinates, clamped into this driver's supplied
    /// read context.
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
        let lx = (x - self.center_x * 16).clamp(self.read_min, self.read_max - 1);
        let lz = (z - self.center_z * 16).clamp(self.read_min, self.read_max - 1);
        (lx, lz)
    }

    /// `level.getHeight(OCEAN_FLOOR_WG, x, z)` for a probe anywhere in (or
    /// clamped into) the driven region. One array index since U15 — see
    /// [`RegionHeights`] for the measurement that motivated it and for why the
    /// missing-entry panic is still here.
    #[inline]
    fn get_height(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.region_local(x, z);
        self.ocean_floor_wg.get(lx, lz)
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
    working: &mut RegionView<'_>,
) -> i64 {
    ore_probe::bump_source_pass(1);
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
/// Writes land in `view`'s own overlay, so the caller reads the placed ores back
/// off the view (or folds them into the one grid it owns) rather than diffing two
/// full copies of the region — see [`region_view`]'s module doc.
pub fn apply_ore_step<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    input: &OreInput<'_>,
    view: &mut RegionView<'_>,
    ores: &[PlacedOre],
) -> i64 {
    apply_one_source(random, seed, input, ores, view)
}

/// The real vanilla 3×3 neighbourhood driver for one CENTRE chunk.
///
/// Vanilla's own one-chunk-into-neighbours write spill at the FEATURES generation stage
/// means a NEIGHBOUR chunk's own ore decoration
/// (its own origin, its own `decorationSeed` — vanilla's own
/// biome-decoration application is called once per chunk, using that chunk's own
/// seed) can legitimately spill blocks into the centre. This runs the full
/// underground-ore decoration step for each of the 9 chunks in `center ± 1`, in turn
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
/// case; composing against real per-quart biome variety uses
/// that function directly with a per-source ore-list closure instead.
///
/// Returns the CENTRE pass's own decoration seed. All 9 passes' writes are in
/// `view`, which the caller reads at any region-local coordinate — the centre
/// 16×16 slice (`in_center`) for the fixture-comparable `ore.*` output, or a
/// neighbour's third to see spill into (or within) a neighbour.
///
/// Takes the heightmap as the sparse `HashMap` a JVM parity fixture naturally
/// parses (`ofh.<x>,<z>` lines) and converts it to the dense
/// [`RegionHeights`] production builds directly, so the fixtures keep their
/// shape while the driver has exactly one heightmap representation to read.
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
    view: &mut RegionView<'_>,
    ores: &[PlacedOre],
) -> i64 {
    let dense = RegionHeights::from_map(ocean_floor_wg);
    apply_ore_step_3x3_per_source(
        random,
        seed,
        center_x,
        center_z,
        min_y,
        height,
        min_gen_y,
        gen_depth,
        REGION_MIN,
        REGION_MAX,
        &dense,
        in_tag,
        view,
        &|_source_x, _source_z| ores,
    )
}

/// The real vanilla 3×3 neighbourhood driver, generalised to a **per-source**
/// ore list: `ores_for_source(x, z)`
/// is called once per of the 9 source chunks (their own chunk coordinates,
/// not centre-relative) and must return that source's own biome's
/// underground-ore list — vanilla's own biome-decoration application
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
    read_min: i32,
    read_max: i32,
    ocean_floor_wg: &RegionHeights,
    in_tag: &dyn Fn(&str, &str) -> bool,
    view: &mut RegionView<'_>,
    ores_for_source: &dyn Fn(i32, i32) -> &'a [PlacedOre],
) -> i64 {
    let mut center_decoration_seed = 0;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let source_x = center_x + dx;
            let source_z = center_z + dz;
            random.begin_decoration_source();
            let input = OreInput {
                chunk_x: source_x,
                chunk_z: source_z,
                center_x,
                center_z,
                min_y,
                height,
                min_gen_y,
                gen_depth,
                read_min,
                read_max,
                ocean_floor_wg,
                in_tag,
            };
            let ores = ores_for_source(source_x, source_z);
            let ds = apply_one_source(random, seed, &input, ores, view);
            if dx == 0 && dz == 0 {
                center_decoration_seed = ds;
            }
        }
    }
    center_decoration_seed
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

/// Reproduce vanilla's own placed-feature place-with-context's depth-first modifier pipeline for
/// one ore feature, calling [`place_ore_feature`] at each surviving position.
fn place_placed_feature<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    ore: &PlacedOre,
    input: &OreInput<'_>,
    ctx: &Ctx,
    working: &mut RegionView<'_>,
) {
    fn recurse<R: RandomSource>(
        random: &mut R,
        modifiers: &[Placement],
        i: usize,
        pos: BlockPos,
        ctx: &Ctx,
        config: &OreConfig,
        input: &OreInput<'_>,
        working: &mut RegionView<'_>,
    ) {
        if i == modifiers.len() {
            place_ore_feature(random, pos, config, input, working);
            return;
        }
        // Depth-first, exactly as `for next in vec` was: `One` recurses once,
        // `Repeat(p, n)` recurses `n` times on the same position (what
        // `vec![p; n]` meant), `None` not at all. An `n <= 0` range is empty,
        // matching the old `n.max(0) as usize` length.
        match modifiers[i].get_positions(random, pos, ctx) {
            OrePositions::None => {}
            OrePositions::One(next) => {
                recurse(random, modifiers, i + 1, next, ctx, config, input, working);
            }
            OrePositions::Repeat(next, n) => {
                for _ in 0..n {
                    recurse(random, modifiers, i + 1, next, ctx, config, input, working);
                }
            }
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

/// Vanilla's own ore-feature place plus its inner place step, for a single
/// origin. Writes into `working`
/// at region-local coordinates (centre-relative, clamped beyond the driven
/// 3×3 neighbourhood — see [`OreInput::region_local`]), so a write can land
/// in a neighbour chunk exactly as vanilla's real one-chunk-into-neighbours
/// spill does; the caller decides which of those matter (only the CENTRE's
/// own 16×16 is fixture-comparable — see [`OreInput::in_center`]).
pub fn place_ore_feature<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    config: &OreConfig,
    input: &OreInput<'_>,
    working: &mut RegionView<'_>,
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

    ore_probe::bump_feature(1);
    let mut proceed = false;
    let mut probes = 0u64;
    'probe: for xprobe in x_start..=x_start + size_xz {
        for zprobe in z_start..=z_start + size_xz {
            probes += 1;
            if y_start <= input.get_height(xprobe, zprobe) {
                proceed = true;
                break 'probe;
            }
        }
    }
    ore_probe::bump_height_probes(probes);
    if !proceed {
        ore_probe::bump_feature_culled(1);
        return;
    }
    do_place(
        random, config, input, working, x0, x1, z0, z1, y0, y1, x_start, y_start, z_start,
    );
}

thread_local! {
    /// [`do_place`]'s per-blob sphere table (`size * 4` f64s: x, y, z, r) and
    /// [`VisitedBox`]'s bitset words, reused across blobs.
    ///
    /// # Why thread-local, and why taken-and-returned
    ///
    /// These were `vec![0.0; size * 4]` and `vec![0u64; cells / 64]`, one of each
    /// per `do_place` call, i.e. per blob of every ore feature of all nine source
    /// chunks. U18 measured them at **9.65%** and **10.21%** of the ore stage's
    /// 207,671 allocations on a 3×3 cold sweep — the whole non-`OrePositions`
    /// remainder. The size histogram identifies them independently: 2,048 bytes
    /// is `size = 64` (`64 * 4 * 8`) and 1,056 bytes is `size = 33`.
    ///
    /// Thread-local rather than a field on a scratch struct threaded through the
    /// call graph, because [`place_ore_feature`] and [`apply_ore_step`] are `pub`
    /// and the parity fixtures call them directly — a signature change would
    /// churn files this unit does not own for no measured gain.
    ///
    /// **Taken and returned, not borrowed across the body**, matching U8's
    /// precedent in `feature/vegetation/`: a `Cell::take` leaves an empty `Vec`
    /// behind, so a hypothetical re-entrant call gets a correct (merely
    /// allocating) fresh buffer rather than a `RefCell` panic or, worse, a shared
    /// one. `do_place` is not re-entrant today — `try_place_ore` reaches only
    /// `should_skip_air_check` and `is_adjacent_to_air` — and this pattern means
    /// that fact is not load-bearing.
    ///
    /// A panic inside the body loses the buffer to the empty default, which costs
    /// one allocation on the next call and nothing else. There is deliberately no
    /// shared pool: `docs/worldgen-in-place-decoration.md` makes "no shared pool
    /// here, ever" a change rule for U6's store, and a per-thread free-list is
    /// what that rule permits.
    static BLOB_DATA: Cell<Vec<f64>> = const { Cell::new(Vec::new()) };
    /// [`VisitedBox`]'s bitset words. See [`BLOB_DATA`].
    static VISITED_BITS: Cell<Vec<u64>> = const { Cell::new(Vec::new()) };
}

#[allow(clippy::too_many_arguments)]
fn do_place<R: RandomSource>(
    random: &mut R,
    config: &OreConfig,
    input: &OreInput<'_>,
    working: &mut RegionView<'_>,
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
    ore_probe::bump_blob(1);
    ore_probe::bump_spheres(size.max(0) as u64);
    // Reused scratch, not a fresh allocation per blob — see `BLOB_DATA`.
    // `clear` + `resize` reproduces `vec![0.0; size * 4]` exactly (every element
    // is zeroed and the length is the same); the first loop below then writes
    // all `size * 4` of them before any is read, so the zeroing is belt and
    // braces rather than load-bearing.
    let mut data = BLOB_DATA.take();
    data.clear();
    data.resize((size * 4) as usize, 0.0_f64);
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

    // Vanilla's own ore-feature inner place step's bit-set of already-tested positions. Vanilla keys
    // it by a flat index into the blob's own box; until U15 this was a
    // `HashSet<(i32, i32, i32)>`, which meant a SipHash per candidate position
    // plus a chain of `RawTable::reserve_rehash` calls as it grew — measured
    // (`samply`, release, `threadCPUDelta`) at 3.85% self in
    // `hash_one::<&(i32, i32, i32)>` and a further 3.04% self across two
    // `reserve_rehash` instantiations, inside an ore subtree that is itself
    // 22.85% of process CPU.
    //
    // The box is derived by a **first pass over the very same per-sphere bound
    // expressions the placement loop below uses**, rather than from an
    // algebraic bound on `size`/`spread_xy`/`max_radius`. That is deliberate:
    // an algebraic bound would be a second derivation to keep in step, and
    // getting it one block small would silently drop a dedup and place an extra
    // ore. Derived this way the box is exactly the set of coordinates the loop
    // can visit, so `VisitedBox` cannot be too small by construction.
    let mut tested =
        VisitedBox::over_spheres(VISITED_BITS.take(), &data, size, x_start, y_start, z_start);
    // Per-blob, deliberately: see [`TargetCache`]'s "how to change it". One blob is
    // one `OreConfig`, so the config is not in the key, and a stack-local cache is
    // what makes that true by construction.
    let mut cache = TargetCache::empty();
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
                    if !tested.insert(x, y, z) {
                        ore_probe::bump_candidate_deduped(1);
                        continue;
                    }
                    ore_probe::bump_candidate(1);
                    try_place_ore(random, config, input, working, x, y, z, &mut cache);
                }
            }
        }
    }

    // Hand both scratch buffers back for the next blob. Reached on every normal
    // exit; a panic in the body loses them to the empty default, which costs one
    // allocation next call. See `BLOB_DATA`.
    VISITED_BITS.set(tested.into_bits());
    BLOB_DATA.set(data);
}

/// The set of block positions one `doPlace` blob has already tested, as a dense
/// bitset over the blob's own bounding box.
///
/// Replaces a `HashSet<(i32, i32, i32)>` — see [`do_place`]'s comment for the
/// measurement. `insert` returns whether the position was **newly** inserted,
/// exactly like `HashSet::insert`, so the call site is unchanged in meaning.
struct VisitedBox {
    x0: i32,
    y0: i32,
    z0: i32,
    xs: i32,
    ys: i32,
    zs: i32,
    bits: Vec<u64>,
}

impl VisitedBox {
    /// The union of every live sphere's clamped bound box, computed with the
    /// **same expressions** `do_place`'s placement loop uses. A sphere with
    /// `r < 0` is skipped there and so is skipped here.
    ///
    /// `bits` is a reused buffer taken from [`VISITED_BITS`]; it is cleared and
    /// resized here, so its incoming contents and length are irrelevant. It is
    /// handed back by [`Self::into_bits`].
    fn over_spheres(
        mut bits: Vec<u64>,
        data: &[f64],
        size: i32,
        x_start: i32,
        y_start: i32,
        z_start: i32,
    ) -> Self {
        let (mut lo_x, mut lo_y, mut lo_z) = (i32::MAX, i32::MAX, i32::MAX);
        let (mut hi_x, mut hi_y, mut hi_z) = (i32::MIN, i32::MIN, i32::MIN);
        for i in 0..size {
            let b = (i * 4) as usize;
            let r = data[b + 3];
            if r < 0.0 {
                continue;
            }
            let (xx, yy, zz) = (data[b], data[b + 1], data[b + 2]);
            let x_min = math::floor(xx - r).max(x_start);
            let y_min = math::floor(yy - r).max(y_start);
            let z_min = math::floor(zz - r).max(z_start);
            lo_x = lo_x.min(x_min);
            lo_y = lo_y.min(y_min);
            lo_z = lo_z.min(z_min);
            hi_x = hi_x.max(math::floor(xx + r).max(x_min));
            hi_y = hi_y.max(math::floor(yy + r).max(y_min));
            hi_z = hi_z.max(math::floor(zz + r).max(z_min));
        }
        if lo_x > hi_x {
            // Every sphere was culled; the placement loop will visit nothing.
            bits.clear();
            return Self { x0: 0, y0: 0, z0: 0, xs: 0, ys: 0, zs: 0, bits };
        }
        let (xs, ys, zs) = (hi_x - lo_x + 1, hi_y - lo_y + 1, hi_z - lo_z + 1);
        let cells = (xs as usize) * (ys as usize) * (zs as usize);
        // `clear` before `resize` matters: `resize` only zeroes the elements it
        // *adds*, so a buffer returning from a larger blob would arrive carrying
        // that blob's set bits. A stale set bit reads as "already tested" and
        // silently DROPS an ore placement — a changed world, not a slow one.
        bits.clear();
        bits.resize(cells.div_ceil(64), 0u64);
        Self {
            x0: lo_x,
            y0: lo_y,
            z0: lo_z,
            xs,
            ys,
            zs,
            bits,
        }
    }

    /// Hands the bitset buffer back for reuse. See [`VISITED_BITS`].
    fn into_bits(self) -> Vec<u64> {
        self.bits
    }

    /// `HashSet::insert`'s contract: `true` iff the position was not already in
    /// the set.
    ///
    /// A position outside the box would be a defect in
    /// [`Self::over_spheres`]'s derivation rather than a case to tolerate, so it
    /// asserts instead of silently answering `true` (which would re-test a
    /// position and could place an extra ore — a world change).
    #[inline]
    fn insert(&mut self, x: i32, y: i32, z: i32) -> bool {
        let (dx, dy, dz) = (x - self.x0, y - self.y0, z - self.z0);
        assert!(
            (0..self.xs).contains(&dx) && (0..self.ys).contains(&dy) && (0..self.zs).contains(&dz),
            "VisitedBox: ({x},{y},{z}) outside the box derived from the same sphere \
             bounds the placement loop walks — the derivation and the loop have drifted"
        );
        let idx = ((dy * self.zs + dz) * self.xs + dx) as usize;
        let (word, bit) = (idx / 64, idx % 64);
        let mask = 1u64 << bit;
        let already = self.bits[word] & mask != 0;
        self.bits[word] |= mask;
        !already
    }
}

fn is_outside_build_height(y: i32, min_y: i32, height: i32) -> bool {
    y < min_y || y >= min_y + height
}

/// How many targets [`TargetCache`] can hold a bitmask for. Vanilla's widest
/// `UNDERGROUND_ORES` config has **two** (a `stone_ore_replaceables` target and
/// a `deepslate_ore_replaceables` one), so this is 4× headroom; a config with
/// more falls back to [`match_targets_by_name`] per candidate, which is exactly
/// the code path that existed before this cache and therefore cannot diverge.
const MAX_CACHED_TARGETS: usize = 8;

/// Slots in [`TargetCache`]'s direct-mapped table. A blob visits ~82 candidate
/// positions (measured: 79,148 candidates over 962 blobs per interior column) and
/// the states under them are a handful of terrain blocks, so 16 slots keyed on the
/// low bits of the [`StateId`] is enough for a near-total hit rate while staying
/// 32 bytes of stack.
const TARGET_CACHE_SLOTS: usize = 16;

/// Per-**blob** memo for the two string operations `try_place_ore` used to
/// perform on every candidate position: resolving the cell's base name and
/// testing it against each of the config's `RuleTest`s.
///
/// # Why this is where the ore stage's cost was
///
/// DESIGN.md §12.149: with `ore` at 38.7% of a steady-state column, a `samply`
/// profile of the stage alone put **18.5% of it in `SipHash::write`/`hash_one::<&str>`
/// plus 3.5% in `memcmp`** — the `ore_tag_map` lookup and its member-set probe,
/// two string hashes per target per candidate, 81,316 target tests per column —
/// and a further ~13% of the inlined placement loop in `memchr`, which is
/// `current.split('[')`. All of it recomputed the same answer for the same
/// [`StateId`] tens of thousands of times per column, because the *terrain* under
/// a blob is a handful of distinct states.
///
/// # How it works
///
/// Direct-mapped on the raw [`StateId`], with a full key compare, holding a
/// bitmask of which of `config.targets` match that state. A hit is one array
/// index and one `u16` compare. A miss runs [`match_targets_by_name`] — the
/// original name-based code, unchanged — and stores the answer.
///
/// The mask is only ever *consumed* in ascending target order, so the sequence of
/// matching targets, and therefore the `should_skip_air_check` draw sequence, is
/// bit-for-bit what the name-based loop produced. **That is the whole correctness
/// argument and it is why the mask is a bitmask rather than a "first match"
/// index:** vanilla's own "can place ore" check can refuse a matching target and vanilla then continues
/// to the *next* matching one, drawing again.
///
/// # How to change it, and the trap
///
/// **The cache must not outlive one `OreConfig`.** It is constructed inside
/// [`do_place`], which handles exactly one config, so the config is not part of
/// the key. Hoisting it to a `thread_local!` (the shape [`BLOB_DATA`] uses) would
/// make it wrong in the §12.143 way — a value from a *different function* at a
/// plausible magnitude — unless the config's identity went into the key and was
/// cleared between generators. Do not hoist it.
///
/// `state_ids` caches the placed state's own [`StateId`] per target, which is the
/// other string cost this removes: `RegionView::set` interned the target's state
/// *string* on every one of the 62,796 writes a column performs (measured at 4.2%
/// of the stage in `StateInterner::id_of`), and a blob writes one or two distinct
/// states.
struct TargetCache {
    /// Raw [`StateId`] per slot; `u16::MAX` means empty. `u16::MAX` is safe as a
    /// sentinel because `intern_locked` panics past 65,536 states, so no live id
    /// can equal it.
    keys: [u16; TARGET_CACHE_SLOTS],
    masks: [u8; TARGET_CACHE_SLOTS],
    /// Lazily-resolved [`StateId`] of each target's placed state; `u16::MAX` means
    /// not yet resolved.
    state_ids: [u16; MAX_CACHED_TARGETS],
}

/// The sentinel both of [`TargetCache`]'s tables use for "empty".
const NO_ID: u16 = u16::MAX;

impl TargetCache {
    fn empty() -> Self {
        Self {
            keys: [NO_ID; TARGET_CACHE_SLOTS],
            masks: [0; TARGET_CACHE_SLOTS],
            state_ids: [NO_ID; MAX_CACHED_TARGETS],
        }
    }

    /// The bitmask of `config.targets` matching the state at `id`, computed by
    /// [`match_targets_by_name`] on a miss.
    #[inline]
    fn mask_for(
        &mut self,
        id: crate::interner::StateId,
        config: &OreConfig,
        input: &OreInput<'_>,
        working: &RegionView<'_>,
    ) -> u8 {
        let raw = id.raw();
        let slot = (raw as usize) & (TARGET_CACHE_SLOTS - 1);
        if self.keys[slot] == raw {
            return self.masks[slot];
        }
        let name = working.interner().name_of(id);
        let mask = match_targets_by_name(name, config, input);
        self.keys[slot] = raw;
        self.masks[slot] = mask;
        mask
    }

    /// The [`crate::interner::StateId`] of target `i`'s placed state, interned
    /// against the view's own interner on first use.
    #[inline]
    fn state_id_for(
        &mut self,
        i: usize,
        config: &OreConfig,
        working: &RegionView<'_>,
    ) -> crate::interner::StateId {
        let cached = self.state_ids[i];
        if cached != NO_ID {
            return crate::interner::StateId::from_raw(cached);
        }
        let id = working.interner().id_of(&config.targets[i].state);
        self.state_ids[i] = id.raw();
        id
    }
}

/// Vanilla's own target-block-state test for every target of `config` against the block
/// state named `name`, as a bitmask over target index.
///
/// This is the original `try_place_ore` body's inner test, moved out verbatim so
/// [`TargetCache`] and the uncached path cannot drift: the tag/block comparison,
/// the `split('[')` base strip and the `in_tag` closure are all still exactly
/// what they were. Neither test draws, so hoisting them out of the placement loop
/// cannot move the RNG.
fn match_targets_by_name(name: &str, config: &OreConfig, input: &OreInput<'_>) -> u8 {
    let base = name.split('[').next().unwrap_or(name);
    let mut mask = 0u8;
    for (i, target) in config.targets.iter().enumerate().take(MAX_CACHED_TARGETS) {
        ore_probe::bump_target_test(1);
        let matches = match &target.target {
            RuleTest::TagMatch(tag) => {
                ore_probe::bump_target_test_tag(1);
                (input.in_tag)(base, tag)
            }
            RuleTest::BlockMatch(block) => base == block.as_str(),
        };
        if matches {
            mask |= 1 << i;
        }
    }
    mask
}

fn try_place_ore<R: RandomSource>(
    random: &mut R,
    config: &OreConfig,
    input: &OreInput<'_>,
    working: &mut RegionView<'_>,
    x: i32,
    y: i32,
    z: i32,
    cache: &mut TargetCache,
) {
    // Writes always land somewhere in the driven 3×3 region (clamped beyond
    // it — see `OreInput::region_local`); whether this particular write is
    // in the CENTRE (and therefore fixture-comparable) is decided later by
    // the caller via `in_center`, not here — a neighbour-chunk write still
    // has to happen so later reads (isAdjacentToAir, a later source's own
    // placement) see it, exactly as vanilla's real, shared block field would.
    let (lx, lz) = input.region_local(x, z);
    // Ids, not names. Until §12.149 this read the cell as a `&str`, stripped its
    // base name with `split('[')` and hashed that string against `ore_tag_map`
    // once per target — three string operations per candidate position, ~79,148
    // candidates per column, all answering the same question about the same
    // handful of terrain states. `TargetCache` answers it from a `u16` compare;
    // `match_targets_by_name` is still the only implementation of the test
    // itself.
    //
    // The draw sequence is untouched, as it was by the earlier removal of the
    // `to_string()` here: `should_skip_air_check` still fires per **matching**
    // target, in ascending target order, and the walk still stops at the first
    // target that places.
    let mut chosen: Option<usize> = None;
    {
        ore_probe::bump_region_read(1);
        let id = working.get_id(lx, y, lz);
        let mut mask = if config.targets.len() <= MAX_CACHED_TARGETS {
            cache.mask_for(id, config, input, working)
        } else {
            // Wider than the cache's mask. Recompute per candidate, which is the
            // pre-§12.149 behaviour; `MAX_CACHED_TARGETS` documents why no real
            // config reaches here.
            match_targets_by_name(working.interner().name_of(id), config, input)
        };
        while mask != 0 {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            // canPlaceOre: shouldSkipAirCheck (may draw a nextFloat) ? true
            // : !isAdjacentToAir.
            let place = should_skip_air_check(random, config.discard_chance_on_air_exposure)
                || {
                    ore_probe::bump_air_check(1);
                    !is_adjacent_to_air(input, working, x, y, z)
                };
            if place {
                chosen = Some(i);
                break;
            }
            // canPlaceOre returned false; vanilla continues to the next target.
        }
    }
    if let Some(i) = chosen {
        ore_probe::bump_write(1);
        // The interned id, resolved once per blob rather than once per write —
        // `RegionView::set` hashed the target's state *string* through
        // `StateInterner::id_of` on every one of a column's 62,796 ore writes. The
        // written value is identical: `id_of` is that string's id in this view's
        // own interner, which is what `set` looked up.
        let state = if i < MAX_CACHED_TARGETS {
            cache.state_id_for(i, config, working)
        } else {
            working.interner().id_of(&config.targets[i].state)
        };
        working.set_id(lx, y, lz, state);
    }
}

/// Vanilla's own ore-feature "should skip air check". Draws a single `nextFloat` iff
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

/// Vanilla's own "is adjacent to air" check over the six direction neighbours. Reads the
/// live working grid across the whole driven 3×3 region (clamped beyond it —
/// see `OreInput::region_local`), so a read just outside the centre sees the
/// real neighbour terrain (or an in-flight write from an earlier source
/// pass), not an assumed-empty scratch chunk.
fn is_adjacent_to_air(input: &OreInput<'_>, working: &RegionView<'_>, x: i32, y: i32, z: i32) -> bool {
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

fn block_at<'a>(input: &OreInput<'_>, working: &'a RegionView<'_>, x: i32, y: i32, z: i32) -> &'a str {
    if is_outside_build_height(y, input.min_y, input.height) {
        return "minecraft:air";
    }
    ore_probe::bump_region_read(1);
    let (lx, lz) = input.region_local(x, z);
    working.get(lx, y, lz)
}

fn is_air(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The height a probe resolves to must be, for **every** probe coordinate the
    /// ore driver can produce, exactly what the `HashMap<(i32, i32), i32>` this
    /// replaced resolved to.
    ///
    /// # Why this drives `OreInput::get_height` and not `RegionHeights::get`
    ///
    /// A first version of this test compared `RegionHeights::get` against the map
    /// over every region column, and it was **vacuous**: `set` and `get` share
    /// one `index` function, so permuting that function permutes writes and reads
    /// together and nothing observable changes. Its companion "a transposed index
    /// would be caught" control was run against a deliberately transposed
    /// `index` and **passed** — a premise-false control of exactly the shape
    /// `docs/plans/worldgen-rewrite.md`'s evidence standard warns about. The
    /// index formula is not a correctness property at all; any bijection over the
    /// region is equivalent.
    ///
    /// What *is* observable, and what this therefore tests, is the composition
    /// the driver actually performs: the stitch's offset arithmetic, the
    /// [`OreInput::region_local`] clamp, and the read, end to end — with the
    /// expected side computed by the deleted `HashMap` lookup written out
    /// longhand. The control below perturbs the stitch and does fire.
    #[test]
    fn every_probe_resolves_to_the_height_the_hashmap_resolved_to() {
        let heights_for = |dx: i32, dz: i32| -> [i32; 256] {
            let mut h = [0i32; 256];
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    // Per-column variety, asymmetric in (lx, lz) and distinct per
                    // source chunk, so a swapped offset or a wrong source shows up.
                    h[(lz * 16 + lx) as usize] = dx * 1009 + dz * 97 + lx * 31 + lz * 7 - 64;
                }
            }
            h
        };
        let mut map: HashMap<(i32, i32), i32> = HashMap::new();
        let mut dense = RegionHeights::unset();
        for dz in -1..=1i32 {
            for dx in -1..=1i32 {
                let h = heights_for(dx, dz);
                for lz in 0..16i32 {
                    for lx in 0..16i32 {
                        let v = h[(lz * 16 + lx) as usize];
                        map.insert((dx * 16 + lx, dz * 16 + lz), v);
                        dense.set(dx * 16 + lx, dz * 16 + lz, v);
                    }
                }
            }
        }

        let input = OreInput {
            chunk_x: 4,
            chunk_z: -3,
            center_x: 4,
            center_z: -3,
            min_y: -64,
            height: 384,
            min_gen_y: -64,
            gen_depth: 384,
            read_min: REGION_MIN,
            read_max: REGION_MAX,
            ocean_floor_wg: &dense,
            in_tag: &|_, _| false,
        };
        // Probe range: the whole region PLUS a wide ring outside it, because the
        // largest blob ores really do probe past the 3x3 and the clamp is the
        // thing that has to still agree there.
        let base_x = 4 * 16;
        let base_z = -3 * 16;
        let mut compared = 0u32;
        let mut clamped_compared = 0u32;
        for z in (base_z + REGION_MIN - 20)..(base_z + REGION_MAX + 20) {
            for x in (base_x + REGION_MIN - 20)..(base_x + REGION_MAX + 20) {
                // The deleted lookup, longhand: clamp, then probe the map.
                let lx = (x - base_x).clamp(REGION_MIN, REGION_MAX - 1);
                let lz = (z - base_z).clamp(REGION_MIN, REGION_MAX - 1);
                let expected = map[&(lx, lz)];
                assert_eq!(
                    input.get_height(x, z),
                    expected,
                    "probe ({x}, {z}) resolved differently from the HashMap",
                );
                compared += 1;
                if lx != x - base_x || lz != z - base_z {
                    clamped_compared += 1;
                }
            }
        }
        assert_eq!(compared, 88 * 88, "the probe sweep did not cover the intended range");
        assert!(
            clamped_compared > 4_000,
            "only {clamped_compared} probes were outside the region, so the clamp is \
             barely exercised",
        );
    }

    /// The control for the test above, with a premise that is **true**: writing
    /// the nine sources into the dense map with their `(dx, dz)` offsets swapped
    /// must be caught, because the expected side is built with the correct ones.
    ///
    /// This is the assertion the first draft of this control failed to make — it
    /// perturbed something unobservable and passed. Watch it fail by removing the
    /// swap.
    #[test]
    fn a_swapped_source_offset_in_the_stitch_is_caught() {
        let value = |dx: i32, dz: i32, lx: i32, lz: i32| dx * 1009 + dz * 97 + lx * 31 + lz * 7;
        let mut map: HashMap<(i32, i32), i32> = HashMap::new();
        let mut dense = RegionHeights::unset();
        for dz in -1..=1i32 {
            for dx in -1..=1i32 {
                for lz in 0..16i32 {
                    for lx in 0..16i32 {
                        map.insert((dx * 16 + lx, dz * 16 + lz), value(dx, dz, lx, lz));
                        // The bug: source (dx, dz) written at offset (dz, dx).
                        dense.set(dz * 16 + lx, dx * 16 + lz, value(dx, dz, lx, lz));
                    }
                }
            }
        }
        let mut disagreements = 0u32;
        for lz in REGION_MIN..REGION_MAX {
            for lx in REGION_MIN..REGION_MAX {
                if dense.get(lx, lz) != map[&(lx, lz)] {
                    disagreements += 1;
                }
            }
        }
        assert!(
            disagreements > 1_000,
            "control: swapping the source offsets must disagree over most of the region, \
             but only {disagreements} of 2304 columns differed — the detector is blind",
        );
    }

    /// A probe outside the region is clamped by [`OreInput::region_local`], and
    /// an unstitched column still panics rather than reading as 0 — the detector
    /// the `HashMap`'s `unwrap_or_else(|| panic!(…))` provided.
    #[test]
    #[should_panic(expected = "no heightmap entry for region-local")]
    fn an_unstitched_region_column_still_panics() {
        let dense = RegionHeights::unset();
        let _ = dense.get(0, 0);
    }

    /// [`VisitedBox`] must behave exactly like the `HashSet<(i32, i32, i32)>` it
    /// replaced, over the real coordinate walk `do_place` performs.
    ///
    /// The expected side is a live `HashSet`, driven by the same nested loop, so
    /// the two are compared **decision by decision** (`insert`'s boolean, which
    /// is what gates a `try_place_ore` call) rather than only on their final
    /// contents. That distinction matters: a set that agreed on membership but
    /// disagreed on *which* insert was the first would change how many ores are
    /// placed.
    #[test]
    fn visited_box_makes_the_same_insert_decisions_as_the_hashset() {
        // A blob shaped like a real one: `size = 33` (`ore_andesite`-ish), a
        // sloped centre line, radii from a fixed sequence rather than an RNG so
        // this test is deterministic without pulling a generator in.
        let size = 33i32;
        let (x_start, y_start, z_start) = (-13, -70, 41);
        let mut data = vec![0.0f64; (size * 4) as usize];
        for i in 0..size {
            let b = (i * 4) as usize;
            let t = f64::from(i) / f64::from(size);
            data[b] = f64::from(x_start) + 6.0 + 8.0 * t;
            data[b + 1] = f64::from(y_start) + 5.0 + 3.0 * t;
            data[b + 2] = f64::from(z_start) + 6.0 + 8.0 * t;
            // Cull a few spheres, exactly as the overlap pass does, so the
            // `r < 0.0` skip is exercised on both sides.
            data[b + 3] = if i % 7 == 3 { -1.0 } else { 1.0 + 2.0 * (t * 3.0).sin().abs() };
        }

        let mut boxed =
            VisitedBox::over_spheres(Vec::new(), &data, size, x_start, y_start, z_start);
        let mut set: HashSet<(i32, i32, i32)> = HashSet::new();
        let mut visits = 0u32;
        let mut firsts = 0u32;
        for i in 0..size {
            let b = (i * 4) as usize;
            let r = data[b + 3];
            if r < 0.0 {
                continue;
            }
            let (xx, yy, zz) = (data[b], data[b + 1], data[b + 2]);
            let x_min = math::floor(xx - r).max(x_start);
            let y_min = math::floor(yy - r).max(y_start);
            let z_min = math::floor(zz - r).max(z_start);
            let x_max = math::floor(xx + r).max(x_min);
            let y_max = math::floor(yy + r).max(y_min);
            let z_max = math::floor(zz + r).max(z_min);
            for x in x_min..=x_max {
                for y in y_min..=y_max {
                    for z in z_min..=z_max {
                        visits += 1;
                        let a = boxed.insert(x, y, z);
                        let e = set.insert((x, y, z));
                        assert_eq!(a, e, "insert({x},{y},{z}) disagreed with the HashSet");
                        if e {
                            firsts += 1;
                        }
                    }
                }
            }
        }
        // Non-vacuity, in both directions: the walk really happened, and it
        // really contained repeats — a walk with no repeat would satisfy the
        // assertion above with a stub that always returns `true`.
        assert!(visits > 5_000, "the walk visited only {visits} positions");
        assert!(
            firsts < visits,
            "the walk produced no repeated position ({visits} visits, {firsts} first-time), \
             so it cannot distinguish a real dedup from a stub that always answers true",
        );
    }

    /// The control for the test above: a box one block too small in every axis
    /// must be *caught*, not silently wrap or drop. This is what makes
    /// [`VisitedBox::over_spheres`]'s "derived from the same expressions"
    /// argument load-bearing rather than decorative — if an out-of-box insert
    /// were tolerated, an undersized derivation would re-test positions and
    /// place extra ore.
    #[test]
    #[should_panic(expected = "outside the box derived from the same sphere bounds")]
    fn an_undersized_visited_box_panics_instead_of_dropping_a_position() {
        let mut shrunk = VisitedBox {
            x0: 0,
            y0: 0,
            z0: 0,
            xs: 2,
            ys: 2,
            zs: 2,
            bits: vec![0u64; 1],
        };
        assert!(shrunk.insert(1, 1, 1), "in-box insert must work first");
        let _ = shrunk.insert(2, 1, 1);
    }

    /// An all-culled blob produces an empty box, and the placement loop that
    /// follows visits nothing — so the empty case must not panic on
    /// construction.
    #[test]
    fn a_fully_culled_blob_yields_an_empty_visited_box() {
        let size = 4i32;
        let mut data = vec![0.0f64; (size * 4) as usize];
        for i in 0..size {
            data[(i * 4 + 3) as usize] = -1.0;
        }
        let b = VisitedBox::over_spheres(Vec::new(), &data, size, 0, 0, 0);
        assert_eq!(b.bits.len(), 0);
        assert_eq!((b.xs, b.ys, b.zs), (0, 0, 0));
    }

    /// A recycled `VISITED_BITS` buffer arriving with bits already set must not
    /// be believed. `Vec::resize` only zeroes the elements it **adds**, so a
    /// buffer coming back from a larger blob keeps that blob's set bits in the
    /// words the smaller blob reuses — and a stale set bit reads as "already
    /// tested", which makes `do_place` **skip** a `try_place_ore` it must
    /// perform. That is a dropped ore, i.e. a changed world, and it is invisible
    /// to any test that only ever hands `over_spheres` a fresh `Vec`.
    ///
    /// The control is the same call with the same dirty buffer against the
    /// `clear()` removed — asserted here by checking that every word really is
    /// zero on entry, which is the property `clear()` provides and `resize()`
    /// alone does not.
    #[test]
    fn a_recycled_visited_buffer_starts_clear() {
        let size = 4i32;
        let mut data = vec![0.0f64; (size * 4) as usize];
        for i in 0..size {
            let b = (i * 4) as usize;
            data[b] = f64::from(i);
            data[b + 1] = f64::from(i);
            data[b + 2] = f64::from(i);
            data[b + 3] = 2.0;
        }
        // A buffer as it would come back from a previous, larger blob: every bit
        // set, and longer than this blob needs.
        let dirty = vec![u64::MAX; 64];
        let boxed = VisitedBox::over_spheres(dirty, &data, size, 0, 0, 0);
        assert!(
            !boxed.bits.is_empty(),
            "this blob should need at least one word; the scene is wrong"
        );
        assert!(
            boxed.bits.iter().all(|&w| w == 0),
            "a recycled buffer reached the placement loop with bits already set, \
             so `insert` would report positions as already tested and `do_place` \
             would silently skip placing them: {:?}",
            boxed.bits
        );

        // And the box really is used as a dedup afterwards, so the assertion
        // above is about a live mechanism rather than an inert field.
        let mut boxed = boxed;
        assert!(boxed.insert(1, 1, 1), "first insert must be new");
        assert!(!boxed.insert(1, 1, 1), "second insert must be a duplicate");
    }
}
