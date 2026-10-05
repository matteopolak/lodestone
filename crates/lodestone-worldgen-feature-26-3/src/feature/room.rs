//! `monster_room`: a small cobblestone room with a spawner in the middle and up to two chests.
//!
//! The room needs a solid floor and ceiling over its whole footprint and between one and five
//! doorways in its wall at floor level. The chest loot seed and the spawner's mob each consume a
//! draw because the real region attaches block entities to both blocks.

use lodestone_worldgen_core::rng::RandomSource;

use crate::blocks::{Dir, State};
use crate::level::Level;
use crate::pos::{Pos, Rng};

fn named(level: &Level<'_>, name: &str) -> State {
    let b = &level.env.blocks;
    b.default_state(b.block_by_name(name).unwrap_or_else(|| panic!("block {name}")))
}

fn is_block(level: &Level<'_>, p: Pos, name: &str) -> bool {
    let b = &level.env.blocks;
    b.block_by_name(name) == Some(b.block_of(level.get(p.x, p.y, p.z)))
}

fn solid(level: &Level<'_>, p: Pos) -> bool {
    level.env.blocks.solid(level.get(p.x, p.y, p.z))
}

fn empty(level: &Level<'_>, p: Pos) -> bool {
    level.env.blocks.is_air(level.get(p.x, p.y, p.z))
}

/// Writes only over blocks the feature may replace.
fn safe_set(level: &mut Level<'_>, p: Pos, s: State) {
    let env = level.env;
    let cannot = env.tags.get("features_cannot_replace").expect("features_cannot_replace tag");
    if !cannot.contains(env.blocks.block_of(level.get(p.x, p.y, p.z))) {
        level.set(p.x, p.y, p.z, s);
    }
}

/// A chest turned to lie against a wall: the single solid neighbour decides, otherwise the
/// default facing is rotated away from solid blocks in a fixed order.
fn reorient_chest(level: &Level<'_>, p: Pos) -> State {
    let b = &level.env.blocks;
    let chest = named(level, "chest");
    let render = |q: Pos| b.solid_render(level.get(q.x, q.y, q.z));
    let mut found: Option<Dir> = None;
    for d in Dir::HORIZONTAL {
        let n = p.relative(d);
        if is_block(level, n, "chest") {
            return chest;
        }
        if render(n) {
            if found.is_some() {
                found = None;
                break;
            }
            found = Some(d);
        }
    }
    let facing = if let Some(d) = found {
        d.opposite()
    } else {
        let mut lock = Dir::North;
        if render(p.relative(lock)) {
            lock = lock.opposite();
        }
        if render(p.relative(lock)) {
            lock = lock.clockwise();
        }
        if render(p.relative(lock)) {
            lock = lock.opposite();
        }
        lock
    };
    b.with(chest, "facing", facing.name()).expect("facing")
}

pub fn place(level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let xr = rng.next_int_bounded(2) + 2;
    let (min_x, max_x) = (-xr - 1, xr + 1);
    let zr = rng.next_int_bounded(2) + 2;
    let (min_z, max_z) = (-zr - 1, zr + 1);
    let mut holes = 0;
    for dx in min_x..=max_x {
        for dy in -1..=4 {
            for dz in min_z..=max_z {
                let p = origin.offset(dx, dy, dz);
                let is_solid = solid(level, p);
                if (dy == -1 || dy == 4) && !is_solid {
                    return false;
                }
                if (dx == min_x || dx == max_x || dz == min_z || dz == max_z) && dy == 0 && empty(level, p) && empty(level, p.above()) {
                    holes += 1;
                }
            }
        }
    }
    if !(1..=5).contains(&holes) {
        return false;
    }
    let cave_air = named(level, "cave_air");
    let cobble = named(level, "cobblestone");
    let mossy = named(level, "mossy_cobblestone");
    for dx in min_x..=max_x {
        for dy in (-1..=3).rev() {
            for dz in min_z..=max_z {
                let p = origin.offset(dx, dy, dz);
                let wall_state = level.get(p.x, p.y, p.z);
                if dx == min_x || dy == -1 || dz == min_z || dx == max_x || dy == 4 || dz == max_z {
                    if p.y >= level.min_y && !solid(level, p.below()) {
                        level.set(p.x, p.y, p.z, cave_air);
                    } else if level.env.blocks.solid(wall_state) && !is_block(level, p, "chest") {
                        if dy == -1 && rng.next_int_bounded(4) != 0 {
                            safe_set(level, p, mossy);
                        } else {
                            safe_set(level, p, cobble);
                        }
                    }
                } else if !is_block(level, p, "chest") && !is_block(level, p, "spawner") {
                    safe_set(level, p, cave_air);
                }
            }
        }
    }
    for _ in 0..2 {
        for _ in 0..3 {
            let x = origin.x + rng.next_int_bounded(xr * 2 + 1) - xr;
            let z = origin.z + rng.next_int_bounded(zr * 2 + 1) - zr;
            let chest_pos = Pos::new(x, origin.y, z);
            if empty(level, chest_pos) {
                let walls = Dir::HORIZONTAL.iter().filter(|d| solid(level, chest_pos.relative(**d))).count();
                if walls == 1 {
                    let s = reorient_chest(level, chest_pos);
                    safe_set(level, chest_pos, s);
                    rng.next_long();
                    break;
                }
            }
        }
    }
    let spawner = named(level, "spawner");
    safe_set(level, origin, spawner);
    // The mob is chosen only when the region attached a spawner entity to the block.
    if is_block(level, origin, "spawner") {
        rng.next_int_bounded(4);
    }
    true
}
