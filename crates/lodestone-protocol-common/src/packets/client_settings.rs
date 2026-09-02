//! Client-configuration serverbound packets.
//!
//! [`BrandPayload`] is byte-identical across all three families' field
//! lists -- verified by hand (`cargo xtask protocol-dup`'s struct scan does
//! not report it: it compares field-level doc comments too, and each
//! family's `channel` field doc names its own protocol number/channel
//! spelling, which is enough to make the scan call it "different" even
//! though the wire shape is identical). No `#[mc(protocols = ...)]` is
//! declared; it keeps the derive's default `ProtocolRange::ALL`.
//!
//! [`PlayerAbilities`] is shared only between v47 and v340 (declared
//! `#[mc(protocols = "47..=340")]`): both carry two trailing `f32` speed
//! fields the vanilla server ignores serverbound; 1.16 (v735) dropped them
//! to a single flags byte.
//!
//! [`Settings`] and [`ResourcePackReceive`] are shared only between v340 and
//! v735 (declared `#[mc(protocols = "340..=754")]`): 1.8's `Settings`
//! (called `Settings` there too, but a different shape) has no `main_hand`
//! and a signed-byte `chat_flags` rather than a varint, and its
//! `resource_pack_receive` carries a leading pack-hash string this era's
//! does not.
//!
//! Note: `packets::game::ClientSettings` (a *different* struct, also
//! declaring `#[mc(name = "minecraft:settings", ...)]`) is identical across
//! all three families too, but is dead code in all of them -- every
//! family's adapter actually encodes the `Settings` type from this module
//! (or its own local one, for v47) for the real `minecraft:settings` wire
//! packet, and nothing reads `packets::game::ClientSettings` in production
//! or tests. It is intentionally left unmoved and unwired; see the
//! project's dead-code-removal backlog rather than treating its continued
//! presence here as an oversight of this move.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `custom_payload` carrying the client brand.
///
/// Wire layout: string channel, string brand. The channel is the legacy
/// pipe-namespaced `MC|Brand` pre-1.13 (v47, v340) and `minecraft:brand`
/// from 1.13 onward (v735) -- a runtime value, not a wire-shape difference,
/// so the struct itself is shared.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_payload", state = Play, bound = Server)]
pub struct BrandPayload {
    /// Plugin-message channel (`MC|Brand` pre-1.13, `minecraft:brand` from
    /// 1.13 onward).
    #[mc(max = 32767)]
    pub channel: String,
    /// Client brand string.
    #[mc(max = 32767)]
    pub brand: String,
}

/// Serverbound `abilities` (player abilities) -- the client toggling
/// flight. Shared only 47..=340 -- see the module docs.
///
/// Wire layout: signed-byte flags (bit `0x02` = flying), f32 flying speed,
/// f32 walking speed (both server-ignored serverbound, sent as vanilla
/// defaults).
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Server, protocols = "47..=340")]
pub struct PlayerAbilities {
    /// Ability flag bitset; bit `0x02` marks the client as flying.
    pub flags: i8,
    /// Flying speed -- server-ignored serverbound; sent as the vanilla default.
    pub flying_speed: f32,
    /// Walking speed -- server-ignored serverbound; sent as the vanilla default.
    pub walking_speed: f32,
}

/// Serverbound `settings` (client settings). Shared only 340..=754 -- see
/// the module docs.
///
/// Wire layout: string locale, signed-byte view distance, varint chat
/// flags, bool chat colors, unsigned-byte displayed skin parts, varint main
/// hand.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Play, bound = Server, protocols = "340..=754")]
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

/// Serverbound `resource_pack_receive` -- the client reports the outcome of
/// a server-pushed resource pack. Shared only 340..=754 -- see the module
/// docs.
///
/// Wire layout: varint result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:resource_pack_receive",
    state = Play,
    bound = Server,
    protocols = "340..=754"
)]
pub struct ResourcePackReceive {
    /// Outcome: `0` loaded, `1` declined, `2` failed download, `3` accepted.
    #[mc(varint)]
    pub result: i32,
}
