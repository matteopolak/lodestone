//! Tree decorators: blocks added around a finished tree (vines, hives, ground cover).
//!
//! Decorators read the trunk and foliage position sets in the reference's hash-set iteration
//! order, stably sorted by height ([`super::sorted_by_y`]); the draw order inside each one is the
//! contract with the oracle.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use std::sync::Arc;

use super::{Run, sorted_by_y};
use crate::feature::Feature;
use crate::registry::Loader;
use crate::blocks::{Dir, State};
use crate::env::{Env, Heightmap};
use crate::json::{Res, array, float, get, int, int_or, type_of};
use crate::pos::Pos;
use crate::stateprovider::StateProvider;

#[derive(Clone, Debug)]
pub enum Decorator {
    Beehive { probability: f32 },
    TrunkVine,
    LeaveVine { probability: f32 },
    Cocoa { probability: f32 },
    PlaceOnGround { tries: i32, radius: i32, height: i32, provider: StateProvider },
    AlterGround { provider: StateProvider },
    AttachedToLogs { probability: f32, provider: StateProvider, directions: Vec<Dir> },
    AttachedToLeaves {
        probability: f32,
        exclusion_xz: i32,
        exclusion_y: i32,
        provider: StateProvider,
        required_empty: i32,
        directions: Vec<Dir>,
    },
    ShelfMushroom { probability: f32 },
    CreakingHeart { probability: f32 },
    PaleMoss { leaves: f32, trunk: f32, ground: f32, patch: Arc<Feature> },
}

fn directions(v: &Value, ctx: &str) -> Res<Vec<Dir>> {
    let mut out = Vec::new();
    for d in array(v, "directions", ctx)? {
        let n = d.as_str().unwrap_or_default();
        out.push(Dir::from_name(n).ok_or_else(|| format!("{ctx}: unknown direction `{n}`"))?);
    }
    Ok(out)
}

impl Decorator {
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, ctx: &str, unsupported: &mut Vec<String>) -> Res<Self> {
        Ok(match type_of(v, ctx)? {
            "beehive" => Self::Beehive { probability: float(v, "probability", ctx)? },
            "trunk_vine" => Self::TrunkVine,
            "leave_vine" => Self::LeaveVine { probability: float(v, "probability", ctx)? },
            "cocoa" => Self::Cocoa { probability: float(v, "probability", ctx)? },
            "place_on_ground" => Self::PlaceOnGround {
                tries: int_or(v, "tries", 128, ctx)?,
                radius: int_or(v, "radius", 2, ctx)?,
                height: int_or(v, "height", 1, ctx)?,
                provider: StateProvider::parse(env, get(v, "block_state_provider", ctx)?, ctx)?,
            },
            "alter_ground" => Self::AlterGround { provider: StateProvider::parse(env, get(v, "provider", ctx)?, ctx)? },
            "attached_to_logs" => Self::AttachedToLogs {
                probability: float(v, "probability", ctx)?,
                provider: StateProvider::parse(env, get(v, "block_provider", ctx)?, ctx)?,
                directions: directions(v, ctx)?,
            },
            "attached_to_leaves" => Self::AttachedToLeaves {
                probability: float(v, "probability", ctx)?,
                exclusion_xz: int(v, "exclusion_radius_xz", ctx)?,
                exclusion_y: int(v, "exclusion_radius_y", ctx)?,
                provider: StateProvider::parse(env, get(v, "block_provider", ctx)?, ctx)?,
                required_empty: int(v, "required_empty_blocks", ctx)?,
                directions: directions(v, ctx)?,
            },
            "shelf_mushroom" => Self::ShelfMushroom { probability: float(v, "probability", ctx)? },
            "creaking_heart" => Self::CreakingHeart { probability: float(v, "probability", ctx)? },
            "pale_moss" => Self::PaleMoss {
                leaves: float(v, "leaves_probability", ctx)?,
                trunk: float(v, "trunk_probability", ctx)?,
                ground: float(v, "ground_probability", ctx)?,
                patch: loader.named_feature("pale_moss_patch")?,
            },
            other => {
                unsupported.push(format!("decorator {other}"));
                Self::TrunkVine
            }
        })
    }

    pub fn place(&self, run: &mut Run<'_, '_>) {
        let logs = sorted_by_y(&run.trunks);
        let leaves = sorted_by_y(&run.foliage);
        match self {
            Self::Beehive { probability } => beehive(run, &logs, &leaves, *probability),
            Self::TrunkVine => {
                for p in &logs {
                    for (d, face) in [(Dir::West, "east"), (Dir::East, "west"), (Dir::North, "south"), (Dir::South, "north")] {
                        if run.rng.next_int_bounded(3) > 0 {
                            let n = p.relative(d);
                            if run.is_air(n) {
                                place_vine(run, n, face);
                            }
                        }
                    }
                }
            }
            Self::LeaveVine { probability } => {
                for p in &leaves {
                    for (d, face) in [(Dir::West, "east"), (Dir::East, "west"), (Dir::North, "south"), (Dir::South, "north")] {
                        if run.rng.next_float() < *probability {
                            let n = p.relative(d);
                            if run.is_air(n) {
                                place_vine(run, n, face);
                                let mut max = 4;
                                let mut below = n.below();
                                while run.is_air(below) && max > 0 {
                                    place_vine(run, below, face);
                                    below = below.below();
                                    max -= 1;
                                }
                            }
                        }
                    }
                }
            }
            Self::Cocoa { probability } => cocoa(run, &logs, *probability),
            Self::PlaceOnGround { tries, radius, height, provider } => place_on_ground(run, *tries, *radius, *height, provider),
            Self::AlterGround { provider } => alter_ground(run, provider),
            Self::ShelfMushroom { probability } => shelf_mushroom(run, &logs, *probability),
            Self::CreakingHeart { probability } => creaking_heart(run, &logs, *probability),
            Self::PaleMoss { leaves: lp, trunk, ground, patch } => pale_moss(run, &logs, &leaves, *lp, *trunk, *ground, patch),
            Self::AttachedToLeaves { probability, exclusion_xz, exclusion_y, provider, required_empty, directions } => {
                let mut shuffled = leaves;
                super::trunk::shuffle(&mut shuffled, run.rng);
                let mut blacklist: std::collections::HashSet<Pos> = std::collections::HashSet::new();
                for leaf in shuffled {
                    let d = directions[run.rng.next_int_bounded(directions.len() as i32) as usize];
                    let at = leaf.relative(d);
                    if !blacklist.contains(&at)
                        && run.rng.next_float() < *probability
                        && (1..=*required_empty).all(|i| run.is_air(leaf.relative_n(d, i)))
                    {
                        for x in -exclusion_xz..=*exclusion_xz {
                            for y in -exclusion_y..=*exclusion_y {
                                for z in -exclusion_xz..=*exclusion_xz {
                                    blacklist.insert(at.offset(x, y, z));
                                }
                            }
                        }
                        let s = provider.get(run.level, run.rng, at.x, at.y, at.z);
                        run.set_decoration(at, s);
                    }
                }
            }
            Self::AttachedToLogs { probability, provider, directions } => {
                let mut shuffled = logs;
                super::trunk::shuffle(&mut shuffled, run.rng);
                for p in shuffled {
                    let d = directions[run.rng.next_int_bounded(directions.len() as i32) as usize];
                    let at = p.relative(d);
                    if run.rng.next_float() <= *probability && run.is_air(at) {
                        let s = provider.get(run.level, run.rng, at.x, at.y, at.z);
                        run.set_decoration(at, s);
                    }
                }
            }
        }
    }
}

