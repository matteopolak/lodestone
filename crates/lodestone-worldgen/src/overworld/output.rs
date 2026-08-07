//! The read-mostly result types [`OverworldGenerator::column`] hands back:
//! [`GeneratedColumn`] (the interned block field) and [`StageTimes`] (the per-stage
//! split `column_timed` measures), plus the adopt-the-dense-grid step between them.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A.

use super::OverworldGenerator;

impl OverworldGenerator {
    /// Adopts the final (post-carve, post-ore) dense world grid straight into
    /// a [`GeneratedColumn`] — no re-intern pass (issue #295's Job 2): a
    /// centre-chunk-sized [`crate::dense_grid::DenseBlockGrid`]'s own
    /// `(palette, blocks)` layout is already identical to
    /// [`GeneratedColumn`]'s (`((ly * 16 + lz) * 16 + lx)`, verified by the
    /// `debug_assert!` below rather than merely asserted in a doc comment).
    pub(super) fn intern_from_dense(
        &self,
        world: crate::dense_grid::DenseBlockGrid,
        biome_quarts: [(String, bool); 16],
    ) -> GeneratedColumn {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Intern);
        debug_assert_eq!(world.bounds().3, 16, "centre chunk width must be 16");
        debug_assert_eq!(world.bounds().4, self.height, "centre chunk height must match the generator's");
        debug_assert_eq!(world.bounds().5, 16, "centre chunk depth must be 16");
        let (palette, blocks) = world.into_palette_and_blocks();

        GeneratedColumn {
            min_y: self.min_y,
            height: self.height,
            palette,
            blocks,
            biome_quarts: biome_quarts.map(|(name, _)| name),
        }
    }
}

/// Per-stage wall-clock cost of one [`OverworldGenerator::column_timed`] call:
/// **one field per stage the pipeline actually has**, which is what issue #85
/// asks for.
///
/// # What changed here, and why the old field names were misleading
///
/// This struct previously had four buckets — `shape` / `fluid_heightmap` /
/// `surface` / `intern` — and two of the four did not measure what their name
/// said:
///
/// * **`fluid_heightmap` measured the biome stage and nothing else.** The
///   heightmap (`heights_from_field`) is computed inside the `shape` window, so
///   the bucket between the two timestamps contained exactly
///   [`OverworldGenerator::biome_stage`]. It is now called `biome`.
/// * **`intern` measured materialize + carve + ore + vegetation + interning.**
///   The old doc comment did admit the folding, but the recorded benchmark
///   metric was named `stage_intern_pct`, so the persisted number attributed
///   carvers, ore features and vegetal decoration to "interning" — the precise
///   rot #85 exists to stop. Those four now have their own fields.
///
/// So a `stage_intern_pct` figure in `bench-results/generation.jsonl` recorded
/// before this change is **not** interning cost and must not be compared
/// against `stage_intern_pct` recorded after it. The scene strings differ, which
/// is what stops `cargo xtask bench-compare` from silently pairing them.
///
/// # These are cache-cold stage costs, deliberately
///
/// [`OverworldGenerator::column`] reaches its stages through two memo caches
/// ([`OverworldGenerator::pre_ore_stage`] and `post_ore_world`).
/// `column_timed` calls the same stage functions *without* those caches, on
/// purpose: a cache hit costs almost nothing, so a per-stage split taken over
/// memoised calls would attribute ~0% to whichever stage happened to be warm
/// and is not a split of anything. The stage functions, their order and their
/// inputs are identical to `column`'s, so the *output* is identical too —
/// `benches/generation.rs` asserts exactly that against a pair of freshly
/// constructed generators, which is the anti-drift control the "do not create a
/// second pipeline" half of #85 asks for.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
pub struct StageTimes {
    /// Building the per-chunk [`crate::aquifer::AquiferSystem`] (issue #295) —
    /// split out of `shape` because #85 names the aquifer as a stage whose cost
    /// should be visible on its own terms.
    pub aquifer: std::time::Duration,
    /// The noise router: `fill_stage` (density field, aquifer-participating
    /// fill) plus `heights_from_field`.
    pub shape: std::time::Duration,
    /// Biome sampling (issue #405's [`OverworldGenerator::biome_stage`]).
    /// Previously, and misleadingly, called `fluid_heightmap`.
    pub biome: std::time::Duration,
    /// Surface rules.
    pub surface: std::time::Duration,
    /// Turning the density field plus the surface diff into a dense block grid.
    pub materialize: std::time::Duration,
    /// Carvers (`crate::carver::apply_carvers`, issue #295).
    pub carve: std::time::Duration,
    /// The `UNDERGROUND_ORES` 3×3 neighbourhood driver (issue #295).
    pub ore: std::time::Duration,
    /// Vegetal decoration (issue #406).
    pub vegetation: std::time::Duration,
    /// `TOP_LAYER_MODIFICATION` — `freeze_top_layer`'s snow and ice (issue
    /// #404's U2). The first stage to earn its own field rather than being
    /// folded into `intern`, because `docs/plans/worldgen-parity.md` §6 makes a
    /// *quantitative* prediction about it (<5% of composed column cost) that
    /// something has to be able to check.
    pub top_layer: std::time::Duration,
    /// Palette interning only — nothing else, now.
    pub intern: std::time::Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl StageTimes {
    /// Total of every stage (wall-clock, so approximately equal to but not
    /// exactly the same instant range as timing the whole `column()` call).
    ///
    /// Every field is included, so this covers the same span it did when the
    /// struct had four buckets — splitting a bucket does not move the total, and
    /// existing callers that divide one stage by `total()` (e.g.
    /// `lodestone-server`'s `top_layer` share assertion) keep the same meaning.
    #[must_use]
    pub fn total(&self) -> std::time::Duration {
        self.aquifer
            + self.shape
            + self.biome
            + self.surface
            + self.materialize
            + self.carve
            + self.ore
            + self.vegetation
            + self.top_layer
            + self.intern
    }
}

/// A generated 16×`height`×16 block field, block-state strings interned into a
/// small per-column palette.
#[derive(Debug, Clone)]
pub struct GeneratedColumn {
    min_y: i32,
    height: i32,
    palette: Vec<String>,
    blocks: Vec<u16>,
    /// Biome id per horizontal quart, row-major `qz * 4 + qx` (issue #405) —
    /// see [`OverworldGenerator::biome_stage`]. Broadcast vertically: the
    /// whole column shares these 16 values regardless of `y`.
    biome_quarts: [String; 16],
}

impl GeneratedColumn {
    /// World Y of the lowest block row.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows.
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Canonical block-state string at local `(lx, lz)` in `0..16` and world `y`.
    /// Out-of-range Y is `"minecraft:air"`.
    #[must_use]
    pub fn block_state(&self, lx: usize, y: i32, lz: usize) -> &str {
        let ly = y - self.min_y;
        if !(0..self.height).contains(&ly) {
            return "minecraft:air";
        }
        let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
        &self.palette[self.blocks[idx] as usize]
    }

