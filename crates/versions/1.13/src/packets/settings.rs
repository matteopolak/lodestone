//! Client-configuration serverbound packets for protocol 404 (Minecraft
//! 1.13.2).
//!
//! All four are shared. `BrandPayload`, `Settings` and `ResourcePackReceive`
//! were already ranged `110..=754`, which covers 404. `PlayerAbilities` was
//! ranged `47..=340` and is widened to `47..=404` here: 1.13 does not touch
//! it (the packet is absent from the 1.12.2 -> 1.13.2 shape diff), and 1.16
//! is where the two trailing `f32` speed hints were dropped. See
//! `lodestone-protocol-common`'s `packets::client_settings` module docs.

pub use lodestone_protocol_common::packets::client_settings::{
    BrandPayload, PlayerAbilities, ResourcePackReceive, Settings,
};
