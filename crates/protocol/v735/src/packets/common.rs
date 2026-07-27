//! Packets shared by both directions of the play state for protocol 754.

use lodestone_macros::{Decode, Encode, Packet};

/// Clientbound `keep_alive` challenge.
///
/// Wire layout: a single `i64` id.
///
/// # Architectural note
///
/// 1.8 sent the keep-alive id as a **varint**, but 1.9+ (protocol 754 is
/// 1.16.5) widened it to a fixed **64-bit** integer. The canonical model
/// (`ClientEvent::KeepAlive { id: i64 }` / `ClientAction::KeepAliveResponse {
/// id: i64 }`) already uses `i64`, so no conversion is required here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Client)]
pub struct KeepAliveRequest {
    /// Keep-alive id to echo back.
    pub id: i64,
}

/// Serverbound `keep_alive` response.
///
/// Wire layout: a single `i64` id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Server)]
pub struct KeepAliveResponse {
    /// Echoed keep-alive id.
    pub id: i64,
}
