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

/// Clientbound `open_window`.
///
/// Protocol 404 retains the string container type and slot count while using
/// the flattened post-1.13 item slot. A horse window is the sole conditional
/// tail, selected by the literal `EntityHorse` type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    /// Window handle.
    pub window_id: u8,
    /// Container type identifier.
    pub inventory_type: String,
    /// JSON chat title.
    pub window_title: String,
    /// Number of container-owned slots.
    pub slot_count: u8,
    /// Horse entity id, present only for `EntityHorse`.
    pub entity_id: Option<i32>,
}

impl lodestone_core::Decode for OpenWindow {
    fn decode(
        reader: &mut lodestone_core::Reader<'_>,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<Self> {
        let window_id = reader.u8()?;
        let inventory_type = reader.string(32_767)?;
        let window_title = reader.string(32_767)?;
        let slot_count = reader.u8()?;
        let entity_id = if inventory_type == "EntityHorse" {
            Some(reader.i32()?)
        } else {
            None
        };
        Ok(Self {
            window_id,
            inventory_type,
            window_title,
            slot_count,
            entity_id,
        })
    }
}

impl lodestone_core::Encode for OpenWindow {
    fn encode(
        &self,
        writer: &mut lodestone_core::Writer,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<()> {
        writer.u8(self.window_id);
        writer.string(&self.inventory_type);
        writer.string(&self.window_title);
        writer.u8(self.slot_count);
        if self.inventory_type == "EntityHorse" {
            writer.i32(self.entity_id.ok_or_else(|| {
                lodestone_core::Error::Custom("horse window needs an entity id".to_owned())
            })?);
        }
        Ok(())
    }
}

impl lodestone_core::Packet for OpenWindow {
    const NAME: &'static str = "minecraft:open_window";
    const STATE: lodestone_core::State = lodestone_core::State::Play;
    const BOUND: lodestone_core::Bound = lodestone_core::Bound::Client;
    const PROTOCOLS: lodestone_core::ProtocolRange = lodestone_core::ProtocolRange::new(404, 404);
}

/// Clientbound `window_items`, containing the complete menu snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:window_items", state = Play, bound = Client)]
pub struct WindowItems {
    /// Window handle.
    pub window_id: u8,
    /// Slots in menu order.
    #[mc(len = "i16")]
    pub items: Vec<Slot>,
}

/// Clientbound `set_slot`, containing one changed menu slot.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_slot", state = Play, bound = Client)]
pub struct SetSlot {
    /// Window handle, `0` for the player inventory and `-1` for the cursor.
    pub window_id: i8,
    /// Slot index.
    pub slot: i16,
    /// New contents.
    pub item: Slot,
}

/// Serverbound `window_click`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:window_click", state = Play, bound = Server)]
pub struct WindowClick {
    /// Window handle.
    pub window_id: u8,
    /// Clicked menu slot.
    pub slot: i16,
    /// Mouse button.
    pub button: i8,
    /// Legacy transaction/action counter.
    pub action: i16,
    /// Click mode.
    pub mode: i8,
    /// Pre-click slot contents reported by the client.
    pub item: Slot,
}

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
