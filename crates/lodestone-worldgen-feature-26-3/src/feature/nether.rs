//! Nether features: `netherrack_replace_blobs`, `delta_feature`, `huge_fungus`,
//! `random_neighbor_spread`, `single_block_pillar`, `stepped_column_cluster` and
//! `projected_random_patchy_square`.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::{BlockId, Dir, State};
use crate::env::Env;
use crate::json::{Res, boolean, float_or, get, int, string};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::{BlockMatch, BlockPred, state_of};
use crate::provider::IntProvider;
use crate::registry::{Loader, PlacedFeature};
use crate::stateprovider::StateProvider;

/// `a` when `a >= b`, else a uniform draw from `a..=b`.
fn next_int_between(rng: &mut Rng, a: i32, b: i32) -> i32 {
    if a >= b { a } else { rng.next_int_bounded(b - a + 1) + a }
}

/// The positions of a box around `origin` in order of increasing Manhattan distance: depth by
/// depth, `x` ascending, then `y` ascending, each `z` offset visited positive before negative.
pub fn manhattan_ordered(origin: Pos, rx: i32, ry: i32, rz: i32) -> Vec<Pos> {
    let mut out = Vec::new();
    for depth in 0..=rx + ry + rz {
        let max_x = rx.min(depth);
        for x in -max_x..=max_x {
            let max_y = ry.min(depth - x.abs());
            for y in -max_y..=max_y {
                let z = depth - x.abs() - y.abs();
                if z <= rz {
                    out.push(origin.offset(x, y, z));
                    if z != 0 {
                        out.push(origin.offset(x, y, -z));
                    }
                }
            }
        }
    }
    out
}

fn dist_manhattan(a: Pos, b: Pos) -> i32 {
    (a.x - b.x).abs() + (a.y - b.y).abs() + (a.z - b.z).abs()
}

fn block_state(env: &Env, v: &Value, key: &str, ctx: &str) -> Res<State> {
    state_of(env, get(v, key, ctx)?, ctx)
}

#[derive(Clone, Debug)]
pub struct ReplaceBlobsConfig {
    pub target: BlockId,
    pub state: State,
    pub radius: IntProvider,
}

impl ReplaceBlobsConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            target: env.blocks.block_of(block_state(env, v, "target", ctx)?),
            state: block_state(env, v, "state", ctx)?,
            radius: IntProvider::parse(get(v, "radius", ctx)?, ctx)?,
        })
    }
}

/// Finds the target block at or below the origin (clamped into the level), then replaces it
/// inside a Manhattan blob whose three radii are drawn independently.
pub fn place_replace_blobs(cfg: &ReplaceBlobsConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let blocks = &level.env.blocks;
    let mut cursor = origin.at_y(origin.y.clamp(level.min_y + 1, level.max_y()));
    let center = loop {
        if cursor.y <= level.min_y + 1 {
            return false;
        }
        if blocks.block_of(level.get(cursor.x, cursor.y, cursor.z)) == cfg.target {
            break cursor;
        }
        cursor = cursor.below();
    };
    let rx = cfg.radius.sample(rng);
    let ry = cfg.radius.sample(rng);
    let rz = cfg.radius.sample(rng);
    let max = rx.max(ry).max(rz);
    let mut any = false;
    for p in manhattan_ordered(center, rx, ry, rz) {
        if dist_manhattan(p, center) > max {
            break;
        }
        if blocks.block_of(level.get(p.x, p.y, p.z)) == cfg.target {
            level.set(p.x, p.y, p.z, cfg.state);
            any = true;
        }
    }
    any
}

#[derive(Clone, Debug)]
pub struct DeltaConfig {
    pub contents: State,
    pub rim: State,
    pub size: IntProvider,
    pub rim_size: IntProvider,
}

impl DeltaConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            contents: block_state(env, v, "contents", ctx)?,
            rim: block_state(env, v, "rim", ctx)?,
            size: IntProvider::parse(get(v, "size", ctx)?, ctx)?,
            rim_size: IntProvider::parse(get(v, "rim_size", ctx)?, ctx)?,
        })
    }
}

/// Blocks a delta never overwrites (fortress pieces and their contents, and bedrock).
const DELTA_PROTECTED: [&str; 7] = ["bedrock", "nether_bricks", "nether_brick_fence", "nether_brick_stairs", "nether_wart", "chest", "spawner"];

