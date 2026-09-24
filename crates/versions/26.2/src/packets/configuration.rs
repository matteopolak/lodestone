//! Configuration-state packets for protocol 776.

use lodestone_macros::{Decode, Encode, Packet};

/// A single known resource-pack entry exchanged during configuration.
///
/// Wire layout: string namespace, string id, string version.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct KnownPack {
    /// Pack namespace, such as `minecraft`.
    pub namespace: String,
    /// Pack id, such as `core`.
    pub id: String,
    /// Pack version string.
    pub version: String,
}

/// Clientbound `select_known_packs` packet advertising the server's built-in
/// resource packs.
///
/// Wire layout: a varint-prefixed list of [`KnownPack`] values.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:select_known_packs", state = Configuration, bound = Client)]
pub struct ClientboundKnownPacks {
    /// Packs the server knows about.
    pub packs: Vec<KnownPack>,
}

/// Serverbound `select_known_packs` reply listing the packs the client also
/// has. Phase 1 always replies with an empty list.
///
/// Wire layout: a varint-prefixed list of [`KnownPack`] values.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:select_known_packs", state = Configuration, bound = Server)]
pub struct ServerboundKnownPacks {
    /// Packs the client acknowledges.
    pub packs: Vec<KnownPack>,
}

/// Clientbound `code_of_conduct` packet (new in the 26.x configuration flow).
///
/// Wire layout: a single string carrying the code-of-conduct text.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:code_of_conduct", state = Configuration, bound = Client)]
pub struct CodeOfConduct {
    /// Code-of-conduct text presented to the player.
    pub text: String,
}

/// Serverbound `accept_code_of_conduct` packet with an empty body. The 26.2
/// wire format carries no hash or payload; acceptance is signalled purely by
/// the packet id.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:accept_code_of_conduct", state = Configuration, bound = Server)]
pub struct AcceptCodeOfConduct;

/// Serverbound `finish_configuration` acknowledgement with an empty body, sent
/// to leave configuration and enter play.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:finish_configuration", state = Configuration, bound = Server)]
pub struct FinishConfiguration;
