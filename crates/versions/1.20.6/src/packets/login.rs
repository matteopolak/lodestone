//! Login-state packets for this era (protocol 766).
//!
//! Login is where this era stops resembling the ones below it, in three
//! places, and every one of them is a field at the end of a packet with
//! nothing after it — the shape a decoder is least likely to notice getting
//! wrong, which is why each is checked against a real join capture rather
//! than against our own encoder.
//!
//! * [`LoginStart`] carries a **required** 128-bit profile UUID. The era
//!   below writes an option byte and the eras below that write nothing at
//!   all, so a 762 client's single `false` byte is read here as the first 16
//!   bytes of a UUID and the connection dies.
//! * [`LoginSuccess`] gained a trailing `strict_error_handling` flag on top
//!   of the profile property list.
//! * [`EncryptionRequest`] gained a trailing `should_authenticate` flag, so
//!   the shared definition (which stops after the verify token) leaves a byte
//!   unread.
//!
//! Login also no longer ends in the play state: the client answers
//! [`LoginSuccess`] with [`LoginAcknowledged`] and the connection moves to
//! the configuration state. See [`crate::packets::configuration`].

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

/// Clientbound `disconnect` during login, whose reason is still a **JSON
/// string** at this protocol — the one text field on the wire that did not
/// become NBT, because the login state has no registry to resolve components
/// against.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:disconnect", state = Login, bound = Client, protocols = "766..=766")]
pub struct LoginDisconnect {
    /// JSON-encoded chat component describing the refusal.
    #[mc(max = 262_144)]
    pub reason: String,
}

/// Clientbound `compress`, setting the zlib threshold for the rest of the
/// connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:compress", state = Login, bound = Client, protocols = "766..=766")]
pub struct SetCompression {
    /// Packets at least this large are compressed; a negative value disables
    /// compression.
    #[mc(varint)]
    pub threshold: i32,
}

/// Clientbound `encryption_begin`, opening the online-mode handshake.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:encryption_begin", state = Login, bound = Client, protocols = "766..=766")]
pub struct EncryptionRequest {
    /// ASCII server id folded into the authentication hash.
    #[mc(max = 20)]
    pub server_id: String,
    /// The server's DER-encoded RSA public key.
    pub public_key: Vec<u8>,
    /// Verify token the client echoes back encrypted.
    pub verify_token: Vec<u8>,
    /// Whether the server expects a session-server join call. New at this
    /// era: the shared definition ends at the verify token.
    pub should_authenticate: bool,
}

/// Serverbound `encryption_begin`, answering [`EncryptionRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:encryption_begin", state = Login, bound = Server, protocols = "766..=766")]
pub struct EncryptionResponse {
    /// RSA-encrypted shared secret.
    pub shared_secret: Vec<u8>,
    /// RSA-encrypted verify token.
    pub verify_token: Vec<u8>,
}

/// Serverbound `login_start`: the requested name and the profile UUID this
/// client expects to be assigned.
///
/// The UUID is unconditional here. An offline-mode server derives its own
/// from the name and ignores the value, but the sixteen bytes are read
/// regardless.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_start", state = Login, bound = Server, protocols = "766..=766")]
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

/// Clientbound `success` carrying the authenticated game profile.
///
/// Wire layout: 128-bit binary uuid, string username, the varint-counted
/// property list, then the `strict_error_handling` flag this era appended. A
/// decoder inherited from the era below reads a clean profile and silently
/// leaves that last byte in the buffer.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:success", state = Login, bound = Client, protocols = "766..=766")]
pub struct LoginSuccess {
    /// Binary profile UUID.
    pub uuid: Uuid,
    /// Authenticated profile name.
    #[mc(max = 16)]
    pub username: String,
    /// Signed profile properties (empty on an offline-mode server).
    pub properties: Vec<ProfilePropertyEntry>,
    /// Whether the server wants a decode error on an unknown packet field to
    /// close the connection rather than be tolerated.
    pub strict_error_handling: bool,
}

/// Serverbound `login_acknowledged`: an empty packet that ends the login
/// state and puts the connection into configuration.
///
/// It carries no fields at all, so the derive would generate an empty codec;
/// the hand-written pair below exists only because an empty derived struct
/// still has to satisfy `Encode`/`Decode`, and writing nothing is clearer
/// stated than generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Packet)]
#[mc(name = "minecraft:login_acknowledged", state = Login, bound = Server, protocols = "766..=766")]
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
