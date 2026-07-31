//! Public menu id→identifier resolution for protocol 776.
//!
//! `open_screen` carries the menu as a `minecraft:menu` registry id (a VarInt).
//! That id→name mapping is version-specific data — ids shift as the registry
//! grows — so it lives here in the version crate, generated from Mojang's own
//! `registries.json`, never in a shared crate.

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
