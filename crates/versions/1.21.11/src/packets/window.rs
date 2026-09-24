//! Inventory / window packets for this era (protocol 774).
//!
//! Nothing here comes from `lodestone-protocol-common`. Every packet in this
//! module either embeds this era's component-shaped [`Slot`] or carries a
//! window handle that is a **varint** at this protocol, where the 1.20.6 era
//! spells the same handle as an unsigned byte. A byte read of a varint handle
//! agrees for handles `0..=127` and then silently disagrees, which is exactly
//! the range a long-lived session walks into.

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};

use super::common::NetworkNbt;
use super::slot::Slot;

/// Clientbound `minecraft:open_screen` — asks the client to open a container
/// window.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_screen", state = Play, bound = Client, protocols = "774..=774")]
pub struct OpenScreen {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
    /// Menu type id from the `minecraft:menu` registry.
    #[mc(varint)]
    pub inventory_type: i32,
    /// Window title as a chat component.
    pub window_title: NetworkNbt,
}

/// Clientbound `minecraft:container_close` — the server closes a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_close", state = Play, bound = Client, protocols = "774..=774")]
pub struct ContainerClose {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
}

/// Clientbound `minecraft:container_set_data` — one window property, such as a
/// furnace's burn time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_set_data", state = Play, bound = Client, protocols = "774..=774")]
pub struct ContainerSetData {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
    /// Property index.
    pub property: i16,
    /// Property value.
    pub value: i16,
}

/// Clientbound `minecraft:set_held_slot` — the server moving the held hotbar
/// slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_held_slot", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetHeldSlot {
    /// Hotbar slot index, `0`-`8`.
    #[mc(varint)]
    pub slot: i32,
}

/// Serverbound `minecraft:container_close`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_close", state = Play, bound = Server, protocols = "774..=774")]
pub struct ServerboundContainerClose {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
}

/// Serverbound `minecraft:set_carried_item` — the client selecting a hotbar
/// slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_carried_item", state = Play, bound = Server, protocols = "774..=774")]
pub struct SetCarriedItem {
    /// Hotbar slot index, `0`-`8`.
    pub slot: i16,
}

/// Serverbound `minecraft:container_button_click` — the player picking an
/// enchantment offer or another indexed container button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_button_click", state = Play, bound = Server, protocols = "774..=774")]
pub struct ContainerButtonClick {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
    /// Which button (for an enchantment table, which of the three offers).
    #[mc(varint)]
    pub button_id: i32,
}

/// The serverbound form of a stack inside a container click.
///
/// The client does not echo a server-sent stack back verbatim at this
/// protocol: it sends the item id, the count and a *hashed* view of the
/// components, which the server compares against its own. Only the empty form
/// is modelled — a present stack needs both a numeric item id and the exact
/// component hash function, and a hash this crate computed differently from
/// the server's would be rejected as a stale click rather than erroring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HashedStack;

impl Encode for HashedStack {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        // The `option` byte, always absent.
        w.bool(false);
        Ok(())
    }
}

impl Decode for HashedStack {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        if r.bool()? {
            return Err(lodestone_core::Error::Custom(
                "a serverbound container click carrying a present stack is not modelled: it \
                 needs the server's own component hash function"
                    .to_owned(),
            ));
        }
        Ok(Self)
    }
}

/// One slot a click changed, as the client saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct ChangedSlot {
    /// Slot index.
    pub location: i16,
    /// The stack the client believes is now there.
    pub item: HashedStack,
}

/// Serverbound `minecraft:container_click` — the player clicks a slot.
///
/// The client sends the window's state id, **every slot the click changed**
/// with its resulting contents, and the resulting cursor stack, so the server
/// can accept or reject the whole outcome rather than replay the click.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_click", state = Play, bound = Server, protocols = "774..=774")]
pub struct ContainerClick {
    /// Window handle id.
    #[mc(varint)]
    pub window_id: i32,
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
    pub cursor_item: HashedStack,
}

/// Serverbound `minecraft:set_creative_mode_slot` — the creative-mode client
/// sets a slot's item directly.
///
/// Unlike a container click, this one carries a full stack rather than a
/// hashed one, so it embeds this era's [`Slot`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_creative_mode_slot", state = Play, bound = Server, protocols = "774..=774")]
pub struct SetCreativeModeSlot {
    /// Slot index being set.
    pub slot: i16,
    /// The item to place in the slot.
    pub item: Slot,
}
