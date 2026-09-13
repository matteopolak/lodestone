//! Inventory / window packets for this era (protocol 766).
//!
//! Nothing here comes from `lodestone-protocol-common`. Every packet in this
//! module either embeds this era's component-shaped [`Slot`] — a different
//! wire type from both the pre-1.13 `(id, damage)` slot and the 1.13-through-1.20.4
//! `(id, count, NBT)` one — or changed shape at this era in its own right:
//! the window handle became an unsigned byte with its own reserved values,
//! `window_items` and `set_slot` gained the state id that pairs a click with
//! the server's own view of the window, and `open_window`'s title became
//! anonymous NBT.

use lodestone_macros::{Decode, Encode, Packet};

use super::common::NetworkNbt;
use super::slot::Slot;

/// Clientbound `open_window` — asks the client to open a container window.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_window", state = Play, bound = Client, protocols = "766..=766")]
pub struct OpenWindow {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
    /// Menu type id from the `minecraft:menu` registry.
    #[mc(varint)]
    pub inventory_type: i32,
    /// Window title as a chat component.
    pub window_title: NetworkNbt,
}

/// Clientbound `window_items` — the full contents of a window.
///
/// Two things separate this from the eras below: the item array is prefixed
/// by a **varint** count rather than a signed `i16`, and the carried
/// (cursor) stack is appended after the array.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:window_items", state = Play, bound = Client, protocols = "766..=766")]
pub struct WindowItems {
    /// Window handle id.
    pub window_id: u8,
    /// The server's revision counter for this window; echoed back on a click
    /// so the server can detect a client acting on a stale view.
    #[mc(varint)]
    pub state_id: i32,
    /// Every slot in the window, in slot order.
    pub items: Vec<Slot>,
    /// The stack held by the cursor.
    pub carried_item: Slot,
}

/// Clientbound `set_slot` — updates a single slot.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_slot", state = Play, bound = Client, protocols = "766..=766")]
pub struct SetSlot {
    /// Window handle id.
    pub window_id: u8,
    /// The window's revision counter — see [`WindowItems::state_id`].
    #[mc(varint)]
    pub state_id: i32,
    /// Slot index.
    pub slot: i16,
    /// New slot contents.
    pub item: Slot,
}

/// Clientbound `close_window` — the server closes a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:close_window", state = Play, bound = Client, protocols = "766..=766")]
pub struct CloseWindow {
    /// Window handle id.
    pub window_id: u8,
}

/// Clientbound `craft_progress_bar` — one window property, such as a
/// furnace's burn time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:craft_progress_bar", state = Play, bound = Client, protocols = "766..=766")]
pub struct CraftProgressBar {
    /// Window handle id.
    pub window_id: u8,
    /// Property index.
    pub property: i16,
    /// Property value.
    pub value: i16,
}

/// Clientbound `held_item_slot` — the server moving the held hotbar slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:held_item_slot", state = Play, bound = Client, protocols = "766..=766")]
pub struct HeldItemSlot {
    /// Hotbar slot index, `0`-`8`.
    pub slot: i8,
}

/// Serverbound `close_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:close_window", state = Play, bound = Server, protocols = "766..=766")]
pub struct ServerboundCloseWindow {
    /// Window handle id.
    pub window_id: u8,
}

/// Serverbound `held_item_slot` — the client selecting a hotbar slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:held_item_slot", state = Play, bound = Server, protocols = "766..=766")]
pub struct ServerboundHeldItemSlot {
    /// Hotbar slot index, `0`-`8`.
    pub slot: i16,
}

/// Serverbound `enchant_item` — the player picking an enchantment offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:enchant_item", state = Play, bound = Server, protocols = "766..=766")]
pub struct EnchantItem {
    /// Window handle id.
    pub window_id: i8,
    /// Which of the three offers was chosen.
    pub enchantment: i8,
}

/// One slot a click changed, as the client saw it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ChangedSlot {
    /// Slot index.
    pub location: i16,
    /// The stack the client believes is now there.
    pub item: Slot,
}

/// Serverbound `window_click` — the player clicks a slot.
///
/// The era below sends a single transaction id and the clicked stack. Here
/// the client sends the window's state id, **every slot the click changed**
/// with its resulting contents, and the resulting cursor stack, so the server
/// can accept or reject the whole outcome rather than replay the click.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:window_click", state = Play, bound = Server, protocols = "766..=766")]
pub struct WindowClick {
    /// Window handle id.
    pub window_id: u8,
    /// The window revision this click was made against.
    #[mc(varint)]
    pub state_id: i32,
    /// Clicked slot index.
    pub slot: i16,
    /// Mouse button used.
    pub button: i8,
    /// Click mode (normal, shift, number key, …).
    #[mc(varint)]
    pub mode: i32,
    /// Slots the click changed, as the client resolved them.
    pub changed_slots: Vec<ChangedSlot>,
    /// The resulting cursor stack.
    pub cursor_item: Slot,
}

/// Serverbound `set_creative_slot` — the creative-mode client sets a slot's
/// item directly.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_creative_slot", state = Play, bound = Server, protocols = "766..=766")]
pub struct SetCreativeSlot {
    /// Slot index being set.
    pub slot: i16,
    /// The item to place in the slot.
    pub item: Slot,
}
