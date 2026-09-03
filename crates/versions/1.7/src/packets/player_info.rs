//! The protocol 5 `player_info` packet.
//!
//! # The one concept in this era with no canonical equivalent
//!
//! Every later protocol keys the player list by profile UUID. Protocol 5 does
//! not have one. The whole packet is three fields:
//!
//! ```text
//! string playerName    (the *display* name, colour codes and all)
//! bool   online        (true = add or update, false = remove)
//! i16    ping          (latency in milliseconds)
//! ```
//!
//! Measured from a real join: a 13-byte body for the name `Lodestone` — one
//! length byte, nine name bytes, one boolean, two ping bytes. There is no
//! profile, no properties, and therefore **no skin**: a remote player's
//! texture cannot be obtained from this packet at all, because the mechanism
//! that carries it does not exist yet.
//!
//! The canonical `PlayerListEntry` requires a `Uuid`, so translating this
//! packet means supplying one that the wire never sent. This module does not
//! decide that; it decodes what is there and leaves the choice to
//! [`crate::adapter`], where the reasoning and its limits are recorded next to
//! the code that makes it.
//!
//! One further consequence worth stating because it is easy to miss: the name
//! field is a *display* name. It may carry section-sign colour codes and it is
//! truncated to 16 characters by the server, so two players whose names differ
//! only past that limit are indistinguishable here.

use lodestone_macros::{Decode, Encode, Packet};

/// Clientbound `player_info` packet.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_info", state = Play, bound = Client)]
pub struct PlayerInfo {
    /// Display name, at most 16 characters, possibly with colour codes.
    #[mc(max = 16)]
    pub player_name: String,
    /// Whether the player is being added/updated rather than removed.
    pub online: bool,
    /// Reported latency in milliseconds.
    pub ping: i16,
}
