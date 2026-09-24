//! Packets shared by both directions of the play state for protocol 404.
//!
//! Byte-identical to v1-9's and v1-14's own `keep_alive` pair (measured: the
//! packet is absent from the 1.12.2 -> 1.13.2 and 1.13.2 -> 1.14.4 shape
//! diffs entirely), so these live in `lodestone-protocol-common` shared
//! 340..=754, a range that already covered 404. Not shared with v1-8: 1.8
//! sent the id as a varint (`i32`), not a fixed 64-bit integer.

pub use lodestone_protocol_common::packets::keep_alive::{KeepAliveRequest, KeepAliveResponse};
