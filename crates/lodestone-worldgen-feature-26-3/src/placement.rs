//! Placement modifiers: the position-producing stages of a placed feature.

use lodestone_worldgen_core::engine::release26_3::biome::BiomeId;
use lodestone_worldgen_core::engine::release26_3::noise::Simplex;
use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource};
use serde_json::Value;

use crate::blocks::Dir;
use crate::env::{Env, Heightmap};
use crate::json::{Res, array, boolean, double, float, get, int, int_or, string, type_of};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::BlockPred;
use crate::provider::{HeightProvider, IntProvider, double_or};

/// What a modifier may consult besides the level: the placed feature being decorated (for the
/// biome filter) and which biomes carry which features.
pub struct PlacementCtx<'a> {
    pub top: Option<usize>,
    pub biome_has: &'a dyn Fn(BiomeId, usize) -> bool,
}

impl std::fmt::Debug for PlacementCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlacementCtx").field("top", &self.top).finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub enum Placement {
    Count(IntProvider),
    InSquare,
    Heightmap(Heightmap),
    HeightRange(HeightProvider),
    Biome,
    RarityFilter(i32),
    RandomChance(f32),
    BlockPredicateFilter(BlockPred),
    CountOnEveryLayer(IntProvider),
    EnvironmentScan { dir: Dir, target: BlockPred, allowed: BlockPred, max_steps: i32 },
    FixedPlacement(Vec<Pos>),
    NoiseBasedCount { ratio: i32, factor: f64, offset: f64 },
    NoiseThresholdCount { level: f64, below: i32, above: i32 },
    Offset { x: IntProvider, y: IntProvider, z: IntProvider },
    RandomlySelected(Vec<Placement>),
    SurfaceRelativeThresholdFilter { heightmap: Heightmap, min: i32, max: i32 },
    SurfaceWaterDepthFilter(i32),
    Cuboid { xz: IntProvider, y: IntProvider, edges: bool, interior: bool },
}

fn info_noise() -> &'static Simplex {
    use std::sync::OnceLock;
    static NOISE: OnceLock<Simplex> = OnceLock::new();
    NOISE.get_or_init(|| Simplex::new_discarding_offset(&mut LegacyRandomSource::new(2345)))
}

