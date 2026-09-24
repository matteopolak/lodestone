//! Woodland-mansion layout and template assembly.
//!
//! The generator builds the seeded three-floor grid, classifies its rooms, and
//! emits the complete template piece list: entrance, exterior walls, corridors,
//! carpets, room dividers, doors, room furnishings, and roofs.  Entity data
//! markers remain the responsibility of the server-side structure consumer.

use lodestone_worldgen_core::rng::RandomSource;

use super::processor::Processor;
use super::template::{Mirror, PlaceSettings, Rotation};
use super::{template_piece, StructurePiece, TemplateStore};

const CLEAR: i32 = 0;
const CORRIDOR: i32 = 1;
const ROOM: i32 = 2;
const START_ROOM: i32 = 3;
const BLOCKED: i32 = 5;

const ROOM_1X1: i32 = 1 << 16;
const ROOM_1X2: i32 = 2 << 16;
const ROOM_2X2: i32 = 4 << 16;
const ROOM_ORIGIN: i32 = 1 << 20;
const ROOM_DOOR: i32 = 1 << 21;
const ROOM_STAIRS: i32 = 1 << 22;
const ROOM_CORRIDOR: i32 = 1 << 23;
const ROOM_TYPE_MASK: i32 = 0x0f0000;
const ROOM_ID_MASK: i32 = 0x00ffff;

/// Every template the complete mansion assembly can name.
pub const TEMPLATE_IDS: &[&str] = &[
    "minecraft:woodland_mansion/entrance",
    "minecraft:woodland_mansion/wall_flat",
    "minecraft:woodland_mansion/wall_window",
    "minecraft:woodland_mansion/wall_corner",
    "minecraft:woodland_mansion/corridor_floor",
    "minecraft:woodland_mansion/carpet_north",
    "minecraft:woodland_mansion/carpet_east",
    "minecraft:woodland_mansion/carpet_south_1",
    "minecraft:woodland_mansion/carpet_south_2",
    "minecraft:woodland_mansion/carpet_west_1",
    "minecraft:woodland_mansion/carpet_west_2",
    "minecraft:woodland_mansion/indoors_wall_1",
    "minecraft:woodland_mansion/indoors_wall_2",
    "minecraft:woodland_mansion/indoors_door_1",
    "minecraft:woodland_mansion/indoors_door_2",
    "minecraft:woodland_mansion/roof",
    "minecraft:woodland_mansion/roof_front",
    "minecraft:woodland_mansion/roof_corner",
    "minecraft:woodland_mansion/roof_inner_corner",
    "minecraft:woodland_mansion/small_wall",
    "minecraft:woodland_mansion/small_wall_corner",
    "minecraft:woodland_mansion/1x1_a1",
    "minecraft:woodland_mansion/1x1_a2",
    "minecraft:woodland_mansion/1x1_a3",
    "minecraft:woodland_mansion/1x1_a4",
    "minecraft:woodland_mansion/1x1_a5",
    "minecraft:woodland_mansion/1x1_b1",
    "minecraft:woodland_mansion/1x1_b2",
    "minecraft:woodland_mansion/1x1_b3",
    "minecraft:woodland_mansion/1x1_b4",
    "minecraft:woodland_mansion/1x1_b5",
    "minecraft:woodland_mansion/1x1_as1",
    "minecraft:woodland_mansion/1x1_as2",
    "minecraft:woodland_mansion/1x1_as3",
    "minecraft:woodland_mansion/1x1_as4",
    "minecraft:woodland_mansion/1x2_a1",
    "minecraft:woodland_mansion/1x2_a2",
    "minecraft:woodland_mansion/1x2_a3",
    "minecraft:woodland_mansion/1x2_a4",
    "minecraft:woodland_mansion/1x2_a5",
    "minecraft:woodland_mansion/1x2_a6",
    "minecraft:woodland_mansion/1x2_a7",
    "minecraft:woodland_mansion/1x2_a8",
    "minecraft:woodland_mansion/1x2_a9",
    "minecraft:woodland_mansion/1x2_b1",
    "minecraft:woodland_mansion/1x2_b2",
    "minecraft:woodland_mansion/1x2_b3",
    "minecraft:woodland_mansion/1x2_b4",
    "minecraft:woodland_mansion/1x2_b5",
    "minecraft:woodland_mansion/1x2_c1",
    "minecraft:woodland_mansion/1x2_c2",
    "minecraft:woodland_mansion/1x2_c3",
    "minecraft:woodland_mansion/1x2_c4",
    "minecraft:woodland_mansion/1x2_d1",
    "minecraft:woodland_mansion/1x2_d2",
    "minecraft:woodland_mansion/1x2_d3",
    "minecraft:woodland_mansion/1x2_d4",
    "minecraft:woodland_mansion/1x2_d5",
    "minecraft:woodland_mansion/1x2_s1",
    "minecraft:woodland_mansion/1x2_s2",
    "minecraft:woodland_mansion/1x2_se1",
    "minecraft:woodland_mansion/1x2_c_stairs",
    "minecraft:woodland_mansion/1x2_d_stairs",
    "minecraft:woodland_mansion/2x2_a1",
    "minecraft:woodland_mansion/2x2_a2",
    "minecraft:woodland_mansion/2x2_a3",
    "minecraft:woodland_mansion/2x2_a4",
    "minecraft:woodland_mansion/2x2_b1",
    "minecraft:woodland_mansion/2x2_b2",
    "minecraft:woodland_mansion/2x2_b3",
    "minecraft:woodland_mansion/2x2_b4",
    "minecraft:woodland_mansion/2x2_b5",
    "minecraft:woodland_mansion/2x2_s1",
];

/// Every template which must be loaded before this complete assembly starts.
#[must_use]
pub fn template_ids() -> Vec<&'static str> {
    TEMPLATE_IDS.to_vec()
}

/// The realised boundary of this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MansionCoverage {
    /// All bundled mansion templates are emitted.  Data-marker entity spawning
    /// is still handled by the server-side consumer.
    CompleteTemplates,
}

/// A generated piece list paired with the coverage an integration must expose.
#[derive(Debug, Clone)]
pub struct MansionAssembly {
    pieces: Vec<StructurePiece>,
    coverage: MansionCoverage,
}

impl MansionAssembly {
    /// The generated template pieces, ready for the normal placement stage.
    #[must_use]
    pub fn pieces(&self) -> &[StructurePiece] {
        &self.pieces
    }

    /// Which parts of a mansion this assembly actually emits.
    #[must_use]
    pub fn coverage(&self) -> MansionCoverage {
        self.coverage
    }

