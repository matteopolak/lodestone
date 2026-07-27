//! Bundled singleplayer overworld generator.
//!
//! Closes the worldgen island: the verified [`lodestone_worldgen`] pipeline is
//! version-free and holds no data, so *something* must supply the vanilla noise
//! settings, density functions and noises. This module embeds the 26.2 shape +
//! surface data (see `build.rs`) and exposes a synchronous
//! [`overworld_generator`] the shell's local world can call directly — no async
//! runtime, no files, no network.
//!
//! # Where this belongs long-term
//!
//! Per plan §3 version-specific worldgen data eventually lives in the version
//! crate, dropped when the version is dropped. This bundled copy is the
//! singleplayer *default* the direct-call path consumes today; when the
//! integrated-server-over-loopback path lands, it calls the same
//! [`OverworldGenerator`] — only the data's home moves, not the generator.
//!
//! # Honest scope
//!
//! [`OverworldGenerator`] composes shape + a sea-level aquifer approximation +
//! surface rules — real terrain shape and surface, verified block-for-block
//! against a JVM in isolation. It does **not** yet run carvers, the full
//! aquifer, or features (no caves/ores/trees). See [`lodestone_worldgen::overworld`].

use std::sync::OnceLock;

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

include!(concat!(env!("OUT_DIR"), "/embedded_worldgen.rs"));

/// The fixed biome generation runs under until the multi-noise biome source
/// exists. Plains has snow disabled, matching `cold_enough_to_snow == false`.
const DEFAULT_BIOME: &str = "minecraft:plains";
const DEFAULT_BIOME_SNOWS: bool = false;

/// A [`Resolver`] backed by the embedded worldgen table.
///
/// Parsed `Value`s are cached so repeated references to the same density
/// function (the router tree revisits shared nodes heavily) parse once.
#[derive(Debug, Default)]
struct EmbeddedResolver;

impl EmbeddedResolver {
    fn raw(&self, key: &str) -> &'static str {
        // Binary search: the table is sorted by id in `build.rs`.
        EMBEDDED_WORLDGEN
            .binary_search_by(|(id, _)| (*id).cmp(key))
            .map(|i| EMBEDDED_WORLDGEN[i].1)
            .unwrap_or_else(|_| panic!("embedded worldgen data missing '{key}'"))
    }

    fn json(&self, key: &str) -> Value {
        serde_json::from_str(self.raw(key))
            .unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
    }
}

impl Resolver for EmbeddedResolver {
    fn density_function(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.json(&format!("density_function/{name}"))
    }

    fn noise(&self, id: &str) -> NoiseParams {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let v = self.json(&format!("noise/{name}"));
        NoiseParams {
            first_octave: v["firstOctave"]
                .as_i64()
                .unwrap_or_else(|| panic!("noise '{name}' missing firstOctave"))
                as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .unwrap_or_else(|| panic!("noise '{name}' missing amplitudes"))
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }
}

/// The parsed overworld noise settings (parsed once, reused across seeds).
fn overworld_settings() -> &'static Value {
    static SETTINGS: OnceLock<Value> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        let raw = EmbeddedResolver.raw("noise_settings/overworld");
        serde_json::from_str(raw).expect("parsing embedded overworld noise settings")
    })
}

/// Builds the bundled overworld generator for `seed`.
///
/// This is the synchronous direct-call entry point the shell uses to render a
/// real world. It reuses the parsed settings but rebuilds the seed-dependent
/// density/noise state per call, so callers should build it once per world and
/// reuse it across chunks.
#[must_use]
pub fn overworld_generator(seed: i64) -> OverworldGenerator {
    OverworldGenerator::new(
        seed,
        overworld_settings(),
        &EmbeddedResolver,
        DEFAULT_BIOME,
        DEFAULT_BIOME_SNOWS,
    )
}

