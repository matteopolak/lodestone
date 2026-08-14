//! Public attribute id→name resolution for protocol 776.
//!
//! `update_attributes` carries each attribute as a network **registry id** (a
//! varint), not its identifier. The id→name mapping is generated from Mojang's
//! own `registries.json` for 26.2, the one canonical internal version,
//! so it lives here in this data crate rather than in `lodestone-v770` —
//! it is a game-data census, not wire-format code, so a version-free
//! consumer can read it with no protocol dependency. The generated array is
//! the single source of truth; this module is only the thin bounds-checked
//! accessor over it.

pub use crate::generated_attribute_types::ATTRIBUTE_COUNT;
use crate::generated_attribute_types::ATTRIBUTE_NAMES;

/// Resolves a network attribute id to its canonical `minecraft:*` identifier.
///
/// Returns `None` for ids outside `0..ATTRIBUTE_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong attribute.
#[must_use]
pub fn attribute_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ATTRIBUTE_NAMES.get(index).copied())
}
