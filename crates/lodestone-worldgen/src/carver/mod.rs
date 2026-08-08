//! Version-free **carver** interpreter: caves and canyons (ravines).
//!
//! After the noise field is filled and surface rules run, vanilla scans a
//! 17×17 neighbourhood of *source* chunks around the target chunk and, for each
//! carver configured on the source chunk's biome, seeds a positional RNG
//! (`setLargeFeatureSeed`), rolls a probability gate (`isStartChunk`), and — on
//! success — carves tunnels/ravines that write air/water/lava into the *centre*
//! chunk only. The algorithm is hand-written machinery (this crate); the
//! per-version parameters (probabilities, radii, y-ranges) arrive as JSON data
//! parsed by [`CarverConfig::parse`], exactly like the density/surface layers.
//!
//! Parity is proven block-for-block against the JVM over whole chunks
//! (`carver_parity`), plus a per-carver draw-count probe: the exact *number* of
//! RNG values a carver consumes must match, or every feature placed afterwards
//! desynchronises.
//!
//! Float-vs-double discipline mirrors the decompiled source precisely: rotations
//! and radii accumulate in `f32`, positions in `f64`, and `Mth.sin/cos` take a
//! `f64` argument (a promoted `f32`) and return `f32`.

use std::collections::{HashMap, HashSet};

use lodestone_worldgen_core::hash::FastSet;

use serde_json::Value;

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::math;
use crate::rng::RandomSource;

const AIR: &str = "minecraft:air";
const WATER: &str = "minecraft:water[level=0]";
const LAVA: &str = "minecraft:lava[level=0]";
/// `WorldCarver.CAVE_AIR`. Only `NetherWorldCarver.carveBlock` writes it — the
/// Overworld's `getCarveState` goes through the aquifer, whose air is plain
/// `Blocks.AIR`.
const CAVE_AIR: &str = "minecraft:cave_air";

/// The carver neighbourhood radius (`applyCarvers`: `dx,dz ∈ [-8, 8]`).
///
/// `pub` since Unit 9, and the visibility is load-bearing rather than cosmetic:
/// [`crate::biome::memo`]'s slot map is sized so that one carve stage's
/// `2 * NEIGHBOURHOOD_RANGE + 1` wide source window cannot self-collide, and that
/// module asserts the relationship **at compile time** against this constant. Left
/// private, the memo would have had to hard-code 17, and widening the carver
/// neighbourhood would have silently degraded the memo to a thrashing cache with
/// every test still green.
pub const NEIGHBOURHOOD_RANGE: i32 = 8;
/// Each carver's own `getRange()` (== 4), giving a max tunnel length of
/// `(4*2-1)*16 = 112` blocks.
const CARVER_RANGE: i32 = 4;
const MAX_DISTANCE: i32 = (CARVER_RANGE * 2 - 1) * 16;

/// A vertical position that resolves against the world's height accessor
/// (`net.minecraft.world.level.levelgen.VerticalAnchor`).
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
            VerticalAnchor::Absolute(y.as_i64().expect("absolute") as i32)
        } else if let Some(o) = v.get("above_bottom") {
            VerticalAnchor::AboveBottom(o.as_i64().expect("above_bottom") as i32)
        } else if let Some(o) = v.get("below_top") {
            VerticalAnchor::BelowTop(o.as_i64().expect("below_top") as i32)
        } else {
            panic!("unrecognised vertical anchor: {v}");
        }
    }
}

/// `net.minecraft.util.valueproviders.FloatProvider`. Only the variants used by
/// the overworld carvers are modelled; each `sample` consumes exactly the same
/// number of RNG draws as vanilla (constant: 0, uniform: 1, trapezoid: 2).
#[derive(Clone, Copy, Debug)]
pub enum FloatProvider {
    Constant(f32),
    Uniform { min: f32, max: f32 },
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatProvider {
    #[must_use]
    pub fn sample<R: RandomSource>(self, random: &mut R) -> f32 {
        match self {
            FloatProvider::Constant(v) => v,
            FloatProvider::Uniform { min, max } => math::random_between(random, min, max),
            FloatProvider::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let plateau_start = (range - plateau) / 2.0;
                let plateau_end = range - plateau_start;
                min + random.next_float() * plateau_end + random.next_float() * plateau_start
            }
        }
    }

    fn parse(v: &Value) -> Self {
        if let Some(n) = v.as_f64() {
            return FloatProvider::Constant(n as f32);
        }
        match v["type"].as_str().expect("float provider type") {
            "minecraft:constant" => FloatProvider::Constant(v["value"].as_f64().unwrap() as f32),
            "minecraft:uniform" => FloatProvider::Uniform {
                min: v["min_inclusive"].as_f64().unwrap() as f32,
                max: v["max_exclusive"].as_f64().unwrap() as f32,
            },
            "minecraft:trapezoid" => FloatProvider::Trapezoid {
                min: v["min"].as_f64().unwrap() as f32,
                max: v["max"].as_f64().unwrap() as f32,
                plateau: v["plateau"].as_f64().unwrap() as f32,
            },
            other => panic!("unsupported float provider: {other}"),
        }
    }
}

