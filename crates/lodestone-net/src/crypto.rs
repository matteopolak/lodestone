//! Connection cryptography: the AES-128-CFB8 stream cipher plus the handshake
//! helpers (shared-secret generation and RSA encryption of the secret and
//! verify token).
//!
//! The cipher itself is pure computation with no I/O and no RNG, so it compiles
//! to wasm untouched and the browser transport inherits online-mode encryption
//! for free — the same property the sans-IO framing layer has. Secret
//! generation and RSA live here too because they are the client half of the
//! handshake, but they are used only while establishing a connection, not on the
//! hot path — and they are **native-only** (see [`generate_shared_secret`]),
//! because online-mode auth needs the native-only `lodestone-auth` crate, so
//! gating them keeps `rsa`/`rand`/`getrandom 0.2` out of the browser build.

use std::fmt;

use aes::Aes128;
use cfb8::cipher::KeyIvInit;
#[cfg(not(target_arch = "wasm32"))]
use rsa::RsaPublicKey;
#[cfg(not(target_arch = "wasm32"))]
use rsa::pkcs1v15::Pkcs1v15Encrypt;
#[cfg(not(target_arch = "wasm32"))]
use rsa::pkcs8::DecodePublicKey;

use crate::error::{NetError, Result};

/// Length in bytes of the AES-128 key / the Minecraft shared secret.
pub const SHARED_SECRET_LEN: usize = 16;

type Aes128Cfb8Enc = cfb8::Encryptor<Aes128>;
type Aes128Cfb8Dec = cfb8::Decryptor<Aes128>;

/// A bidirectional AES-128-CFB8 cipher for one connection.
///
/// Minecraft keys **and** IVs both to the 16-byte shared secret, and maintains a
/// *separate* CFB8 feedback register per direction. Crucially the register is
/// stateful for the entire life of the connection: the cipher is created once
/// when encryption is enabled and every subsequent packet continues the same
/// keystream. Re-initialising per packet decrypts the first byte-block
/// correctly and then produces garbage — a bug that reads like a framing fault,
/// which is why the cipher lives here as a single long-lived object.
pub(crate) struct Cfb8Cipher {
    encryptor: Aes128Cfb8Enc,
    decryptor: Aes128Cfb8Dec,
}

impl Cfb8Cipher {
    /// Builds the cipher from the 16-byte shared secret.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::BadSharedSecret`] if `secret` is not exactly
    /// [`SHARED_SECRET_LEN`] bytes.
    pub(crate) fn new(secret: &[u8]) -> Result<Self> {
        if secret.len() != SHARED_SECRET_LEN {
            return Err(NetError::BadSharedSecret { len: secret.len() });
        }
        // Minecraft keys and IVs both to the shared secret.
        let encryptor = Aes128Cfb8Enc::new_from_slices(secret, secret)
            .map_err(|_| NetError::BadSharedSecret { len: secret.len() })?;
        let decryptor = Aes128Cfb8Dec::new_from_slices(secret, secret)
            .map_err(|_| NetError::BadSharedSecret { len: secret.len() })?;
        Ok(Self {
            encryptor,
            decryptor,
        })
    }

    /// Encrypts `buf` in place, advancing the outgoing keystream.
    pub(crate) fn encrypt(&mut self, buf: &mut [u8]) {
        self.encryptor.encrypt(buf);
    }

    /// Decrypts `buf` in place, advancing the incoming keystream.
    pub(crate) fn decrypt(&mut self, buf: &mut [u8]) {
        self.decryptor.decrypt(buf);
    }
}

impl fmt::Debug for Cfb8Cipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never expose key material.
        f.write_str("Cfb8Cipher(..)")
    }
}

/// Generates a fresh 16-byte shared secret using the OS CSPRNG.
///
/// Native-only: the shared-secret and RSA handshake helpers are gated to
/// `cfg(not(target_arch = "wasm32"))` because completing an online-mode join
/// also requires [`lodestone-auth`](../lodestone_auth/index.html)'s
/// session-server call, which is itself native-only. Keeping this path off wasm
/// removes `rsa` and `rand` (and their `rand_core 0.6` / `getrandom 0.2` pin)
/// from the browser dependency tree entirely, rather than dragging in a wasm RNG
/// shim for code the browser cannot yet reach. The pure [`Cfb8Cipher`] stays
/// cross-platform, so the browser still gets the stream cipher for free; when a
/// wasm auth story exists, this seam is where a wasm RNG choice lands.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn generate_shared_secret() -> [u8; SHARED_SECRET_LEN] {
    use rand::RngCore;
    let mut secret = [0u8; SHARED_SECRET_LEN];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    secret
}

