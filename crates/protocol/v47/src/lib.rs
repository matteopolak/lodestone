//! Protocol 47 (Minecraft 1.8.8 / 1.8.9) client protocol crate.
//!
//! This crate implements the oldest protocol version Lodestone supports. It
//! deliberately mirrors the scope of the modern `lodestone-v770` crate — the
//! handshake, status, login, and enough of play to join a 1.8 server and stay
//! connected — while validating that the shared abstractions in
//! `lodestone-core` and `lodestone-model` are not silently over-fitted to the
//! modern wire format.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros`, and the version-free
//! `lodestone-world` (whose paletted storage the 1.8 chunk decoder targets,
//! exactly as the modern crate does), so the entire 1.8 family can be removed by
//! deleting this one folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id tables for protocol 47.
///
/// Unlike the modern crates — whose ids come from Mojang's own
/// `reports/packets.json` — protocol 47 predates Mojang's data generator, so
/// this table is generated from the community-maintained `minecraft-data`
/// project (`vendor/minecraft-data/data/pc/1.8/protocol.json`). See the module
/// documentation in `xtask` for the judgement calls that entails.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id→name tables for protocol 47.
///
/// Like `packet_ids`, these are generated from the community `minecraft-data`
/// project rather than a Mojang report (protocol 47 predates the data
/// generator). See `tests/entity_types.rs` for the generator and the naming
/// judgement calls it entails.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

pub mod adapter;
pub mod entity_metadata;
pub mod entity_types;
pub mod packets;

pub use adapter::{PROTOCOL, PROTOCOLS, V47Adapter, adapter, adapter_for};
