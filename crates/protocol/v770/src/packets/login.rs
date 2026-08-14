//! Login-state packets for protocol 776.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

/// Serverbound `hello` packet that begins login with the client's identity.
///
/// Wire layout: string name (max 16 chars) followed by a 16-byte profile UUID.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:hello", state = Login, bound = Server)]
pub struct LoginHello {
    /// Requested player username.
    #[mc(max = 16)]
    pub name: String,
    /// Client-provided profile UUID (offline-mode name-based UUID).
    pub profile_id: Uuid,
}

/// Serverbound `login_acknowledged` packet with an empty body, sent to confirm
/// the transition out of login into configuration.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_acknowledged", state = Login, bound = Server)]
pub struct LoginAcknowledged;

/// Serverbound `key` packet answering the server's encryption request.
///
/// Wire layout: two byte arrays (each a varint length prefix followed by the
/// bytes) — the RSA-encrypted shared secret, then the RSA-encrypted verify
/// token. This is the modern (1.19.3+) shape with no salt/signature; older
/// versions frame it differently, which is why this lives in the version crate.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:key", state = Login, bound = Server)]
pub struct EncryptionResponse {
    /// RSA-encrypted AES shared secret.
    pub shared_secret: Vec<u8>,
    /// RSA-encrypted verify token echoed back from the request.
    pub verify_token: Vec<u8>,
}

/// Clientbound `login_compression` packet enabling packet compression.
///
/// Wire layout: a single varint threshold. Packets whose length is at least the
/// threshold are zlib compressed; a negative threshold disables compression.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_compression", state = Login, bound = Client)]
pub struct LoginCompression {
    /// Compression threshold in bytes.
    #[mc(varint)]
    pub threshold: i32,
}

/// A single signed profile property attached to a completed login.
///
/// Wire layout: string name, string value, then an optional string signature
/// (a boolean presence flag followed by the string when present).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Property {
    /// Property key, such as `textures`.
    pub name: String,
    /// Property value.
    pub value: String,
    /// Optional Yggdrasil signature over the value.
    pub signature: Option<String>,
}

/// Clientbound `login_finished` packet carrying the authenticated game profile.
///
/// Wire layout: 16-byte profile UUID, string name (max 16 chars), a
/// varint-prefixed list of [`Property`] values, then a 16-byte session UUID.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_finished", state = Login, bound = Client)]
pub struct LoginFinished {
    /// Authenticated profile UUID.
    pub profile_id: Uuid,
    /// Authenticated profile name.
    #[mc(max = 16)]
    pub name: String,
    /// Profile properties (textures, and similar).
    pub properties: Vec<Property>,
    /// Server-assigned session UUID.
    pub session_id: Uuid,
}

/// Clientbound `hello` packet, the online-mode encryption request.
///
/// Wire layout: string server id (max 20 chars), a byte-array public key, a
/// byte-array verify token, then a boolean requesting Mojang authentication.
/// Phase 1 does not implement encryption, so receiving this is a hard error.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:hello", state = Login, bound = Client)]
pub struct EncryptionRequest {
    /// Server id string used in the authentication hash.
    #[mc(max = 20)]
    pub server_id: String,
    /// DER-encoded RSA public key.
    pub public_key: Vec<u8>,
    /// Verify token the client must echo back encrypted.
    pub challenge: Vec<u8>,
    /// Whether the server expects Mojang session authentication.
    pub should_authenticate: bool,
}

/// Clientbound `login_disconnect` packet.
///
/// Unlike later states, the login disconnect reason is a length-prefixed JSON
/// string rather than binary NBT, so it is decoded directly as a string.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_disconnect", state = Login, bound = Client)]
pub struct LoginDisconnect {
    /// JSON-encoded disconnect reason component.
    pub reason: String,
}

/// Serverbound `custom_query_answer`, replying to a clientbound `custom_query`
/// — the old, pre-`custom_payload` login-phase plugin-message
/// mechanism (historically Forge/FML's handshake). `payload` is nullable on
/// the wire (`writeNullable`); this crate never has one to send, matching
/// vanilla's own reference client
/// (`ClientHandshakePacketListenerImpl.handleCustomQuery`), which answers
/// every `custom_query` with `payload: null` unconditionally regardless of
/// channel.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_query_answer", state = Login, bound = Server)]
pub struct CustomQueryAnswer {
    /// Transaction id echoed from the `custom_query` this answers.
    #[mc(varint)]
    pub transaction_id: i32,
    /// Response payload; always `None` — see the type's own doc.
    pub payload: Option<Vec<u8>>,
}