    /// Consumes this report and returns the template pieces.
    #[must_use]
    pub fn into_pieces(self) -> Vec<StructurePiece> {
        self.pieces
    }
}

/// A required bundled template was absent.  This is an error rather than an
/// empty piece list so missing data can never become a plausible empty mansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingTemplate {
    /// The namespaced template id that was not loaded.
    pub id: &'static str,
}

/// Builds a seeded complete template assembly at its already height-adjusted
/// origin.  `rotation` and `random` are owned by the structure start, following
/// the same stream that selected its generation point.
///
/// # Errors
///
/// Returns the first required template absent from `templates`; it never drops a
/// placement and continues with a visually plausible incomplete building.
pub fn generate<R: RandomSource>(
    origin: [i32; 3],
    rotation: Rotation,
    templates: &TemplateStore,
    random: &mut R,
) -> Result<MansionAssembly, MissingTemplate> {
    for &id in TEMPLATE_IDS {
        if templates.get(id).is_none() {
            return Err(MissingTemplate { id });
        }
    }
    let layout = Layout::new(random);
    let mut out = Vec::new();
    let entrance = offset(origin, rotation, -9, 0, 0);
    add(&mut out, templates, "entrance", entrance, rotation, Mirror::None)?;
    // The wall traversal starts from the structure origin shifted south, not
    // from the entrance template's westward placement position.
    let first = offset(origin, rotation, 0, 0, 16);

    let start_x = layout.entrance_x + 1;
    let start_y = layout.entrance_y + 1;
    let end_x = layout.entrance_x + 1;
    let end_y = layout.entrance_y;

    let mut first_floor = PlacementData { position: first, rotation, wall: "wall_flat" };
    traverse_outer_walls(&mut out, templates, &mut first_floor, &layout.base, Direction::South, start_x, start_y, end_x, end_y)?;
    let mut second_floor = PlacementData { position: offset(first, rotation, 0, 8, 0), rotation, wall: "wall_window" };
    traverse_outer_walls(&mut out, templates, &mut second_floor, &layout.base, Direction::South, start_x, start_y, end_x, end_y)?;

    let mut third_floor = PlacementData {
        position: offset(first_floor.position, first_floor.rotation, 0, 19, 0),
        rotation: first_floor.rotation,
        wall: "wall_window",
    };
    'third: for y in 0..11 {
        for x in (0..11).rev() {
            if is_house(layout.third.get(x, y)) {
                third_floor.position = offset(
                    third_floor.position,
                    rotation,
                    (x - start_x) * 8,
                    0,
                    8 + (y - start_y) * 8,
                );
                traverse_wall_piece(&mut out, templates, &mut third_floor)?;
                traverse_outer_walls(&mut out, templates, &mut third_floor, &layout.third, Direction::South, x, y, x, y)?;
                break 'third;
            }
        }
    }

    create_roof(&mut out, templates, offset(origin, rotation, 0, 16, 0), rotation, &layout.base, Some(&layout.third), start_x, start_y)?;
    create_roof(&mut out, templates, offset(origin, rotation, 0, 27, 0), rotation, &layout.third, None, start_x, start_y)?;
    create_rooms(&mut out, templates, origin, rotation, &layout, start_x, start_y, random)?;

    Ok(MansionAssembly { pieces: out, coverage: MansionCoverage::CompleteTemplates })
}

fn add(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    name: &str,
    position: [i32; 3],
    rotation: Rotation,
    mirror: Mirror,
) -> Result<[i32; 3], MissingTemplate> {
    let id = mansion_id(name);
    let template = templates.get(&id).ok_or(MissingTemplate { id: template_id(name) })?;
    out.push(template_piece("minecraft:wj", &id, template, position, settings(rotation, mirror)));
    Ok(position)
}

fn template_id(name: &str) -> &'static str {
    TEMPLATE_IDS
        .iter()
        .copied()
        .find(|id| id.strip_prefix("minecraft:woodland_mansion/") == Some(name))
        .unwrap_or("minecraft:woodland_mansion/unknown")
}

fn mansion_id(name: &str) -> String {
    format!("minecraft:woodland_mansion/{name}")
}

fn settings(rotation: Rotation, mirror: Mirror) -> PlaceSettings {
    PlaceSettings {
        rotation,
        mirror,
        pivot: [0, 0, 0],
        processors: vec![Processor::structure_block()],
        waterlogging: false,
    }
}

struct PlacementData<'a> {
    position: [i32; 3],
    rotation: Rotation,
    wall: &'a str,
}

fn traverse_outer_walls(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    data: &mut PlacementData<'_>,
    grid: &Grid,
    mut direction: Direction,
    start_x: i32,
    start_y: i32,
    end_x: i32,
    end_y: i32,
) -> Result<(), MissingTemplate> {
    let mut x = start_x;
    let mut y = start_y;
    let start_direction = direction;
    loop {
        let next_x = x + direction.x();
        let next_y = y + direction.z();
        if !is_house(grid.get(next_x, next_y)) {
            traverse_turn(out, templates, data)?;
            direction = direction.cw();
            if x != end_x || y != end_y || start_direction != direction {
                traverse_wall_piece(out, templates, data)?;
            }
        } else if is_house(grid.get(next_x, next_y))
            && is_house(grid.get(
                next_x + direction.ccw().x(),
                next_y + direction.ccw().z(),
            ))
        {
            traverse_inner_turn(data);
            x = next_x;
            y = next_y;
            direction = direction.ccw();
        } else {
            x = next_x;
            y = next_y;
            if x != end_x || y != end_y || start_direction != direction {
                traverse_wall_piece(out, templates, data)?;
            }
        }
        if x == end_x && y == end_y && start_direction == direction {
            break;
        }
    }
    Ok(())
}

fn traverse_wall_piece(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    data: &mut PlacementData<'_>,
) -> Result<(), MissingTemplate> {
    add(
        out,
        templates,
        data.wall,
        offset(data.position, data.rotation, 7, 0, 0),
        data.rotation,
        Mirror::None,
    )?;
    data.position = offset(data.position, data.rotation, 0, 0, 8);
    Ok(())
}

fn traverse_turn(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    data: &mut PlacementData<'_>,
) -> Result<(), MissingTemplate> {
    data.position = offset(data.position, data.rotation, 0, 0, -1);
    add(out, templates, "wall_corner", data.position, data.rotation, Mirror::None)?;
    data.position = offset(data.position, data.rotation, 0, 0, -7);
    data.position = offset(data.position, data.rotation, 6, 0, 0);
    data.rotation = rotation_add(data.rotation, Rotation::Cw90);
    Ok(())
}

