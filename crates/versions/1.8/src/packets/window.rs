//! Inventory / window packets for protocol 47.
//!
//! Byte-identical to v1-9's own window packets (measured: only doc comments
//! differed). `CloseWindow`, `EnchantItem`, `HeldItemSlot`,
//! `ServerboundCloseWindow` and `ServerboundHeldItemSlot` carry no `Slot`
//! field and are shared with v1-14 too. `OpenWindow`, `SetCreativeSlot`,
//! `SetSlot`, `WindowClick` and `WindowItems` embed `Slot`, which is shared
//! only 47..=340 (the 1.13 flattening changed its wire shape), so those five
//! are shared with v1-9 only -- see `lodestone-protocol-common`'s
//! `packets::window` module docs.

pub use lodestone_protocol_common::packets::window::{
    CloseWindow, EnchantItem, HeldItemSlot, OpenWindow, ServerboundCloseWindow,
    ServerboundHeldItemSlot, SetCreativeSlot, SetSlot, WindowClick, WindowItems,
};
