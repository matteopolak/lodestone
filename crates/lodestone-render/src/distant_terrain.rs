//! Bounded data backing the coarse distant-terrain heightfield.
//!
//! This module deliberately owns only the representation and its coordinate
//! arithmetic. It creates no `wgpu` resources and is not yet submitted by the
//! shell; that separation lets the allocation and coverage invariants be
//! tested without an adapter. The future shell bridge owns population from the
//! worldgen surface query and the single far-terrain draw pass.

/// Block width and depth represented by one coarse horizon cell.
pub const HORIZON_CELL_BLOCKS: i32 = 16;
/// Cells along one edge of a world-anchored horizon tile.
pub const HORIZON_TILE_CELLS: usize = 64;
/// Block width and depth of one horizon tile.
pub const HORIZON_TILE_BLOCKS: i32 = HORIZON_CELL_BLOCKS * HORIZON_TILE_CELLS as i32;
/// Largest horizon radius this first tier can represent, in chunks.
///
/// This is a visual horizon limit, not the normal chunk-streaming radius.
pub const MAX_HORIZON_DISTANCE_CHUNKS: i32 = 256;
/// The number of whole tiles retained in either direction from the camera's
/// current tile. The inclusive square is intentionally one tile wider than
/// the requested radius, so a camera anywhere within its centre tile still has
/// the full 256-chunk horizon.
pub const HORIZON_TILE_RADIUS: i32 = 4;
/// Tiles in one retained row or column.
pub const HORIZON_TILES_PER_AXIS: usize = (HORIZON_TILE_RADIUS as usize * 2) + 1;
/// Number of cells stored in one tile.
pub const HORIZON_CELLS_PER_TILE: usize = HORIZON_TILE_CELLS * HORIZON_TILE_CELLS;
/// Absolute tile-allocation limit for one [`DistantTerrain`] instance.
pub const MAX_HORIZON_TILES: usize = HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS;
/// Absolute cell-allocation limit for one [`DistantTerrain`] instance.
pub const MAX_HORIZON_CELLS: usize = MAX_HORIZON_TILES * HORIZON_CELLS_PER_TILE;
/// CPU bytes retained by the fixed first-tier grid.
pub const MAX_HORIZON_BYTES: usize = MAX_HORIZON_CELLS * std::mem::size_of::<HorizonCell>();

/// One coarse terrain sample, laid out exactly as a future GPU texture triplet
/// can carry it.
///
/// Heights use the world `y + 64` convention, `water_y` uses `u16::MAX` for a
/// dry cell, `surface_rgb565` is gamma-space packed colour, and `flags` is
/// reserved for a small material class or a future canopy offset. Keeping this
/// record at eight bytes is the representation change that keeps the horizon
/// bounded; it is not a compressed chunk column.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizonCell {
    /// Terrain height, stored as `world_y + 64`.
    pub terrain_y: u16,
    /// Water-surface height, or [`Self::DRY`].
    pub water_y: u16,
    /// Gamma-space RGB565 surface colour.
    pub surface_rgb565: u16,
    /// Material-class and future extension bits.
    pub flags: u16,
}

impl HorizonCell {
    /// Sentinel for a cell with no water surface.
    pub const DRY: u16 = u16::MAX;

    /// An empty black, dry sample. A populated grid is never rendered until a
    /// caller writes real samples, so this sentinel cannot masquerade as a
    /// distant horizon.
    pub const EMPTY: Self = Self {
        terrain_y: 0,
        water_y: Self::DRY,
        surface_rgb565: 0,
        flags: 0,
    };
}

/// World-aligned tile coordinate, measured in [`HORIZON_TILE_BLOCKS`] blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HorizonTileCoord {
    /// Tile coordinate along world +X.
    pub x: i32,
    /// Tile coordinate along world +Z.
    pub z: i32,
}

impl HorizonTileCoord {
    /// Finds the tile containing a world-space block coordinate.
    #[must_use]
    pub const fn containing_block(x: i32, z: i32) -> Self {
        Self {
            x: x.div_euclid(HORIZON_TILE_BLOCKS),
            z: z.div_euclid(HORIZON_TILE_BLOCKS),
        }
    }

    /// World-space block origin of this tile.
    #[must_use]
    pub const fn block_origin(self) -> (i32, i32) {
        (
            self.x.saturating_mul(HORIZON_TILE_BLOCKS),
            self.z.saturating_mul(HORIZON_TILE_BLOCKS),
        )
    }
}

