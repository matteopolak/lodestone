//! Client-configuration serverbound play packets for this era (protocol 774).
//!
//! `BrandPayload` comes from `lodestone-protocol-common`, whose definition
//! carries no protocol range at all — the one group this repo measured as
//! unchanged across every protocol it serves, and whose packet name
//! (`minecraft:custom_payload`) is the same identifier this era's generated id
//! table carries. Everything else is defined here: the shared client-settings
//! definition is declared `110..=754`, is named for the community dataset's
//! spelling rather than the jar's, and lacks this era's trailing
//! particle-status field.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

pub use lodestone_protocol_common::packets::client_settings::BrandPayload;

/// Serverbound `minecraft:client_information` in the play state.
///
/// # A polarity this repo cannot settle
///
/// `minecraft-data` names the 1.17 flag as if it *disables* filtering and the
/// 1.18 one as if it *enables* it — the same wire byte, described with
/// opposite senses one release apart, and no dump in this tree states which is
/// right. The framing does not depend on it (one byte either way) and no
/// server rejects either value, so the field is carried through from the model
/// as an opaque flag and named for the subject rather than the sense.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:client_information", state = Play, bound = Server, protocols = "774..=774")]
pub struct ClientInformation {
    /// Client locale, such as `en_us` (at most 16 characters).
    #[mc(max = 16)]
    pub locale: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility: `0` full, `1` commands only, `2` hidden.
    #[mc(varint)]
    pub chat_flags: i32,
    /// Whether chat colours are enabled.
    pub chat_colors: bool,
    /// Displayed skin-part bitmask.
    pub skin_parts: u8,
    /// Dominant hand: `0` left, `1` right.
    #[mc(varint)]
    pub main_hand: i32,
    /// The text-filtering flag — see the polarity note above.
    pub text_filtering: bool,
    /// Whether the player may be listed in the server's public player sample.
    pub allow_server_listing: bool,
    /// Particle detail level: `0` all, `1` decreased, `2` minimal.
    #[mc(varint)]
    pub particle_status: i32,
}

/// Serverbound `minecraft:player_abilities` — the client toggling flight.
///
/// A single flags byte (bit `0x02` = flying).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_abilities", state = Play, bound = Server, protocols = "774..=774")]
pub struct PlayerAbilities {
    /// Ability flag bitset; bit `0x02` marks the client as flying.
    pub flags: i8,
}

/// Serverbound `minecraft:resource_pack` — the client reporting the outcome of
/// a server-pushed pack.
///
/// The pack is named by **UUID**: a server may have several packs applied at
/// once and pushes and removes them individually, so the reply has to say
/// which one it is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:resource_pack", state = Play, bound = Server, protocols = "774..=774")]
pub struct ResourcePackReceive {
    /// The pack this reply is about.
    pub uuid: Uuid,
    /// Outcome: `0` loaded, `1` declined, `2` failed download, `3` accepted.
    #[mc(varint)]
    pub result: i32,
}