/// `UniformHeight` — the only `HeightProvider` the overworld carvers use.
#[derive(Clone, Copy, Debug)]
pub struct HeightProvider {
    min: VerticalAnchor,
    max: VerticalAnchor,
}

impl HeightProvider {
    #[must_use]
    pub fn sample<R: RandomSource>(self, random: &mut R, min_gen_y: i32, gen_depth: i32) -> i32 {
        let min = self.min.resolve_y(min_gen_y, gen_depth);
        let max = self.max.resolve_y(min_gen_y, gen_depth);
        if min > max {
            min
        } else {
            math::random_between_inclusive(random, min, max)
        }
    }

    fn parse(v: &Value) -> Self {
        assert_eq!(
            v["type"].as_str(),
            Some("minecraft:uniform"),
            "only uniform height providers supported"
        );
        HeightProvider {
            min: VerticalAnchor::parse(&v["min_inclusive"]),
            max: VerticalAnchor::parse(&v["max_inclusive"]),
        }
    }
}

/// Cave carver configuration (`CaveCarverConfiguration`).
///
/// One struct for `minecraft:cave` **and** `minecraft:nether_cave`: they share the
/// codec exactly (`WorldCarver.java:32-34` registers both against
/// `CaveCarverConfiguration`), and `NetherWorldCarver extends CaveWorldCarver`
/// overriding only the four things [`CaveConfig::nether`] selects.
#[derive(Clone, Debug)]
pub struct CaveConfig {
    pub probability: f32,
    pub y: HeightProvider,
    pub y_scale: FloatProvider,
    pub horizontal_radius_multiplier: FloatProvider,
    pub vertical_radius_multiplier: FloatProvider,
    pub floor_level: FloatProvider,
    pub lava_level: VerticalAnchor,
    /// `minecraft:nether_cave` rather than `minecraft:cave`.
    ///
    /// **Three of the four differences are RNG draws, so this is not cosmetic.**
    /// `getCaveBound()` is 10 instead of 15 (same three draws, different bound),
    /// and `getThickness` is `(nextFloat()*2 + nextFloat()) * 2.0` — **two**
    /// draws, where the Overworld's is 2 plus a `nextInt(10)` plus a conditional
    /// 2 more. Routing a nether carver through the Overworld formula desyncs the
    /// stream on the first tunnel and every later cave in the chunk is wrong.
    /// The fourth, `getYScale() = 5.0`, consumes nothing but stretches every
    /// trunk tunnel vertically; the recursive splits still pass `1.0`.
    pub nether: bool,
}

/// Canyon (ravine) carver configuration (`CanyonCarverConfiguration`).
#[derive(Clone, Debug)]
pub struct CanyonConfig {
    pub probability: f32,
    pub y: HeightProvider,
    pub vertical_rotation: FloatProvider,
    pub y_scale: FloatProvider,
    pub lava_level: VerticalAnchor,
    pub distance_factor: FloatProvider,
    pub thickness: FloatProvider,
    pub width_smoothness: i32,
    pub horizontal_radius_factor: FloatProvider,
    pub vertical_radius_default_factor: f32,
    pub vertical_radius_center_factor: f32,
}

/// A parsed configured carver.
#[derive(Clone, Debug)]
pub enum CarverConfig {
    Cave(CaveConfig),
    Canyon(CanyonConfig),
}