fn traverse_inner_turn(data: &mut PlacementData<'_>) {
    data.position = offset(data.position, data.rotation, 8, 0, 6);
    data.rotation = rotation_add(data.rotation, Rotation::Ccw90);
}

fn offset(origin: [i32; 3], rotation: Rotation, east: i32, up: i32, south: i32) -> [i32; 3] {
    let (ex, ez) = horizontal(rotation, 1, 0);
    let (sx, sz) = horizontal(rotation, 0, 1);
    [origin[0] + east * ex + south * sx, origin[1] + up, origin[2] + east * ez + south * sz]
}

/// Applies a horizontal turn to a local east/south vector.
fn horizontal(rotation: Rotation, east: i32, south: i32) -> (i32, i32) {
    match rotation {
        Rotation::None => (east, south),
        Rotation::Cw90 => (-south, east),
        Rotation::Cw180 => (-east, -south),
        Rotation::Ccw90 => (south, -east),
    }
}

fn create_roof(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    origin: [i32; 3],
    rotation: Rotation,
    grid: &Grid,
    above: Option<&Grid>,
    start_x: i32,
    start_y: i32,
) -> Result<(), MissingTemplate> {
    for y in 0..11 {
        for x in 0..11 {
            let position = cell_position(origin, rotation, x, y, start_x, start_y);
            let is_above = above.is_some_and(|upper| is_house(upper.get(x, y)));
            if is_house(grid.get(x, y)) && !is_above {
                add(out, templates, "roof", offset(position, rotation, 0, 3, 0), rotation, Mirror::None)?;
                if !is_house(grid.get(x + 1, y)) {
                    add(out, templates, "roof_front", offset(position, rotation, 6, 0, 0), rotation, Mirror::None)?;
                }
                if !is_house(grid.get(x - 1, y)) {
                    add(out, templates, "roof_front", offset(position, rotation, 0, 0, 7), rotation_add(rotation, Rotation::Cw180), Mirror::None)?;
                }
                if !is_house(grid.get(x, y - 1)) {
                    add(out, templates, "roof_front", offset(position, rotation, -1, 0, 0), rotation_add(rotation, Rotation::Ccw90), Mirror::None)?;
                }
                if !is_house(grid.get(x, y + 1)) {
                    add(out, templates, "roof_front", offset(position, rotation, 6, 0, 6), rotation_add(rotation, Rotation::Cw90), Mirror::None)?;
                }
            }
        }
    }

    if above.is_some() {
        for y in 0..11 {
            for x in 0..11 {
                let position = cell_position(origin, rotation, x, y, start_x, start_y);
                if is_house(grid.get(x, y)) && above.is_some_and(|upper| is_house(upper.get(x, y))) {
                    if !is_house(grid.get(x + 1, y)) {
                        add(out, templates, "small_wall", offset(position, rotation, 7, 0, 0), rotation, Mirror::None)?;
                    }
                    if !is_house(grid.get(x - 1, y)) {
                        add(out, templates, "small_wall", offset(position, rotation, -1, 0, 6), rotation_add(rotation, Rotation::Cw180), Mirror::None)?;
                    }
                    if !is_house(grid.get(x, y - 1)) {
                        add(out, templates, "small_wall", offset(position, rotation, 0, 0, -1), rotation_add(rotation, Rotation::Ccw90), Mirror::None)?;
                    }
                    if !is_house(grid.get(x, y + 1)) {
                        add(out, templates, "small_wall", offset(position, rotation, 6, 0, 7), rotation_add(rotation, Rotation::Cw90), Mirror::None)?;
                    }
                    if !is_house(grid.get(x + 1, y)) {
                        if !is_house(grid.get(x, y - 1)) {
                            add(out, templates, "small_wall_corner", offset(position, rotation, 7, 0, -2), rotation, Mirror::None)?;
                        }
                        if !is_house(grid.get(x, y + 1)) {
                            add(out, templates, "small_wall_corner", offset(position, rotation, 8, 0, 7), rotation_add(rotation, Rotation::Cw90), Mirror::None)?;
                        }
                    }
                    if !is_house(grid.get(x - 1, y)) {
                        if !is_house(grid.get(x, y - 1)) {
                            add(out, templates, "small_wall_corner", offset(position, rotation, -2, 0, -1), rotation_add(rotation, Rotation::Ccw90), Mirror::None)?;
                        }
                        if !is_house(grid.get(x, y + 1)) {
                            add(out, templates, "small_wall_corner", offset(position, rotation, -1, 0, 8), rotation_add(rotation, Rotation::Cw180), Mirror::None)?;
                        }
                    }
                }
            }
        }
    }

    for y in 0..11 {
        for x in 0..11 {
            let position = cell_position(origin, rotation, x, y, start_x, start_y);
            let is_above = above.is_some_and(|upper| is_house(upper.get(x, y)));
            if is_house(grid.get(x, y)) && !is_above {
                if !is_house(grid.get(x + 1, y)) {
                    let edge = offset(position, rotation, 6, 0, 0);
                    if !is_house(grid.get(x, y + 1)) {
                        add(out, templates, "roof_corner", offset(edge, rotation, 0, 0, 6), rotation, Mirror::None)?;
                    } else if is_house(grid.get(x + 1, y + 1)) {
                        add(out, templates, "roof_inner_corner", offset(edge, rotation, 0, 0, 5), rotation, Mirror::None)?;
                    }
                    if !is_house(grid.get(x, y - 1)) {
                        add(out, templates, "roof_corner", edge, rotation_add(rotation, Rotation::Ccw90), Mirror::None)?;
                    } else if is_house(grid.get(x + 1, y - 1)) {
                        add(out, templates, "roof_inner_corner", offset(position, rotation, 9, 0, -2), rotation_add(rotation, Rotation::Cw90), Mirror::None)?;
                    }
                }
                if !is_house(grid.get(x - 1, y)) {
                    let edge = position;
                    if !is_house(grid.get(x, y + 1)) {
                        add(out, templates, "roof_corner", offset(edge, rotation, 0, 0, 6), rotation_add(rotation, Rotation::Cw90), Mirror::None)?;
                    } else if is_house(grid.get(x - 1, y + 1)) {
                        add(out, templates, "roof_inner_corner", offset(edge, rotation, -3, 0, 8), rotation_add(rotation, Rotation::Ccw90), Mirror::None)?;
                    }
                    if !is_house(grid.get(x, y - 1)) {
                        add(out, templates, "roof_corner", edge, rotation_add(rotation, Rotation::Cw180), Mirror::None)?;
                    } else if is_house(grid.get(x - 1, y - 1)) {
                        add(out, templates, "roof_inner_corner", offset(edge, rotation, 0, 0, 1), rotation_add(rotation, Rotation::Cw180), Mirror::None)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn create_rooms<R: RandomSource>(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    origin: [i32; 3],
    rotation: Rotation,
    layout: &Layout,
    start_x: i32,
    start_y: i32,
    random: &mut R,
) -> Result<(), MissingTemplate> {
    for floor in 0..3 {
        let floor_origin = offset(origin, rotation, 0, 8 * floor + if floor == 2 { 3 } else { 0 }, 0);
        let grid = if floor == 2 { &layout.third } else { &layout.base };
        let rooms = &layout.rooms[floor as usize];
        let south_carpet = if floor == 0 { "carpet_south_1" } else { "carpet_south_2" };
        let west_carpet = if floor == 0 { "carpet_west_1" } else { "carpet_west_2" };

        for y in 0..11 {
            for x in 0..11 {
                if grid.get(x, y) != CORRIDOR {
                    continue;
                }
                let position = cell_position(floor_origin, rotation, x, y, start_x, start_y);
                add(out, templates, "corridor_floor", position, rotation, Mirror::None)?;
                if grid.get(x, y - 1) == CORRIDOR || rooms.get(x, y - 1) & ROOM_CORRIDOR != 0 {
                    add(out, templates, "carpet_north", offset(position, rotation, 1, 1, 0), rotation, Mirror::None)?;
                }
                if grid.get(x + 1, y) == CORRIDOR || rooms.get(x + 1, y) & ROOM_CORRIDOR != 0 {
                    add(out, templates, "carpet_east", offset(position, rotation, 5, 1, 1), rotation, Mirror::None)?;
                }
                if grid.get(x, y + 1) == CORRIDOR || rooms.get(x, y + 1) & ROOM_CORRIDOR != 0 {
                    add(out, templates, south_carpet, offset(position, rotation, -1, 0, 5), rotation, Mirror::None)?;
                }
                if grid.get(x - 1, y) == CORRIDOR || rooms.get(x - 1, y) & ROOM_CORRIDOR != 0 {
                    add(out, templates, west_carpet, offset(position, rotation, -1, 0, -1), rotation, Mirror::None)?;
                }
            }
        }

        let wall = if floor == 0 { "indoors_wall_1" } else { "indoors_wall_2" };
        let door = if floor == 0 { "indoors_door_1" } else { "indoors_door_2" };
        for y in 0..11 {
            for x in 0..11 {
                let mut third_start = floor == 2 && grid.get(x, y) == START_ROOM;
                if grid.get(x, y) != ROOM && !third_start {
                    continue;
                }
                let room_data = rooms.get(x, y);
                let room_type = room_data & ROOM_TYPE_MASK;
                let room_id = room_data & ROOM_ID_MASK;
                third_start = third_start && room_data & ROOM_CORRIDOR != 0;
                let mut door_dirs = Vec::new();
                if room_data & ROOM_DOOR != 0 {
                    for direction in Direction::horizontal() {
                        if grid.get(x + direction.x(), y + direction.z()) == CORRIDOR {
                            door_dirs.push(direction);
                        }
                    }
                }
                let door_dir = if door_dirs.is_empty() {
                    if room_data & ROOM_ORIGIN != 0 { Some(Direction::Up) } else { None }
                } else {
                    Some(door_dirs[random.next_int_bounded(door_dirs.len() as i32).max(0) as usize])
                };
                let room_pos = offset(
                    floor_origin,
                    rotation,
                    -1 + (x - start_x) * 8,
                    0,
                    8 + (y - start_y) * 8,
                );

                if is_house(grid.get(x - 1, y)) && !layout.is_room_id(grid, x - 1, y, floor as usize, room_id) {
                    add(out, templates, if door_dir == Some(Direction::West) { door } else { wall }, room_pos, rotation, Mirror::None)?;
                }
                if grid.get(x + 1, y) == CORRIDOR && !third_start {
                    add(out, templates, if door_dir == Some(Direction::East) { door } else { wall }, offset(room_pos, rotation, 8, 0, 0), rotation, Mirror::None)?;
                }
                if is_house(grid.get(x, y + 1)) && !layout.is_room_id(grid, x, y + 1, floor as usize, room_id) {
                    add(out, templates, if door_dir == Some(Direction::South) { door } else { wall }, offset(room_pos, rotation, 7, 0, 7), rotation_add(rotation, Rotation::Cw90), Mirror::None)?;
                }
                if grid.get(x, y - 1) == CORRIDOR && !third_start {
                    add(out, templates, if door_dir == Some(Direction::North) { door } else { wall }, offset(room_pos, rotation, 7, 0, 1), rotation_add(rotation, Rotation::Cw90), Mirror::None)?;
                }

                match room_type {
                    ROOM_1X1 => add_room_1x1(out, templates, room_pos, rotation, door_dir, floor, random)?,
                    ROOM_1X2 => {
                        if let Some(door_dir) = door_dir {
                            let room_dir = layout.room_1x2_direction(grid, x, y, floor as usize, room_id);
                            let stairs = room_data & ROOM_STAIRS != 0;
                            if let Some(room_dir) = room_dir {
                                add_room_1x2(out, templates, room_pos, rotation, room_dir, door_dir, floor, stairs, random)?;
                            }
                        }
                    }
                    ROOM_2X2 => {
                        if let Some(door_dir) = door_dir {
                            if door_dir == Direction::Up {
                                add_room_2x2_secret(out, templates, room_pos, rotation, floor)?;
                            } else {
                                let mut room_dir = door_dir.cw();
                                if !layout.is_room_id(grid, x + room_dir.x(), y + room_dir.z(), floor as usize, room_id) {
                                    room_dir = room_dir.opposite();
                                }
                                add_room_2x2(out, templates, room_pos, rotation, room_dir, door_dir, floor, random)?;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn room_1x1_name<R: RandomSource>(floor: i32, random: &mut R) -> String {
    let prefix = if floor == 0 { "1x1_a" } else { "1x1_b" };
    format!("{prefix}{}", random.next_int_bounded(5) + 1)
}

fn room_1x1_secret_name<R: RandomSource>(random: &mut R) -> String {
    format!("1x1_as{}", random.next_int_bounded(4) + 1)
}

fn room_1x2_side_name<R: RandomSource>(floor: i32, stairs: bool, random: &mut R) -> String {
    if floor == 0 {
        format!("1x2_a{}", random.next_int_bounded(9) + 1)
    } else if stairs {
        "1x2_c_stairs".to_string()
    } else {
        format!("1x2_c{}", random.next_int_bounded(4) + 1)
    }
}

fn room_1x2_front_name<R: RandomSource>(floor: i32, stairs: bool, random: &mut R) -> String {
    if floor == 0 {
        format!("1x2_b{}", random.next_int_bounded(5) + 1)
    } else if stairs {
        "1x2_d_stairs".to_string()
    } else {
        format!("1x2_d{}", random.next_int_bounded(5) + 1)
    }
}

fn room_1x2_secret_name<R: RandomSource>(floor: i32, random: &mut R) -> String {
    if floor == 0 {
        format!("1x2_s{}", random.next_int_bounded(2) + 1)
    } else {
        // The one-choice bound is intentional: it still consumes one RNG draw.
        format!("1x2_se{}", random.next_int_bounded(1) + 1)
    }
}

fn room_2x2_name<R: RandomSource>(floor: i32, random: &mut R) -> String {
    if floor == 0 {
        format!("2x2_a{}", random.next_int_bounded(4) + 1)
    } else {
        format!("2x2_b{}", random.next_int_bounded(5) + 1)
    }
}

fn zero_position_with_transform(
    zero: [i32; 3],
    mirror: Mirror,
    rotation: Rotation,
    size_x: i32,
    size_z: i32,
) -> [i32; 3] {
    let sx = size_x - 1;
    let sz = size_z - 1;
    let mdx = if mirror == Mirror::FrontBack { sx } else { 0 };
    let mdz = if mirror == Mirror::LeftRight { sz } else { 0 };
    let relative = match rotation {
        Rotation::Ccw90 => [mdz, 0, sx - mdx],
        Rotation::Cw90 => [sz - mdz, 0, mdx],
        Rotation::Cw180 => [sx - mdx, 0, sz - mdz],
        Rotation::None => [mdx, 0, mdz],
    };
    [zero[0] + relative[0], zero[1], zero[2] + relative[2]]
}

fn add_room_1x1<R: RandomSource>(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    room_pos: [i32; 3],
    rotation: Rotation,
    door_dir: Option<Direction>,
    floor: i32,
    random: &mut R,
) -> Result<(), MissingTemplate> {
    let mut piece_rotation = Rotation::None;
    let normal_name = room_1x1_name(floor, random);
    let name = match door_dir {
        Some(Direction::East) => normal_name.clone(),
        Some(Direction::North) => {
            piece_rotation = Rotation::Ccw90;
            normal_name.clone()
        }
        Some(Direction::West) => {
            piece_rotation = Rotation::Cw180;
            normal_name.clone()
        }
        Some(Direction::South) => {
            piece_rotation = Rotation::Cw90;
            normal_name
        }
        Some(Direction::Up) | None => room_1x1_secret_name(random),
    };
    let orientation = zero_position_with_transform([1, 0, 0], Mirror::None, piece_rotation, 7, 7);
    let position = [room_pos[0] + orientation[0], room_pos[1], room_pos[2] + orientation[2]];
    add(
        out,
        templates,
        &name,
        position,
        rotation_add(piece_rotation, rotation),
        Mirror::None,
    )?;
    Ok(())
}

fn add_room_1x2<R: RandomSource>(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    room_pos: [i32; 3],
    rotation: Rotation,
    room_dir: Direction,
    door_dir: Direction,
    floor: i32,
    stairs: bool,
    random: &mut R,
) -> Result<(), MissingTemplate> {
    let (east, south, piece_rotation, mirror, front) = match (door_dir, room_dir) {
        (Direction::East, Direction::South) => (1, 0, rotation, Mirror::None, false),
        (Direction::East, Direction::North) => (1, 6, rotation, Mirror::LeftRight, false),
        (Direction::West, Direction::North) => (7, 6, rotation_add(rotation, Rotation::Cw180), Mirror::None, false),
        (Direction::West, Direction::South) => (7, 0, rotation, Mirror::FrontBack, false),
        (Direction::South, Direction::East) => (1, 0, rotation_add(rotation, Rotation::Cw90), Mirror::LeftRight, false),
        (Direction::South, Direction::West) => (7, 0, rotation_add(rotation, Rotation::Cw90), Mirror::None, false),
        (Direction::North, Direction::West) => (7, 6, rotation_add(rotation, Rotation::Cw90), Mirror::FrontBack, false),
        (Direction::North, Direction::East) => (1, 6, rotation_add(rotation, Rotation::Ccw90), Mirror::None, false),
        (Direction::South, Direction::North) => (1, -8, rotation, Mirror::None, true),
        (Direction::North, Direction::South) => (7, 14, rotation_add(rotation, Rotation::Cw180), Mirror::None, true),
        (Direction::West, Direction::East) => (15, 0, rotation_add(rotation, Rotation::Cw90), Mirror::None, true),
        (Direction::East, Direction::West) => (-7, 6, rotation_add(rotation, Rotation::Ccw90), Mirror::None, true),
        (Direction::Up, Direction::East) => (15, 0, rotation_add(rotation, Rotation::Cw90), Mirror::None, false),
        (Direction::Up, Direction::South) => (1, 0, rotation, Mirror::None, false),
        _ => return Ok(()),
    };
    let name = if door_dir == Direction::Up {
        room_1x2_secret_name(floor, random)
    } else if front {
        room_1x2_front_name(floor, stairs, random)
    } else {
        room_1x2_side_name(floor, stairs, random)
    };
    let position = offset(room_pos, rotation, east, 0, south);
    add(out, templates, &name, position, piece_rotation, mirror)?;
    Ok(())
}

fn add_room_2x2<R: RandomSource>(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    room_pos: [i32; 3],
    rotation: Rotation,
    room_dir: Direction,
    door_dir: Direction,
    floor: i32,
    random: &mut R,
) -> Result<(), MissingTemplate> {
    let (east, south, piece_rotation, mirror) = match (door_dir, room_dir) {
        (Direction::East, Direction::South) => (-7, 0, rotation, Mirror::None),
        (Direction::East, Direction::North) => (-7, 6, rotation, Mirror::LeftRight),
        (Direction::North, Direction::East) => (1, 14, rotation_add(rotation, Rotation::Ccw90), Mirror::None),
        (Direction::North, Direction::West) => (7, 14, rotation_add(rotation, Rotation::Ccw90), Mirror::LeftRight),
        (Direction::South, Direction::West) => (7, -8, rotation_add(rotation, Rotation::Cw90), Mirror::None),
        (Direction::South, Direction::East) => (1, -8, rotation_add(rotation, Rotation::Cw90), Mirror::LeftRight),
        (Direction::West, Direction::North) => (15, 6, rotation_add(rotation, Rotation::Cw180), Mirror::None),
        (Direction::West, Direction::South) => (15, 0, rotation, Mirror::FrontBack),
        _ => return Ok(()),
    };
    let position = offset(room_pos, rotation, east, 0, south);
    let name = room_2x2_name(floor, random);
    add(out, templates, &name, position, piece_rotation, mirror)?;
    Ok(())
}

fn add_room_2x2_secret(
    out: &mut Vec<StructurePiece>,
    templates: &TemplateStore,
    room_pos: [i32; 3],
    rotation: Rotation,
    _floor: i32,
) -> Result<(), MissingTemplate> {
    let position = offset(room_pos, rotation, 1, 0, 0);
    add(out, templates, "2x2_s1", position, rotation, Mirror::None)?;
    Ok(())
}

fn cell_position(
    origin: [i32; 3],
    rotation: Rotation,
    x: i32,
    z: i32,
    start_x: i32,
    start_y: i32,
) -> [i32; 3] {
    offset(origin, rotation, (x - start_x) * 8, 0, 8 + (z - start_y) * 8)
}

fn rotation_add(a: Rotation, b: Rotation) -> Rotation {
    match (a.turns() + b.turns()) % 4 {
        1 => Rotation::Cw90,
        2 => Rotation::Cw180,
        3 => Rotation::Ccw90,
        _ => Rotation::None,
    }
}

fn is_house(value: i32) -> bool {
    matches!(value, CORRIDOR | ROOM | START_ROOM | 4)
}

#[derive(Clone)]
struct Grid {
    cells: [[i32; 11]; 11],
}

impl Grid {
    fn new(outside: i32) -> Self {
        let grid = Self { cells: [[CLEAR; 11]; 11] };
        // The caller's outside value is encoded only through `get`; storing a
        // blocked border would make edge cleanup incorrectly see it as a cell.
        let _ = outside;
        grid
    }

    fn get(&self, x: i32, y: i32) -> i32 {
        if (0..11).contains(&x) && (0..11).contains(&y) { self.cells[x as usize][y as usize] } else { BLOCKED }
    }

    fn set(&mut self, x: i32, y: i32, value: i32) {
        if (0..11).contains(&x) && (0..11).contains(&y) { self.cells[x as usize][y as usize] = value; }
    }

    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, value: i32) {
        for y in y0..=y1 { for x in x0..=x1 { self.set(x, y, value); } }
    }

    fn set_if(&mut self, x: i32, y: i32, old: i32, value: i32) {
        if self.get(x, y) == old { self.set(x, y, value); }
    }

    fn edges_to(&self, x: i32, y: i32, value: i32) -> bool {
        Direction::horizontal()
            .into_iter()
            .any(|direction| self.get(x + direction.x(), y + direction.z()) == value)
    }
}

struct Layout {
    base: Grid,
    third: Grid,
    rooms: [Grid; 3],
    entrance_x: i32,
    entrance_y: i32,
}

impl Layout {
    fn new<R: RandomSource>(random: &mut R) -> Self {
        let mut base = Grid::new(BLOCKED);
        let (entrance_x, entrance_y) = (7, 4);
        base.fill(entrance_x, entrance_y, entrance_x + 1, entrance_y + 1, START_ROOM);
        base.fill(entrance_x - 1, entrance_y, entrance_x - 1, entrance_y + 1, ROOM);
        base.fill(entrance_x + 2, entrance_y - 2, entrance_x + 3, entrance_y + 3, BLOCKED);
        base.fill(entrance_x + 1, entrance_y - 2, entrance_x + 1, entrance_y - 1, CORRIDOR);
        base.fill(entrance_x + 1, entrance_y + 2, entrance_x + 1, entrance_y + 3, CORRIDOR);
        base.set(entrance_x - 1, entrance_y - 1, CORRIDOR);
        base.set(entrance_x - 1, entrance_y + 2, CORRIDOR);
        base.fill(0, 0, 10, 0, BLOCKED);
        base.fill(0, 9, 10, 10, BLOCKED);
        for (x, y, heading, depth) in [
            (entrance_x, entrance_y - 2, Direction::West, 6),
            (entrance_x, entrance_y + 3, Direction::West, 6),
            (entrance_x - 2, entrance_y - 1, Direction::West, 3),
            (entrance_x - 2, entrance_y + 2, Direction::West, 3),
        ] { carve(&mut base, x, y, heading, depth, random); }
        while clean_edges(&mut base) {}

        let mut rooms = [Grid::new(BLOCKED), Grid::new(BLOCKED), Grid::new(BLOCKED)];
        identify_rooms(&base, &mut rooms[0], random);
        identify_rooms(&base, &mut rooms[1], random);
        rooms[0].set(entrance_x + 1, entrance_y, ROOM_CORRIDOR);
        rooms[0].set(entrance_x + 1, entrance_y + 1, ROOM_CORRIDOR);
        rooms[1].set(entrance_x + 1, entrance_y, ROOM_CORRIDOR);
        rooms[1].set(entrance_x + 1, entrance_y + 1, ROOM_CORRIDOR);

        let mut third = Grid::new(BLOCKED);
        let candidates: Vec<(i32, i32)> = (0..11)
            .flat_map(|y| (0..11).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let room_data = rooms[1].get(x, y);
                room_data & ROOM_TYPE_MASK == ROOM_1X2 && room_data & ROOM_DOOR != 0
            })
            .collect();
        if candidates.is_empty() {
            third.fill(0, 0, 10, 10, BLOCKED);
        } else {
            let candidate = candidates[random.next_int_bounded(candidates.len() as i32).max(0) as usize];
            let room_data = rooms[1].get(candidate.0, candidate.1);
            rooms[1].set(candidate.0, candidate.1, room_data | ROOM_STAIRS);
            if let Some(room_direction) = room_1x2_direction(&rooms[1], candidate.0, candidate.1, room_data & ROOM_ID_MASK) {
                let end_x = candidate.0 + room_direction.x();
                let end_y = candidate.1 + room_direction.z();
                for y in 0..11 {
                    for x in 0..11 {
                        if !is_house(base.get(x, y)) {
                            third.set(x, y, BLOCKED);
                        } else if x == candidate.0 && y == candidate.1 {
                            third.set(x, y, START_ROOM);
                        } else if x == end_x && y == end_y {
                            third.set(x, y, START_ROOM);
                            rooms[2].set(x, y, ROOM_CORRIDOR);
                        }
                    }
                }
                let directions: Vec<Direction> = Direction::horizontal()
                    .into_iter()
                    .filter(|direction| third.get(end_x + direction.x(), end_y + direction.z()) == CLEAR)
                    .collect();
                if directions.is_empty() {
                    third.fill(0, 0, 10, 10, BLOCKED);
                    rooms[1].set(candidate.0, candidate.1, room_data);
                } else {
                    let direction = directions[random.next_int_bounded(directions.len() as i32).max(0) as usize];
                    carve(&mut third, end_x + direction.x(), end_y + direction.z(), direction, 4, random);
                    while clean_edges(&mut third) {}
                }
            } else {
                third.fill(0, 0, 10, 10, BLOCKED);
                rooms[1].set(candidate.0, candidate.1, room_data);
            }
        }
        identify_rooms(&third, &mut rooms[2], random);
        Self {
            base,
            third,
            rooms,
            entrance_x,
            entrance_y,
        }
    }

    fn is_room_id(&self, _grid: &Grid, x: i32, y: i32, floor: usize, room_id: i32) -> bool {
        self.rooms[floor].get(x, y) & ROOM_ID_MASK == room_id
    }

    fn room_1x2_direction(
        &self,
        _grid: &Grid,
        x: i32,
        y: i32,
        floor: usize,
        room_id: i32,
    ) -> Option<Direction> {
        room_1x2_direction(&self.rooms[floor], x, y, room_id)
    }
}

fn identify_rooms<R: RandomSource>(from: &Grid, rooms: &mut Grid, random: &mut R) {
    let mut positions: Vec<(i32, i32)> = (0..11)
        .flat_map(|y| (0..11).map(move |x| (x, y)))
        .filter(|&(x, y)| from.get(x, y) == ROOM)
        .collect();
    super::pool::shuffle(&mut positions, random);
    let mut room_id = 10;
    for (x, y) in positions {
        if rooms.get(x, y) != CLEAR {
            continue;
        }
        let mut x0 = x;
        let mut x1 = x;
        let mut y0 = y;
        let mut y1 = y;
        let mut room_type = ROOM_1X1;
        if rooms.get(x + 1, y) == CLEAR
            && rooms.get(x, y + 1) == CLEAR
            && rooms.get(x + 1, y + 1) == CLEAR
            && from.get(x + 1, y) == ROOM
            && from.get(x, y + 1) == ROOM
            && from.get(x + 1, y + 1) == ROOM
        {
            x1 += 1;
            y1 += 1;
            room_type = ROOM_2X2;
        } else if rooms.get(x - 1, y) == CLEAR
            && rooms.get(x, y + 1) == CLEAR
            && rooms.get(x - 1, y + 1) == CLEAR
            && from.get(x - 1, y) == ROOM
            && from.get(x, y + 1) == ROOM
            && from.get(x - 1, y + 1) == ROOM
        {
            x0 -= 1;
            y1 += 1;
            room_type = ROOM_2X2;
        } else if rooms.get(x - 1, y) == CLEAR
            && rooms.get(x, y - 1) == CLEAR
            && rooms.get(x - 1, y - 1) == CLEAR
            && from.get(x - 1, y) == ROOM
            && from.get(x, y - 1) == ROOM
            && from.get(x - 1, y - 1) == ROOM
        {
            x0 -= 1;
            y0 -= 1;
            room_type = ROOM_2X2;
        } else if rooms.get(x + 1, y) == CLEAR && from.get(x + 1, y) == ROOM {
            x1 += 1;
            room_type = ROOM_1X2;
        } else if rooms.get(x, y + 1) == CLEAR && from.get(x, y + 1) == ROOM {
            y1 += 1;
            room_type = ROOM_1X2;
        } else if rooms.get(x - 1, y) == CLEAR && from.get(x - 1, y) == ROOM {
            x0 -= 1;
            room_type = ROOM_1X2;
        } else if rooms.get(x, y - 1) == CLEAR && from.get(x, y - 1) == ROOM {
            y0 -= 1;
            room_type = ROOM_1X2;
        }

        let mut door_x = if random.next_bool() { x0 } else { x1 };
        let mut door_y = if random.next_bool() { y0 } else { y1 };
        let mut door_flag = ROOM_DOOR;
        if !from.edges_to(door_x, door_y, CORRIDOR) {
            door_x = if door_x == x0 { x1 } else { x0 };
            door_y = if door_y == y0 { y1 } else { y0 };
            if !from.edges_to(door_x, door_y, CORRIDOR) {
                door_y = if door_y == y0 { y1 } else { y0 };
                if !from.edges_to(door_x, door_y, CORRIDOR) {
                    door_x = if door_x == x0 { x1 } else { x0 };
                    door_y = if door_y == y0 { y1 } else { y0 };
                    if !from.edges_to(door_x, door_y, CORRIDOR) {
                        door_flag = 0;
                        door_x = x0;
                        door_y = y0;
                    }
                }
            }
        }

        for ry in y0..=y1 {
            for rx in x0..=x1 {
                let flags = if rx == door_x && ry == door_y {
                    ROOM_ORIGIN | door_flag
                } else {
                    0
                };
                rooms.set(rx, ry, flags | room_type | room_id);
            }
        }
        room_id += 1;
    }
}

fn room_1x2_direction(rooms: &Grid, x: i32, y: i32, room_id: i32) -> Option<Direction> {
    Direction::horizontal()
        .into_iter()
        .find(|&direction| rooms.get(x + direction.x(), y + direction.z()) & ROOM_ID_MASK == room_id)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction { North, East, South, West, Up }

impl Direction {
    fn x(self) -> i32 { match self { Self::East => 1, Self::West => -1, _ => 0 } }
    fn z(self) -> i32 { match self { Self::South => 1, Self::North => -1, _ => 0 } }
    fn opposite(self) -> Self { match self { Self::North => Self::South, Self::East => Self::West, Self::South => Self::North, Self::West => Self::East, Self::Up => Self::Up } }
    fn cw(self) -> Self { match self { Self::North => Self::East, Self::East => Self::South, Self::South => Self::West, Self::West => Self::North, Self::Up => Self::Up } }
    fn ccw(self) -> Self { self.cw().opposite() }
    fn horizontal() -> [Self; 4] { [Self::North, Self::East, Self::South, Self::West] }
    fn random<R: RandomSource>(random: &mut R) -> Self { match random.next_int_bounded(4) { 0 => Self::South, 1 => Self::West, 2 => Self::North, _ => Self::East } }
}

fn carve<R: RandomSource>(grid: &mut Grid, x: i32, y: i32, heading: Direction, depth: i32, random: &mut R) {
    if depth <= 0 { return; }
    grid.set(x, y, CORRIDOR);
    grid.set_if(x + heading.x(), y + heading.z(), CLEAR, CORRIDOR);
    for _ in 0..8 {
        let next = Direction::random(random);
        if next != heading.opposite() && (next != Direction::East || !random.next_bool()) {
            let nx = x + heading.x(); let ny = y + heading.z();
            if grid.get(nx + next.x(), ny + next.z()) == CLEAR && grid.get(nx + 2 * next.x(), ny + 2 * next.z()) == CLEAR {
                carve(grid, nx + next.x(), ny + next.z(), next, depth - 1, random);
                break;
            }
        }
    }
    for direction in [heading.cw(), heading.ccw()] {
        grid.set_if(x + direction.x(), y + direction.z(), CLEAR, ROOM);
        grid.set_if(x + heading.x() + direction.x(), y + heading.z() + direction.z(), CLEAR, ROOM);
        grid.set_if(x + 2 * direction.x(), y + 2 * direction.z(), CLEAR, ROOM);
    }
    grid.set_if(x + 2 * heading.x(), y + 2 * heading.z(), CLEAR, ROOM);
}

fn clean_edges(grid: &mut Grid) -> bool {
    let mut touched = false;
    for y in 0..11 { for x in 0..11 {
        if grid.get(x, y) != CLEAR { continue; }
        let adjacent = [Direction::North, Direction::East, Direction::South, Direction::West]
            .into_iter().filter(|d| is_house(grid.get(x + d.x(), y + d.z()))).count();
        if adjacent >= 3 {
            grid.set(x, y, ROOM); touched = true;
        } else if adjacent == 2 {
            let diagonals = [(-1, -1), (1, -1), (-1, 1), (1, 1)]
                .into_iter().filter(|&(dx, dy)| is_house(grid.get(x + dx, y + dy))).count();
            if diagonals <= 1 { grid.set(x, y, ROOM); touched = true; }
        }
    }}
    touched
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::dense_grid::DenseBlockGrid;
    use super::super::template::{PlaceOrigin, StructureTemplate};
    use lodestone_worldgen_core::rng::LegacyRandomSource;

    fn bundled_templates() -> TemplateStore {
        let mut store = TemplateStore::default();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/structure");
        for &id in TEMPLATE_IDS {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let bytes = std::fs::read(root.join(format!("{name}.nbt")))
                .unwrap_or_else(|e| panic!("reading {name}: {e}"));
            let parsed = StructureTemplate::parse(&bytes)
                .unwrap_or_else(|e| panic!("decoding {name}: {e}"));
            store.templates.insert(id.to_string(), std::sync::Arc::new(parsed));
        }
        store
    }

    #[test]
    fn seeded_layout_has_a_real_top_floor_or_an_explicitly_blocked_one() {
        let mut random = LegacyRandomSource::new(91);
        let layout = Layout::new(&mut random);
        assert!(layout.base.cells.iter().flatten().any(|&v| v == CORRIDOR));
        assert!(layout.third.cells.iter().flatten().any(|&v| is_house(v)) || layout.third.cells.iter().flatten().all(|&v| v == BLOCKED));
    }

    #[test]
    fn complete_template_list_is_closed_over_all_emitted_names() {
        for id in TEMPLATE_IDS {
            assert!(id.starts_with("minecraft:woodland_mansion/"));
        }
    }

    #[test]
    fn bundled_complete_assembly_places_a_seeded_rotated_block_footprint() {
        let templates = bundled_templates();
        let mut random = LegacyRandomSource::new(91);
        let assembly = generate([32, 70, -48], Rotation::Cw90, &templates, &mut random)
            .expect("all complete templates are bundled");
        assert_eq!(assembly.coverage(), MansionCoverage::CompleteTemplates);
        // These counts and the first wall coordinates come from an independent
        // implementation of the reference 26.2 grid and placement traversal.
        assert_eq!(assembly.pieces().len(), 613, "seeded layout or piece order drifted");
        assert_eq!(assembly.pieces()[0].placement.as_ref().unwrap().position, [32, 70, -57]);
        assert_eq!(assembly.pieces()[1].template.as_deref(), Some("minecraft:woodland_mansion/wall_flat"));
        assert_eq!(assembly.pieces()[1].placement.as_ref().unwrap().position, [16, 70, -41]);
        assert_eq!(assembly.pieces()[4].template.as_deref(), Some("minecraft:woodland_mansion/wall_corner"));
        let names: Vec<_> = assembly.pieces().iter().filter_map(|piece| piece.template.as_deref()).collect();
        assert!(names.iter().any(|name| name.ends_with("/carpet_north")));
        assert!(names.iter().any(|name| name.ends_with("/indoors_door_1")));
        assert!(names.iter().any(|name| name.ends_with("/1x2_c_stairs")));
        assert!(names.iter().any(|name| name.ends_with("/2x2_b1")));

        let mut grid = DenseBlockGrid::new(-128, 0, -160, 256, 128, 256, "minecraft:air");
        let mut writes = 0usize;
        for piece in assembly.pieces() {
            let placement = piece.placement.as_ref().expect("every mansion piece is template-driven");
            writes += placement.template.place(
                PlaceOrigin { position: placement.position, reference: [32, 70, -48], seed: 91 },
                &placement.settings,
                &mut grid,
            );
        }
        assert_eq!(writes, 127_556, "template placement footprint drifted");
        assert_eq!(grid.get(32, 70, -48), "minecraft:birch_planks", "entrance footprint drifted");
        assert_eq!(grid.get(-90, 70, 90), "minecraft:air", "outside-footprint control gained a block");
    }

    #[test]
    fn missing_template_stops_before_an_incomplete_shell_is_reported() {
        let mut random = LegacyRandomSource::new(91);
        let error = generate([0, 70, 0], Rotation::None, &TemplateStore::default(), &mut random)
            .expect_err("an empty template store is a hard data error");
        assert_eq!(error.id, "minecraft:woodland_mansion/entrance");
    }
}