impl Placement {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let hm = |key: &str| Heightmap::from_name(string(v, key, ctx)?).ok_or_else(|| format!("{ctx}: unknown heightmap"));
        Ok(match type_of(v, ctx)? {
            "count" => Self::Count(IntProvider::parse(get(v, "count", ctx)?, ctx)?),
            "in_square" => Self::InSquare,
            "heightmap" => Self::Heightmap(hm("heightmap")?),
            "height_range" => Self::HeightRange(HeightProvider::parse(get(v, "height", ctx)?, ctx)?),
            "biome" => Self::Biome,
            "rarity_filter" => Self::RarityFilter(int(v, "chance", ctx)?),
            "random_chance" => Self::RandomChance(float(v, "chance", ctx)?),
            "block_predicate_filter" => Self::BlockPredicateFilter(BlockPred::parse(env, get(v, "predicate", ctx)?, ctx)?),
            "count_on_every_layer" => Self::CountOnEveryLayer(IntProvider::parse(get(v, "count", ctx)?, ctx)?),
            "environment_scan" => Self::EnvironmentScan {
                dir: Dir::from_name(string(v, "direction_of_search", ctx)?).ok_or_else(|| format!("{ctx}: bad direction"))?,
                target: BlockPred::parse(env, get(v, "target_condition", ctx)?, ctx)?,
                allowed: match v.get("allowed_search_condition") {
                    Some(a) => BlockPred::parse(env, a, ctx)?,
                    None => BlockPred::AlwaysTrue,
                },
                max_steps: int(v, "max_steps", ctx)?,
            },
            "fixed_placement" => {
                let mut out = Vec::new();
                for p in array(v, "positions", ctx)? {
                    let a = p.as_array().filter(|a| a.len() == 3).ok_or_else(|| format!("{ctx}: position expected"))?;
                    let n = |i: usize| a[i].as_i64().map(|x| x as i32).ok_or_else(|| format!("{ctx}: integer expected"));
                    out.push(Pos::new(n(0)?, n(1)?, n(2)?));
                }
                Self::FixedPlacement(out)
            }
            "noise_based_count" => Self::NoiseBasedCount {
                ratio: int(v, "noise_to_count_ratio", ctx)?,
                factor: double(v, "noise_factor", ctx)?,
                offset: double_or(v, "noise_offset", 0.0, ctx)?,
            },
            "noise_threshold_count" => Self::NoiseThresholdCount {
                level: double(v, "noise_level", ctx)?,
                below: int(v, "below_noise", ctx)?,
                above: int(v, "above_noise", ctx)?,
            },
            "offset" => Self::Offset {
                x: IntProvider::parse(get(v, "x", ctx)?, ctx)?,
                y: IntProvider::parse(get(v, "y", ctx)?, ctx)?,
                z: IntProvider::parse(get(v, "z", ctx)?, ctx)?,
            },
            "randomly_selected" => {
                Self::RandomlySelected(array(v, "placements", ctx)?.iter().map(|p| Self::parse(env, p, ctx)).collect::<Res<_>>()?)
            }
            "surface_relative_threshold_filter" => Self::SurfaceRelativeThresholdFilter {
                heightmap: hm("heightmap")?,
                min: int_or(v, "min_inclusive", i32::MIN, ctx)?,
                max: int_or(v, "max_inclusive", i32::MAX, ctx)?,
            },
            "surface_water_depth_filter" => Self::SurfaceWaterDepthFilter(int(v, "max_water_depth", ctx)?),
            "cuboid" => Self::Cuboid {
                xz: IntProvider::parse(get(v, "xz_size", ctx)?, ctx)?,
                y: IntProvider::parse(get(v, "y_size", ctx)?, ctx)?,
                edges: boolean(v, "include_edges", true, ctx)?,
                interior: boolean(v, "include_interior", true, ctx)?,
            },
            other => return Err(format!("{ctx}: unknown placement modifier `{other}`")),
        })
    }

    /// Collects the states whose placement rule this modifier consults.
    pub fn survive_states(&self, out: &mut Vec<crate::blocks::State>) {
        match self {
            Self::BlockPredicateFilter(p) => p.survive_states(out),
            Self::EnvironmentScan { target, allowed, .. } => {
                target.survive_states(out);
                allowed.survive_states(out);
            }
            Self::RandomlySelected(l) => l.iter().for_each(|p| p.survive_states(out)),
            _ => {}
        }
    }

    /// Appends the positions this modifier emits for `origin`.
    pub fn modify(&self, ctx: &PlacementCtx<'_>, level: &Level<'_>, rng: &mut Rng, origin: Pos, out: &mut Vec<Pos>) {
        match self {
            Self::Count(c) => {
                let n = c.sample(rng);
                for _ in 0..n {
                    out.push(origin);
                }
            }
            Self::InSquare => {
                let x = rng.next_int_bounded(16) + origin.x;
                let z = rng.next_int_bounded(16) + origin.z;
                out.push(Pos::new(x, origin.y, z));
            }
            Self::Heightmap(h) => {
                let height = level.height(*h, origin.x, origin.z);
                if height > level.min_y {
                    out.push(Pos::new(origin.x, height, origin.z));
                }
            }
            Self::HeightRange(h) => {
                let y = h.sample(rng, level);
                out.push(origin.at_y(y));
            }
            Self::Biome => {
                let top = ctx.top.expect("biome filter needs the top-level feature");
                let biome = level.biome(origin.x, origin.y, origin.z);
                if (ctx.biome_has)(biome, top) {
                    out.push(origin);
                }
            }
            Self::RarityFilter(chance) => {
                if rng.next_float() < 1.0 / *chance as f32 {
                    out.push(origin);
                }
            }
            Self::RandomChance(chance) => {
                if rng.next_float() < *chance {
                    out.push(origin);
                }
            }
            Self::BlockPredicateFilter(p) => {
                if p.test(level, origin.x, origin.y, origin.z) {
                    out.push(origin);
                }
            }
            Self::CountOnEveryLayer(c) => {
                let mut layer = 0;
                loop {
                    let mut found = false;
                    let mut i = 0;
                    while i < c.sample(rng) {
                        let x = rng.next_int_bounded(16) + origin.x;
                        let z = rng.next_int_bounded(16) + origin.z;
                        let start_y = level.height(Heightmap::MotionBlocking, x, z);
                        let y = find_on_ground(level, x, start_y, z, layer);
                        if y != i32::MAX {
                            out.push(Pos::new(x, y, z));
                            found = true;
                        }
                        i += 1;
                    }
                    layer += 1;
                    if !found {
                        break;
                    }
                }
            }
            Self::EnvironmentScan { dir, target, allowed, max_steps } => {
                let mut p = origin;
                if allowed.test(level, p.x, p.y, p.z) {
                    for _ in 0..*max_steps {
                        if target.test(level, p.x, p.y, p.z) {
                            out.push(p);
                            return;
                        }
                        p = p.relative(*dir);
                        if level.is_outside_build_height(p.y) {
                            return;
                        }
                        if !allowed.test(level, p.x, p.y, p.z) {
                            break;
                        }
                    }
                    if target.test(level, p.x, p.y, p.z) {
                        out.push(p);
                    }
                }
            }
            Self::FixedPlacement(list) => {
                let (cx, cz) = (origin.x >> 4, origin.z >> 4);
                for p in list {
                    if p.x >> 4 == cx && p.z >> 4 == cz {
                        out.push(*p);
                    }
                }
            }
            Self::NoiseBasedCount { ratio, factor, offset } => {
                let noise = f64::from(info_noise().get(f64::from(origin.x) / factor, f64::from(origin.z) / factor));
                let n = ((noise + offset) * f64::from(*ratio)).ceil() as i32;
                for _ in 0..n {
                    out.push(origin);
                }
            }
            Self::NoiseThresholdCount { level: threshold, below, above } => {
                let noise = f64::from(info_noise().get(f64::from(origin.x) / 200.0, f64::from(origin.z) / 200.0));
                let n = if noise < *threshold { *below } else { *above };
                for _ in 0..n {
                    out.push(origin);
                }
            }
            Self::Offset { x, y, z } => {
                let (dx, dy, dz) = (x.sample(rng), y.sample(rng), z.sample(rng));
                out.push(origin.offset(dx, dy, dz));
            }
            Self::RandomlySelected(list) => {
                let i = rng.next_int_bounded(list.len() as i32) as usize;
                list[i].modify(ctx, level, rng, origin, out);
            }
            Self::SurfaceRelativeThresholdFilter { heightmap, min, max } => {
                let surface = i64::from(level.height(*heightmap, origin.x, origin.z));
                let (lo, hi) = (surface + i64::from(*min), surface + i64::from(*max));
                if lo <= i64::from(origin.y) && i64::from(origin.y) <= hi {
                    out.push(origin);
                }
            }
            Self::Cuboid { xz, y, edges, interior } => {
                let height = y.sample(rng);
                let width = xz.sample(rng);
                let length = xz.sample(rng);
                for x in 0..=width {
                    for dy in 0..=height {
                        for z in 0..=length {
                            let on_x = x == 0 || x == width;
                            let on_y = dy == 0 || dy == height;
                            let on_z = z == 0 || z == length;
                            if (*edges || !on_x || !on_y) && (*edges || !on_z || !on_y) && (*edges || !on_x || !on_z) && (*interior || on_x || on_y || on_z) {
                                out.push(origin.offset(x, dy, z));
                            }
                        }
                    }
                }
            }
            Self::SurfaceWaterDepthFilter(max_depth) => {
                let floor = level.height(Heightmap::OceanFloor, origin.x, origin.z);
                let surface = level.height(Heightmap::WorldSurface, origin.x, origin.z);
                if surface - floor <= *max_depth {
                    out.push(origin);
                }
            }
        }
    }
}

fn find_on_ground(level: &Level<'_>, x: i32, y_start: i32, z: i32, layer_to_place_on: i32) -> i32 {
    let env = level.env;
    let empty = |s| {
        env.blocks.is_air(s) || env.blocks.block_of(s) == env.blocks.block_of(env.known.water) || env.blocks.block_of(s) == env.blocks.block_of(env.known.lava)
    };
    let mut current_layer = 0;
    let mut current = level.get(x, y_start, z);
    let mut y = y_start;
    while y >= level.min_y + 1 {
        let below = level.get(x, y - 1, z);
        if !empty(below) && empty(current) && env.blocks.block_of(below) != env.blocks.block_of(env.known.bedrock) {
            if current_layer == layer_to_place_on {
                return y;
            }
            current_layer += 1;
        }
        current = below;
        y -= 1;
    }
    i32::MAX
}