fn place_vine(run: &mut Run<'_, '_>, p: Pos, face: &str) {
    let blocks = &run.env.blocks;
    let vine = blocks.default_state(blocks.block_by_name("vine").expect("vine"));
    let s = blocks.with(vine, face, "true").expect("vine face");
    run.set_decoration(p, s);
}

fn beehive(run: &mut Run<'_, '_>, logs: &[Pos], leaves: &[Pos], probability: f32) {
    if logs.is_empty() || run.rng.next_float() >= probability {
        return;
    }
    let hive_y = if let Some(first_leaf) = leaves.first() {
        (first_leaf.y - 1).max(logs[0].y + 1)
    } else {
        (logs[0].y + 1 + run.rng.next_int_bounded(3)).min(logs[logs.len() - 1].y)
    };
    let mut placements: Vec<Pos> = Vec::new();
    for p in logs.iter().filter(|p| p.y == hive_y) {
        for d in [Dir::East, Dir::South, Dir::West] {
            placements.push(p.relative(d));
        }
    }
    if placements.is_empty() {
        return;
    }
    super::trunk::shuffle(&mut placements, run.rng);
    let found = placements.into_iter().find(|p| run.is_air(*p) && run.is_air(p.relative(Dir::South)));
    if let Some(p) = found {
        let blocks = &run.env.blocks;
        let nest = blocks.default_state(blocks.block_by_name("bee_nest").expect("bee_nest"));
        let s = blocks.with(nest, "facing", "south").expect("facing");
        run.set_decoration(p, s);
        // The hive block entity exists in the region, so its bees are drawn.
        let bees = 2 + run.rng.next_int_bounded(2);
        let bee_ticks = (0..bees).map(|_| run.rng.next_int_bounded(599)).collect();
        run.level.attach_block_entity(crate::level::PlacedBlockEntity::Beehive { x: p.x, y: p.y, z: p.z, bee_ticks });
    }
}

