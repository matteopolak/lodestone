//! Client-configuration serverbound packets for protocol 754 (Minecraft
//! 1.16.5).
//!
//! These carry the player's client options and are all ordinary derived
//! structs. 1.16 renamed the brand channel to `minecraft:brand` and reduced the
//! serverbound `abilities` packet to a single flags byte (the two speed floats
//! present in 1.8/1.12 were dropped), so the shapes are defined here per-version
//! under the project's duplication-over-sharing rule.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `settings` (client settings).
///
/// # 1.16 divergence
///
/// 1.16 encodes `chat_flags` and `main_hand` as varints. The modern model's
/// `text_filtering`, `allow_server_listing` and `particle_status` fields have no
/// 1.16 wire representation and are dropped by the adapter.
///
/// Wire layout: string locale, signed-byte view distance, varint chat flags,
/// bool chat colors, unsigned-byte displayed skin parts, varint main hand.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Play, bound = Server)]
pub struct Settings {
    /// Client locale, such as `en_us` (at most 16 characters).
    #[mc(max = 16)]
    pub locale: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility: `0` full, `1` commands only, `2` hidden.
    #[mc(varint)]
    pub chat_flags: i32,
    /// Whether chat colors are enabled.
    pub chat_colors: bool,
    /// Displayed skin-part bitmask.
    pub skin_parts: u8,
    /// Dominant hand: `0` left, `1` right.
    #[mc(varint)]
    pub main_hand: i32,
}

/// Serverbound `custom_payload` carrying the client brand on the
/// `minecraft:brand` channel.
///
/// # 1.16 divergence
///
/// 1.13 renamed the legacy `MC|Brand` channel to `minecraft:brand`; 1.16 uses
/// the new name. The brand is a length-prefixed string that occupies the whole
/// payload, modelled as an ordinary trailing `String`.
///
/// Wire layout: string channel, string brand.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_payload", state = Play, bound = Server)]
pub struct BrandPayload {
    /// Plugin-message channel, always `minecraft:brand` for protocol 754.
    #[mc(max = 32767)]
    pub channel: String,
    /// Client brand string.
    #[mc(max = 32767)]
    pub brand: String,
}

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

/// Serverbound `resource_pack_receive` — the client reports the outcome of a
/// server-pushed resource pack.
///
/// # 1.16 divergence
///
/// 1.16 sends **only** the result varint (no pack hash), so the response is
/// encodable from the model without the pack hash the Uuid-keyed variant cannot
/// supply.
///
/// Wire layout: varint result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:resource_pack_receive", state = Play, bound = Server)]
pub struct ResourcePackReceive {
    /// Outcome: `0` loaded, `1` declined, `2` failed download, `3` accepted.
    #[mc(varint)]
    pub result: i32,
}
