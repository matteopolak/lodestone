//! Terrain source for the integrated server.
//!
//! A [`ChunkSource`] answers "what blocks are in column `(cx, cz)`?".
//! [`WorldgenChunkSource`] implements it from the version-free density-function
//! noise router in [`lodestone_worldgen`].
//!
//! # Honest scope of [`WorldgenChunkSource`]
//!
//! It samples the router's `final_density` per block and treats `> 0` as solid.
//! This is the **noise-router terrain-shape stage only**. Two deliberate,
//! documented gaps remain before this equals a vanilla chunk block-for-block:
//!
//! * **Cell interpolation.** Vanilla evaluates `final_density` only at cell
//!   corners (cells are 4×8×4 blocks for the overworld) and trilinearly
//!   interpolates the interior; this source point-samples every block instead.
//!   The interpreter tree is proven bit-exact at points (see
//!   `lodestone-worldgen`'s `density_parity` test), but per-block interpolation
//!   is a separate stage not yet implemented here.
//! * **Surface rules, aquifers, carvers, features.** None are applied, so the
//!   column is `default_block`/air only — no grass, water table, caves, or ores.
//!
//! The type is therefore useful as a real, self-contained terrain-shape source
//! and as the integration point the remaining stages will slot into, but it is
//! not represented as vanilla-accurate blocks.

use lodestone_worldgen::density::{Context, Density};

/// A decoded chunk column: solid/non-solid for every block in a 16×`height`×16
/// prism whose bottom is at `min_y`.
#[derive(Debug, Clone)]
pub struct ChunkColumn {
    /// World Y of the lowest block row.
    pub min_y: i32,
    /// Number of block rows (world height).
    pub height: i32,
    /// `solid[(y * 16 + z) * 16 + x]`, `x`/`z` in `0..16`, `y` in `0..height`.
    solid: Vec<bool>,
}

impl ChunkColumn {
    /// Creates an all-air column of the given vertical extent.
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        assert!(height > 0, "height must be positive");
        Self {
            min_y,
            height,
            solid: vec![false; 16 * 16 * height as usize],
        }
    }

    #[inline]
    fn index(&self, x: i32, y_local: i32, z: i32) -> usize {
        debug_assert!((0..16).contains(&x));
        debug_assert!((0..16).contains(&z));
        debug_assert!((0..self.height).contains(&y_local));
        ((y_local * 16 + z) * 16 + x) as usize
    }

    /// Sets solidity at a local `(x, z)` in `0..16` and world `y`.
    pub fn set_solid(&mut self, x: i32, y: i32, z: i32, solid: bool) {
        let y_local = y - self.min_y;
        let i = self.index(x, y_local, z);
        self.solid[i] = solid;
    }

    /// Returns solidity at a local `(x, z)` in `0..16` and world `y`. Blocks
    /// outside the vertical range are non-solid.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return false;
        }
        self.solid[self.index(x, y_local, z)]
    }

    /// Total number of solid blocks (useful for tests/telemetry).
    #[must_use]
    pub fn solid_count(&self) -> usize {
        self.solid.iter().filter(|s| **s).count()
    }
}

/// Supplies terrain columns to the integrated server.
pub trait ChunkSource: Send + Sync {
    /// Generates the column at chunk coordinates `(cx, cz)`.
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn;
}

/// A [`ChunkSource`] backed by the density-function noise router.
///
/// Build the router `final_density` once (via
/// [`lodestone_worldgen::density::Builder`]) and hand it here; the source then
/// point-samples it per block. See the module docs for the scope limits.
#[derive(Debug, Clone)]
pub struct WorldgenChunkSource {
    final_density: Density,
    min_y: i32,
    height: i32,
}

impl WorldgenChunkSource {
    /// Wraps a pre-built `final_density` node with the world's vertical extent.
    #[must_use]
    pub fn new(final_density: Density, min_y: i32, height: i32) -> Self {
        Self {
            final_density,
            min_y,
            height,
        }
    }
}

impl ChunkSource for WorldgenChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(self.min_y, self.height);
        let base_x = cx * 16;
        let base_z = cz * 16;
        for lx in 0..16 {
            for lz in 0..16 {
                let wx = base_x + lx;
                let wz = base_z + lz;
                for ly in 0..self.height {
                    let wy = self.min_y + ly;
                    let d = self.final_density.compute(Context::new(wx, wy, wz));
                    if d > 0.0 {
                        col.set_solid(lx, wy, lz, true);
                    }
                }
            }
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `y_clamped_gradient` that is positive below y=0 and negative above acts
    /// as a flat solid floor, letting us verify the sign-field logic with no
    /// external data.
    fn floor_density() -> Density {
        Density::YClampedGradient {
            from_y: -64.0,
            to_y: 64.0,
            from_value: 1.0,
            to_value: -1.0,
        }
    }

    #[test]
    fn worldgen_source_maps_positive_density_to_solid() {
        let src = WorldgenChunkSource::new(floor_density(), -64, 128);
        let col = src.column(0, 0);
        // Deep down (y = -64) density is +1 → solid; high up (y = 63) it is
        // near -1 → air. The crossover is y = 0.
        assert!(col.is_solid(0, -64, 0));
        assert!(col.is_solid(5, -1, 9));
        assert!(!col.is_solid(0, 0, 0));
        assert!(!col.is_solid(5, 40, 9));
        // Every one of the 16×16 columns is solid for exactly y in [-64, -1].
        assert_eq!(col.solid_count(), 16 * 16 * 64);
    }

    #[test]
    fn out_of_range_is_air() {
        let src = WorldgenChunkSource::new(floor_density(), -64, 128);
        let col = src.column(1, -3);
        assert!(!col.is_solid(0, 5000, 0));
        assert!(!col.is_solid(0, -5000, 0));
    }
}
