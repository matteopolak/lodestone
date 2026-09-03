//! Login-state packets for this era (protocol 774).
//!
//! Login does not end in the play state: the client answers
//! [`LoginFinished`] with [`LoginAcknowledged`] and the connection moves into
//! the configuration state. See [`crate::packets::configuration`].
//!
//! Two shapes here differ from the 1.20.6 era below, and both are trailing
//! fields with nothing after them — the position a decoder is least likely to
//! notice getting wrong, which is why each is checked against a recorded join
//! rather than against this crate's own encoder.
//!
//! * [`LoginFinished`] ends at the profile property list. The era below
//!   appends a `strict_error_handling` flag, so a decoder carried forward from
//!   there reads one byte past the packet.
//! * [`LoginStart`] still carries a required 128-bit profile UUID, as the
//!   1.20.6 era does; the eras below that write a presence boolean or nothing
//!   at all.

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

/// Clientbound `minecraft:login_disconnect`, whose reason is a **JSON
/// string** — the one text field on this wire that is not NBT, because the
/// login state has no registry to resolve components against.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_disconnect", state = Login, bound = Client, protocols = "774..=774")]
pub struct LoginDisconnect {
    /// JSON-encoded chat component describing the refusal.
    #[mc(max = 262_144)]
    pub reason: String,
}

/// Clientbound `minecraft:login_compression`, setting the zlib threshold for
/// the rest of the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_compression", state = Login, bound = Client, protocols = "774..=774")]
pub struct SetCompression {
    /// Packets at least this large are compressed; a negative value disables
    /// compression.
    #[mc(varint)]
    pub threshold: i32,
}

/// Clientbound `minecraft:hello`, opening the online-mode handshake.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:hello", state = Login, bound = Client, protocols = "774..=774")]
pub struct EncryptionRequest {
    /// ASCII server id folded into the authentication hash.
    #[mc(max = 20)]
    pub server_id: String,
    /// The server's DER-encoded RSA public key.
    pub public_key: Vec<u8>,
    /// Verify token the client echoes back encrypted.
    pub verify_token: Vec<u8>,
    /// Whether the server expects a session-server join call.
    pub should_authenticate: bool,
}

/// Serverbound `minecraft:key`, answering [`EncryptionRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:key", state = Login, bound = Server, protocols = "774..=774")]
pub struct EncryptionResponse {
    /// RSA-encrypted shared secret.
    pub shared_secret: Vec<u8>,
    /// RSA-encrypted verify token.
    pub verify_token: Vec<u8>,
}

/// Serverbound `minecraft:hello`: the requested name and the profile UUID
/// this client expects to be assigned.
///
/// The UUID is unconditional. An offline-mode server derives its own from the
/// name and ignores the value, but the sixteen bytes are read regardless.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:hello", state = Login, bound = Server, protocols = "774..=774")]
pub struct LoginStart {
    /// Requested player username.
    #[mc(max = 16)]
    pub username: String,
    /// The profile UUID this client expects.
    pub uuid: Uuid,
}

/// One signed property on a game profile — in vanilla, the skin texture blob
/// and the session server's signature over it.
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

/// Clientbound `minecraft:login_finished` carrying the authenticated game
/// profile.
///
/// Wire layout: 128-bit binary uuid, string username, the varint-counted
/// property list, and nothing after it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_finished", state = Login, bound = Client, protocols = "774..=774")]
pub struct LoginFinished {
    /// Binary profile UUID.
    pub uuid: Uuid,
    /// Authenticated profile name.
    #[mc(max = 16)]
    pub username: String,
    /// Signed profile properties (empty on an offline-mode server).
    pub properties: Vec<ProfilePropertyEntry>,
}

/// Serverbound `minecraft:login_acknowledged`: an empty packet that ends the
/// login state and puts the connection into configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Packet)]
#[mc(name = "minecraft:login_acknowledged", state = Login, bound = Server, protocols = "774..=774")]
pub struct LoginAcknowledged;

impl Encode for LoginAcknowledged {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for LoginAcknowledged {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}