pub fn place_delta(cfg: &DeltaConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    let contents = blocks.block_of(cfg.contents);
    let protected: Vec<BlockId> = DELTA_PROTECTED.iter().map(|n| blocks.block_by_name(n).expect("delta block")).collect();
    // A position is clear when it is not already contents, not protected, and is roofed by air
    // with every other neighbour solid.
    let clear = |level: &Level<'_>, p: Pos| {
        let b = blocks.block_of(level.get(p.x, p.y, p.z));
        if b == contents || protected.contains(&b) {
            return false;
        }
        Dir::ALL.iter().all(|&d| {
            let n = p.relative(d);
            blocks.is_air(level.get(n.x, n.y, n.z)) == (d == Dir::Up)
        })
    };
    let spawn_rim = rng.next_double() < 0.9;
    let rim_x = if spawn_rim { cfg.rim_size.sample(rng) } else { 0 };
    let rim_z = if spawn_rim { cfg.rim_size.sample(rng) } else { 0 };
    let has_rim = spawn_rim && rim_x != 0 && rim_z != 0;
    let rx = cfg.size.sample(rng);
    let rz = cfg.size.sample(rng);
    let limit = rx.max(rz);
    let mut any = false;
    for p in manhattan_ordered(origin, rx, 0, rz) {
        if dist_manhattan(p, origin) > limit {
            break;
        }
        if clear(level, p) {
            if has_rim {
                any = true;
                level.set(p.x, p.y, p.z, cfg.rim);
            }
            let q = p.offset(rim_x, 0, rim_z);
            if clear(level, q) {
                any = true;
                level.set(q.x, q.y, q.z, cfg.contents);
            }
        }
    }
    any
}

#[derive(Clone, Debug)]
pub struct FungusConfig {
    pub valid_base: BlockId,
    pub stem: State,
    pub hat: State,
    pub decor: State,
    pub replaceable: BlockPred,
    pub planted: bool,
}

impl FungusConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            valid_base: env.blocks.block_of(block_state(env, v, "valid_base_block", ctx)?),
            stem: block_state(env, v, "stem_state", ctx)?,
            hat: block_state(env, v, "hat_state", ctx)?,
            decor: block_state(env, v, "decor_state", ctx)?,
            replaceable: BlockPred::parse(env, get(v, "replaceable_blocks", ctx)?, ctx)?,
            planted: boolean(v, "planted", false, ctx)?,
        })
    }
}

/// A huge crimson or warped fungus: a stem (three by three, corners sparse, when huge) and a
/// layered hat whose bottom rows hang down and whose crimson blocks may grow weeping vines.
pub fn place_huge_fungus(cfg: &FungusConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    if blocks.block_of(level.get(origin.x, origin.y - 1, origin.z)) != cfg.valid_base {
        return false;
    }
    let mut height = next_int_between(rng, 4, 13);
    if rng.next_int_bounded(12) == 0 {
        height *= 2;
    }
    if !cfg.planted && origin.y + height + 1 >= level.gen_depth {
        return false;
    }
    let huge = !cfg.planted && rng.next_float() < 0.06;
    level.set(origin.x, origin.y, origin.z, env.known.air);

    let replaceable = |level: &Level<'_>, p: Pos, plants: bool| {
        blocks.replaceable(level.get(p.x, p.y, p.z)) || (plants && cfg.replaceable.test(level, p.x, p.y, p.z))
    };
    // A planted fungus breaks whatever stands on a non-air block before overwriting it.
    let clear_planted = |level: &mut Level<'_>, p: Pos| {
        if cfg.planted && !blocks.is_air(level.get(p.x, p.y - 1, p.z)) {
            level.set(p.x, p.y, p.z, env.known.air);
        }
    };

    let radius = i32::from(huge);
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let corner = huge && dx.abs() == radius && dz.abs() == radius;
            for dy in 0..height {
                let p = origin.offset(dx, dy, dz);
                if !replaceable(level, p, true) {
                    continue;
                }
                if cfg.planted {
                    clear_planted(level, p);
                    level.set(p.x, p.y, p.z, cfg.stem);
                } else if corner {
                    if rng.next_float() < 0.1 {
                        level.set(p.x, p.y, p.z, cfg.stem);
                    }
                } else {
                    level.set(p.x, p.y, p.z, cfg.stem);
                }
            }
        }
    }

    let hat_block = blocks.block_of(cfg.hat);
    let vines = blocks.block_name(hat_block) == "minecraft:nether_wart_block";
    let hat_height = (rng.next_int_bounded(1 + height / 3) + 5).min(height);
    let hat_start = height - hat_height;
    for dy in hat_start..=height {
        let mut r = if dy < height - rng.next_int_bounded(3) { 2 } else { 1 };
        if hat_height > 8 && dy < hat_start + 4 {
            r = 3;
        }
        if huge {
            r += 1;
        }
        for dx in -r..=r {
            for dz in -r..=r {
                let edge_x = dx == -r || dx == r;
                let edge_z = dz == -r || dz == r;
                let inside = !edge_x && !edge_z && dy != height;
                let corner = edge_x && edge_z;
                let bottom = dy < hat_start + 3;
                let p = origin.offset(dx, dy, dz);
                if !replaceable(level, p, false) {
                    continue;
                }
                clear_planted(level, p);
                if bottom {
                    if !inside {
                        if blocks.block_of(level.get(p.x, p.y - 1, p.z)) == hat_block {
                            level.set(p.x, p.y, p.z, cfg.hat);
                        } else if f64::from(rng.next_float()) < 0.15 {
                            level.set(p.x, p.y, p.z, cfg.hat);
                            if vines && rng.next_int_bounded(11) == 0 {
                                hat_vines(level, rng, p);
                            }
                        }
                    }
                } else {
                    let (decor, hat, vine) = if inside {
                        (0.1, 0.2, if vines { 0.1 } else { 0.0 })
                    } else if corner {
                        (0.01, 0.7, if vines { 0.083 } else { 0.0 })
                    } else {
                        (5.0e-4, 0.98, if vines { 0.07 } else { 0.0 })
                    };
                    if rng.next_float() < decor {
                        level.set(p.x, p.y, p.z, cfg.decor);
                    } else if rng.next_float() < hat {
                        level.set(p.x, p.y, p.z, cfg.hat);
                        if rng.next_float() < vine {
                            hat_vines(level, rng, p);
                        }
                    }
                }
            }
        }
    }
    true
}

