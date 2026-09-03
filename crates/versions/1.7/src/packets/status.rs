//! Status-state packets for protocol 5.
//!
//! Every one of the four is byte-identical to its protocol 47 counterpart --
//! measured, in both directions, with every referenced type inlined. The
//! server-list ping is the one part of this wire that did not change at the
//! era boundary at all, so these are re-exports rather than definitions.

pub use lodestone_protocol_common::packets::status::{
    StatusPing, StatusPong, StatusRequest, StatusResponse,
};
