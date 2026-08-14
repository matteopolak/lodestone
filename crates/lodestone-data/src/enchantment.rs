//! Public enchantment census: names and level ranges, with **no network id**.
//!
//! # What it is
//!
//! `minecraft:enchantment` is data-driven (`data/minecraft/enchantment/*.json`), so
//! unlike [`crate::potion`] or [`crate::mob_effects`] its network registry id is
//! assigned per-connection by configuration-phase registry sync order, not a fixed
//! protocol id — it is absent from `registries.json` entirely (see
//! [`crate::generated_enchantments`]'s module doc). This module therefore answers only
//! session-independent questions: what enchantments exist, and what levels each one
//! spans. A caller needing the *current session's* network id for an enchantment must
//! get it from that session's own registry sync, not from here.
//!
//! # How it works
//!
//! [`ENCHANTMENTS`] is `Enchantment.getMinLevel()`/`getMaxLevel()` for every
//! `data/minecraft/enchantment/*.json` file, ordered alphabetically by registry path —
//! the same order `CreativeModeTabs.generateEnchantmentBookTypesOnlyMaxLevel` walks
//! (see [`crate::generated_enchantments`]'s doc for why that is the vanilla order).

pub use crate::generated_enchantments::{ENCHANTMENT_COUNT, ENCHANTMENTS, EnchantmentCensus};

/// The full `minecraft:*` identifier and max level for one enchantment, by registry
/// path (no `minecraft:` prefix, e.g. `"sharpness"`).
#[must_use]
pub fn max_level(path: &str) -> Option<u8> {
    ENCHANTMENTS.iter().find(|e| e.path == path).map(|e| e.max_level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_is_unique_and_alphabetical() {
        let paths: Vec<&str> = ENCHANTMENTS.iter().map(|e| e.path).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "ENCHANTMENTS must be alphabetical by path");
        let mut deduped = paths.clone();
        deduped.dedup();
        assert_eq!(paths.len(), deduped.len(), "duplicate enchantment path");
        assert_eq!(paths.len(), ENCHANTMENT_COUNT as usize);
    }

    #[test]
    fn max_level_resolves_known_and_unknown_paths() {
        assert_eq!(max_level("sharpness"), Some(5));
        assert_eq!(max_level("mending"), Some(1));
        assert_eq!(max_level("not_a_real_enchantment"), None);
    }
}
