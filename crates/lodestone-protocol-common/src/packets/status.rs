//! Status-state packets, byte-identical across every protocol these three
//! crates cover (47, 340, 754) -- server-list ping predates any of the
//! discontinuities in `docs/plans/multi-version-protocol-dedup.md`'s "Where
//! sharing genuinely breaks" section, so no `#[mc(protocols = ...)]` is
//! declared: these carry the derive's default `ProtocolRange::ALL`.
//! Measured identical by `cargo xtask protocol-dup`'s struct-identity scan
//! (same field list, same attrs) with no hand-written codec on either side
//! to hide a divergence behind that scan.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `ping_start` packet with an empty body, requesting the
/// server's status response.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:ping_start", state = Status, bound = Server)]
pub struct StatusRequest;

/// Clientbound `server_info` packet carrying the JSON status document.
///
/// Wire layout: a single JSON string.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:server_info", state = Status, bound = Client)]
pub struct StatusResponse {
    /// JSON-encoded server status document.
    pub response: String,
}

/// Serverbound `ping` packet echoing a client-chosen payload for latency
/// measurement.
///
/// Wire layout: a single big-endian 64-bit payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:ping", state = Status, bound = Server)]
pub struct StatusPing {
    /// Arbitrary client-chosen payload the server echoes back.
    pub time: i64,
}

/// Clientbound `ping` packet echoing the client's ping payload back.
///
/// Wire layout: a single big-endian 64-bit payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:ping", state = Status, bound = Client)]
pub struct StatusPong {
    /// Echoed payload matching the client's [`StatusPing`].
    pub time: i64,
}
