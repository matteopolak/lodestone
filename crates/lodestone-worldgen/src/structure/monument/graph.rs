//! Room-graph construction for the ocean monument.
//!
//! This stage owns the fixed 5×3×5 room arena, its special roof/wing nodes,
//! the connectivity-preserving opening-closing pass, and the shuffled room
//! definition order consumed by the later room fitter. Block-writing code
//! remains in the parent module so the graph's output order is explicit at the
//! generation boundary.

use std::collections::HashMap;

use lodestone_worldgen_core::rng::RandomSource;

use super::{ALL_DIRS, EAST, NORTH, SOUTH, UP, opposite, room_index, step};

/// A room definition, as an arena node — see the parent module's
/// "arena, not a linked object graph" note.
#[derive(Debug, Clone)]
pub(super) struct RoomDef {
    /// The grid index (`0..46`) or a special id (`1001` left wing, `1002`
    /// right wing, `1003` roof/penthouse stub).
    pub(super) index: i32,
    pub(super) connections: [Option<usize>; 6],
    pub(super) has_opening: [bool; 6],
    pub(super) claimed: bool,
    pub(super) is_source: bool,
    scan_index: i32,
}

impl RoomDef {
    pub(super) fn new(index: i32) -> Self {
        Self {
            index,
            connections: [None; 6],
            has_opening: [false; 6],
            claimed: false,
            is_source: false,
            scan_index: 0,
        }
    }

    /// `isSpecial()`.
    pub(super) fn is_special(&self) -> bool {
        self.index >= 75
    }

    /// The number of open connections this room has.
    pub(super) fn count_openings(&self) -> i32 {
        self.has_opening.iter().filter(|open| **open).count() as i32
    }
}

/// Writes both sides of the edge in one call.
fn set_connection(arena: &mut [RoomDef], a: usize, dir: usize, b: usize) {
    arena[a].connections[dir] = Some(b);
    arena[b].connections[opposite(dir)] = Some(a);
}

/// Refreshes a room's opening flags from its live connections.
fn update_openings(def: &mut RoomDef) {
    for i in 0..6 {
        def.has_opening[i] = def.connections[i].is_some();
    }
}

/// A DFS to a source room, marking visited nodes
/// with `scan_index` so the traversal terminates on the grid's cycles.
fn find_source(arena: &mut [RoomDef], idx: usize, scan_index: i32) -> bool {
    if arena[idx].is_source {
        return true;
    }
    arena[idx].scan_index = scan_index;
    for d in 0..6 {
        let Some(next) = arena[idx].connections[d] else {
            continue;
        };
        if arena[idx].has_opening[d]
            && arena[next].scan_index != scan_index
            && find_source(arena, next, scan_index)
        {
            return true;
        }
    }
    false
}

/// A top-down Fisher-Yates shuffle: `for (i = size; i > 1; i--) swap(i
/// - 1, random.next_int_bounded(i))`.
fn shuffle<R: RandomSource>(list: &mut [usize], random: &mut R) {
    let mut i = list.len();
    while i > 1 {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap_to);
        i -= 1;
    }
}

