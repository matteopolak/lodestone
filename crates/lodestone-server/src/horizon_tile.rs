//! Packed coarse-horizon tiles, the payload the browser server worker returns
//! to the page for a distant-terrain tile.

use crate::{ChunkSource, HorizonSample};

/// Cells along one edge of a horizon tile.
pub const HORIZON_TILE_CELLS: usize = 64;
/// Block width and depth of one horizon cell.
pub const HORIZON_CELL_BLOCKS: i32 = 16;
/// Words per packed cell.
pub const HORIZON_WORDS_PER_CELL: usize = 2;

/// Sentinel stored in a cell's water word when it has no water surface.
const DRY: u16 = u16::MAX;

fn encode_y(y: i32) -> u16 {
    y.saturating_add(64).clamp(0, i32::from(u16::MAX)) as u16
}

/// One cell as two words: terrain height | water height << 16, then colour |
/// flags << 16. Heights are offset by 64 and saturate to 16 bits.
#[must_use]
pub fn pack_horizon_sample(sample: HorizonSample) -> [u32; HORIZON_WORDS_PER_CELL] {
    let water = sample.water_y.map_or(DRY, encode_y);
    [
        u32::from(encode_y(sample.terrain_y)) | (u32::from(water) << 16),
        u32::from(sample.surface_rgb565) | (u32::from(sample.flags) << 16),
    ]
}

/// Packs the tile at `(tile_x, tile_z)` row by row along +X, or `None` when
/// the source has no horizon data for any cell of it.
#[must_use]
pub fn pack_horizon_tile(source: &dyn ChunkSource, tile_x: i32, tile_z: i32) -> Option<Vec<u32>> {
    let tile_blocks = HORIZON_CELL_BLOCKS * HORIZON_TILE_CELLS as i32;
    let origin_x = tile_x.saturating_mul(tile_blocks);
    let origin_z = tile_z.saturating_mul(tile_blocks);
    let mut words = Vec::with_capacity(HORIZON_TILE_CELLS * HORIZON_TILE_CELLS * HORIZON_WORDS_PER_CELL);
    for z in 0..HORIZON_TILE_CELLS as i32 {
        for x in 0..HORIZON_TILE_CELLS as i32 {
            let sample = source.horizon_sample(
                origin_x.saturating_add(x * HORIZON_CELL_BLOCKS),
                origin_z.saturating_add(z * HORIZON_CELL_BLOCKS),
            )?;
            words.extend(pack_horizon_sample(sample));
        }
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dry_cell_packs_the_sentinel_and_offsets_heights() {
        let packed = pack_horizon_sample(HorizonSample {
            terrain_y: 10,
            water_y: None,
            surface_rgb565: 0xABCD,
            flags: 0x0102,
        });
        assert_eq!(packed, [74 | (0xFFFF << 16), 0xABCD | (0x0102 << 16)]);
    }

    #[test]
    fn heights_saturate_to_sixteen_bits() {
        let packed = pack_horizon_sample(HorizonSample {
            terrain_y: -1000,
            water_y: Some(i32::MAX),
            surface_rgb565: 0,
            flags: 0,
        });
        assert_eq!(packed[0], 0xFFFF_0000);
    }
}