/// A fixed-size tile and the world coordinate it currently represents.
#[derive(Debug)]
pub struct HorizonTile {
    coord: HorizonTileCoord,
    cells: Box<[HorizonCell]>,
}

impl HorizonTile {
    fn empty(coord: HorizonTileCoord) -> Result<Self, HorizonAllocationError> {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(HORIZON_CELLS_PER_TILE)
            .map_err(|_| HorizonAllocationError)?;
        cells.resize(HORIZON_CELLS_PER_TILE, HorizonCell::EMPTY);
        Ok(Self {
            coord,
            cells: cells.into_boxed_slice(),
        })
    }

    /// This tile's world coordinate.
    #[must_use]
    pub const fn coord(&self) -> HorizonTileCoord {
        self.coord
    }

    /// Reads one cell at tile-local coordinates, or `None` outside the tile.
    #[must_use]
    pub fn cell(&self, x: usize, z: usize) -> Option<HorizonCell> {
        (x < HORIZON_TILE_CELLS && z < HORIZON_TILE_CELLS)
            .then(|| self.cells[z * HORIZON_TILE_CELLS + x])
    }

    /// The tile's fixed row-major samples.
    ///
    /// The coarse GPU atlas copies one tile at a time from this slice, avoiding
    /// a second retained full-horizon staging allocation.
    #[must_use]
    pub fn cells(&self) -> &[HorizonCell] {
        &self.cells
    }

    /// Replaces one cell at tile-local coordinates, returning whether it was in
    /// range. The narrow return avoids panics from an untrusted tile payload.
    pub fn set_cell(&mut self, x: usize, z: usize, cell: HorizonCell) -> bool {
        if x >= HORIZON_TILE_CELLS || z >= HORIZON_TILE_CELLS {
            return false;
        }
        self.cells[z * HORIZON_TILE_CELLS + x] = cell;
        true
    }
}

/// The first bounded coarse distant-terrain tier, centred on one camera tile.
///
/// Its 9×9 tile square has a fixed 2.53 MiB CPU ceiling. Recentring replaces
/// coordinates but does not resize the backing vector; GPU upload and drawing
/// are intentionally a later shell-owned layer.
#[derive(Debug)]
pub struct DistantTerrain {
    centre: HorizonTileCoord,
    tiles: Vec<HorizonTile>,
}

/// Allocation failed before the horizon could reach a partially constructed
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizonAllocationError;

impl std::fmt::Display for HorizonAllocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unable to allocate the bounded distant-terrain grid")
    }
}

impl std::error::Error for HorizonAllocationError {}

impl DistantTerrain {
    /// Allocates the fixed grid around a camera at world block `(x, z)`.
    pub fn new(camera_x: i32, camera_z: i32) -> Result<Self, HorizonAllocationError> {
        Self::around(HorizonTileCoord::containing_block(camera_x, camera_z))
    }

    fn around(centre: HorizonTileCoord) -> Result<Self, HorizonAllocationError> {
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(MAX_HORIZON_TILES)
            .map_err(|_| HorizonAllocationError)?;
        for z in -HORIZON_TILE_RADIUS..=HORIZON_TILE_RADIUS {
            for x in -HORIZON_TILE_RADIUS..=HORIZON_TILE_RADIUS {
                tiles.push(HorizonTile::empty(HorizonTileCoord {
                    x: centre.x.saturating_add(x),
                    z: centre.z.saturating_add(z),
                })?);
            }
        }
        debug_assert_eq!(tiles.len(), MAX_HORIZON_TILES);
        Ok(Self { centre, tiles })
    }

    /// The tile containing the camera when this grid was built or recentered.
    #[must_use]
    pub const fn centre(&self) -> HorizonTileCoord {
        self.centre
    }

    /// The number of allocated tiles. Always [`MAX_HORIZON_TILES`].
    #[must_use]
    pub const fn tile_count(&self) -> usize {
        MAX_HORIZON_TILES
    }

    /// Iterates the fixed square in stable row-major order.
    pub fn tiles(&self) -> impl ExactSizeIterator<Item = &HorizonTile> {
        self.tiles.iter()
    }

    /// Iterates the fixed square mutably in the same stable row-major order as
    /// [`Self::tiles`].
    ///
    /// The GPU bridge fills one tile at a time and must not retain a second
    /// copy of all horizon cells merely to make them writable.
    pub fn tiles_mut(&mut self) -> impl ExactSizeIterator<Item = &mut HorizonTile> {
        self.tiles.iter_mut()
    }

