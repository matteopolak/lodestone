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

/// Canonicalises `path` against the census, returning the table's own `'static`
/// copy of it rather than the caller's borrow. Useful when a caller holds a
/// borrowed or owned (e.g. `format!`-built) string and needs a `'static` one back
/// to store on a value with no borrow to hold — [`super::potion::potion_name`]
/// solves the analogous problem for potions by returning a network id instead,
/// which is not available here (see the module doc for why).
#[must_use]
pub fn canonical_path(path: &str) -> Option<&'static str> {
    ENCHANTMENTS.iter().find(|e| e.path == path).map(|e| e.path)
}

/// `enchantment.minecraft.<path>` (`en_us.json`) — the enchantment's own display
/// name, e.g. `"Sharpness"`, `"Curse of Binding"`. Two of the 43 do not follow
/// `humanize(path)` at all (`binding_curse` -> "Curse of Binding",
/// `vanishing_curse` -> "Curse of Vanishing" — the noun moves to the front), so
/// this is transcribed verbatim rather than derived.
#[must_use]
pub fn display_name(path: &str) -> Option<&'static str> {
    Some(match path {
        "aqua_affinity" => "Aqua Affinity",
        "bane_of_arthropods" => "Bane of Arthropods",
        "binding_curse" => "Curse of Binding",
        "blast_protection" => "Blast Protection",
        "breach" => "Breach",
        "channeling" => "Channeling",
        "density" => "Density",
        "depth_strider" => "Depth Strider",
        "efficiency" => "Efficiency",
        "feather_falling" => "Feather Falling",
        "fire_aspect" => "Fire Aspect",
        "fire_protection" => "Fire Protection",
        "flame" => "Flame",
        "fortune" => "Fortune",
        "frost_walker" => "Frost Walker",
        "impaling" => "Impaling",
        "infinity" => "Infinity",
        "knockback" => "Knockback",
        "looting" => "Looting",
        "loyalty" => "Loyalty",
        "luck_of_the_sea" => "Luck of the Sea",
        "lunge" => "Lunge",
        "lure" => "Lure",
        "mending" => "Mending",
        "multishot" => "Multishot",
        "piercing" => "Piercing",
        "power" => "Power",
        "projectile_protection" => "Projectile Protection",
        "protection" => "Protection",
        "punch" => "Punch",
        "quick_charge" => "Quick Charge",
        "respiration" => "Respiration",
        "riptide" => "Riptide",
        "sharpness" => "Sharpness",
        "silk_touch" => "Silk Touch",
        "smite" => "Smite",
        "soul_speed" => "Soul Speed",
        "sweeping_edge" => "Sweeping Edge",
        "swift_sneak" => "Swift Sneak",
        "thorns" => "Thorns",
        "unbreaking" => "Unbreaking",
        "vanishing_curse" => "Curse of Vanishing",
        "wind_burst" => "Wind Burst",
        _ => return None,
    })
}

/// `#minecraft:curse` (`.cache/mc/26.2/src/data/minecraft/tags/enchantment/curse.json`)
/// — exactly two entries. `Enchantment.getFullname` colours a cursed enchantment's
/// lore line red instead of the ordinary gray.
#[must_use]
pub fn is_curse(path: &str) -> bool {
    matches!(path, "binding_curse" | "vanishing_curse")
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

    /// Every census entry must resolve a display name and a canonical path — a
    /// missing arm in [`display_name`]'s `match` would otherwise surface only as
    /// a silent `None` for whichever enchantment was added last.
    #[test]
    fn every_census_entry_resolves_a_display_name() {
        let mut missing = Vec::new();
        for entry in ENCHANTMENTS.iter() {
            if display_name(entry.path).is_none() {
                missing.push(entry.path);
            }
            if canonical_path(entry.path) != Some(entry.path) {
                missing.push(entry.path);
            }
        }
        assert!(missing.is_empty(), "{missing:#?}");
    }

    /// The two discriminating names: one that follows the ordinary
    /// `humanize(path)` shape, and the one class (`_curse` suffix) that reorders
    /// the words entirely — a formula-based fallback would get this wrong while
    /// looking plausible.
    #[test]
    fn curse_names_do_not_follow_the_humanized_path_shape() {
        assert_eq!(display_name("sharpness"), Some("Sharpness"));
        assert_eq!(display_name("binding_curse"), Some("Curse of Binding"));
        assert_eq!(display_name("vanishing_curse"), Some("Curse of Vanishing"));
    }

    /// Exactly the two curse-tagged enchantments in this build's 43-entry census
    /// — every other entry, including `binding_curse`'s alphabetical neighbours,
    /// must read as `false`.
    #[test]
    fn exactly_two_enchantments_are_curses() {
        let curses: Vec<&str> = ENCHANTMENTS.iter().map(|e| e.path).filter(|p| is_curse(p)).collect();
        assert_eq!(curses, vec!["binding_curse", "vanishing_curse"]);
    }
}
