//! Public data-component-type id→identifier resolution for protocol 776.
//!
//! An item stack's `DataComponentPatch` names each added or removed component by
//! a `minecraft:data_component_type` registry id (a VarInt). The id→name mapping
//! is version-specific data — ids shift as components are added between
//! versions — so it lives here in the version crate, generated from Mojang's own
//! `registries.json`, never in a shared crate.

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
