//! Status-state packets for this era (protocol 766).
//!
//! Status is the server-list-ping flow. It is not needed to join and stay
//! connected, but this era's status packets are trivially derive-expressible
//! and are included so the crate can answer a server ping.
//!
//! The four shared definitions in `lodestone-protocol-common` carry no
//! `#[mc(protocols = ...)]` range at all — they are the one group this repo
//! measured as unchanged across every protocol it serves — so they are
//! re-exported here rather than restated.

pub use lodestone_protocol_common::packets::status::{
    StatusPing, StatusPong, StatusRequest, StatusResponse,
};
