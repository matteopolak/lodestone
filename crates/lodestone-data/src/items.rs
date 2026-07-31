//! Public item id→identifier resolution for protocol 776.
//!
//! Item stacks carry the item as a `Holder<Item>`: a VarInt referencing the
//! `minecraft:item` registry by id. The registry id→name mapping is generated
//! from Mojang's own `registries.json` for 26.2, the one canonical internal
//! version (#343), so it lives here in this data crate rather than in
//! `lodestone-v770` (issue #361) — it is a game-data census, not wire-format
//! code.

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

/// Resolves a canonical `minecraft:*` item identifier to its network registry
/// id for protocol 776.
///
/// This is the reverse of [`item_name`], needed to encode serverbound item
/// stacks (`container_click`, `set_creative_mode_slot`). A linear scan is
/// acceptable here: it runs once per encoded stack, not per tick, and keeping
/// it out of the generated table avoids hand-maintaining a second, easily
/// desynchronised index.
#[must_use]
pub fn item_id(name: &str) -> Option<i32> {
    ITEM_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| i32::try_from(index).ok())
}
