//! Public menu id→identifier resolution for protocol 776.
//!
//! `open_screen` carries the menu as a `minecraft:menu` registry id (a VarInt).
//! The id→name mapping is generated from Mojang's own `registries.json` for
//! 26.2, the one canonical internal version, so it lives here in this
//! data crate rather than in `lodestone-v26-2` — it is a
//! game-data census, not wire-format code.

pub use crate::generated_menus::MENU_COUNT;
use crate::generated_menus::MENU_NAMES;

/// A validated entry in the 26.2 menu registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MenuId(i32);

impl MenuId {
    /// Validates a raw network registry id at a wire or import boundary.
    #[must_use]
    pub const fn new(raw: i32) -> Option<Self> {
        if raw >= 0 && raw < MENU_COUNT as i32 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// The registry id used by the version-specific wire codec.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// Resolves a validated network menu id to its canonical `minecraft:*`
/// identifier.
///
/// Raw values are validated by [`MenuId::new`] before they enter this total
/// lookup, so a malformed or future-version id remains an explicit miss at
/// the boundary rather than a silently wrong menu.
#[must_use]
pub fn menu_name(id: MenuId) -> &'static str {
    MENU_NAMES[id.raw() as usize]
}

/// Resolves a canonical `minecraft:*` menu identifier to its network registry
/// id for protocol 776.
///
/// This is the reverse of [`menu_name`], needed to encode `open_screen`
/// server-side. A linear scan over `MENU_COUNT` (25) entries is acceptable
/// here: it runs once per opened container, not per tick.
#[must_use]
pub fn menu_id(name: &str) -> Option<MenuId> {
    MENU_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| i32::try_from(index).ok())
        .and_then(MenuId::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every generated entry round-trips both directions. This checks the
    /// reverse scan, while the integration suite supplies literal registry-id
    /// controls that do not derive either expected value from this table.
    #[test]
    fn menu_id_and_menu_name_round_trip_every_generated_entry() {
        for id in 0..MENU_COUNT as i32 {
            let id = MenuId::new(id).expect("table id validates");
            let name = menu_name(id);
            assert_eq!(menu_id(name), Some(id), "menu {name} ({id:?}) did not round-trip");
        }
    }

    #[test]
    fn menu_id_resolves_the_furnace_family_and_hopper() {
        assert_eq!(menu_name(menu_id("minecraft:furnace").unwrap()), "minecraft:furnace");
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
