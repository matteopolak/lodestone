//! Mesher input: an abstract view over a 16³ chunk section and its neighbours.
//!
//! The mesher never depends on `lodestone-world`. Instead it consumes the
//! [`SectionView`] trait, and `lodestone-world` (or a test double) implements
//! it. The single most important correctness property a section mesher needs is
//! **neighbour access**: whether a face on a section boundary is drawn depends
//! on the block in the *adjacent* section, not the one being meshed. Meshing a
//! section in isolation produces a visible shell of wrong faces at every
//! boundary.
//!
//! We make that access structural via [`SectionNeighborhood`], a 3×3×3 grid of
//! sections centred on the one being meshed. Why 3×3×3 (26 neighbours) rather
//! than just the 6 face-adjacent sections?
//!
//! * **Face culling** only ever steps one axis, so it needs just the 6 face
//!   neighbours.
//! * **Ambient occlusion** samples the three blocks around each vertex corner,
//!   which can step two or three axes at once. At a section *corner* those
//!   samples land in edge- and corner-adjacent sections. Providing the full
//!   3×3×3 makes AO correct everywhere, including section corners, instead of
//!   silently wrong there.
//!
//! A missing neighbour (`None`, e.g. an unloaded or out-of-world section) reads
//! as [`Cell::EMPTY`]. That means boundary faces toward unloaded space are
//! drawn; document this at the call site if you prefer to defer meshing until
//! neighbours load.

/// Section edge length in blocks. Minecraft sections are 16×16×16.
pub const SECTION_SIZE: usize = 16;

/// An opaque per-face texture handle carried straight through the mesher into
/// the packed vertex. Its meaning (which atlas sprite) is defined by the
/// producer; the mesher treats it as an opaque id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpriteId(pub u16);

/// The six axis-aligned faces of a cube, ordered `-X,+X,-Y,+Y,-Z,+Z`.
///
/// The numeric order matches the neighbour indexing used throughout the crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    /// Towards `-X` (west).
    NegX,
    /// Towards `+X` (east).
    PosX,
    /// Towards `-Y` (down).
    NegY,
    /// Towards `+Y` (up).
    PosY,
    /// Towards `-Z` (north).
    NegZ,
    /// Towards `+Z` (south).
    PosZ,
}

impl Face {
    /// All six faces in canonical order.
    pub const ALL: [Face; 6] = [
        Face::NegX,
        Face::PosX,
        Face::NegY,
        Face::PosY,
        Face::NegZ,
        Face::PosZ,
    ];

    /// Dense index `0..6` in canonical order.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Face::NegX => 0,
            Face::PosX => 1,
            Face::NegY => 2,
            Face::PosY => 3,
            Face::NegZ => 4,
            Face::PosZ => 5,
        }
    }

    /// Unit normal as integer offsets.
    #[must_use]
    pub const fn normal(self) -> [i32; 3] {
        match self {
            Face::NegX => [-1, 0, 0],
            Face::PosX => [1, 0, 0],
            Face::NegY => [0, -1, 0],
            Face::PosY => [0, 1, 0],
            Face::NegZ => [0, 0, -1],
            Face::PosZ => [0, 0, 1],
        }
    }

    /// The opposite face.
    #[must_use]
    pub const fn opposite(self) -> Face {
        match self {
            Face::NegX => Face::PosX,
            Face::PosX => Face::NegX,
            Face::NegY => Face::PosY,
            Face::PosY => Face::NegY,
            Face::NegZ => Face::PosZ,
            Face::PosZ => Face::NegZ,
        }
    }
}

/// The render appearance of a solid block: one sprite per face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    /// Atlas sprite per [`Face`], indexed by [`Face::index`].
    pub sprites: [SpriteId; 6],
}

impl Surface {
    /// A surface using the same sprite on all six faces.
    #[must_use]
    pub const fn uniform(sprite: SpriteId) -> Self {
        Self {
            sprites: [sprite; 6],
        }
    }
}

/// Everything the mesher needs to know about one block cell.
///
/// This is deliberately *resolved* render data, not a raw block state. The
/// producer is responsible for turning version-specific block states into these
/// fields, which keeps the mesher free of block-registry semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Whether this cell fully occludes the adjacent face of its neighbours.
    ///
    /// This drives both face culling (a face is hidden if its neighbour
    /// occludes) and ambient occlusion (occluding cells cast AO).
    pub occludes: bool,
    /// The block's drawable surface, or `None` if the cell emits no geometry
    /// (air, or a non-rendered marker).
    pub surface: Option<Surface>,
    /// Block light level `0..=15`.
    pub block_light: u8,
    /// Sky light level `0..=15`.
    pub sky_light: u8,
}

