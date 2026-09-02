//! Bridges a plugin's [`lodestone_worldgen::generator::ChunkGenerator`] into a
//! real [`ChunkSource`] — issue #132's decision made concrete, and the seam
//! #134's dimension registration and #136's structure placement both build
//! on. See `docs/plugin-worldgen-api.md` for the full design.
//!
//! # Why this file, not a method on `ChunkGenerator` itself
//!
//! `ChunkGenerator` lives in `lodestone-worldgen`, which is deliberately
//! version-free and knows nothing of [`ChunkColumn`] (a `lodestone-server`
//! type carrying the biome/structure/heightmap bookkeeping a served chunk
//! needs). [`PluginChunkSource`] is the one place that gap is crossed: it
//! calls the generator for a [`lodestone_worldgen::dense_grid::DenseBlockGrid`]
//! and adopts it into a real [`ChunkColumn`] the same way
//! [`OverworldChunkSource`](crate::chunk::OverworldChunkSource) adopts a
//! [`GeneratedColumn`](lodestone_worldgen::overworld::GeneratedColumn) —
//! through [`ChunkColumn`]'s own public constructor and `set_block`, not by
//! reaching into its private fields.
//!
//! # Edit retention matches every other `ChunkSource`
//!
//! Same policy as [`OverworldChunkSource`](crate::chunk::OverworldChunkSource):
//! an untouched column is regenerated from the plugin's generator on every
//! request (a plugin generator is expected to be cheap — a demo world's whole
//! point is that it need not be a verified pipeline), and only a column a
//! `set_block` has touched is retained.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_worldgen::generator::ChunkGenerator;

use crate::chunk::{ChunkColumn, ChunkSource};

/// A [`ChunkSource`] backed entirely by a plugin-supplied
/// [`ChunkGenerator`] — no oracle, no verification, exactly Bukkit's own
/// `ChunkGenerator` contract (see the module doc on
/// `lodestone_worldgen::generator` for why that is the accepted trade).
pub struct PluginChunkSource {
    generator: Arc<dyn ChunkGenerator>,
    edits: Mutex<HashMap<(i32, i32), ChunkColumn>>,
}

impl PluginChunkSource {
    /// Wraps a plugin's generator. Cheap: nothing is generated until a
    /// column is actually requested.
    #[must_use]
    pub fn new(generator: Arc<dyn ChunkGenerator>) -> Self {
        Self {
            generator,
            edits: Mutex::new(HashMap::new()),
        }
    }

    /// The wrapped generator, for a caller that needs to name it directly
    /// (a gate asserting the registry handed back the generator it was
    /// given, for one).
    #[must_use]
    pub fn generator(&self) -> &Arc<dyn ChunkGenerator> {
        &self.generator
    }

    /// Builds a fresh [`ChunkColumn`] from the generator's answer for
    /// `(cx, cz)` — the un-cached path every miss in [`Self::column`] and
    /// [`Self::set_block`] falls through to.
    fn generate_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let min_y = self.generator.min_y();
        let height = self.generator.height();
        let grid = self.generator.generate(cx, cz);
        let mut column = ChunkColumn::new(min_y, height);
        let biome = self.generator.biome();
        // Two separate biome answers this column carries (issue #512): the
        // 2-D "surface" quarts `biome_state`/`biome_quarts()` read, and the
        // full 3-D grid `biome_state_at`/`ChunkSource::biome_state_at` read.
        // A generator with one uniform biome fills both the same way, so
        // whichever a caller asks (surface tint vs. `/execute if biome` at a
        // real `y`) gets the same, correct answer rather than one of them
        // silently keeping `ChunkColumn::new`'s plains default.
        column.set_biome_quarts(&vec![biome.to_string(); 16]);
        for qy in 0..column.biome_y_quarts() {
            for qz in 0..4 {
                for qx in 0..4 {
                    column.set_biome_cell(qx, qy, qz, biome);
                }
            }
        }
        for ly in 0..height {
            let y = min_y + ly;
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let state = grid.get(cx * 16 + lx, y, cz * 16 + lz);
                    if state != "minecraft:air" {
                        column.set_block(lx, y, lz, state);
                    }
                }
            }
        }
        column
    }
}

impl std::fmt::Debug for PluginChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginChunkSource").finish_non_exhaustive()
    }
}

impl ChunkSource for PluginChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let edits = self.edits.lock().expect("plugin chunk edit cache lock poisoned");
        if let Some(edited) = edits.get(&(cx, cz)) {
            return edited.clone();
        }
        drop(edits);
        self.generate_column(cx, cz)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("plugin chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| self.generate_column(cx, cz));
        column.set_block(lx, y, lz, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_worldgen::dense_grid::DenseBlockGrid;

    /// A tiny checkerboard generator — stone on `(x+z)` even, air odd — used
    /// only to prove [`PluginChunkSource`]'s bridging is correct in
    /// isolation. The real end-to-end proof (a plugin generator driven
    /// through a real `IntegratedServer`) lives in
    /// `crates/plugins/lodestone-void-world`.
    struct Checkerboard;

    impl ChunkGenerator for Checkerboard {
        fn min_y(&self) -> i32 {
            0
        }
        fn height(&self) -> i32 {
            8
        }
        fn generate(&self, cx: i32, cz: i32) -> DenseBlockGrid {
            let mut grid = DenseBlockGrid::new(cx * 16, 0, cz * 16, 16, 8, 16, "minecraft:air");
            for lx in 0..16 {
                for lz in 0..16 {
                    let x = cx * 16 + lx;
                    let z = cz * 16 + lz;
                    if (x + z).rem_euclid(2) == 0 {
                        grid.set(x, 0, z, "minecraft:stone");
                    }
                }
            }
            grid
        }
        fn biome(&self) -> &str {
            "minecraft:the_void"
        }
    }

    #[test]
    fn column_reflects_the_generators_checkerboard() {
        let source = PluginChunkSource::new(Arc::new(Checkerboard));
        let column = source.column(0, 0);
        assert_eq!(column.block_state(0, 0, 0), "minecraft:stone");
        assert_eq!(column.block_state(1, 0, 0), "minecraft:air");
        assert_eq!(column.biome_state(0, 0), "minecraft:the_void");
    }

    #[test]
    fn block_state_reads_agree_with_column_reads() {
        let source = PluginChunkSource::new(Arc::new(Checkerboard));
        assert_eq!(source.block_state(0, 0, 0), "minecraft:stone");
        assert_eq!(source.block_state(1, 0, 0), "minecraft:air");
        assert_eq!(source.biome_state_at(3, 0, 3), "minecraft:the_void");
    }

    #[test]
    fn set_block_edits_are_retained_across_reads() {
        let source = PluginChunkSource::new(Arc::new(Checkerboard));
        assert_eq!(source.block_state(1, 0, 0), "minecraft:air");
        source.set_block(1, 0, 0, "minecraft:diamond_block");
        assert_eq!(source.block_state(1, 0, 0), "minecraft:diamond_block");
        // A neighbouring, untouched cell in the same edited column keeps the
        // generator's own answer — proof the edit did not blow away the rest
        // of the column.
        assert_eq!(source.block_state(0, 0, 0), "minecraft:stone");
    }
}
