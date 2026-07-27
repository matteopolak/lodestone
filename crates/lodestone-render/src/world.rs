//! Bridge from `lodestone-world`'s real chunk storage into the mesher's
//! [`SectionView`] input.
//!
//! The mesher deliberately knows nothing about block-state ids, palettes, or the
//! block registry — it consumes [`Cell`]s. This module adapts a real
//! [`lodestone_world::ChunkSection`] (paletted `u32` block-state storage,
//! section-local `0..16` coordinates) into a [`SectionView`] by delegating the
//! "what does state id `N` look like?" question to a [`BlockClassifier`].
//!
//! That split is the important one: `lodestone-world` owns storage, a version
//! crate owns the state-id → appearance mapping, and the renderer owns geometry.
//! The classifier is the single seam where a real block registry (or a test
//! double) plugs in. Because [`ChunkSection::get_block`] is section-local
//! `0..16` — not world-Y like the column API — the adapter is a direct wrap with
//! no coordinate translation, and cross-section access stays the mesher's job
//! via [`SectionNeighborhood`](crate::section::SectionNeighborhood).

use lodestone_world::ChunkSection;

use crate::section::{Cell, SectionView};

/// Resolves a block-state id into the renderer's [`Cell`] appearance.
///
/// This is the version-free seam between raw block-state ids and render data. A
/// protocol/version crate implements it against the real block registry; tests
/// implement it with a synthetic mapping. It is intentionally a pure function of
/// the id plus light — the renderer never reaches into block semantics itself.
pub trait BlockClassifier {
    /// The rendered [`Cell`] for `state_id`, given the block and sky light at
    /// that position (`0..=15`). Return a **lit but empty** cell for air /
    /// non-rendered states (`occludes: false`, `surface: None`, but with the
    /// real `block_light`/`sky_light`) — *not* [`Cell::EMPTY`].
    ///
    /// This matters: a block face samples its lighting from the neighbouring
    /// cell it faces into. If air is reported as the unlit [`Cell::EMPTY`]
    /// (`sky_light: 0`), every exposed block face bordering that air renders
    /// black. Air carries light in Minecraft; the classifier must preserve it.
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell;
}

/// A per-position light source for a section, in section-local `0..16`
/// coordinates. `lodestone-world` exposes light per column; a caller adapts it
/// to this for the section being meshed.
pub trait SectionLight {
    /// Block light `0..=15` at section-local `(x, y, z)`.
    fn block_light(&self, x: usize, y: usize, z: usize) -> u8;
    /// Sky light `0..=15` at section-local `(x, y, z)`.
    fn sky_light(&self, x: usize, y: usize, z: usize) -> u8;
}

/// A [`SectionLight`] that reports a constant sky light everywhere and no block
/// light. Useful for tests and for sections above the heightmap where full sky
/// light is a fine approximation until real light data is wired in.
#[derive(Debug, Clone, Copy)]
pub struct UniformLight {
    /// Block light returned for every cell.
    pub block_light: u8,
    /// Sky light returned for every cell.
    pub sky_light: u8,
}

impl Default for UniformLight {
    fn default() -> Self {
        Self {
            block_light: 0,
            sky_light: 15,
        }
    }
}

impl SectionLight for UniformLight {
    fn block_light(&self, _x: usize, _y: usize, _z: usize) -> u8 {
        self.block_light
    }
    fn sky_light(&self, _x: usize, _y: usize, _z: usize) -> u8 {
        self.sky_light
    }
}

/// Adapts a real [`ChunkSection`] into a [`SectionView`] using a
/// [`BlockClassifier`] and a [`SectionLight`].
///
/// Build one per section (including the 26 neighbours) and assemble them into a
/// [`SectionNeighborhood`](crate::section::SectionNeighborhood) for correct
/// boundary meshing.
#[derive(Debug)]
pub struct ChunkSectionView<'a, C: BlockClassifier, L: SectionLight> {
    section: &'a ChunkSection,
    classifier: &'a C,
    light: &'a L,
}

