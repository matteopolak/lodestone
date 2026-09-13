//! Clientbound `player_info` packet for protocol 404 -- the tab list.
//!
//! Byte-identical to v1-8's, v1-9's and v1-14's own hand-written decoder
//! (measured: `player_info` appears in neither the 1.12.2 -> 1.13.2 nor the
//! 1.13.2 -> 1.14.4 shape diff), so this type family lives in
//! `lodestone-protocol-common` and is re-exported here. See that crate's
//! module docs for the full rationale, including why there is no
//! `#[mc(protocols = ...)]` on `PlayerInfo` (it has no `Packet` derive to
//! carry one).

pub use lodestone_protocol_common::packets::player_info::{
    PlayerInfo, PlayerInfoAction, PlayerInfoEntry, PlayerInfoProperty,
};
