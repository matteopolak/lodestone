//! **Ocean monument** — `OceanMonumentStructure` + `OceanMonumentPieces`, the
//! fixed 58×23×58 building plus its `RoomDefinition` grid graph.
//!
//! # What it is
//!
//! A port of vanilla's third and last piece-placement engine in this crate,
//! and the one that is neither a jigsaw pool graph nor
//! [`super::stronghold`]'s weighted-table tree: `OceanMonumentStructure`
//! places exactly **one** [`StructurePiece`] — `MonumentBuilding` — and that
//! single Java object internally builds a fixed-size room *grid* (5×3×5
//! cells, `RoomDefinition.index = y*25 + z*5 + x`), threads a doubly-linked
//! connection graph through it, then fits each unclaimed cell against seven
//! ordered [`MonumentRoomFitter`](#fitters)s to decide whether it is a plain
//! room or merges with a neighbour into a double-wide/tall/deep one. This
//! module ports that whole graph plus the fixed core (entry, core, both
//! wings, the roof penthouse) and the shell that ties them together
//! (moat, pillars, entrance, walls, roof).
//!
//! Unlike [`super::stronghold`], every generated room here shares **one**
//! coordinate space: vanilla's `generateWaterBox`/`generateBoxOnFillOnly`
//! read blocks a room's own earlier calls (or the shell's initial flood)
//! already wrote. See [`Canvas`] for how that read-back is reproduced
//! without a real per-chunk world.
//!
//! # How it works
//!
//! ```text
//! generate(cx, cz, seed, ctx):
//!     random = structure_random(seed, cx, cz)          <- context.random()
//!     west, north = cx*16 - 29, cz*16 - 29
//!     direction = Facing::random(random)                <- one nextInt(4)
//!     shell = makeBoundingBox(west, 39, north, direction, 58, 23, 58)
//!
//!     (arena, room_defs, source, core) = generate_room_graph(random)
//!         <- builds the 46-cell grid, connects neighbours, splices in the
//!            roof/left-wing/right-wing stub cells, closes 2 random openings
//!            per shuffled cell (never disconnecting a cell from `source`)
//!     selected = select_rooms(arena, room_defs, random)
//!         <- the 7-fitter cascade, claiming cells as it goes; only
//!            `FitSimpleRoom` draws random (mainDesign, one nextInt(3))
//!
//!     resolve blocks: shell, then entry, core, every selected room, then
//!         left wing, right wing, penthouse — matching vanilla's own
//!         postProcess order (shell's own writes complete before any child
//!         reads them; children never read each other).
//! ```
//!
//! # How to change it
//!
//! * **Every piece shares one `direction`.** Vanilla's `MonumentBuilding`
//!   constructor passes the *same* `Direction` to itself and to every child
//!   piece's constructor — there is no per-room rotation, only one rigid
//!   whole. [`Piece::world_pos`] is therefore identical across every piece
//!   type in this module (the same `getWorldX/Y/Z` switch [`super::stronghold`]
//!   already ported once), the difference is only which `box_` it carries.
//! * **`RoomDefinition` is an arena, not a Java object graph.** Every
//!   `connections[dir]` pointer becomes an `Option<usize>` index into one
//!   `Vec<RoomDef>` built by [`generate_room_graph`]; `set_connection` writes
//!   both directions in one call, matching `RoomDefinition.setConnection`'s
//!   own two-sided write.
//! * **The pre-shuffle order is load-bearing.** `Util.shuffle` swaps against
//!   the list's *current* positions, so the same random draws over a
//!   differently-ordered input produce a different permutation.
//!   [`generate_room_graph`] walks the grid in ascending `index` order
//!   (`y`, then `z`, then `x` — the same nesting `getRoomIndex` encodes) to
//!   match `for (RoomDefinition definition : roomGrid)`'s flat-array walk
//!   before appending roof/left-wing/right-wing *after* the shuffle+close
//!   pass, exactly where vanilla appends them.
//! * **The closing loop's `&&` short-circuits, and it costs a `scanIndex`.**
//!   `definition.findSource(scanIndex++) && definition.connections[f]
//!   .findSource(scanIndex++)` skips the second draw (and its `scanIndex`
//!   bump) entirely when the first call returns `false`. [`close_openings`]
//!   preserves that shape rather than always advancing by two.
//! * **`FitDoubleZRoom.create`'s neighbour reassignment is unreachable.**
//!   Its own `fits` already requires `hasOpening[NORTH] &&
//!   !connections[NORTH].claimed`, so the `else` branch that would swap the
//!   anchor to the `SOUTH` neighbour can never run. Ported as a comment, not
//!   as dead code, for the same reason [`super::stronghold`] transcribes
//!   vanilla's own short-circuit shapes literally.
//!
//! # Deviations
//!
//! * **`postProcess`'s `RandomSource` is not derived from the world seed.**
//!   `ChunkGenerator.applyBiomeDecoration` seeds it from
//!   `RandomSupport.generateUniqueSeed()` — real entropy, regenerated every
//!   time a chunk decorates, with **no** relationship to the seed that picks
//!   `mainDesign` or the room graph. So `SimpleRoom`'s `centerPillar` coin
//!   flip and `SimpleTopRoom`'s sponge scatter have no "vanilla answer" a
//!   fixed seed could reproduce — there is no ground truth to match, only a
//!   requirement that *our* engine stay deterministic. This port threads the
//!   **same** seeded stream construction used (`structure_random`) through
//!   both phases rather than inventing a second unseeded source, so the same
//!   world seed always yields the same monument. Ledgered as
//!   `monument:postprocess_random_unseeded`.
//! * **Entity spawning is absent.** `OceanMonumentPenthouse` and
//!   `OceanMonumentWingRoom`'s design-0 variant each call `spawnElder`,
//!   matching the `coded:worldgen_entities` gap [`super::stronghold`]'s
//!   portal-room spawner and [`super::coded`]'s swamp hut witch/cat are
//!   already ledgered under: this engine has no worldgen-time entity driver
//!   yet.
//! * **`skipAir` is not honoured**, for the same reason
//!   [`super::stronghold`] documents under `stronghold:skip_air_shell`: every
//!   `generateBox` call here passes vanilla's own `skipAir = false`, so no
//!   caller needed the flag in the first place — this is not actually a gap.
//!
//! # Dependencies
//!
//! [`StartContext::sea_level`] for the flood/pillar-base decisions,
//! [`StartContext::min_y`] and [`StartContext::is_replaceable_at`] for
//! [`Canvas::fill_column_down`]'s pillar footings — the one place this piece
//! reads terrain shape, exactly as [`super::coded::Builder::fill_column_down`]
//! does for a mineshaft-adjacent reason.

use std::collections::HashMap;

use lodestone_worldgen_core::rng::RandomSource;

use super::coded::Facing;
use super::template::{BlockState, Mirror, Rotation};
use super::{BoundingBox, CodedBlock, StartContext, StructurePiece};

const BASE_GRAY: &str = "minecraft:prismarine";
const BASE_LIGHT: &str = "minecraft:prismarine_bricks";
const BASE_BLACK: &str = "minecraft:dark_prismarine";
const LAMP_BLOCK: &str = "minecraft:sea_lantern";
const WET_SPONGE: &str = "minecraft:wet_sponge";
const GOLD_BLOCK: &str = "minecraft:gold_block";
const WATER: &str = "minecraft:water";
const AIR: &str = "minecraft:air";

/// `Direction.get3DDataValue()` ordinals: `DOWN(0) UP(1) NORTH(2) SOUTH(3)
/// WEST(4) EAST(5)`, read from `Direction.java`'s own constructor order —
/// not derivable from [`Facing`], which only carries the four horizontals.
const DOWN: usize = 0;
const UP: usize = 1;
const NORTH: usize = 2;
const SOUTH: usize = 3;
const WEST: usize = 4;
const EAST: usize = 5;
const ALL_DIRS: [usize; 6] = [DOWN, UP, NORTH, SOUTH, WEST, EAST];

/// `Direction.getOpposite()` — vanilla's constructor pairs each direction
/// with its opposite one index apart (`DOWN`/`UP`, `NORTH`/`SOUTH`,
/// `WEST`/`EAST`), so opposite is `d ^ 1`.
const fn opposite(d: usize) -> usize {
    d ^ 1
}

/// `Direction`'s `(stepX, stepY, stepZ)` `Vec3i`.
const fn step(d: usize) -> (i32, i32, i32) {
    match d {
        DOWN => (0, -1, 0),
        UP => (0, 1, 0),
        NORTH => (0, 0, -1),
        SOUTH => (0, 0, 1),
        WEST => (-1, 0, 0),
        EAST => (1, 0, 0),
        _ => unreachable!(),
    }
}

/// `getRoomIndex(x, y, z)`.
const fn room_index(x: i32, y: i32, z: i32) -> i32 {
    y * 25 + z * 5 + x
}

/// `OceanMonumentPieces.RoomDefinition`, as an arena node — see the module
/// doc's "arena, not a Java object graph" note.
#[derive(Debug, Clone)]
struct RoomDef {
    /// The grid index (`0..46`) or a special id (`1001` left wing, `1002`
    /// right wing, `1003` roof/penthouse stub).
    index: i32,
    connections: [Option<usize>; 6],
    has_opening: [bool; 6],
    claimed: bool,
    is_source: bool,
    scan_index: i32,
}

impl RoomDef {
    fn new(index: i32) -> Self {
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
    fn is_special(&self) -> bool {
        self.index >= 75
    }

    /// `countOpenings()`.
    fn count_openings(&self) -> i32 {
        self.has_opening.iter().filter(|open| **open).count() as i32
    }
}

/// `RoomDefinition.setConnection` — writes both sides of the edge in one call.
fn set_connection(arena: &mut [RoomDef], a: usize, dir: usize, b: usize) {
    arena[a].connections[dir] = Some(b);
    arena[b].connections[opposite(dir)] = Some(a);
}

/// `RoomDefinition.updateOpenings`.
fn update_openings(def: &mut RoomDef) {
    for i in 0..6 {
        def.has_opening[i] = def.connections[i].is_some();
    }
}

/// `RoomDefinition.findSource` — a DFS to `isSource`, marking visited nodes
/// with `scanIndex` so the traversal terminates on the grid's cycles.
fn find_source(arena: &mut [RoomDef], idx: usize, scan_index: i32) -> bool {
    if arena[idx].is_source {
        return true;
    }
    arena[idx].scan_index = scan_index;
    for d in 0..6 {
        let Some(next) = arena[idx].connections[d] else {
            continue;
        };
        if arena[idx].has_opening[d] && arena[next].scan_index != scan_index && find_source(arena, next, scan_index) {
            return true;
        }
    }
    false
}

/// `Util.shuffle` — top-down Fisher-Yates, `for (i = size; i > 1; i--) swap(i
/// - 1, random.nextInt(i))`.
fn shuffle<R: RandomSource>(list: &mut [usize], random: &mut R) {
    let mut i = list.len();
    while i > 1 {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap_to);
        i -= 1;
    }
}

/// `MonumentBuilding.generateRoomGraph`, minus the final three
/// `roomDefs.add(...)` calls (the caller appends roof/wings after selecting
/// rooms, matching vanilla's own statement order in the constructor).
///
/// Returns `(arena, room_defs, source_idx, core_idx, roof_idx, left_wing_idx,
/// right_wing_idx)`.
#[allow(clippy::type_complexity)]
fn generate_room_graph<R: RandomSource>(
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
    // `this.sourceRoom.claimed = true;` — set by `MonumentBuilding`'s own
    // constructor immediately after `generateRoomGraph` returns, not inside
    // the Java method itself; done here instead since nothing observes the
    // difference (it costs no RNG) and every caller of this function needs
    // it before `select_rooms` runs. Its absence let the entry room's own
    // cell be re-selected as an ordinary grid room — caught by
    // `selected_rooms_partition_the_grid_exactly` and
    // `no_two_interior_pieces_overlap`.
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
            // The `&&` short-circuits in vanilla: the second `findSource` (and
            // its `scanIndex` draw) never runs when the first is `false`.
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

/// The seven fitted child-room kinds `select_rooms` can produce, plus
/// [`RoomKind::Entry`]/[`RoomKind::Core`] for the two always-present rooms.
#[derive(Debug, Clone, Copy)]
enum RoomKind {
    Entry,
    Core,
    DoubleX,
    DoubleXy,
    DoubleY,
    DoubleYz,
    DoubleZ,
    SimpleTop,
    Simple { main_design: i32 },
}

impl RoomKind {
    /// The `(roomWidth, roomHeight, roomDepth)` triple each subclass's
    /// constructor hardcodes.
    fn dims(self) -> (i32, i32, i32) {
        match self {
            Self::Entry | Self::SimpleTop | Self::Simple { .. } => (1, 1, 1),
            Self::Core => (2, 2, 2),
            Self::DoubleX => (2, 1, 1),
            Self::DoubleXy => (2, 2, 1),
            Self::DoubleY => (1, 2, 1),
            Self::DoubleYz => (1, 2, 2),
            Self::DoubleZ => (1, 1, 2),
        }
    }

    fn piece_id(self) -> &'static str {
        match self {
            Self::Entry => "minecraft:omentry",
            Self::Core => "minecraft:omcr",
            Self::DoubleX => "minecraft:omdxr",
            Self::DoubleXy => "minecraft:omdxyr",
            Self::DoubleY => "minecraft:omdyr",
            Self::DoubleYz => "minecraft:omdyzr",
            Self::DoubleZ => "minecraft:omdzr",
            Self::SimpleTop => "minecraft:omsimplet",
            Self::Simple { .. } => "minecraft:omsimple",
        }
    }
}

