//! Login-state packets for this era (protocols 498, 578, 754).
//!
//! `LoginStart`, `EncryptionRequest`, `EncryptionResponse`, `LoginDisconnect`
//! and `SetCompression` are byte-identical to v1-8's and v1-9's own login
//! packets (measured: no hand-written codec on either side), so they now
//! live in `lodestone-protocol-common` and are re-exported below.
//!
//! [`LoginSuccess`] stays defined **here**, not re-exported: at 754 it sends
//! the profile UUID as a **128-bit binary** value. This is the 1.16 change:
//! 1.8 through 1.15 -- which is 498 and 578 as well as 47 and 340 -- send the
//! UUID as a dashed *string*, and that version lives in
//! `lodestone-protocol-common` ranged `#[mc(protocols = "47..=578")]`,
//! re-exported below as [`LoginSuccessString`]. Same packet name, different
//! wire type: reading sixteen raw bytes where a length-prefixed 36-character
//! string was sent does not fail, it consumes the username too, so this is
//! two structs and not a predicate.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

pub use lodestone_protocol_common::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart, SetCompression,
};
/// The pre-1.16 (498/578) form of clientbound `success`, whose profile UUID
/// is a dashed string rather than sixteen raw bytes. Shared with v1-8 and
/// v1-9, whose wire is identical here.
pub use lodestone_protocol_common::packets::login::LoginSuccess as LoginSuccessString;

/// Clientbound `success` packet carrying the authenticated game profile, in
/// its 1.16 (protocol 754) form.
///
/// Wire layout: 128-bit binary uuid followed by string username (max 16 chars).
/// The UUID is sent as a **binary** value in 1.16+, not the dashed-string
/// form 498 and 578 use ([`LoginSuccessString`]).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:success", state = Login, bound = Client, protocols = "754..=754")]
pub struct LoginSuccess {
    /// Binary profile UUID.
    pub uuid: Uuid,
    /// Authenticated profile name.
    #[mc(max = 16)]
    pub username: String,
}
