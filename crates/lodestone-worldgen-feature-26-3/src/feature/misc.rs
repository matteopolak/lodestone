//! Small single-purpose features: `vines`, `bamboo`, `underwater_magma` and `block_blob`.

use serde_json::Value;

use crate::blocks::{Dir, Support};
use crate::env::{Env, Heightmap};
use crate::json::{Res, float, get, int};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::BlockPred;
use crate::stateprovider::StateProvider;
use crate::survive;
use lodestone_worldgen_core::rng::RandomSource;

/// Whether a vine (or any multiface block) can attach to the block at `p` in direction `d`:
/// the neighbour's face toward the block must be full.
pub fn can_attach(level: &Level<'_>, p: Pos, d: Dir) -> bool {
    let blocks = &level.env.blocks;
    let n = level.get(p.x + d.step().0, p.y + d.step().1, p.z + d.step().2);
    blocks.face_sturdy(n, d.opposite(), Support::Full) || blocks.full_collision(n)
}

pub fn place_vines(level: &mut Level<'_>, origin: Pos) -> bool {
    let env = level.env;
    if !env.blocks.is_air(level.get(origin.x, origin.y, origin.z)) {
        return false;
    }
    for d in Dir::ALL {
        if d != Dir::Down && can_attach(level, origin, d) {
            let blocks = &env.blocks;
            let vine = blocks.default_state(blocks.block_by_name("vine").expect("vine"));
            let s = blocks.with(vine, d.name(), "true").expect("vine face");
            level.set(origin.x, origin.y, origin.z, s);
            return true;
        }
    }
    false
}

pub fn place_bamboo(probability: f32, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    if !blocks.is_air(level.get(origin.x, origin.y, origin.z)) {
        return false;
    }
    let bamboo = blocks.block_by_name("bamboo").expect("bamboo");
    if !survive::can_survive(level, blocks.default_state(bamboo), origin.x, origin.y, origin.z) {
        return true;
    }
    let height = rng.next_int_bounded(12) + 5;
    if rng.next_float() < probability {
        let r = rng.next_int_bounded(4) + 1;
        let podzol = blocks.default_state(blocks.block_by_name("podzol").expect("podzol"));
        let replaceable = env.tags.get("beneath_bamboo_podzol_replaceable").expect("tag");
        for xx in origin.x - r..=origin.x + r {
            for zz in origin.z - r..=origin.z + r {
                let (xd, zd) = (xx - origin.x, zz - origin.z);
                if xd * xd + zd * zd <= r * r {
                    let y = level.height(Heightmap::WorldSurface, xx, zz) - 1;
                    if replaceable.contains(blocks.block_of(level.get(xx, y, zz))) {
                        level.set(xx, y, zz, podzol);
                    }
                }
            }
        }
    }
    let base = blocks.default_state(bamboo);
    let trunk = blocks.with(blocks.with(blocks.with(base, "age", "1").expect("age"), "leaves", "none").expect("leaves"), "stage", "0").expect("stage");
    let with_leaves = |leaves: &str, stage: &str| {
        blocks.with(blocks.with(trunk, "leaves", leaves).expect("leaves"), "stage", stage).expect("stage")
    };
    let mut p = origin;
    let mut i = 0;
    while i < height && blocks.is_air(level.get(p.x, p.y, p.z)) {
        level.set(p.x, p.y, p.z, trunk);
        p = p.above();
        i += 1;
    }
    if p.y - origin.y >= 3 {
        level.set(p.x, p.y, p.z, with_leaves("large", "1"));
        p = p.below();
        level.set(p.x, p.y, p.z, with_leaves("large", "0"));
        p = p.below();
        level.set(p.x, p.y, p.z, with_leaves("small", "0"));
    }
    true
}

#[derive(Clone, Debug)]
pub struct MagmaConfig {
    pub floor_search_range: i32,
    pub radius: i32,
    pub probability: f32,
}

impl MagmaConfig {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            floor_search_range: int(v, "floor_search_range", ctx)?,
            radius: int(v, "placement_radius_around_floor", ctx)?,
            probability: float(v, "placement_probability_per_valid_position", ctx)?,
        })
    }
}

pub fn place_underwater_magma(cfg: &MagmaConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    let water = blocks.block_by_name("water").expect("water");
    let is_water = |level: &Level<'_>, p: Pos| blocks.block_of(level.get(p.x, p.y, p.z)) == water;
    if !is_water(level, origin) {
        return false;
    }
    // Scan down from the origin through water, at most the search range, to a non-water edge.
    let mut p = origin;
    let mut i = 1;
    while i < cfg.floor_search_range && is_water(level, p) {
        p = p.below();
        i += 1;
    }
    if is_water(level, p) {
        return false;
    }
    let floor_y = p.y;
    let r = cfg.radius;
    let (cx, cy, cz) = (origin.x, floor_y, origin.z);
    let magma = blocks.default_state(blocks.block_by_name("magma_block").expect("magma_block"));
    let visible = |level: &Level<'_>, p: Pos| !blocks.can_occlude(level.get(p.x, p.y, p.z));
    let mut placed = 0;
    for z in cz - r..=cz + r {
        for y in cy - r..=cy + r {
            for x in cx - r..=cx + r {
                if rng.next_float() >= cfg.probability {
                    continue;
                }
                let pos = Pos::new(x, y, z);
                let s = level.get(x, y, z);
                if blocks.is_air(s) || blocks.block_of(s) == water || visible(level, pos.below()) {
                    continue;
                }
                if Dir::HORIZONTAL.iter().any(|d| visible(level, pos.relative(*d))) {
                    continue;
                }
                level.set(x, y, z, magma);
                placed += 1;
            }
        }
    }
    placed > 0
}

#[derive(Clone, Debug)]
pub struct BlobConfig {
    pub state: StateProvider,
    pub can_place_on: BlockPred,
}

impl BlobConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            state: StateProvider::parse(env, get(v, "state", ctx)?, ctx)?,
            can_place_on: BlockPred::parse(env, get(v, "can_place_on", ctx)?, ctx)?,
        })
    }
}

pub fn place_block_blob(cfg: &BlobConfig, level: &mut Level<'_>, rng: &mut Rng, mut origin: Pos) -> bool {
    while origin.y > level.min_y + 3 && !cfg.can_place_on.test(level, origin.x, origin.y - 1, origin.z) {
        origin = origin.below();
    }
    if origin.y <= level.min_y + 3 {
        return false;
    }
    for _ in 0..3 {
        let xr = rng.next_int_bounded(2);
        let yr = rng.next_int_bounded(2);
        let zr = rng.next_int_bounded(2);
        let tr = (xr + yr + zr) as f32 * 0.333f32 + 0.5f32;
        for z in origin.z - zr..=origin.z + zr {
            for y in origin.y - yr..=origin.y + yr {
                for x in origin.x - xr..=origin.x + xr {
                    let (dx, dy, dz) = (x - origin.x, y - origin.y, z - origin.z);
                    if ((dx * dx + dy * dy + dz * dz) as f32) <= tr * tr {
                        let s = cfg.state.get(level, rng, x, y, z);
                        level.set(x, y, z, s);
                    }
                }
            }
        }
        origin = origin.offset(-1 + rng.next_int_bounded(2), -rng.next_int_bounded(2), -1 + rng.next_int_bounded(2));
    }
    true
}
