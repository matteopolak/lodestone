//! Packets shared by both directions of the play state for protocol 47.

use lodestone_macros::{Decode, Encode, Packet};

/// Clientbound `keep_alive` challenge.
///
/// Wire layout: a single varint id.
///
/// # Architectural note
///
/// In 1.8 the keep-alive id is a **varint** (`i32`), whereas the modern
/// protocol uses a fixed 64-bit integer. The canonical model
/// (`ClientEvent::KeepAlive { id: i64 }` / `ClientAction::KeepAliveResponse {
/// id: i64 }`) uses `i64`, which is a lossless superset of the 1.8 range, so
/// the adapter converts cleanly in both directions. No model change is
/// required, but it is worth recording that the model field width was chosen to
/// accommodate the widest version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Client)]
pub struct KeepAliveRequest {
    /// Keep-alive id to echo back.
    #[mc(varint)]
    pub id: i32,
}

/// Serverbound `keep_alive` response.
///
/// Wire layout: a single varint id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Server)]
pub struct KeepAliveResponse {
    /// Echoed keep-alive id.
    #[mc(varint)]
    pub id: i32,
}