struct RoomPiece {
    kind: RoomKind,
    /// The arena index `create()` claimed for this piece — `west`/`south`/
    /// the lower cell, matching each `postProcess`'s own naming.
    anchor: usize,
}

/// `FitDoubleXYRoom`.
fn try_double_xy(arena: &mut [RoomDef], idx: usize) -> bool {
    if !arena[idx].has_opening[EAST] {
        return false;
    }
    let east = arena[idx].connections[EAST].expect("hasOpening implies a connection");
    if arena[east].claimed {
        return false;
    }
    if !arena[idx].has_opening[UP] {
        return false;
    }
    let up = arena[idx].connections[UP].expect("hasOpening implies a connection");
    if arena[up].claimed {
        return false;
    }
    if !arena[east].has_opening[UP] {
        return false;
    }
    let east_up = arena[east].connections[UP].expect("hasOpening implies a connection");
    if arena[east_up].claimed {
        return false;
    }
    arena[idx].claimed = true;
    arena[east].claimed = true;
    arena[up].claimed = true;
    arena[east_up].claimed = true;
    true
}

/// `FitDoubleYZRoom`.
fn try_double_yz(arena: &mut [RoomDef], idx: usize) -> bool {
    if !arena[idx].has_opening[NORTH] {
        return false;
    }
    let north = arena[idx].connections[NORTH].expect("hasOpening implies a connection");
    if arena[north].claimed {
        return false;
    }
    if !arena[idx].has_opening[UP] {
        return false;
    }
    let up = arena[idx].connections[UP].expect("hasOpening implies a connection");
    if arena[up].claimed {
        return false;
    }
    if !arena[north].has_opening[UP] {
        return false;
    }
    let north_up = arena[north].connections[UP].expect("hasOpening implies a connection");
    if arena[north_up].claimed {
        return false;
    }
    arena[idx].claimed = true;
    arena[north].claimed = true;
    arena[up].claimed = true;
    arena[north_up].claimed = true;
    true
}

/// `FitDoubleZRoom`. Its own `create()` has an `else` branch that would
/// retarget the anchor to the `SOUTH` neighbour — unreachable given `fits()`'s
/// own precondition, see the module doc.
fn try_double_z(arena: &mut [RoomDef], idx: usize) -> bool {
    if !arena[idx].has_opening[NORTH] {
        return false;
    }
    let north = arena[idx].connections[NORTH].expect("hasOpening implies a connection");
    if arena[north].claimed {
        return false;
    }
    arena[idx].claimed = true;
    arena[north].claimed = true;
    true
}

/// `FitDoubleXRoom`.
fn try_double_x(arena: &mut [RoomDef], idx: usize) -> bool {
    if !arena[idx].has_opening[EAST] {
        return false;
    }
    let east = arena[idx].connections[EAST].expect("hasOpening implies a connection");
    if arena[east].claimed {
        return false;
    }
    arena[idx].claimed = true;
    arena[east].claimed = true;
    true
}

/// `FitDoubleYRoom`.
fn try_double_y(arena: &mut [RoomDef], idx: usize) -> bool {
    if !arena[idx].has_opening[UP] {
        return false;
    }
    let up = arena[idx].connections[UP].expect("hasOpening implies a connection");
    if arena[up].claimed {
        return false;
    }
    arena[idx].claimed = true;
    arena[up].claimed = true;
    true
}

/// `FitSimpleTopRoom.fits` — no `create()` claim beyond `definition.claimed`,
/// applied by the caller alongside every other fitter.
fn fits_simple_top(arena: &[RoomDef], idx: usize) -> bool {
    !arena[idx].has_opening[WEST]
        && !arena[idx].has_opening[EAST]
        && !arena[idx].has_opening[NORTH]
        && !arena[idx].has_opening[SOUTH]
        && !arena[idx].has_opening[UP]
}

/// The `for (definition : roomDefinitions) { if (!claimed && !special) { for
/// (fitter : fitters) ... } }` loop, fitter list order:
/// `[DoubleXY, DoubleYZ, DoubleZ, DoubleX, DoubleY, SimpleTop, Simple]`.
fn select_rooms<R: RandomSource>(arena: &mut [RoomDef], room_defs: &[usize], random: &mut R) -> Vec<RoomPiece> {
    let mut out = Vec::new();
    for &idx in room_defs {
        if arena[idx].claimed || arena[idx].is_special() {
            continue;
        }
        if try_double_xy(arena, idx) {
            out.push(RoomPiece { kind: RoomKind::DoubleXy, anchor: idx });
            continue;
        }
        if try_double_yz(arena, idx) {
            out.push(RoomPiece { kind: RoomKind::DoubleYz, anchor: idx });
            continue;
        }
        if try_double_z(arena, idx) {
            out.push(RoomPiece { kind: RoomKind::DoubleZ, anchor: idx });
            continue;
        }
        if try_double_x(arena, idx) {
            out.push(RoomPiece { kind: RoomKind::DoubleX, anchor: idx });
            continue;
        }
        if try_double_y(arena, idx) {
            out.push(RoomPiece { kind: RoomKind::DoubleY, anchor: idx });
            continue;
        }
        if fits_simple_top(arena, idx) {
            arena[idx].claimed = true;
            out.push(RoomPiece { kind: RoomKind::SimpleTop, anchor: idx });
            continue;
        }
        // `FitSimpleRoom.fits` always returns true — the cascade's fallback.
        arena[idx].claimed = true;
        let main_design = random.next_int_bounded(3);
        out.push(RoomPiece { kind: RoomKind::Simple { main_design }, anchor: idx });
    }
    out
}

/// `(mirror, rotation)` — `StructurePiece.setOrientation`'s table, the same
/// one [`super::stronghold::Node::transform`] and [`super::coded::Facing`]'s
/// own private copy carry, ported a third time because the helper is not
/// `pub`.
fn transform(orientation: Facing) -> (Mirror, Rotation) {
    match orientation {
        Facing::South => (Mirror::LeftRight, Rotation::None),
        Facing::West => (Mirror::LeftRight, Rotation::Cw90),
        Facing::East => (Mirror::None, Rotation::Cw90),
        Facing::North => (Mirror::None, Rotation::None),
    }
}

/// `StructurePiece.makeBoundingBox(x, y, z, direction, width, height,
/// depth)` — anchored at an explicit corner, no offset.
fn make_bounding_box_simple(x: i32, y: i32, z: i32, direction: Facing, width: i32, height: i32, depth: i32) -> BoundingBox {
    if direction.is_z_axis() {
        BoundingBox {
            min: [x, y, z],
            max: [x + width - 1, y + height - 1, z + depth - 1],
        }
    } else {
        BoundingBox {
            min: [x, y, z],
            max: [x + depth - 1, y + height - 1, z + width - 1],
        }
    }
}

/// `OceanMonumentPiece.makeBoundingBox(orientation, roomDefinition,
/// roomWidth, roomHeight, roomDepth)` — the grid-index-to-box formula,
/// anchored near the world origin; [`generate`] translates the result by
/// `shell.world_pos(9, 0, 22)` afterwards, matching vanilla's own
/// `child.getBoundingBox().move(offset)`.
fn room_bounding_box(index: i32, direction: Facing, room_width: i32, room_height: i32, room_depth: i32) -> BoundingBox {
    let room_x = index % 5;
    let room_z = (index / 5) % 5;
    let room_y = index / 25;
    let base = make_bounding_box_simple(0, 0, 0, direction, room_width * 8, room_height * 4, room_depth * 8);
    let (dx, dy, dz) = match direction {
        Facing::North => (room_x * 8, room_y * 4, -(room_z + room_depth) * 8 + 1),
        Facing::South => (room_x * 8, room_y * 4, room_z * 8),
        Facing::West => (-(room_z + room_depth) * 8 + 1, room_y * 4, room_x * 8),
        Facing::East => (room_z * 8, room_y * 4, room_x * 8),
    };
    translate(base, [dx, dy, dz])
}

fn translate(bb: BoundingBox, offset: [i32; 3]) -> BoundingBox {
    BoundingBox {
        min: [bb.min[0] + offset[0], bb.min[1] + offset[1], bb.min[2] + offset[2]],
        max: [bb.max[0] + offset[0], bb.max[1] + offset[1], bb.max[2] + offset[2]],
    }
}

/// One piece's fixed geometry — `this.boundingBox` plus `this.orientation`,
/// which every `OceanMonumentPiece` in one monument shares (see the module
/// doc's "every piece shares one `direction`" note).
#[derive(Debug, Clone, Copy)]
struct Piece {
    box_: BoundingBox,
    orientation: Facing,
}

impl Piece {
    /// `getWorldX(x, z)`.
    fn world_x(&self, x: i32, z: i32) -> i32 {
        match self.orientation {
            Facing::North | Facing::South => self.box_.min[0] + x,
            Facing::West => self.box_.max[0] - z,
            Facing::East => self.box_.min[0] + z,
        }
    }

    /// `getWorldY(y)`.
    fn world_y(&self, y: i32) -> i32 {
        y + self.box_.min[1]
    }

    /// `getWorldZ(x, z)`.
    fn world_z(&self, x: i32, z: i32) -> i32 {
        match self.orientation {
            Facing::North => self.box_.max[2] - z,
            Facing::South => self.box_.min[2] + z,
            Facing::West | Facing::East => self.box_.min[2] + x,
        }
    }

    /// `getWorldPos(x, y, z)`.
    fn world_pos(&self, x: i32, y: i32, z: i32) -> [i32; 3] {
        [self.world_x(x, z), self.world_y(y), self.world_z(x, z)]
    }
}

/// One piece's block canvas: a local write log keyed by **local** `(x, y,
/// z)`, standing in for vanilla's `WorldGenLevel.getBlockState` reads.
///
/// # Why a local map is enough — no shared cross-piece canvas
///
/// `generateWaterBox`/`generateBoxOnFillOnly` are the only reads in this
/// whole piece family, and every call site reads a cell **this same piece**
/// (or, for the shell, the piece's own initial flood) wrote moments earlier —
/// never a cell a *different* piece wrote. The shell's initial
/// `generateWaterBox(0, 0, waterHeight, 58, 58)` floods the entire building
/// footprint before any child's `postProcess` runs in vanilla, and that
/// flood is a pure function of `(sea_level, world_y)` — no real terrain
/// dependency — so [`Self::flood_default`] reproduces it exactly for any
/// cell a room's own canvas has not yet written, without needing the shell's
/// writes in scope at all.
struct Canvas<'a> {
    piece: &'a Piece,
    ctx: &'a dyn StartContext,
    local: HashMap<(i32, i32, i32), BlockState>,
    blocks: Vec<CodedBlock>,
}

impl<'a> Canvas<'a> {
    fn new(piece: &'a Piece, ctx: &'a dyn StartContext) -> Self {
        Self {
            piece,
            ctx,
            local: HashMap::new(),
            blocks: Vec::new(),
        }
    }

    /// The implicit flood state for a cell this canvas has not written yet —
    /// `getWorldY(y) >= level.getSeaLevel()` ? air : water, matching the
    /// branch inside vanilla's own `generateWaterBox`.
    fn flood_default(&self, y: i32) -> BlockState {
        if self.piece.world_y(y) >= self.ctx.sea_level() {
            BlockState::of(AIR)
        } else {
            BlockState::of(WATER)
        }
    }

    /// `getBlock(level, x, y, z, chunkBB)`.
    fn get(&self, x: i32, y: i32, z: i32) -> BlockState {
        self.local.get(&(x, y, z)).cloned().unwrap_or_else(|| self.flood_default(y))
    }

    /// `placeBlock(level, state, x, y, z, chunkBB)` — mirror, rotate, record
    /// into both the local map (so a later read sees it) and the ordered
    /// block list (so the placement pipeline applies it, last write wins).
    fn place(&mut self, state: &BlockState, x: i32, y: i32, z: i32) {
        let (mirror, rotation) = transform(self.piece.orientation);
        let transformed = state.mirror(mirror).rotate(rotation);
        self.local.insert((x, y, z), transformed.clone());
        let pos = self.piece.world_pos(x, y, z);
        self.blocks.push(CodedBlock {
            pos,
            state: transformed.canonical(),
        });
    }

