//! Handshake-state packets for protocol 5.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `set_protocol` handshake opening a connection.
///
/// Wire layout: varint protocol version, string host, unsigned short port,
/// varint next state. Measured identical to protocol 47's, but defined here
/// rather than shared: this is the packet that *declares* the protocol
/// number, so it is the one definition an era should own outright.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_protocol", state = Handshaking, bound = Server)]
pub struct SetProtocol {
    /// Protocol version the client is requesting.
    #[mc(varint)]
    pub protocol_version: i32,
    /// Host string as typed by the user.
    #[mc(max = 255)]
    pub server_host: String,
    /// TCP port.
    pub server_port: u16,
    /// Requested next state: `1` status, `2` login.
    #[mc(varint)]
    pub next_state: i32,
}
