//! The 1.19 wire era: Minecraft 1.19.4 (protocol 762) from one crate.
//!
//! This is the wire generation where **chat became cryptographic**. Below it
//! a chat message is one JSON string and a position byte; here a player
//! message carries a sender profile id, a per-sender chain index, an optional
//! 256-byte signature, the exact bytes that signature was taken over, and an
//! acknowledgement window naming the messages the sender had seen — and the
//! serverbound side must send a matching acknowledgement window on every
//! message it sends, signed or not. The other era-defining change is that
//! **every entity now spawns through one packet**: the separate mob-spawn
//! packet the four eras below all carry is gone at 762, and its fields were
//! folded into the object-spawn packet with a head-rotation byte inserted
//! before the object-data field.
//!
//! It is a singleton era, measured rather than assumed: against the era below
//! it shares 137 of its 175 packet shapes (78%), and against 1.20.6 only 113
//! of 201 (56%), both from `minecraft-data` with named types inlined and
//! primitive aliases kept. Neither neighbour is above the 85% grouping
//! threshold, so no other version joins it.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros`, `lodestone-protocol-common`,
//! `lodestone-data` and `lodestone-world`, so the entire era can be removed by
//! deleting this one folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id table for protocol 762 (Minecraft
/// 1.19.4), the era's only release.
///
/// Generated from the community-maintained `minecraft-data` project
/// (`vendor/minecraft-data/data/pc/1.19.4/protocol.json`), which predates
/// Mojang's own machine-readable packet report. See the module documentation
/// in `xtask` for the judgement calls that entails, and
/// `tests/capture_join.rs` for the wire evidence that checks it.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated entity-type id->name table for this era.
///
/// Generated from the 1.19.4 jar's own `--reports` registry dump — an
/// authority rather than a cross-check. 1.19 added `allay`, which sorts
/// first alphabetically and therefore takes id 0, moving every id the eras
/// below assign; `tests/entity_types.rs` asserts that rather than describing
/// it.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated 1.19.4 -> canonical 26.2 block-state id table.
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

pub use adapter::{PROTOCOL, PROTOCOL_1_19_4, PROTOCOLS, V762Adapter, adapter, adapter_for};
pub use server_protocol::V762ServerProtocol;
