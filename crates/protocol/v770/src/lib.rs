//! Protocol 770-family (Minecraft 1.21.5 – 26.2) client protocol crate.
//!
//! Phase 1 implements protocol 776 (Minecraft 26.2): the packet definitions for
//! the handshake, login, configuration, and play join flow, plus the
//! [`V770Adapter`] that lifts those packets into the version-free
//! [`lodestone_model`] canonical model, and [`server_protocol::V770ServerProtocol`]
//! that implements the mirror-image [`lodestone_server::ServerProtocol`] seam so
//! `lodestone-server` can drive the same wire format from the other side.
//!
//! This crate depends on the version-free `lodestone-core`, `lodestone-model`,
//! `lodestone-macros`, `lodestone-world`, and `lodestone-data` crates, plus
//! `lodestone-server` (also version-free) for the `V770ServerProtocol` seam —
//! never on another version crate — so the whole version family can be removed
//! by deleting this folder. `cargo xtask check-isolation` enforces this in one
//! direction only: a version crate may depend on version-free crates, but no
//! version-free crate may depend back on this one.
//!
//! # Game data lives in `lodestone-data`, not here
//!
//! Of the ~20 tables that used to be generated into this crate, only
//! [`packet_ids`] is wire format. Every other census (block collision,
//! hardness, entity dimensions, item prototypes, ...) was extracted to
//! [`lodestone_data`] (issue #361): it describes the game, not the protocol,
//! and a version-free consumer (`lodestone-server` in particular) can now
//! read it without depending on this crate at all. [`adapter`]'s
//! `VersionAdapter` impl is a thin seam over those tables for consumers that
//! only hold a `&dyn VersionAdapter` — see `docs/lodestone-data-crate.md`.

#![forbid(unsafe_code)]

/// Generated authoritative packet id tables for protocol 776.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated `stat_type` / `custom_stat` / `debug_subscription` id tables.
///
/// These three registries are carried as bare VarInt ids by `award_stats` and
/// the `debug_*` packets, so the adapter resolves them here rather than leaking
/// numbers into version-free state. Unlike the censuses named above they *are*
/// wire format — the id space is the protocol's, not the game's.
#[path = "generated/stat_debug_registries.rs"]
pub mod stat_debug_registries;

pub mod adapter;
pub mod chunk_batch;
pub mod entity_variants;
pub mod packets;
/// Issue #275: the 27 synchronized `registry_data` payloads (of 29) this
/// crate relays as captured vanilla bytes rather than a typed encode, plus
/// `update_tags` and `select_known_packs`. Not `pub`: only
/// [`server_protocol`] calls into it.
mod registry_data_fixtures;
pub mod server_protocol;

pub use adapter::{PROTOCOL, V770Adapter, adapter};
pub use server_protocol::V770ServerProtocol;