    /// `generateBox(..., edge, fill, skipAir = false)`.
    #[allow(clippy::too_many_arguments)]
    fn generate_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, edge: &BlockState, fill: &BlockState) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let interior = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    self.place(if interior { fill } else { edge }, x, y, z);
                }
            }
        }
    }

    /// `generateWaterBox` — air above sea level, water at or below, except a
    /// cell already `FILL_BLOCK` (water) is left untouched (`FILL_KEEP`'s
    /// ice variants never occur pre-surface, so water is the only member
    /// that can appear here).
    #[allow(clippy::too_many_arguments)]
    fn generate_water_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) {
        let water = BlockState::of(WATER);
        let air = BlockState::of(AIR);
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if self.get(x, y, z) == water {
                        continue;
                    }
                    if self.piece.world_y(y) >= self.ctx.sea_level() {
                        self.place(&air, x, y, z);
                    } else {
                        self.place(&water, x, y, z);
                    }
                }
            }
        }
    }

    /// `generateBoxOnFillOnly` — overwrite only cells still `FILL_BLOCK`
    /// (still water, i.e. never touched by a wall since the initial flood),
    /// used to seal a ceiling/wall where a connection was closed.
    #[allow(clippy::too_many_arguments)]
    fn generate_box_on_fill_only(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, target: &BlockState) {
        let water = BlockState::of(WATER);
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if self.get(x, y, z) == water {
                        self.place(target, x, y, z);
                    }
                }
            }
        }
    }

    /// `generateDefaultFloor(xOff, zOff, downOpening)`.
    fn generate_default_floor(&mut self, x_off: i32, z_off: i32, down_opening: bool) {
        let gray = BlockState::of(BASE_GRAY);
        let light = BlockState::of(BASE_LIGHT);
        if down_opening {
            self.generate_box(x_off, 0, z_off, x_off + 2, 0, z_off + 7, &gray, &gray);
            self.generate_box(x_off + 5, 0, z_off, x_off + 7, 0, z_off + 7, &gray, &gray);
            self.generate_box(x_off + 3, 0, z_off, x_off + 4, 0, z_off + 2, &gray, &gray);
            self.generate_box(x_off + 3, 0, z_off + 5, x_off + 4, 0, z_off + 7, &gray, &gray);
            self.generate_box(x_off + 3, 0, z_off + 2, x_off + 4, 0, z_off + 2, &light, &light);
            self.generate_box(x_off + 3, 0, z_off + 5, x_off + 4, 0, z_off + 5, &light, &light);
            self.generate_box(x_off + 2, 0, z_off + 3, x_off + 2, 0, z_off + 4, &light, &light);
            self.generate_box(x_off + 5, 0, z_off + 3, x_off + 5, 0, z_off + 4, &light, &light);
        } else {
            self.generate_box(x_off, 0, z_off, x_off + 7, 0, z_off + 7, &gray, &gray);
        }
    }

    /// `fillColumnDown` — the one read of real terrain shape, through
    /// [`StartContext::is_replaceable_at`].
    fn fill_column_down(&mut self, state: &BlockState, x: i32, start_y: i32, z: i32) {
        let floor = self.ctx.min_y() + 1;
        let (wx, wz) = (self.piece.world_x(x, z), self.piece.world_z(x, z));
        let mut local_y = start_y;
        let mut world_y = self.piece.world_y(start_y);
        while world_y > floor && self.ctx.is_replaceable_at(wx, world_y, wz) {
            self.place(state, x, local_y, z);
            local_y -= 1;
            world_y -= 1;
        }
    }

    fn finish(self, id: &str, gen_depth: i32) -> StructurePiece {
        StructurePiece {
            id: id.to_string(),
            bounding_box: self.piece.box_,
            orientation: Some(self.piece.orientation.data_2d()),
            gen_depth,
            template: None,
            placement: None,
            extra_placements: Vec::new(),
            blocks: Some(std::sync::Arc::new(self.blocks)),
            loot: Vec::new(),
            // `Beardifier.java:75`'s `else` branch, matching every other
            // coded piece in this crate: `minecraft:ocean_monument`'s
            // `terrain_adaptation` is `none`.
            beard: None,
            refine: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The shell: `MonumentBuilding.postProcess` minus every `chunkIntersects`
// guard (a per-chunk optimisation with nothing to guard here — this module
// resolves the whole structure eagerly and lets `structure_place_stage`
// clip it, exactly as every other coded piece in this crate does).
// ---------------------------------------------------------------------------

/// `MonumentBuilding.generateWing(isFlipped, xoff, ...)`.
fn generate_wing(c: &mut Canvas, is_flipped: bool, xoff: i32) {
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let dot = BlockState::of(BASE_LIGHT);
    c.generate_box(xoff, 0, 0, xoff + 24, 0, 20, &gray, &gray);
    c.generate_water_box(xoff, 1, 0, xoff + 24, 10, 20);
    for i in 0..4 {
        c.generate_box(xoff + i, i + 1, i, xoff + i, i + 1, 20, &light, &light);
        c.generate_box(xoff + i + 7, i + 5, i + 7, xoff + i + 7, i + 5, 20, &light, &light);
        c.generate_box(xoff + 17 - i, i + 5, i + 7, xoff + 17 - i, i + 5, 20, &light, &light);
        c.generate_box(xoff + 24 - i, i + 1, i, xoff + 24 - i, i + 1, 20, &light, &light);
        c.generate_box(xoff + i + 1, i + 1, i, xoff + 23 - i, i + 1, i, &light, &light);
        c.generate_box(xoff + i + 8, i + 5, i + 7, xoff + 16 - i, i + 5, i + 7, &light, &light);
    }
    c.generate_box(xoff + 4, 4, 4, xoff + 6, 4, 20, &gray, &gray);
    c.generate_box(xoff + 7, 4, 4, xoff + 17, 4, 6, &gray, &gray);
    c.generate_box(xoff + 18, 4, 4, xoff + 20, 4, 20, &gray, &gray);
    c.generate_box(xoff + 11, 8, 11, xoff + 13, 8, 20, &gray, &gray);
    c.place(&dot, xoff + 12, 9, 12);
    c.place(&dot, xoff + 12, 9, 15);
    c.place(&dot, xoff + 12, 9, 18);
    let left_pos = xoff + if is_flipped { 19 } else { 5 };
    let right_pos = xoff + if is_flipped { 5 } else { 19 };
    let mut z = 20;
    while z >= 5 {
        c.place(&dot, left_pos, 5, z);
        z -= 3;
    }
    let mut z = 19;
    while z >= 7 {
        c.place(&dot, right_pos, 5, z);
        z -= 3;
    }
    for i in 0..4 {
        let pos = if is_flipped { xoff + 24 - (17 - i * 3) } else { xoff + 17 - i * 3 };
        c.place(&dot, pos, 5, 5);
    }
    c.place(&dot, right_pos, 5, 5);
    c.generate_box(xoff + 11, 1, 12, xoff + 13, 7, 12, &gray, &gray);
    c.generate_box(xoff + 12, 1, 11, xoff + 12, 7, 13, &gray, &gray);
}

/// `MonumentBuilding.generateEntranceArchs`.
fn generate_entrance_archs(c: &mut Canvas) {
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let lamp = BlockState::of(LAMP_BLOCK);
    c.generate_water_box(25, 0, 0, 32, 8, 20);
    for i in 0..4 {
        let z = 5 + i * 4;
        c.generate_box(24, 2, z, 24, 4, z, &light, &light);
        c.generate_box(22, 4, z, 23, 4, z, &light, &light);
        c.place(&light, 25, 5, z);
        c.place(&light, 26, 6, z);
        c.place(&lamp, 26, 5, z);
        c.generate_box(33, 2, z, 33, 4, z, &light, &light);
        c.generate_box(34, 4, z, 35, 4, z, &light, &light);
        c.place(&light, 32, 5, z);
        c.place(&light, 31, 6, z);
        c.place(&lamp, 31, 5, z);
        c.generate_box(27, 6, z, 30, 6, z, &gray, &gray);
    }
}

/// `MonumentBuilding.generateEntranceWall`.
fn generate_entrance_wall(c: &mut Canvas) {
    let gray = BlockState::of(BASE_GRAY);
    let light = BlockState::of(BASE_LIGHT);
    let black = BlockState::of(BASE_BLACK);
    c.generate_box(15, 0, 21, 42, 0, 21, &gray, &gray);
    c.generate_water_box(26, 1, 21, 31, 3, 21);
    c.generate_box(21, 12, 21, 36, 12, 21, &gray, &gray);
    c.generate_box(17, 11, 21, 40, 11, 21, &gray, &gray);
    c.generate_box(16, 10, 21, 41, 10, 21, &gray, &gray);
    c.generate_box(15, 7, 21, 42, 9, 21, &gray, &gray);
    c.generate_box(16, 6, 21, 41, 6, 21, &gray, &gray);
    c.generate_box(17, 5, 21, 40, 5, 21, &gray, &gray);
    c.generate_box(21, 4, 21, 36, 4, 21, &gray, &gray);
    c.generate_box(22, 3, 21, 26, 3, 21, &gray, &gray);
    c.generate_box(31, 3, 21, 35, 3, 21, &gray, &gray);
    c.generate_box(23, 2, 21, 25, 2, 21, &gray, &gray);
    c.generate_box(32, 2, 21, 34, 2, 21, &gray, &gray);
    c.generate_box(28, 4, 20, 29, 4, 21, &light, &light);
    c.place(&light, 27, 3, 21);
    c.place(&light, 30, 3, 21);
    c.place(&light, 26, 2, 21);
    c.place(&light, 31, 2, 21);
    c.place(&light, 25, 1, 21);
    c.place(&light, 32, 1, 21);
    for i in 0..7 {
        c.place(&black, 28 - i, 6 + i, 21);
        c.place(&black, 29 + i, 6 + i, 21);
    }
    for i in 0..4 {
        c.place(&black, 28 - i, 9 + i, 21);
        c.place(&black, 29 + i, 9 + i, 21);
    }
    c.place(&black, 28, 12, 21);
    c.place(&black, 29, 12, 21);
    for i in 0..3 {
        c.place(&black, 22 - i * 2, 8, 21);
        c.place(&black, 22 - i * 2, 9, 21);
        c.place(&black, 35 + i * 2, 8, 21);
        c.place(&black, 35 + i * 2, 9, 21);
    }
    c.generate_water_box(15, 13, 21, 42, 15, 21);
    c.generate_water_box(15, 1, 21, 15, 6, 21);
    c.generate_water_box(16, 1, 21, 16, 5, 21);
    c.generate_water_box(17, 1, 21, 20, 4, 21);
    c.generate_water_box(21, 1, 21, 21, 3, 21);
    c.generate_water_box(22, 1, 21, 22, 2, 21);
    c.generate_water_box(23, 1, 21, 24, 1, 21);
    c.generate_water_box(42, 1, 21, 42, 6, 21);
    c.generate_water_box(41, 1, 21, 41, 5, 21);
    c.generate_water_box(37, 1, 21, 40, 4, 21);
    c.generate_water_box(36, 1, 21, 36, 3, 21);
    c.generate_water_box(33, 1, 21, 34, 1, 21);
    c.generate_water_box(35, 1, 21, 35, 2, 21);
}

/// `MonumentBuilding.generateRoofPiece`.
fn generate_roof_piece(c: &mut Canvas) {
    let gray = BlockState::of(BASE_GRAY);
    let light = BlockState::of(BASE_LIGHT);
    let lamp = BlockState::of(LAMP_BLOCK);
    c.generate_box(21, 0, 22, 36, 0, 36, &gray, &gray);
    c.generate_water_box(21, 1, 22, 36, 23, 36);
    for i in 0..4 {
        c.generate_box(21 + i, 13 + i, 21 + i, 36 - i, 13 + i, 21 + i, &light, &light);
        c.generate_box(21 + i, 13 + i, 36 - i, 36 - i, 13 + i, 36 - i, &light, &light);
        c.generate_box(21 + i, 13 + i, 22 + i, 21 + i, 13 + i, 35 - i, &light, &light);
        c.generate_box(36 - i, 13 + i, 22 + i, 36 - i, 13 + i, 35 - i, &light, &light);
    }
    c.generate_box(25, 16, 25, 32, 16, 32, &gray, &gray);
    c.generate_box(25, 17, 25, 25, 19, 25, &light, &light);
    c.generate_box(32, 17, 25, 32, 19, 25, &light, &light);
    c.generate_box(25, 17, 32, 25, 19, 32, &light, &light);
    c.generate_box(32, 17, 32, 32, 19, 32, &light, &light);
    c.place(&light, 26, 20, 26);
    c.place(&light, 27, 21, 27);
    c.place(&lamp, 27, 20, 27);
    c.place(&light, 26, 20, 31);
    c.place(&light, 27, 21, 30);
    c.place(&lamp, 27, 20, 30);
    c.place(&light, 31, 20, 31);
    c.place(&light, 30, 21, 30);
    c.place(&lamp, 30, 20, 30);
    c.place(&light, 31, 20, 26);
    c.place(&light, 30, 21, 27);
    c.place(&lamp, 30, 20, 27);
    c.generate_box(28, 21, 27, 29, 21, 27, &gray, &gray);
    c.generate_box(27, 21, 28, 27, 21, 29, &gray, &gray);
    c.generate_box(28, 21, 30, 29, 21, 30, &gray, &gray);
    c.generate_box(30, 21, 28, 30, 21, 29, &gray, &gray);
}

/// `MonumentBuilding.generateLowerWall`.
fn generate_lower_wall(c: &mut Canvas) {
    let gray = BlockState::of(BASE_GRAY);
    let light = BlockState::of(BASE_LIGHT);
    let dot = BlockState::of(BASE_LIGHT);
    c.generate_box(0, 0, 21, 6, 0, 57, &gray, &gray);
    c.generate_water_box(0, 1, 21, 6, 7, 57);
    c.generate_box(4, 4, 21, 6, 4, 53, &gray, &gray);
    for i in 0..4 {
        c.generate_box(i, i + 1, 21, i, i + 1, 57 - i, &light, &light);
    }
    let mut z = 23;
    while z < 53 {
        c.place(&dot, 5, 5, z);
        z += 3;
    }
    c.place(&dot, 5, 5, 52);
    for i in 0..4 {
        c.generate_box(i, i + 1, 21, i, i + 1, 57 - i, &light, &light);
    }
    c.generate_box(4, 1, 52, 6, 3, 52, &gray, &gray);
    c.generate_box(5, 1, 51, 5, 3, 53, &gray, &gray);

    c.generate_box(51, 0, 21, 57, 0, 57, &gray, &gray);
    c.generate_water_box(51, 1, 21, 57, 7, 57);
    c.generate_box(51, 4, 21, 53, 4, 53, &gray, &gray);
    for i in 0..4 {
        c.generate_box(57 - i, i + 1, 21, 57 - i, i + 1, 57 - i, &light, &light);
    }
    let mut z = 23;
    while z < 53 {
        c.place(&dot, 52, 5, z);
        z += 3;
    }
    c.place(&dot, 52, 5, 52);
    c.generate_box(51, 1, 52, 53, 3, 52, &gray, &gray);
    c.generate_box(52, 1, 51, 52, 3, 53, &gray, &gray);

    c.generate_box(7, 0, 51, 50, 0, 57, &gray, &gray);
    c.generate_water_box(7, 1, 51, 50, 10, 57);
    for i in 0..4 {
        c.generate_box(i + 1, i + 1, 57 - i, 56 - i, i + 1, 57 - i, &light, &light);
    }
}

/// `MonumentBuilding.generateMiddleWall`.
fn generate_middle_wall(c: &mut Canvas) {
    let gray = BlockState::of(BASE_GRAY);
    let light = BlockState::of(BASE_LIGHT);
    let dot = BlockState::of(BASE_LIGHT);
    c.generate_box(7, 0, 21, 13, 0, 50, &gray, &gray);
    c.generate_water_box(7, 1, 21, 13, 10, 50);
    c.generate_box(11, 8, 21, 13, 8, 53, &gray, &gray);
    for i in 0..4 {
        c.generate_box(i + 7, i + 5, 21, i + 7, i + 5, 54, &light, &light);
    }
    let mut z = 21;
    while z <= 45 {
        c.place(&dot, 12, 9, z);
        z += 3;
    }

    c.generate_box(44, 0, 21, 50, 0, 50, &gray, &gray);
    c.generate_water_box(44, 1, 21, 50, 10, 50);
    c.generate_box(44, 8, 21, 46, 8, 53, &gray, &gray);
    for i in 0..4 {
        c.generate_box(50 - i, i + 5, 21, 50 - i, i + 5, 54, &light, &light);
    }
    let mut z = 21;
    while z <= 45 {
        c.place(&dot, 45, 9, z);
        z += 3;
    }

    c.generate_box(14, 0, 44, 43, 0, 50, &gray, &gray);
    c.generate_water_box(14, 1, 44, 43, 10, 50);
    let mut x = 12;
    while x <= 45 {
        c.place(&dot, x, 9, 45);
        c.place(&dot, x, 9, 52);
        if x == 12 || x == 18 || x == 24 || x == 33 || x == 39 || x == 45 {
            c.place(&dot, x, 9, 47);
            c.place(&dot, x, 9, 50);
            c.place(&dot, x, 10, 45);
            c.place(&dot, x, 10, 46);
            c.place(&dot, x, 10, 51);
            c.place(&dot, x, 10, 52);
            c.place(&dot, x, 11, 47);
            c.place(&dot, x, 11, 50);
            c.place(&dot, x, 12, 48);
            c.place(&dot, x, 12, 49);
        }
        x += 3;
    }
    for i in 0..3 {
        c.generate_box(8 + i, 5 + i, 54, 49 - i, 5 + i, 54, &gray, &gray);
    }
    c.generate_box(11, 8, 54, 46, 8, 54, &light, &light);
    c.generate_box(14, 8, 44, 43, 8, 53, &gray, &gray);
}

/// `MonumentBuilding.generateUpperWall`.
fn generate_upper_wall(c: &mut Canvas) {
    let gray = BlockState::of(BASE_GRAY);
    let light = BlockState::of(BASE_LIGHT);
    let dot = BlockState::of(BASE_LIGHT);
    c.generate_box(14, 0, 21, 20, 0, 43, &gray, &gray);
    c.generate_water_box(14, 1, 22, 20, 14, 43);
    c.generate_box(18, 12, 22, 20, 12, 39, &gray, &gray);
    c.generate_box(18, 12, 21, 20, 12, 21, &light, &light);
    for i in 0..4 {
        c.generate_box(i + 14, i + 9, 21, i + 14, i + 9, 43 - i, &light, &light);
    }
    let mut z = 23;
    while z <= 39 {
        c.place(&dot, 19, 13, z);
        z += 3;
    }

    c.generate_box(37, 0, 21, 43, 0, 43, &gray, &gray);
    c.generate_water_box(37, 1, 22, 43, 14, 43);
    c.generate_box(37, 12, 22, 39, 12, 39, &gray, &gray);
    c.generate_box(37, 12, 21, 39, 12, 21, &light, &light);
    for i in 0..4 {
        c.generate_box(43 - i, i + 9, 21, 43 - i, i + 9, 43 - i, &light, &light);
    }
    let mut z = 23;
    while z <= 39 {
        c.place(&dot, 38, 13, z);
        z += 3;
    }

    c.generate_box(21, 0, 37, 36, 0, 43, &gray, &gray);
    c.generate_water_box(21, 1, 37, 36, 14, 43);
    c.generate_box(21, 12, 37, 36, 12, 39, &gray, &gray);
    for i in 0..4 {
        c.generate_box(15 + i, i + 9, 43 - i, 42 - i, i + 9, 43 - i, &light, &light);
    }
    let mut x = 21;
    while x <= 36 {
        c.place(&dot, x, 13, 38);
        x += 3;
    }
}

/// `MonumentBuilding.postProcess` — the shell blocks only (the `childPieces`
/// loop at the end is what [`generate`] does by resolving every other piece
/// separately, in the same order).
fn build_shell(piece: &Piece, ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let water_height = ctx.sea_level().max(64) - piece.box_.min[1];
    c.generate_water_box(0, 0, 0, 58, water_height, 58);
    generate_wing(&mut c, false, 0);
    generate_wing(&mut c, true, 33);
    generate_entrance_archs(&mut c);
    generate_entrance_wall(&mut c);
    generate_roof_piece(&mut c);
    generate_lower_wall(&mut c);
    generate_middle_wall(&mut c);
    generate_upper_wall(&mut c);

    for pillar_x in 0..7 {
        let mut pillar_z = 0;
        while pillar_z < 7 {
            if pillar_z == 0 && pillar_x == 3 {
                pillar_z = 6;
            }
            let bx = pillar_x * 9;
            let bz = pillar_z * 9;
            for w in 0..4 {
                for d in 0..4 {
                    c.place(&light, bx + w, 0, bz + d);
                    c.fill_column_down(&light, bx + w, -1, bz + d);
                }
            }
            if pillar_x != 0 && pillar_x != 6 {
                pillar_z += 6;
            } else {
                pillar_z += 1;
            }
        }
    }

    for i in 0..5 {
        c.generate_water_box(-1 - i, i * 2, -1 - i, -1 - i, 23, 58 + i);
        c.generate_water_box(58 + i, i * 2, -1 - i, 58 + i, 23, 58 + i);
        c.generate_water_box(-i, i * 2, -1 - i, 57 + i, 23, -1 - i);
        c.generate_water_box(-i, i * 2, 58 + i, 57 + i, 23, 58 + i);
    }

    c.finish("minecraft:omb", 0)
}

// ---------------------------------------------------------------------------
// Grid rooms. Every function below reads `arena` for the anchor's own
// `connections`/`has_opening`, exactly as vanilla's `postProcess` reads
// `this.roomDefinition` and its immediate neighbours.
// ---------------------------------------------------------------------------

/// `OceanMonumentEntryRoom.postProcess`.
fn build_entry_room(piece: &Piece, anchor: usize, arena: &[RoomDef], ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    c.generate_box(0, 3, 0, 2, 3, 7, &light, &light);
    c.generate_box(5, 3, 0, 7, 3, 7, &light, &light);
    c.generate_box(0, 2, 0, 1, 2, 7, &light, &light);
    c.generate_box(6, 2, 0, 7, 2, 7, &light, &light);
    c.generate_box(0, 1, 0, 0, 1, 7, &light, &light);
    c.generate_box(7, 1, 0, 7, 1, 7, &light, &light);
    c.generate_box(0, 1, 7, 7, 3, 7, &light, &light);
    c.generate_box(1, 1, 0, 2, 3, 0, &light, &light);
    c.generate_box(5, 1, 0, 6, 3, 0, &light, &light);
    let def = &arena[anchor];
    if def.has_opening[NORTH] {
        c.generate_water_box(3, 1, 7, 4, 2, 7);
    }
    if def.has_opening[WEST] {
        c.generate_water_box(0, 1, 3, 1, 2, 4);
    }
    if def.has_opening[EAST] {
        c.generate_water_box(6, 1, 3, 7, 2, 4);
    }
    c.finish(RoomKind::Entry.piece_id(), 1)
}

/// `OceanMonumentCoreRoom.postProcess`.
fn build_core_room(piece: &Piece, ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let gray = BlockState::of(BASE_GRAY);
    let light = BlockState::of(BASE_LIGHT);
    let black = BlockState::of(BASE_BLACK);
    let gold = BlockState::of(GOLD_BLOCK);
    let lamp = BlockState::of(LAMP_BLOCK);
    c.generate_box_on_fill_only(1, 8, 0, 14, 8, 14, &gray);
    let block = light.clone();
    c.generate_box(0, 7, 0, 0, 7, 15, &block, &block);
    c.generate_box(15, 7, 0, 15, 7, 15, &block, &block);
    c.generate_box(1, 7, 0, 15, 7, 0, &block, &block);
    c.generate_box(1, 7, 15, 14, 7, 15, &block, &block);
    for yx in 1..=6 {
        let block = if yx == 2 || yx == 6 { gray.clone() } else { light.clone() };
        let mut x = 0;
        while x <= 15 {
            c.generate_box(x, yx, 0, x, yx, 1, &block, &block);
            c.generate_box(x, yx, 6, x, yx, 9, &block, &block);
            c.generate_box(x, yx, 14, x, yx, 15, &block, &block);
            x += 15;
        }
        c.generate_box(1, yx, 0, 1, yx, 0, &block, &block);
        c.generate_box(6, yx, 0, 9, yx, 0, &block, &block);
        c.generate_box(14, yx, 0, 14, yx, 0, &block, &block);
        c.generate_box(1, yx, 15, 14, yx, 15, &block, &block);
    }
    c.generate_box(6, 3, 6, 9, 6, 9, &black, &black);
    c.generate_box(7, 4, 7, 8, 5, 8, &gold, &gold);
    let mut yx = 3;
    while yx <= 6 {
        let mut x = 6;
        while x <= 9 {
            c.place(&lamp, x, yx, 6);
            c.place(&lamp, x, yx, 9);
            x += 3;
        }
        yx += 3;
    }
    c.generate_box(5, 1, 6, 5, 2, 6, &light, &light);
    c.generate_box(5, 1, 9, 5, 2, 9, &light, &light);
    c.generate_box(10, 1, 6, 10, 2, 6, &light, &light);
    c.generate_box(10, 1, 9, 10, 2, 9, &light, &light);
    c.generate_box(6, 1, 5, 6, 2, 5, &light, &light);
    c.generate_box(9, 1, 5, 9, 2, 5, &light, &light);
    c.generate_box(6, 1, 10, 6, 2, 10, &light, &light);
    c.generate_box(9, 1, 10, 9, 2, 10, &light, &light);
    c.generate_box(5, 2, 5, 5, 6, 5, &light, &light);
    c.generate_box(5, 2, 10, 5, 6, 10, &light, &light);
    c.generate_box(10, 2, 5, 10, 6, 5, &light, &light);
    c.generate_box(10, 2, 10, 10, 6, 10, &light, &light);
    c.generate_box(5, 7, 1, 5, 7, 6, &light, &light);
    c.generate_box(10, 7, 1, 10, 7, 6, &light, &light);
    c.generate_box(5, 7, 9, 5, 7, 14, &light, &light);
    c.generate_box(10, 7, 9, 10, 7, 14, &light, &light);
    c.generate_box(1, 7, 5, 6, 7, 5, &light, &light);
    c.generate_box(1, 7, 10, 6, 7, 10, &light, &light);
    c.generate_box(9, 7, 5, 14, 7, 5, &light, &light);
    c.generate_box(9, 7, 10, 14, 7, 10, &light, &light);
    c.generate_box(2, 1, 2, 2, 1, 3, &light, &light);
    c.generate_box(3, 1, 2, 3, 1, 2, &light, &light);
    c.generate_box(13, 1, 2, 13, 1, 3, &light, &light);
    c.generate_box(12, 1, 2, 12, 1, 2, &light, &light);
    c.generate_box(2, 1, 12, 2, 1, 13, &light, &light);
    c.generate_box(3, 1, 13, 3, 1, 13, &light, &light);
    c.generate_box(13, 1, 12, 13, 1, 13, &light, &light);
    c.generate_box(12, 1, 13, 12, 1, 13, &light, &light);
    c.finish(RoomKind::Core.piece_id(), 1)
}

/// `OceanMonumentDoubleXRoom.postProcess`. `west = this.roomDefinition`,
/// `east = west.connections[EAST]`.
fn build_double_x_room(piece: &Piece, anchor: usize, arena: &[RoomDef], ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let lamp = BlockState::of(LAMP_BLOCK);
    let west = &arena[anchor];
    let east = &arena[west.connections[EAST].expect("DoubleXRoom anchor always has an EAST neighbour")];
    if west.index / 25 > 0 {
        c.generate_default_floor(8, 0, east.has_opening[DOWN]);
        c.generate_default_floor(0, 0, west.has_opening[DOWN]);
    }
    if west.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 4, 1, 7, 4, 6, &gray);
    }
    if east.connections[UP].is_none() {
        c.generate_box_on_fill_only(8, 4, 1, 14, 4, 6, &gray);
    }
    c.generate_box(0, 3, 0, 0, 3, 7, &light, &light);
    c.generate_box(15, 3, 0, 15, 3, 7, &light, &light);
    c.generate_box(1, 3, 0, 15, 3, 0, &light, &light);
    c.generate_box(1, 3, 7, 14, 3, 7, &light, &light);
    c.generate_box(0, 2, 0, 0, 2, 7, &gray, &gray);
    c.generate_box(15, 2, 0, 15, 2, 7, &gray, &gray);
    c.generate_box(1, 2, 0, 15, 2, 0, &gray, &gray);
    c.generate_box(1, 2, 7, 14, 2, 7, &gray, &gray);
    c.generate_box(0, 1, 0, 0, 1, 7, &light, &light);
    c.generate_box(15, 1, 0, 15, 1, 7, &light, &light);
    c.generate_box(1, 1, 0, 15, 1, 0, &light, &light);
    c.generate_box(1, 1, 7, 14, 1, 7, &light, &light);
    c.generate_box(5, 1, 0, 10, 1, 4, &light, &light);
    c.generate_box(6, 2, 0, 9, 2, 3, &gray, &gray);
    c.generate_box(5, 3, 0, 10, 3, 4, &light, &light);
    c.place(&lamp, 6, 2, 3);
    c.place(&lamp, 9, 2, 3);
    if west.has_opening[SOUTH] {
        c.generate_water_box(3, 1, 0, 4, 2, 0);
    }
    if west.has_opening[NORTH] {
        c.generate_water_box(3, 1, 7, 4, 2, 7);
    }
    if west.has_opening[WEST] {
        c.generate_water_box(0, 1, 3, 0, 2, 4);
    }
    if east.has_opening[SOUTH] {
        c.generate_water_box(11, 1, 0, 12, 2, 0);
    }
    if east.has_opening[NORTH] {
        c.generate_water_box(11, 1, 7, 12, 2, 7);
    }
    if east.has_opening[EAST] {
        c.generate_water_box(15, 1, 3, 15, 2, 4);
    }
    c.finish(RoomKind::DoubleX.piece_id(), 1)
}

