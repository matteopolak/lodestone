//! Stage 2 of [`OverworldGenerator::column`]: multi-noise biome resolution — the
//! per-quart surface-height sample plus the separate `y = 0` per-source-chunk answer
//! carver and ore selection use.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A. See [`crate::biome`] for the
//! sampler itself and its "y = 0 trap" section.

use crate::biome::{BiomeTable, ClimateSampler};

use super::OverworldGenerator;

/// Real multi-noise biome assignment (issue #405), present on
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
    /// Stage 2 (issue #405): one climate sample per horizontal quart
    /// `(qx, qz)` in `0..4`, row-major `qz * 4 + qx` — 16 per chunk, matching
    /// [`lodestone_world`](crate)'s own `ChunkSection::BIOME_EDGE` (4) so a
    /// future encoder can write this straight into a real biome container.
    /// Broadcast vertically: see `crate::biome`'s module doc for why one
    /// sample per quart *column*, not a full 3-D grid, is this phase's
    /// deliberate scope.
    ///
    /// Each quart samples at its own already-generated surface height
    /// (`heights[]`, [`Self::heights_from_field`]'s output) rather than a
    /// fixed Y — the module doc's "y = 0 trap" section is why a constant
    /// height silently produces almost all cave/deep-ocean biomes instead of
    /// the terrain biome a player standing there would actually see.
    pub(super) fn biome_stage(&self, heights: &[i32; 256], base_x: i32, base_z: i32) -> [(String, bool); 16] {
        // Entered unconditionally, unlike `ore_stage`/`vegetation_stage` below:
        // this stage has no single early return, it degrades per-quart when
        // `dynamic_biome` is `None`. So `stage_entered[biome]` is NOT the
        // vacuity signal for this one — `biome_searches` is. A fixed-biome
        // generator enters this stage 16 times and searches zero times.
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Biome);
        std::array::from_fn(|i| {
            let Some(dynamic) = &self.dynamic_biome else {
                return (
                    self.fallback_biome.clone(),
                    self.fallback_cold_enough_to_snow,
                );
            };
            let qx = (i % 4) as i32;
            let qz = (i / 4) as i32;
            // Quart cell (qx, qz) covers local x/z in [qx*4, qx*4+4); sample
            // at its own **corner** (`qx*4`), not its center — see
            // `crate::biome`'s module doc and `docs/worldgen-biomes.md` for
            // why this matched a real dark_forest/river boundary and the
            // center convention did not.
            let lx = qx * 4;
            let lz = qz * 4;
            // Y needs the same quart-rounding as X/Z (see the module doc).
            let y = (heights[(lz * 16 + lx) as usize] >> 2) << 2;
            let target = dynamic.climate.target(base_x + lx, y, base_z + lz);
            // Unit 9: the tree, not `crate::biome::nearest_biome`'s brute-force
            // scan. Result-identical at every target by construction — see
            // `crate::biome::tree`'s module doc. **Not memoised**, deliberately:
            // each quart samples at its own surface height, so all 16 targets in
            // a chunk are distinct and there is nothing to reuse. The memo is for
            // `biome_for_carver_source`, whose key really does repeat.
            let name = dynamic.table.nearest(&target);
            let cold = crate::biome::cold_enough_to_snow(&dynamic.temperatures, name);
            (name.to_string(), cold)
        })
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
