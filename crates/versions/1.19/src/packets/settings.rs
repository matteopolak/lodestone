//! Client-configuration serverbound packets for this era (protocol 762).
//!
//! `BrandPayload` and `ResourcePackReceive` are byte-identical to the eras
//! below (measured), so they live in `lodestone-protocol-common`.
//! `PlayerAbilities` and [`Settings`] stay defined **here**: the abilities
//! packet lost its two speed floats at 1.16, and the settings packet grew a
//! trailing flag at 1.17 and a second one at 1.18, so neither is inside the
//! shared range. See `lodestone-protocol-common`'s
//! `packets::client_settings` module docs.

use lodestone_macros::{Decode, Encode, Packet};

pub use lodestone_protocol_common::packets::client_settings::{BrandPayload, ResourcePackReceive};

/// Serverbound `settings` (client information) for this era.
///
/// The shared 110..=754 definition ends two releases short of this crate:
/// 1.17 appended a text-filtering flag and 1.18 appended a server-listing
/// flag after it. Both are present unconditionally here, and 1.19 changed
/// nothing further — measured from `minecraft-data`, the packet's shape is
/// identical at 758 and 762.
///
/// # A polarity this repo cannot settle
///
/// `minecraft-data` names the 1.17 flag as if it *disables* filtering and the
/// 1.18 one as if it *enables* it — the same wire byte, described with
/// opposite senses one release apart, and no dump in this tree states which
/// is right. The framing does not depend on it (one byte either way) and no
/// server rejects either value, so the field is carried through from the
/// model as an opaque flag and named for the subject rather than the sense.
/// Settling it needs a server-side oracle, which is recorded here rather
/// than guessed at.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Play, bound = Server, protocols = "762..=762")]
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
    /// The text-filtering flag added in 1.17 — see the polarity note above.
    pub text_filtering: bool,
    /// Whether the player may be listed in the server's public player
    /// sample. Added in 1.18.
    pub allow_server_listing: bool,
}

/// Serverbound `abilities` (player abilities) — the client toggling flight.
///
/// # 1.16 divergence
///
/// 1.16 reduced the serverbound packet to a **single flags byte**; the two
/// `f32` speed fields 1.8 through 1.15 sent after it were removed, and 1.19
/// changed nothing further. The pre-1.16 eras write nine bytes here and this
/// one writes one. The model's `SetFlying` maps directly onto the flying bit
/// with nothing dropped.
///
/// The two speeds are client *hints* the server ignores, so the values below
/// are the vanilla client's own defaults rather than anything derived: a
/// wrong pair would be accepted silently either way, which is why they are
/// not modelled as caller input.
///
/// Wire layout: a single signed-byte flags field (bit `0x02` = flying). The
/// two speed floats the pre-1.16 eras carry were dropped before this one
/// begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Server, protocols = "762..=762")]
pub struct PlayerAbilities {
    /// Ability flag bitset; bit `0x02` marks the client as flying.
    pub flags: i8,
}