/// `OceanMonumentDoubleXYRoom.postProcess`.
fn build_double_xy_room(piece: &Piece, anchor: usize, arena: &[RoomDef], ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let lamp = BlockState::of(LAMP_BLOCK);
    let west_idx = anchor;
    let east_idx = arena[west_idx].connections[EAST].expect("DoubleXYRoom anchor always has an EAST neighbour");
    let west_up_idx = arena[west_idx].connections[UP].expect("DoubleXYRoom anchor always has an UP neighbour");
    let east_up_idx = arena[east_idx].connections[UP].expect("DoubleXYRoom east neighbour always has an UP neighbour");
    let west = &arena[west_idx];
    let east = &arena[east_idx];
    let west_up = &arena[west_up_idx];
    let east_up = &arena[east_up_idx];
    if west.index / 25 > 0 {
        c.generate_default_floor(8, 0, east.has_opening[DOWN]);
        c.generate_default_floor(0, 0, west.has_opening[DOWN]);
    }
    if west_up.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 8, 1, 7, 8, 6, &gray);
    }
    if east_up.connections[UP].is_none() {
        c.generate_box_on_fill_only(8, 8, 1, 14, 8, 6, &gray);
    }
    for y in 1..=7 {
        let block = if y == 2 || y == 6 { gray.clone() } else { light.clone() };
        c.generate_box(0, y, 0, 0, y, 7, &block, &block);
        c.generate_box(15, y, 0, 15, y, 7, &block, &block);
        c.generate_box(1, y, 0, 15, y, 0, &block, &block);
        c.generate_box(1, y, 7, 14, y, 7, &block, &block);
    }
    c.generate_box(2, 1, 3, 2, 7, 4, &light, &light);
    c.generate_box(3, 1, 2, 4, 7, 2, &light, &light);
    c.generate_box(3, 1, 5, 4, 7, 5, &light, &light);
    c.generate_box(13, 1, 3, 13, 7, 4, &light, &light);
    c.generate_box(11, 1, 2, 12, 7, 2, &light, &light);
    c.generate_box(11, 1, 5, 12, 7, 5, &light, &light);
    c.generate_box(5, 1, 3, 5, 3, 4, &light, &light);
    c.generate_box(10, 1, 3, 10, 3, 4, &light, &light);
    c.generate_box(5, 7, 2, 10, 7, 5, &light, &light);
    c.generate_box(5, 5, 2, 5, 7, 2, &light, &light);
    c.generate_box(10, 5, 2, 10, 7, 2, &light, &light);
    c.generate_box(5, 5, 5, 5, 7, 5, &light, &light);
    c.generate_box(10, 5, 5, 10, 7, 5, &light, &light);
    c.place(&light, 6, 6, 2);
    c.place(&light, 9, 6, 2);
    c.place(&light, 6, 6, 5);
    c.place(&light, 9, 6, 5);
    c.generate_box(5, 4, 3, 6, 4, 4, &light, &light);
    c.generate_box(9, 4, 3, 10, 4, 4, &light, &light);
    c.place(&lamp, 5, 4, 2);
    c.place(&lamp, 5, 4, 5);
    c.place(&lamp, 10, 4, 2);
    c.place(&lamp, 10, 4, 5);
    if west.has_opening[SOUTH] {
        c.generate_water_box(3, 1, 0, 4, 2, 0);
    }
    if west.has_opening[NORTH] {
        c.generate_water_box(3, 1, 7, 4, 2, 7);
    }
    if west.has_opening[WEST] {
        c.generate_water_box(0, 1, 3, 0, 2, 4);
    }
    if east.has_opening[SOUTH] {
        c.generate_water_box(11, 1, 0, 12, 2, 0);
    }
    if east.has_opening[NORTH] {
        c.generate_water_box(11, 1, 7, 12, 2, 7);
    }
    if east.has_opening[EAST] {
        c.generate_water_box(15, 1, 3, 15, 2, 4);
    }
    if west_up.has_opening[SOUTH] {
        c.generate_water_box(3, 5, 0, 4, 6, 0);
    }
    if west_up.has_opening[NORTH] {
        c.generate_water_box(3, 5, 7, 4, 6, 7);
    }
    if west_up.has_opening[WEST] {
        c.generate_water_box(0, 5, 3, 0, 6, 4);
    }
    if east_up.has_opening[SOUTH] {
        c.generate_water_box(11, 5, 0, 12, 6, 0);
    }
    if east_up.has_opening[NORTH] {
        c.generate_water_box(11, 5, 7, 12, 6, 7);
    }
    if east_up.has_opening[EAST] {
        c.generate_water_box(15, 5, 3, 15, 6, 4);
    }
    c.finish(RoomKind::DoubleXy.piece_id(), 1)
}

