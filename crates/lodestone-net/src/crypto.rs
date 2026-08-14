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
use rsa::RsaPrivateKey;
#[cfg(not(target_arch = "wasm32"))]
use rsa::RsaPublicKey;
#[cfg(not(target_arch = "wasm32"))]
use rsa::pkcs1v15::Pkcs1v15Encrypt;
#[cfg(not(target_arch = "wasm32"))]
use rsa::pkcs8::DecodePublicKey;
#[cfg(not(target_arch = "wasm32"))]
use rsa::pkcs8::EncodePublicKey;

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

/// Length in bytes of the online-mode verify-token challenge a server sends
/// alongside `EncryptionRequest`, matching vanilla's own framing
/// (`Ints.toByteArray(RandomSource.create().nextInt())` —
/// `ServerLoginPacketListenerImpl`'s constructor — a 4-byte big-endian `int`).
pub const VERIFY_TOKEN_LEN: usize = 4;

/// Generates a fresh 4-byte verify-token challenge using the OS CSPRNG. The
/// server half of the handshake: it is sent in the clear inside
/// `EncryptionRequest`, and the client must echo it back RSA-encrypted so the
/// server can confirm the reply actually used its public key.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn generate_verify_token() -> [u8; VERIFY_TOKEN_LEN] {
    use rand::RngCore;
    let mut token = [0u8; VERIFY_TOKEN_LEN];
    rand::rngs::OsRng.fill_bytes(&mut token);
    token
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

/// The server half of the online-mode handshake: a 1024-bit RSA keypair, the
/// same size vanilla ships (`MinecraftServer.keyPair`, generated via
/// `KeyPairGenerator.getInstance("RSA")` seeded at 1024 in
/// `Crypt.generateKeyPair`). Holds the private key for decrypting the
/// client's `EncryptionResponse` and the DER (SubjectPublicKeyInfo) form of
/// the public half, which is what travels on the wire unmodified inside
/// `EncryptionRequest`.
///
/// **Generated per connection, not once per server.** Vanilla caches one
/// keypair for the process lifetime; this type's constructor is instead
/// called fresh by whichever code drives a single login (`lodestone-server`'s
/// connection loop), because the `ServerProtocol` implementors in this repo
/// are deliberately stateless (`V770ServerProtocol` is a unit struct — see
/// its own doc comment on why `ChunkEncoder` is implemented on it directly
/// rather than through a field). Nothing in the wire protocol requires one
/// keypair per *server*: within a single connection, all that matters is that
/// the public key sent in `EncryptionRequest` and the private key used to
/// decrypt that connection's `EncryptionResponse` are the same pair, which a
/// fresh keypair per login satisfies. The cost (RSA-1024 keygen, tens of
/// milliseconds) is paid only by a connection that actually reaches online-mode
/// login, not on every packet.
#[cfg(not(target_arch = "wasm32"))]
pub struct ServerKeyPair {
    private: RsaPrivateKey,
    public_der: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl fmt::Debug for ServerKeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never expose key material; the DER-encoded public key is not
        // secret, but printing it verbatim would still be noise in a log.
        f.write_str("ServerKeyPair(..)")
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ServerKeyPair {
    /// Generates a fresh 1024-bit RSA keypair using the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::Rsa`] if key generation or the public-key DER
    /// encoding fails.
    pub fn generate() -> Result<Self> {
        let mut rng = rand::rngs::OsRng;
        let private =
            RsaPrivateKey::new(&mut rng, 1024).map_err(|e| NetError::Rsa(e.to_string()))?;
        let public_der = RsaPublicKey::from(&private)
            .to_public_key_der()
            .map_err(|e| NetError::Rsa(e.to_string()))?
            .into_vec();
        Ok(Self {
            private,
            public_der,
        })
    }

    /// The DER-encoded (SPKI) public key, sent verbatim in `EncryptionRequest`
    /// and hashed (unmodified) into the session-server's server-id digest.
    #[must_use]
    pub fn public_key_der(&self) -> &[u8] {
        &self.public_der
    }

    /// Decrypts a client-submitted ciphertext (the shared secret or the
    /// verify token) with PKCS#1 v1.5 padding — the inverse of
    /// [`rsa_encrypt`].
    ///
    /// # Errors
    ///
    /// Returns [`NetError::Rsa`] if decryption fails (wrong key, corrupt
    /// ciphertext, or bad padding).
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        self.private
            .decrypt(Pkcs1v15Encrypt, ciphertext)
            .map_err(|e| NetError::Rsa(e.to_string()))
    }
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
    fn generate_verify_token_is_4_bytes_and_varies() {
        let a = generate_verify_token();
        let b = generate_verify_token();
        assert_eq!(a.len(), VERIFY_TOKEN_LEN);
        assert_ne!(a, b, "two tokens must not collide");
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

    #[test]
    fn server_keypair_decrypts_what_the_client_side_rsa_encrypt_produces() {
        // Mirrors the test above from the other role: `ServerKeyPair` is the
        // server side of the exact same handshake `rsa_encrypt` already
        // proves for the client. Using `rsa_encrypt` (the client's own
        // function, already NIST/pyca-independent for the cipher and
        // round-trip-verified for RSA above) as the encryptor, rather than
        // hand-rolling a second encrypt call, is what makes this a real
        // cross-role check rather than testing `ServerKeyPair` against
        // itself.
        let server_key = ServerKeyPair::generate().unwrap();

        // Pairwise-distinct: the shared secret and verify token are two
        // adjacent same-shaped byte buffers travelling through the same
        // decrypt call, so they must differ from each other to catch a
        // transposition.
        let secret = generate_shared_secret();
        let verify_token: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
        assert_ne!(&secret[..4], &verify_token[..], "fixture must be distinguishable");

        let enc_secret = rsa_encrypt(server_key.public_key_der(), &secret).unwrap();
        let enc_token = rsa_encrypt(server_key.public_key_der(), &verify_token).unwrap();

        let dec_secret = server_key.decrypt(&enc_secret).unwrap();
        let dec_token = server_key.decrypt(&enc_token).unwrap();

        assert_eq!(dec_secret, secret, "server must recover the exact shared secret");
        assert_eq!(dec_token, verify_token, "server must recover the exact verify token");
        assert_ne!(dec_secret, dec_token, "the two decrypted buffers must not collide");
    }

    #[test]
    fn server_keypair_public_der_is_parseable_by_the_client_side_decoder() {
        // The public key travels the wire as opaque bytes inside
        // `EncryptionRequest`; a real client feeds it straight into
        // `RsaPublicKey::from_public_key_der` (what `rsa_encrypt` does
        // internally). If `ServerKeyPair::generate` ever emitted PKCS#1
        // rather than SPKI DER, `rsa_encrypt` against it would fail — this
        // test is that check, independent of the round-trip test above.
        let server_key = ServerKeyPair::generate().unwrap();
        let probe = [7u8; 16];
        assert!(rsa_encrypt(server_key.public_key_der(), &probe).is_ok());
    }

    #[test]
    fn wrong_keypair_cannot_decrypt() {
        // Negative control: a ciphertext encrypted for one keypair must not
        // decrypt under a different one — proves `decrypt` is actually
        // checking against its own private key rather than, say, ignoring
        // padding failures.
        let right_key = ServerKeyPair::generate().unwrap();
        let wrong_key = ServerKeyPair::generate().unwrap();
        let secret = generate_shared_secret();
        let ciphertext = rsa_encrypt(right_key.public_key_der(), &secret).unwrap();
        assert!(wrong_key.decrypt(&ciphertext).is_err());
    }
}
