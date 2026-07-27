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
                .unwrap_or_else(|| panic!("noise '{name}' missing firstOctave")) as i32,
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
                EMBEDDED_WORLDGEN.binary_search_by(|(id, _)| (*id).cmp(key)).is_ok(),
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
}
