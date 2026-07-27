//! Inventory / window packets for protocol 754 (Minecraft 1.16.5).
//!
//! These carry the [`Slot`](super::slot::Slot) item type and are all ordinary
//! derived structs.

use lodestone_macros::{Decode, Encode, Packet};

use super::slot::Slot;

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

/// Clientbound `held_item_slot` — the server sets the player's selected hotbar
/// slot.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:held_item_slot", state = Play, bound = Client)]
pub struct HeldItemSlot {
    /// Selected hotbar index (`0..=8`).
    pub slot: i8,
}

/// Clientbound `close_window` — the server forces a window closed.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:close_window", state = Play, bound = Client)]
pub struct CloseWindow {
    /// Window handle id being closed.
    pub window_id: u8,
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

/// Serverbound `close_window` — the player closes a window.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:close_window", state = Play, bound = Server)]
pub struct ServerboundCloseWindow {
    /// Window handle id being closed.
    pub window_id: u8,
}

/// Serverbound `held_item_slot` — the player changes hotbar selection.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:held_item_slot", state = Play, bound = Server)]
pub struct ServerboundHeldItemSlot {
    /// Newly selected hotbar index (`0..=8`).
    pub slot: i16,
}

/// Serverbound `enchant_item` — the player clicks a non-slot menu button, such
/// as an enchanting-table option or a lectern page turn.
///
/// 1.8 through 1.16 share this exact `{windowId, button}` shape, so the model's
/// `ContainerButtonClick { window_id, button_id }` maps onto it directly with no
/// item registry or transaction id involved.
///
/// Wire layout: signed-byte window id, signed-byte button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:enchant_item", state = Play, bound = Server)]
pub struct EnchantItem {
    /// Open window handle id.
    pub window_id: i8,
    /// Button id defined by the open menu type.
    pub button: i8,
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