/// Builds the bundled overworld [`ChunkSource`](crate::ChunkSource) for `seed`.
///
/// This is the terrain source the **integrated server** serves to a real client
/// (and the path `ServerProtocol::encode_chunk` drives). It wraps the same
/// [`overworld_generator`] the shell calls directly, so both the direct
/// singleplayer path and the loopback-server path produce identical, verified
/// block states — no simplified second generator lives one layer in.
#[must_use]
pub fn overworld_chunk_source(seed: i64) -> crate::chunk::OverworldChunkSource {
    crate::chunk::OverworldChunkSource::new(overworld_generator(seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_is_sorted_and_nonempty() {
        assert!(
            EMBEDDED_WORLDGEN.len() > 90,
            "expected the full shape+surface data subset, got {} files",
            EMBEDDED_WORLDGEN.len()
        );
        assert!(
            EMBEDDED_WORLDGEN.windows(2).all(|w| w[0].0 < w[1].0),
            "embedded table must be sorted for binary_search"
        );
        // The load-bearing entries the generator dereferences by name.
        for key in [
            "noise_settings/overworld",
            "density_function/overworld/sloped_cheese",
            "noise/continentalness",
        ] {
            assert!(
                EMBEDDED_WORLDGEN
                    .binary_search_by(|(id, _)| (*id).cmp(key))
                    .is_ok(),
                "embedded table missing '{key}'"
            );
        }
    }

    #[test]
    fn generator_builds_and_produces_real_terrain() {
        let generator = overworld_generator(42);
        let col = generator.column(0, 0);
        // Anti-vacuity: a real column is neither all air nor all one block.
        let non_air = col.non_air_count();
        assert!(
            non_air > 16 * 16 * 10,
            "bundled generator produced near-empty column ({non_air} non-air)"
        );
        let mut kinds = std::collections::BTreeSet::new();
        for lz in 0..16 {
            for lx in 0..16 {
                for y in col.min_y()..col.min_y() + col.height() {
                    let b = col.block_state(lx, y, lz);
                    kinds.insert(b.split('[').next().unwrap_or(b).to_string());
                }
            }
        }
        assert!(
            kinds.len() >= 3,
            "expected shape+fluid+surface variety, got only {kinds:?}"
        );
    }

    /// The integrated server's chunk source must serve the **real** generator
    /// block-for-block — no simplified terrain one layer in. This diffs the
    /// [`crate::ChunkSource`] output against the generator over a whole column
    /// and floors on fluid + surface presence so it can't pass on empty air.
    #[test]
    fn chunk_source_serves_generator_block_for_block() {
        use crate::ChunkSource;

        let seed = 42; // chunk (0,0) is a submerged ocean column at this seed.
        let generator = overworld_generator(seed);
        let source = overworld_chunk_source(seed);
        let expected = generator.column(0, 0);
        let served = source.column(0, 0);

        assert_eq!(served.min_y, expected.min_y());
        assert_eq!(served.height, expected.height());

        let mut checked = 0usize;
        let mut water = 0usize;
        let mut surface = 0usize; // non-stone solid: grass/dirt/sand/gravel/…
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for y in served.min_y..served.min_y + served.height {
                    let want = expected.block_state(lx as usize, y, lz as usize);
                    let got = served.block_state(lx, y, lz);
                    assert_eq!(got, want, "served/generated mismatch at ({lx},{y},{lz})");
                    checked += 1;
                    match got.split('[').next().unwrap_or(got) {
                        "minecraft:water" => water += 1,
                        "minecraft:air"
                        | "minecraft:cave_air"
                        | "minecraft:void_air"
                        | "minecraft:lava"
                        | "minecraft:stone"
                        | "minecraft:bedrock" => {}
                        _ => surface += 1,
                    }
                }
            }
        }
        // The comparison loop covered the whole column (not a short-circuit).
        assert_eq!(checked, 16 * 16 * served.height as usize);
        // Fluid fill survived into the served chunk (this ocean column is wet).
        assert!(water > 0, "served ocean chunk has no water — fluid stage lost");
        // Surface rules survived too (gravel/dirt on the ocean floor).
        assert!(
            surface > 0,
            "served chunk has no surface material — surface stage lost"
        );
    }
}
