//! Client-configuration serverbound packets for protocol 340.
//!
//! These carry the player's client options and are all ordinary derived
//! structs. The 1.12 wire shapes differ from both 1.8 (which lacks `main_hand`
//! and uses a signed-byte `chat_flags`) and 1.16 (which dropped the two
//! `abilities` speed floats), so they are defined here per-version rather than
//! shared under the project's duplication-over-sharing rule.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `settings` (client settings).
///
/// # 1.12 divergence
///
/// 1.12 encodes `chat_flags` and `main_hand` as varints (1.8 used a signed byte
/// and had no `main_hand`). The modern model's `text_filtering`,
/// `allow_server_listing` and `particle_status` fields have no 1.12 wire
/// representation and are dropped by the adapter.
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

/// Serverbound `custom_payload` carrying the client brand on the `MC|Brand`
/// channel.
///
/// # 1.12 divergence
///
/// 1.12 (protocol 340) predates the 1.13 channel rename, so the channel is the
/// legacy pipe-namespaced `MC|Brand`. The brand is a length-prefixed string that
/// occupies the whole payload, modelled as an ordinary trailing `String`.
///
/// Wire layout: string channel, string brand.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_payload", state = Play, bound = Server)]
pub struct BrandPayload {
    /// Plugin-message channel, always `MC|Brand` for protocol 340.
    #[mc(max = 32767)]
    pub channel: String,
    /// Client brand string.
    #[mc(max = 32767)]
    pub brand: String,
}

/// Serverbound `abilities` (player abilities) — the client toggling flight.
///
/// # 1.12 divergence
///
/// 1.12 carries two trailing `f32` speed fields that the vanilla server
/// **ignores** for the serverbound direction (1.16 dropped them). The model's
/// `SetFlying` carries only the flying state, so the adapter sends the vanilla
/// default speeds for the two ignored fields.
///
/// Wire layout: signed-byte flags (bit `0x02` = flying), f32 flying speed, f32
/// walking speed.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Server)]
pub struct PlayerAbilities {
    /// Ability flag bitset; bit `0x02` marks the client as flying.
    pub flags: i8,
    /// Flying speed — server-ignored serverbound; sent as the vanilla default.
    pub flying_speed: f32,
    /// Walking speed — server-ignored serverbound; sent as the vanilla default.
    pub walking_speed: f32,
}

/// Serverbound `resource_pack_receive` — the client reports the outcome of a
/// server-pushed resource pack.
///
/// # 1.12 divergence
///
/// Unlike 1.8 (which prefixes the pack hash string), 1.12 sends **only** the
/// result varint. The response is therefore encodable from the model without
/// the pack hash the model's Uuid-keyed variant cannot supply.
///
/// Wire layout: varint result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:resource_pack_receive", state = Play, bound = Server)]
pub struct ResourcePackReceive {
    /// Outcome: `0` loaded, `1` declined, `2` failed download, `3` accepted.
    #[mc(varint)]
    pub result: i32,
}
