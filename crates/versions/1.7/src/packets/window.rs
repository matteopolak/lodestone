//! Container (window) packets for protocol 5.

use lodestone_macros::{Decode, Encode, Packet};

use super::slot::Slot;

/// Clientbound `open_window`.
///
/// # The inventory type is a string
///
/// The container kind arrives as an identifier string such as
/// `minecraft:chest` or `minecraft:furnace`, not the varint menu-registry id
/// later protocols use. There is no menu registry yet, so the adapter maps
/// the string plus the slot count onto a canonical menu key.
///
/// `entity_id` is present only for a horse inventory, and its presence
/// depends on the *value* of `inventory_type`, so the field is decoded by
/// hand rather than declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    /// Window handle the server will use for this container.
    pub window_id: u8,
    /// Container kind identifier.
    pub inventory_type: String,
    /// Title, already resolved to a display string by the server.
    pub window_title: String,
    /// Slots in the container's own inventory, excluding the player's.
    pub slot_count: u8,
    /// Whether `window_title` is a literal rather than a translation key.
    pub use_provided_title: bool,
    /// Horse entity id, present only for a horse inventory.
    pub entity_id: Option<i32>,
}

/// Container kind whose `open_window` carries a trailing entity id.
const HORSE_INVENTORY: &str = "EntityHorse";

impl lodestone_core::Decode for OpenWindow {
    fn decode(
        reader: &mut lodestone_core::Reader<'_>,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<Self> {
        let window_id = reader.u8()?;
        let inventory_type = reader.string(32)?;
        let window_title = reader.string(32)?;
        let slot_count = reader.u8()?;
        let use_provided_title = reader.bool()?;
        let entity_id = if inventory_type == HORSE_INVENTORY {
            Some(reader.i32()?)
        } else {
            None
        };
        Ok(Self {
            window_id,
            inventory_type,
            window_title,
            slot_count,
            use_provided_title,
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
        writer.bool(self.use_provided_title);
        if let Some(entity_id) = self.entity_id {
            writer.i32(entity_id);
        }
        Ok(())
    }
}

impl lodestone_core::Packet for OpenWindow {
    const NAME: &'static str = "minecraft:open_window";
    const STATE: lodestone_core::State = lodestone_core::State::Play;
    const BOUND: lodestone_core::Bound = lodestone_core::Bound::Client;
    const PROTOCOLS: lodestone_core::ProtocolRange = lodestone_core::ProtocolRange::new(5, 5);
}

/// Clientbound `close_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:close_window", state = Play, bound = Client)]
pub struct CloseWindow {
    /// Window being closed.
    pub window_id: u8,
}

/// Serverbound `close_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:close_window", state = Play, bound = Server)]
pub struct ServerboundCloseWindow {
    /// Window being closed.
    pub window_id: u8,
}

/// Clientbound `set_slot`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_slot", state = Play, bound = Client)]
pub struct SetSlot {
    /// Window handle, `0` for the player inventory, `-1` for the cursor.
    pub window_id: i8,
    /// Slot index within the window.
    pub slot: i16,
    /// The stack now in that slot.
    pub item: Slot,
}

/// Clientbound `window_items`.
///
/// The array count is an `i16`; protocol 47 keeps that, but the stacks inside
/// have this era's shape, so the packet cannot be shared.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:window_items", state = Play, bound = Client)]
pub struct WindowItems {
    /// Window handle.
    pub window_id: u8,
    /// Every slot in the window, in slot order.
    #[mc(len = "i16")]
    pub items: Vec<Slot>,
}

/// Clientbound `craft_progress_bar`: a container's progress property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:craft_progress_bar", state = Play, bound = Client)]
pub struct CraftProgressBar {
    /// Window handle.
    pub window_id: u8,
    /// Property index.
    pub property: i16,
    /// Property value.
    pub value: i16,
}

/// Clientbound `transaction`: a container action's accept or reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:transaction", state = Play, bound = Client)]
pub struct Transaction {
    /// Window handle.
    pub window_id: i8,
    /// Action counter this answers.
    pub action: i16,
    /// Whether the action was accepted.
    pub accepted: bool,
}

/// Clientbound `held_item_slot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:held_item_slot", state = Play, bound = Client)]
pub struct HeldItemSlot {
    /// Hotbar index, `0..=8`.
    pub slot: i8,
}

/// Serverbound `held_item_slot`.
///
/// An `i16` here, against the clientbound packet's `i8` — the two directions
/// of the same name genuinely disagree in this era.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:held_item_slot", state = Play, bound = Server)]
pub struct ServerboundHeldItemSlot {
    /// Hotbar index, `0..=8`.
    pub slot_id: i16,
}

/// Serverbound `set_creative_slot`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_creative_slot", state = Play, bound = Server)]
pub struct SetCreativeSlot {
    /// Slot index.
    pub slot: i16,
    /// Stack to place there.
    pub item: Slot,
}

/// Serverbound `enchant_item`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:enchant_item", state = Play, bound = Server)]
pub struct EnchantItem {
    /// Window handle.
    pub window_id: i8,
    /// Which of the three offers was chosen.
    pub enchantment: i8,
}