    /// Reassigns every tile to the fixed square around the new camera tile and
    /// clears its samples. Callers refill lazily; no allocation grows with a
    /// long walk across the world.
    pub fn recenter(&mut self, camera_x: i32, camera_z: i32) {
        let centre = HorizonTileCoord::containing_block(camera_x, camera_z);
        if centre == self.centre {
            return;
        }
        self.centre = centre;
        for (index, tile) in self.tiles.iter_mut().enumerate() {
            let x = index % HORIZON_TILES_PER_AXIS;
            let z = index / HORIZON_TILES_PER_AXIS;
            tile.coord = HorizonTileCoord {
                x: centre.x.saturating_add(x as i32 - HORIZON_TILE_RADIUS),
                z: centre.z.saturating_add(z as i32 - HORIZON_TILE_RADIUS),
            };
            tile.cells.fill(HorizonCell::EMPTY);
        }
    }
}

/// WGSL source for the future vertex-pulled horizon pass.
///
/// Kept in its own `.wgsl` file so the shader-validation gate sees it even
/// before the shell owns a pipeline object.
pub const DISTANT_TERRAIN_WGSL: &str = include_str!("shaders/lod_terrain.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_tier_has_a_fixed_small_allocation_ceiling() {
        assert_eq!(std::mem::size_of::<HorizonCell>(), 8);
        assert_eq!(HORIZON_TILES_PER_AXIS, 9);
        assert_eq!(MAX_HORIZON_TILES, 81);
        assert_eq!(MAX_HORIZON_CELLS, 331_776);
        assert_eq!(MAX_HORIZON_BYTES, 2_654_208);
        assert!(
            MAX_HORIZON_BYTES < 3 * 1024 * 1024,
            "the coarse 256-chunk tier must stay below 3 MiB, got {MAX_HORIZON_BYTES} bytes"
        );
    }

    #[test]
    fn world_coordinates_use_floor_tiles_across_negative_boundaries() {
        assert_eq!(HorizonTileCoord::containing_block(0, 0), HorizonTileCoord { x: 0, z: 0 });
        assert_eq!(
            HorizonTileCoord::containing_block(-1, -1),
            HorizonTileCoord { x: -1, z: -1 },
            "negative world coordinates must not truncate into tile zero"
        );
        assert_eq!(
            HorizonTileCoord::containing_block(HORIZON_TILE_BLOCKS - 1, HORIZON_TILE_BLOCKS),
            HorizonTileCoord { x: 0, z: 1 }
        );
    }

    #[test]
    fn recentering_preserves_the_fixed_tile_count_and_reassigns_the_full_square() {
        let mut terrain = DistantTerrain::new(-1, HORIZON_TILE_BLOCKS + 1).expect("bounded grid");
        assert_eq!(terrain.centre(), HorizonTileCoord { x: -1, z: 1 });
        assert_eq!(terrain.tiles().len(), MAX_HORIZON_TILES);
        terrain.recenter(4 * HORIZON_TILE_BLOCKS, -3 * HORIZON_TILE_BLOCKS);
        assert_eq!(terrain.centre(), HorizonTileCoord { x: 4, z: -3 });
        let coords: Vec<_> = terrain.tiles().map(HorizonTile::coord).collect();
        assert_eq!(coords.first(), Some(&HorizonTileCoord { x: 0, z: -7 }));
        assert_eq!(coords.last(), Some(&HorizonTileCoord { x: 8, z: 1 }));
        assert_eq!(coords.len(), MAX_HORIZON_TILES);
    }

    #[test]
    fn out_of_range_writes_do_not_alias_the_last_cell() {
        let mut terrain = DistantTerrain::new(0, 0).expect("bounded grid");
        let tile = terrain.tiles.get_mut(0).expect("first tile");
        let last = HorizonCell {
            terrain_y: 123,
            ..HorizonCell::EMPTY
        };
        assert!(tile.set_cell(HORIZON_TILE_CELLS - 1, HORIZON_TILE_CELLS - 1, last));
        assert!(!tile.set_cell(HORIZON_TILE_CELLS, HORIZON_TILE_CELLS - 1, HorizonCell::EMPTY));
        assert_eq!(
            tile.cell(HORIZON_TILE_CELLS - 1, HORIZON_TILE_CELLS - 1),
            Some(last),
            "control: an out-of-range write must not overwrite a valid edge sample"
        );
    }
}
