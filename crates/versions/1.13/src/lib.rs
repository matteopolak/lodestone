//! The 1.13 wire era: Minecraft 1.13.2 (protocol 404) from one crate.
//!
//! This crate implements the **flattening boundary** itself. Before it, a
//! block on the wire is a `(numeric id, metadata)` composite and an item is a
//! `(numeric id, damage)` pair; from here on both are single flat registry
//! ids, and the whole id space was renumbered in one release. That break is
//! why this era has exactly one member: measured against `minecraft-data`
//! with named types inlined, 1.13.2 agrees with 1.12.2 on 104 of 125 shared
//! packet shapes and with 1.14.4 on 114 of 142 — 72% and 73% once the
//! 18 packets 1.13 adds and the 11 that 1.14 adds are counted, both below the
//! 85% an era run needs. It neighbours a discontinuity on each side, and
//! [`docs/protocol-1-13-era.md`](../../../docs/protocol-1-13-era.md) records
//! what each of those two breaks costs.
//!
//! Concretely, relative to the era above it (1.14–1.16): light still travels
//! *inside* `map_chunk` rather than in a separate packet, a chunk column has
//! no heightmap NBT and no per-section non-air block count, a full column's
//! biomes are 256 big-endian ints at the tail of the section buffer, the
//! packed block `position` still puts Y in the middle, join and respawn still
//! carry a difficulty byte, and the entity-metadata serializer table stops at
//! type 15. Relative to the era below it (1.8–1.12.2): a palette entry is a
//! flat state id, a slot leads with a `present` boolean, and mobs and objects
//! index one unified entity registry instead of two overlapping ones.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros`, `lodestone-world` and the
//! version-free `lodestone-protocol-common`, so the whole era can be removed
//! by deleting this one folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id table for protocol 404 (Minecraft
/// 1.13.2).
///
/// Protocol 404 predates Mojang's machine-readable packet report, so this
/// table is generated from the community-maintained `minecraft-data` project
/// (`vendor/minecraft-data/data/pc/1.13.2/protocol.json`). See the module
/// documentation in `xtask` for the judgement calls that entails, and
/// `tests/capture_join.rs` for the real-server bytes that check it.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id→name table for protocol 404.
///
/// 1.13.2's own `--reports` run emits block, item and command reports but
/// **no** registry dump (that provider arrived in 1.14), so unlike the 1.14
/// era's 498/578 tables this one is generated from `minecraft-data`'s
/// `entities.json` and then checked id-by-id against ids a real 1.13.2 server
/// put on the wire. See `tests/entity_types.rs` for the generator and
/// `tests/captures/entity_types_1_13_2.txt` for the wire oracle.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated 1.13.2 (protocol 404) -> canonical 26.2 block-state id table.
///
/// `pub` (unlike `generated_entity_types`) because `tests/canonicalisation.rs`
/// asserts directly against it from outside the crate. See [`canonical`]'s
/// module docs for why this table exists and `tests/canonicalisation.rs` for
/// the generator and its provenance.
#[path = "generated/canonical.rs"]
pub mod generated_canonical;

/// Generated object-type id→name table for protocol 404 — a **second** id
/// space, not the unified registry.
///
/// At 404 `spawn_entity` still carries the pre-1.13 object numbering, so this
/// table exists and disagrees with [`generated_entity_types`] on almost every
/// id. Generated from the wire transcript in
/// `tests/captures/entity_types_1_13_2.txt`, since no dataset covers it
/// usably; see `tests/entity_types.rs`.
#[path = "generated/object_types.rs"]
pub(crate) mod generated_object_types;

pub mod adapter;
pub mod canonical;
pub mod entity_types;
pub mod packets;

pub use adapter::{PROTOCOL, PROTOCOL_1_13_2, PROTOCOLS, V404Adapter, adapter, adapter_for};
