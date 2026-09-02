//! Login-state packets for protocol 754 (Minecraft 1.16.5).
//!
//! `LoginStart`, `EncryptionRequest`, `EncryptionResponse`, `LoginDisconnect`
//! and `SetCompression` are byte-identical to v1-8's and v1-9's own login
//! packets (measured: no hand-written codec on either side), so they now
//! live in `lodestone-protocol-common` and are re-exported below.
//!
//! [`LoginSuccess`] stays defined **here**, not re-exported: it sends the
//! profile UUID as a **128-bit binary** value. This is the 1.16 change: 1.8
//! through 1.15 sent the UUID as a dashed *string* (that version is shared
//! between v1-8 and v1-9 in `lodestone-protocol-common`, ranged
//! `#[mc(protocols = "47..=340")]`), but protocol 735+ (1.16) switched to the
//! binary form the modern client uses. Same packet name, different wire
//! type -- the project's "separate structs where a field's type changes"
//! rule, not a range this crate can widen into.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

pub use lodestone_protocol_common::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart, SetCompression,
};

/// Clientbound `success` packet carrying the authenticated game profile.
///
/// Wire layout: 128-bit binary uuid followed by string username (max 16 chars).
/// The UUID is sent as a **binary** value in 1.16+ (protocol 735+), not the
/// dashed-string form used by 1.8 through 1.15.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:success", state = Login, bound = Client)]
pub struct LoginSuccess {
    /// Binary profile UUID.
    pub uuid: Uuid,
    /// Authenticated profile name.
    #[mc(max = 16)]
    pub username: String,
}
