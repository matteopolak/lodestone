//! Inventory / window packets for this era (protocols 756 and 758).
//!
//! `CloseWindow`, `EnchantItem`, `HeldItemSlot`, `ServerboundCloseWindow`
//! and `ServerboundHeldItemSlot` carry no `Slot` field and are byte-identical
//! to v1-8's and v1-9's own definitions (measured), so they now live in
//! `lodestone-protocol-common` and are re-exported below.
//!
//! `OpenWindow`, `SetSlot`, `WindowItems`, `WindowClick` and
//! `SetCreativeSlot` stay defined **here**: 1.14 replaced `OpenWindow`'s
//! whole shape (a flat `(varint window id, varint menu type, chat title)`
//! triple, no slot count, no horse special case), and the other four embed
//! this crate's own post-1.13-flattening `Slot`, which is a different wire
//! type from the pre-1.13 `Slot` v1-8/v1-9 share -- see
//! `lodestone-protocol-common`'s `packets::slot` and `packets::window`
//! module docs.

use lodestone_macros::{Decode, Encode, Packet};

use super::slot::Slot;

pub use lodestone_protocol_common::packets::window::{
    CloseWindow, EnchantItem, HeldItemSlot, ServerboundCloseWindow, ServerboundHeldItemSlot,
};

/// Clientbound `open_window` — asks the client to open a container window.
///
/// # 1.14+ shape
///
/// 1.14 replaced the pre-1.13 `(u8 id, string type, chat title, u8 slot count,
/// [horse entity id])` shape with a flat `(varint window id, varint menu type,
/// chat title)` triple. The window's slot count is implied by the menu type, and
/// the horse-inventory special case (a trailing entity id) is gone — horse
/// inventories use the generic menu registry — so this is now a plain derived
/// struct with no conditional tail.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_window", state = Play, bound = Client)]
pub struct OpenWindow {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
    /// Menu type id from the `minecraft:menu` registry.
    #[mc(varint)]
    pub inventory_type: i32,
    /// Window title as a JSON chat component.
    #[mc(max = 32767)]
    pub window_title: String,
}

/// Clientbound `window_items` — the full contents of a window.
///
/// The item array is prefixed by a signed `i16` count (not the modern varint).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:window_items", state = Play, bound = Client)]
pub struct WindowItems {
    /// Window handle id.
    pub window_id: u8,
    /// Every slot in the window, in slot order.
    #[mc(len = "i16")]
    pub items: Vec<Slot>,
}

/// Clientbound `set_slot` — updates a single slot.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_slot", state = Play, bound = Client)]
pub struct SetSlot {
    /// Window handle id (`-1` = cursor, `0` = player inventory).
    pub window_id: i8,
    /// Slot index.
    pub slot: i16,
    /// New slot contents.
    pub item: Slot,
}

/// Serverbound `window_click` — the player clicks a slot.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:window_click", state = Play, bound = Server)]
pub struct WindowClick {
    /// Window handle id.
    pub window_id: u8,
    /// Clicked slot index.
    pub slot: i16,
    /// Mouse button used.
    pub button: i8,
    /// Transaction id (echoed by the server in a confirm packet).
    pub action: i16,
    /// Click mode (normal, shift, number key, …).
    pub mode: i8,
    /// The item that was in the clicked slot (for the server to verify).
    pub item: Slot,
}

/// Serverbound `set_creative_slot` — the creative-mode client sets a slot's item
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_creative_slot", state = Play, bound = Server)]
pub struct SetCreativeSlot {
    /// Slot index being set.
    pub slot: i16,
    /// The item to place in the slot.
    pub item: Slot,
}
