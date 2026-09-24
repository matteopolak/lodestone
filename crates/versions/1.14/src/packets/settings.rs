//! Client-configuration serverbound packets for protocol 754 (Minecraft
//! 1.16.5).
//!
//! `BrandPayload`, `Settings` and `ResourcePackReceive` are byte-identical
//! to v1-9's own definitions (measured/verified), so they now live in
//! `lodestone-protocol-common`. `PlayerAbilities` stays defined **here**:
//! 1.16 reduced the serverbound packet to a single flags byte (the two
//! speed floats v1-8/v1-9 share were dropped), so it is not part of that
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
/// `f32` speed fields 1.8 through 1.15 sent after it were removed. Both are
/// trailing fields that simply disappear, so an `until` predicate carries
/// them: 498 and 578 write nine bytes here and 754 writes one. The model's
/// `SetFlying` maps directly onto the flying bit with nothing dropped.
///
/// The two speeds are client *hints* the server ignores, so the values below
/// are the vanilla client's own defaults rather than anything derived: a
/// wrong pair would be accepted silently either way, which is why they are
/// not modelled as caller input.
///
/// Wire layout: signed-byte flags (bit `0x02` = flying), then — 498/578
/// only — f32 flying speed and f32 walking speed.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Server)]
pub struct PlayerAbilities {
    /// Ability flag bitset; bit `0x02` marks the client as flying.
    pub flags: i8,
    /// Flying speed hint. Removed in 1.16.
    #[mc(until = 578)]
    pub flying_speed: f32,
    /// Walking speed hint. Removed in 1.16.
    #[mc(until = 578)]
    pub walking_speed: f32,
}
