//! Handshake-state packets for this era (protocol 766).

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `set_protocol` packet that opens every connection and declares
/// the state the client wishes to enter next.
///
/// Wire layout: varint protocol version, string host, unsigned short port,
/// varint next-state (`1` = status, `2` = login). Structurally identical to
/// every other era's handshake and duplicated here rather than shared, per
/// the strict version-isolation rule.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_protocol", state = Handshaking, bound = Server, protocols = "766..=766")]
pub struct SetProtocol {
    /// Protocol version the client speaks (766 for 1.20.5 and 1.20.6).
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
