//! Solidity-only chunk source used by transport and seam tests.
//!
//! [`WorldgenChunkSource`] deliberately point-samples a density node rather
//! than invoking the composed world-generation pipeline. Keeping this source
//! separate from the retained column and production dimension adapters makes
//! its intentionally limited semantics visible: it has no surface rules,
//! fluids, biome variation, or edit retention.

use lodestone_worldgen::density::{Context, Density};

use crate::chunk::{ChunkColumn, ChunkSource, AIR, STONE};

/// A solidity-only [`ChunkSource`] backed by a bare density node.
///
/// **Not the real generator** — see the module docs. It point-samples
/// `final_density` per block and maps `> 0` to stone, with no cell
/// interpolation, surface, or fluid. Kept for the in-memory-transport tests,
/// which need a deterministic terrain to prove the wire round-trip, not a
/// vanilla-accurate one. For real terrain use
/// [`crate::chunk::OverworldChunkSource`].
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

    // This source is solidity-only: one block is stone iff its density
    // sample is positive, mirroring `column()`'s `set_solid` rule exactly
    // (including air for any y outside the vertical extent). Point-sampling
    // the density node is cheaper than `column()` and gives the same answer,
    // so unlike the column-regenerating form this is the efficient read.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if !(self.min_y..self.min_y + self.height).contains(&y) {
            return AIR.to_string();
        }
        if self.final_density.compute(Context::new(x, y, z)) > 0.0 {
            STONE.to_string()
        } else {
            AIR.to_string()
        }
    }

    /// This source stamps no biome data of its own (a solidity-only
    /// transport-test source — see [`block_state`](Self::block_state)'s own
    /// doc), so every cell reads [`crate::chunk::DEFAULT_BIOME`] via
    /// [`ChunkColumn::new`]'s own default, through the one path that column
    /// actually exists on: `column()`, not a point-sampled shortcut like
    /// `block_state`'s (there is no density-shaped biome field to sample).
    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    /// This source has no per-column retention — every `column()` call
    /// regenerates fresh from the density node — so there is nowhere for an
    /// edit to live. That is a deliberate property of a solidity-only
    /// transport-test source, not a gap: reach for
    /// [`crate::chunk::OverworldChunkSource`] (or
    /// [`crate::region_source::RegionChunkSource`]) when edits must persist.
    /// Panics loudly rather than silently discarding the placement.
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let _ = (x, y, z, name);
        todo!("WorldgenChunkSource is a solidity-only, non-retaining source; it cannot accept a set_block edit");
    }
}
