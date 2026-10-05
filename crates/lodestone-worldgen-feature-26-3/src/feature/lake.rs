//! Lakes (`lake`) and fluid springs (`spring_feature`).

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::{FluidKind, State};
use crate::climate;
use crate::env::Env;
use crate::json::{Res, array, boolean, get, int_or};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::{BlockMatch, BlockPred};
use crate::stateprovider::StateProvider;

#[derive(Clone, Debug)]
pub struct LakeConfig {
    pub fluid: StateProvider,
    pub barrier: StateProvider,
    pub can_place_feature: BlockPred,
    pub can_replace_with_air_or_fluid: BlockPred,
    pub can_replace_with_barrier: BlockPred,
}

impl LakeConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            fluid: StateProvider::parse(env, get(v, "fluid", ctx)?, ctx)?,
            barrier: StateProvider::parse(env, get(v, "barrier", ctx)?, ctx)?,
            can_place_feature: BlockPred::parse(env, get(v, "can_place_feature", ctx)?, ctx)?,
            can_replace_with_air_or_fluid: BlockPred::parse(env, get(v, "can_replace_with_air_or_fluid", ctx)?, ctx)?,
            can_replace_with_barrier: BlockPred::parse(env, get(v, "can_replace_with_barrier", ctx)?, ctx)?,
        })
    }
}

const W: usize = 16;
const H: usize = 8;

fn cell(x: usize, z: usize, y: usize) -> usize {
    (x * W + z) * H + y
}

/// A grid cell outside the carved blob that touches it on any of the six sides.
fn borders_blob(grid: &[bool], x: usize, z: usize, y: usize) -> bool {
    !grid[cell(x, z, y)]
        && (x < W - 1 && grid[cell(x + 1, z, y)]
            || x > 0 && grid[cell(x - 1, z, y)]
            || z < W - 1 && grid[cell(x, z + 1, y)]
            || z > 0 && grid[cell(x, z - 1, y)]
            || y < H - 1 && grid[cell(x, z, y + 1)]
            || y > 0 && grid[cell(x, z, y - 1)])
}

pub fn place_lake(cfg: &LakeConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if origin.y <= level.min_y + 4 {
        return false;
    }
    let origin = origin.offset(-8, -4, -8);
    let env = level.env;
    let mut grid = vec![false; W * W * H];
    let spots = rng.next_int_bounded(4) + 4;
    for _ in 0..spots {
        let xr = rng.next_double() * 6.0 + 3.0;
        let yr = rng.next_double() * 4.0 + 2.0;
        let zr = rng.next_double() * 6.0 + 3.0;
        let xp = rng.next_double() * (16.0 - xr - 2.0) + 1.0 + xr / 2.0;
        let yp = rng.next_double() * (8.0 - yr - 4.0) + 2.0 + yr / 2.0;
        let zp = rng.next_double() * (16.0 - zr - 2.0) + 1.0 + zr / 2.0;
        for xx in 1..15usize {
            for zz in 1..15usize {
                for yy in 1..7usize {
                    let xd = (xx as f64 - xp) / (xr / 2.0);
                    let yd = (yy as f64 - yp) / (yr / 2.0);
                    let zd = (zz as f64 - zp) / (zr / 2.0);
                    if xd * xd + yd * yd + zd * zd < 1.0 {
                        grid[cell(xx, zz, yy)] = true;
                    }
                }
            }
        }
    }
    let fluid = cfg.fluid.get(level, rng, origin.x, origin.y, origin.z);
    for xx in 0..W {
        for zz in 0..W {
            for yy in 0..H {
                if !borders_blob(&grid, xx, zz, yy) {
                    continue;
                }
                let (px, py, pz) = (origin.x + xx as i32, origin.y + yy as i32, origin.z + zz as i32);
                let here = level.get(px, py, pz);
                if yy >= 4 && env.blocks.liquid(here) {
                    return false;
                }
                if yy < 4 && !env.blocks.solid(here) && here != fluid {
                    return false;
                }
                if !cfg.can_place_feature.test(level, px, py, pz) {
                    return false;
                }
            }
        }
    }
    for xx in 0..W {
        for zz in 0..W {
            for yy in 0..H {
                if grid[cell(xx, zz, yy)] {
                    let (px, py, pz) = (origin.x + xx as i32, origin.y + yy as i32, origin.z + zz as i32);
                    if cfg.can_replace_with_air_or_fluid.test(level, px, py, pz) {
                        level.set(px, py, pz, if yy >= 4 { env.known.cave_air } else { fluid });
                    }
                }
            }
        }
    }
    let barrier = cfg.barrier.get(level, rng, origin.x, origin.y, origin.z);
    if !env.blocks.is_air(barrier) {
        for xx in 0..W {
            for zz in 0..W {
                for yy in 0..H {
                    if borders_blob(&grid, xx, zz, yy) && (yy < 4 || rng.next_int_bounded(2) != 0) {
                        let (px, py, pz) = (origin.x + xx as i32, origin.y + yy as i32, origin.z + zz as i32);
                        if env.blocks.solid(level.get(px, py, pz)) && cfg.can_replace_with_barrier.test(level, px, py, pz) {
                            level.set(px, py, pz, barrier);
                        }
                    }
                }
            }
        }
    }
    if env.blocks.fluid(fluid) == FluidKind::Water {
        for xx in 0..W {
            for zz in 0..W {
                let (px, py, pz) = (origin.x + xx as i32, origin.y + 4, origin.z + zz as i32);
                if climate::should_freeze(level, px, py, pz, false) && cfg.can_replace_with_air_or_fluid.test(level, px, py, pz) {
                    level.set(px, py, pz, env.known.ice);
                }
            }
        }
    }
    true
}