fn cocoa(run: &mut Run<'_, '_>, logs: &[Pos], probability: f32) {
    if run.rng.next_float() >= probability || logs.is_empty() {
        return;
    }
    let tree_y = logs[0].y;
    for p in logs.iter().filter(|p| p.y - tree_y <= 2) {
        for d in Dir::HORIZONTAL {
            if run.rng.next_float() <= 0.25f32 {
                let o = d.opposite();
                let (dx, _, dz) = o.step();
                let at = p.offset(dx, 0, dz);
                if run.is_air(at) {
                    let blocks = &run.env.blocks;
                    let base = blocks.default_state(blocks.block_by_name("cocoa").expect("cocoa"));
                    let age = run.rng.next_int_bounded(3).to_string();
                    let s = blocks.with(blocks.with(base, "age", &age).expect("age"), "facing", d.name()).expect("facing");
                    run.set_decoration(at, s);
                }
            }
        }
    }
}

/// The lowest trunk blocks (and roots at the same height), the anchor of the ground decorators.
fn lowest(run: &Run<'_, '_>) -> Vec<Pos> {
    let roots = sorted_by_y(&run.roots);
    let logs = sorted_by_y(&run.trunks);
    if roots.is_empty() {
        logs
    } else if !logs.is_empty() && roots[0].y == logs[0].y {
        logs.into_iter().chain(roots).collect()
    } else {
        roots
    }
}

fn place_on_ground(run: &mut Run<'_, '_>, tries: i32, radius: i32, height: i32, provider: &StateProvider) {
    let positions = lowest(run);
    let Some(origin) = positions.first().copied() else { return };
    let min_y = origin.y;
    let (mut min_x, mut max_x, mut min_z, mut max_z) = (origin.x, origin.x, origin.z, origin.z);
    for p in positions.iter().filter(|p| p.y == min_y) {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_z = min_z.min(p.z);
        max_z = max_z.max(p.z);
    }
    let (bx0, bx1, by0, by1, bz0, bz1) = (min_x - radius, max_x + radius, min_y - height, min_y + height, min_z - radius, max_z + radius);
    for _ in 0..tries {
        let x = run.rng.next_int_bounded(bx1 - bx0 + 1) + bx0;
        let y = run.rng.next_int_bounded(by1 - by0 + 1) + by0;
        let z = run.rng.next_int_bounded(bz1 - bz0 + 1) + bz0;
        let pos = Pos::new(x, y, z);
        let above = pos.above();
        let blocks = &run.env.blocks;
        let ok = {
            let a = run.get(above);
            let air_or_vine = blocks.is_air(a) || run.is_vine(above);
            air_or_vine && blocks.solid_render(run.get(pos)) && run.level.height(Heightmap::MotionBlockingNoLeaves, x, z) <= above.y
        };
        if ok {
            let s: State = provider.get(run.level, run.rng, above.x, above.y, above.z);
            run.set_decoration(above, s);
        }
    }
}

fn alter_ground(run: &mut Run<'_, '_>, provider: &StateProvider) {
    let positions = lowest(run);
    let Some(first) = positions.first().copied() else { return };
    for p in positions.iter().filter(|p| p.y == first.y) {
        place_circle(run, provider, p.offset(-1, 0, -1));
        place_circle(run, provider, p.offset(2, 0, -1));
        place_circle(run, provider, p.offset(-1, 0, 2));
        place_circle(run, provider, p.offset(2, 0, 2));
        for _ in 0..5 {
            let placement = run.rng.next_int_bounded(64);
            let (xx, zz) = (placement % 8, placement / 8);
            if xx == 0 || xx == 7 || zz == 0 || zz == 7 {
                place_circle(run, provider, p.offset(-3 + xx, 0, -3 + zz));
            }
        }
    }
}

fn place_circle(run: &mut Run<'_, '_>, provider: &StateProvider, p: Pos) {
    for xx in -2..=2i32 {
        for zz in -2..=2i32 {
            if xx.abs() != 2 || zz.abs() != 2 {
                place_block_at(run, provider, p.offset(xx, 0, zz));
            }
        }
    }
}

fn place_block_at(run: &mut Run<'_, '_>, provider: &StateProvider, p: Pos) {
    for dy in (-3..=2).rev() {
        let cursor = p.offset(0, dy, 0);
        if let Some(s) = provider.get_optional(run.level, run.rng, cursor.x, cursor.y, cursor.z) {
            run.set_decoration(cursor, s);
            break;
        }
        if !run.is_air(cursor) && dy < 0 {
            break;
        }
    }
}

fn block_is(run: &Run<'_, '_>, p: Pos, name: &str) -> bool {
    run.env.blocks.block_by_name(name) == Some(run.env.blocks.block_of(run.get(p)))
}