/// A weeping-vine column under a hat block: one to five blocks (doubled one time in seven), the
/// tip aged 23..=25, stopping early above the first non-empty block.
fn hat_vines(level: &mut Level<'_>, rng: &mut Rng, hat: Pos) {
    let blocks = &level.env.blocks;
    let start = hat.below();
    if !blocks.is_air(level.get(start.x, start.y, start.z)) {
        return;
    }
    let mut goal = next_int_between(rng, 1, 5);
    if rng.next_int_bounded(7) == 0 {
        goal *= 2;
    }
    let tip = blocks.default_state(blocks.block_by_name("weeping_vines").expect("weeping_vines"));
    let plant = blocks.default_state(blocks.block_by_name("weeping_vines_plant").expect("weeping_vines_plant"));
    let mut p = start;
    for h in 0..=goal {
        if blocks.is_air(level.get(p.x, p.y, p.z)) {
            if h == goal || !blocks.is_air(level.get(p.x, p.y - 1, p.z)) {
                let age = next_int_between(rng, 23, 25).to_string();
                let s = blocks.with(tip, "age", &age).expect("vine age");
                level.set(p.x, p.y, p.z, s);
                break;
            }
            level.set(p.x, p.y, p.z, plant);
        }
        p = p.below();
    }
}

#[derive(Clone, Debug)]
pub struct NeighborSpreadConfig {
    pub block: StateProvider,
    pub accepted: BlockMatch,
    pub can_replace: BlockPred,
    pub attempts: IntProvider,
    pub xz: IntProvider,
    pub y: IntProvider,
}

impl NeighborSpreadConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            block: StateProvider::parse(env, get(v, "block", ctx)?, ctx)?,
            accepted: BlockMatch::parse(env, get(v, "accepted_neighbors", ctx)?, ctx)?,
            can_replace: BlockPred::parse(env, get(v, "can_replace", ctx)?, ctx)?,
            attempts: IntProvider::parse(get(v, "attempts", ctx)?, ctx)?,
            xz: IntProvider::parse(get(v, "xz_offset", ctx)?, ctx)?,
            y: IntProvider::parse(get(v, "y_offset", ctx)?, ctx)?,
        })
    }
}

