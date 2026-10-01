//! Concrete immutable read handles for dense prefixes and packed resident columns.

use std::sync::Arc;

use lodestone_data::block_states::StateId;

use crate::dense_grid::{BaseStateFacts, DenseBlockGrid};
use crate::generated_storage::CompactBlockStorage;

/// A packed field and the palette whose indices its sections contain.
#[derive(Debug, Clone)]
pub struct PackedBlockColumn {
    origin_x: i32,
    origin_z: i32,
    min_y: i32,
    palette: Arc<Vec<StateId>>,
    blocks: CompactBlockStorage,
}

impl PackedBlockColumn {
    #[must_use]
    pub fn new(
        origin_x: i32,
        origin_z: i32,
        min_y: i32,
        palette: Arc<Vec<StateId>>,
        blocks: CompactBlockStorage,
    ) -> Self {
        Self { origin_x, origin_z, min_y, palette, blocks: blocks.into_shared_compact() }
    }

    #[inline]
    fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let lx = x - self.origin_x;
        let lz = z - self.origin_z;
        if !(0..16).contains(&lx) || !(0..16).contains(&lz)
            || !(self.min_y..self.min_y + self.blocks.height()).contains(&y)
        {
            return StateId::AIR;
        }
        crate::counters::bump_resident_packed_read();
        let storage_y = self.blocks.min_y() + y - self.min_y;
        self.palette[self.blocks.get(lx as usize, storage_y, lz as usize) as usize]
    }
}

/// Owning read context; dispatch stays concrete in block and height probes.
#[derive(Debug, Clone)]
pub enum BlockRead {
    Dense(Arc<DenseBlockGrid>),
    Packed(PackedBlockColumn),
}

impl From<Arc<DenseBlockGrid>> for BlockRead {
    fn from(grid: Arc<DenseBlockGrid>) -> Self {
        Self::Dense(grid)
    }
}

impl BlockRead {
    #[must_use]
    pub fn as_read(&self) -> BlockReadRef<'_> {
        match self {
            Self::Dense(grid) => BlockReadRef::Dense(grid),
            Self::Packed(column) => BlockReadRef::Packed(column),
        }
    }
}

/// Borrowed counterpart used by the ore view and vertical height scans.
#[derive(Debug, Clone, Copy)]
pub enum BlockReadRef<'a> {
    Dense(&'a DenseBlockGrid),
    Packed(&'a PackedBlockColumn),
}

impl BlockReadRef<'_> {
    #[inline]
    #[must_use]
    pub fn get_id(self, x: i32, y: i32, z: i32) -> StateId {
        match self {
            Self::Dense(grid) => grid.get_id(x, y, z),
            Self::Packed(column) => column.get_id(x, y, z),
        }
    }

    #[inline]
    pub(crate) fn get_id_and_facts(self, x: i32, y: i32, z: i32) -> (StateId, BaseStateFacts) {
        match self {
            Self::Dense(grid) => grid.get_id_and_facts(x, y, z),
            Self::Packed(column) => {
                let state = column.get_id(x, y, z);
                (state, BaseStateFacts::Builtin {
                    is_air: matches!(state.block(), lodestone_data::block::Block::Air
                        | lodestone_data::block::Block::CaveAir | lodestone_data::block::Block::VoidAir),
                    is_fluid: lodestone_data::snow_support::has_fluid_state(state),
                    blocks_motion: lodestone_data::block_solidity::blocks_motion(state),
                })
            }
        }
    }

    #[must_use]
    pub fn bounds(self) -> (i32, i32, i32, i32, i32, i32) {
        match self {
            Self::Dense(grid) => grid.bounds(),
            Self::Packed(column) => (
                column.origin_x, column.min_y, column.origin_z,
                16, column.blocks.height(), 16,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_negative_partial_column_preserves_nonzero_air_and_frozen_payload() {
        let stone = lodestone_data::block::Block::Stone.default_state();
        let glowstone = lodestone_data::block::Block::Glowstone.default_state();
        let palette = Arc::new(vec![stone, glowstone, StateId::AIR]);
        let mut cells = vec![0u16; 19 * 256];
        cells[16 * 256..].fill(2);
        cells[17 * 256 + 9 * 16 + 3] = 1;
        let mut blocks = CompactBlockStorage::from_flat(0, 19, &cells);
        let frozen = BlockRead::Packed(PackedBlockColumn::new(
            -48, -80, -7, Arc::clone(&palette), blocks.clone(),
        ));
        assert_eq!(frozen.as_read().get_id(-47, 8, -78), stone);
        assert_eq!(frozen.as_read().get_id(-45, 10, -71), glowstone);
        assert_eq!(frozen.as_read().get_id(-45, 11, -71), StateId::AIR);
        assert_eq!(frozen.as_read().get_id(-45, 12, -71), StateId::AIR);
        blocks.set(3, 17, 9, 0);
        blocks.set(1, 15, 2, 1);
        assert_eq!(blocks.get(3, 17, 9), 0);
        assert_eq!(frozen.as_read().get_id(-45, 10, -71), glowstone);
        assert_eq!(frozen.as_read().get_id(-47, 8, -78), stone);
        assert_eq!(frozen.as_read().bounds(), (-48, -7, -80, 16, 19, 16));
    }
}
