//! Stage 2 of [`OverworldGenerator::column`]: multi-noise biome resolution — the
//! per-quart surface-height sample plus the separate `y = 0` per-source-chunk answer
//! carver and ore selection use.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A. See [`crate::biome`] for the
//! sampler itself and its "y = 0 trap" section.

use crate::biome::{BiomeTable, ClimateSampler};

use super::OverworldGenerator;
use super::biome_cells::BiomeCells;

/// Real multi-noise biome assignment, present on
/// [`OverworldGenerator`] whenever its [`Resolver`] supplies a non-empty
/// [`Resolver::biome_parameters`] table. See `crate::biome`'s module doc for
/// the resolution/height/excluded-biome decisions baked into this.
#[allow(missing_debug_implementations)]
pub(super) struct DynamicBiome {
    pub(super) climate: ClimateSampler,
    /// The climate rows **and** their search tree (Unit 9). Was a bare
    /// `Vec<BiomeParameterPoint>`; [`BiomeTable`] derefs to that slice, which is
    /// what keeps `super`'s own `d.table.iter()` and this struct's literal in
    /// `overworld/mod.rs` compiling untouched — see [`BiomeTable`]'s doc.
    pub(super) table: BiomeTable,
    pub(super) temperatures: std::collections::HashMap<String, f32>,
}

impl OverworldGenerator {
    /// Stage 2: the 16 **surface** quarts —
    /// `(qx, qz)` in `0..4`, row-major `qz * 4 + qx`, matching `ChunkSection`'s own
    /// `BIOME_EDGE` of 4 — each read at that quart's own already-generated surface
    /// height.
    ///
    /// **This no longer samples anything.** The full biome grid made
    /// [`Self::biome_cells_stage`]'s 4×4×4 grid the primary product, and this reads
    /// the layer each quart's surface falls in. That is exact, not an
    /// approximation: the surface sample height is `(height >> 2) << 2`, already
    /// quart-aligned, so it is by construction one of that grid's own layers. If
    /// you change either sample convention, change both — a divergence would show
    /// up as surface vegetation being chosen from a biome the served chunk does not
    /// report.
    ///
    /// Why a *surface* array still exists alongside the grid: the module doc's
    /// "y = 0 trap". Surface material, carve and decorate each ask a specific,
    /// different question about Y, and having the grid does not license collapsing
    /// them onto one answer — see [`super::biome_cells`].
    pub(super) fn biome_stage(
        &self,
        cells: &BiomeCells,
        heights: &[i32; 256],
    ) -> [(String, bool); 16] {
        std::array::from_fn(|i| {
            let qx = i % 4;
            let qz = i / 4;
            let (lx, lz) = (qx as i32 * 4, qz as i32 * 4);
            let y = (heights[(lz * 16 + lx) as usize] >> 2) << 2;
            let qy = ((y - self.min_y) >> 2).max(0) as usize;
            let name = cells.at_quart(qx, qy, qz);
            let cold = match &self.dynamic_biome {
                Some(d) => crate::biome::cold_enough_to_snow(&d.temperatures, name),
                None => self.fallback_cold_enough_to_snow,
            };
            (name.to_string(), cold)
        })
    }