/// Encrypts `data` for the server using its DER-encoded (SPKI) RSA public key
/// with PKCS#1 v1.5 padding.
///
/// This is used to wrap both the shared secret and the verify token in the
/// `EncryptionResponse`. Minecraft servers ship a 1024-bit key in
/// SubjectPublicKeyInfo form, exactly what [`DecodePublicKey`] parses.
///
/// Native-only for the same reason as [`generate_shared_secret`]; see its docs.
///
/// # Errors
///
/// Returns [`NetError::Rsa`] if the key cannot be parsed or the encryption
/// fails (for example, `data` too long for the modulus).
#[cfg(not(target_arch = "wasm32"))]
pub fn rsa_encrypt(public_key_der: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let key = RsaPublicKey::from_public_key_der(public_key_der)
        .map_err(|e| NetError::Rsa(e.to_string()))?;
    let mut rng = rand::rngs::OsRng;
    key.encrypt(&mut rng, Pkcs1v15Encrypt, data)
        .map_err(|e| NetError::Rsa(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // NIST SP 800-38A F.3.7 CFB8-AES128, cross-checked against pyca/cryptography.
    const NIST_KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
    const NIST_IV: &str = "000102030405060708090a0b0c0d0e0f";
    const NIST_PT: &str = "6bc1bee22e409f96e93d7e117393172aae2d";
    const NIST_CT: &str = "3b79424c9c0dd436bace9e0ed4586a4f32b9";

    /// Build a cipher with distinct key and IV (NIST vector uses different
    /// values), bypassing the Minecraft key==iv==secret convention.
    fn nist_cipher() -> (Aes128Cfb8Enc, Aes128Cfb8Dec) {
        let key = hex(NIST_KEY);
        let iv = hex(NIST_IV);
        (
            Aes128Cfb8Enc::new_from_slices(&key, &iv).unwrap(),
            Aes128Cfb8Dec::new_from_slices(&key, &iv).unwrap(),
        )
    }

    #[test]
    fn cfb8_matches_nist_vector_encrypt_and_decrypt() {
        let (mut enc, mut dec) = nist_cipher();
        let mut buf = hex(NIST_PT);
        enc.encrypt(&mut buf);
        assert_eq!(buf, hex(NIST_CT), "encrypt must match NIST/pyca vector");
        dec.decrypt(&mut buf);
        assert_eq!(buf, hex(NIST_PT), "decrypt must invert");
    }

    #[test]
    fn cfb8_is_stateful_across_chunk_boundaries() {
        // Encrypting the whole buffer at once must equal encrypting it in
        // arbitrary chunks with the *same* cipher instance.
        let key = hex(NIST_KEY);
        let iv = hex(NIST_IV);

        let pt = hex(NIST_PT);
        let mut one_shot = Aes128Cfb8Enc::new_from_slices(&key, &iv).unwrap();
        let mut whole = pt.clone();
        one_shot.encrypt(&mut whole);

        let mut chunked = Aes128Cfb8Enc::new_from_slices(&key, &iv).unwrap();
        let mut streamed = Vec::new();
        for chunk in pt.chunks(3) {
            let mut c = chunk.to_vec();
            chunked.encrypt(&mut c);
            streamed.extend_from_slice(&c);
        }
        assert_eq!(streamed, whole, "chunked stream must equal one-shot");
        assert_eq!(streamed, hex(NIST_CT));
    }

    #[test]
    fn per_packet_reinit_is_wrong_after_first_block() {
        // Demonstrates the classic bug: a fresh cipher per chunk decrypts the
        // first byte correctly then diverges, proving statefulness is required.
        let key = hex(NIST_KEY);
        let iv = hex(NIST_IV);
        let pt = hex(NIST_PT);

        let mut reinit = Vec::new();
        for chunk in pt.chunks(3) {
            // WRONG on purpose: new cipher each chunk.
            let mut enc = Aes128Cfb8Enc::new_from_slices(&key, &iv).unwrap();
            let mut c = chunk.to_vec();
            enc.encrypt(&mut c);
            reinit.extend_from_slice(&c);
        }
        let correct = hex(NIST_CT);
        assert_eq!(reinit[0], correct[0], "first byte still matches");
        assert_ne!(reinit, correct, "but the reinitialised stream is wrong");
    }

    #[test]
    fn cipher_wrapper_roundtrips_with_minecraft_key_equals_iv() {
        let secret = [0x42u8; SHARED_SECRET_LEN];
        let mut sender = Cfb8Cipher::new(&secret).unwrap();
        let mut receiver = Cfb8Cipher::new(&secret).unwrap();

        let messages: [&[u8]; 3] = [b"hello", b"a much longer second message!!", b"third"];
        for msg in messages {
            let mut buf = msg.to_vec();
            sender.encrypt(&mut buf);
            assert_ne!(&buf, msg, "ciphertext must differ from plaintext");
            receiver.decrypt(&mut buf);
            assert_eq!(&buf, msg, "receiver must recover plaintext across packets");
        }
    }

    #[test]
    fn cipher_rejects_wrong_length_secret() {
        assert!(matches!(
            Cfb8Cipher::new(&[0u8; 15]),
            Err(NetError::BadSharedSecret { len: 15 })
        ));
        assert!(matches!(
            Cfb8Cipher::new(&[0u8; 17]),
            Err(NetError::BadSharedSecret { len: 17 })
        ));
    }

    #[test]
    fn generate_shared_secret_is_16_bytes_and_varies() {
        let a = generate_shared_secret();
        let b = generate_shared_secret();
        assert_eq!(a.len(), 16);
        assert_ne!(a, b, "two secrets must not collide");
    }

    #[test]
    fn rsa_encrypt_roundtrips_against_a_generated_key() {
        use rsa::pkcs8::EncodePublicKey;
        use rsa::{RsaPrivateKey, pkcs1v15::Pkcs1v15Encrypt};

        let mut rng = rand::rngs::OsRng;
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let pub_der = RsaPublicKey::from(&priv_key).to_public_key_der().unwrap();

        let secret = generate_shared_secret();
        let ciphertext = rsa_encrypt(pub_der.as_bytes(), &secret).unwrap();
        let recovered = priv_key.decrypt(Pkcs1v15Encrypt, &ciphertext).unwrap();
        assert_eq!(recovered, secret, "server-side decrypt must recover secret");
    }
}
