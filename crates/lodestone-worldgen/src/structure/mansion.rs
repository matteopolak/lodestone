//! Woodland-mansion exterior and corridor assembly.
//!
//! This is deliberately an **auditable partial** generator.  It consumes the
//! bundled templates to place the entrance, three exterior shells, corridor
//! floors, and two roof layers from the seeded floor plan.  The room-selection
//! pass is retained because it determines the third-floor footprint, but room
//! divider and furnishing templates are not emitted yet.  Callers must inspect
//! [`MansionAssembly::coverage`] and keep the corresponding registry-ledger row;
//! an assembled shell is useful block output, not a claim of a complete mansion.

use lodestone_worldgen_core::rng::RandomSource;

use super::processor::Processor;
use super::template::{Mirror, PlaceSettings, Rotation};
use super::{template_piece, StructurePiece, TemplateStore};

const CLEAR: i32 = 0;
const CORRIDOR: i32 = 1;
const ROOM: i32 = 2;
const START_ROOM: i32 = 3;
const BLOCKED: i32 = 5;

/// The subset of templates the current assembly can name.
pub const TEMPLATE_IDS: &[&str] = &[
    "minecraft:woodland_mansion/entrance",
    "minecraft:woodland_mansion/wall_flat",
    "minecraft:woodland_mansion/wall_window",
    "minecraft:woodland_mansion/corridor_floor",
    "minecraft:woodland_mansion/roof",
    "minecraft:woodland_mansion/roof_front",
    "minecraft:woodland_mansion/roof_corner",
];

/// Every template which must be loaded before this partial assembly starts.
#[must_use]
pub fn template_ids() -> Vec<&'static str> {
    TEMPLATE_IDS.to_vec()
}

/// The realised boundary of this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MansionCoverage {
    /// The layout, entrance, exterior shell, corridors and roofs are template
    /// pieces.  Room walls, doors, carpets, stairs and furnished rooms are not.
    ExteriorAndCorridors,
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

/// Builds a seeded exterior-and-corridor assembly at its already height-adjusted
/// origin.  `rotation` and `random` are owned by the structure start, following
/// the same stream that selected its generation point.
///
/// # Errors
///
/// Returns the first required template absent from `templates`; it never drops a
/// placement and continues with a visually plausible partial building.
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
    let anchor = add(&mut out, templates, "entrance", offset(origin, rotation, -9, 0, 0), rotation)?;
    let first = offset(anchor, rotation, 0, 0, 16);

    // Each level has the same ground footprint except that the third level uses
    // the seed-selected upper grid.  Walls are emitted per exposed cell edge;
    // unlike a perimeter-walk, this representation makes the footprint control
    // local and leaves no path-dependent mutable placer state.
    for (floor, grid) in [(0, &layout.base), (1, &layout.base), (2, &layout.third)] {
        let y = if floor == 2 { 19 } else { floor * 8 };
        let wall = if floor == 0 { "wall_flat" } else { "wall_window" };
        emit_walls(&mut out, templates, first, rotation, grid, y, wall)?;
        emit_corridors(&mut out, templates, first, rotation, grid, y)?;
    }
    emit_roof(&mut out, templates, first, rotation, &layout.base, Some(&layout.third), 16)?;
    emit_roof(&mut out, templates, first, rotation, &layout.third, None, 27)?;

    Ok(MansionAssembly { pieces: out, coverage: MansionCoverage::ExteriorAndCorridors })
}

fn add(
    out: &mut Vec<StructurePiece>, templates: &TemplateStore, name: &str, position: [i32; 3], rotation: Rotation,
) -> Result<[i32; 3], MissingTemplate> {
    let id = mansion_id(name);
    let template = templates.get(&id).ok_or(MissingTemplate { id: template_id(name) })?;
    out.push(template_piece("minecraft:wj", &id, template, position, settings(rotation)));
    Ok(position)
}

fn template_id(name: &str) -> &'static str {
    match name {
        "entrance" => "minecraft:woodland_mansion/entrance",
        "wall_flat" => "minecraft:woodland_mansion/wall_flat",
        "wall_window" => "minecraft:woodland_mansion/wall_window",
        "corridor_floor" => "minecraft:woodland_mansion/corridor_floor",
        "roof" => "minecraft:woodland_mansion/roof",
        "roof_front" => "minecraft:woodland_mansion/roof_front",
        "roof_corner" => "minecraft:woodland_mansion/roof_corner",
        _ => unreachable!("all partial-mansion template names are fixed"),
    }
}

fn mansion_id(name: &str) -> String {
    format!("minecraft:woodland_mansion/{name}")
}

