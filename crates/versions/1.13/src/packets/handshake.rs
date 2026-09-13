//! Handshake-state packets for protocol 404.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `set_protocol` packet that opens every connection and declares
/// the state the client wishes to enter next.
///
/// Wire layout: varint protocol version, string host, unsigned short port,
/// varint next-state (`1` = status, `2` = login). This is structurally
/// identical to the modern handshake but is duplicated here rather than
/// shared, per the strict version-isolation rule.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_protocol", state = Handshaking, bound = Server)]
pub struct SetProtocol {
    /// Protocol version the client speaks (404 for 1.13.2).
    #[mc(varint)]
    pub protocol_version: i32,
    /// Hostname or IP literal the client used to connect.
    #[mc(max = 255)]
    pub server_host: String,
    /// Server port the client connected to.
    pub server_port: u16,
    /// Requested next connection state (`2` for login).
    #[mc(varint)]
    pub next_state: i32,
}