impl CarverConfig {
    /// Parse a `worldgen/configured_carver/*.json` document.
    #[must_use]
    pub fn parse(doc: &Value) -> Self {
        let kind = doc["type"].as_str().expect("carver type");
        let c = &doc["config"];
        let lava_level = VerticalAnchor::parse(&c["lava_level"]);
        match kind {
            "minecraft:cave" | "minecraft:nether_cave" => CarverConfig::Cave(CaveConfig {
                probability: c["probability"].as_f64().unwrap() as f32,
                y: HeightProvider::parse(&c["y"]),
                y_scale: FloatProvider::parse(&c["yScale"]),
                horizontal_radius_multiplier: FloatProvider::parse(
                    &c["horizontal_radius_multiplier"],
                ),
                vertical_radius_multiplier: FloatProvider::parse(&c["vertical_radius_multiplier"]),
                floor_level: FloatProvider::parse(&c["floor_level"]),
                lava_level,
                nether: kind == "minecraft:nether_cave",
            }),
            "minecraft:canyon" => {
                let shape = &c["shape"];
                CarverConfig::Canyon(CanyonConfig {
                    probability: c["probability"].as_f64().unwrap() as f32,
                    y: HeightProvider::parse(&c["y"]),
                    vertical_rotation: FloatProvider::parse(&c["vertical_rotation"]),
                    y_scale: FloatProvider::parse(&c["yScale"]),
                    lava_level,
                    distance_factor: FloatProvider::parse(&shape["distance_factor"]),
                    thickness: FloatProvider::parse(&shape["thickness"]),
                    width_smoothness: shape["width_smoothness"].as_i64().unwrap() as i32,
                    horizontal_radius_factor: FloatProvider::parse(
                        &shape["horizontal_radius_factor"],
                    ),
                    vertical_radius_default_factor: shape["vertical_radius_default_factor"]
                        .as_f64()
                        .unwrap() as f32,
                    vertical_radius_center_factor: shape["vertical_radius_center_factor"]
                        .as_f64()
                        .unwrap() as f32,
                })
            }
            other => panic!("unsupported carver type: {other}"),
        }
    }

    fn probability(&self) -> f32 {
        match self {
            CarverConfig::Cave(c) => c.probability,
            CarverConfig::Canyon(c) => c.probability,
        }
    }

    fn lava_level(&self) -> VerticalAnchor {
        match self {
            CarverConfig::Cave(c) => c.lava_level,
            CarverConfig::Canyon(c) => c.lava_level,
        }
    }
}

/// The centre chunk's mutable block field, addressed by world coordinates.
/// Carvers read the current block (to test replaceability) and overwrite it
/// with air/water/lava.
///
/// Backed by [`crate::dense_grid::DenseBlockGrid`] (issue #295's Job 2) — a
/// flat, palette-indexed array instead of a `HashMap<(i32,i32,i32), String>`.
/// The measured regression this replaces: composing carvers over the old
/// `HashMap`-keyed shape (designed for parity-harness fixtures, not a
/// per-chunk-per-neighbour production hot loop) took a 144-chunk sweep from
/// sub-second to ~68s in debug. [`CarveGrid::new`]/[`CarveGrid::into_blocks`]
/// keep the original `HashMap` shape as a **test adapter** (existing
/// `carver_parity.rs` fixtures build one that way, and hand-writing a sparse
/// fixture as a map literal is clearer than constructing a dense grid by
/// hand); the production path
/// (`crate::overworld::OverworldGenerator::carve_stage`) uses
/// [`CarveGrid::from_dense`]/[`CarveGrid::into_dense`] instead, with no
/// `HashMap<(i32,i32,i32), String>` anywhere in the loop.
pub struct CarveGrid {
    dense: crate::dense_grid::DenseBlockGrid,
}

impl std::fmt::Debug for CarveGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CarveGrid").finish_non_exhaustive()
    }
}
impl CarveGrid {
    /// Test-adapter constructor (see struct doc): the bounding box is
    /// derived from the map's own key range, so this remains a drop-in
    /// replacement for the pre-#295-Job-2 `HashMap`-keyed constructor —
    /// every existing fixture-driven caller is unchanged.
    #[must_use]
    pub fn new(blocks: HashMap<(i32, i32, i32), String>) -> Self {
        if blocks.is_empty() {
            return CarveGrid {
                dense: crate::dense_grid::DenseBlockGrid::new(0, 0, 0, 0, 0, 0, AIR),
            };
        }
        let (mut min_x, mut min_y, mut min_z) = (i32::MAX, i32::MAX, i32::MAX);
        let (mut max_x, mut max_y, mut max_z) = (i32::MIN, i32::MIN, i32::MIN);
        for &(x, y, z) in blocks.keys() {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            min_z = min_z.min(z);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            max_z = max_z.max(z);
        }
        let dense = crate::dense_grid::DenseBlockGrid::from_hashmap(
            min_x,
            min_y,
            min_z,
            max_x - min_x + 1,
            max_y - min_y + 1,
            max_z - min_z + 1,
            &blocks,
        );
        CarveGrid { dense }
    }

    /// Production constructor (see struct doc): adopts an already-built
    /// dense grid with no conversion.
    #[must_use]
    pub fn from_dense(dense: crate::dense_grid::DenseBlockGrid) -> Self {
        CarveGrid { dense }
    }

    fn get(&self, x: i32, y: i32, z: i32) -> &str {
        self.dense.get(x, y, z)
    }

    fn set(&mut self, x: i32, y: i32, z: i32, state: &str) {
        self.dense.set(x, y, z, state);
    }

    /// Test-adapter destructor (see struct doc).
    #[must_use]
    pub fn into_blocks(self) -> HashMap<(i32, i32, i32), String> {
        self.dense.into_hashmap()
    }

