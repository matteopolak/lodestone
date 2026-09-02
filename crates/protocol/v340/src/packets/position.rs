//! The pre-1.14 packed block `position` type, for protocol 340 (1.12.2).
//!
//! Byte-identical to v47's own codec (measured: only doc comments differed),
//! but genuinely **not** to v735's -- 1.14 repacked the bit layout, a
//! divergence the naive struct-identity scan cannot see because it never
//! inspects the hand-written `Encode`/`Decode` impl. Shared with v47 only;
//! see `lodestone-protocol-common`'s `packets::position` module docs for the
//! full finding.

pub use lodestone_protocol_common::packets::position::{
    Position, pack_position, unpack_position,
};
