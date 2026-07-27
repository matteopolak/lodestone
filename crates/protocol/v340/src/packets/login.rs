//! Login-state packets for protocol 340.
//!
//! Two fields here are architectural probes rather than routine ports:
//!
//! * [`LoginStart`] carries **only** a username. Unlike the modern `hello`
//!   packet there is no client-provided profile UUID, so this crate needs no
//!   `uuid` dependency at all on the serverbound path.
//! * [`LoginSuccess`] sends the profile UUID as a **dashed string**, not the
//!   modern 128-bit binary form. That is exactly why per-version duplicated
//!   structs are the right design: the same logical field has a different wire
//!   type across versions, and a shared struct could not express both.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `login_start` packet that begins login with the client's name.
///
/// Wire layout: string username (max 16 chars). There is no profile UUID in
/// 1.8, in contrast to the modern login `hello` packet.
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
/// Wire layout: string uuid (dashed, max 36 chars) followed by string username
/// (max 16 chars). The UUID is sent as a **string** in 1.8 through 1.15, not
/// the modern (1.16+) 128-bit binary UUID.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:success", state = Login, bound = Client)]
pub struct LoginSuccess {
    /// Dashed profile UUID string, such as `069a79f4-44e9-4726-a5be-fca90e38aaf5`.
    #[mc(max = 36)]
    pub uuid: String,
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
