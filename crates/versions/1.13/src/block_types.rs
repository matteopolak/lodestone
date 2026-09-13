//! Protocol-404 block-type registry resolution for `block_action`.
//!
//! This is deliberately separate from the flat block-*state* canonical table:
//! block events name one registered block type, while terrain packets name a
//! state. The complete 1.13.2 type census is generated from the vendored data
//! source and committed for hermetic decoding.

use crate::generated_block_types::BLOCK_TYPES;

/// Resolves a protocol-404 block type id to its canonical identifier.
#[must_use]
pub fn block_type_name(id: i32) -> Option<&'static str> {
    BLOCK_TYPES
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| BLOCK_TYPES[index].1)
}