    /// Production destructor (see struct doc): no conversion, no
    /// `HashMap<(i32,i32,i32), String>` ever built.
    #[must_use]
    pub fn into_dense(self) -> crate::dense_grid::DenseBlockGrid {
        self.dense
    }
}

fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// Mutable per-carve state threaded through the carve tree.
struct CarveEnv<'a> {
    grid: &'a mut CarveGrid,
    aquifer: &'a AquiferSystem,
    replaceable: &'a HashSet<String>,
    top_material: &'a dyn Fn(i32, i32, i32, bool) -> Option<String>,
    /// Cells this carve pass has already written.
    ///
    /// [`FastSet`], not the default hasher — a per-carved-cell membership test on
    /// a coordinate key. Insert/contains only, never iterated, so no order is
    /// observable; see [`lodestone_worldgen_core::hash::fast`] for why that has
    /// to be established per map. U17.
    mask: FastSet<(i32, i32, i32)>,
    min_gen_y: i32,
    gen_depth: i32,
    center_x: i32,
    center_z: i32,
    lava_level_y: i32,
    /// Whether the carver currently running is `minecraft:nether_cave`, which
    /// overrides `carveBlock` wholesale. Set per carver in [`apply_carvers`]
    /// rather than per env, because a biome's list can in principle mix types.
    nether: bool,
}

impl CarveEnv<'_> {
    fn can_replace(&self, state: &str) -> bool {
        self.replaceable.contains(base_name(state))
    }

    /// `WorldCarver.getCarveState` for `density == 0.0`: lava below `lava_level`,
    /// otherwise the aquifer's substance (air/water/lava). Never `None` here.
    fn carve_state(&self, x: i32, y: i32, z: i32) -> Option<&'static str> {
        if y <= self.lava_level_y {
            return Some(LAVA);
        }
        match self.aquifer.carve_substance(x, y, z) {
            Some(BlockKind::Air) => Some(AIR),
            Some(BlockKind::Water) => Some(WATER),
            Some(BlockKind::Lava) => Some(LAVA),
            Some(BlockKind::Stone) | None => None,
        }
    }

    /// `WorldCarver.carveBlock`. `has_grass` tracks whether a grass/mycelium
    /// block has been seen higher in this column; when set, a dirt block exposed
    /// directly beneath a carved block is re-capped with the biome's
    /// `topMaterial`.
    fn carve_block(&mut self, x: i32, y: i32, z: i32, has_grass: &mut bool) -> bool {
        if self.nether {
            return self.carve_block_nether(x, y, z);
        }
        let block = self.grid.get(x, y, z).to_string();
        let base = base_name(&block);
        if base == "minecraft:grass_block" || base == "minecraft:mycelium" {
            *has_grass = true;
        }
        if !self.can_replace(&block) {
            return false;
        }
        let state = match self.carve_state(x, y, z) {
            None => return false,
            Some(state) => state,
        };
        self.grid.set(x, y, z, state);

        if *has_grass {
            let below = self.grid.get(x, y - 1, z).to_string();
            if base_name(&below) == "minecraft:dirt" {
                let under_fluid = state != AIR;
                if let Some(top) = (self.top_material)(x, y - 1, z, under_fluid) {
                    self.grid.set(x, y - 1, z, &top);
                }
            }
        }
        true
    }

    /// `NetherWorldCarver.carveBlock` — the whole method, which is *not* a
    /// specialisation of the Overworld one but a replacement:
    ///
    /// ```text
    /// if (canReplaceBlock(config, chunk.getBlockState(pos))) {
    ///    chunk.setBlockState(pos, pos.getY() <= context.getMinGenY() + 31
    ///        ? LAVA : CAVE_AIR);
    ///    return true;
    /// }
    /// return false;
    /// ```
    ///
    /// No aquifer consultation, no `lava_level` from the config (the `+ 31` is
    /// hardcoded and, at `min_y 0`, means y ≤ 31 — one below the Nether's
    /// `sea_level` of 32), no grass tracking and no `topMaterial` re-cap. So none
    /// of [`CarveEnv::carve_state`]'s inputs are read on this path, which is what
    /// makes a *disabled* aquifer harmless here.
    fn carve_block_nether(&mut self, x: i32, y: i32, z: i32) -> bool {
        let block = self.grid.get(x, y, z).to_string();
        if !self.can_replace(&block) {
            return false;
        }
        let state = if y <= self.min_gen_y + 31 { LAVA } else { CAVE_AIR };
        self.grid.set(x, y, z, state);
        true
    }
}