/// Places the block at the origin, then at random nearby replaceable positions that touch
/// exactly one accepted neighbour (glowstone clusters, the crimson wart patches).
pub fn place_neighbor_spread(cfg: &NeighborSpreadConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let s = cfg.block.get(level, rng, origin.x, origin.y, origin.z);
    level.set(origin.x, origin.y, origin.z, s);
    let attempts = cfg.attempts.sample(rng);
    for _ in 0..attempts {
        let dx = cfg.xz.sample(rng);
        let dy = cfg.y.sample(rng);
        let dz = cfg.xz.sample(rng);
        let p = origin.offset(dx, dy, dz);
        if !cfg.can_replace.test(level, p.x, p.y, p.z) {
            continue;
        }
        let blocks = &level.env.blocks;
        let mut neighbours = 0;
        for d in Dir::ALL {
            let n = p.relative(d);
            if cfg.accepted.contains(blocks.block_of(level.get(n.x, n.y, n.z))) {
                neighbours += 1;
            }
            if neighbours > 1 {
                break;
            }
        }
        if neighbours == 1 {
            let s = cfg.block.get(level, rng, p.x, p.y, p.z);
            level.set(p.x, p.y, p.z, s);
        }
    }
    true
}

#[derive(Clone, Debug)]
pub struct PillarConfig {
    pub block: StateProvider,
    pub can_replace: BlockPred,
    pub direction: Dir,
    pub chance_to_continue: f32,
    pub cap: Option<Arc<PlacedFeature>>,
}

impl PillarConfig {
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            block: StateProvider::parse(env, get(v, "block", ctx)?, ctx)?,
            can_replace: match v.get("can_replace") {
                Some(p) => BlockPred::parse(env, p, ctx)?,
                None => BlockPred::AlwaysTrue,
            },
            direction: match string(v, "direction", ctx)? {
                "up" => Dir::Up,
                "down" => Dir::Down,
                d => return Err(format!("{ctx}: pillar direction {d}")),
            },
            chance_to_continue: float_or(v, "chance_to_continue", 1.0, ctx)?,
            cap: v.get("cap_feature").map(|c| loader.placed_ref(env, c, ctx)).transpose()?,
        })
    }
}

/// A one-block column grown while the next position is replaceable and a continue roll
/// succeeds, then the cap feature at its last block.
pub fn place_pillar(cfg: &PillarConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let mut p = origin;
    while cfg.can_replace.test(level, p.x, p.y, p.z) && rng.next_float() < cfg.chance_to_continue && !level.is_outside_build_height(p.y) {
        let s = cfg.block.get(level, rng, p.x, p.y, p.z);
        level.set(p.x, p.y, p.z, s);
        p = p.relative(cfg.direction);
    }
    p = p.relative(cfg.direction.opposite());
    if let Some(cap) = &cfg.cap {
        cap.place_nested(level, rng, p);
    }
    true
}

#[derive(Clone, Debug)]
pub struct PatchySquareConfig {
    pub block: StateProvider,
    pub project_through: BlockPred,
    pub size: IntProvider,
    pub max_projection: i32,
}

impl PatchySquareConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            block: StateProvider::parse(env, get(v, "block", ctx)?, ctx)?,
            project_through: BlockPred::parse(env, get(v, "project_through", ctx)?, ctx)?,
            size: IntProvider::parse(get(v, "size", ctx)?, ctx)?,
            max_projection: int(v, "max_projection_height", ctx)?,
        })
    }
}

/// A square whose cells are kept with a chance falling off with `|dx| * |dz|`, each dropped
/// through the projectable blocks below it (at most the projection height) before placing.
pub fn place_patchy_square(cfg: &PatchySquareConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let size = cfg.size.sample(rng);
    let bound = size * size + 1;
    for dx in -size..=size {
        for dz in -size..=size {
            let probability = dx.abs() * dz.abs();
            if rng.next_int_bounded(bound) >= bound - probability {
                continue;
            }
            let mut p = origin.offset(dx, 0, dz);
            let mut drop = cfg.max_projection;
            while cfg.project_through.test(level, p.x, p.y - 1, p.z) {
                p = p.below();
                drop -= 1;
                if drop <= 0 {
                    break;
                }
            }
            if let Some(s) = cfg.block.get_optional(level, rng, p.x, p.y, p.z) {
                level.set(p.x, p.y, p.z, s);
            }
        }
    }
    true
}

#[derive(Clone, Debug)]
pub struct ColumnClusterConfig {
    pub block: StateProvider,
    pub continue_through: BlockPred,
    pub can_replace: BlockPred,
    pub cannot_place_on: BlockMatch,
    pub cluster_reach: IntProvider,
    pub column_count: IntProvider,
    pub column_reach: IntProvider,
    pub height: IntProvider,
}

