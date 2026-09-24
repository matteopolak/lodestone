//! Protocol-766 registry-id bridges used by packets whose wire values name
//! registry entries rather than carrying identifiers.

use crate::generated_registry;

fn name(entries: &'static [(i32, &'static str)], id: i32) -> Option<&'static str> {
    entries
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| entries[index].1)
}

/// Resolves a protocol-766 item registry id to its canonical key.
#[must_use]
pub fn item_name(id: i32) -> Option<&'static str> {
    name(&generated_registry::ITEMS, id)
}

/// Resolves a protocol-766 attribute registry id to its wire key.
#[must_use]
pub fn attribute_name(id: i32) -> Option<&'static str> {
    name(&generated_registry::ATTRIBUTES, id)
}

/// Resolves a protocol-766 block registry id to its canonical key.
#[must_use]
pub fn block_name(id: i32) -> Option<&'static str> {
    name(&generated_registry::BLOCKS, id)
}
