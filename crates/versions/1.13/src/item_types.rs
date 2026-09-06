//! Protocol-404 flattened item-registry resolution.
//!
//! A 1.13.2 slot carries one flat item id.  The id ordering is local to
//! protocol 404, so looking it up in a current registry would display a
//! plausible but unrelated item. `generated_item_types` is the complete
//! `minecraft-data` 1.13.2 census, committed for hermetic packet decoding and
//! checked against the vendored source by the ignored drift test.

pub use crate::generated_item_types::ITEM_TYPE_COUNT;
use crate::generated_item_types::ITEM_TYPES;

/// Resolves a protocol-404 item id to its canonical identifier.
#[must_use]
pub fn item_name(id: i32) -> Option<&'static str> {
    ITEM_TYPES
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| ITEM_TYPES[index].1)
}
