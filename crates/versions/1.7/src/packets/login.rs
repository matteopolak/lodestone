//! Login-state packets for protocol 5.
//!
//! # What is shared and what is not
//!
//! `login_start`, `disconnect` and `success` are measured identical to their
//! protocol 47 counterparts and are re-exported. [`LoginSuccess`]'s shared
//! definition carries an explicit protocol range, widened downward to include
//! protocol 5 on the strength of the committed join capture: the dashed-UUID
//! string form it decodes is exactly what a real 1.7.10 server sends, 36
//! characters of it.
//!
//! # The two that are not shared, and the trap inside them
//!
//! `encryption_begin` exists in both directions at protocol 5 with the same
//! field names, the same field order -- and a different wire type. Protocol 5
//! prefixes both byte arrays with a big-endian `i16` count; protocol 47
//! prefixes them with a varint. For a blob shorter than 128 bytes a varint
//! prefix is one byte where this is two, so the shared definition decodes a
//! four-byte verify token without complaint and mis-frames everything after
//! it.
//!
//! The shared definitions declare no protocol range, so nothing structurally
//! stops a protocol-5 caller from reaching for them; the guard is that this
//! module defines its own and never re-exports those two. That is weaker than
//! a declared range, and it is stated here rather than left to be discovered.
//!
//! There is no compression-threshold packet in this state at all. Whole-
//! connection compression arrives with protocol 47; before it the only
//! compressed bytes on this wire are chunk payloads, which carry their own
//! zlib streams (see [`crate::packets::chunk`]).

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer};

pub use lodestone_protocol_common::packets::login::{LoginDisconnect, LoginStart, LoginSuccess};

/// Largest byte array either encryption packet may carry.
///
/// A 1024-bit RSA key's DER encoding is about 162 bytes and a verify token is
/// four, so this is a generous ceiling whose only job is to stop a corrupt
/// length from driving a large allocation.
const MAX_ENCRYPTION_BLOB: usize = 1024;

/// Clientbound `encryption_begin` (encryption request), protocol 5 form.
///
/// Wire layout: string server id, `i16`-prefixed public key, `i16`-prefixed
/// verify token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionRequest {
    /// Server id string used in the authentication hash.
    pub server_id: String,
    /// DER-encoded RSA public key.
    pub public_key: Vec<u8>,
    /// Verify token the client must echo back encrypted.
    pub verify_token: Vec<u8>,
}

impl Decode for EncryptionRequest {
    fn decode(reader: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self {
            server_id: reader.string(20)?,
            public_key: read_short_blob(reader)?,
            verify_token: read_short_blob(reader)?,
        })
    }
}

impl Encode for EncryptionRequest {
    fn encode(&self, writer: &mut Writer, _ctx: Ctx) -> Result<()> {
        writer.string(&self.server_id);
        write_short_blob(writer, &self.public_key)?;
        write_short_blob(writer, &self.verify_token)
    }
}

/// Serverbound `encryption_begin` (encryption response), protocol 5 form.
///
/// Wire layout: `i16`-prefixed shared secret, `i16`-prefixed verify token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionResponse {
    /// RSA-encrypted shared secret.
    pub shared_secret: Vec<u8>,
    /// RSA-encrypted verify token echoed from the request.
    pub verify_token: Vec<u8>,
}

impl Decode for EncryptionResponse {
    fn decode(reader: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self {
            shared_secret: read_short_blob(reader)?,
            verify_token: read_short_blob(reader)?,
        })
    }
}

impl Encode for EncryptionResponse {
    fn encode(&self, writer: &mut Writer, _ctx: Ctx) -> Result<()> {
        write_short_blob(writer, &self.shared_secret)?;
        write_short_blob(writer, &self.verify_token)
    }
}

/// Reads one `i16`-length-prefixed byte blob.
fn read_short_blob(reader: &mut Reader<'_>) -> Result<Vec<u8>> {
    let declared = reader.i16()?;
    let length = usize::try_from(declared).map_err(|_| Error::NegativeLength(i32::from(declared)))?;
    if length > MAX_ENCRYPTION_BLOB {
        return Err(Error::LimitExceeded {
            limit: MAX_ENCRYPTION_BLOB,
            actual: length,
        });
    }
    Ok(reader.bytes(length)?.to_vec())
}

/// Writes one `i16`-length-prefixed byte blob.
fn write_short_blob(writer: &mut Writer, blob: &[u8]) -> Result<()> {
    let length = i16::try_from(blob.len()).map_err(|_| {
        Error::Custom(format!(
            "encryption byte array of {} bytes does not fit an i16 prefix",
            blob.len()
        ))
    })?;
    writer.i16(length);
    writer.bytes(blob);
    Ok(())
}