impl<'a, C: BlockClassifier, L: SectionLight> ChunkSectionView<'a, C, L> {
    /// Wraps a section with the classifier and light source used to resolve its
    /// cells.
    #[must_use]
    pub fn new(section: &'a ChunkSection, classifier: &'a C, light: &'a L) -> Self {
        Self {
            section,
            classifier,
            light,
        }
    }
}

impl<C: BlockClassifier, L: SectionLight> SectionView for ChunkSectionView<'_, C, L> {
    fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
        let state = self.section.get_block(x, y, z);
        let bl = self.light.block_light(x, y, z);
        let sl = self.light.sky_light(x, y, z);
        self.classifier.classify(state, bl, sl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{mesh_greedy, mesh_simple};
    use crate::section::{SectionNeighborhood, SpriteId};
    use lodestone_world::PaletteKind;

    const AIR: u32 = 0;
    const STONE: u32 = 1;

    /// Air is a lit-but-empty cell; every other id is a solid cube whose sprite
    /// is its id.
    #[derive(Debug)]
    struct SimpleClassifier;
    impl BlockClassifier for SimpleClassifier {
        fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
            if state_id == AIR {
                Cell {
                    occludes: false,
                    surface: None,
                    block_light,
                    sky_light,
                }
            } else {
                let mut c = Cell::solid(SpriteId(state_id as u16));
                c.block_light = block_light;
                c.sky_light = sky_light;
                c
            }
        }
    }

    fn stone_section() -> ChunkSection {
        // A real paletted section: solid stone floor (y=0), air above.
        let mut s = ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), AIR, 0);
        for x in 0..16 {
            for z in 0..16 {
                s.set_block(x, 0, z, STONE);
            }
        }
        s
    }

    #[test]
    fn adapter_reads_real_section_storage() {
        let s = stone_section();
        let light = UniformLight::default();
        let view = ChunkSectionView::new(&s, &SimpleClassifier, &light);
        // Floor cell is solid stone; the cell above is air.
        assert!(view.cell(3, 0, 5).occludes);
        assert_eq!(view.cell(3, 0, 5).surface.unwrap().sprites[0], SpriteId(1));
        assert!(!view.cell(3, 1, 5).occludes);
        assert!(view.cell(3, 1, 5).surface.is_none());
    }

    #[test]
    fn real_section_meshes_to_the_expected_floor() {
        let s = stone_section();
        let light = UniformLight::default();
        let view = ChunkSectionView::new(&s, &SimpleClassifier, &light);
        // Smooth lighting samples corner neighbours that cross the section
        // boundary, so a bare `centre_only` hood reads out-of-section cells as
        // unlit and fragments the merge at every edge. Surround the slab with
        // lit air (as the real pipeline's populated neighbourhood would) so the
        // boundary light is continuous and greedy can merge each plane.
        struct AirLit;
        impl SectionView for AirLit {
            fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
                Cell {
                    occludes: false,
                    surface: None,
                    block_light: 0,
                    sky_light: 15,
                }
            }
        }
        let air = AirLit;
        let mut hood = SectionNeighborhood::centre_only(&view);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx, dy, dz) != (0, 0, 0) {
                        hood.set(dx, dy, dz, Some(&air));
                    }
                }
            }
        }
        // A 16×16 floor slab with lit-air neighbours: top + bottom + 4 sides.
        // Simple emits every exposed unit face; greedy merges each plane.
        let simple = mesh_simple(&hood);
        let greedy = mesh_greedy(&hood);
        assert!(simple.quad_count() > greedy.quad_count());
        // Greedy: top plane (1) + bottom plane (1) + 4 one-block-tall sides.
        assert_eq!(greedy.quad_count(), 6);
        assert!(!simple.vertices.is_empty());
    }

    #[test]
    fn light_flows_from_source_into_cells() {
        let s = stone_section();
        let light = UniformLight {
            block_light: 7,
            sky_light: 12,
        };
        let view = ChunkSectionView::new(&s, &SimpleClassifier, &light);
        let c = view.cell(0, 0, 0);
        assert_eq!(c.block_light, 7);
        assert_eq!(c.sky_light, 12);
    }
}