/// `OceanMonumentDoubleYRoom.postProcess`.
fn build_double_y_room(piece: &Piece, anchor: usize, arena: &[RoomDef], ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let lower_idx = anchor;
    if arena[lower_idx].index / 25 > 0 {
        let down_opening = arena[lower_idx].has_opening[DOWN];
        c.generate_default_floor(0, 0, down_opening);
    }
    let above_idx = arena[lower_idx].connections[UP].expect("DoubleYRoom anchor always has an UP neighbour");
    if arena[above_idx].connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 8, 1, 6, 8, 6, &gray);
    }
    c.generate_box(0, 4, 0, 0, 4, 7, &light, &light);
    c.generate_box(7, 4, 0, 7, 4, 7, &light, &light);
    c.generate_box(1, 4, 0, 6, 4, 0, &light, &light);
    c.generate_box(1, 4, 7, 6, 4, 7, &light, &light);
    c.generate_box(2, 4, 1, 2, 4, 2, &light, &light);
    c.generate_box(1, 4, 2, 1, 4, 2, &light, &light);
    c.generate_box(5, 4, 1, 5, 4, 2, &light, &light);
    c.generate_box(6, 4, 2, 6, 4, 2, &light, &light);
    c.generate_box(2, 4, 5, 2, 4, 6, &light, &light);
    c.generate_box(1, 4, 5, 1, 4, 5, &light, &light);
    c.generate_box(5, 4, 5, 5, 4, 6, &light, &light);
    c.generate_box(6, 4, 5, 6, 4, 5, &light, &light);

    let mut level_idx = lower_idx;
    let mut y = 1;
    while y <= 5 {
        let def = &arena[level_idx];
        let z0 = 0;
        if def.has_opening[SOUTH] {
            c.generate_box(2, y, z0, 2, y + 2, z0, &light, &light);
            c.generate_box(5, y, z0, 5, y + 2, z0, &light, &light);
            c.generate_box(3, y + 2, z0, 4, y + 2, z0, &light, &light);
        } else {
            c.generate_box(0, y, z0, 7, y + 2, z0, &light, &light);
            c.generate_box(0, y + 1, z0, 7, y + 1, z0, &gray, &gray);
        }
        let z1 = 7;
        if def.has_opening[NORTH] {
            c.generate_box(2, y, z1, 2, y + 2, z1, &light, &light);
            c.generate_box(5, y, z1, 5, y + 2, z1, &light, &light);
            c.generate_box(3, y + 2, z1, 4, y + 2, z1, &light, &light);
        } else {
            c.generate_box(0, y, z1, 7, y + 2, z1, &light, &light);
            c.generate_box(0, y + 1, z1, 7, y + 1, z1, &gray, &gray);
        }
        let x0 = 0;
        if def.has_opening[WEST] {
            c.generate_box(x0, y, 2, x0, y + 2, 2, &light, &light);
            c.generate_box(x0, y, 5, x0, y + 2, 5, &light, &light);
            c.generate_box(x0, y + 2, 3, x0, y + 2, 4, &light, &light);
        } else {
            c.generate_box(x0, y, 0, x0, y + 2, 7, &light, &light);
            c.generate_box(x0, y + 1, 0, x0, y + 1, 7, &gray, &gray);
        }
        let x1 = 7;
        if def.has_opening[EAST] {
            c.generate_box(x1, y, 2, x1, y + 2, 2, &light, &light);
            c.generate_box(x1, y, 5, x1, y + 2, 5, &light, &light);
            c.generate_box(x1, y + 2, 3, x1, y + 2, 4, &light, &light);
        } else {
            c.generate_box(x1, y, 0, x1, y + 2, 7, &light, &light);
            c.generate_box(x1, y + 1, 0, x1, y + 1, 7, &gray, &gray);
        }
        level_idx = above_idx;
        y += 4;
    }
    c.finish(RoomKind::DoubleY.piece_id(), 1)
}

/// `OceanMonumentDoubleYZRoom.postProcess`. `south = this.roomDefinition`,
/// `north = south.connections[NORTH]`.
fn build_double_yz_room(piece: &Piece, anchor: usize, arena: &[RoomDef], ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let black = BlockState::of(BASE_BLACK);
    let lamp = BlockState::of(LAMP_BLOCK);
    let south_idx = anchor;
    let north_idx = arena[south_idx].connections[NORTH].expect("DoubleYZRoom anchor always has a NORTH neighbour");
    let north_up_idx = arena[north_idx].connections[UP].expect("DoubleYZRoom north neighbour always has an UP neighbour");
    let south_up_idx = arena[south_idx].connections[UP].expect("DoubleYZRoom anchor always has an UP neighbour");
    let south = &arena[south_idx];
    let north = &arena[north_idx];
    let north_up = &arena[north_up_idx];
    let south_up = &arena[south_up_idx];
    if south.index / 25 > 0 {
        c.generate_default_floor(0, 8, north.has_opening[DOWN]);
        c.generate_default_floor(0, 0, south.has_opening[DOWN]);
    }
    if south_up.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 8, 1, 6, 8, 7, &gray);
    }
    if north_up.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 8, 8, 6, 8, 14, &gray);
    }
    for y in 1..=7 {
        let block = if y == 2 || y == 6 { gray.clone() } else { light.clone() };
        c.generate_box(0, y, 0, 0, y, 15, &block, &block);
        c.generate_box(7, y, 0, 7, y, 15, &block, &block);
        c.generate_box(1, y, 0, 6, y, 0, &block, &block);
        c.generate_box(1, y, 15, 6, y, 15, &block, &block);
    }
    for y in 1..=7 {
        let block = if y == 2 || y == 6 { lamp.clone() } else { black.clone() };
        c.generate_box(3, y, 7, 4, y, 8, &block, &block);
    }
    if south.has_opening[SOUTH] {
        c.generate_water_box(3, 1, 0, 4, 2, 0);
    }
    if south.has_opening[EAST] {
        c.generate_water_box(7, 1, 3, 7, 2, 4);
    }
    if south.has_opening[WEST] {
        c.generate_water_box(0, 1, 3, 0, 2, 4);
    }
    if north.has_opening[NORTH] {
        c.generate_water_box(3, 1, 15, 4, 2, 15);
    }
    if north.has_opening[WEST] {
        c.generate_water_box(0, 1, 11, 0, 2, 12);
    }
    if north.has_opening[EAST] {
        c.generate_water_box(7, 1, 11, 7, 2, 12);
    }
    if south_up.has_opening[SOUTH] {
        c.generate_water_box(3, 5, 0, 4, 6, 0);
    }
    if south_up.has_opening[EAST] {
        c.generate_water_box(7, 5, 3, 7, 6, 4);
        c.generate_box(5, 4, 2, 6, 4, 5, &light, &light);
        c.generate_box(6, 1, 2, 6, 3, 2, &light, &light);
        c.generate_box(6, 1, 5, 6, 3, 5, &light, &light);
    }
    if south_up.has_opening[WEST] {
        c.generate_water_box(0, 5, 3, 0, 6, 4);
        c.generate_box(1, 4, 2, 2, 4, 5, &light, &light);
        c.generate_box(1, 1, 2, 1, 3, 2, &light, &light);
        c.generate_box(1, 1, 5, 1, 3, 5, &light, &light);
    }
    if north_up.has_opening[NORTH] {
        c.generate_water_box(3, 5, 15, 4, 6, 15);
    }
    if north_up.has_opening[WEST] {
        c.generate_water_box(0, 5, 11, 0, 6, 12);
        c.generate_box(1, 4, 10, 2, 4, 13, &light, &light);
        c.generate_box(1, 1, 10, 1, 3, 10, &light, &light);
        c.generate_box(1, 1, 13, 1, 3, 13, &light, &light);
    }
    if north_up.has_opening[EAST] {
        c.generate_water_box(7, 5, 11, 7, 6, 12);
        c.generate_box(5, 4, 10, 6, 4, 13, &light, &light);
        c.generate_box(6, 1, 10, 6, 3, 10, &light, &light);
        c.generate_box(6, 1, 13, 6, 3, 13, &light, &light);
    }
    c.finish(RoomKind::DoubleYz.piece_id(), 1)
}

