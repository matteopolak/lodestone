//! Login-state packets for this era (protocol 762).
//!
//! `EncryptionRequest`, `EncryptionResponse`, `LoginDisconnect` and
//! `SetCompression` are byte-identical to the eras below (measured: no
//! hand-written codec on either side), so they live in
//! `lodestone-protocol-common` and are re-exported here.
//!
//! Two do **not** come from there, and both are chat-signing consequences.
//! [`LoginStart`] gained an optional profile UUID at 1.19: the shared
//! definition is a bare username, and sending that to a 762 server leaves the
//! option byte unread. [`LoginSuccess`] gained the profile's **property
//! list** — the skin blob and its Mojang signature — which the client needs
//! precisely because signed chat makes a profile's identity load-bearing.
//! Both are fields appearing at the end of a packet with nothing after them,
//! which is the shape a decoder is least likely to notice getting wrong, so
//! each is checked against a real join capture rather than against our own
//! encoder.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

pub use lodestone_protocol_common::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, SetCompression,
};

/// Serverbound `login_start` packet that begins login with the client's name
/// and, from 1.19, the profile UUID it expects to be assigned.
///
/// The UUID is a **hint**: an offline-mode server derives its own from the
/// name and ignores what is sent here. It still has to be on the wire, since
/// the option byte is read unconditionally.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_start", state = Login, bound = Server, protocols = "762..=762")]
pub struct LoginStart {
    /// Requested player username.
    #[mc(max = 16)]
    pub username: String,
    /// Whether a profile UUID follows.
    pub has_uuid: bool,
    /// The profile UUID this client expects, when it has one.
    #[mc(present_if = "has_uuid == true")]
    pub uuid: Option<Uuid>,
}

/// One signed property on an authenticated game profile — in vanilla, the
/// skin texture blob and the session server's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ProfilePropertyEntry {
    /// Property name, such as `textures`.
    pub name: String,
    /// Property value (base64 for `textures`).
    pub value: String,
    /// Whether a signature follows.
    pub has_signature: bool,
    /// Base64 signature over `value`, when the property is signed.
    #[mc(present_if = "has_signature == true")]
    pub signature: Option<String>,
}
/// Clientbound `success` packet carrying the authenticated game profile.
///
/// Wire layout: 128-bit binary uuid, string username (max 16 chars), then the
/// varint-counted property list 1.19 added. The eras below stop after the
/// username, so a decoder inherited from one of them reads a clean profile
/// and silently leaves the whole property list in the buffer.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:success", state = Login, bound = Client, protocols = "762..=762")]
pub struct LoginSuccess {
    /// Binary profile UUID.
    pub uuid: Uuid,
    /// Authenticated profile name.
    #[mc(max = 16)]
    pub username: String,
    /// Signed profile properties (empty on an offline-mode server).
    pub properties: Vec<ProfilePropertyEntry>,
}
