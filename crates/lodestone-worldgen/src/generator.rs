//! The plugin-facing chunk generation seam.
//!
//! # The decision
//!
//! A plugin generator cannot live behind the same trait as the verified
//! overworld/Nether/End pipelines without accepting that its output carries
//! no oracle guarantee — that is exactly Bukkit's own `ChunkGenerator`
//! contract (a plugin fully replaces vanilla terrain for its world, with no
//! correctness check from the platform). [`ChunkGenerator`] is that seam:
//! **`dyn`-dispatched, called imperatively from plain functions** (never
//! installed as a bevy `System` — `docs/bevy-migration.md` §8's rule that
//! verified worldgen math must never sit behind a scheduler is unaffected,
//! because nothing here is a system).
//!
//! # Why the output type is [`DenseBlockGrid`], not [`crate::overworld::GeneratedColumn`]
//!
//! `GeneratedColumn` carries data a demo/plugin generator has no business
//! producing — a 4×4×4 biome grid, generation-time block entities, a
//! `MOTION_BLOCKING` heightmap snapshot, stage timings — and forcing a
//! plugin to fill all of that honestly would make the simplest possible
//! generator (a flat floor, a checkerboard) carry fields it cannot
//! meaningfully answer. [`DenseBlockGrid`] is this crate's own
//! already-existing "dense block field over a box" vocabulary (see that
//! module's doc), used internally by every real generator's composition
//! stage — so a plugin generator speaks the same shape the engine already
//! converges on, not a new one invented for this trait.
//!
//! # The native proof
//!
//! [`crate::flat::FlatLevelSource`] — a real, jar-verified generator already
//! serving vanilla superflat/void worlds — implements [`ChunkGenerator`] too
//! (see the `impl` below), so this is one dispatch point serving both a
//! verified native generator and an unverified plugin one, not two parallel
//! systems that happen to look similar.

use std::sync::Arc;

use crate::dense_grid::DenseBlockGrid;

/// A source of chunk terrain, dispatched as `Arc<dyn ChunkGenerator>` from
/// `lodestone-server`'s plugin-facing chunk source (see
/// `docs/plugin-worldgen-api.md`).
///
/// Object-safe on purpose: this is the whole point of the seam. A generator
/// answers one 16×`height()`×16 column at a time, addressed by chunk
/// coordinates — the same granularity every native generator's own `column`
/// method uses.
pub trait ChunkGenerator: Send + Sync {
    /// World Y of the lowest block row this generator's columns contain.
    fn min_y(&self) -> i32;

    /// Number of block rows (world height) this generator's columns contain.
    fn height(&self) -> i32;

    /// Generates the column at chunk coordinates `(cx, cz)`.
    ///
    /// The returned grid's box must cover exactly
    /// `[cx*16, cx*16+16) × [min_y(), min_y()+height()) × [cz*16, cz*16+16)`
    /// — a caller reads it at those world coordinates and a mismatched box
    /// silently reads back air (see [`DenseBlockGrid`]'s own "outside the
    /// box" contract), not a panic, so getting this wrong is a quiet bug
    /// rather than a loud one. `docs/plugin-worldgen-api.md` shows the
    /// one-line constructor call that gets the box right.
    fn generate(&self, cx: i32, cz: i32) -> DenseBlockGrid;

    /// The biome id this generator reports for every column — uniform across
    /// the whole world, matching vanilla's own `FixedBiomeSource` (the same
    /// fallback [`crate::overworld::OverworldGenerator`] takes when no real
    /// biome-parameter table is supplied — see that constructor's doc).
    ///
    /// A demo/plugin generator has no obligation to vary this per column; the
    /// default is vanilla's own biome-decoration/mob-spawn fallback.
    fn biome(&self) -> &str {
        "minecraft:plains"
    }
}

impl ChunkGenerator for crate::flat::FlatLevelSource {
    fn min_y(&self) -> i32 {
        crate::flat::FlatLevelSource::min_y(self)
    }

    fn height(&self) -> i32 {
        crate::flat::FlatLevelSource::height(self)
    }

