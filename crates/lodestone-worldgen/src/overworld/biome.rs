//! Stage 2 of [`OverworldGenerator::column`]: multi-noise biome resolution — the
//! per-quart surface-height sample plus the separate `y = 0` per-source-chunk answer
//! carver and ore selection use.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A. See [`crate::biome`] for the
//! sampler itself and its "y = 0 trap" section.

use crate::biome::{BiomeParameterPoint, ClimateSampler};

use super::OverworldGenerator;

/// Real multi-noise biome assignment (issue #405), present on
/// [`OverworldGenerator`] whenever its [`Resolver`] supplies a non-empty
/// [`Resolver::biome_parameters`] table. See `crate::biome`'s module doc for
/// the resolution/height/excluded-biome decisions baked into this.
#[allow(missing_debug_implementations)]
pub(super) struct DynamicBiome {
    pub(super) climate: ClimateSampler,
    pub(super) table: Vec<BiomeParameterPoint>,
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
            let name = crate::biome::nearest_biome(&dynamic.table, &target);
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
    pub(super) fn biome_for_carver_source(&self, source_cx: i32, source_cz: i32) -> &str {
        match &self.dynamic_biome {
            None => self.fallback_biome.as_str(),
            Some(d) => {
                let target = d.climate.target(source_cx * 16, 0, source_cz * 16);
                crate::biome::nearest_biome(&d.table, &target)
            }
        }
    }
}
