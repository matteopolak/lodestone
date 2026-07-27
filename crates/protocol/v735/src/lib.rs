//! Protocol 754 (Minecraft 1.16.5) client protocol crate.
//!
//! This crate implements a post-flattening 1.16 protocol version (flat
//! block-state ids, 1.13.1 slot format, 3-D biomes, non-straddling long packing,
//! light split out of `map_chunk` into `update_light`, and the NBT-codec
//! `join_game`). It deliberately mirrors the scope of the other version crates —
//! the handshake, status, login, and enough of play to join a 1.16.5 server and
//! stay connected — while validating that the shared abstractions in
//! `lodestone-core` and `lodestone-model` are not silently over-fitted to any
//! single wire format.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros` and `lodestone-world`, so the entire
//! 1.16.5 family can be removed by deleting this one folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id tables for protocol 754.
///
/// Unlike the modern crates — whose ids come from Mojang's own
/// `reports/packets.json` — protocol 754 predates Mojang's machine-readable
/// packet report, so this table is generated from the community-maintained
/// `minecraft-data` project (`vendor/minecraft-data/data/pc/1.16.2/protocol.json`,
/// which 1.16.5 shares). See the module documentation in `xtask` for the
/// judgement calls that entails.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id→name table for protocol 754.
///
/// Like `packet_ids`, this is generated from the community `minecraft-data`
/// project rather than a Mojang report. See `tests/entity_types.rs` for the
/// generator.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

pub mod adapter;
pub mod entity_types;
pub mod packets;

pub use adapter::{PROTOCOL, V735Adapter, adapter};
