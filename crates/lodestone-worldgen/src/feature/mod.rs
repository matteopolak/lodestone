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

/// `GenerationStep.Decoration.UNDERGROUND_ORES.ordinal()`.
pub const STEP_UNDERGROUND_ORES: i32 = 6;

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
#[derive(Clone, Debug)]
pub enum IntProvider {
    Constant(i32),
    Uniform { min: i32, max: i32 },
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
                    other => panic!("unsupported int provider: {other}"),
                }
            }
            other => panic!("unexpected int provider json: {other}"),
        }
    }

    fn sample<R: RandomSource>(&self, random: &mut R) -> i32 {
        match *self {
            IntProvider::Constant(v) => v,
            IntProvider::Uniform { min, max } => math::random_between_inclusive(random, min, max),
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

/// The mutable centre-chunk block field the ore stage reads and writes. Keyed by
/// `(local_x, y, local_z)` with `local_x, local_z ∈ [0,16)` and `y` absolute.
pub type CenterGrid = HashMap<(i32, i32, i32), String>;

/// Inputs the ore driver needs beyond the RNG.
pub struct OreInput<'a> {
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub min_y: i32,
    pub height: i32,
    /// `getMinGenY` / `getGenDepth` for `VerticalAnchor` resolution.
    pub min_gen_y: i32,
    pub gen_depth: i32,
    /// `OCEAN_FLOOR_WG` heightmap as `level.getHeight` returns it, keyed by local
    /// `(x, z) ∈ [0,16)` (probes outside the chunk wrap via `& 15`, matching the
    /// oracle's proxy level).
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

    fn in_center(&self, x: i32, z: i32) -> Option<(i32, i32)> {
        let lx = x - self.chunk_x * 16;
        let lz = z - self.chunk_z * 16;
        if (0..16).contains(&lx) && (0..16).contains(&lz) {
            Some((lx, lz))
        } else {
            None
        }
    }

    fn get_height(&self, x: i32, z: i32) -> i32 {
        // Proxy level maps any probe into the centre heightmap via `& 15`.
        self.ocean_floor_wg[&(x & 15, z & 15)]
    }
}

/// Run the whole `UNDERGROUND_ORES` decoration step for the centre chunk over an
/// identical post-carve input, returning the block field after placement. `ores`
/// must be in step order with each entry's `index` set to its position within the
/// step's feature list (matching vanilla's `setFeatureSeed` index).
///
/// The returned grid is the input `grid` with ore writes applied; the caller
/// diffs it against the original to obtain the placed ores.
pub fn apply_ore_step<R: RandomSource>(
    random: &mut WorldgenRandom<R>,
    seed: i64,
    input: &OreInput<'_>,
    grid: &CenterGrid,
    ores: &[PlacedOre],
) -> CenterGrid {
    let origin = input.origin();
    let decoration_seed = random.set_decoration_seed(seed, origin.x, origin.z);
    let ctx = Ctx {
        min_gen_y: input.min_gen_y,
        gen_depth: input.gen_depth,
    };
    let mut working = grid.clone();
    for ore in ores {
        random.set_feature_seed(decoration_seed, ore.index as i32, STEP_UNDERGROUND_ORES);
        place_placed_feature(random, origin, ore, input, &ctx, &mut working);
    }
    working
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
    working: &mut CenterGrid,
) {
    fn recurse<R: RandomSource>(
        random: &mut R,
        modifiers: &[Placement],
        i: usize,
        pos: BlockPos,
        ctx: &Ctx,
        config: &OreConfig,
        input: &OreInput<'_>,
        working: &mut CenterGrid,
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
/// (centre-local); positions outside the centre read as air and so are never
/// placed (the isolated oracle uses empty neighbour chunks identically).
pub fn place_ore_feature<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    config: &OreConfig,
    input: &OreInput<'_>,
    working: &mut CenterGrid,
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
    working: &mut CenterGrid,
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
    working: &mut CenterGrid,
    x: i32,
    y: i32,
    z: i32,
) {
    let Some((lx, lz)) = input.in_center(x, z) else {
        // Neighbour chunks are empty air in the isolated oracle → no target
        // tag matches air, so `canPlaceOre` returns false with no RNG draw.
        return;
    };
    let key = (lx, y, lz);
    let current = working.get(&key).map_or("minecraft:air", String::as_str);
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
            working.insert(key, target.state.clone());
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

/// `Feature.isAdjacentToAir` over the six `Direction` neighbours. Reads the live
/// working grid for in-centre cells and treats everything else (neighbour
/// chunks, out-of-build-height) as air, matching the oracle's empty scratch
/// sections.
fn is_adjacent_to_air(input: &OreInput<'_>, working: &CenterGrid, x: i32, y: i32, z: i32) -> bool {
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

fn block_at<'a>(input: &OreInput<'_>, working: &'a CenterGrid, x: i32, y: i32, z: i32) -> &'a str {
    if is_outside_build_height(y, input.min_y, input.height) {
        return "minecraft:air";
    }
    match input.in_center(x, z) {
        Some((lx, lz)) => working
            .get(&(lx, y, lz))
            .map_or("minecraft:air", String::as_str),
        None => "minecraft:air",
    }
}

fn is_air(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}