/// `WorldCarver.carveEllipsoid`. `skip(xd, yd, zd, world_y)` mirrors the carver's
/// `CarveSkipChecker`.
fn carve_ellipsoid<F>(
    env: &mut CarveEnv,
    x: f64,
    y: f64,
    z: f64,
    horizontal_radius: f64,
    vertical_radius: f64,
    skip: F,
) where
    F: Fn(f64, f64, f64, i32) -> bool,
{
    let center_x = f64::from(env.center_x * 16 + 8);
    let center_z = f64::from(env.center_z * 16 + 8);
    let max_delta = 16.0 + horizontal_radius * 2.0;
    if (x - center_x).abs() > max_delta || (z - center_z).abs() > max_delta {
        return;
    }

    let chunk_min_x = env.center_x * 16;
    let chunk_min_z = env.center_z * 16;
    let min_x_index = (math::floor(x - horizontal_radius) - chunk_min_x - 1).max(0);
    let max_x_index = (math::floor(x + horizontal_radius) - chunk_min_x).min(15);
    let min_y = (math::floor(y - vertical_radius) - 1).max(env.min_gen_y + 1);
    // protectedBlocksOnTop = 7 (chunk is not upgrading).
    let max_y = (math::floor(y + vertical_radius) + 1).min(env.min_gen_y + env.gen_depth - 1 - 7);
    let min_z_index = (math::floor(z - horizontal_radius) - chunk_min_z - 1).max(0);
    let max_z_index = (math::floor(z + horizontal_radius) - chunk_min_z).min(15);

    for x_index in min_x_index..=max_x_index {
        let world_x = chunk_min_x + x_index;
        let xd = (f64::from(world_x) + 0.5 - x) / horizontal_radius;
        for z_index in min_z_index..=max_z_index {
            let world_z = chunk_min_z + z_index;
            let zd = (f64::from(world_z) + 0.5 - z) / horizontal_radius;
            if xd * xd + zd * zd >= 1.0 {
                continue;
            }
            let mut world_y = max_y;
            let mut has_grass = false;
            while world_y > min_y {
                let yd = (f64::from(world_y) - 0.5 - y) / vertical_radius;
                if !skip(xd, yd, zd, world_y) && !env.mask.contains(&(x_index, world_y, z_index)) {
                    env.mask.insert((x_index, world_y, z_index));
                    env.carve_block(world_x, world_y, world_z, &mut has_grass);
                }
                world_y -= 1;
            }
        }
    }
}

fn cave_should_skip(xd: f64, yd: f64, zd: f64, floor_level: f64) -> bool {
    if yd <= floor_level {
        true
    } else {
        xd * xd + yd * yd + zd * zd >= 1.0
    }
}

const CAVE_BOUND: i32 = 15;
const CAVE_Y_SCALE: f64 = 1.0;
/// `NetherWorldCarver.getCaveBound()`.
const NETHER_CAVE_BOUND: i32 = 10;
/// `NetherWorldCarver.getYScale()`. Applied to the **trunk** tunnel only; the
/// two recursive splits pass a literal `1.0` in both carvers.
const NETHER_CAVE_Y_SCALE: f64 = 5.0;

impl CaveConfig {
    /// `getCaveBound()`.
    fn cave_bound(&self) -> i32 {
        if self.nether { NETHER_CAVE_BOUND } else { CAVE_BOUND }
    }

    /// `getYScale()`.
    fn trunk_y_scale(&self) -> f64 {
        if self.nether { NETHER_CAVE_Y_SCALE } else { CAVE_Y_SCALE }
    }

    /// `getThickness(random)` — **different draw counts per family**, see
    /// [`CaveConfig::nether`].
    fn thickness<R: RandomSource>(&self, random: &mut R) -> f32 {
        if self.nether {
            (random.next_float() * 2.0 + random.next_float()) * 2.0
        } else {
            get_cave_thickness(random)
        }
    }

    fn carve<R: RandomSource>(
        &self,
        env: &mut CarveEnv,
        random: &mut R,
        source_x: i32,
        source_z: i32,
    ) {
        let inner = random.next_int_bounded(self.cave_bound());
        let mid = random.next_int_bounded(inner + 1);
        let cave_count = random.next_int_bounded(mid + 1);

        for _cave in 0..cave_count {
            let x = f64::from(source_x * 16 + random.next_int_bounded(16));
            let y = f64::from(self.y.sample(random, env.min_gen_y, env.gen_depth));
            let z = f64::from(source_z * 16 + random.next_int_bounded(16));
            let h_mult = f64::from(self.horizontal_radius_multiplier.sample(random));
            let v_mult = f64::from(self.vertical_radius_multiplier.sample(random));
            let floor_level = f64::from(self.floor_level.sample(random));

            let mut tunnels = 1;
            if random.next_int_bounded(4) == 0 {
                let y_scale = f64::from(self.y_scale.sample(random));
                let thickness = 1.0f32 + random.next_float() * 6.0;
                self.create_room(env, x, y, z, thickness, y_scale, floor_level);
                tunnels += random.next_int_bounded(4);
            }

            for _ in 0..tunnels {
                let horizontal_rotation = random.next_float() * std::f32::consts::TAU;
                let vertical_rotation = (random.next_float() - 0.5) / 4.0;
                let thickness = self.thickness(random);
                let distance = MAX_DISTANCE - random.next_int_bounded(MAX_DISTANCE / 4);
                let seed = random.next_long();
                self.create_tunnel(
                    env,
                    seed,
                    x,
                    y,
                    z,
                    h_mult,
                    v_mult,
                    thickness,
                    horizontal_rotation,
                    vertical_rotation,
                    0,
                    distance,
                    self.trunk_y_scale(),
                    floor_level,
                );
            }
        }
    }

