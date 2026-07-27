//! Public entity-type id→name resolution for protocol 776.
//!
//! `add_entity` carries the entity type as a network **registry id** (a
//! varint), not its identifier. That id→name mapping is version-specific data —
//! ids shift between releases as the registry grows — so it lives here in the
//! version crate, generated from Mojang's own `registries.json`, and never in a
//! shared crate. The generated array is the single source of truth; this module
//! is only the thin bounds-checked accessor over it.

use crate::generated_entity_types::ENTITY_TYPE_NAMES;
pub use crate::generated_entity_types::TYPE_COUNT;

/// Resolves a network entity-type id to its canonical `minecraft:*` identifier.
///
/// Returns `None` for ids outside `0..TYPE_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong type.
#[must_use]
pub fn entity_type_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ENTITY_TYPE_NAMES.get(index).copied())
}
