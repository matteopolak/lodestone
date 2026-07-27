//! Client-configuration serverbound packets for protocol 47.
//!
//! These carry the player's client options and are all ordinary derived
//! structs. The 1.8 wire shapes differ from later versions (no `main_hand`, a
//! signed-byte `chat_flags`, a two-`f32` `abilities` tail), so they are defined
//! here per-version rather than shared — the same shapes are duplicated in the
//! v340/v735 crates under the project's duplication-over-sharing rule.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `settings` (client settings).
///
/// # 1.8 divergence
///
/// 1.8 predates the off-hand, so there is **no** `main_hand` field (added in
/// 1.9), and `chat_flags` is a plain signed byte rather than the varint used
/// from 1.9 onward. The modern model's `text_filtering`, `allow_server_listing`
/// and `particle_status` fields have no 1.8 wire representation and are dropped
/// by the adapter.
///
/// Wire layout: string locale, signed-byte view distance, signed-byte chat
/// flags, bool chat colors, unsigned-byte displayed skin parts.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Play, bound = Server)]
pub struct Settings {
    /// Client locale, such as `en_us` (at most 16 characters).
    #[mc(max = 16)]
    pub locale: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility: `0` full, `1` commands only, `2` hidden.
    pub chat_flags: i8,
    /// Whether chat colors are enabled.
    pub chat_colors: bool,
    /// Displayed skin-part bitmask.
    pub skin_parts: u8,
}

/// Serverbound `custom_payload` carrying the client brand on the `MC|Brand`
/// channel.
///
/// # 1.8 divergence
///
/// 1.8 uses the legacy pipe-namespaced channel name `MC|Brand` (1.13 renamed it
/// to `minecraft:brand`). The brand itself is a length-prefixed string that
/// occupies the whole payload, so it is modelled as an ordinary trailing
/// `String` field rather than an opaque `restBuffer`.
///
/// Wire layout: string channel, string brand.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_payload", state = Play, bound = Server)]
pub struct BrandPayload {
    /// Plugin-message channel, always `MC|Brand` for protocol 47.
    #[mc(max = 32767)]
    pub channel: String,
    /// Client brand string.
    #[mc(max = 32767)]
    pub brand: String,
}

/// Serverbound `abilities` (player abilities) — the client toggling flight.
///
/// # 1.8 divergence
///
/// 1.8 (and 1.12) carry two trailing `f32` speed fields that the vanilla server
/// **ignores** for the serverbound direction (it only reads the flying bit);
/// 1.16 dropped them entirely. The model's `SetFlying` carries only the flying
/// state, so the adapter sends the vanilla default speeds for the two ignored
/// fields — they are constants, not an invented wire format, and never reach
/// game logic.
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
