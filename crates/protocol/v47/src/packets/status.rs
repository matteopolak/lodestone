//! Status-state packets for protocol 47.
//!
//! Status is the server-list-ping flow. It is not needed to join and stay
//! connected, but 1.8's status packets are trivially derive-expressible and are
//! included so the crate can answer a server ping and to exercise the derive
//! macro across another state.
//!
//! Byte-identical to 340's and 754's own status packets (measured: no
//! hand-written codec on either side to hide a divergence), so these four
//! types now live in `lodestone-protocol-common` and are re-exported here to
//! keep every existing `crate::packets::status::*` path working unchanged.

pub use lodestone_protocol_common::packets::status::{
    StatusPing, StatusPong, StatusRequest, StatusResponse,
};
