//! The 66 `minecraft:worldgen/biome` ids shipped as 26.2's own base data —
//! `/execute if biome`'s census.
//!
//! # Why this is a hand-listed array, not a generated table
//!
//! Every other id census in this crate (`block`, `item`, `entity_types`, …)
//! resolves a **network id** — a registry ordinal carried on the wire — so it
//! has to be generated from `registries.json` to stay byte-exact with the
//! protocol. A biome is never referred to by a network id anywhere this
//! crate's callers touch it: `/execute if biome` compares a *string*
//! (`ChunkSource::biome_state_at`'s return value) against a parsed
//! `minecraft:*` identifier, and the client never receives a per-block biome
//! id over the wire in a form this crate encodes. So there is no ordinal to
//! keep in sync — only "is this string a real biome" — and a plain sorted
//! array is the whole of what that needs.
//!
//! # Data source
//!
//! The 66 filenames under `data/minecraft/worldgen/biome/*.json` in 26.2's
//! own generated/client data (CLAUDE.md data-source #1 — Mojang's own
//! generator, not a community dataset) — the same "vanilla ships this as base
//! data, not a datapack addition" category
//! [`crate::block`]/`lodestone_server::loot`'s bundled loot tables are in.
//! Every biome's own worldgen definition (climate parameters, surface rules,
//! feature list) is **not** modelled here; this is only the name census
//! `/execute if biome` needs to validate a bare `<biome>` argument at parse
//! time, the same posture [`crate::entity_types::entity_type_id`] takes for
//! `/summon`.
//!
//! # How to change it
//!
//! A biome added or removed by a future version bump is a diff against the
//! same `data/minecraft/worldgen/biome/` listing — re-list the directory and
//! update [`BIOME_NAMES`]. `tests::the_census_matches_the_generated_directory`
//! is a regenerate-or-assert gate the same shape as this crate's other
//! `LODESTONE_REGEN=1` tests, guarded on `.cache` being present.

/// Every biome id 26.2 ships as base data, path-only (the `minecraft:`
/// namespace is implicit — every entry here is `minecraft:*`), sorted for a
/// stable [`is_biome`] scan and reproducible [`all`] iteration.
pub const BIOME_NAMES: [&str; 66] = [
    "badlands",
    "bamboo_jungle",
    "basalt_deltas",
    "beach",
    "birch_forest",
    "cherry_grove",
    "cold_ocean",
    "crimson_forest",
    "dark_forest",
    "deep_cold_ocean",
    "deep_dark",
    "deep_frozen_ocean",
    "deep_lukewarm_ocean",
    "deep_ocean",
    "desert",
    "dripstone_caves",
    "end_barrens",
    "end_highlands",
    "end_midlands",
    "eroded_badlands",
    "flower_forest",
    "forest",
    "frozen_ocean",
    "frozen_peaks",
    "frozen_river",
    "grove",
    "ice_spikes",
    "jagged_peaks",
    "jungle",
    "lukewarm_ocean",
    "lush_caves",
    "mangrove_swamp",
    "meadow",
    "mushroom_fields",
    "nether_wastes",
    "ocean",
    "old_growth_birch_forest",
    "old_growth_pine_taiga",
    "old_growth_spruce_taiga",
    "pale_garden",
    "plains",
    "river",
    "savanna",
    "savanna_plateau",
    "small_end_islands",
    "snowy_beach",
    "snowy_plains",
    "snowy_slopes",
    "snowy_taiga",
    "soul_sand_valley",
    "sparse_jungle",
    "stony_peaks",
    "stony_shore",
    "sulfur_caves",
    "sunflower_plains",
    "swamp",
    "taiga",
    "the_end",
    "the_void",
    "warm_ocean",
    "warped_forest",
    "windswept_forest",
    "windswept_gravelly_hills",
    "windswept_hills",
    "windswept_savanna",
    "wooded_badlands",
];

/// Whether `namespace:path` (or a bare `path`, defaulting to `minecraft`) names
/// a real 26.2 biome — `/execute if biome`'s parse-time validation, the same
/// posture [`crate::block::Block::from_name`]/[`crate::entity_types::entity_type_id`]
/// take for their own registries.
#[must_use]
pub fn is_biome(qualified: &str) -> bool {
    qualified
        .strip_prefix("minecraft:")
        .is_some_and(|path| BIOME_NAMES.binary_search(&path).is_ok())
}

/// Every biome id, namespace-qualified — [`lodestone_command_mc::BiomeArg`]'s
/// own suggestion list reaches through this rather than re-deriving one.
pub fn all() -> impl Iterator<Item = String> {
    BIOME_NAMES.iter().map(|name| format!("minecraft:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_array_is_sorted_for_binary_search() {
        let mut sorted = BIOME_NAMES;
        sorted.sort_unstable();
        assert_eq!(BIOME_NAMES, sorted, "BIOME_NAMES must stay sorted for is_biome's binary_search");
    }

    #[test]
    fn a_real_biome_validates_with_and_without_the_namespace() {
        assert!(is_biome("minecraft:plains"));
        assert!(is_biome("minecraft:the_end"));
        assert!(!is_biome("plains"), "is_biome takes a qualified id, unlike the McArg layer above it");
    }

    #[test]
    fn an_unknown_biome_and_a_foreign_namespace_both_fail() {
        assert!(!is_biome("minecraft:not_a_real_biome"));
        assert!(!is_biome("modded:custom_biome"));
    }

    #[test]
    fn all_yields_every_entry_namespace_qualified() {
        let all: Vec<String> = all().collect();
        assert_eq!(all.len(), BIOME_NAMES.len());
        assert!(all.contains(&"minecraft:plains".to_string()));
    }

    /// Regenerate-or-assert against the real 26.2 data directory, the same
    /// shape as this crate's other `LODESTONE_REGEN=1` gates — guarded on the
    /// cache being present, since it is not committed to the repo.
    #[test]
    fn the_census_matches_the_generated_directory() {
        let dir = std::path::Path::new(
            "../../.cache/mc/26.2/client-src/data/minecraft/worldgen/biome",
        );
        if !dir.exists() {
            eprintln!("skipping: {} not present in this checkout", dir.display());
            return;
        }
        let mut found: Vec<String> = std::fs::read_dir(dir)
            .expect("read the biome directory")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    path.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        found.sort();
        let expected: Vec<String> = BIOME_NAMES.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            found, expected,
            "BIOME_NAMES has drifted from data/minecraft/worldgen/biome — regenerate this module's array"
        );
    }
}
