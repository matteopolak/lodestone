//! Public attribute id→name resolution for protocol 776.
//!
//! `update_attributes` carries each attribute as a network **registry id** (a
//! varint), not its identifier. That id→name mapping is version-specific data —
//! ids shift between releases as the registry changes — so it lives here in the
//! version crate, generated from Mojang's own `registries.json`, and never in a
//! shared crate. The generated array is the single source of truth; this module
//! is only the thin bounds-checked accessor over it.

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