#[derive(Clone, Debug)]
pub struct SpringConfig {
    /// The fluid block the spring places (the legacy block of the configured fluid state).
    pub state: State,
    pub requires_block_below: bool,
    pub rock_count: i32,
    pub hole_count: i32,
    pub valid_blocks: BlockMatch,
}

impl SpringConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let fluid = get(v, "state", ctx)?;
        let name = fluid.get("id").and_then(Value::as_str).ok_or_else(|| format!("{ctx}: fluid state without id"))?;
        // The configured fluid is the source fluid (the `falling` flag does not change the block a
        // source becomes), so the block is the source block, level 0.
        let state = env.blocks.state_by_name(&format!("{name}[level=0]"))?;
        let valid = get(v, "valid_blocks", ctx)?;
        let _ = array;
        Ok(Self {
            state,
            requires_block_below: boolean(v, "requires_block_below", true, ctx)?,
            rock_count: int_or(v, "rock_count", 4, ctx)?,
            hole_count: int_or(v, "hole_count", 1, ctx)?,
            valid_blocks: BlockMatch::parse(env, valid, ctx)?,
        })
    }
}

pub fn place_spring(cfg: &SpringConfig, level: &mut Level<'_>, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    let valid = |p: Pos| cfg.valid_blocks.contains(blocks.block_of(level.get(p.x, p.y, p.z)));
    let empty = |p: Pos| blocks.is_air(level.get(p.x, p.y, p.z));
    if !valid(origin.above()) {
        return false;
    }
    if cfg.requires_block_below && !valid(origin.below()) {
        return false;
    }
    let here = level.get(origin.x, origin.y, origin.z);
    if !blocks.is_air(here) && !cfg.valid_blocks.contains(blocks.block_of(here)) {
        return false;
    }
    let sides = [origin.offset(-1, 0, 0), origin.offset(1, 0, 0), origin.offset(0, 0, -1), origin.offset(0, 0, 1), origin.below()];
    let rocks = sides.iter().filter(|p| valid(**p)).count() as i32;
    let holes = sides.iter().filter(|p| empty(**p)).count() as i32;
    if rocks == cfg.rock_count && holes == cfg.hole_count {
        level.set(origin.x, origin.y, origin.z, cfg.state);
        return true;
    }
    false
}
