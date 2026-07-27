//! Login-state packets for protocol 754 (Minecraft 1.16.5).
//!
//! Two fields here are architectural probes rather than routine ports:
//!
//! * [`LoginStart`] carries **only** a username. Unlike the modern `hello`
//!   packet there is no client-provided profile UUID, so the serverbound path
//!   needs no `uuid` value at all.
//! * [`LoginSuccess`] sends the profile UUID as a **128-bit binary** value. This
//!   is the 1.16 change: 1.8 through 1.15 sent the UUID as a dashed *string*,
//!   but protocol 735+ (1.16) switched to the binary form the modern client
//!   uses. That is exactly why per-version duplicated structs are the right
//!   design: the same logical field has a different wire type across versions,
//!   and a shared struct could not express both.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

/// Serverbound `login_start` packet that begins login with the client's name.
///
/// Wire layout: string username (max 16 chars). There is no profile UUID, in
/// contrast to the modern login `hello` packet.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login_start", state = Login, bound = Server)]
pub struct LoginStart {
    /// Requested player username.
    #[mc(max = 16)]
    pub username: String,
}

/// Serverbound `encryption_begin` (encryption response) packet.
///
/// Wire layout: a varint-length-prefixed encrypted shared secret followed by a
/// varint-length-prefixed encrypted verify token. Phase 1 never sends this
/// (offline mode only); it is modelled for completeness and round-trip tests.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:encryption_begin", state = Login, bound = Server)]
pub struct EncryptionResponse {
    /// RSA-encrypted shared secret.
    pub shared_secret: Vec<u8>,
    /// RSA-encrypted verify token echoed from the request.
    pub verify_token: Vec<u8>,
}

/// Clientbound `disconnect` packet sent during login.
///
/// The login disconnect reason is a length-prefixed JSON string rather than
/// binary NBT, so it is decoded directly as a string and interpreted by the
/// crate's own JSON text extractor.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:disconnect", state = Login, bound = Client)]
pub struct LoginDisconnect {
    /// JSON-encoded disconnect reason component.
    pub reason: String,
}

/// Clientbound `encryption_begin` (encryption request) packet, the online-mode
/// handshake.
///
/// Wire layout: string server id (max 20 chars), a varint-length-prefixed
/// public key, then a varint-length-prefixed verify token. Phase 1 does not
/// implement encryption, so receiving this is a hard error.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:encryption_begin", state = Login, bound = Client)]
pub struct EncryptionRequest {
    /// Server id string used in the authentication hash.
    #[mc(max = 20)]
    pub server_id: String,
    /// DER-encoded RSA public key.
    pub public_key: Vec<u8>,
    /// Verify token the client must echo back encrypted.
    pub verify_token: Vec<u8>,
}

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

/// Clientbound `compress` packet enabling packet compression.
///
/// Wire layout: a single varint threshold. Packets whose length is at least the
/// threshold are zlib compressed; a negative threshold disables compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:compress", state = Login, bound = Client)]
pub struct SetCompression {
    /// Compression threshold in bytes.
    #[mc(varint)]
    pub threshold: i32,
}
