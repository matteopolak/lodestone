//! The 1.20.6 wire era: Minecraft 1.20.5 and 1.20.6 (protocol 766) from one
//! crate.
//!
//! Two breaks land inside this era and both reshape the join, which is why it
//! is the first family here that cannot reuse any neighbour's choreography.
//!
//! * **A connection now has a configuration phase.** Login no longer ends in
//!   the play state: the client acknowledges login, the server then sends its
//!   registries, feature flags and tags in a state of its own, and play
//!   begins only after both sides exchange a finish-configuration packet.
//!   Every era below goes straight from login success into play, and the join
//!   packet carried the registries inline.
//! * **An item stack is a component map, not a damage value plus NBT.** A
//!   stack on the wire is a count, an item id and two component lists — the
//!   ones to add, and the ones to remove — where every era below carries an
//!   id, a count, a damage/metadata short and an optional NBT compound.
//!
//! The era's boundaries are measured, not assumed. Using `minecraft-data`
//! with named types inlined recursively (the same instrument
//! `cargo xtask protocol-dup` reports with), 766 shares 119 of its 220 packet
//! shapes with 762 (54%) and 204 of 226 with 767 (90%); 765 below it agrees on
//! 177 of 220 (80%). The grouping threshold for one crate to serve two
//! protocols is 85% agreement, so the era's **lower** boundary is real and its
//! upper one is not: 767 (Minecraft 1.21 and 1.21.1) is inside the same wire
//! era by that measure and is the natural second member of this crate.
//! `PROTOCOLS` therefore lists what is implemented and tested against real
//! bytes, which is 766 alone.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros`, `lodestone-protocol-common`,
//! `lodestone-data` and `lodestone-world`, so the entire era can be removed by
//! deleting this one folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id table for protocol 766 (Minecraft
/// 1.20.5 and 1.20.6).
///
/// Generated from the community-maintained `minecraft-data` project
/// (`vendor/minecraft-data/data/pc/1.20.5/protocol.json`, which its own
/// `dataPaths.json` names for both 1.20.5 and 1.20.6). This era's jar does
/// **not** ship a machine-readable packet report — its data generator emits
/// block, item, command and registry reports and no packet report — so the
/// wire evidence that checks these ids is `tests/capture_join.rs`'s captured
/// bytes rather than a second table.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id->name table for this era.
///
/// Generated from the 1.20.6 jar's own registry report — an authority rather
/// than a cross-check. `tests/entity_types.rs` asserts the ids against that
/// dump rather than describing them.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated 766 -> canonical 26.2 block-state id table.
///
/// `pub` (unlike `generated_entity_types`) because `tests/canonicalisation.rs`
/// asserts directly against it from outside the crate. See [`canonical`]'s
/// module docs for why the table exists at all.
#[path = "generated/canonical.rs"]
pub mod generated_canonical;

pub mod adapter;
pub mod canonical;
pub mod entity_types;
pub mod packets;
pub mod server_protocol;
pub use server_protocol::V766ServerProtocol;

pub use adapter::{PROTOCOL, PROTOCOL_1_20_6, PROTOCOLS, V766Adapter, adapter, adapter_for};