impl Cell {
    /// An empty, fully transparent, unlit cell. Missing neighbours read as this.
    pub const EMPTY: Cell = Cell {
        occludes: false,
        surface: None,
        block_light: 0,
        sky_light: 0,
    };

    /// A convenience constructor for a solid opaque block with one sprite on
    /// every face and full sky light.
    #[must_use]
    pub const fn solid(sprite: SpriteId) -> Self {
        Cell {
            occludes: true,
            surface: Some(Surface::uniform(sprite)),
            block_light: 0,
            sky_light: 15,
        }
    }
}

/// A read-only view over a single 16³ section.
///
/// Implementors resolve raw storage into [`Cell`]s. Coordinates are section-
/// local, each in `0..16`.
pub trait SectionView {
    /// The cell at section-local `(x, y, z)`, each in `0..16`.
    fn cell(&self, x: usize, y: usize, z: usize) -> Cell;
}

/// A 3×3×3 grid of sections centred on the one being meshed.
///
/// Index `[dx+1][dy+1][dz+1]` holds the section offset by `(dx,dy,dz)` sections
/// from the centre, each of `dx,dy,dz` in `-1..=1`. The centre is `[1][1][1]`.
/// Any slot may be `None` (unloaded / out of world), which reads as
/// [`Cell::EMPTY`].
///
/// The whole point of this type is [`cell`](Self::cell), which accepts
/// coordinates that step *outside* `0..16` into the neighbours, so the mesher
/// can look one block past every boundary (for face culling) and diagonally
/// past every corner (for AO) without the caller tracking which section owns a
/// given coordinate.
#[derive(Default)]
pub struct SectionNeighborhood<'a> {
    sections: [[[Option<&'a dyn SectionView>; 3]; 3]; 3],
}

impl core::fmt::Debug for SectionNeighborhood<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `dyn SectionView` is not `Debug`; summarise which slots are present.
        let mut present = 0usize;
        for plane in &self.sections {
            for row in plane {
                for slot in row {
                    if slot.is_some() {
                        present += 1;
                    }
                }
            }
        }
        f.debug_struct("SectionNeighborhood")
            .field("present_sections", &present)
            .finish()
    }
}

impl<'a> SectionNeighborhood<'a> {
    /// A neighbourhood with only the centre section populated; all 26 neighbours
    /// are empty.
    #[must_use]
    pub fn centre_only(centre: &'a dyn SectionView) -> Self {
        let mut n = Self::default();
        n.set(0, 0, 0, Some(centre));
        n
    }

    /// Set the section at section-offset `(dx, dy, dz)`, each in `-1..=1`.
    ///
    /// Offsets outside that range are ignored.
    pub fn set(&mut self, dx: i32, dy: i32, dz: i32, section: Option<&'a dyn SectionView>) {
        if let (Some(ix), Some(iy), Some(iz)) =
            (offset_index(dx), offset_index(dy), offset_index(dz))
        {
            self.sections[ix][iy][iz] = section;
        }
    }

    /// The cell at centre-local `(x, y, z)`, where each coordinate may range
    /// over `-16..32` — i.e. anywhere in the 3×3×3 block volume. Coordinates in
    /// `0..16` hit the centre section; others route into the appropriate
    /// neighbour. Anything outside the loaded 3×3×3 reads as [`Cell::EMPTY`].
    #[must_use]
    pub fn cell(&self, x: i32, y: i32, z: i32) -> Cell {
        let size = SECTION_SIZE as i32;
        // Section offset via floor-division so negatives route correctly.
        let (sx, lx) = split(x, size);
        let (sy, ly) = split(y, size);
        let (sz, lz) = split(z, size);
        let (Some(ix), Some(iy), Some(iz)) = (offset_index(sx), offset_index(sy), offset_index(sz))
        else {
            return Cell::EMPTY;
        };
        match self.sections[ix][iy][iz] {
            Some(section) => section.cell(lx as usize, ly as usize, lz as usize),
            None => Cell::EMPTY,
        }
    }
}

/// Floor-divide `v` by `size`, returning `(section_offset, local_coord)` with
/// `local_coord` always in `0..size`.
fn split(v: i32, size: i32) -> (i32, i32) {
    (v.div_euclid(size), v.rem_euclid(size))
}

