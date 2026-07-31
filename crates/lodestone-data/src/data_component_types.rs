//! Public data-component-type id→identifier resolution for protocol 776.
//!
//! An item stack's `DataComponentPatch` names each added or removed component by
//! a `minecraft:data_component_type` registry id (a VarInt). The id→name
//! mapping is generated from Mojang's own `registries.json` for 26.2, the one
//! canonical internal version (#343), so it lives here in this data crate
//! rather than in `lodestone-v770` (issue #361) — it is a game-data census,
//! not wire-format code.

pub use crate::generated_data_component_types::DATA_COMPONENT_TYPE_COUNT;
use crate::generated_data_component_types::DATA_COMPONENT_TYPE_NAMES;

/// Resolves a network data-component-type registry id to its canonical
/// `minecraft:*` identifier.
///
/// Returns `None` for ids outside `0..DATA_COMPONENT_TYPE_COUNT`, so a malformed
/// or future-version component id surfaces as an explicit miss (an unmodeled
/// component) rather than a panic.
#[must_use]
pub fn component_type_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| DATA_COMPONENT_TYPE_NAMES.get(index).copied())
}