fn creaking_heart(run: &mut Run<'_, '_>, logs: &[Pos], probability: f32) {
    if logs.is_empty() || run.rng.next_float() >= probability {
        return;
    }
    let mut candidates = logs.to_vec();
    super::trunk::shuffle(&mut candidates, run.rng);
    let target = candidates
        .into_iter()
        .find(|p| Dir::ALL.iter().all(|d| run.logs.contains(run.env.blocks.block_of(run.get(p.relative(*d))))));
    if let Some(p) = target {
        let blocks = &run.env.blocks;
        let heart = blocks.default_state(blocks.block_by_name("creaking_heart").expect("creaking_heart"));
        let s = blocks.with(heart, "creaking_heart_state", "dormant").expect("state");
        let s = blocks.with(s, "natural", "true").expect("natural");
        run.set_decoration(p, s);
    }
}

fn pale_moss(run: &mut Run<'_, '_>, logs: &[Pos], leaves: &[Pos], leaves_p: f32, trunk_p: f32, ground_p: f32, patch: &Feature) {
    let mut shuffled = logs.to_vec();
    super::trunk::shuffle(&mut shuffled, run.rng);
    let Some(origin) = shuffled.iter().copied().reduce(|best, p| if p.y < best.y { p } else { best }) else {
        return;
    };
    if run.rng.next_float() < ground_p {
        patch.place(run.level, run.rng, origin.above());
    }
    for (list, chance) in [(logs, trunk_p), (leaves, leaves_p)] {
        for p in list {
            if run.rng.next_float() < chance {
                let down = p.below();
                if run.is_air(down) {
                    moss_hanger(run, down);
                }
            }
        }
    }
}

fn moss_hanger(run: &mut Run<'_, '_>, mut p: Pos) {
    let blocks = &run.env.blocks;
    let moss = blocks.default_state(blocks.block_by_name("pale_hanging_moss").expect("pale_hanging_moss"));
    let body = blocks.with(moss, "tip", "false").expect("tip");
    let tip = blocks.with(moss, "tip", "true").expect("tip");
    while run.is_air(p.below()) && run.rng.next_float() >= 0.5 {
        run.set_decoration(p, body);
        p = p.below();
    }
    run.set_decoration(p, tip);
}

fn shelf_replaceable(run: &Run<'_, '_>, p: Pos) -> bool {
    let water = |q: Pos| block_is(run, q, "water");
    run.env.blocks.replaceable(run.get(p))
        && !(water(p) || water(p.relative(Dir::East)) || water(p.relative(Dir::West)) || water(p.relative(Dir::North)) || water(p.relative(Dir::South)))
}

fn shelf_at(run: &Run<'_, '_>, p: Pos) -> bool {
    block_is(run, p, "shelf_mushroom")
}

fn shelf_beside(run: &Run<'_, '_>, p: Pos) -> bool {
    Dir::HORIZONTAL.iter().any(|d| shelf_at(run, p.relative(*d)))
}

fn place_shelf(run: &mut Run<'_, '_>, p: Pos, facing: Dir) {
    let age = run.rng.next_int_bounded(2);
    let blocks = &run.env.blocks;
    let shelf = blocks.default_state(blocks.block_by_name("shelf_mushroom").expect("shelf_mushroom"));
    let s = blocks.with(shelf, "age", &age.to_string()).expect("age");
    let s = blocks.with(s, "facing", facing.name()).expect("facing");
    run.set_decoration(p, s);
}

fn beside(p: Pos, d: Dir) -> Pos {
    let (dx, _, dz) = d.step();
    p.offset(dx, 0, dz)
}

fn shelf_mushroom(run: &mut Run<'_, '_>, logs: &[Pos], probability: f32) {
    if run.rng.next_float() >= probability || logs.is_empty() {
        return;
    }
    let first = logs[0];
    let last = logs[logs.len() - 1];
    if first.y == last.y {
        let dirs = if first.x != last.x { [Dir::North, Dir::South] } else { [Dir::East, Dir::West] };
        for log in logs {
            for facing in dirs {
                if !(run.rng.next_float() > 0.25) {
                    let at = beside(*log, facing);
                    if shelf_replaceable(run, at) && !shelf_beside(run, at) && !shelf_beside(run, *log) {
                        place_shelf(run, at, facing);
                    }
                }
            }
        }
    } else {
        let a = Dir::HORIZONTAL[run.rng.next_int_bounded(4) as usize];
        let dirs = [a, a.clockwise()];
        for log in logs {
            let dy = log.y - first.y;
            if !(1..=4).contains(&dy) {
                continue;
            }
            for facing in dirs {
                if !(run.rng.next_float() > 0.25) {
                    let at = beside(*log, facing);
                    if shelf_replaceable(run, at) && !shelf_at(run, at.below()) {
                        place_shelf(run, at, facing);
                        break;
                    }
                }
            }
        }
    }
}
