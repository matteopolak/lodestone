//! The 1.17 wire era: Minecraft 1.17.1 and 1.18.2 (protocols 756 and 758)
//! from one crate.
//!
//! This is the wire generation where the world stopped being sixteen
//! sections tall. Before it, a column was a fixed 0..255 stack and a
//! chunk packet's section mask fitted in one VarInt; here the mask is a
//! length-prefixed array of 64-bit words, the vertical range is read out of
//! the dimension entry the join packet carries, and 1.18 moves the floor
//! below `y = 0` and the biomes into the section payload. A section count
//! taken from the wrong place does not error — it consumes the wrong number
//! of bytes and desynchronises the stream — so the shape is resolved once,
//! from the server's own dimension entry, and carried into every decode.
//!
//! Like every version crate it depends only on `lodestone-core`,
//! `lodestone-model`, `lodestone-macros`, `lodestone-protocol-common` and
//! `lodestone-world`, so the entire era can be removed by deleting this one
//! folder.

#![forbid(unsafe_code)]

/// Generated authoritative packet id table for protocol 756 (Minecraft
/// 1.17.1), the opening release of this era.
///
/// Generated from the community-maintained `minecraft-data` project
/// (`vendor/minecraft-data/data/pc/1.17.1/protocol.json`), which predates
/// Mojang's own machine-readable packet report. See the module documentation
/// in `xtask` for the judgement calls that entails, and
/// `tests/capture_join.rs` for the wire evidence that checks it.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated authoritative packet id table for protocol 758 (Minecraft
/// 1.18.2).
///
/// Same provenance as [`packet_ids`]:
/// `vendor/minecraft-data/data/pc/1.18.2/protocol.json`. 1.18 inserted
/// `simulation_distance` at clientbound play id 87, shifting the fifteen ids
/// above it by one, so this genuinely is a different table and not a copy.
#[path = "generated/packet_ids_758.rs"]
pub mod packet_ids_758;

/// Generated entity-type id->name table for this era.
///
/// Generated from the 1.17.1 jar's own `--reports` registry dump — an
/// authority rather than a cross-check. One table serves both protocols:
/// the two jars' dumps assign the same 113 ids to the same names, measured
/// entry by entry in `tests/entity_types.rs` against **both** committed
/// dumps. See that test for the generator.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

/// Generated 1.17.1/1.18.2 -> canonical 26.2 block-state id table.
///
/// `pub` (unlike `generated_entity_types`) because `tests/canonicalisation.rs`
/// asserts directly against it from outside the crate. One table serves both
/// protocols: the two jars' `blocks.json` dumps are byte-identical, which
/// `tests/canonicalisation.rs` records and re-derives rather than assumes.
/// See [`canonical`]'s module docs for why the table exists at all.
#[path = "generated/canonical.rs"]
pub mod generated_canonical;

pub mod adapter;
pub mod canonical;
pub mod entity_types;
pub mod packets;
mod registry;
pub mod server_protocol;

pub use adapter::{
    PROTOCOL, PROTOCOL_1_17_1, PROTOCOL_1_18_2, PROTOCOLS, V756Adapter, adapter, adapter_for,
};
pub use server_protocol::{V756ServerProtocol, V758ServerProtocol};
