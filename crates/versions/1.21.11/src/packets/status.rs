//! Status-state packets for this era (protocol 774).
//!
//! Status is the server-list-ping flow. It is not needed to join and stay
//! connected, but the four packets are trivially derive-expressible and are
//! included so the crate can answer a server ping.
//!
//! They are restated here rather than re-exported from
//! `lodestone-protocol-common`: the shared definitions carry the packet names
//! the community dataset uses (`minecraft:ping_start`, `minecraft:server_info`,
//! `minecraft:ping`), and this crate's id table is generated from the jar's own
//! packet report, which names the same four packets
//! `minecraft:status_request`, `minecraft:status_response`,
//! `minecraft:ping_request` and `minecraft:pong_response`. The wire bodies are
//! the same bytes; only the identifiers differ, and an identifier that does
//! not appear in the generated table cannot be resolved to an id.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `minecraft:status_request` with an empty body.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:status_request", state = Status, bound = Server, protocols = "774..=774")]
pub struct StatusRequest;

/// Clientbound `minecraft:status_response` carrying the JSON status document.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:status_response", state = Status, bound = Client, protocols = "774..=774")]
pub struct StatusResponse {
    /// JSON-encoded server status document.
    pub response: String,
}

/// Serverbound `minecraft:ping_request` echoing a client-chosen payload for
/// latency measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:ping_request", state = Status, bound = Server, protocols = "774..=774")]
pub struct StatusPing {
    /// Arbitrary client-chosen payload the server echoes back.
    pub time: i64,
}

/// Clientbound `minecraft:pong_response` echoing the client's payload back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:pong_response", state = Status, bound = Client, protocols = "774..=774")]
pub struct StatusPong {
    /// Echoed payload matching the client's [`StatusPing`].
    pub time: i64,
}
