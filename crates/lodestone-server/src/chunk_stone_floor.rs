//! A flat stone floor: the deterministic, generator-free terrain the
//! transport, command and simulation tests stand players and mobs on.
//!
//! [`StoneFloorSource`] is stone for every `y` below a fixed surface and air
//! at and above it, with the default biome everywhere. It has no surface rules,
//! fluids, structures or edit retention, so a test that uses it proves the
//! wire and the simulation, not the terrain generator. For real terrain use
//! [`crate::chunk::Terrain263ChunkSource`].

use lodestone_data::block_states::StateId;

use crate::chunk::{ChunkColumn, ChunkSource, DEFAULT_BIOME, air_state, stone_state};

/// A [`ChunkSource`] that is stone for `min_y <= y < surface_y` and air above.
///
/// The top solid block is `surface_y - 1`, so an entity standing on the floor
/// has its feet at `surface_y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoneFloorSource {
    min_y: i32,
    height: i32,
    surface_y: i32,
}

impl StoneFloorSource {
    /// A floor in a world spanning `min_y..min_y + height`, solid below
    /// `surface_y`.
    #[must_use]
    pub fn new(min_y: i32, height: i32, surface_y: i32) -> Self {
        Self {
            min_y,
            height,
            surface_y,
        }
    }

    fn is_solid(&self, y: i32) -> bool {
        y >= self.min_y && y < self.surface_y.min(self.min_y + self.height)
    }
}

impl ChunkSource for StoneFloorSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(self.min_y, self.height);
        for y in (self.min_y..self.min_y + self.height).filter(|&y| self.is_solid(y)) {
            for lx in 0..16 {
                for lz in 0..16 {
                    col.set_solid(lx, y, lz, true);
                }
            }
        }
        col
    }

    fn block_state_id(&self, _x: i32, y: i32, _z: i32) -> StateId {
        if self.is_solid(y) {
            stone_state()
        } else {
            air_state()
        }
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        DEFAULT_BIOME.to_string()
    }

    /// The floor regenerates every column from its three numbers, so there is
    /// nowhere for an edit to live. Panics rather than silently discarding the
    /// placement; a test that edits blocks needs a retaining source.
    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        panic!(
            "StoneFloorSource retains no edits; cannot set ({x}, {y}, {z}) to state {state:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor's top block is `surface_y - 1` and its bottom is `min_y`, and
    /// the generated column agrees with the point read cell by cell.
    #[test]
    fn column_and_point_reads_agree_on_the_floor_top() {
        let src = StoneFloorSource::new(-64, 128, 0);
        assert_eq!(src.block_state_id(5, -1, -7), stone_state());
        assert_eq!(src.block_state_id(5, 0, -7), air_state());
        assert_eq!(src.block_state_id(5, -64, -7), stone_state());
        assert_eq!(src.block_state_id(5, -65, -7), air_state());
        let col = src.column(3, -2);
        for y in -66..66 {
            assert_eq!(
                col.block_state_id(5, y, 9),
                src.block_state_id(3 * 16 + 5, y, -2 * 16 + 9),
                "y = {y}"
            );
        }
        assert_eq!(src.biome_state_at(0, 0, 0), DEFAULT_BIOME);
    }
}