    fn create_room(
        &self,
        env: &mut CarveEnv,
        x: f64,
        y: f64,
        z: f64,
        thickness: f32,
        y_scale: f64,
        floor_level: f64,
    ) {
        let horizontal_radius =
            1.5 + f64::from(math::sin(f64::from(std::f32::consts::FRAC_PI_2)) * thickness);
        let vertical_radius = horizontal_radius * y_scale;
        carve_ellipsoid(
            env,
            x + 1.0,
            y,
            z,
            horizontal_radius,
            vertical_radius,
            move |xd, yd, zd, _| cave_should_skip(xd, yd, zd, floor_level),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn create_tunnel(
        &self,
        env: &mut CarveEnv,
        tunnel_seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        horizontal_radius_multiplier: f64,
        vertical_radius_multiplier: f64,
        thickness: f32,
        mut horizontal_rotation: f32,
        mut vertical_rotation: f32,
        step: i32,
        dist: i32,
        y_scale: f64,
        floor_level: f64,
    ) {
        let mut random = crate::rng::LegacyRandomSource::new(tunnel_seed);
        let split_point = random.next_int_bounded(dist / 2) + dist / 4;
        let steep = random.next_int_bounded(6) == 0;
        let mut y_rota = 0.0f32;
        let mut x_rota = 0.0f32;

        for current_step in step..dist {
            let horizontal_radius = 1.5
                + f64::from(
                    math::sin(f64::from(
                        std::f32::consts::PI * current_step as f32 / dist as f32,
                    )) * thickness,
                );
            let vertical_radius = horizontal_radius * y_scale;
            let cos_x = math::cos(f64::from(vertical_rotation));
            x += f64::from(math::cos(f64::from(horizontal_rotation)) * cos_x);
            y += f64::from(math::sin(f64::from(vertical_rotation)));
            z += f64::from(math::sin(f64::from(horizontal_rotation)) * cos_x);
            vertical_rotation *= if steep { 0.92 } else { 0.7 };
            vertical_rotation += x_rota * 0.1;
            horizontal_rotation += y_rota * 0.1;
            x_rota *= 0.9;
            y_rota *= 0.75;
            x_rota += (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            y_rota += (random.next_float() - random.next_float()) * random.next_float() * 4.0;

            if current_step == split_point && thickness > 1.0 {
                let seed_a = random.next_long();
                let thick_a = random.next_float() * 0.5 + 0.5;
                self.create_tunnel(
                    env,
                    seed_a,
                    x,
                    y,
                    z,
                    horizontal_radius_multiplier,
                    vertical_radius_multiplier,
                    thick_a,
                    horizontal_rotation - std::f32::consts::FRAC_PI_2,
                    vertical_rotation / 3.0,
                    current_step,
                    dist,
                    1.0,
                    floor_level,
                );
                let seed_b = random.next_long();
                let thick_b = random.next_float() * 0.5 + 0.5;
                self.create_tunnel(
                    env,
                    seed_b,
                    x,
                    y,
                    z,
                    horizontal_radius_multiplier,
                    vertical_radius_multiplier,
                    thick_b,
                    horizontal_rotation + std::f32::consts::FRAC_PI_2,
                    vertical_rotation / 3.0,
                    current_step,
                    dist,
                    1.0,
                    floor_level,
                );
                return;
            }

            if random.next_int_bounded(4) != 0 {
                if !can_reach(
                    env.center_x,
                    env.center_z,
                    x,
                    z,
                    current_step,
                    dist,
                    thickness,
                ) {
                    return;
                }
                carve_ellipsoid(
                    env,
                    x,
                    y,
                    z,
                    horizontal_radius * horizontal_radius_multiplier,
                    vertical_radius * vertical_radius_multiplier,
                    move |xd, yd, zd, _| cave_should_skip(xd, yd, zd, floor_level),
                );
            }
        }
    }
}

fn get_cave_thickness<R: RandomSource>(random: &mut R) -> f32 {
    let mut thickness = random.next_float() * 2.0 + random.next_float();
    if random.next_int_bounded(10) == 0 {
        thickness *= random.next_float() * random.next_float() * 3.0 + 1.0;
    }
    thickness
}

fn can_reach(
    center_x: i32,
    center_z: i32,
    x: f64,
    z: f64,
    current_step: i32,
    total_steps: i32,
    thickness: f32,
) -> bool {
    let x_mid = f64::from(center_x * 16 + 8);
    let z_mid = f64::from(center_z * 16 + 8);
    let xd = x - x_mid;
    let zd = z - z_mid;
    let remaining = f64::from(total_steps - current_step);
    let rr = f64::from(thickness + 2.0 + 16.0);
    xd * xd + zd * zd - remaining * remaining <= rr * rr
}

impl CanyonConfig {
    fn carve<R: RandomSource>(
        &self,
        env: &mut CarveEnv,
        random: &mut R,
        source_x: i32,
        source_z: i32,
    ) {
        let x = f64::from(source_x * 16 + random.next_int_bounded(16));
        let y = f64::from(self.y.sample(random, env.min_gen_y, env.gen_depth));
        let z = f64::from(source_z * 16 + random.next_int_bounded(16));
        let horizontal_rotation = random.next_float() * std::f32::consts::TAU;
        let vertical_rotation = self.vertical_rotation.sample(random);
        let y_scale = f64::from(self.y_scale.sample(random));
        let thickness = self.thickness.sample(random);
        let distance = (MAX_DISTANCE as f32 * self.distance_factor.sample(random)) as i32;
        let seed = random.next_long();
        self.do_carve(
            env,
            seed,
            x,
            y,
            z,
            thickness,
            horizontal_rotation,
            vertical_rotation,
            0,
            distance,
            y_scale,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn do_carve(
        &self,
        env: &mut CarveEnv,
        tunnel_seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        thickness: f32,
        mut horizontal_rotation: f32,
        mut vertical_rotation: f32,
        step: i32,
        distance: i32,
        y_scale: f64,
    ) {
        let mut random = crate::rng::LegacyRandomSource::new(tunnel_seed);
        let width_factors = self.init_width_factors(env.gen_depth, &mut random);
        let mut y_rota = 0.0f32;
        let mut x_rota = 0.0f32;

        for current_step in step..distance {
            let mut horizontal_radius = 1.5
                + f64::from(
                    math::sin(f64::from(
                        current_step as f32 * std::f32::consts::PI / distance as f32,
                    )) * thickness,
                );
            let mut vertical_radius = horizontal_radius * y_scale;
            horizontal_radius *= f64::from(self.horizontal_radius_factor.sample(&mut random));
            vertical_radius = self.update_vertical_radius(
                &mut random,
                vertical_radius,
                distance as f32,
                current_step as f32,
            );
            let xc = math::cos(f64::from(vertical_rotation));
            let xs = math::sin(f64::from(vertical_rotation));
            x += f64::from(math::cos(f64::from(horizontal_rotation)) * xc);
            y += f64::from(xs);
            z += f64::from(math::sin(f64::from(horizontal_rotation)) * xc);
            vertical_rotation *= 0.7;
            vertical_rotation += x_rota * 0.05;
            horizontal_rotation += y_rota * 0.05;
            x_rota *= 0.8;
            y_rota *= 0.5;
            x_rota += (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            y_rota += (random.next_float() - random.next_float()) * random.next_float() * 4.0;

            if random.next_int_bounded(4) != 0 {
                if !can_reach(
                    env.center_x,
                    env.center_z,
                    x,
                    z,
                    current_step,
                    distance,
                    thickness,
                ) {
                    return;
                }
                let min_gen_y = env.min_gen_y;
                let wf = width_factors.clone();
                carve_ellipsoid(
                    env,
                    x,
                    y,
                    z,
                    horizontal_radius,
                    vertical_radius,
                    move |xd, yd, zd, world_y| {
                        canyon_should_skip(&wf, min_gen_y, xd, yd, zd, world_y)
                    },
                );
            }
        }
    }

    fn init_width_factors<R: RandomSource>(&self, depth: i32, random: &mut R) -> Vec<f32> {
        let mut width_factors = Vec::with_capacity(depth as usize);
        let mut width_factor = 1.0f32;
        for y_index in 0..depth {
            if y_index == 0 || random.next_int_bounded(self.width_smoothness) == 0 {
                width_factor = 1.0 + random.next_float() * random.next_float();
            }
            width_factors.push(width_factor * width_factor);
        }
        width_factors
    }

    fn update_vertical_radius<R: RandomSource>(
        &self,
        random: &mut R,
        vertical_radius: f64,
        distance: f32,
        current_step: f32,
    ) -> f64 {
        let vertical_multiplier = 1.0 - math::abs_f32(0.5 - current_step / distance) * 2.0;
        let factor = self.vertical_radius_default_factor
            + self.vertical_radius_center_factor * vertical_multiplier;
        f64::from(factor) * vertical_radius * f64::from(math::random_between(random, 0.75, 1.0))
    }
}

fn canyon_should_skip(
    width_factors: &[f32],
    min_gen_y: i32,
    xd: f64,
    yd: f64,
    zd: f64,
    world_y: i32,
) -> bool {
    let y_index = world_y - min_gen_y;
    (xd * xd + zd * zd) * f64::from(width_factors[(y_index - 1) as usize]) + yd * yd / 6.0 >= 1.0
}

/// Observer hook for the parity test: after each carver is processed for a
/// source chunk, it is handed `(source_x, source_z, index, started, random)` so
/// it can record the draw-count probe (`random.next_long()`). In production a
/// no-op observer is passed and the extra draw never happens.
pub trait CarveObserver {
    fn after_carver<R: RandomSource>(
        &mut self,
        source_x: i32,
        source_z: i32,
        index: usize,
        started: bool,
        random: &mut R,
    );
}

/// A no-op observer for the real integrated-server carve path.
#[derive(Debug)]
pub struct NoObserver;
impl CarveObserver for NoObserver {
    fn after_carver<R: RandomSource>(&mut self, _: i32, _: i32, _: usize, _: bool, _: &mut R) {}
}

/// `NoiseBasedChunkGenerator.applyCarvers`: drive every carver over the 17×17
/// source-chunk neighbourhood of the centre chunk, seeding a positional RNG per
/// source chunk × carver and writing carved blocks into `grid`.
///
/// `carvers_for_source(source_x, source_z)` resolves the carver list to run
/// for one source chunk. Vanilla's own `carverBiome` resolution
/// (`NoiseBasedChunkGenerator.applyCarvers`) samples that source chunk's
/// biome (at its own quart corner, `y = 0` — **not** the biome's surface
/// height; carver selection is a different question from surface material)
/// and reads *that* biome's `carvers` list, so the list — and its order,
/// which the `index` used for `setLargeFeatureSeed` depends on — can differ
/// per source chunk. A caller with a single fixed biome for the whole
/// neighbourhood (every isolated fixture test in this crate) can ignore the
/// arguments and return the same list every time.
#[allow(clippy::too_many_arguments)]
pub fn apply_carvers<O: CarveObserver>(
    seed: i64,
    chunk_x: i32,
    chunk_z: i32,
    min_gen_y: i32,
    gen_depth: i32,
    carvers_for_source: &dyn Fn(i32, i32) -> Vec<CarverConfig>,
    grid: &mut CarveGrid,
    aquifer: &AquiferSystem,
    replaceable: &HashSet<String>,
    top_material: &dyn Fn(i32, i32, i32, bool) -> Option<String>,
    observer: &mut O,
) {
    // The outer RNG's initial seed is irrelevant: setLargeFeatureSeed overwrites
    // it before every carver. Seed with 0 for determinism.
    let mut random = crate::rng::WorldgenRandom::new(crate::rng::LegacyRandomSource::new(0));

    let mut env = CarveEnv {
        grid,
        aquifer,
        replaceable,
        top_material,
        mask: FastSet::default(),
        min_gen_y,
        gen_depth,
        center_x: chunk_x,
        center_z: chunk_z,
        lava_level_y: 0,
        nether: false,
    };

    for dx in -NEIGHBOURHOOD_RANGE..=NEIGHBOURHOOD_RANGE {
        for dz in -NEIGHBOURHOOD_RANGE..=NEIGHBOURHOOD_RANGE {
            let source_x = chunk_x + dx;
            let source_z = chunk_z + dz;
            let carvers = carvers_for_source(source_x, source_z);
            for (index, carver) in carvers.iter().enumerate() {
                random.set_large_feature_seed(seed + index as i64, source_x, source_z);
                let started = random.next_float() <= carver.probability();
                if started {
                    env.lava_level_y = carver.lava_level().resolve_y(min_gen_y, gen_depth);
                    env.nether = matches!(carver, CarverConfig::Cave(c) if c.nether);
                    match carver {
                        CarverConfig::Cave(c) => c.carve(&mut env, &mut random, source_x, source_z),
                        CarverConfig::Canyon(c) => {
                            c.carve(&mut env, &mut random, source_x, source_z);
                        }
                    }
                }
                observer.after_carver(source_x, source_z, index, started, &mut random);
            }
        }
    }
}