/// The monument building's own room-graph generator, minus the final three
/// room-definition appends (the caller appends roof/wings after selecting
/// rooms, matching a faithful implementation's own statement order in the constructor).
///
/// Returns `(arena, room_defs, source_idx, core_idx, roof_idx, left_wing_idx,
/// right_wing_idx)`.
#[allow(clippy::type_complexity)]
pub(super) fn generate_room_graph<R: RandomSource>(
    random: &mut R,
) -> (Vec<RoomDef>, Vec<usize>, usize, usize, usize, usize, usize) {
    let mut arena: Vec<RoomDef> = Vec::new();
    let mut grid: HashMap<i32, usize> = HashMap::new();
    let push = |arena: &mut Vec<RoomDef>, grid: &mut HashMap<i32, usize>, x: i32, y: i32, z: i32| {
        let pos = room_index(x, y, z);
        let idx = arena.len();
        arena.push(RoomDef::new(pos));
        grid.insert(pos, idx);
    };
    for x in 0..5 {
        for z in 0..4 {
            push(&mut arena, &mut grid, x, 0, z);
        }
    }
    for x in 0..5 {
        for z in 0..4 {
            push(&mut arena, &mut grid, x, 1, z);
        }
    }
    for x in 1..4 {
        for z in 0..2 {
            push(&mut arena, &mut grid, x, 2, z);
        }
    }
    let source_idx = grid[&room_index(2, 0, 0)];

    for x in 0..5 {
        for z in 0..5 {
            for y in 0..3 {
                let Some(&cell_idx) = grid.get(&room_index(x, y, z)) else {
                    continue;
                };
                for &d in &ALL_DIRS {
                    let (dx, dy, dz) = step(d);
                    let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                    if !(0..5).contains(&nx) || !(0..5).contains(&nz) || !(0..3).contains(&ny) {
                        continue;
                    }
                    let Some(&neigh_idx) = grid.get(&room_index(nx, ny, nz)) else {
                        continue;
                    };
                    if nz == z {
                        set_connection(&mut arena, cell_idx, d, neigh_idx);
                    } else {
                        set_connection(&mut arena, cell_idx, opposite(d), neigh_idx);
                    }
                }
            }
        }
    }

    let roof_idx = arena.len();
    arena.push(RoomDef::new(1003));
    let left_wing_idx = arena.len();
    arena.push(RoomDef::new(1001));
    let right_wing_idx = arena.len();
    arena.push(RoomDef::new(1002));

    set_connection(&mut arena, grid[&room_index(2, 2, 0)], UP, roof_idx);
    set_connection(&mut arena, grid[&room_index(0, 1, 0)], SOUTH, left_wing_idx);
    set_connection(&mut arena, grid[&room_index(4, 1, 0)], SOUTH, right_wing_idx);
    arena[roof_idx].claimed = true;
    arena[left_wing_idx].claimed = true;
    arena[right_wing_idx].claimed = true;
    arena[source_idx].is_source = true;
    // Set by the monument building's own constructor immediately after room-graph
    // generation returns, not inside the room-graph generator itself; done here
    // since nothing observes the difference (it costs no RNG) and every caller of
    // this function needs it before `select_rooms` runs.
    arena[source_idx].claimed = true;

    let core_idx = grid[&room_index(random.next_int_bounded(4), 0, 2)];
    let core_east = arena[core_idx].connections[EAST].expect("core room always has an EAST neighbour");
    let core_north = arena[core_idx].connections[NORTH].expect("core room always has a NORTH neighbour");
    let core_east_north = arena[core_east].connections[NORTH].expect("core room's EAST neighbour always has a NORTH neighbour");
    let core_up = arena[core_idx].connections[UP].expect("core room always has an UP neighbour");
    let core_east_up = arena[core_east].connections[UP].expect("core room's EAST neighbour always has an UP neighbour");
    let core_north_up = arena[core_north].connections[UP].expect("core room's NORTH neighbour always has an UP neighbour");
    let core_east_north_up = arena[core_east_north].connections[UP].expect("core room's EAST/NORTH neighbour always has an UP neighbour");
    arena[core_idx].claimed = true;
    arena[core_east].claimed = true;
    arena[core_north].claimed = true;
    arena[core_east_north].claimed = true;
    arena[core_up].claimed = true;
    arena[core_east_up].claimed = true;
    arena[core_north_up].claimed = true;
    arena[core_east_north_up].claimed = true;

    let mut room_defs: Vec<usize> = Vec::new();
    for y in 0..3 {
        for z in 0..5 {
            for x in 0..5 {
                if let Some(&idx) = grid.get(&room_index(x, y, z)) {
                    update_openings(&mut arena[idx]);
                    room_defs.push(idx);
                }
            }
        }
    }
    update_openings(&mut arena[roof_idx]);

    shuffle(&mut room_defs, random);
    close_openings(&mut arena, &room_defs, random);

    (arena, room_defs, source_idx, core_idx, roof_idx, left_wing_idx, right_wing_idx)
}

/// The shuffled-order closing loop: up to two openings closed per cell,
/// never disconnecting either side from the source room.
fn close_openings<R: RandomSource>(arena: &mut [RoomDef], room_defs: &[usize], random: &mut R) {
    let mut scan_index = 1;
    for &idx in room_defs {
        let mut close_count = 0;
        let mut attempt_count = 0;
        while close_count < 2 && attempt_count < 5 {
            attempt_count += 1;
            let f = random.next_int_bounded(6) as usize;
            if !arena[idx].has_opening[f] {
                continue;
            }
            let of = opposite(f);
            let neighbor = arena[idx].connections[f].expect("hasOpening implies a connection");
            arena[idx].has_opening[f] = false;
            arena[neighbor].has_opening[of] = false;
            let s1 = scan_index;
            scan_index += 1;
            // The `&&` short-circuits in a faithful implementation: the second
            // find-source check and its own scan-index bump never run when the
            // first call is `false`.
            let closed = find_source(arena, idx, s1) && {
                let s2 = scan_index;
                scan_index += 1;
                find_source(arena, neighbor, s2)
            };
            if closed {
                close_count += 1;
            } else {
                arena[idx].has_opening[f] = true;
                arena[neighbor].has_opening[of] = true;
            }
        }
    }
}
