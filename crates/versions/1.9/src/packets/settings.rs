//! Client-configuration serverbound packets for protocol 340.
//!
//! `BrandPayload`, `Settings` and `ResourcePackReceive` are byte-identical
//! to v1-14's own definitions (measured/verified), so they now live in
//! `lodestone-protocol-common`. `PlayerAbilities` is shared with v1-8 only
//! (`#[mc(protocols = "47..=340")]` -- 1.16/v1-14 dropped the two speed
//! floats to a single flags byte). See that crate's
//! `packets::client_settings` module docs.

pub use lodestone_protocol_common::packets::client_settings::{
    BrandPayload, PlayerAbilities, ResourcePackReceive, Settings,
};