/// `OceanMonumentDoubleZRoom.postProcess`. `south = this.roomDefinition`,
/// `north = south.connections[NORTH]`.
fn build_double_z_room(piece: &Piece, anchor: usize, arena: &[RoomDef], ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let lamp = BlockState::of(LAMP_BLOCK);
    let south_idx = anchor;
    let north_idx = arena[south_idx].connections[NORTH].expect("DoubleZRoom anchor always has a NORTH neighbour");
    let south = &arena[south_idx];
    let north = &arena[north_idx];
    if south.index / 25 > 0 {
        c.generate_default_floor(0, 8, north.has_opening[DOWN]);
        c.generate_default_floor(0, 0, south.has_opening[DOWN]);
    }
    if south.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 4, 1, 6, 4, 7, &gray);
    }
    if north.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 4, 8, 6, 4, 14, &gray);
    }
    c.generate_box(0, 3, 0, 0, 3, 15, &light, &light);
    c.generate_box(7, 3, 0, 7, 3, 15, &light, &light);
    c.generate_box(1, 3, 0, 7, 3, 0, &light, &light);
    c.generate_box(1, 3, 15, 6, 3, 15, &light, &light);
    c.generate_box(0, 2, 0, 0, 2, 15, &gray, &gray);
    c.generate_box(7, 2, 0, 7, 2, 15, &gray, &gray);
    c.generate_box(1, 2, 0, 7, 2, 0, &gray, &gray);
    c.generate_box(1, 2, 15, 6, 2, 15, &gray, &gray);
    c.generate_box(0, 1, 0, 0, 1, 15, &light, &light);
    c.generate_box(7, 1, 0, 7, 1, 15, &light, &light);
    c.generate_box(1, 1, 0, 7, 1, 0, &light, &light);
    c.generate_box(1, 1, 15, 6, 1, 15, &light, &light);
    c.generate_box(1, 1, 1, 1, 1, 2, &light, &light);
    c.generate_box(6, 1, 1, 6, 1, 2, &light, &light);
    c.generate_box(1, 3, 1, 1, 3, 2, &light, &light);
    c.generate_box(6, 3, 1, 6, 3, 2, &light, &light);
    c.generate_box(1, 1, 13, 1, 1, 14, &light, &light);
    c.generate_box(6, 1, 13, 6, 1, 14, &light, &light);
    c.generate_box(1, 3, 13, 1, 3, 14, &light, &light);
    c.generate_box(6, 3, 13, 6, 3, 14, &light, &light);
    c.generate_box(2, 1, 6, 2, 3, 6, &light, &light);
    c.generate_box(5, 1, 6, 5, 3, 6, &light, &light);
    c.generate_box(2, 1, 9, 2, 3, 9, &light, &light);
    c.generate_box(5, 1, 9, 5, 3, 9, &light, &light);
    c.generate_box(3, 2, 6, 4, 2, 6, &light, &light);
    c.generate_box(3, 2, 9, 4, 2, 9, &light, &light);
    c.generate_box(2, 2, 7, 2, 2, 8, &light, &light);
    c.generate_box(5, 2, 7, 5, 2, 8, &light, &light);
    c.place(&lamp, 2, 2, 5);
    c.place(&lamp, 5, 2, 5);
    c.place(&lamp, 2, 2, 10);
    c.place(&lamp, 5, 2, 10);
    c.place(&light, 2, 3, 5);
    c.place(&light, 5, 3, 5);
    c.place(&light, 2, 3, 10);
    c.place(&light, 5, 3, 10);
    if south.has_opening[SOUTH] {
        c.generate_water_box(3, 1, 0, 4, 2, 0);
    }
    if south.has_opening[EAST] {
        c.generate_water_box(7, 1, 3, 7, 2, 4);
    }
    if south.has_opening[WEST] {
        c.generate_water_box(0, 1, 3, 0, 2, 4);
    }
    if north.has_opening[NORTH] {
        c.generate_water_box(3, 1, 15, 4, 2, 15);
    }
    if north.has_opening[WEST] {
        c.generate_water_box(0, 1, 11, 0, 2, 12);
    }
    if north.has_opening[EAST] {
        c.generate_water_box(7, 1, 11, 7, 2, 12);
    }
    c.finish(RoomKind::DoubleZ.piece_id(), 1)
}

/// `OceanMonumentSimpleRoom.postProcess`.
fn build_simple_room<R: RandomSource>(
    piece: &Piece,
    anchor: usize,
    arena: &[RoomDef],
    main_design: i32,
    ctx: &dyn StartContext,
    random: &mut R,
) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let black = BlockState::of(BASE_BLACK);
    let lamp = BlockState::of(LAMP_BLOCK);
    let def = &arena[anchor];
    if def.index / 25 > 0 {
        c.generate_default_floor(0, 0, def.has_opening[DOWN]);
    }
    if def.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 4, 1, 6, 4, 6, &gray);
    }
    // `centerPillar`'s `&&` short-circuits: `random.nextBoolean()` is drawn
    // only when `mainDesign != 0`, matching vanilla exactly (the `random`
    // stream position depends on this).
    let center_pillar = main_design != 0
        && random.next_bool()
        && !def.has_opening[DOWN]
        && !def.has_opening[UP]
        && def.count_openings() > 1;
    if main_design == 0 {
        c.generate_box(0, 1, 0, 2, 1, 2, &light, &light);
        c.generate_box(0, 3, 0, 2, 3, 2, &light, &light);
        c.generate_box(0, 2, 0, 0, 2, 2, &gray, &gray);
        c.generate_box(1, 2, 0, 2, 2, 0, &gray, &gray);
        c.place(&lamp, 1, 2, 1);
        c.generate_box(5, 1, 0, 7, 1, 2, &light, &light);
        c.generate_box(5, 3, 0, 7, 3, 2, &light, &light);
        c.generate_box(7, 2, 0, 7, 2, 2, &gray, &gray);
        c.generate_box(5, 2, 0, 6, 2, 0, &gray, &gray);
        c.place(&lamp, 6, 2, 1);
        c.generate_box(0, 1, 5, 2, 1, 7, &light, &light);
        c.generate_box(0, 3, 5, 2, 3, 7, &light, &light);
        c.generate_box(0, 2, 5, 0, 2, 7, &gray, &gray);
        c.generate_box(1, 2, 7, 2, 2, 7, &gray, &gray);
        c.place(&lamp, 1, 2, 6);
        c.generate_box(5, 1, 5, 7, 1, 7, &light, &light);
        c.generate_box(5, 3, 5, 7, 3, 7, &light, &light);
        c.generate_box(7, 2, 5, 7, 2, 7, &gray, &gray);
        c.generate_box(5, 2, 7, 6, 2, 7, &gray, &gray);
        c.place(&lamp, 6, 2, 6);
        if def.has_opening[SOUTH] {
            c.generate_box(3, 3, 0, 4, 3, 0, &light, &light);
        } else {
            c.generate_box(3, 3, 0, 4, 3, 1, &light, &light);
            c.generate_box(3, 2, 0, 4, 2, 0, &gray, &gray);
            c.generate_box(3, 1, 0, 4, 1, 1, &light, &light);
        }
        if def.has_opening[NORTH] {
            c.generate_box(3, 3, 7, 4, 3, 7, &light, &light);
        } else {
            c.generate_box(3, 3, 6, 4, 3, 7, &light, &light);
            c.generate_box(3, 2, 7, 4, 2, 7, &gray, &gray);
            c.generate_box(3, 1, 6, 4, 1, 7, &light, &light);
        }
        if def.has_opening[WEST] {
            c.generate_box(0, 3, 3, 0, 3, 4, &light, &light);
        } else {
            c.generate_box(0, 3, 3, 1, 3, 4, &light, &light);
            c.generate_box(0, 2, 3, 0, 2, 4, &gray, &gray);
            c.generate_box(0, 1, 3, 1, 1, 4, &light, &light);
        }
        if def.has_opening[EAST] {
            c.generate_box(7, 3, 3, 7, 3, 4, &light, &light);
        } else {
            c.generate_box(6, 3, 3, 7, 3, 4, &light, &light);
            c.generate_box(7, 2, 3, 7, 2, 4, &gray, &gray);
            c.generate_box(6, 1, 3, 7, 1, 4, &light, &light);
        }
    } else if main_design == 1 {
        c.generate_box(2, 1, 2, 2, 3, 2, &light, &light);
        c.generate_box(2, 1, 5, 2, 3, 5, &light, &light);
        c.generate_box(5, 1, 5, 5, 3, 5, &light, &light);
        c.generate_box(5, 1, 2, 5, 3, 2, &light, &light);
        c.place(&lamp, 2, 2, 2);
        c.place(&lamp, 2, 2, 5);
        c.place(&lamp, 5, 2, 5);
        c.place(&lamp, 5, 2, 2);
        c.generate_box(0, 1, 0, 1, 3, 0, &light, &light);
        c.generate_box(0, 1, 1, 0, 3, 1, &light, &light);
        c.generate_box(0, 1, 7, 1, 3, 7, &light, &light);
        c.generate_box(0, 1, 6, 0, 3, 6, &light, &light);
        c.generate_box(6, 1, 7, 7, 3, 7, &light, &light);
        c.generate_box(7, 1, 6, 7, 3, 6, &light, &light);
        c.generate_box(6, 1, 0, 7, 3, 0, &light, &light);
        c.generate_box(7, 1, 1, 7, 3, 1, &light, &light);
        c.place(&gray, 1, 2, 0);
        c.place(&gray, 0, 2, 1);
        c.place(&gray, 1, 2, 7);
        c.place(&gray, 0, 2, 6);
        c.place(&gray, 6, 2, 7);
        c.place(&gray, 7, 2, 6);
        c.place(&gray, 6, 2, 0);
        c.place(&gray, 7, 2, 1);
        if !def.has_opening[SOUTH] {
            c.generate_box(1, 3, 0, 6, 3, 0, &light, &light);
            c.generate_box(1, 2, 0, 6, 2, 0, &gray, &gray);
            c.generate_box(1, 1, 0, 6, 1, 0, &light, &light);
        }
        if !def.has_opening[NORTH] {
            c.generate_box(1, 3, 7, 6, 3, 7, &light, &light);
            c.generate_box(1, 2, 7, 6, 2, 7, &gray, &gray);
            c.generate_box(1, 1, 7, 6, 1, 7, &light, &light);
        }
        if !def.has_opening[WEST] {
            c.generate_box(0, 3, 1, 0, 3, 6, &light, &light);
            c.generate_box(0, 2, 1, 0, 2, 6, &gray, &gray);
            c.generate_box(0, 1, 1, 0, 1, 6, &light, &light);
        }
        if !def.has_opening[EAST] {
            c.generate_box(7, 3, 1, 7, 3, 6, &light, &light);
            c.generate_box(7, 2, 1, 7, 2, 6, &gray, &gray);
            c.generate_box(7, 1, 1, 7, 1, 6, &light, &light);
        }
    } else if main_design == 2 {
        c.generate_box(0, 1, 0, 0, 1, 7, &light, &light);
        c.generate_box(7, 1, 0, 7, 1, 7, &light, &light);
        c.generate_box(1, 1, 0, 6, 1, 0, &light, &light);
        c.generate_box(1, 1, 7, 6, 1, 7, &light, &light);
        c.generate_box(0, 2, 0, 0, 2, 7, &black, &black);
        c.generate_box(7, 2, 0, 7, 2, 7, &black, &black);
        c.generate_box(1, 2, 0, 6, 2, 0, &black, &black);
        c.generate_box(1, 2, 7, 6, 2, 7, &black, &black);
        c.generate_box(0, 3, 0, 0, 3, 7, &light, &light);
        c.generate_box(7, 3, 0, 7, 3, 7, &light, &light);
        c.generate_box(1, 3, 0, 6, 3, 0, &light, &light);
        c.generate_box(1, 3, 7, 6, 3, 7, &light, &light);
        c.generate_box(0, 1, 3, 0, 2, 4, &black, &black);
        c.generate_box(7, 1, 3, 7, 2, 4, &black, &black);
        c.generate_box(3, 1, 0, 4, 2, 0, &black, &black);
        c.generate_box(3, 1, 7, 4, 2, 7, &black, &black);
        if def.has_opening[SOUTH] {
            c.generate_water_box(3, 1, 0, 4, 2, 0);
        }
        if def.has_opening[NORTH] {
            c.generate_water_box(3, 1, 7, 4, 2, 7);
        }
        if def.has_opening[WEST] {
            c.generate_water_box(0, 1, 3, 0, 2, 4);
        }
        if def.has_opening[EAST] {
            c.generate_water_box(7, 1, 3, 7, 2, 4);
        }
    }
    if center_pillar {
        c.generate_box(3, 1, 3, 4, 1, 4, &light, &light);
        c.generate_box(3, 2, 3, 4, 2, 4, &gray, &gray);
        c.generate_box(3, 3, 3, 4, 3, 4, &light, &light);
    }
    c.finish(RoomKind::Simple { main_design }.piece_id(), 1)
}