    /// Stage 2b: the **full** 4×4×4 biome grid for this column —
    /// `16 × height/4` cells, one `MultiNoiseBiomeSource.getNoiseBiome` per
    /// `QuartPos` cell, which is what `LevelChunkSection`'s biome container holds.
    ///
    /// [`Self::biome_stage`]'s 16-entry surface array is the *same data* read at
    /// one Y per column — that function takes this grid as a parameter rather than
    /// sampling again; see [`super::biome_cells`]'s module doc for why that is
    /// exact and not an approximation.
    ///
    /// Falls back to a single-biome column when the resolver supplied no climate
    /// table, matching [`Self::biome_stage`]'s own per-quart degradation.
    pub(super) fn biome_cells_stage(&self, base_x: i32, base_z: i32) -> BiomeCells {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Biome);
        let Some(dynamic) = &self.dynamic_biome else {
            return BiomeCells::uniform(&self.fallback_biome, self.min_y, self.height);
        };
        BiomeCells::from_fn(self.min_y, self.height, |qx, qy, qz| {
            // Quart *corner*, not centre — the convention `biome_stage`'s own
            // comment records as having matched a real dark_forest/river boundary
            // where the centre convention did not. Applied to Y as well, which is
            // why `min_y` has to be quart-aligned for this to be exact (it is:
            // -64 >> 2 << 2 == -64).
            let y = self.min_y + (qy as i32) * 4;
            let target = dynamic
                .climate
                .target(base_x + qx as i32 * 4, y, base_z + qz as i32 * 4);
            dynamic.table.nearest(&target).to_string()
        })
    }

    /// **Diagnostic for the R-tree ruling, not used by generation.** At one source chunk's own
    /// `y = 0` climate target, does vanilla's indexed search (what
    /// [`Self::biome_for_carver_source`] now uses) resolve to a different biome id
    /// than the brute-force scan it used before?
    ///
    /// This exists because "no gate failed" and "nothing changed" are different
    /// claims, and only the second one is worth reporting. The 0.98% divergence
    /// figure is measured over *arbitrary* climate targets; whether real generated
    /// climate at real coordinates ever lands on an exact tie is a separate
    /// question that no existing gate answers, so this lets one count it directly.
    /// Returns `(vanilla's indexed answer, the brute-force answer)`, equal when the
    /// tie-break makes no difference here, or `None` for a fixed-biome generator
    /// (which has no table to search). Returning both *names* rather than a bool is
    /// what lets a gate print coordinates and expected values a JVM oracle run can
    /// be pointed straight at.
    #[must_use]
    pub fn source_biome_tiebreak(&self, source_cx: i32, source_cz: i32) -> Option<(&str, &str)> {
        let d = self.dynamic_biome.as_ref()?;
        let target = d.climate.target(source_cx * 16, 0, source_cz * 16);
        let tree = d.table.nearest_row(&target);
        let brute = crate::biome::nearest_row_brute_force(&d.table, &target);
        Some((d.table.biome_at(tree), d.table.biome_at(brute)))
    }

    /// The same question for the 16 **surface** quarts of one chunk — the biome a
    /// player standing there sees. Returns `(quarts, differing)`.
    ///
    /// Needs the chunk's own generated heights, so it runs the pre-ore pipeline via
    /// the store exactly as [`Self::biome_stage`]'s caller does; the sample height
    /// convention is therefore identical to production's rather than a restatement
    /// of it.
    #[must_use]
    pub fn surface_biome_tiebreak_differences(&self, cx: i32, cz: i32) -> (usize, usize) {
        let Some(d) = self.dynamic_biome.as_ref() else {
            return (0, 0);
        };
        let cached = self.pre_ore_stage(cx, cz);
        let heights = &cached.1;
        let base_x = cx * 16;
        let base_z = cz * 16;
        let mut differing = 0usize;
        for i in 0..16usize {
            let qx = (i % 4) as i32;
            let qz = (i / 4) as i32;
            let lx = qx * 4;
            let lz = qz * 4;
            let y = (heights[(lz * 16 + lx) as usize] >> 2) << 2;
            let target = d.climate.target(base_x + lx, y, base_z + lz);
            let tree = d.table.nearest_row(&target);
            let brute = crate::biome::nearest_row_brute_force(&d.table, &target);
            if d.table.biome_at(tree) != d.table.biome_at(brute) {
                differing += 1;
            }
        }
        (16, differing)
    }

    /// Biome for one *source chunk* in the carve neighbourhood — vanilla's
    /// real `carverBiome` resolution (`NoiseBasedChunkGenerator.applyCarvers`):
    /// sampled at the source chunk's own quart corner (`QuartPos.fromBlock`
    /// of its min block X/Z, which is `source_cx * 16` / `source_cz * 16` —
    /// already quart-aligned since 16 is a multiple of 4, so no extra
    /// rounding is needed) and **`y = 0`**, not the source chunk's surface
    /// height. This is deliberately not [`Self::biome_stage`]'s question:
    /// carver *selection* and surface *material* sample the same climate
    /// fields at different heights and get different (correct) answers —
    /// see `docs/worldgen-parity.md`'s description of `ComposedChunkOracle
    /// .java`'s own `sourceBiome` resolution, which this reproduces exactly.
    ///
    /// # Unit 9: this is the D5 call site
    ///
    /// Called **289 times per pre-ore chunk** by `carve_stage` (`carver`'s
    /// `NEIGHBOURHOOD_RANGE = 8` ⇒ a 17×17 source window) and 9 more times per
    /// post-ore chunk by `ore_stage`, and the answer depends only on
    /// `(this generator, source_cx, source_cz)` — the requesting centre never
    /// enters it. So adjacent chunks' windows, which overlap in 272 of 289
    /// positions, were recomputing the same climate search. That is
    /// `docs/plans/worldgen-rewrite.md`'s D5: ~2.2M squared-distance comparisons
    /// per pre-ore chunk.
    ///
    /// Both halves of the fix are visible in the four lines below:
    /// [`crate::biome::memo`] answers a repeated `(cx, cz)` without searching at
    /// all, and a real search goes through the tree rather than the full-table
    /// scan. The signature is unchanged — still `-> &str` borrowed from `self` —
    /// which is why `carve_stage` and `ore_stage` needed no edit: the memo stores
    /// a **table row**, and the row indexes back into this generator's own table.
    pub(super) fn biome_for_carver_source(&self, source_cx: i32, source_cz: i32) -> &str {
        match &self.dynamic_biome {
            None => self.fallback_biome.as_str(),
            Some(d) => {
                let row = crate::biome::memo::source_row(d.table.id(), source_cx, source_cz, || {
                    let target = d.climate.target(source_cx * 16, 0, source_cz * 16);
                    d.table.nearest_row(&target)
                });
                d.table.biome_at(row)
            }
        }
    }
}
