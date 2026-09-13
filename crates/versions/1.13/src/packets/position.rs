//! The pre-1.14 packed block `position` type, for protocol 404 (1.13.2).
//!
//! 1.13 is *not* where the packing changed: 1.8 through 1.13.2 all pack
//! `x(26) | y(12) | z(26)` with y in the middle, and 1.14 repacked it to
//! `x(26) | z(26) | y(12)`. That is the single most widespread difference
//! between this era and the one above it -- fifteen of the twenty-eight
//! packets whose shape changes at 1.14 change *only* because they carry a
//! position -- and it is invisible to a round-trip test, because both halves
//! would agree on the wrong layout. See `lodestone-protocol-common`'s
//! `packets::position` module docs for the full finding.

pub use lodestone_protocol_common::packets::position::{
    Position, pack_position, unpack_position,
};