    /// Highest world Y whose block is not air, or `min_y - 1` for an all-air
    /// column. Water counts as non-air (matching `WORLD_SURFACE_WG`).
    #[must_use]
    pub fn top_non_air_y(&self, lx: usize, lz: usize) -> i32 {
        for ly in (0..self.height).rev() {
            let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
            if self.blocks[idx] != 0 {
                return self.min_y + ly;
            }
        }
        self.min_y - 1
    }

    /// Number of non-air blocks (telemetry / anti-vacuity).
    #[must_use]
    pub fn non_air_count(&self) -> usize {
        self.blocks.iter().filter(|b| **b != 0).count()
    }

    /// Biome id at local `(lx, lz)` in `0..16` (issue #405) — quart
    /// resolution, broadcast vertically (see [`OverworldGenerator::biome_stage`]),
    /// so the same answer comes back for every `y` at this `(lx, lz)`.
    ///
    /// # Panics
    /// Panics if `lx`/`lz` are not in `0..16`.
    #[must_use]
    pub fn biome_state(&self, lx: usize, lz: usize) -> &str {
        assert!(lx < 16 && lz < 16, "biome_state coordinates out of range");
        &self.biome_quarts[(lz >> 2) * 4 + (lx >> 2)]
    }

    /// Distinct biome count in this column (telemetry / anti-vacuity — a
    /// chunk straddling a biome boundary should report more than one).
    #[must_use]
    pub fn distinct_biome_count(&self) -> usize {
        let mut seen: Vec<&str> = Vec::with_capacity(16);
        for name in &self.biome_quarts {
            if !seen.contains(&name.as_str()) {
                seen.push(name.as_str());
            }
        }
        seen.len()
    }

    /// Consumes the column into its raw parts: `(min_y, height, palette,
    /// blocks, biome_quarts)`, where `blocks[(ly * 16 + lz) * 16 + lx]`
    /// indexes into `palette` (`palette[0] == "minecraft:air"`), `ly = y -
    /// min_y`, and `biome_quarts[qz * 4 + qx]` is this column's biome id for
    /// horizontal quart `(qx, qz)` (issue #405), constant across `y`.
    ///
    /// This is the zero-copy hand-off a downstream carrier (e.g. the integrated
    /// server's chunk column) uses to adopt the generated block field without
    /// re-interning every block. The index layout is stable and part of the
    /// contract.
    #[must_use]
    pub fn into_raw(self) -> (i32, i32, Vec<String>, Vec<u16>, [String; 16]) {
        (
            self.min_y,
            self.height,
            self.palette,
            self.blocks,
            self.biome_quarts,
        )
    }
}
