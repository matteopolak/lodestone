//! Packets shared by both directions of the play state for protocol 340.
//!
//! Byte-identical to v1-14's own `keep_alive` pair (measured), so these now
//! live in `lodestone-protocol-common` shared 340..=754. Not shared with
//! v1-8: 1.8 sent the id as a varint (`i32`), not a fixed 64-bit integer.

pub use lodestone_protocol_common::packets::keep_alive::{KeepAliveRequest, KeepAliveResponse};
