//! A single 16×16×16 chunk section: block states, biomes, and a non-air count.

use crate::container::{PaletteKind, PalettedContainer};

/// One cubic chunk section.
///
/// A section pairs a 4096-entry block-state [`PalettedContainer`] with a
/// 64-entry biome container (a 4×4×4 grid, one biome per 4×4×4 block cell) and
/// tracks how many blocks are non-air. Vanilla keeps this count so it can skip
/// rendering and ticking sections that contain only air without scanning them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSection {
    block_states: PalettedContainer,
    biomes: PalettedContainer,
    air_id: u32,
    non_air_count: u16,
}

impl ChunkSection {
    /// Number of blocks along one section edge.
    pub const EDGE: usize = 16;
    /// Number of biome cells along one section edge.
    pub const BIOME_EDGE: usize = 4;

    /// Creates a section filled with `air_id` blocks and `biome_id` biomes.
    #[must_use]
    pub fn new(
        block_kind: PaletteKind,
        biome_kind: PaletteKind,
        air_id: u32,
        biome_id: u32,
    ) -> Self {
        Self {
            block_states: PalettedContainer::new(block_kind, air_id),
            biomes: PalettedContainer::new(biome_kind, biome_id),
            air_id,
            non_air_count: 0,
        }
    }

    /// Builds a section from decoded containers, recomputing the non-air count.
    ///
    /// A version crate decodes the two containers from the chunk packet (along
    /// with any leading counts it defines) and hands them here.
    ///
    /// # Panics
    /// Panics if the container entry counts are not 4096 (blocks) and 64
    /// (biomes).
    #[must_use]
    pub fn from_containers(
        block_states: PalettedContainer,
        biomes: PalettedContainer,
        air_id: u32,
    ) -> Self {
        assert_eq!(
            block_states.entry_count(),
            4096,
            "block container must hold 4096 entries"
        );
        assert_eq!(
            biomes.entry_count(),
            64,
            "biome container must hold 64 entries"
        );
        let non_air_count = (0..block_states.entry_count())
            .filter(|&i| block_states.get(i) != air_id)
            .count() as u16;
        Self {
            block_states,
            biomes,
            air_id,
            non_air_count,
        }
    }

    /// The block-state container.
    #[must_use]
    pub const fn block_states(&self) -> &PalettedContainer {
        &self.block_states
    }

    /// The biome container.
    #[must_use]
    pub const fn biomes(&self) -> &PalettedContainer {
        &self.biomes
    }

    /// The block-state id treated as air.
    #[must_use]
    pub const fn air_id(&self) -> u32 {
        self.air_id
    }

    /// Number of non-air blocks in this section.
    #[must_use]
    pub const fn non_air_count(&self) -> u16 {
        self.non_air_count
    }

    /// Returns `true` if every block in the section is air.
    #[must_use]
    pub const fn is_air_only(&self) -> bool {
        self.non_air_count == 0
    }

    /// Returns `true` if the section is air-only and every biome equals
    /// `biome_id`, i.e. it carries no information beyond an empty section.
    #[must_use]
    pub fn is_empty(&self, biome_id: u32) -> bool {
        self.is_air_only() && self.biomes.single_value() == Some(biome_id)
    }

    /// Returns the block state at local `(x, y, z)`, each in `0..16`.
    ///
    /// # Panics
    /// Panics if any coordinate is out of range.
    #[must_use]
    pub fn get_block(&self, x: usize, y: usize, z: usize) -> u32 {
        self.block_states
            .get(self.block_states.kind().index(x, y, z))
    }

    /// Sets the block state at local `(x, y, z)`, maintaining the non-air count.
    ///
    /// # Panics
    /// Panics if any coordinate is out of range.
    pub fn set_block(&mut self, x: usize, y: usize, z: usize, value: u32) {
        let index = self.block_states.kind().index(x, y, z);
        let old = self.block_states.get(index);
        if old == value {
            return;
        }
        if old == self.air_id {
            self.non_air_count += 1;
        } else if value == self.air_id {
            self.non_air_count -= 1;
        }
        self.block_states.set(index, value);
    }

    /// Returns the biome at local biome cell `(x, y, z)`, each in `0..4`.
    ///
    /// # Panics
    /// Panics if any coordinate is out of range.
    #[must_use]
    pub fn get_biome(&self, x: usize, y: usize, z: usize) -> u32 {
        self.biomes.get(self.biomes.kind().index(x, y, z))
    }

    /// Sets the biome at local biome cell `(x, y, z)`, each in `0..4`.
    ///
    /// # Panics
    /// Panics if any coordinate is out of range.
    pub fn set_biome(&mut self, x: usize, y: usize, z: usize, value: u32) {
        let index = self.biomes.kind().index(x, y, z);
        self.biomes.set(index, value);
    }

    /// Heap bytes owned by this section's two containers.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.block_states.heap_bytes() + self.biomes.heap_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section() -> ChunkSection {
        ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), 0, 0)
    }

    #[test]
    fn yzx_indexing_is_pinned() {
        // Vanilla flat index is (y*256 + z*16 + x). Confirm each axis stride.
        let kind = PaletteKind::block_states();
        assert_eq!(kind.index(1, 0, 0), 1, "x advances by 1");
        assert_eq!(kind.index(0, 0, 1), 16, "z advances by 16");
        assert_eq!(kind.index(0, 1, 0), 256, "y advances by 256");
        assert_eq!(kind.index(2, 3, 4), 3 * 256 + 4 * 16 + 2);

        // Biomes use a 4-wide axis: index = y*16 + z*4 + x.
        let bkind = PaletteKind::biomes();
        assert_eq!(bkind.index(1, 0, 0), 1);
        assert_eq!(bkind.index(0, 0, 1), 4);
        assert_eq!(bkind.index(0, 1, 0), 16);
    }

    #[test]
    fn set_block_tracks_non_air_count() {
        let mut s = section();
        assert!(s.is_air_only());
        assert_eq!(s.non_air_count(), 0);

        s.set_block(1, 2, 3, 42);
        assert_eq!(s.non_air_count(), 1);
        assert!(!s.is_air_only());
        assert_eq!(s.get_block(1, 2, 3), 42);

        // Overwriting a non-air block with another non-air block: no change.
        s.set_block(1, 2, 3, 43);
        assert_eq!(s.non_air_count(), 1);

        // Setting it back to air decrements the count.
        s.set_block(1, 2, 3, 0);
        assert_eq!(s.non_air_count(), 0);
        assert!(s.is_air_only());
    }

    #[test]
    fn biomes_are_independent_of_blocks() {
        let mut s = section();
        s.set_biome(0, 0, 0, 5);
        assert_eq!(s.get_biome(0, 0, 0), 5);
        assert!(s.is_air_only(), "biomes do not affect the block count");
        assert!(
            !s.is_empty(0),
            "a non-default biome keeps the section non-empty"
        );
    }

    #[test]
    fn from_containers_recomputes_non_air() {
        let kind = PaletteKind::block_states();
        let mut blocks = PalettedContainer::new(kind, 0);
        blocks.set(0, 1);
        blocks.set(1, 1);
        blocks.set(2, 2);
        let biomes = PalettedContainer::new(PaletteKind::biomes(), 0);
        let s = ChunkSection::from_containers(blocks, biomes, 0);
        assert_eq!(s.non_air_count(), 3);
    }
}
