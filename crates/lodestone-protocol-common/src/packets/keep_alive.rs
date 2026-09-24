//! The `keep_alive` challenge/response pair, shared between v1-9 and v1-14
//! (protocols 340 and 754) only.
//!
//! 1.8 (v1-8, protocol 47) sent the id as a **varint**; 1.9+ widened it to a
//! fixed 64-bit integer, which is why this is not shared with v1-8. Declared
//! `#[mc(protocols = "340..=762")]`.

//!
//! The upper bound moved 758 -> 762 when the 1.19 era landed. `minecraft-data`
//! reports each widened packet's shape identical from 758 to 762 (named types
//! inlined, primitive aliases kept), and each additionally decodes or encodes
//! out of the committed real-join capture at
//! `crates/versions/1.19/tests/captures/join_1_19_4.txt` -- a widening without
//! one is the inheritance-by-range hazard the dedup plan names.

use lodestone_macros::{Decode, Encode, Packet};

/// Clientbound `keep_alive` challenge.
///
/// Wire layout: a single `i64` id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Client, protocols = "340..=762")]
pub struct KeepAliveRequest {
    /// Keep-alive id to echo back.
    pub id: i64,
}

/// Serverbound `keep_alive` response.
///
/// Wire layout: a single `i64` id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Server, protocols = "340..=762")]
pub struct KeepAliveResponse {
    /// Echoed keep-alive id.
    pub id: i64,
}
