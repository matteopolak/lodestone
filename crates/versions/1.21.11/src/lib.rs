//! The 1.21.11 wire era: Minecraft 1.21.11 (protocol 774) from one crate.
//!
//! This is the last release line before the canonical 26.2 version, and the
//! upper bound of `minecraft-data`'s coverage — so the highest protocol for
//! which a second, independent packet dataset exists at all.
//!
//! Three breaks separate this era from the 1.20.6 one below it, and each is
//! invisible to a decoder that keeps the older shape:
//!
//! * **A teleport is absolute *and* relative in the same packet.** The
//!   clientbound player-position packet leads with its teleport id, then
//!   carries a position, a *delta* velocity, a rotation, and a 32-bit
//!   relative-flag word. The era below carries position, rotation, a
//!   single-byte flag set, and the teleport id last.
//! * **A column's heightmaps are a typed array, not NBT.** Where the 1.20.6
//!   column opens with an anonymous NBT compound, this one carries a
//!   varint-counted list of `(heightmap type, packed long array)` pairs.
//! * **Chat carries a server-global message index and a registry-holder chat
//!   type.** The clientbound player-chat packet opens with a monotone index
//!   the era below does not have, and identifies its chat type through a
//!   registry-entry holder rather than a bare registry id.
//!
//! The era's boundaries are measured, not assumed. Using `minecraft-data`
//! with named types inlined recursively and primitive aliases kept — the
//! methodology `cargo xtask protocol-dup` reports the adjacency table with —
//! 774 shares this fraction of its packet shapes with each protocol below it:
//! 767 66.8%, 768 75.1%, 769 77.3%, 770 80.4%, 771 88.5%, 772 87.4%,
//! 773 94.0%. The grouping threshold for one crate to serve two protocols is
//! 85% agreement, so the era's lower boundary sits between 770 and 771:
//! **771, 772, 773 and 774** are one wire era by that measure, and 770 and
//! below are not. `PROTOCOLS` lists what is implemented and checked against
//! real bytes, which is 774 alone.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros`, `lodestone-protocol-common`,
//! `lodestone-data` and `lodestone-world`, so the entire era can be removed
//! by deleting this one folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id table for protocol 774 (Minecraft
/// 1.21.11).
///
/// Generated from **Mojang's own packet report** — the jar's data generator
/// run against the real `.cache/mc/1.21.11/server.jar`, which does emit one
/// at this version. `minecraft-data`'s own table for 1.21.11 agrees on every
/// one of the 220 ids across the five states, which is a second independent
/// source rather than the authority.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id->name table for this era.
///
/// Generated from the 1.21.11 jar's own registry report — an authority rather
/// than a cross-check. `tests/entity_types.rs` asserts the ids against that
/// dump rather than describing them.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated 774 -> canonical 26.2 block-state id table.
///
/// `pub` (unlike `generated_entity_types`) because `tests/canonicalisation.rs`
/// asserts directly against it from outside the crate. See [`canonical`]'s
/// module docs for why the table exists at all.
#[path = "generated/canonical.rs"]
pub mod generated_canonical;

pub mod adapter;
mod block_registry;
pub mod canonical;
pub mod entity_types;
mod item_registry;
pub mod packets;
pub mod server_protocol;
pub use server_protocol::V774ServerProtocol;

pub use adapter::{PROTOCOL, PROTOCOL_1_21_11, PROTOCOLS, V774Adapter, adapter, adapter_for};
