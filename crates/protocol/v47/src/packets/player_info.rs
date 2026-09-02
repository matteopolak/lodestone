//! Clientbound `player_info` packet for protocol 47 -- the tab list.
//!
//! Byte-identical to v340's and v735's own hand-written decoder (measured:
//! only doc comments and a test-only `CTX` constant differed), so this type
//! family now lives in `lodestone-protocol-common` and is re-exported here
//! to keep every existing `crate::packets::player_info::*` path working
//! unchanged. See that crate's module docs for the full rationale, including
//! why there is no `#[mc(protocols = ...)]` on `PlayerInfo` (it has no
//! `Packet` derive to carry one).

pub use lodestone_protocol_common::packets::player_info::{
    PlayerInfo, PlayerInfoAction, PlayerInfoEntry, PlayerInfoProperty,
};
