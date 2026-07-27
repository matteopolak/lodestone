//! Public item id→identifier resolution for protocol 776.
//!
//! Item stacks carry the item as a `Holder<Item>`: a VarInt referencing the
//! `minecraft:item` registry by id. The registry id→name mapping is
//! version-specific data — ids shift as the registry grows — so it lives here in
//! the version crate, generated from Mojang's own `registries.json`, never in a
//! shared crate.

pub use crate::generated_items::ITEM_COUNT;
use crate::generated_items::ITEM_NAMES;

/// Resolves a network item registry id to its canonical `minecraft:*`
/// identifier.
///
/// Returns `None` for ids outside `0..ITEM_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong item.
#[must_use]
pub fn item_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ITEM_NAMES.get(index).copied())
}