    /// Bridges [`crate::flat::FlatColumn`] (one Y-indexed row list, identical
    /// at every `(x, z)` — see that type's own doc) into a per-chunk
    /// [`DenseBlockGrid`] by broadcasting each non-air row across the full
    /// 16×16 footprint.
    fn generate(&self, cx: i32, cz: i32) -> DenseBlockGrid {
        let column = crate::flat::FlatLevelSource::column(self, cx, cz);
        let min_y = crate::flat::FlatLevelSource::min_y(self);
        let height = crate::flat::FlatLevelSource::height(self);
        let mut grid = DenseBlockGrid::new(
            cx * 16,
            min_y,
            cz * 16,
            16,
            height,
            16,
            "minecraft:air",
        );
        for ly in 0..height {
            let y = min_y + ly;
            let state = column.block_state(y);
            if state == "minecraft:air" {
                continue;
            }
            for lz in 0..16 {
                for lx in 0..16 {
                    grid.set(cx * 16 + lx, y, cz * 16 + lz, state);
                }
            }
        }
        grid
    }

    fn biome(&self) -> &str {
        &self.settings().biome
    }
}

/// A boxed generator, the shape `lodestone-server`'s plugin chunk source and
/// dimension registry hold — spelled out once here so a caller does not have
/// to write `Arc<dyn ChunkGenerator>` at every call site.
pub type BoxedChunkGenerator = Arc<dyn ChunkGenerator>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flat::{FlatLayer, FlatLevelGeneratorSettings, FlatLevelSource, StructureOverrides};

    fn settings() -> FlatLevelGeneratorSettings {
        FlatLevelGeneratorSettings {
            biome: "minecraft:desert".to_string(),
            features: false,
            lakes: false,
            layers: vec![
                FlatLayer {
                    block: "minecraft:bedrock".to_string(),
                    height: 1,
                },
                FlatLayer {
                    block: "minecraft:stone".to_string(),
                    height: 2,
                },
                FlatLayer {
                    block: "minecraft:grass_block".to_string(),
                    height: 1,
                },
            ],
            structure_overrides: StructureOverrides::Default,
        }
    }

    #[test]
    fn flat_level_source_implements_chunk_generator_and_matches_its_own_column() {
        let source = FlatLevelSource::new(settings(), -64, 384);
        let generator: &dyn ChunkGenerator = &source;

        assert_eq!(generator.min_y(), -64);
        assert_eq!(generator.height(), 384);
        assert_eq!(generator.biome(), "minecraft:desert");

        let grid = generator.generate(2, -3);
        assert_eq!(grid.get(2 * 16 + 5, -64, -3 * 16 + 9), "minecraft:bedrock");
        assert_eq!(grid.get(2 * 16 + 5, -63, -3 * 16 + 9), "minecraft:stone");
        assert_eq!(grid.get(2 * 16 + 5, -62, -3 * 16 + 9), "minecraft:stone");
        assert_eq!(
            grid.get(2 * 16 + 5, -61, -3 * 16 + 9),
            "minecraft:grass_block[snowy=false]"
        );
        assert_eq!(grid.get(2 * 16 + 5, -60, -3 * 16 + 9), "minecraft:air");

        // Every cell in the chunk footprint carries the layer stack — a flat
        // world is uniform across x/z by definition (see `FlatColumn`'s doc).
        assert_eq!(grid.get(2 * 16 + 0, -64, -3 * 16 + 0), "minecraft:bedrock");
        assert_eq!(grid.get(2 * 16 + 15, -64, -3 * 16 + 15), "minecraft:bedrock");

        // Outside the chunk's own box reads back air (`DenseBlockGrid`'s
        // documented "outside the box" contract) — proof the generator
        // wrote the box the trait's own doc promises, not a wider one that
        // would happen to also answer a neighbouring chunk's coordinates.
        assert_eq!(grid.get(2 * 16 + 16, -64, -3 * 16 + 0), "minecraft:air");
    }

    #[test]
    fn deterministic_across_repeated_calls_at_the_same_coordinates() {
        let source = FlatLevelSource::new(settings(), -64, 384);
        let generator: &dyn ChunkGenerator = &source;
        let a = generator.generate(0, 0);
        let b = generator.generate(0, 0);
        assert_eq!(a.get(4, -64, 4), b.get(4, -64, 4));
        assert_eq!(a.get(4, -64, 4), "minecraft:bedrock");
    }
}
