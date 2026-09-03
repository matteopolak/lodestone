//! Protocol 5 client protocol crate — the bottom of the ladder.
//!
//! Protocol 5 is spoken by Minecraft 1.7.6 through 1.7.10. The protocol
//! number, not the folder name, is what [`adapter::PROTOCOLS`] declares and
//! what `VersionAdapter::supports` answers for; 1.7.10 is simply the version
//! the oracle in `tests/` is recorded against, because it is the last and most
//! widely deployed member of the run.
//!
//! # Why this era is a singleton
//!
//! The era boundary above it is the widest on the ladder. Measured over every
//! packet definition in both directions and all four connection states, with
//! every referenced named type inlined, protocol 5 and protocol 47 agree on
//! **37 of 112** packet shapes — 33%, against the 85% threshold
//! `docs/plans/multi-version-protocol-dedup.md` uses to group adjacent
//! versions into one era. Nothing adjacent can be folded in.
//!
//! Below it sits protocol 4 (Minecraft 1.7.2 through 1.7.5), which this crate
//! deliberately does **not** claim. See `docs/protocol-1-7-era.md` for what a
//! measurement would have to show before it could be admitted.
//!
//! # What is genuinely different here, not merely older
//!
//! Four things in this era have no counterpart in any later one, and each is
//! documented where it is implemented rather than only here:
//!
//! - **Chunk payloads are zlib streams inside the packet body**
//!   ([`packets::chunk`]). Whole-connection compression does not exist at
//!   protocol 5 — the login state has no compression-threshold packet at all
//!   — so the inflate is per chunk packet.
//! - **Block ids and block metadata arrive in separate arrays**, with a third
//!   conditional nibble array supplying the high bits of ids above 255
//!   ([`packets::chunk`]). Later eras pack one 16-bit value per block.
//! - **Positions are three separate numbers** on the wire, in three different
//!   widths depending on the packet ([`packets::position`]). The packed
//!   64-bit block position arrives with protocol 47.
//! - **The player list carries no profile UUID** ([`packets::player_info`]),
//!   which is the one concept here that cannot be mapped into canonical state
//!   without inventing something. That module's docs state exactly what is
//!   invented and how it is checked.
//!
//! Like every version crate this one can be removed by deleting its folder
//! plus its dependency and feature lines in `lodestone-registry`.

#![forbid(unsafe_code)]

/// Generated authoritative packet id tables for protocol 5.
///
/// Protocol 5 predates Mojang's data generator by six years, so this table is
/// generated from the community-maintained `minecraft-data` project
/// (`vendor/minecraft-data/data/pc/1.7/protocol.json`, whose own
/// `version.json` names 1.7.10 and protocol 5). `minecraft-data` is
/// cross-check-grade in this repo, never an authority, so the ids it produces
/// are checked against a recorded real join in
/// `tests/captures/join_1_7_10.txt` — see `tests/capture_join.rs`.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id→name tables for protocol 5.
///
/// Two tables, because protocol 5 numbers objects and mobs in two separate
/// spaces. Both are checked against a wire oracle rather than trusted; see
/// [`entity_types`] and `tests/entity_types.rs`.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated legacy item id->name table for protocol 5.
///
/// Sourced the same way as [`packet_ids`], from `minecraft-data`'s own 1.7
/// dataset, and cross-checked against ids observed in a real protocol-5
/// container packet; see [`item_types`].
#[path = "generated/item_types.rs"]
pub(crate) mod generated_item_types;

pub mod adapter;
pub mod entity_metadata;
pub mod entity_types;
pub mod item_types;
pub mod packets;

pub use adapter::{PROTOCOL, PROTOCOLS, V5Adapter, adapter, adapter_for};
