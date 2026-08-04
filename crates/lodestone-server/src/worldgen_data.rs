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
//! and ore features (issue #295, the real 3×3 `blockStateWriteRadius(1)`
//! driver) + grass/flower/tree vegetal decoration (issue #406, **single-
//! chunk only — no cross-chunk canopy/patch spill**, see
//! `lodestone_worldgen::feature::vegetation`'s own module doc for the full
//! scope and named gaps) — real terrain shape, surface, biome variety,
//! caves/ravines, and now vegetation, block-for-block verified where a JVM
//! oracle exists for the stage (`docs/worldgen-parity.md`'s harness
//! measures the composed subset directly; vegetation has no such oracle
//! yet — see that module's doc). Structures are still unbuilt anywhere in
//! this repo (`#136`).

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

    /// The exact, named vegetal-decoration gap surface for every biome
    /// reachable via the real overworld biome-parameter table (issue #406's
    /// "loud, not silent" gate). Each entry is `(biome, sorted deduped
    /// reasons)`, where a reason is a
    /// `lodestone_worldgen::feature::vegetation::ConfiguredFeature::Unsupported`
    /// string that biome's `VEGETAL_DECORATION` step actually reaches —
    /// `multiface_growth` (glow lichen, `MultifaceGrowthFeature` — never in
    /// #406's scope, present in nearly every biome), `fallen_tree`
    /// (`FallenTreeFeature`), `tree: unsupported trunk/foliage/size/provider`
    /// (fancy/giant trunks — oak's `fancy_oak` branch and every
    /// jungle/dark-oak/acacia/mangrove/cherry tree, none of which parse a
    /// `TreeConfig` this engine implements), plus a long tail of features
    /// #406 never claimed (`kelp`/`seagrass`/coral/`bamboo`/`vines`/
    /// `huge_*_mushroom`/cave-only `lush_caves` features). Measured by
    /// running every reachable biome through
    /// `lodestone_worldgen::compose::build_biome_vegetation` +
    /// `vegetation::collect_unsupported` once, by hand, and transcribing the
    /// result — see [`vegetation_placer_gaps_are_named_not_silent`] below.
    ///
    /// **A floor, not a ceiling.** [`vegetation_gap_mismatches`] fails loudly
    /// in BOTH directions: a biome producing a reason NOT listed here (a new
    /// silent gap — the exact failure mode this gate exists to catch) or a
    /// listed biome/reason no longer occurring (this table gone stale after
    /// a fix landed — prune the entry, don't leave it). Before cacti/sugar
    /// cane (`BlockColumnFeature`) landed, `minecraft:desert`'s own entry
    /// here would have needed `"block_column: unsupported layer/direction
    /// /predicate"` in addition to `multiface_growth` — this table's job is
    /// to force that kind of entry to be written down, not to auto-shrink.
    const KNOWN_VEGETATION_GAPS: &[(&str, &[&str])] = &[
        ("minecraft:badlands", &["multiface_growth"]),
        ("minecraft:bamboo_jungle", &["bamboo", "multiface_growth", "tree: unsupported trunk/foliage/size/provider", "vines"]),
        ("minecraft:beach", &["multiface_growth"]),
        ("minecraft:birch_forest", &["fallen_tree", "multiface_growth"]),
        ("minecraft:cherry_grove", &["multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:cold_ocean", &["kelp", "multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:dark_forest", &["fallen_tree", "huge_brown_mushroom", "huge_red_mushroom", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:deep_cold_ocean", &["kelp", "multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:deep_dark", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:deep_frozen_ocean", &["multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:deep_lukewarm_ocean", &["kelp", "multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:deep_ocean", &["kelp", "multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:desert", &["multiface_growth"]),
        ("minecraft:dripstone_caves", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:eroded_badlands", &["multiface_growth"]),
        ("minecraft:flower_forest", &["fallen_tree", "multiface_growth", "simple_block: unsupported to_place", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:forest", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:frozen_ocean", &["multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:frozen_peaks", &["multiface_growth"]),
        ("minecraft:frozen_river", &["multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:grove", &["multiface_growth"]),
        ("minecraft:ice_spikes", &["fallen_tree", "multiface_growth"]),
        ("minecraft:jagged_peaks", &["multiface_growth"]),
        ("minecraft:jungle", &["bamboo", "fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider", "vines"]),
        ("minecraft:lukewarm_ocean", &["kelp", "multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:lush_caves", &["block_column: unsupported layer/direction/predicate", "multiface_growth", "random_boolean_selector", "root_system", "vegetation_patch", "vines"]),
        ("minecraft:mangrove_swamp", &["multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:meadow", &["multiface_growth", "simple_block: unsupported to_place", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:mushroom_fields", &["multiface_growth", "random_boolean_selector"]),
        ("minecraft:ocean", &["kelp", "multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:old_growth_birch_forest", &["fallen_tree", "multiface_growth"]),
        ("minecraft:old_growth_pine_taiga", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:old_growth_spruce_taiga", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:pale_garden", &["multiface_growth", "tree: unsupported trunk/foliage/size/provider", "vegetation_patch"]),
        ("minecraft:plains", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:river", &["multiface_growth", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        // savanna/savanna_plateau/windswept_savanna all resolve through
        // trees_savanna's RandomSelector (oak_checked default, acacia_checked
        // 80%, fallen_oak_tree 1.25%) — issue #428's `TrunkPlacerCfg::Forking`/
        // `FoliagePlacerCfg::Acacia` closes the "tree: unsupported..." entry
        // for all three; `fallen_tree` stays (still unimplemented).
        ("minecraft:savanna", &["fallen_tree", "multiface_growth"]),
        ("minecraft:savanna_plateau", &["fallen_tree", "multiface_growth"]),
        ("minecraft:snowy_beach", &["multiface_growth"]),
        ("minecraft:snowy_plains", &["fallen_tree", "multiface_growth"]),
        ("minecraft:snowy_slopes", &["multiface_growth"]),
        ("minecraft:snowy_taiga", &["fallen_tree", "multiface_growth"]),
        ("minecraft:sparse_jungle", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider", "vines"]),
        ("minecraft:stony_peaks", &["multiface_growth"]),
        ("minecraft:stony_shore", &["multiface_growth"]),
        ("minecraft:sulfur_caves", &["multiface_growth"]),
        ("minecraft:sunflower_plains", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:swamp", &["multiface_growth", "seagrass"]),
        ("minecraft:taiga", &["fallen_tree", "multiface_growth"]),
        ("minecraft:warm_ocean", &["coral_claw", "coral_mushroom", "coral_tree", "multiface_growth", "sea_pickle", "seagrass", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:windswept_forest", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:windswept_gravelly_hills", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:windswept_hills", &["fallen_tree", "multiface_growth", "tree: unsupported trunk/foliage/size/provider"]),
        ("minecraft:windswept_savanna", &["fallen_tree", "multiface_growth"]),
        ("minecraft:wooded_badlands", &["fallen_tree", "multiface_growth"]),
    ];

    /// Diffs a measured `biome -> sorted, deduped reasons` map against
    /// [`KNOWN_VEGETATION_GAPS`], both directions. Standalone (no
    /// `EmbeddedResolver` needed) specifically so
    /// [`vegetation_gap_mismatches_fires_on_an_undeclared_gap`] can exercise
    /// it with a synthetic map — CLAUDE.md's "absence assertions need a
    /// control proving the detector fires."
    fn vegetation_gap_mismatches(actual: &std::collections::BTreeMap<String, Vec<String>>) -> Vec<String> {
        let known: std::collections::BTreeMap<&str, &[&str]> =
            KNOWN_VEGETATION_GAPS.iter().copied().collect();
        let mut mismatches = Vec::new();
        for (biome, reasons) in actual {
            let expected: &[&str] = known.get(biome.as_str()).copied().unwrap_or(&[]);
            if reasons.iter().map(String::as_str).ne(expected.iter().copied()) {
                mismatches.push(format!(
                    "{biome}: KNOWN_VEGETATION_GAPS says {expected:?}, measured {reasons:?}"
                ));
            }
        }
        for biome in known.keys() {
            if !actual.contains_key(*biome) {
                mismatches.push(format!(
                    "{biome}: listed in KNOWN_VEGETATION_GAPS but no longer a reachable overworld biome"
                ));
            }
        }
        mismatches
    }

    /// Measures the real gap surface once (via `EmbeddedResolver`, the same
    /// data the bundled generator serves) and asserts it matches
    /// [`KNOWN_VEGETATION_GAPS`] exactly, in both directions. This is the
    /// issue #406 gate: a biome whose declared `VEGETAL_DECORATION` step
    /// includes a placer this crate doesn't implement, and which isn't
    /// already named above, now fails a required check instead of quietly
    /// generating a biome with fewer trees than vanilla.
    #[test]
    fn vegetation_placer_gaps_are_named_not_silent() {
        use std::collections::BTreeMap;
        let table = lodestone_worldgen::biome::parse_table(&EmbeddedResolver.biome_parameters());
        let table = lodestone_worldgen::biome::usable_overworld_table(table);
        let mut names: Vec<String> = table.into_iter().map(|p| p.biome).collect();
        names.sort_unstable();
        names.dedup();
        assert!(
            names.len() >= 50,
            "expected the real ~55-biome reachable overworld set, got {}",
            names.len()
        );

        let mut actual: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for biome in names {
            let list = lodestone_worldgen::compose::build_biome_vegetation(&EmbeddedResolver, &biome);
            let mut reasons: Vec<String> = list
                .iter()
                .flat_map(|(_, placed)| lodestone_worldgen::feature::vegetation::collect_unsupported(placed))
                .collect();
            reasons.sort();
            reasons.dedup();
            actual.insert(biome, reasons);
        }

        let mismatches = vegetation_gap_mismatches(&actual);
        assert!(
            mismatches.is_empty(),
            "vegetation placer gap surface drifted from KNOWN_VEGETATION_GAPS — either a NEW \
             silent gap appeared (implement the placer or add it here, named) or a listed gap \
             was fixed (prune the entry):\n{}",
            mismatches.join("\n")
        );
    }

    /// Control for the gate above: an unsupported reason for a biome that
    /// ISN'T in [`KNOWN_VEGETATION_GAPS]` at all (`minecraft:desert` here,
    /// deliberately given a reason it doesn't really have) must be caught,
    /// proving [`vegetation_gap_mismatches`] actually fires rather than
    /// vacuously passing because nothing in-scope ever changes.
    #[test]
    fn vegetation_gap_mismatches_fires_on_an_undeclared_gap() {
        let mut actual = std::collections::BTreeMap::new();
        actual.insert(
            "minecraft:desert".to_string(),
            vec!["brand_new_unimplemented_placer".to_string(), "multiface_growth".to_string()],
        );
        let mismatches = vegetation_gap_mismatches(&actual);
        assert!(
            mismatches.iter().any(|m| m.contains("minecraft:desert") && m.contains("brand_new_unimplemented_placer")),
            "an undeclared new gap must be caught: {mismatches:?}"
        );
    }

    /// Second half of the same control: a biome/reason pair that's listed
    /// but no longer measured (i.e. the gap got fixed) must ALSO be caught —
    /// this is what keeps `KNOWN_VEGETATION_GAPS` from silently going stale
    /// in the direction that hides a real improvement.
    #[test]
    fn vegetation_gap_mismatches_fires_on_a_gap_that_was_fixed() {
        let mut actual = std::collections::BTreeMap::new();
        // `minecraft:desert`'s real entry is `["multiface_growth"]`; report
        // it as fully clean instead, simulating "multiface_growth got fixed".
        actual.insert("minecraft:desert".to_string(), Vec::<String>::new());
        let mismatches = vegetation_gap_mismatches(&actual);
        assert!(
            mismatches.iter().any(|m| m.contains("minecraft:desert")),
            "a listed gap that no longer measures must be caught: {mismatches:?}"
        );
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
    /// test uses, with no threading involved at all.
    ///
    /// **Made vacuous by `6509a97`'s pre-ore memoisation cache, now fixed.**
    /// `OverworldGenerator::pre_ore_cache` (`crates/lodestone-worldgen/src/overworld.rs`)
    /// is a field on the generator instance, keyed by exact `(cx, cz)`. This
    /// test used to call `column()` twice on *one* `generator`, so the
    /// second call was served straight out of the first call's cache entry
    /// — literally the same `Arc<PreOreResult>` — which guarantees identical
    /// bytes by **pointer identity**, not by `column()` being deterministic.
    /// A regression that reintroduced the historical palette-order bug (see
    /// `crate::overworld::OverworldGenerator::materialize_world`'s own doc
    /// comment — iterating a `surface_diff` `HashMap` directly instead of a
    /// fixed-order point lookup) would still pass this test, because both
    /// calls would still hit the one cached value.
    ///
    /// Fixed by building **two independently-constructed generators** —
    /// each gets its own empty cache, its own `HashMap<String, f32>`
    /// temperature table, its own everything — so a byte match here again
    /// means real determinism, not a shared cache entry. This is also the
    /// property a server restart actually needs: two separate process
    /// lifetimes must generate the same chunk from the same seed.
    ///
    /// If this passes, `OverworldGenerator::column` is a pure function of
    /// `(self.seed_and_settings, cx, cz)` as designed and the #414 failure
    /// is not a value-determinism bug in ore composition itself.
    #[test]
    fn column_is_byte_identical_across_two_independently_constructed_generators() {
        let generator_a = overworld_generator(42);
        let generator_b = overworld_generator(42);
        for &(cx, cz) in &[(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (2, -1)] {
            let a = generator_a.column(cx, cz);
            let b = generator_b.column(cx, cz);
            let (a_min_y, a_height, a_palette, a_blocks, a_biomes) = a.into_raw();
            let (b_min_y, b_height, b_palette, b_blocks, b_biomes) = b.into_raw();
            assert_eq!(
                a_min_y, b_min_y,
                "chunk ({cx},{cz}) min_y differs between two independently constructed generators"
            );
            assert_eq!(
                a_height, b_height,
                "chunk ({cx},{cz}) height differs between two independently constructed generators"
            );
            assert_eq!(
                a_palette, b_palette,
                "chunk ({cx},{cz}) palette differs between two independently constructed generators \
                 — a non-determinism bug or a palette-assignment-order difference, not threading"
            );
            assert_eq!(
                a_blocks, b_blocks,
                "chunk ({cx},{cz}) block indices differ between two independently constructed generators"
            );
            assert_eq!(
                a_biomes, b_biomes,
                "chunk ({cx},{cz}) biome quarts differ between two independently constructed generators"
            );
        }
    }

    /// End-to-end: real vegetation reaches the **served** column for a
    /// known plains chunk — closing the exact island CLAUDE.md's rule 1
    /// warns about, and the specific one this issue's own composition hit:
    /// `crate::feature::vegetation::VegGrid` used to store *and expose*
    /// chunk-local coordinates while every position the placement engine
    /// computes is absolute — so vegetation composed at construction time,
    /// ran without erroring, and placed **zero** blocks in every chunk
    /// except `(0, 0)` (`in_bounds`/`get` compared an absolute world
    /// coordinate against a `0..16` bound that was essentially always
    /// false). `crate::feature::vegetation`'s own hermetic unit tests never
    /// caught this because every one of them happened to use `origin =
    /// BlockPos { x: 8, y: 70, z: 8 }` — coincidentally already "local".
    /// Chunk `(18, -50)` (world `(300, -800)`) is the same known-plains
    /// fixture `biome_matches_vanilla_at_known_coordinates_seed_42` already
    /// names, so this isn't a freshly-picked coordinate chosen to make the
    /// test pass.
    #[test]
    fn vegetation_reaches_a_known_plains_chunk() {
        let generator = overworld_generator(42);
        let col = generator.column(18, -50);
        assert_eq!(col.biome_state(12, 0), "minecraft:plains");

        let mut grass = 0usize;
        for lz in 0..16usize {
            for lx in 0..16usize {
                for y in col.min_y()..col.min_y() + col.height() {
                    if col.block_state(lx, y, lz) == "minecraft:short_grass" {
                        grass += 1;
                    }
                }
            }
        }
        assert!(
            grass > 0,
            "a plains chunk composed through the served pipeline must carry grass"
        );
    }

    /// The issue's own requested gate (`#406`: "an aggregate-statistics
    /// gate (tree count per biome type within an expected band)"),
    /// predicted from the embedded placement JSON itself rather than a live
    /// vanilla dump (no JVM oracle for vegetation exists yet — see
    /// `crate::feature::vegetation`'s module doc). Two independent
    /// predictions, both computed *before* looking at the measured numbers
    /// (recorded here so a future reader can see the reasoning, not just
    /// the assertion):
    ///
    /// - **Grass upper bound**: `patch_grass_plain.json`'s outer
    ///   `noise_threshold_count` yields 5 or 10 attempts per chunk, each
    ///   feeding an inner `count: 32` — so at most `10 * 32 = 320` candidate
    ///   `short_grass` placements per chunk, before the final
    ///   `block_predicate_filter` (air) and `canSurvive` (support-block)
    ///   checks reject most of them. Measured must be `> 0` and comfortably
    ///   under `320 * chunk_count`.
    /// - **Oak logs**: `trees_plains.json`'s outer count is
    ///   `weighted_list{0: 19, 1: 1}`
    ///   (`IntProvider::expected_value() == 0.05`), and the `oak`
    ///   configured-feature branch of `trees_plains`'s `RandomSelector`
    ///   survives with probability `(1 - 0.33333334) * (1 - 0.0125) ≈
    ///   0.6579` (the `fancy_oak`/`fallen_oak` branches are
    ///   `ConfiguredFeature::Unsupported` — see module doc). A successful
    ///   straight oak trunk places `base_height=4` to `4+2=6` logs. So the
    ///   *isolated, single-chunk* expected oak-log count per chunk is
    ///   `0.05 * 0.6579 * (4..6) ≈ 0.132..0.197`, i.e. **not zero, and not
    ///   large** — over a 64-chunk sweep, `8.4..12.6` logs. Measured under
    ///   the pre-#427 single-chunk-only engine: `12`, inside that band.
    ///
    ///   **After issue #427's real 3×3 driver, measured: `6` — a real drop,
    ///   not a regression.** The isolated prediction above assumes each
    ///   swept chunk's tree placement reads only its OWN terrain; the real
    ///   3×3 driver now lets an edge-adjacent tree's space-check
    ///   (`place_tree`'s `getMaxFreeTreeHeight`-equivalent scan) read the
    ///   TRUE neighbour terrain at the tree's own absolute height instead of
    ///   the old clamped approximation (which just re-read the centre's own
    ///   nearest in-bounds column — usually open air above a similar
    ///   surface height, so it almost always reported "free"). Real terrain
    ///   height genuinely varies chunk to chunk; when a neighbour's surface
    ///   is taller than the centre's at the probed offset, the scan now sees
    ///   real solid ground where the old approximation saw air, and the tree
    ///   is correctly rejected instead of spuriously placed. Confirmed to be
    ///   this mechanism, not an unrelated defect, by re-running this exact
    ///   sweep with `LODESTONE_VEG_SINGLE_SOURCE_DEBUG=1` (the debug escape
    ///   hatch in `OverworldGenerator::vegetation_stage` that reverts to the
    ///   pre-#427 single-source-only pass): that reproduces `12`, exactly
    ///   the old measurement, with no other code changed — the entire delta
    ///   is attributable to the 3×3 driver's real neighbour reads, per
    ///   CLAUDE.md's evidence standard ("a control's premise" — here, that
    ///   flipping only the 3×3-vs-single-source toggle recovers the old
    ///   number — "proving the detector/mechanism actually fired").
    ///   This is an internal-consistency check against the engine's own
    ///   inputs, not vanilla parity (named explicitly, per
    ///   `crate::feature::vegetation`'s own module doc and this crate's
    ///   evidence standard) — the isolated band remains documented above as
    ///   a floor on what single-chunk-only placement alone would produce,
    ///   but the assertion below now widens to also accept the real,
    ///   measured 3×3 reduction rather than asserting a number this
    ///   docstring cannot re-derive analytically (real terrain height
    ///   variance has no closed form here) as if it could.
    #[test]
    fn plains_vegetation_counts_are_predicted_and_measured() {
        let generator = overworld_generator(42);
        // (18, -50) is world (300, -800), a known plains chunk
        // (`biome_matches_vanilla_at_known_coordinates_seed_42`).
        let base_cx = 18;
        let base_cz = -50;
        let sweep_chunks = 8 * 8;
        let mut grass = 0usize;
        let mut flowers = 0usize;
        let mut logs = 0usize;
        let mut leaves = 0usize;
        let mut plains_touching_chunks = 0usize;
        for dcx in 0..8 {
            for dcz in 0..8 {
                let cx = base_cx + dcx;
                let cz = base_cz + dcz;
                let col = generator.column(cx, cz);
                let mut any_plains = false;
                for lz in 0..16usize {
                    for lx in 0..16usize {
                        if col.biome_state(lx, lz) == "minecraft:plains" {
                            any_plains = true;
                        }
                        for y in col.min_y()..col.min_y() + col.height() {
                            let b = col.block_state(lx, y, lz);
                            let base = b.split('[').next().unwrap_or(b);
                            match base {
                                "minecraft:short_grass" => grass += 1,
                                "minecraft:dandelion" | "minecraft:poppy" | "minecraft:azure_bluet"
                                | "minecraft:oxeye_daisy" | "minecraft:cornflower"
                                | "minecraft:orange_tulip" | "minecraft:red_tulip"
                                | "minecraft:pink_tulip" | "minecraft:white_tulip" => flowers += 1,
                                "minecraft:oak_log" => logs += 1,
                                "minecraft:oak_leaves" => leaves += 1,
                                _ => {}
                            }
                        }
                    }
                }
                if any_plains {
                    plains_touching_chunks += 1;
                }
            }
        }
        // Anti-vacuity floor per CLAUDE.md's "world" vacuous-test species:
        // the sweep must actually contain plains, or every assertion below
        // would pass by both sides being empty.
        assert!(
            plains_touching_chunks > 0,
            "test's own premise failed: the 8x8 sweep from chunk ({base_cx},{base_cz}) contains \
             no plains — pick a different anchor before trusting anything below"
        );

        // Grass: measured must be positive, and bounded well under the
        // structural upper bound (10 outer * 32 inner = 320 candidates per
        // chunk, before survival checks).
        assert!(grass > 0, "measured zero grass over a plains-touching sweep");
        assert!(
            grass < 320 * sweep_chunks,
            "measured grass ({grass}) exceeds the structural upper bound \
             (320 candidates/chunk * {sweep_chunks} chunks) — the placement \
             pipeline is over-counting, not merely dense"
        );

        // Oak logs: predicted band from the JSON's own IntProvider, not a
        // guessed number — see this test's own doc comment for the
        // derivation. `0.05 * 0.6579 * 4 = 0.1316`, `* 6 = 0.1974`, times 64
        // chunks.
        let isolated_min = 0.05 * 0.6579 * 4.0 * sweep_chunks as f64;
        let isolated_max = 0.05 * 0.6579 * 6.0 * sweep_chunks as f64;
        // Issue #427: the real 3×3 driver's edge-adjacent space-check now
        // reads TRUE neighbour terrain (see this test's own doc comment for
        // the mechanism and the `LODESTONE_VEG_SINGLE_SOURCE_DEBUG=1`
        // control that isolated it), which can legitimately reject a tree
        // the old clamped approximation always let through — measured `6`,
        // half the pre-#427 measurement of `12`. The floor is loosened to
        // `0.25x` the isolated-model's own minimum (not lowered to the bare
        // `> 0` anti-vacuity floor above, which would make this assertion
        // vacuous against a real regression that drove logs to near-zero)
        // rather than re-centred on `6` itself, since `6` is one sample from
        // one real-terrain sweep, not a value with a closed-form derivation
        // this docstring could defend the way the isolated band's `8.4..
        // 12.6` is defended.
        let min = isolated_min * 0.25;
        let max = isolated_max * 1.5;
        assert!(
            (min..=max).contains(&(logs as f64)),
            "measured oak logs ({logs}) over {sweep_chunks} chunks is outside the band \
             [{min:.1}, {max:.1}] — the isolated single-chunk model predicts \
             [{isolated_min:.1}, {isolated_max:.1}] (trees_plains.json's own weighted_list \
             count and RandomSelector branch chances), widened downward for issue #427's real \
             3x3 driver rejecting more edge-adjacent trees against true neighbour terrain (see \
             this test's own doc comment) and upward for sampling noise across which of the \
             swept chunks actually resolve to plains at their own carver-source corner"
        );
        // A tree with logs must also carry leaves (the "not enough room"
        // gate and the log/leaf presence check in `place_tree` both require
        // this — see `crate::feature::vegetation::place_tree`).
        assert!(
            logs == 0 || leaves > 0,
            "measured {logs} oak logs but zero leaves — a real straight-trunk tree always \
             carries both"
        );
        // Flowers are gated behind a rarer noise_threshold_count + a
        // rarity_filter(32) on top — expect them present but sparse
        // relative to grass.
        assert!(flowers > 0, "measured zero flowers over a plains-touching sweep");
        assert!(
            flowers < grass,
            "flowers ({flowers}) should be sparser than grass ({grass}) given \
             flower_plains.json's extra rarity_filter(32) the grass pipeline lacks"
        );
    }

    /// `build_biome_vegetation` must resolve plains' real `trees_plains`/
    /// `flower_plains`/`patch_grass_plain` entries into the concrete
    /// [`ConfiguredFeature`](lodestone_worldgen::feature::vegetation::ConfiguredFeature)
    /// variants this engine actually implements — a construction-time
    /// regression control for the composition step
    /// [`plains_vegetation_counts_are_predicted_and_measured`] depends on:
    /// if any of these three silently degraded to `Unsupported`, that test
    /// would still measure *some* output from the other plains entries and
    /// could mask the regression.
    #[test]
    fn build_biome_vegetation_resolves_plains_grass_flower_and_tree() {
        use lodestone_worldgen::feature::vegetation::{BlockStateProvider, ConfiguredFeature};

        let list = lodestone_worldgen::compose::build_biome_vegetation(
            &EmbeddedResolver,
            "minecraft:plains",
        );
        assert!(!list.is_empty(), "plains must have a non-empty vegetal-decoration list");

        let grass_resolved = list.iter().any(|(_, p)| {
            matches!(
                &*p.feature,
                ConfiguredFeature::SimpleBlock(BlockStateProvider::Simple(s))
                    if s == "minecraft:short_grass"
            )
        });
        assert!(
            grass_resolved,
            "patch_grass_plain must resolve to SimpleBlock(Simple(\"minecraft:short_grass\")), \
             not Unsupported — entries: {list:?}"
        );

        let tree = list
            .iter()
            .find(|(_, p)| matches!(*p.feature, ConfiguredFeature::RandomSelector { .. }))
            .expect("trees_plains must resolve to a RandomSelector");
        if let ConfiguredFeature::RandomSelector { default, .. } = &*tree.1.feature {
            assert!(
                matches!(*default.feature, ConfiguredFeature::Tree(_)),
                "trees_plains' default branch must resolve to a real Tree, not Unsupported"
            );
        }
    }

    /// Regression control for the tag closures
    /// [`crate::feature::vegetation::place_simple_block`]'s `canSurvive`
    /// check and [`crate::feature::vegetation::place_tree`]'s space-check
    /// depend on — if `#minecraft:supports_vegetation`'s nested
    /// `#substrate_overworld` -> `#grass_blocks` chain ever stopped
    /// resolving (a tag file renamed, a resolver regression), every grass/
    /// flower placement in the real embedded data would silently reject at
    /// the `canSurvive` check, exactly as the coordinate-translation bug
    /// this issue's own history section describes did — this test exists
    /// so *that* failure mode has a direct, fast-failing check instead of
    /// only being visible through a 64-chunk sweep's aggregate count.
    #[test]
    fn embedded_veg_tags_resolve_grass_block_as_supporting_vegetation() {
        let tags = lodestone_worldgen::feature::vegetation::build_veg_tags(&EmbeddedResolver);
        assert!(
            tags.supports_vegetation.contains("minecraft:grass_block"),
            "supports_vegetation must include grass_block via \
             #supports_vegetation -> #substrate_overworld -> #grass_blocks"
        );
        assert!(!tags.replaceable_by_trees.is_empty());
        assert!(!tags.logs.is_empty());
        assert!(!tags.cannot_replace_below_tree_trunk.is_empty());
    }
}
