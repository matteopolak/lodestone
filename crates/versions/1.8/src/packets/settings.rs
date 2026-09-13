//! Client-configuration serverbound packets for protocol 47.
//!
//! `BrandPayload` is byte-identical across all three families (verified by
//! hand; the naive struct scan misses it because a field-level doc comment
//! names each family's own channel spelling) and now lives in
//! `lodestone-protocol-common` with the derive's default `ProtocolRange::ALL`.
//! `PlayerAbilities` is shared with v1-9 only (`#[mc(protocols = "47..=340")]`
//! -- 1.16/v1-14 dropped the two speed floats to a single flags byte). `Settings`
//! stays defined **here**: 1.8 has no `main_hand` and a signed-byte
//! `chat_flags`, unlike the varint-based v1-9/v1-14 version shared as
//! `lodestone-protocol-common`'s own `Settings` (declared 340..=754). See
//! that crate's `packets::client_settings` module docs.

use lodestone_macros::{Decode, Encode, Packet};

pub use lodestone_protocol_common::packets::client_settings::{BrandPayload, PlayerAbilities};

/// Serverbound `settings` (client settings).
///
/// # 1.8 divergence
///
/// 1.8 predates the off-hand, so there is **no** `main_hand` field (added in
/// 1.9), and `chat_flags` is a plain signed byte rather than the varint used
/// from 1.9 onward. The modern model's `text_filtering`, `allow_server_listing`
/// and `particle_status` fields have no 1.8 wire representation and are dropped
/// by the adapter.
///
/// Wire layout: string locale, signed-byte view distance, signed-byte chat
/// flags, bool chat colors, unsigned-byte displayed skin parts.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Play, bound = Server)]
pub struct Settings {
    /// Client locale, such as `en_us` (at most 16 characters).
    #[mc(max = 16)]
    pub locale: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility: `0` full, `1` commands only, `2` hidden.
    pub chat_flags: i8,
    /// Whether chat colors are enabled.
    pub chat_colors: bool,
    /// Displayed skin-part bitmask.
    pub skin_parts: u8,
}
