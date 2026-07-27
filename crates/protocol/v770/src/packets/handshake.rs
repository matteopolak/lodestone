//! Handshake-state packets for protocol 776.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `intention` packet that opens every connection and declares the
/// state the client wishes to enter next.
///
/// Wire layout: varint protocol version, string host (max 255 chars), unsigned
/// short port, varint next-state (`1` = status, `2` = login).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:intention", state = Handshaking, bound = Server)]
pub struct Intention {
    /// Protocol version the client speaks.
    #[mc(varint)]
    pub protocol_version: i32,
    /// Hostname or IP literal the client used to connect.
    #[mc(max = 255)]
    pub host: String,
    /// Server port the client connected to.
    pub port: u16,
    /// Requested next connection state (`2` for login).
    #[mc(varint)]
    pub next_state: i32,
}
