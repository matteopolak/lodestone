//! Login-state packets for this era (protocols 756 and 758).
//!
//! `LoginStart`, `EncryptionRequest`, `EncryptionResponse`, `LoginDisconnect`
//! and `SetCompression` are byte-identical to the eras below (measured: no
//! hand-written codec on either side), so they live in
//! `lodestone-protocol-common` and are re-exported here. Measured against
//! `minecraft-data`, the whole login section is identical across 754, 756 and
//! 758.
//!
//! [`LoginSuccess`] stays defined **here**, not re-exported: from 1.16 on it
//! sends the profile UUID as a **128-bit binary** value, where 1.8 through
//! 1.15 send a dashed *string*. That form lives in
//! `lodestone-protocol-common` ranged `47..=578` and is not reachable from
//! any protocol in this crate.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

pub use lodestone_protocol_common::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart, SetCompression,
};
/// Clientbound `success` packet carrying the authenticated game profile.
///
/// Wire layout: 128-bit binary uuid followed by string username (max 16
/// chars) — the 1.16 form, unchanged through this era.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:success", state = Login, bound = Client, protocols = "756..=758")]
pub struct LoginSuccess {
    /// Binary profile UUID.
    pub uuid: Uuid,
    /// Authenticated profile name.
    #[mc(max = 16)]
    pub username: String,
}
