//! Protocol 340 (Minecraft 1.12.2) client protocol crate.
//!
//! This crate implements a pre-1.13 protocol version (numeric block ids, no
//! flattening, pre-Configuration handshake). It deliberately mirrors the scope
//! of the other version crates — the handshake, status, login, and enough of
//! play to join a 1.12.2 server and stay connected — while validating that the
//! shared abstractions in `lodestone-core` and `lodestone-model` are not
//! silently over-fitted to any single wire format.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, and `lodestone-macros`, so the entire 1.12.2 family can
//! be removed by deleting this one folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id tables for protocol 340.
///
/// Unlike the modern crates — whose ids come from Mojang's own
/// `reports/packets.json` — protocol 340 predates Mojang's data generator, so
/// this table is generated from the community-maintained `minecraft-data`
/// project (`vendor/minecraft-data/data/pc/1.12.2/protocol.json`). See the
/// module documentation in `xtask` for the judgement calls that entails.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id→name tables for protocol 340.
///
/// Like `packet_ids`, these are generated from the community `minecraft-data`
/// project rather than a Mojang report. See `tests/entity_types.rs` for the
/// generator.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated `id:meta` (pre-Flattening) → modern block-state table for
/// protocol 340, derived from the real 1.13.2 server jar's own
/// `DataFixerUpper` flattening fix rather than from `minecraft-data` (which
/// has no such table at all — this is *not* a case of "predates Mojang's
/// data generator" like `packet_ids`/`entity_types` above; the authoritative
/// source here is the jar itself). See `tests/flattening.rs` for the
/// generator and `docs/protocol-340-flattening-table.md` for the full
/// ambiguous-case enumeration.
#[path = "generated/flattening.rs"]
pub(crate) mod generated_flattening;

pub mod adapter;
pub mod entity_types;
pub mod flattening;
pub mod packets;

pub use adapter::{PROTOCOL, V340Adapter, adapter};