/// `OceanMonumentSimpleTopRoom.postProcess`.
fn build_simple_top_room<R: RandomSource>(
    piece: &Piece,
    anchor: usize,
    arena: &[RoomDef],
    ctx: &dyn StartContext,
    random: &mut R,
) -> StructurePiece {
    let mut c = Canvas::new(piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let black = BlockState::of(BASE_BLACK);
    let wet_sponge = BlockState::of(WET_SPONGE);
    let def = &arena[anchor];
    if def.index / 25 > 0 {
        c.generate_default_floor(0, 0, def.has_opening[DOWN]);
    }
    if def.connections[UP].is_none() {
        c.generate_box_on_fill_only(1, 4, 1, 6, 4, 6, &BlockState::of(BASE_GRAY));
    }
    for x in 1..=6 {
        for z in 1..=6 {
            if random.next_int_bounded(3) != 0 {
                let y0 = 2 + if random.next_int_bounded(4) == 0 { 0 } else { 1 };
                c.generate_box(x, y0, z, x, 3, z, &wet_sponge, &wet_sponge);
            }
        }
    }
    c.generate_box(0, 1, 0, 0, 1, 7, &light, &light);
    c.generate_box(7, 1, 0, 7, 1, 7, &light, &light);
    c.generate_box(1, 1, 0, 6, 1, 0, &light, &light);
    c.generate_box(1, 1, 7, 6, 1, 7, &light, &light);
    c.generate_box(0, 2, 0, 0, 2, 7, &black, &black);
    c.generate_box(7, 2, 0, 7, 2, 7, &black, &black);
    c.generate_box(1, 2, 0, 6, 2, 0, &black, &black);
    c.generate_box(1, 2, 7, 6, 2, 7, &black, &black);
    c.generate_box(0, 3, 0, 0, 3, 7, &light, &light);
    c.generate_box(7, 3, 0, 7, 3, 7, &light, &light);
    c.generate_box(1, 3, 0, 6, 3, 0, &light, &light);
    c.generate_box(1, 3, 7, 6, 3, 7, &light, &light);
    c.generate_box(0, 1, 3, 0, 2, 4, &black, &black);
    c.generate_box(7, 1, 3, 7, 2, 4, &black, &black);
    c.generate_box(3, 1, 0, 4, 2, 0, &black, &black);
    c.generate_box(3, 1, 7, 4, 2, 7, &black, &black);
    if def.has_opening[SOUTH] {
        c.generate_water_box(3, 1, 0, 4, 2, 0);
    }
    c.finish(RoomKind::SimpleTop.piece_id(), 1)
}

/// `OceanMonumentWingRoom.postProcess`. `mainDesign = randomValue & 1`,
/// resolved by [`generate`] from the single `wingRandom = random.nextInt()`
/// draw. `spawnElder` (the design-0 elder guardian) is skipped — see the
/// module doc's entity-spawning deviation.
fn build_wing_room(piece: Piece, main_design: i32, ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(&piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let black = BlockState::of(BASE_BLACK);
    let lamp = BlockState::of(LAMP_BLOCK);
    if main_design == 0 {
        for i in 0..4 {
            c.generate_box(10 - i, 3 - i, 20 - i, 12 + i, 3 - i, 20, &light, &light);
        }
        c.generate_box(7, 0, 6, 15, 0, 16, &light, &light);
        c.generate_box(6, 0, 6, 6, 3, 20, &light, &light);
        c.generate_box(16, 0, 6, 16, 3, 20, &light, &light);
        c.generate_box(7, 1, 7, 7, 1, 20, &light, &light);
        c.generate_box(15, 1, 7, 15, 1, 20, &light, &light);
        c.generate_box(7, 1, 6, 9, 3, 6, &light, &light);
        c.generate_box(13, 1, 6, 15, 3, 6, &light, &light);
        c.generate_box(8, 1, 7, 9, 1, 7, &light, &light);
        c.generate_box(13, 1, 7, 14, 1, 7, &light, &light);
        c.generate_box(9, 0, 5, 13, 0, 5, &light, &light);
        c.generate_box(10, 0, 7, 12, 0, 7, &black, &black);
        c.generate_box(8, 0, 10, 8, 0, 12, &black, &black);
        c.generate_box(14, 0, 10, 14, 0, 12, &black, &black);
        let mut z = 18;
        while z >= 7 {
            c.place(&lamp, 6, 3, z);
            c.place(&lamp, 16, 3, z);
            z -= 3;
        }
        c.place(&lamp, 10, 0, 10);
        c.place(&lamp, 12, 0, 10);
        c.place(&lamp, 10, 0, 12);
        c.place(&lamp, 12, 0, 12);
        c.place(&lamp, 8, 3, 6);
        c.place(&lamp, 14, 3, 6);
        c.place(&light, 4, 2, 4);
        c.place(&lamp, 4, 1, 4);
        c.place(&light, 4, 0, 4);
        c.place(&light, 18, 2, 4);
        c.place(&lamp, 18, 1, 4);
        c.place(&light, 18, 0, 4);
        c.place(&light, 4, 2, 18);
        c.place(&lamp, 4, 1, 18);
        c.place(&light, 4, 0, 18);
        c.place(&light, 18, 2, 18);
        c.place(&lamp, 18, 1, 18);
        c.place(&light, 18, 0, 18);
        c.place(&light, 9, 7, 20);
        c.place(&light, 13, 7, 20);
        c.generate_box(6, 0, 21, 7, 4, 21, &light, &light);
        c.generate_box(15, 0, 21, 16, 4, 21, &light, &light);
        // `spawnElder(11, 2, 16)` — ledgered, see the module doc.
    } else if main_design == 1 {
        c.generate_box(9, 3, 18, 13, 3, 20, &light, &light);
        c.generate_box(9, 0, 18, 9, 2, 18, &light, &light);
        c.generate_box(13, 0, 18, 13, 2, 18, &light, &light);
        let mut x = 9;
        for _ in 0..2 {
            c.place(&light, x, 6, 20);
            c.place(&lamp, x, 5, 20);
            c.place(&light, x, 4, 20);
            x = 13;
        }
        c.generate_box(7, 3, 7, 15, 3, 14, &light, &light);
        let mut var14 = 10;
        for _ in 0..2 {
            c.generate_box(var14, 0, 10, var14, 6, 10, &light, &light);
            c.generate_box(var14, 0, 12, var14, 6, 12, &light, &light);
            c.place(&lamp, var14, 0, 10);
            c.place(&lamp, var14, 0, 12);
            c.place(&lamp, var14, 4, 10);
            c.place(&lamp, var14, 4, 12);
            var14 = 12;
        }
        let mut var14 = 8;
        for _ in 0..2 {
            c.generate_box(var14, 0, 7, var14, 2, 7, &light, &light);
            c.generate_box(var14, 0, 14, var14, 2, 14, &light, &light);
            var14 = 14;
        }
        c.generate_box(8, 3, 8, 8, 3, 13, &black, &black);
        c.generate_box(14, 3, 8, 14, 3, 13, &black, &black);
        // `spawnElder(11, 5, 13)` — ledgered, see the module doc.
    }
    c.finish("minecraft:omwr", 1)
}

/// `OceanMonumentPenthouse.postProcess`. `spawnElder` is skipped — see the
/// module doc's entity-spawning deviation.
fn build_penthouse(piece: Piece, ctx: &dyn StartContext) -> StructurePiece {
    let mut c = Canvas::new(&piece, ctx);
    let light = BlockState::of(BASE_LIGHT);
    let gray = BlockState::of(BASE_GRAY);
    let black = BlockState::of(BASE_BLACK);
    let lamp = BlockState::of(LAMP_BLOCK);
    c.generate_box(2, -1, 2, 11, -1, 11, &light, &light);
    c.generate_box(0, -1, 0, 1, -1, 11, &gray, &gray);
    c.generate_box(12, -1, 0, 13, -1, 11, &gray, &gray);
    c.generate_box(2, -1, 0, 11, -1, 1, &gray, &gray);
    c.generate_box(2, -1, 12, 11, -1, 13, &gray, &gray);
    c.generate_box(0, 0, 0, 0, 0, 13, &light, &light);
    c.generate_box(13, 0, 0, 13, 0, 13, &light, &light);
    c.generate_box(1, 0, 0, 12, 0, 0, &light, &light);
    c.generate_box(1, 0, 13, 12, 0, 13, &light, &light);
    let mut i = 2;
    while i <= 11 {
        c.place(&lamp, 0, 0, i);
        c.place(&lamp, 13, 0, i);
        c.place(&lamp, i, 0, 0);
        i += 3;
    }
    c.generate_box(2, 0, 3, 4, 0, 9, &light, &light);
    c.generate_box(9, 0, 3, 11, 0, 9, &light, &light);
    c.generate_box(4, 0, 9, 9, 0, 11, &light, &light);
    c.place(&light, 5, 0, 8);
    c.place(&light, 8, 0, 8);
    c.place(&light, 10, 0, 10);
    c.place(&light, 3, 0, 10);
    c.generate_box(3, 0, 3, 3, 0, 7, &black, &black);
    c.generate_box(10, 0, 3, 10, 0, 7, &black, &black);
    c.generate_box(6, 0, 10, 7, 0, 10, &black, &black);
    let mut x = 3;
    for _ in 0..2 {
        let mut z = 2;
        while z <= 8 {
            c.generate_box(x, 0, z, x, 2, z, &light, &light);
            z += 3;
        }
        x = 10;
    }
    c.generate_box(5, 0, 10, 5, 2, 10, &light, &light);
    c.generate_box(8, 0, 10, 8, 2, 10, &light, &light);
    c.generate_box(6, -1, 7, 7, -1, 8, &black, &black);
    c.generate_water_box(6, -1, 3, 7, -1, 4);
    // `spawnElder(6, 1, 6)` — ledgered, see the module doc.
    c.finish("minecraft:ompenthouse", 1)
}

/// `OceanMonumentStructure.generatePieces` → `MonumentBuilding`'s whole
/// constructor and `postProcess`, flattened into one piece per Java object
/// (shell, entry, core, every grid room, both wings, the penthouse) — see
/// the module doc for why that flattening is sound here.
#[must_use]
pub fn generate(cx: i32, cz: i32, seed: i64, ctx: &dyn StartContext) -> Vec<StructurePiece> {
    let mut random = super::structure_random(seed, cx, cz);
    let west = cx * 16 - 29;
    let north = cz * 16 - 29;
    let direction = Facing::random(&mut random);
    let shell = Piece {
        box_: make_bounding_box_simple(west, 39, north, direction, 58, 23, 58),
        orientation: direction,
    };

    let (mut arena, room_defs, source_idx, core_idx, _roof_idx, _left_wing_idx, _right_wing_idx) = generate_room_graph(&mut random);
    let selected = select_rooms(&mut arena, &room_defs, &mut random);

    // `BlockPos offset = this.getWorldPos(9, 0, 22)` — every grid-room box
    // below is anchored near the origin by `room_bounding_box` and then
    // moved by this same offset, matching `child.getBoundingBox().move(offset)`.
    let offset = shell.world_pos(9, 0, 22);

    let entry_piece = Piece {
        box_: translate(room_bounding_box(arena[source_idx].index, direction, 1, 1, 1), offset),
        orientation: direction,
    };
    let core_piece = Piece {
        box_: translate(room_bounding_box(arena[core_idx].index, direction, 2, 2, 2), offset),
        orientation: direction,
    };

    struct Selected {
        kind: RoomKind,
        piece: Piece,
        anchor: usize,
    }
    let selected_pieces: Vec<Selected> = selected
        .into_iter()
        .map(|rp| {
            let (w, h, d) = rp.kind.dims();
            let box_ = translate(room_bounding_box(arena[rp.anchor].index, direction, w, h, d), offset);
            Selected {
                kind: rp.kind,
                piece: Piece { box_, orientation: direction },
                anchor: rp.anchor,
            }
        })
        .collect();

    // Wings and the penthouse sit in world space directly, via the shell's
    // own `getWorldPos` — no `offset`-translate step, matching vanilla's
    // `BoundingBox.fromCorners(this.getWorldPos(...), this.getWorldPos(...))`.
    let left_wing_box = BoundingBox::from_corners(shell.world_pos(1, 1, 1), shell.world_pos(23, 8, 21));
    let right_wing_box = BoundingBox::from_corners(shell.world_pos(34, 1, 1), shell.world_pos(56, 8, 21));
    let penthouse_box = BoundingBox::from_corners(shell.world_pos(22, 13, 22), shell.world_pos(35, 17, 35));
    // `int wingRandom = random.nextInt();` — one draw; `leftWing` gets it
    // as-is, `rightWing` gets the post-incremented value.
    let wing_random = random.next_int();
    let left_wing_design = wing_random & 1;
    let right_wing_design = wing_random.wrapping_add(1) & 1;

    let mut out = Vec::with_capacity(selected_pieces.len() + 6);
    // The shell resolves first — its writes (the initial flood, walls,
    // roof) must exist before any child's `Canvas` reads them via
    // `flood_default`, matching vanilla's own write order.
    out.push(build_shell(&shell, ctx));
    out.push(build_entry_room(&entry_piece, source_idx, &arena, ctx));
    out.push(build_core_room(&core_piece, ctx));
    for s in &selected_pieces {
        let piece = build_room_piece(&s.piece, s.kind, s.anchor, &arena, ctx, &mut random);
        out.push(piece);
    }
    out.push(build_wing_room(
        Piece { box_: left_wing_box, orientation: direction },
        left_wing_design,
        ctx,
    ));
    out.push(build_wing_room(
        Piece { box_: right_wing_box, orientation: direction },
        right_wing_design,
        ctx,
    ));
    out.push(build_penthouse(Piece { box_: penthouse_box, orientation: direction }, ctx));

    out
}

/// Dispatches a selected grid room to its `postProcess` port. `SimpleRoom`
/// and `SimpleTopRoom` are the only kinds that draw from `random` here — see
/// the module doc's `monument:postprocess_random_unseeded` deviation for why
/// this continues the construction-time stream rather than a fresh one.
fn build_room_piece<R: RandomSource>(
    piece: &Piece,
    kind: RoomKind,
    anchor: usize,
    arena: &[RoomDef],
    ctx: &dyn StartContext,
    random: &mut R,
) -> StructurePiece {
    match kind {
        RoomKind::Entry => build_entry_room(piece, anchor, arena, ctx),
        RoomKind::Core => build_core_room(piece, ctx),
        RoomKind::DoubleX => build_double_x_room(piece, anchor, arena, ctx),
        RoomKind::DoubleXy => build_double_xy_room(piece, anchor, arena, ctx),
        RoomKind::DoubleY => build_double_y_room(piece, anchor, arena, ctx),
        RoomKind::DoubleYz => build_double_yz_room(piece, anchor, arena, ctx),
        RoomKind::DoubleZ => build_double_z_room(piece, anchor, arena, ctx),
        RoomKind::SimpleTop => build_simple_top_room(piece, anchor, arena, ctx, random),
        RoomKind::Simple { main_design } => build_simple_room(piece, anchor, arena, main_design, ctx, random),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedCtx;
    impl StartContext for FixedCtx {
        fn first_occupied_height(&self, _x: i32, _z: i32, _h: super::super::HeightmapKind) -> i32 {
            50
        }
        fn biome_at_quart(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:deep_ocean".to_string()
        }
        fn sea_level(&self) -> i32 {
            63
        }
    }

    const SEEDS: [i64; 6] = [1, 2, -195_764_831, 999_999, 42, 8675309];

    /// `generate` never returns an empty list — `MonumentBuilding`'s
    /// constructor is unconditional, matching [`StructureKind::validity`]'s
    /// `Valid`-when-non-empty treatment of this kind.
    #[test]
    fn every_seed_produces_a_non_empty_piece_list() {
        for &seed in &SEEDS {
            let pieces = generate(0, 0, seed, &FixedCtx);
            assert!(!pieces.is_empty(), "seed {seed}: empty piece list");
        }
    }

    /// The shell is always first (every later `Canvas::flood_default` read
    /// depends on the shell's own writes existing "before" it, per the
    /// module doc), and the fixed core — one entry, one core, two wings, one
    /// penthouse — is always present exactly once.
    #[test]
    fn the_fixed_core_is_present_exactly_once_and_the_shell_leads() {
        let mut mismatches = Vec::new();
        for &seed in &SEEDS {
            let pieces = generate(3, -7, seed, &FixedCtx);
            if pieces.first().map(|p| p.id.as_str()) != Some("minecraft:omb") {
                mismatches.push((seed, "shell not first"));
            }
            let count = |id: &str| pieces.iter().filter(|p| p.id == id).count();
            if count("minecraft:omentry") != 1 {
                mismatches.push((seed, "entry room count"));
            }
            if count("minecraft:omcr") != 1 {
                mismatches.push((seed, "core room count"));
            }
            if count("minecraft:omwr") != 2 {
                mismatches.push((seed, "wing room count"));
            }
            if count("minecraft:ompenthouse") != 1 {
                mismatches.push((seed, "penthouse count"));
            }
            if count("minecraft:omb") != 1 {
                mismatches.push((seed, "shell count"));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:?}");
    }

    /// `RoomDefinition.setConnection` writes both sides of an edge — a
    /// one-sided write here would mean `find_source`/`fits` read a stale
    /// `None` from the far side.
    #[test]
    fn every_connection_is_symmetric() {
        let mut random = super::super::structure_random(1, 0, 0);
        let (arena, _room_defs, ..) = generate_room_graph(&mut random);
        let mut mismatches = Vec::new();
        for (idx, def) in arena.iter().enumerate() {
            for d in 0..6 {
                if let Some(other) = def.connections[d] {
                    if arena[other].connections[opposite(d)] != Some(idx) {
                        mismatches.push((idx, d, other));
                    }
                }
            }
        }
        assert!(mismatches.is_empty(), "asymmetric connections: {mismatches:?}");
    }

    /// `close_openings` never disconnects a cell from the source room — every
    /// one of vanilla's own `findSource` checks guarantees this at
    /// construction time; this test recomputes reachability independently,
    /// over the **final** `has_opening` state, as a control.
    #[test]
    fn every_grid_cell_can_still_reach_the_source_room() {
        let mut mismatches = Vec::new();
        for &seed in &SEEDS {
            let mut random = super::super::structure_random(seed, 0, 0);
            let (arena, room_defs, source_idx, ..) = generate_room_graph(&mut random);
            // BFS over `has_opening` edges from the source.
            let mut reached = vec![false; arena.len()];
            let mut queue = std::collections::VecDeque::new();
            reached[source_idx] = true;
            queue.push_back(source_idx);
            while let Some(idx) = queue.pop_front() {
                for d in 0..6 {
                    if !arena[idx].has_opening[d] {
                        continue;
                    }
                    if let Some(next) = arena[idx].connections[d] {
                        if !reached[next] {
                            reached[next] = true;
                            queue.push_back(next);
                        }
                    }
                }
            }
            for &idx in &room_defs {
                if !reached[idx] {
                    mismatches.push((seed, idx, arena[idx].index));
                }
            }
        }
        assert!(mismatches.is_empty(), "cells unreachable from source: {mismatches:?}");
    }

    /// The claim system is a partition: entry (1 cell) + core (8 cells) +
    /// every selected room's own `(w*h*d)` cell count must sum to exactly the
    /// grid's 46 cells, with no cell claimed twice — `select_rooms` only
    /// claims a cell once `try_double_*`/`fits_simple_top` confirms every
    /// neighbour it needs is unclaimed.
    #[test]
    fn selected_rooms_partition_the_grid_exactly() {
        let mut mismatches = Vec::new();
        for &seed in &SEEDS {
            let mut random = super::super::structure_random(seed, 0, 0);
            let (mut arena, room_defs, ..) = generate_room_graph(&mut random);
            let selected = select_rooms(&mut arena, &room_defs, &mut random);
            let mut cells = 1 + 8; // entry + core
            for rp in &selected {
                let (w, h, d) = rp.kind.dims();
                cells += w * h * d;
            }
            if cells != 46 {
                mismatches.push((seed, cells));
            }
            // Every grid cell (46 of them, `room_defs`'s own length) must be
            // `claimed` after selection — a cell nobody's fitter claimed
            // would mean the cascade's fallback (`FitSimpleRoom`, which
            // always fits) failed to run.
            assert_eq!(room_defs.len(), 46, "seed {seed}: grid cell count drifted");
            for &idx in &room_defs {
                if !arena[idx].claimed {
                    mismatches.push((seed, -(idx as i32) - 1));
                }
            }
        }
        assert!(mismatches.is_empty(), "partition mismatches (idx encoded negative): {mismatches:?}");
    }

    /// No two of the fixed-core-plus-selected pieces overlap — the shell is
    /// excluded (it legitimately encloses everything else by design).
    #[test]
    fn no_two_interior_pieces_overlap() {
        let mut mismatches = Vec::new();
        for &seed in &SEEDS {
            let pieces = generate(-2, 5, seed, &FixedCtx);
            let interior: Vec<_> = pieces.iter().filter(|p| p.id != "minecraft:omb").collect();
            for i in 0..interior.len() {
                for j in (i + 1)..interior.len() {
                    if interior[i].bounding_box.intersects(interior[j].bounding_box) {
                        mismatches.push((seed, i, interior[i].id.clone(), j, interior[j].id.clone()));
                    }
                }
            }
        }
        assert!(mismatches.is_empty(), "overlapping interior pieces: {mismatches:?}");
    }

    /// Determinism: the same seed and chunk produce byte-identical output —
    /// including block lists, which is where `monument:postprocess_random_unseeded`
    /// would show up as noise if the continued stream were not actually
    /// deterministic.
    #[test]
    fn deterministic_for_a_fixed_seed() {
        let a = generate(5, 5, 123, &FixedCtx);
        let b = generate(5, 5, 123, &FixedCtx);
        assert_eq!(a.len(), b.len());
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert_eq!(pa.id, pb.id);
            assert_eq!(pa.bounding_box.min, pb.bounding_box.min);
            assert_eq!(pa.bounding_box.max, pb.bounding_box.max);
            let ba = pa.blocks.as_ref().map(|b| b.len()).unwrap_or(0);
            let bb = pb.blocks.as_ref().map(|b| b.len()).unwrap_or(0);
            assert_eq!(ba, bb, "piece {} block count differs between runs", pa.id);
            if let (Some(ba), Some(bb)) = (&pa.blocks, &pb.blocks) {
                for (x, y) in ba.iter().zip(bb.iter()) {
                    assert_eq!(x.pos, y.pos);
                    assert_eq!(x.state, y.state);
                }
            }
        }
    }

    /// Every piece's id is one of the twelve registered `StructurePieceType`s
    /// this module knows about — a stray id would mean a `RoomKind` variant's
    /// `piece_id()` drifted from the dispatch in [`build_room_piece`].
    #[test]
    fn every_piece_id_is_a_known_monument_type() {
        const KNOWN: &[&str] = &[
            "minecraft:omb",
            "minecraft:omentry",
            "minecraft:omcr",
            "minecraft:omdxr",
            "minecraft:omdxyr",
            "minecraft:omdyr",
            "minecraft:omdyzr",
            "minecraft:omdzr",
            "minecraft:omsimple",
            "minecraft:omsimplet",
            "minecraft:omwr",
            "minecraft:ompenthouse",
        ];
        let mut mismatches = Vec::new();
        for &seed in &SEEDS {
            let pieces = generate(1, 1, seed, &FixedCtx);
            for p in &pieces {
                if !KNOWN.contains(&p.id.as_str()) {
                    mismatches.push((seed, p.id.clone()));
                }
            }
        }
        assert!(mismatches.is_empty(), "unknown piece ids: {mismatches:?}");
    }

    /// The shell's own bounding box is vanilla's fixed 58×23×58 footprint at
    /// a literal Y of 39 — re-derived from `makeBoundingBox(west, 39, north,
    /// direction, 58, 23, 58)`, not asserted from memory.
    #[test]
    fn the_shell_box_is_the_fixed_58x23x58_footprint() {
        for &seed in &SEEDS {
            let pieces = generate(0, 0, seed, &FixedCtx);
            let shell = pieces.first().expect("non-empty");
            let box_ = shell.bounding_box;
            let size = [
                box_.max[0] - box_.min[0] + 1,
                box_.max[1] - box_.min[1] + 1,
                box_.max[2] - box_.min[2] + 1,
            ];
            assert_eq!(size, [58, 23, 58], "seed {seed}: shell box size drifted");
            assert_eq!(box_.min[1], 39, "seed {seed}: shell box Y drifted from the fixed 39");
        }
    }

    /// Every piece emits at least one block — a `RoomKind` whose builder
    /// silently produced an empty list would otherwise look identical to a
    /// correctly-generated one in every test above.
    #[test]
    fn every_piece_places_at_least_one_block() {
        let mut mismatches = Vec::new();
        for &seed in &SEEDS {
            let pieces = generate(2, -3, seed, &FixedCtx);
            for p in &pieces {
                let n = p.blocks.as_ref().map(|b| b.len()).unwrap_or(0);
                if n == 0 {
                    mismatches.push((seed, p.id.clone()));
                }
            }
        }
        assert!(mismatches.is_empty(), "pieces with zero blocks: {mismatches:?}");
    }
}
