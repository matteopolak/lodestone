//! Inventory / window packets for protocol 404 (Minecraft 1.13.2).
//!
//! `CloseWindow`, `EnchantItem`, `HeldItemSlot`, `ServerboundCloseWindow` and
//! `ServerboundHeldItemSlot` carry no `Slot` field and are byte-identical
//! across every protocol the legacy families cover (measured), so they live
//! in `lodestone-protocol-common` and are re-exported below.
//!
//! [`SetCreativeSlot`] stays defined **here**. Its field list is textually
//! identical to the shared 47..=340 version, but it embeds a `Slot`, and
//! `Slot` is exactly what 1.13 changed: the pre-1.13 form leads with a signed
//! `i16` item id (`-1` = empty) and carries a separate `damage` short, while
//! 1.13.1 onward leads with a `present` boolean and a single flat VarInt item
//! id. That is the class of divergence a field-list comparison cannot see —
//! two structs that look identical and cannot read each other's bytes — so
//! the shared definition's range stops at 340 and this crate keeps its own.

use lodestone_macros::{Decode, Encode, Packet};

use super::slot::Slot;

pub use lodestone_protocol_common::packets::window::{
    CloseWindow, EnchantItem, HeldItemSlot, ServerboundCloseWindow, ServerboundHeldItemSlot,
};

/// Serverbound `set_creative_slot` — the creative-mode client sets a slot's
/// item directly.
///
/// Wire layout: `i16` slot index, then a post-flattening [`Slot`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_creative_slot", state = Play, bound = Server)]
pub struct SetCreativeSlot {
    /// Slot index being set.
    pub slot: i16,
    /// The item to place in the slot.
    pub item: Slot,
}
