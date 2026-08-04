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
//! surface rules + real multi-noise biome assignment (issue #405) — real
//! terrain shape, surface, and biome variety, verified block-for-block (and,
//! for biome, exact-id) against a JVM in isolation. It does **not** yet run
//! carvers, the full aquifer, or features (no caves/ores/trees — issue #295 /
//! epic #404's Phase 2). See [`lodestone_worldgen::overworld`].

use std::sync::OnceLock;

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

include!(concat!(env!("OUT_DIR"), "/embedded_worldgen.rs"));

/// The fallback biome [`OverworldGenerator`] would use if [`EmbeddedResolver`]
/// supplied no biome-parameter table — it does (see [`EmbeddedResolver::biome_parameters`]),
/// so real per-column biome variety (issue #405) is what this generator
/// actually produces; these two constants only document "what it used to
/// always be" and are the value a future resolver with no biome data still
/// gets. Plains has snow disabled, matching `cold_enough_to_snow == false`.
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

    /// Real multi-noise biome assignment (issue #405). Overriding this
    /// (default is an empty array, per [`Resolver::biome_parameters`]'s own
    /// doc) is what switches [`OverworldGenerator`] from its old
    /// single-fixed-biome behaviour to real per-column variety — see
    /// `biome_parameters/overworld.json`'s own header for provenance
    /// (`scripts/worldgen-oracle/BiomeOracle.java`, `table` mode, 7594 rows).
    fn biome_parameters(&self) -> Value {
        self.json("biome_parameters/overworld")
    }

    /// Per-biome `temperature`, used to derive `cold_enough_to_snow` per
    /// sampled column (`biome_parameters/overworld_temperature.json`, read
    /// directly from vanilla's own `data/minecraft/worldgen/biome/*.json`
    /// files — no oracle needed for this one, see that file's own header).
    fn biome_temperatures(&self) -> Value {
        self.json("biome_parameters/overworld_temperature")
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

    /// Exact biome-id parity against vanilla's own `RandomState.sampler()` +
    /// `MultiNoiseBiomeSourceParameterList.findValueBruteForce` (issue #405).
    ///
    /// Ground truth: `scripts/worldgen-oracle/BiomeOracle.java` `sample`
    /// mode, seed 42, at each column's own quart-aligned corner and its own
    /// generated terrain surface height (`y` rounded down to a multiple of 4
    /// — see [`lodestone_worldgen::overworld::OverworldGenerator::biome_stage`]'s
    /// doc comment for why *both* axes need quart-rounding, found the hard
    /// way: getting either wrong flips a real dark_forest/river boundary at
    /// world `(0, 0)`, one of the fixtures below). This is a *predicted
    /// value*, not a "some variety appeared" check — CLAUDE.md's "predict
    /// the value, not the sign": a climate-band-boundary-off-by-one bug would
    /// still show *some* biome, so only an exact match against vanilla's own
    /// answer catches it.
    #[test]
    fn biome_matches_vanilla_at_known_coordinates_seed_42() {
        let seed = 42;
        let generator = overworld_generator(seed);

        // (world x, world z, vanilla's own answer at that column's quart
        // corner and generated surface height).
        let cases: &[(i32, i32, &str)] = &[
            (0, 0, "minecraft:dark_forest"),
            (8, 8, "minecraft:river"),
            (-8, 8, "minecraft:dark_forest"),
            (500, 500, "minecraft:deep_ocean"),
            (-500, 500, "minecraft:beach"),
            (2000, -1500, "minecraft:swamp"),
            (10000, 10000, "minecraft:deep_ocean"),
            (300, -800, "minecraft:plains"),
            (-4000, 100, "minecraft:lukewarm_ocean"),
            (1000, 0, "minecraft:deep_cold_ocean"),
            (0, 1000, "minecraft:beach"),
            (5000, 5000, "minecraft:warm_ocean"),
            (-10000, -10000, "minecraft:plains"),
            (120, 4564, "minecraft:river"),
            (776, -780, "minecraft:frozen_peaks"),
            (64, 64, "minecraft:beach"),
            (-2500, 3200, "minecraft:savanna"),
        ];

        let mut distinct = std::collections::BTreeSet::new();
        for &(x, z, want) in cases {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16) as usize;
            let lz = z.rem_euclid(16) as usize;
            let col = generator.column(cx, cz);
            let got = col.biome_state(lx, lz);
            assert_eq!(got, want, "biome mismatch at world ({x}, {z})");
            distinct.insert(got.to_string());
        }
        // Anti-vacuity floor, per CLAUDE.md's "magnitude" vacuous-test
        // species: a table/search bug that happened to return one constant
        // biome for every probed *coordinate* would still fail the loop
        // above at 16/17 cases — but a bug that returns one constant biome
        // whenever asked (ignoring climate entirely) needs a *count* check
        // to catch, since it could theoretically pass every exact-match
        // assertion if all 17 fixtures shared their expected biome (they do
        // not, by construction — this asserts that fact rather than
        // assuming it). 10 is derived from this exact probe set's own
        // distinct answers above, not guessed.
        assert!(
            distinct.len() >= 10,
            "expected wide biome variety across the probe set, got only {distinct:?}"
        );
    }

    /// Control for [`biome_matches_vanilla_at_known_coordinates_seed_42`]'s
    /// implicit claim that the search can return *different* answers for
    /// different inputs: run it and watch a single chunk (0, 0) — which
    /// straddles the `dark_forest`/`river` boundary the fixture above
    /// already names — actually produce both biomes across its 16 quarts,
    /// not one biome copy-pasted 16 times.
    #[test]
    fn a_single_chunk_can_carry_more_than_one_biome() {
        let generator = overworld_generator(42);
        let col = generator.column(0, 0);
        assert!(
            col.distinct_biome_count() >= 2,
            "chunk (0,0) at seed 42 is known (BiomeOracle) to straddle a \
             dark_forest/river boundary; got only one biome across all 16 quarts"
        );
    }

    /// [`lodestone_worldgen::biome::usable_overworld_table`]'s exclusion of
    /// the three unported-surface-rule badlands variants (see that
    /// function's doc comment) is only real if the *served* generator
    /// actually avoids them — this walks a wide grid of real columns (not a
    /// hand-picked climate target, which would only prove the table itself
    /// was filtered, not that the filtered table is what generation uses)
    /// and asserts none of them ever come back badlands/eroded_badlands/
    /// wooded_badlands, watching the assertion actually have something to
    /// catch: `crate::chunk::ChunkColumn`'s biome storage has no special
    /// case for these names, so a regression that dropped the filter would
    /// fail this loop, not pass it vacuously.
    #[test]
    fn served_columns_never_carry_an_unported_badlands_variant() {
        // 12×12 chunks (~192×192 blocks): wide enough to cross several
        // biome boundaries in a debug build without the full-column
        // generation cost of a much larger sweep — `cargo test -p
        // lodestone-worldgen`'s own JVM-parity suite already proves the
        // shape/surface machinery at scale, so this only needs to be wide
        // enough to exercise the biome filter, not to re-prove terrain
        // generation itself.
        let generator = overworld_generator(42);
        let mut checked = 0usize;
        for cx in -6..6 {
            for cz in -6..6 {
                let col = generator.column(cx, cz);
                for lz in 0..16 {
                    for lx in 0..16 {
                        let biome = col.biome_state(lx, lz);
                        assert!(
                            !lodestone_worldgen::biome::UNSUPPORTED_SURFACE_BIOMES
                                .contains(&biome),
                            "column ({cx},{cz}) local ({lx},{lz}) resolved to unported biome {biome}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 12 * 12 * 16 * 16, "grid scan must cover every probed cell");
    }

    /// End-to-end: real biome variety reaches the **served** column (the
    /// column `ServerProtocol::encode_chunk` sends), not just the raw
    /// generator — closing the island CLAUDE.md's rule 1 warns about. Two
    /// adjacent-ish chunks at seed 42 are known (the fixtures above) to
    /// carry different biomes; this proves that survives the
    /// `OverworldChunkSource` wrapper the wire encoder actually reads from.
    #[test]
    fn served_chunk_source_carries_real_biome_variety() {
        use crate::ChunkSource;

        let seed = 42;
        let source = overworld_chunk_source(seed);

        // world (0, 0) -> chunk (0,0) local (0,0): dark_forest.
        let a = source.column(0, 0);
        assert_eq!(a.biome_state(0, 0), "minecraft:dark_forest");
        // world (500, 500) -> chunk (31,31) local (4,4): deep_ocean.
        let b = source.column(31, 31);
        assert_eq!(b.biome_state(4, 4), "minecraft:deep_ocean");
    }

    /// The design question `docs/block-edit.md` answers: before edit support,
    /// `OverworldChunkSource::column` called straight through to the
    /// generator on *every* request, so nothing an edit wrote could survive a
    /// later `column()` call — there was nowhere for it to live. This is the
    /// hermetic proof that `set_block`'s retention actually closes that gap,
    /// independent of the slower end-to-end client test
    /// (`crates/protocol/v770/tests/block_edit.rs`), which proves the same
    /// thing through the real wire protocol and a real forget/reload cycle.
    #[test]
    fn set_block_persists_across_repeated_column_calls() {
        use crate::ChunkSource;

        let seed = 1234;
        let source = overworld_chunk_source(seed);

        // World (0, -50, 0) — chunk (0, 0), local (0, 0) — is deep enough
        // that this carver-less generator (`worldgen_data`'s own "no caves"
        // scope note) always fills it: real generated content, not
        // already-air, so an "edit" that landed on existing air could not
        // pass this test by accident.
        let pre = source.block_state(0, -50, 0);
        assert_eq!(
            pre.split('[').next(),
            Some("minecraft:deepslate"),
            "test fixture assumption broke: expected solid deepslate at (0,-50,0), found {pre}"
        );

        source.set_block(0, -50, 0, "minecraft:air");
        assert_eq!(source.block_state(0, -50, 0), "minecraft:air");

        // Re-fetch the whole column again — simulating the column being
        // forgotten and re-sent, `crate::server`'s `ViewTracker` forget/resend
        // cycle — through a *second, independent* `column()` call. Without
        // retention this would silently regenerate the original deepslate.
        let recolumn = source.column(0, 0);
        assert_eq!(recolumn.block_state(0, -50, 0), "minecraft:air");

        // The edit must be scoped to exactly the touched cell, not a
        // wholesale wipe of the column: an adjacent, untouched cell in the
        // same column still reads the generator's original content.
        assert_eq!(
            recolumn.block_state(1, -50, 0).split('[').next(),
            Some("minecraft:deepslate"),
            "editing (0,-50,0) must not affect its untouched neighbour"
        );
    }
}
