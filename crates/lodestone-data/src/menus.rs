//! Public menu id→identifier resolution for protocol 776.
//!
//! `open_screen` carries the menu as a `minecraft:menu` registry id (a VarInt).
//! The id→name mapping is generated from Mojang's own `registries.json` for
//! 26.2, the one canonical internal version, so it lives here in this
//! data crate rather than in `lodestone-v770` — it is a
//! game-data census, not wire-format code.

pub use crate::generated_menus::MENU_COUNT;
use crate::generated_menus::MENU_NAMES;

/// Resolves a network menu id to its canonical `minecraft:*` identifier.
///
/// Returns `None` for ids outside `0..MENU_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong menu.
#[must_use]
pub fn menu_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| MENU_NAMES.get(index).copied())
}

/// Resolves a canonical `minecraft:*` menu identifier to its network registry
/// id for protocol 776.
///
/// This is the reverse of [`menu_name`], needed to encode `open_screen`
/// server-side. A linear scan over `MENU_COUNT` (25) entries is acceptable
/// here: it runs once per opened container, not per tick, matching the same
/// tradeoff `lodestone_data::items::item_id` already makes over its own
/// (much larger) generated table.
#[must_use]
pub fn menu_id(name: &str) -> Option<i32> {
    MENU_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| i32::try_from(index).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every generated entry round-trips both directions, and `menu_id` picks
    /// out exactly the id `menu_name` would resolve back — the control that
    /// [`menu_id`]'s reverse scan agrees with the forward table it scans,
    /// rather than e.g. an off-by-one that happens to compile.
    #[test]
    fn menu_id_and_menu_name_round_trip_every_generated_entry() {
        for id in 0..MENU_COUNT as i32 {
            let name = menu_name(id).unwrap_or_else(|| panic!("id {id} in 0..MENU_COUNT must resolve"));
            assert_eq!(menu_id(name), Some(id), "menu {name} (id {id}) did not round-trip");
        }
    }

    #[test]
    fn menu_id_resolves_the_furnace_family_and_hopper() {
        assert_eq!(menu_name(menu_id("minecraft:furnace").unwrap()), Some("minecraft:furnace"));
        assert!(menu_id("minecraft:smoker").is_some());
        assert!(menu_id("minecraft:blast_furnace").is_some());
        assert!(menu_id("minecraft:hopper").is_some());
    }

    /// **Control**: an unknown key must miss, not silently alias to some
    /// entry via a partial match.
    #[test]
    fn menu_id_rejects_an_unknown_key() {
        assert_eq!(menu_id("minecraft:not_a_real_menu"), None);
    }
}