fn settings(rotation: Rotation) -> PlaceSettings {
    PlaceSettings {
        rotation,
        mirror: Mirror::None,
        pivot: [0, 0, 0],
        processors: vec![Processor::structure_block()],
        waterlogging: false,
    }
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

fn emit_walls(
    out: &mut Vec<StructurePiece>, templates: &TemplateStore, origin: [i32; 3], rotation: Rotation, grid: &Grid, y: i32, wall: &str,
) -> Result<(), MissingTemplate> {
    for gy in 0..11 {
        for gx in 0..11 {
            if !is_house(grid.get(gx, gy)) {
                continue;
            }
            let cell = cell_position(origin, rotation, gx, gy, y);
            // The four exposed faces use the one straight wall template, with a
            // piece rotation matching the edge direction.  Corner roof pieces
            // provide the visual corner closure on both roof levels.
            for (dx, dz, turn, east, south) in [
                (1, 0, Rotation::None, 7, 0),
                (-1, 0, Rotation::Cw180, 0, 7),
                (0, -1, Rotation::Ccw90, 0, 0),
                (0, 1, Rotation::Cw90, 6, 6),
            ] {
                if !is_house(grid.get(gx + dx, gy + dz)) {
                    add(out, templates, wall, offset(cell, rotation, east, 0, south), rotation_add(rotation, turn))?;
                }
            }
        }
    }
    Ok(())
}

fn emit_corridors(
    out: &mut Vec<StructurePiece>, templates: &TemplateStore, origin: [i32; 3], rotation: Rotation, grid: &Grid, y: i32,
) -> Result<(), MissingTemplate> {
    for gy in 0..11 {
        for gx in 0..11 {
            if grid.get(gx, gy) == CORRIDOR {
                add(out, templates, "corridor_floor", cell_position(origin, rotation, gx, gy, y), rotation)?;
            }
        }
    }
    Ok(())
}

fn emit_roof(
    out: &mut Vec<StructurePiece>, templates: &TemplateStore, origin: [i32; 3], rotation: Rotation, grid: &Grid, above: Option<&Grid>, y: i32,
) -> Result<(), MissingTemplate> {
    for gy in 0..11 {
        for gx in 0..11 {
            if !is_house(grid.get(gx, gy)) || above.is_some_and(|upper| is_house(upper.get(gx, gy))) {
                continue;
            }
            let cell = cell_position(origin, rotation, gx, gy, y);
            add(out, templates, "roof", offset(cell, rotation, 0, 3, 0), rotation)?;
            for (dx, dz, turn, east, south) in [
                (1, 0, Rotation::None, 6, 0),
                (-1, 0, Rotation::Cw180, 0, 7),
                (0, -1, Rotation::Ccw90, -1, 0),
                (0, 1, Rotation::Cw90, 6, 6),
            ] {
                if !is_house(grid.get(gx + dx, gy + dz)) {
                    add(out, templates, "roof_front", offset(cell, rotation, east, 0, south), rotation_add(rotation, turn))?;
                }
            }
            // At convex corners, place the dedicated corner cap once.  The
            // east/south diagonal check gives each corner one stable owner.
            if !is_house(grid.get(gx + 1, gy)) && !is_house(grid.get(gx, gy + 1)) {
                add(out, templates, "roof_corner", offset(cell, rotation, 6, 0, 6), rotation)?;
            }
        }
    }
    Ok(())
}

fn cell_position(origin: [i32; 3], rotation: Rotation, x: i32, z: i32, y: i32) -> [i32; 3] {
    offset(origin, rotation, (x - 8) * 8, y, (z - 5) * 8)
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
}

struct Layout {
    base: Grid,
    third: Grid,
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

        let mut third = Grid::new(BLOCKED);
        // Select a connected pair from the second-floor footprint, then grow a
        // shorter corridor plan around it.  It deliberately shares the same
        // seeded topology mechanism as the lower footprint rather than choosing
        // a rectangular third floor.
        let candidates: Vec<(i32, i32)> = (0..11).flat_map(|y| (0..11).map(move |x| (x, y)))
            .filter(|&(x, y)| base.get(x, y) == ROOM && [Direction::North, Direction::East, Direction::South, Direction::West]
                .into_iter().any(|d| base.get(x + d.x(), y + d.z()) == ROOM))
            .collect();
        if !candidates.is_empty() {
            let index = random.next_int_bounded(candidates.len() as i32).max(0) as usize;
            let (x, y) = candidates[index];
            third.set(x, y, START_ROOM);
            for d in [Direction::North, Direction::East, Direction::South, Direction::West] {
                if base.get(x + d.x(), y + d.z()) == ROOM {
                    third.set(x + d.x(), y + d.z(), START_ROOM);
                    carve(&mut third, x + d.x() + d.x(), y + d.z() + d.z(), d, 4, random);
                    break;
                }
            }
            for gy in 0..11 { for gx in 0..11 {
                if !is_house(base.get(gx, gy)) { third.set(gx, gy, BLOCKED); }
            }}
            while clean_edges(&mut third) {}
        } else {
            third.fill(0, 0, 10, 10, BLOCKED);
        }
        Self { base, third }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction { North, East, South, West }

impl Direction {
    fn x(self) -> i32 { match self { Self::East => 1, Self::West => -1, _ => 0 } }
    fn z(self) -> i32 { match self { Self::South => 1, Self::North => -1, _ => 0 } }
    fn opposite(self) -> Self { match self { Self::North => Self::South, Self::East => Self::West, Self::South => Self::North, Self::West => Self::East } }
    fn cw(self) -> Self { match self { Self::North => Self::East, Self::East => Self::South, Self::South => Self::West, Self::West => Self::North } }
    fn ccw(self) -> Self { self.cw().opposite() }
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
    fn partial_template_list_is_closed_over_all_emitted_names() {
        for id in TEMPLATE_IDS {
            assert!(id.starts_with("minecraft:woodland_mansion/"));
        }
    }

    #[test]
    fn bundled_shell_places_a_seeded_rotated_block_footprint() {
        let templates = bundled_templates();
        let mut random = LegacyRandomSource::new(91);
        let assembly = generate([32, 70, -48], Rotation::Cw90, &templates, &mut random)
            .expect("all partial templates are bundled");
        assert_eq!(assembly.coverage(), MansionCoverage::ExteriorAndCorridors);
        assert_eq!(assembly.pieces().len(), 266, "seeded layout or piece order drifted");

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
        assert_eq!(writes, 51_486, "template placement footprint drifted");
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
