//! Item id resolution for protocol 5.
//!
//! # What it is
//!
//! This era's [`Slot`](crate::packets::slot::Slot) carries a numeric item id
//! plus a `damage` value. This module resolves the **id** — the item's family —
//! and deliberately resolves nothing from `damage`.
//!
//! # Why damage is not resolved
//!
//! Before the Flattening, `damage` is two different things depending on the
//! item: a variant selector (wool colour, wood species, dye) for some, and
//! genuine wear or a potion effect for others. Turning it into a modern item
//! key needs a real id-plus-damage table with an outside source to check it
//! against; the source data available here names each metadata value's
//! *display string* ("Rose Red"), and a display string is not a key — modern's
//! equivalent of that one is `minecraft:red_dye`. So a dyed wool block resolves
//! to unspecified-colour wool rather than to a confidently wrong colour.
//!
//! # Where the table comes from
//!
//! `vendor/minecraft-data/data/pc/1.7/items.json`, whose sibling
//! `version.json` names 1.7.10 and protocol 5. That dataset is cross-check
//! grade in this repo, never an authority, so `tests/item_types.rs` checks a
//! sample of it against ids observed in a real protocol-5 `window_items`.

pub use crate::generated_item_types::ITEM_TYPE_COUNT;
use crate::generated_item_types::ITEM_TYPES;

/// Resolves a numeric item id to its canonical family name.
///
/// Returns `None` for an id absent from the table, so an unknown id surfaces
/// as an explicit miss rather than as a wrong item.
#[must_use]
pub fn item_name(id: i16) -> Option<&'static str> {
    let id = i32::from(id);
    ITEM_TYPES
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| ITEM_TYPES[index].1)
}
