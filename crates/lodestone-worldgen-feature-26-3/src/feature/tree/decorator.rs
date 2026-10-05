//! Tree decorators: blocks added around a finished tree (vines, hives, ground cover).
//!
//! Decorators read the trunk and foliage position sets in the reference's hash-set iteration
//! order, stably sorted by height ([`super::sorted_by_y`]); the draw order inside each one is the
//! contract with the oracle.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use super::{Run, sorted_by_y};
use crate::blocks::{Dir, State};
use crate::env::{Env, Heightmap};
use crate::json::{Res, array, float, get, int_or, type_of};
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
}

impl Decorator {
    pub fn parse(env: &Env, v: &Value, ctx: &str, unsupported: &mut Vec<String>) -> Res<Self> {
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
            "attached_to_logs" => {
                let mut directions = Vec::new();
                for d in array(v, "directions", ctx)? {
                    let n = d.as_str().unwrap_or_default();
                    directions.push(Dir::from_name(n).ok_or_else(|| format!("{ctx}: unknown direction `{n}`"))?);
                }
                Self::AttachedToLogs {
                    probability: float(v, "probability", ctx)?,
                    provider: StateProvider::parse(env, get(v, "block_provider", ctx)?, ctx)?,
                    directions,
                }
            }
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
        for _ in 0..bees {
            run.rng.next_int_bounded(599);
        }
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
