//! Terrain source for the integrated server.
//!
//! A [`ChunkSource`] answers "what blocks are in column `(cx, cz)`?".
//!
//! Two implementations ship, and the distinction matters:
//!
//! * [`OverworldChunkSource`] is the **real** pipeline. It wraps
//!   [`lodestone_worldgen::overworld::OverworldGenerator`] — the composed,
//!   JVM-verified generator (interpolated `final_density` shape + sea-level
//!   aquifer + surface rules) — so its columns carry actual vanilla block-state
//!   strings (grass, dirt, stone, gravel, water, …), not a solid/air mask. This
//!   is the source a real client should be served, and the one the shell renders.
//! * [`WorldgenChunkSource`] is a **solidity-only** source kept for the
//!   transport/seam tests. It point-samples a bare [`Density`] node per block and
//!   maps `> 0` to stone — no cell interpolation, no surface, no fluid. It exists
//!   because the in-memory-transport tests only need *a* deterministic terrain to
//!   prove the wire round-trip, not a vanilla-accurate one. Do not reach for it
//!   as "the generator"; that is what [`OverworldChunkSource`] is.
//!
//! # The column carries block states, not just solidity
//!
//! [`ChunkColumn`] stores a per-column palette of block-state strings plus a
//! dense index grid (the same representation [`GeneratedColumn`] uses), so a
//! `ServerProtocol::encode_chunk` can emit a real chunk. The historical
//! solid/air API ([`ChunkColumn::set_solid`]/[`ChunkColumn::is_solid`]) is
//! preserved as a view over that field: a block is "solid" when it is neither air
//! nor a fluid, and `set_solid(true)` writes canonical stone.

use lodestone_worldgen::density::{Context, Density};
use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};

const AIR: &str = "minecraft:air";
const STONE: &str = "minecraft:stone";

/// Returns `true` for blocks that do not count as collidable terrain: air
/// variants and fluids. `is_solid` is the negation of this over the block name.
fn is_air_or_fluid(name: &str) -> bool {
    let base = name.split('[').next().unwrap_or(name);
    matches!(
        base,
        "minecraft:air"
            | "minecraft:cave_air"
            | "minecraft:void_air"
            | "minecraft:water"
            | "minecraft:lava"
    )
}

/// A decoded chunk column: the block state of every block in a 16×`height`×16
/// prism whose bottom is at `min_y`.
///
/// Blocks are stored as indices into a small per-column `palette` of block-state
/// strings, with `palette[0] == "minecraft:air"`. The index layout matches
/// [`GeneratedColumn`] exactly (`blocks[(ly * 16 + z) * 16 + x]`, `ly = y -
/// min_y`) so [`ChunkColumn::from_generated`] is a zero-copy adoption.
#[derive(Debug, Clone)]
pub struct ChunkColumn {
    /// World Y of the lowest block row.
    pub min_y: i32,
    /// Number of block rows (world height).
    pub height: i32,
    /// Block-state palette; `palette[0]` is always `"minecraft:air"`.
    palette: Vec<String>,
    /// `blocks[(y_local * 16 + z) * 16 + x]` indexes into `palette`.
    blocks: Vec<u16>,
}

