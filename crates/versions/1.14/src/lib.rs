//! The 1.14 wire era: Minecraft 1.14.4, 1.15.2 and 1.16.5 (protocols 498,
//! 578 and 754) from one crate.
//!
//! This crate implements the post-flattening, pre-1.17 wire generation: flat
//! block-state ids, the 1.13.1 slot format, light split out of `map_chunk`
//! into `update_light`, and a fixed sixteen-section column. The three
//! releases in it differ in the chunk packet's biome array (absent at 498,
//! a fixed 1024-entry array at 578, a length-prefixed VarInt array at 754),
//! in section long-packing (straddling below 754, padded at 754), and in
//! eight other packet shapes; see `docs/protocol-1-14-era.md`. It
//! deliberately mirrors the scope of the other version crates — the
//! handshake, status, login, and enough of play to join a server and stay
//! connected — while validating that the shared abstractions in
//! `lodestone-core` and `lodestone-model` are not silently over-fitted to any
//! single wire format.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros` and `lodestone-world`, so the entire
//! era can be removed by deleting this one folder.

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

/// Generated authoritative packet id table for protocol 498 (Minecraft
/// 1.14.4), the opening release of this era.
///
/// Same provenance as [`packet_ids`]: `minecraft-data`'s
/// `vendor/minecraft-data/data/pc/1.14.4/protocol.json`. Selected at adapter
/// construction, never referenced by name from a packet arm.
#[path = "generated/packet_ids_498.rs"]
pub mod packet_ids_498;

/// Generated authoritative packet id table for protocol 578 (Minecraft
/// 1.15.2).
///
/// Same provenance as [`packet_ids`]:
/// `vendor/minecraft-data/data/pc/1.15.2/protocol.json`. 1.15 moved
/// `acknowledge_player_digging` from the end of the clientbound table to id
/// 8, shifting all 84 ids above it by one, so this genuinely is a different
/// table and not a copy.
#[path = "generated/packet_ids_578.rs"]
pub mod packet_ids_578;

/// Generated entity-type id→name table for protocol 754.
///
/// Like `packet_ids`, this is generated from the community `minecraft-data`
/// project rather than a Mojang report. See `tests/entity_types.rs` for the
/// generator.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated entity-type id→name table for protocol 498.
///
/// Unlike [`generated_entity_types`], generated from the 1.14.4 jar's own
/// `--reports` registry dump — an authority rather than a cross-check. See
/// `tests/entity_types.rs`.
#[path = "generated/entity_types_498.rs"]
pub(crate) mod generated_entity_types_498;

/// Generated entity-type id→name table for protocol 578.
///
/// Same jar-report provenance as [`generated_entity_types_498`].
#[path = "generated/entity_types_578.rs"]
pub(crate) mod generated_entity_types_578;

/// Generated 1.16.5 (protocol 754) -> canonical 26.2 block-state id table.
///
/// `pub` (unlike `generated_entity_types`) because `tests/canonicalisation.rs`
/// asserts directly against it from outside the crate. See
/// [`canonical`]'s module docs for why this table exists and
/// `tests/canonicalisation.rs` for the generator.
#[path = "generated/canonical.rs"]
pub mod generated_canonical;

/// Generated 1.14.4 (protocol 498) -> canonical 26.2 block-state id table.
///
/// A separate table because 1.14.4's global palette holds 11,271 states to
/// 1.16.5's 17,112 and the two disagree from state 72 on — see
/// [`canonical`]'s module docs.
#[path = "generated/canonical_498.rs"]
pub mod generated_canonical_498;

/// Generated 1.15.2 (protocol 578) -> canonical 26.2 block-state id table.
///
/// 11,337 states: 1.15 inserted the bee and the honey blocks, so this
/// disagrees with 498 from state 11198 and with 754 from state 72.
#[path = "generated/canonical_578.rs"]
pub mod generated_canonical_578;

pub mod adapter;
pub mod canonical;
pub mod entity_types;
pub mod packets;

pub use adapter::{
    PROTOCOL, PROTOCOL_1_14_4, PROTOCOL_1_15_2, PROTOCOL_1_16_5, PROTOCOLS, V735Adapter, adapter,
    adapter_for,
};
