//! Packets shared by both directions of the play state for this era.
//!
//! The 1.12 `keep_alive` pair is byte-identical to v1-14's own (measured), so
//! it lives in `lodestone-protocol-common` shared 340..=754. Not shared with
//! v1-8: 1.8 sent the id as a varint (`i32`), not a fixed 64-bit integer.
//!
//! The widening to 64 bits landed *inside* this era, at 1.12 (protocol 340) —
//! 110, 210 and 316 all still send a varint. That is a field **retype**, not
//! a field appearing or disappearing, so `#[mc(since)]`/`#[mc(until)]` cannot
//! express it: the two forms are two structs, and the adapter picks one by
//! the protocol it was constructed for. Sharing one struct here would mean
//! reading eight bytes where the server sent one to five, which corrupts
//! every subsequent packet in the stream rather than failing cleanly.

use lodestone_macros::{Decode, Encode, Packet};

pub use lodestone_protocol_common::packets::keep_alive::{KeepAliveRequest, KeepAliveResponse};

/// Clientbound `keep_alive` challenge for 110, 210 and 316.
///
/// Wire layout: a single VarInt id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:keep_alive",
    state = Play,
    bound = Client,
    protocols = "110..=316"
)]
pub struct KeepAliveRequestVarInt {
    /// Keep-alive id to echo back.
    #[mc(varint)]
    pub id: i32,
}

/// Serverbound `keep_alive` response for 110, 210 and 316.
///
/// Wire layout: a single VarInt id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:keep_alive",
    state = Play,
    bound = Server,
    protocols = "110..=316"
)]
pub struct KeepAliveResponseVarInt {
    /// Echoed keep-alive id.
    #[mc(varint)]
    pub id: i32,
}
