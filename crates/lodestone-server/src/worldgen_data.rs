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
//! [`OverworldGenerator`] composes shape + the **real** aquifer + surface
//! rules + real multi-noise biome assignment (issue #405) + real carvers
//! (issue #295) — real terrain shape, surface, biome variety, and caves/
//! ravines, verified block-for-block (and, for biome, exact-id) against a
//! JVM (`docs/worldgen-parity.md`'s harness measures the composed subset
//! directly). It does **not** yet run ore/vegetation features — deferred
//! pending a real neighbour-aware `FeatureOracle.java` driver, see
//! [`lodestone_worldgen::overworld`]'s module doc — or structures.

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

    /// Like [`Self::raw`], but a missing key returns `None` instead of
    /// panicking — for the issue #295 composition lookups
    /// (`biome_document`/`configured_carver`/`configured_feature`/
    /// `placed_feature`/`block_tag`), where a name absent from the embedded
    /// table (e.g. a `mineable/*` tool tag never bundled, or a biome id the
    /// parameter table names that this bundle didn't ship) is expected and
    /// should resolve to "no data" per `Resolver`'s own documented default,
    /// not abort chunk generation.
    fn try_raw(&self, key: &str) -> Option<&'static str> {
        EMBEDDED_WORLDGEN
            .binary_search_by(|(id, _)| (*id).cmp(key))
            .ok()
            .map(|i| EMBEDDED_WORLDGEN[i].1)
    }

    fn try_json(&self, key: &str) -> Value {
        self.try_raw(key).map_or(Value::Null, |raw| {
            serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
        })
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

    /// Full `worldgen/biome/<name>.json` documents (issue #295 composition):
    /// carvers + `UNDERGROUND_ORES` feature lists, for
    /// `crate::worldgen_data`'s bundled generator to compose carvers into
    /// [`OverworldGenerator::column`]. 66 files, copied verbatim from
    /// `.cache/mc/26.2/src/data/minecraft/worldgen/biome/` (Mojang's own
    /// generated data, CLAUDE.md data-source #1).
    fn biome_document(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("biome/{name}"))
    }

    /// `worldgen/configured_carver/<name>.json` — 4 files (`cave`,
    /// `cave_extra_underground`, `canyon`, `nether_cave`; only the first
    /// three are ever referenced by an overworld biome).
    fn configured_carver(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("configured_carver/{name}"))
    }

    /// `worldgen/configured_feature/<name>.json` — 226 files, copied
    /// verbatim from `.cache/mc/26.2/src/data/minecraft/worldgen/
    /// configured_feature/` (issue #295's ore-composition increment; the
    /// non-ore entries are bundled too, ready for epic #404 Phase 3's
    /// vegetation features rather than filtered out here).
    fn configured_feature(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("configured_feature/{name}"))
    }

    /// `worldgen/placed_feature/<name>.json` — 262 files, same provenance as
    /// [`Self::configured_feature`].
    fn placed_feature(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("placed_feature/{name}"))
    }

    /// `tags/block/<name>.json` — 261 files, needed to resolve
    /// `#overworld_carver_replaceables`' recursive closure (issue #295).
    fn block_tag(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("tags/block/{name}"))
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

    /// **Superseded property, inverted.** This test used to assert served
    /// columns *never* resolve to badlands/eroded_badlands/wooded_badlands,
    /// because `lodestone_worldgen::biome::usable_overworld_table` used to
    /// exclude them (their surface rule reached an unported
    /// `SurfaceSystem.getBand`, which would panic). `3cf523c` ported
    /// `getBand` (`crate::surface::Rule::Bandlands`) and made
    /// `usable_overworld_table` a pass-through — see that function's own doc
    /// comment, which names this exact test as needing this update. The old
    /// assertion's premise is gone: a column can resolve to any of the three
    /// again, so asserting it never does is now testing a stale invariant,
    /// not a real one.
    ///
    /// Re-verified before rewriting, per `CLAUDE.md`'s "re-verify before
    /// routing around": running the *old* assertion against this tree
    /// (`cargo test -p lodestone-server … served_columns_never_carry_an_unported_badlands_variant
    /// -- --nocapture`) passed — the 12×12 sweep at seed 42 happens not to
    /// cross a badlands boundary in this exact window, so the old test was a
    /// time bomb (would fail the moment correct code touched badlands
    /// climate here), not a test that was actually red on `main` right now.
    ///
    /// That finding is exactly why the sweep alone is insufficient evidence
    /// for the *new* property too: scanning only `-6..6` would find zero
    /// badlands cells and pass vacuously, proving nothing (`CLAUDE.md`'s
    /// "assertions of an absence need a control proving the detector
    /// works" — the mirror image applies to an assertion of *presence*).
    /// `docs/worldgen-parity.md`'s own measured finding — chunk
    /// `(-120,-120)`'s real vanilla biome is badlands/eroded_badlands — is
    /// added to the coordinate list for exactly that reason, so
    /// `badlands_cells > 0` below is asserted, not merely hoped for.
    ///
    /// The predicted value set is not "some badlands block": `SurfaceSystem
    /// .generateBands` (`.cache/mc/26.2/src/net/minecraft/world/level/levelgen/SurfaceSystem.java:286-316`)
    /// and this port's `generate_bands`
    /// (`crates/lodestone-worldgen/src/surface/mod.rs:170-209`) can only ever
    /// emit exactly these seven blocks: base `minecraft:terracotta`
    /// (java:287-288, rust:171), `minecraft:orange_terracotta`
    /// (java:292-293, rust:184), `minecraft:yellow_terracotta` (java:297,
    /// rust:189), `minecraft:brown_terracotta` (java:298, rust:190),
    /// `minecraft:red_terracotta` (java:299, rust:191),
    /// `minecraft:white_terracotta` (java:303-304, rust:197) and
    /// `minecraft:light_gray_terracotta` (java:306/310, rust:199/202) — no
    /// other block can ever come back from `Rule::Bandlands`/`getBand`
    /// (`SurfaceSystem.java:332-334`). These are the only blocks this test's
    /// terracotta scan can match, so a false positive from an unrelated
    /// block is not possible.
    #[test]
    fn badlands_columns_when_present_carry_terracotta_bands() {
        const TERRACOTTA_BAND_BLOCKS: [&str; 7] = [
            "minecraft:terracotta",
            "minecraft:orange_terracotta",
            "minecraft:yellow_terracotta",
            "minecraft:brown_terracotta",
            "minecraft:red_terracotta",
            "minecraft:white_terracotta",
            "minecraft:light_gray_terracotta",
        ];

        let generator = overworld_generator(42);

        // Same 12×12 sweep the old test used, plus the one coordinate
        // `docs/worldgen-parity.md` already measured as real-vanilla
        // badlands at this seed — without it, this test's core claim would
        // never actually fire against this window (see doc comment above).
        let mut coords: Vec<(i32, i32)> = Vec::new();
        for cx in -6..6 {
            for cz in -6..6 {
                coords.push((cx, cz));
            }
        }
        coords.push((-120, -120));

        let mut badlands_cells = 0usize;
        let mut band_hits = 0usize;
        for (cx, cz) in coords {
            let col = generator.column(cx, cz);
            let min_y = col.min_y();
            let height = col.height();
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let biome = col.biome_state(lx, lz);
                    if !lodestone_worldgen::biome::UNSUPPORTED_SURFACE_BIOMES.contains(&biome) {
                        continue;
                    }
                    badlands_cells += 1;
                    for y in min_y..min_y + height {
                        let state = col.block_state(lx, y, lz);
                        let base = state.split('[').next().unwrap_or(state);
                        if TERRACOTTA_BAND_BLOCKS.contains(&base) {
                            band_hits += 1;
                        }
                    }
                }
            }
        }

        assert!(
            badlands_cells > 0,
            "test's own premise failed: expected at least one badlands/eroded_badlands/\
             wooded_badlands cell across the 12×12 sweep plus the known-badlands chunk \
             (-120,-120), found none — this test would otherwise pass vacuously"
        );
        assert!(
            band_hits > 0,
            "found {badlands_cells} badlands cell(s) across {} columns but none carried any of \
             the 7 possible terracotta band blocks — SurfaceSystem.getBand \
             (SurfaceSystem.java:332-334) / Rule::Bandlands is not reaching them",
            12 * 12 + 1
        );
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

    /// **Diagnostic control** for `crate::chunk::tests
    /// ::parallel_generation_is_deterministic_and_matches_serial` (issue
    /// #414), which failed after issue #295's ore composition landed. That
    /// test compares serialised bytes (palette order included) across
    /// independent `column()` calls, so a byte mismatch could mean either
    /// "the actual blocks differ" or "the same blocks, a different palette
    /// assignment order" — this isolates which, for the exact chunks that
    /// test uses, with no threading involved at all (two *sequential* calls
    /// on one thread). If this passes, `OverworldGenerator::column` is a
    /// pure function of `(self, cx, cz)` as designed and the #414 failure is
    /// not a value-determinism bug in ore composition itself.
    #[test]
    fn column_is_byte_identical_across_two_independent_sequential_calls() {
        let generator = overworld_generator(42);
        for &(cx, cz) in &[(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (2, -1)] {
            let a = generator.column(cx, cz);
            let b = generator.column(cx, cz);
            let (a_min_y, a_height, a_palette, a_blocks, a_biomes) = a.into_raw();
            let (b_min_y, b_height, b_palette, b_blocks, b_biomes) = b.into_raw();
            assert_eq!(a_min_y, b_min_y, "chunk ({cx},{cz}) min_y differs between two sequential calls");
            assert_eq!(a_height, b_height, "chunk ({cx},{cz}) height differs between two sequential calls");
            assert_eq!(
                a_palette, b_palette,
                "chunk ({cx},{cz}) palette differs between two sequential calls — a non-determinism \
                 bug or a palette-assignment-order difference, not threading"
            );
            assert_eq!(
                a_blocks, b_blocks,
                "chunk ({cx},{cz}) block indices differ between two sequential calls"
            );
            assert_eq!(
                a_biomes, b_biomes,
                "chunk ({cx},{cz}) biome quarts differ between two sequential calls"
            );
        }
    }
}