impl ColumnClusterConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            block: StateProvider::parse(env, get(v, "block", ctx)?, ctx)?,
            continue_through: BlockPred::parse(env, get(v, "continue_through", ctx)?, ctx)?,
            can_replace: BlockPred::parse(env, get(v, "can_replace", ctx)?, ctx)?,
            cannot_place_on: BlockMatch::parse(env, get(v, "cannot_place_on", ctx)?, ctx)?,
            cluster_reach: IntProvider::parse(get(v, "cluster_reach", ctx)?, ctx)?,
            column_count: IntProvider::parse(get(v, "column_count", ctx)?, ctx)?,
            column_reach: IntProvider::parse(get(v, "column_reach", ctx)?, ctx)?,
            height: IntProvider::parse(get(v, "height", ctx)?, ctx)?,
        })
    }

    fn can_place_at(&self, level: &Level<'_>, p: Pos) -> bool {
        if !self.can_replace.test(level, p.x, p.y, p.z) {
            return false;
        }
        let below = level.get(p.x, p.y - 1, p.z);
        let blocks = &level.env.blocks;
        !blocks.is_air(below) && !self.cannot_place_on.contains(blocks.block_of(below))
    }

    fn find_surface(&self, level: &Level<'_>, mut p: Pos, mut limit: i32) -> Option<Pos> {
        while p.y > level.min_y + 1 && limit > 0 {
            limit -= 1;
            if self.can_place_at(level, p) {
                return Some(p);
            }
            p = p.below();
        }
        None
    }

    fn find_air(&self, level: &Level<'_>, mut p: Pos, mut limit: i32) -> Option<Pos> {
        let blocks = &level.env.blocks;
        while p.y <= level.max_y() && limit > 0 {
            limit -= 1;
            let s = level.get(p.x, p.y, p.z);
            if self.cannot_place_on.contains(blocks.block_of(s)) {
                return None;
            }
            if blocks.is_air(s) {
                return Some(p);
            }
            p = p.above();
        }
        None
    }

    fn place_column(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos, height: i32, reach: i32) -> bool {
        let mut any = false;
        for z in origin.z - reach..=origin.z + reach {
            for x in origin.x - reach..=origin.x + reach {
                let p = Pos::new(x, origin.y, z);
                let step = dist_manhattan(p, origin);
                let start = if self.can_replace.test(level, p.x, p.y, p.z) {
                    self.find_surface(level, p, step)
                } else {
                    self.find_air(level, p, step)
                };
                let Some(mut c) = start else { continue };
                let mut left = height - step / 2;
                while left >= 0 {
                    if self.can_replace.test(level, c.x, c.y, c.z) {
                        let s = self.block.get(level, rng, c.x, c.y, c.z);
                        level.set(c.x, c.y, c.z, s);
                        c = c.above();
                        any = true;
                    } else {
                        if !self.continue_through.test(level, c.x, c.y, c.z) {
                            break;
                        }
                        c = c.above();
                    }
                    left -= 1;
                }
            }
        }
        any
    }
}

/// A cluster of basalt columns: random column sites around the origin, each a small stepped
/// mound grown up from the floor (or from the first air above a non-replaceable block).
pub fn place_column_cluster(cfg: &ColumnClusterConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if !cfg.can_place_at(level, origin) {
        return false;
    }
    let height = cfg.height.sample(rng);
    let reach = height.min(cfg.cluster_reach.sample(rng));
    let count = cfg.column_count.sample(rng);
    let width = 2 * reach + 1;
    let mut placed = false;
    for _ in 0..count {
        let x = origin.x - reach + rng.next_int_bounded(width);
        let y = origin.y + rng.next_int_bounded(1);
        let z = origin.z - reach + rng.next_int_bounded(width);
        let p = Pos::new(x, y, z);
        let left = height - dist_manhattan(p, origin);
        if left >= 0 {
            let column_reach = cfg.column_reach.sample(rng);
            placed |= cfg.place_column(level, rng, p, left, column_reach);
        }
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visiting order of a 1x1x1 Manhattan box, written out by hand from the rule: depth 0,
    /// then depth 1 by ascending `x`, then `y`, positive `z` before its mirror.
    #[test]
    fn manhattan_order_is_depth_then_x_then_y() {
        let got = manhattan_ordered(Pos::new(0, 0, 0), 1, 1, 1);
        let want = [
            (0, 0, 0),
            (-1, 0, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
            (0, 1, 0),
            (1, 0, 0),
        ];
        let got: Vec<_> = got.iter().take(7).map(|p| (p.x, p.y, p.z)).collect();
        assert_eq!(got, want);
    }
}