impl ChunkColumn {
    /// Creates an all-air column of the given vertical extent.
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        assert!(height > 0, "height must be positive");
        Self {
            min_y,
            height,
            palette: vec![AIR.to_string()],
            blocks: vec![0u16; 16 * 16 * height as usize],
        }
    }

    /// Adopts a [`GeneratedColumn`] from the real worldgen pipeline. Zero-copy:
    /// the palette and block grid are moved as-is (their index layout is the
    /// same).
    #[must_use]
    pub fn from_generated(column: GeneratedColumn) -> Self {
        let (min_y, height, palette, blocks) = column.into_raw();
        debug_assert_eq!(
            palette.first().map(String::as_str),
            Some(AIR),
            "generated palette must start with air"
        );
        Self {
            min_y,
            height,
            palette,
            blocks,
        }
    }

    #[inline]
    fn index(&self, x: i32, y_local: i32, z: i32) -> usize {
        debug_assert!((0..16).contains(&x));
        debug_assert!((0..16).contains(&z));
        debug_assert!((0..self.height).contains(&y_local));
        ((y_local * 16 + z) * 16 + x) as usize
    }

    /// Interns a block-state string into the palette, returning its index.
    fn intern(&mut self, name: &str) -> u16 {
        if let Some(i) = self.palette.iter().position(|p| p == name) {
            return i as u16;
        }
        self.palette.push(name.to_string());
        (self.palette.len() - 1) as u16
    }

    /// Sets the block state at a local `(x, z)` in `0..16` and world `y`.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) {
        let id = self.intern(name);
        let y_local = y - self.min_y;
        let i = self.index(x, y_local, z);
        self.blocks[i] = id;
    }

    /// Sets solidity at a local `(x, z)` in `0..16` and world `y`. `true` writes
    /// canonical stone, `false` writes air — the solid/air view preserved for
    /// callers that only reason about collidable terrain.
    pub fn set_solid(&mut self, x: i32, y: i32, z: i32, solid: bool) {
        self.set_block(x, y, z, if solid { STONE } else { AIR });
    }

    /// Canonical block-state string at a local `(x, z)` in `0..16` and world `y`.
    /// Out-of-range Y is `"minecraft:air"`.
    #[must_use]
    pub fn block_state(&self, x: i32, y: i32, z: i32) -> &str {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return AIR;
        }
        &self.palette[self.blocks[self.index(x, y_local, z)] as usize]
    }

    /// Returns solidity at a local `(x, z)` in `0..16` and world `y`. A block is
    /// solid when it is neither air nor a fluid; blocks outside the vertical
    /// range are non-solid.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        !is_air_or_fluid(self.block_state(x, y, z))
    }

    /// Total number of solid (non-air, non-fluid) blocks.
    #[must_use]
    pub fn solid_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|&&id| !is_air_or_fluid(&self.palette[id as usize]))
            .count()
    }
}

/// Supplies terrain columns to the integrated server.
pub trait ChunkSource: Send + Sync {
    /// Generates the column at chunk coordinates `(cx, cz)`.
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn;
}

/// The real terrain source: the composed, JVM-verified overworld generator.
///
/// This is what a client connecting to the integrated server should be served —
/// its columns carry real vanilla block states (shape + sea-level aquifer +
/// surface rules), the same output the shell renders directly. Build one per
/// world (via [`crate::overworld_chunk_source`]) and share it across the view.
pub struct OverworldChunkSource {
    generator: OverworldGenerator,
}

impl OverworldChunkSource {
    /// Wraps a pre-built [`OverworldGenerator`].
    #[must_use]
    pub fn new(generator: OverworldGenerator) -> Self {
        Self { generator }
    }
}

impl std::fmt::Debug for OverworldChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverworldChunkSource")
            .finish_non_exhaustive()
    }
}

impl ChunkSource for OverworldChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        ChunkColumn::from_generated(self.generator.column(cx, cz))
    }
}

/// A solidity-only [`ChunkSource`] backed by a bare density node.
///
/// **Not the real generator** — see the module docs. It point-samples
/// `final_density` per block and maps `> 0` to stone, with no cell
/// interpolation, surface, or fluid. Kept for the in-memory-transport tests,
/// which need a deterministic terrain to prove the wire round-trip, not a
/// vanilla-accurate one. For real terrain use [`OverworldChunkSource`].
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

    #[test]
    fn set_block_round_trips_and_fluids_are_not_solid() {
        let mut col = ChunkColumn::new(0, 16);
        col.set_block(3, 5, 7, "minecraft:grass_block[snowy=false]");
        col.set_block(3, 4, 7, "minecraft:water[level=0]");
        assert_eq!(
            col.block_state(3, 5, 7),
            "minecraft:grass_block[snowy=false]"
        );
        // Grass is solid; water is a fluid and therefore not solid.
        assert!(col.is_solid(3, 5, 7));
        assert!(!col.is_solid(3, 4, 7));
        // Only the grass block counts toward solidity.
        assert_eq!(col.solid_count(), 1);
    }
}
