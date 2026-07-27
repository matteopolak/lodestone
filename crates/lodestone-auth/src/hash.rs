//! The Minecraft server-ID hash.
//!
//! During the online-mode handshake the client and server independently derive
//! the same string from the server ID, the shared secret and the server's
//! public key, and the client sends it to the session server as proof it holds
//! the secret. The hash is a **non-standard** rendering of a SHA-1 digest:
//! Mojang's original implementation formatted the digest with Java's
//! [`BigInteger(byte[]).toString(16)`], which treats the 20 bytes as a *signed*
//! two's-complement integer. Consequently:
//!
//! * a digest whose top bit is set is negative and is printed with a leading
//!   `-`, then the magnitude in hex;
//! * leading zero bytes collapse (there is no zero-padding to 40 hex digits).
//!
//! A naive "lowercase hex of the digest" reproduces the positive cases but
//! silently diverges on the ~50% of inputs that hash to a negative value, which
//! is exactly the class of bug this function exists to avoid.
//!
//! [`BigInteger(byte[]).toString(16)`]: https://docs.oracle.com/javase/8/docs/api/java/math/BigInteger.html

use num_bigint::BigInt;
use sha1::{Digest, Sha1};

/// Computes Minecraft's server-ID hash from its three inputs.
///
/// The digest is taken over `server_id` (ASCII) followed by the raw 16-byte
/// shared secret followed by the server's public key in DER form, then rendered
/// as a signed two's-complement hex string exactly as vanilla does.
///
/// # Examples
///
/// The canonical published vectors use the bare username as the sole input:
///
/// ```
/// use lodestone_auth::server_hash;
///
/// assert_eq!(server_hash("Notch", &[], &[]), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
/// assert_eq!(server_hash("jeb_", &[], &[]), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
/// ```
#[must_use]
pub fn server_hash(server_id: &str, shared_secret: &[u8], public_key_der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(server_id.as_bytes());
    hasher.update(shared_secret);
    hasher.update(public_key_der);
    let digest = hasher.finalize();
    // Java's BigInteger(byte[]) constructor reads a signed, big-endian,
    // two's-complement number; `to_str_radix(16)` then produces the exact same
    // text (including a leading '-' and no zero padding).
    BigInt::from_signed_bytes_be(&digest).to_str_radix(16)
}

#[cfg(test)]
mod tests {
    use super::server_hash;

    // These three vectors are the ones Mojang published for their own
    // implementation. They were independently reproduced here with Python's
    // stdlib (`hashlib.sha1` + `int.from_bytes(.., 'big', signed=True)`), which
    // shares no code with this crate, before being committed.
    #[test]
    fn matches_published_positive_vector_notch() {
        assert_eq!(
            server_hash("Notch", &[], &[]),
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        );
    }

    #[test]
    fn matches_published_negative_vector_jeb() {
        // The whole point: a negative digest must render with a leading '-'.
        let h = server_hash("jeb_", &[], &[]);
        assert_eq!(h, "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
        assert!(h.starts_with('-'));
    }

    #[test]
    fn matches_published_leading_zero_vector_simon() {
        // 39 hex digits, not 40: the leading zero nibble is not padded.
        let h = server_hash("simon", &[], &[]);
        assert_eq!(h, "88e16a1019277b15d58faf0541e11910eb756f6");
        assert_eq!(h.len(), 39);
    }

    #[test]
    fn concatenates_all_three_inputs_in_order() {
        // Same total bytes, different split point => same hash (proves we hash
        // the concatenation, not the pieces).
        let a = server_hash("ab", b"cd", b"ef");
        let b = server_hash("abcdef", b"", b"");
        assert_eq!(a, b);
    }
}
