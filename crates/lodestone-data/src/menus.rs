//! Public menu id→identifier resolution for protocol 776.
//!
//! `open_screen` carries the menu as a `minecraft:menu` registry id (a VarInt).
//! The id→name mapping is generated from Mojang's own `registries.json` for
//! 26.2, the one canonical internal version (#343), so it lives here in this
//! data crate rather than in `lodestone-v770` (issue #361) — it is a
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
