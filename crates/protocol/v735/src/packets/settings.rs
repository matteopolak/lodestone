//! Client-configuration serverbound packets for protocol 754 (Minecraft
//! 1.16.5).
//!
//! `BrandPayload`, `Settings` and `ResourcePackReceive` are byte-identical
//! to v340's own definitions (measured/verified), so they now live in
//! `lodestone-protocol-common`. `PlayerAbilities` stays defined **here**:
//! 1.16 reduced the serverbound packet to a single flags byte (the two
//! speed floats v47/v340 share were dropped), so it is not part of that
//! shared range. See `lodestone-protocol-common`'s
//! `packets::client_settings` module docs.

use lodestone_macros::{Decode, Encode, Packet};

pub use lodestone_protocol_common::packets::client_settings::{
    BrandPayload, ResourcePackReceive, Settings,
};

/// Serverbound `abilities` (player abilities) — the client toggling flight.
///
/// # 1.16 divergence
///
/// 1.16 reduced the serverbound packet to a **single flags byte**; the two
/// `f32` speed fields present in 1.8/1.12 were removed. The model's `SetFlying`
/// maps directly onto the flying bit with nothing dropped.
///
/// Wire layout: signed-byte flags (bit `0x02` = flying).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Server)]
pub struct PlayerAbilities {
    /// Ability flag bitset; bit `0x02` marks the client as flying.
    pub flags: i8,
}