/// Map a section offset in `-1..=1` to an array index `0..3`.
fn offset_index(offset: i32) -> Option<usize> {
    match offset {
        -1 => Some(0),
        0 => Some(1),
        1 => Some(2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A section that is empty everywhere except a set of solid blocks.
    struct SparseSection {
        solids: Vec<(usize, usize, usize, SpriteId)>,
    }

    impl SectionView for SparseSection {
        fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
            for &(sx, sy, sz, sprite) in &self.solids {
                if (sx, sy, sz) == (x, y, z) {
                    return Cell::solid(sprite);
                }
            }
            Cell::EMPTY
        }
    }

    /// A section that is solid everywhere with one sprite.
    struct FullSection(SpriteId);
    impl SectionView for FullSection {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell::solid(self.0)
        }
    }

    #[test]
    fn face_order_is_canonical() {
        for (i, f) in Face::ALL.iter().enumerate() {
            assert_eq!(f.index(), i);
        }
    }

    #[test]
    fn opposite_faces_have_negated_normals() {
        for f in Face::ALL {
            let n = f.normal();
            let o = f.opposite().normal();
            assert_eq!([-n[0], -n[1], -n[2]], o);
        }
    }

    #[test]
    fn centre_only_reads_neighbours_as_empty() {
        let centre = FullSection(SpriteId(7));
        let hood = SectionNeighborhood::centre_only(&centre);
        // Inside the centre is solid.
        assert!(hood.cell(0, 0, 0).occludes);
        assert!(hood.cell(15, 15, 15).occludes);
        // One step past every boundary is empty (no neighbour loaded).
        assert_eq!(hood.cell(-1, 0, 0), Cell::EMPTY);
        assert_eq!(hood.cell(16, 0, 0), Cell::EMPTY);
        assert_eq!(hood.cell(0, -1, 0), Cell::EMPTY);
        assert_eq!(hood.cell(0, 16, 0), Cell::EMPTY);
        assert_eq!(hood.cell(0, 0, -1), Cell::EMPTY);
        assert_eq!(hood.cell(0, 0, 16), Cell::EMPTY);
    }

    #[test]
    fn neighbour_routing_hits_the_right_section() {
        let centre = SparseSection { solids: vec![] };
        let east = FullSection(SpriteId(2)); // +X neighbour
        let up = FullSection(SpriteId(3)); // +Y neighbour
        let mut hood = SectionNeighborhood::centre_only(&centre);
        hood.set(1, 0, 0, Some(&east));
        hood.set(0, 1, 0, Some(&up));

        // x=16 is local x=0 of the +X neighbour.
        assert_eq!(
            hood.cell(16, 5, 5).surface,
            Some(Surface::uniform(SpriteId(2)))
        );
        assert_eq!(
            hood.cell(31, 5, 5).surface,
            Some(Surface::uniform(SpriteId(2)))
        );
        // y=16 is local y=0 of the +Y neighbour.
        assert_eq!(
            hood.cell(5, 16, 5).surface,
            Some(Surface::uniform(SpriteId(3)))
        );
        // -X neighbour was never set: empty.
        assert_eq!(hood.cell(-1, 5, 5), Cell::EMPTY);
    }

    #[test]
    fn diagonal_corner_routes_into_corner_neighbour() {
        // AO at a section corner needs the corner-adjacent section, which is a
        // diagonal neighbour, not one of the 6 face neighbours.
        let centre = SparseSection { solids: vec![] };
        let corner = FullSection(SpriteId(9));
        let mut hood = SectionNeighborhood::centre_only(&centre);
        hood.set(1, 1, 1, Some(&corner));
        // (16,16,16) steps diagonally into the +X+Y+Z corner section.
        assert_eq!(
            hood.cell(16, 16, 16).surface,
            Some(Surface::uniform(SpriteId(9)))
        );
        assert!(hood.cell(16, 16, 16).occludes);
    }

    #[test]
    fn split_floor_divides_for_negatives() {
        assert_eq!(split(-1, 16), (-1, 15));
        assert_eq!(split(0, 16), (0, 0));
        assert_eq!(split(15, 16), (0, 15));
        assert_eq!(split(16, 16), (1, 0));
        assert_eq!(split(-16, 16), (-1, 0));
        assert_eq!(split(-17, 16), (-2, 15));
    }
}
