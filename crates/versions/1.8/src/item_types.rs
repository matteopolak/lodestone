//! Public item-id→name resolution for protocol 47 (Minecraft 1.8.x).
//!
//! 1.8's `Slot` wire type (`crate::packets::slot::Slot`) carries a legacy
//! numeric item id plus a `damage` value. `damage` is a **metadata/variant**
//! selector for roughly a ninth of items (wool colour, dye colour, wood
//! type, stone type, …) and a genuine sub-type (tool durability, potion
//! effect) for the rest.
//!
//! This module resolves only the **id**, i.e. the item's *family* —
//! `minecraft-data`'s `pc/1.8/items.json` `name` field is already a clean
//! `snake_case` identifier (verified: every one of its 336 entries matches
//! `^[a-z0-9_]+$`, so no case-folding judgement call is needed here the way
//! `entity_types` needed one for CamelCase names). `damage` is deliberately
//! **not** resolved into a variant here: `items.json` names each metadata
//! value's *display string* (e.g. `"Black Wool"`, `"Rose Red"`), but a
//! display string is not a modern item key, and for a family like dye the
//! two have genuinely diverged (1.8's "Rose Red" is modern's
//! `minecraft:red_dye`) — turning that into a table without an outside
//! source to check it against is exactly the "predict the plausible round
//! number" mistake this project has paid for before. So every stack
//! currently resolves to its *base* item regardless of `damage`; a dyed
//! wool block in an inventory therefore shows as (unspecified-colour) wool
//! rather than the wrong colour. See `crate::adapter`'s slot-to-`ItemStack`
//! conversion for where this is applied, and treat a real legacy
//! id+damage → modern-key table (ideally sourced the way
//! `lodestone-canonical::flattening` was — a reflective JVM oracle dump
//! rather than a hand-typed table) as separate, larger follow-up work.

pub use crate::generated_item_types::ITEM_TYPE_COUNT;
use crate::generated_item_types::ITEM_TYPES;

/// Resolves a 1.8 `Slot` item id to its canonical family identifier.
///
/// Returns `None` for ids absent from the 1.8 item table, so an unknown id
/// surfaces as an explicit miss rather than a wrong item.
#[must_use]
pub fn item_name(id: i16) -> Option<&'static str> {
    let id = i32::from(id);
    ITEM_TYPES
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| ITEM_TYPES[index].1)
}
