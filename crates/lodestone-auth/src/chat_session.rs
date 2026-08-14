//! Secure chat: the Mojang-issued RSA profile key pair, and the per-message
//! signing chain built on it. See `docs/secure-chat.md` for what this
//! plugs into (and does not yet plug into).
//!
//! Two halves, mirroring 26.2's client-side split:
//!
//! * **Key acquisition** ([`fetch_key_pair`]) — `AccountProfileKeyPairManager`
//!   in `.cache/mc/26.2/client-src`: a `POST` to
//!   `https://api.minecraftservices.com/player/certificates` (confirmed from
//!   `authlib-9.0.75.jar`'s `YggdrasilUserApiService.getKeyPair`/`routeKeyPair`
//!   constant pool — the route string `/player/certificates` and the response
//!   record's field names, including the `publicKeySignatureV2`
//!   `@SerializedName`, come from that jar directly since authlib itself is not
//!   in the decompiled source tree). Mojang generates the RSA-2048 key pair
//!   server-side and hands **both** halves to the authenticated client over
//!   HTTPS — there is no local keygen here, matching vanilla.
//! * **Per-message signing** ([`ChatSession`]) — `LocalChatSession` +
//!   `SignedMessageChain.Encoder` + `PlayerChatMessage.updateSignature` +
//!   `SignedMessageLink.updateSignature` + `SignedMessageBody.updateSignature`
//!   (`.cache/mc/26.2/src/net/minecraft/network/chat/`), hand-expanded clause
//!   by clause into [`build_signature_payload`] since there is no captured
//!   real signed-chat packet or published test vector for this scheme
//!   available here. **Independent check performed**: the RSA-SHA256-PKCS1v15
//!   primitive and this exact payload layout were verified against an
//!   out-of-tree oracle — an RSA-2048 key pair generated with `openssl`, and
//!   the same payload bytes signed with Python's `cryptography` library
//!   (OpenSSL-backed, no code or authorship shared with this crate or with
//!   `rsa`/RustCrypto) — see `sign_matches_an_independently_generated_oracle`.
//!   That test is the strongest evidence available here for "is the payload
//!   byte layout right", short of a real vanilla client's own signed packet.
//!
//! ## What is verified and what is not
//!
//! The oracle test above pins the RSA primitive and the payload byte layout.
//! What it **cannot** verify: whether a real vanilla server accepts a message
//! signed this way, since that requires a network round trip this crate's
//! tests never make (same standard the rest of this crate holds itself to —
//! see the crate doc's "What is and isn't verified"). Nothing here has been
//! checked against a live server.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AuthError, Result};

/// `POST`-only, empty body, `Authorization: Bearer <minecraft access token>` —
/// see this module's doc comment for how the route was confirmed.
const KEY_PAIR_URL: &str = "https://api.minecraftservices.com/player/certificates";

/// The RSA signature length vanilla hard-codes
/// (`MessageSignature.BYTES`/`Crypt.SIGNATURE_BYTES` = 256, i.e. a 2048-bit
/// key) and asserts on with `Preconditions.checkState` on every decode.
pub const SIGNATURE_BYTES: usize = 256;

/// Maximum entries in a last-seen window (`LastSeenMessages.LAST_SEEN_MESSAGES_MAX_LENGTH`).
pub const LAST_SEEN_MAX_LEN: usize = 20;

// --- Response shape -------------------------------------------------------

/// `/player/certificates`' JSON body. Field names and the `publicKeySignatureV2`
/// rename come from `KeyPairResponse`/`KeyPairResponse$KeyPair`'s constant pool
/// in `authlib-9.0.75.jar` (record component + `@SerializedName` annotation),
/// not from the (absent-here) decompiled source.
#[derive(Deserialize)]
struct KeyPairResponse {
    #[serde(rename = "keyPair")]
    key_pair: KeyPairData,
    /// Vanilla reads **this** field (`publicKeySignatureV2`), not the sibling
    /// `publicKeySignature` the response also carries — see
    /// `AccountProfileKeyPairManager.parsePublicKey`, which builds
    /// `ProfilePublicKey.Data` from `response.publicKeySignature()`, itself
    /// the Java getter for the `@SerializedName("publicKeySignatureV2")` field.
    #[serde(rename = "publicKeySignatureV2")]
    public_key_signature_v2: String,
    #[serde(rename = "expiresAt")]
    expires_at: String,
    #[serde(rename = "refreshedAfter")]
    refreshed_after: String,
}

#[derive(Deserialize)]
struct KeyPairData {
    #[serde(rename = "privateKey")]
    private_key: String,
    #[serde(rename = "publicKey")]
    public_key: String,
}

// --- Key pair ---------------------------------------------------------------

/// A Mojang-issued chat-signing RSA key pair, ready to sign outgoing chat.
///
/// Corresponds to vanilla's `ProfileKeyPair` (private key + `ProfilePublicKey`
/// + `refreshedAfter`), flattened: the public key is kept as the raw DER bytes
/// Mojang sent (see [`Self::public_key_der`]'s doc for why that is exactly
/// what the wire needs, with no re-encoding step to get subtly wrong).
#[derive(Clone)]
pub struct ChatKeyPair {
    private_key: RsaPrivateKey,
    public_key_der: Vec<u8>,
    key_signature: Vec<u8>,
    expires_at_millis: i64,
    refreshed_after_millis: i64,
}

/// Manual, not derived: a plain `#[derive(Debug)]` would print the RSA
/// private key's modulus/exponents (`RsaPrivateKey` does implement `Debug`),
/// which is exactly the kind of thing this crate's doc comment promises never
/// ends up in a log line.
impl std::fmt::Debug for ChatKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatKeyPair")
            .field("private_key", &"<redacted>")
            .field("public_key_der_len", &self.public_key_der.len())
            .field("key_signature_len", &self.key_signature.len())
            .field("expires_at_millis", &self.expires_at_millis)
            .field("refreshed_after_millis", &self.refreshed_after_millis)
            .finish()
    }
}

impl ChatKeyPair {
    /// DER-encoded (X.509 `SubjectPublicKeyInfo`) RSA public key, exactly as
    /// Mojang sent it.
    ///
    /// This is deliberately **not** re-derived from `private_key` at send
    /// time: `PublicKey.getEncoded()` in Java for a key built from
    /// `X509EncodedKeySpec` returns the identical bytes it was constructed
    /// from, so `FriendlyByteBuf.writePublicKey`'s `writeByteArray(key.getEncoded())`
    /// is byte-for-byte the DER Mojang issued. Keeping the original bytes
    /// instead of re-encoding a parsed [`RsaPublicKey`] removes an entire
    /// class of "our encoder disagrees with Mojang's" bug.
    #[must_use]
    pub fn public_key_der(&self) -> &[u8] {
        &self.public_key_der
    }

    /// Mojang's signature over the public key (`publicKeySignatureV2`),
    /// forwarded verbatim in `chat_session_update` — never verified by this
    /// client, only by servers, against Mojang's own services public key.
    #[must_use]
    pub fn key_signature(&self) -> &[u8] {
        &self.key_signature
    }

    /// Key expiry, epoch milliseconds — the exact wire representation
    /// `chat_session_update` sends (`ProfilePublicKey.Data.expiresAt`, written
    /// via `FriendlyByteBuf.writeInstant` = `writeLong(toEpochMilli())`).
    #[must_use]
    pub fn expires_at_millis(&self) -> i64 {
        self.expires_at_millis
    }

    /// Whether this key pair should be refreshed, given the current time
    /// (epoch milliseconds). Mirrors `ProfileKeyPair.dueRefresh`:
    /// `refreshedAfter.isBefore(Instant.now())`.
    #[must_use]
    pub fn due_refresh(&self, now_millis: i64) -> bool {
        self.refreshed_after_millis < now_millis
    }

    /// Assembles a [`ChatKeyPair`] from already-parsed parts, bypassing
    /// [`fetch_key_pair`]'s network round trip and [`parse_key_pair_response`]'s
    /// PEM/base64 framing.
    ///
    /// **Test-only fixture builder, not a real acquisition path.** Every
    /// field here is private for a reason (see the crate's evidence
    /// standard: nothing outside this module should be able to *mint* a
    /// signing key), and this exists solely so another crate's hermetic test
    /// — e.g. `lodestone-client`'s driver-level signed/unsigned choreography
    /// test — can construct a throwaway [`ChatSession`] without touching the
    /// real `/player/certificates` endpoint or a keychain. Never call this
    /// from anything but a test: the whole point of the real path is that
    /// Mojang, not the caller, generates the private key.
    #[doc(hidden)]
    #[must_use]
    pub fn for_tests(
        private_key: RsaPrivateKey,
        public_key_der: Vec<u8>,
        key_signature: Vec<u8>,
        expires_at_millis: i64,
        refreshed_after_millis: i64,
    ) -> Self {
        Self {
            private_key,
            public_key_der,
            key_signature,
            expires_at_millis,
            refreshed_after_millis,
        }
    }
}

/// Fetches a fresh [`ChatKeyPair`] for the authenticated account.
///
/// `mc_access_token` is the same Minecraft-services access token
/// [`crate::flow::Session::access_token`] carries — this call sits on the same
/// authenticated surface as [`crate::flow::fetch_profile`]/[`crate::flow::join_server`],
/// just a different endpoint on `api.minecraftservices.com`.
///
/// # Errors
/// [`AuthError::Http`]/[`AuthError::Json`] on transport/parse failure,
/// [`AuthError::Service`] if the endpoint rejects the request, or
/// [`AuthError::ChatSessionKeyMalformed`] if a 2xx body does not decode into a
/// usable RSA key pair (bad PEM framing, non-PKCS8 private key bytes, or a
/// missing/empty `publicKeySignatureV2`).
pub async fn fetch_key_pair(
    client: &reqwest::Client,
    mc_access_token: &str,
) -> Result<ChatKeyPair> {
    let http = client
        .post(KEY_PAIR_URL)
        .bearer_auth(mc_access_token)
        .send()
        .await?;
    if !http.status().is_success() {
        let status = http.status();
        let text = http.text().await.unwrap_or_default();
        return Err(AuthError::Service {
            step: "chat_session_key_pair",
            message: format!("{status}: {text}"),
        });
    }
    let resp: KeyPairResponse = http.json().await?;
    parse_key_pair_response(resp)
}

fn parse_key_pair_response(resp: KeyPairResponse) -> Result<ChatKeyPair> {
    let private_der = extract_pem_der(
        &resp.key_pair.private_key,
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----END RSA PRIVATE KEY-----",
    )?;
    // Despite the PKCS#1-style header text, the bytes inside are PKCS#8
    // (`Crypt.byteToPrivateKey` parses them with `PKCS8EncodedKeySpec`) — a
    // Mojang quirk, not a mistake here; a real PKCS#1 `RSAPrivateKey` DER
    // would fail this parse.
    let private_key = RsaPrivateKey::from_pkcs8_der(&private_der).map_err(|e| {
        AuthError::ChatSessionKeyMalformed(format!("private key is not PKCS#8 DER: {e}"))
    })?;

    let public_key_der = extract_pem_der(
        &resp.key_pair.public_key,
        "-----BEGIN RSA PUBLIC KEY-----",
        "-----END RSA PUBLIC KEY-----",
    )?;
    // Sanity-parse as X.509 SubjectPublicKeyInfo (`Crypt.byteToPublicKey` uses
    // `X509EncodedKeySpec`) so a malformed key fails here rather than as a
    // mysterious rejection three steps later. The parsed value is discarded —
    // see `ChatKeyPair::public_key_der`'s doc for why the raw bytes, not this
    // parse's re-encoding, are what the wire carries.
    let _: RsaPublicKey = RsaPublicKey::from_public_key_der(&public_key_der).map_err(|e| {
        AuthError::ChatSessionKeyMalformed(format!("public key is not X.509 SPKI DER: {e}"))
    })?;

    let key_signature = BASE64.decode(resp.public_key_signature_v2.trim()).map_err(|e| {
        AuthError::ChatSessionKeyMalformed(format!("publicKeySignatureV2 is not base64: {e}"))
    })?;
    if key_signature.is_empty() {
        return Err(AuthError::ChatSessionKeyMalformed(
            "publicKeySignatureV2 was empty".to_owned(),
        ));
    }

    let expires_at_millis = parse_iso8601_millis(&resp.expires_at)?;
    let refreshed_after_millis = parse_iso8601_millis(&resp.refreshed_after)?;

    Ok(ChatKeyPair {
        private_key,
        public_key_der,
        key_signature,
        expires_at_millis,
        refreshed_after_millis,
    })
}

/// Extracts and base64-decodes the DER payload from one of Mojang's PEM-ish
/// strings, tolerant of the same things Java's `Base64.getMimeDecoder()` is:
/// line breaks inside the block and characters outside the base64 alphabet
/// (`Crypt.rsaStringToKey` locates `header`/`footer` by substring and hands
/// the decoder whatever is between them without trimming precisely, relying
/// on the MIME decoder's tolerance for stray non-alphabet bytes; filtering to
/// the base64 alphabet here reproduces that tolerance directly rather than
/// replicating the same off-by-one).
///
/// Falls back to treating the whole (whitespace-stripped) string as the
/// base64 body when the header/footer markers are absent, matching
/// `rsaStringToKey`'s `begin == -1` branch.
fn extract_pem_der(pem: &str, header: &str, footer: &str) -> Result<Vec<u8>> {
    let body = match (pem.find(header), pem.find(footer)) {
        (Some(start), Some(end)) if end > start => &pem[start + header.len()..end],
        _ => pem,
    };
    let cleaned: String = body
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .collect();
    BASE64
        .decode(cleaned)
        .map_err(|e| AuthError::ChatSessionKeyMalformed(format!("invalid base64 in PEM block: {e}")))
}

/// Parses a Java `Instant.toString()`-shaped UTC timestamp
/// (`yyyy-MM-ddTHH:mm:ss[.fraction]Z`, the shape `expiresAt`/`refreshedAfter`
/// use) into epoch milliseconds, with no external date/time dependency.
///
/// Day-number arithmetic is Howard Hinnant's `days_from_civil` (public-domain,
/// widely used — <https://howardhinnant.github.io/date_algorithms.html>), a
/// textbook proleptic-Gregorian formula rather than anything specific to this
/// scheme; see this function's tests for values cross-checked against
/// Python's `datetime`.
fn parse_iso8601_millis(s: &str) -> Result<i64> {
    let malformed = || AuthError::ChatSessionKeyMalformed(format!("not a valid instant: {s:?}"));
    let s = s.strip_suffix('Z').ok_or_else(malformed)?;
    let (date, time) = s.split_once('T').ok_or_else(malformed)?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next().ok_or_else(malformed)?.parse().map_err(|_| malformed())?;
    let month: u32 = date_parts.next().ok_or_else(malformed)?.parse().map_err(|_| malformed())?;
    let day: u32 = date_parts.next().ok_or_else(malformed)?.parse().map_err(|_| malformed())?;
    if date_parts.next().is_some() {
        return Err(malformed());
    }

    let (hms, frac_millis) = match time.split_once('.') {
        Some((hms, frac)) => {
            // Java's fractional seconds can be 0-9 digits; normalise to
            // exactly 3 (milliseconds) by right-padding then truncating.
            let mut digits: String = frac.chars().take(9).collect();
            while digits.len() < 3 {
                digits.push('0');
            }
            let millis: i64 = digits[..3].parse().map_err(|_| malformed())?;
            (hms, millis)
        }
        None => (time, 0),
    };
    let mut hms_parts = hms.split(':');
    let hour: i64 = hms_parts.next().ok_or_else(malformed)?.parse().map_err(|_| malformed())?;
    let minute: i64 = hms_parts.next().ok_or_else(malformed)?.parse().map_err(|_| malformed())?;
    let second: i64 = hms_parts.next().ok_or_else(malformed)?.parse().map_err(|_| malformed())?;
    if hms_parts.next().is_some() {
        return Err(malformed());
    }

    let days = days_from_civil(year, month, day);
    let seconds_of_day = hour * 3600 + minute * 60 + second;
    Ok(days * 86_400_000 + seconds_of_day * 1000 + frac_millis)
}

/// Days since the Unix epoch (1970-01-01) for a proleptic-Gregorian date.
/// See [`parse_iso8601_millis`]'s doc for the source.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (i64::from(m) + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

// --- Signing chain ----------------------------------------------------------

/// One link in the per-session signature chain (`SignedMessageLink`): the
/// message's position, and the sender/session identity every message in the
/// chain repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedMessageLink {
    /// Chain position, starting at 0 for the session's first message.
    pub index: i32,
    /// The signing account's profile UUID.
    pub sender: Uuid,
    /// This chat session's UUID (`LocalChatSession::create`'s
    /// `UUID.randomUUID()`, client-generated once per session).
    pub session_id: Uuid,
}

impl SignedMessageLink {
    /// The first link in a new session's chain (`SignedMessageLink.root`).
    #[must_use]
    pub fn root(sender: Uuid, session_id: Uuid) -> Self {
        Self {
            index: 0,
            sender,
            session_id,
        }
    }

    /// The next link, or `None` at `i32::MAX` — vanilla's own signal
    /// (`SignedMessageLink.advance`'s ternary) that a session has run out of
    /// chain and a new one must be started.
    #[must_use]
    pub fn advance(self) -> Option<Self> {
        if self.index == i32::MAX {
            None
        } else {
            Some(Self {
                index: self.index + 1,
                ..self
            })
        }
    }

    /// Appends this link's signed bytes: `SignedMessageLink.updateSignature` —
    /// sender UUID (16 bytes, `UUIDUtil.uuidToByteArray`: MSB then LSB,
    /// big-endian each — identical to [`Uuid::as_bytes`]'s layout), session
    /// UUID (16 bytes, same layout), then the index as a big-endian `i32`.
    fn write_signed_bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.sender.as_bytes());
        out.extend_from_slice(self.session_id.as_bytes());
        out.extend_from_slice(&self.index.to_be_bytes());
    }
}

/// Builds the exact byte payload `PlayerChatMessage.updateSignature` signs,
/// clause by clause against the decompiled 26.2 source (no captured packet or
/// published vector exists for this scheme — see this module's doc comment
/// for the independent check that was possible instead):
///
/// 1. `PlayerChatMessage.updateSignature`: a constant version tag,
///    `Ints.toByteArray(1)` — big-endian `i32` `1`.
/// 2. `SignedMessageLink.updateSignature`: sender UUID, session UUID, index —
///    see [`SignedMessageLink::write_signed_bytes`].
/// 3. `SignedMessageBody.updateSignature`, in order:
///    - `Longs.toByteArray(salt)` — big-endian `i64`.
///    - `Longs.toByteArray(timeStamp.getEpochSecond())` — big-endian `i64`,
///      **epoch seconds**, not the milliseconds the wire packet itself
///      carries (`ChatMessage`/`ServerboundChatPacket` use
///      `writeInstant`/`readInstant`, which is epoch *millis* —
///      `updateSignature` is the one place seconds appear; conflating the two
///      is exactly the kind of subtle-port mistake this repo's evidence
///      standard exists to catch, so it is called out here explicitly).
///    - `Ints.toByteArray(contentBytes.length)` then `contentBytes` — a
///      big-endian `i32` UTF-8 byte length prefix, then the UTF-8 bytes
///      themselves (not a `writeUtf`-style VarInt-prefixed string: this is
///      the *signature* payload, not the wire encoding).
///    - `LastSeenMessages.updateSignature`: `Ints.toByteArray(entries.size())`
///      (big-endian `i32`) then each entry's raw signature bytes
///      concatenated in order.
///
/// `last_seen` entries must each be exactly [`SIGNATURE_BYTES`] long — the
/// same 256-byte RSA signatures this function itself produces for earlier
/// messages, per `MessageSignature`'s own `Preconditions.checkState`.
#[must_use]
pub fn build_signature_payload(
    link: &SignedMessageLink,
    content: &str,
    timestamp_epoch_seconds: i64,
    salt: i64,
    last_seen: &[[u8; SIGNATURE_BYTES]],
) -> Vec<u8> {
    let content_bytes = content.as_bytes();
    let mut out = Vec::with_capacity(
        4 + 16 + 16 + 4 + 8 + 8 + 4 + content_bytes.len() + 4 + last_seen.len() * SIGNATURE_BYTES,
    );
    out.extend_from_slice(&1i32.to_be_bytes());
    link.write_signed_bytes(&mut out);
    out.extend_from_slice(&salt.to_be_bytes());
    out.extend_from_slice(&timestamp_epoch_seconds.to_be_bytes());
    out.extend_from_slice(&(content_bytes.len() as i32).to_be_bytes());
    out.extend_from_slice(content_bytes);
    out.extend_from_slice(&(last_seen.len() as i32).to_be_bytes());
    for entry in last_seen {
        out.extend_from_slice(entry);
    }
    out
}

/// Signs `payload` with `SHA256withRSA` (RSASSA-PKCS1-v1.5, `Crypt.SIGNING_ALGORITHM`)
/// — `Signer.from(privateKey, "SHA256withRSA")`. Deterministic: PKCS#1 v1.5
/// signature padding needs no randomness (unlike PSS or OAEP), so this needs
/// no RNG and no `getrandom` — see `Cargo.toml`'s comment on why `rsa` is
/// native-only here for a different reason (the crate's own `rand_core`
/// pin), not because signing itself needs entropy.
fn sign_payload(private_key: &RsaPrivateKey, payload: &[u8]) -> Result<[u8; SIGNATURE_BYTES]> {
    let digest = Sha256::digest(payload);
    let sig = private_key
        .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
        .map_err(|e| AuthError::ChatSessionKeyMalformed(format!("signing failed: {e}")))?;
    sig.try_into()
        .map_err(|v: Vec<u8>| AuthError::ChatSessionKeyMalformed(format!(
            "signature was {} bytes, expected {SIGNATURE_BYTES}",
            v.len()
        )))
}

/// A live chat-signing session: the client-generated session UUID, the
/// Mojang-issued key pair, and the chain-position cursor.
///
/// Mirrors `LocalChatSession` (session id + key pair) fused with the encoder
/// half of `SignedMessageChain` (the `nextLink` cursor) — 26.2 keeps these as
/// two collaborating objects (`LocalChatSession::createMessageEncoder`
/// constructs a fresh `SignedMessageChain` around the same key), but nothing
/// here needs that indirection since one client only ever encodes, never
/// decodes, its own chain.
pub struct ChatSession {
    session_id: Uuid,
    key_pair: ChatKeyPair,
    next_link: Option<SignedMessageLink>,
}

/// Manual for the same reason as [`ChatKeyPair`]'s: derives through to the
/// private key otherwise.
impl std::fmt::Debug for ChatSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatSession")
            .field("session_id", &self.session_id)
            .field("key_pair", &self.key_pair)
            .field("next_link", &self.next_link)
            .finish()
    }
}

impl ChatSession {
    /// Starts a new session: a fresh random session UUID
    /// (`LocalChatSession::create`'s `UUID.randomUUID()`) and the chain rooted
    /// at index 0 for `sender`.
    #[must_use]
    pub fn new(sender: Uuid, key_pair: ChatKeyPair) -> Self {
        let session_id = Uuid::new_v4();
        Self {
            session_id,
            next_link: Some(SignedMessageLink::root(sender, session_id)),
            key_pair,
        }
    }

    /// This session's UUID — the one `chat_session_update` announces and
    /// every signed message's link carries.
    #[must_use]
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// The underlying key pair, e.g. to build a `chat_session_update` packet.
    #[must_use]
    pub fn key_pair(&self) -> &ChatKeyPair {
        &self.key_pair
    }

    /// Signs one outgoing message and advances the chain
    /// (`SignedMessageChain.Encoder`'s closure: read `nextLink`, advance it,
    /// sign against the *pre-advance* link). Returns `None` once the chain is
    /// exhausted (`SignedMessageLink::advance` hit `i32::MAX`) — vanilla's own
    /// signal that this session must be replaced with a new one, mirroring
    /// `SignedMessageChain.Encoder.pack` returning `null` after
    /// `this.nextLink = null`.
    ///
    /// # Errors
    /// Propagates a signing failure from the underlying RSA operation.
    pub fn sign(
        &mut self,
        content: &str,
        timestamp_epoch_seconds: i64,
        salt: i64,
        last_seen: &[[u8; SIGNATURE_BYTES]],
    ) -> Result<Option<([u8; SIGNATURE_BYTES], i32)>> {
        let Some(link) = self.next_link else {
            return Ok(None);
        };
        self.next_link = link.advance();
        let payload = build_signature_payload(&link, content, timestamp_epoch_seconds, salt, last_seen);
        let signature = sign_payload(&self.key_pair.private_key, &payload)?;
        Ok(Some((signature, link.index)))
    }
}

/// Verifies a received message's signature against a sender's announced
/// public key (`RemoteChatSession.createMessageDecoder` +
/// `SignedMessageChain.Decoder.unpack`'s `unpacked.verify(signatureValidator)`
/// call, i.e. `PlayerChatMessage.verify` → `MessageSignature.verify` →
/// `SignatureValidator.from(publicKey, "SHA256withRSA")`).
///
/// This is the decode-side primitive only: it does **not** track a per-sender
/// chain, enforce link ordering/expiry, or feed a "verified" badge into any
/// UI — those are the remainder of `SignedMessageChain.Decoder` and
/// `RemoteChatSession`'s state machine, out of scope here (see this crate's
/// report: rendering other players' messages as verified is explicitly a
/// later concern per issue #283's own text).
///
/// # Errors
/// [`AuthError::ChatSessionKeyMalformed`] if `public_key_der` does not parse
/// as an X.509 SPKI RSA key.
pub fn verify_signature(
    public_key_der: &[u8],
    link: &SignedMessageLink,
    content: &str,
    timestamp_epoch_seconds: i64,
    salt: i64,
    last_seen: &[[u8; SIGNATURE_BYTES]],
    signature: &[u8; SIGNATURE_BYTES],
) -> Result<bool> {
    let public_key = RsaPublicKey::from_public_key_der(public_key_der).map_err(|e| {
        AuthError::ChatSessionKeyMalformed(format!("public key is not X.509 SPKI DER: {e}"))
    })?;
    let payload = build_signature_payload(link, content, timestamp_epoch_seconds, salt, last_seen);
    let digest = Sha256::digest(payload);
    Ok(public_key
        .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, signature)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed RSA-2048 test key (PKCS#8 DER, base64), generated once with
    /// `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048` — shared
    /// by every test below that needs a real key, so the same fixture backs
    /// both the independent-oracle check and the ordinary unit tests.
    const TEST_PRIVATE_KEY_DER_B64: &str = concat!(
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDtM+q+4UwoW3cZ",
        "Q8TVkfa9TfGdxpl13PlfNei77mmWz+kLCxeOXpF2hX/VXSoxj3yBjjhtGHZB59eX",
        "0VW2zw+G913ZMmtT+9phKBA9BOID4c4hNpz852wJ5sp2pFOyrrg47UTrakey9iQT",
        "+ckO4qfeMR13NTDP44cLFBwa1/ot80Fwq00xg5KHJK6WeWmjPayc+lf3FSPC+cNO",
        "aOJ3oaWK16b2LFqvzwwkl53e0yyHFgffA5AdClVJgZc7pEDScO0zLHLqe8ySrbsJ",
        "yZ9PQSTNC7cmXkJPQjlYJJ2M4/+HJRtjY/CQyP5C7sTdu/Lhn1nUawhj74Egyvg8",
        "HXeysPeZAgMBAAECggEBAMg+0ee+jupq/MpJWbvqc2Awks7dP+QuXh8whX9Rr7Xv",
        "Yw89l+9KioaCAP8AnYQlW7iLdbszsXHF5U13HWMsvjD0VzfqxoypyxvGFJ9Opfcd",
        "A0Uqs7EVNTHOshEifL4VndQBCfOrT0gXXzG15zQ3x/tdf0CJmOGHdRO3MFrBBaUP",
        "XJgVcGCWyKK9/p+uV9lolnQprotiuctX6nI5hYAX7PG1XFJlPAW5k9DLE4W31+8Y",
        "FiJgsS/WTRAsvjs7zJefGwUNE0+86ylREEmSvHWqjS6pgxf7REZed0208kTHC1P8",
        "aGP9nnrHZfiKBDtxt2usRbG00Whf9NVTOZBeC9ExKjkCgYEA948Wr8q0lFVMZ7xt",
        "u5Dx8Mvjvz2Bl5wclX27qrqeu7T3aGnP2EwVSQW5xUB/KpYpxoMFiJIy9cVqo1XT",
        "Vege7i8WsGRK+D9xpd6QEhME79nIbltmxTVP9Ue9foBev0S0QM5n1Qk6L4hKUnva",
        "dwQ1Ow6XoPejGcu2BhYzywUPrJsCgYEA9UpuTzZgMg7CVCIRH6Ze8jNP56GADXYB",
        "8BH5hSuaKO67PukLa/iqSo38w1uZSVLvNgLxts5Q+pinSglJlZ8mRrLVFI8qkcIg",
        "j/qZKpVP0mfOuBYu/DNkX0VO4nG1pBSKgT1dmUiVVAvBfgbUHeG1vVEENKh0NbSH",
        "nswL84z8XdsCgYBpnapYJWsVPa7zMvi95QDTcqkfleYMAJZRUOsX07aU7of/C+WY",
        "qh0Kol63QOUADkCUaKGbuoPzRt5QAPXA2N8ZTw2nA6LYdnjOAz4D+AlLKubP7j7S",
        "NASA6LJ3ndzOTUl5vJWf1ef1D3hl6GE0FZ+AKqGWExCKmNZ3klFWdDpTsQKBgFG9",
        "FttApHep4WoF3Czu1O7i2Hq4n6Jcs7KbWsncyMdhHnaNVCgLujuT6ynyiTcc8ufN",
        "vVyMjgGkAwMx6xp36Vpf14+9UZM23ID+IjJFhU75FrLTeZ7DRWxV/T6KY9wkmC8P",
        "EvS0ckaKkFT904uNnnFS4RLnG6qV2Se6mTT0w1hHAoGADIwcasJrU/5xnBPICA6f",
        "u43x6dk1/v+GeRLz0N0aVADsj7tInJ+7pHV1/NrHaGONJKIQ0uWIKxVdHufDmYVU",
        "KY0Oh6wzS/m5Z2tmxK24z0UJyXvAu67ETx5QUhqH63i5km9a2Au+zkwGXBBg6Bvh",
        "7kWCpm322pipbRs6hKc7klQ=",
    );

    fn test_private_key() -> RsaPrivateKey {
        let der = BASE64.decode(TEST_PRIVATE_KEY_DER_B64).unwrap();
        RsaPrivateKey::from_pkcs8_der(&der).unwrap()
    }

    // --- ISO8601 parsing --------------------------------------------------

    #[test]
    fn epoch_parses_to_zero() {
        assert_eq!(parse_iso8601_millis("1970-01-01T00:00:00Z").unwrap(), 0);
    }

    /// Cross-checked against Python: `int(datetime(2024,1,1,tzinfo=timezone.utc).timestamp()*1000) == 1704067200000`.
    #[test]
    fn a_later_date_matches_an_independently_computed_value() {
        assert_eq!(
            parse_iso8601_millis("2024-01-01T00:00:00Z").unwrap(),
            1_704_067_200_000
        );
    }

    /// Real `Instant.toString()` output carries fractional seconds (up to
    /// nanosecond) with a variable digit count; this must normalise to
    /// milliseconds rather than mis-scale a short fraction.
    #[test]
    fn fractional_seconds_normalise_to_milliseconds() {
        assert_eq!(
            parse_iso8601_millis("2024-01-01T00:00:00.5Z").unwrap(),
            1_704_067_200_500
        );
        assert_eq!(
            parse_iso8601_millis("2024-01-01T00:00:00.123456789Z").unwrap(),
            1_704_067_200_123
        );
    }

    #[test]
    fn a_pre_epoch_date_is_negative() {
        // 1969-12-31T23:59:59Z is exactly one second before the epoch.
        assert_eq!(parse_iso8601_millis("1969-12-31T23:59:59Z").unwrap(), -1000);
    }

    #[test]
    fn malformed_instants_are_a_typed_error_not_a_panic() {
        assert!(parse_iso8601_millis("not an instant").is_err());
        assert!(parse_iso8601_millis("2024-01-01 00:00:00Z").is_err()); // missing 'T'
        assert!(parse_iso8601_millis("2024-01-01T00:00:00").is_err()); // missing 'Z'
    }

    // --- PEM extraction -----------------------------------------------------

    #[test]
    fn extracts_der_from_mojang_style_pem_markers() {
        let pem = "-----BEGIN RSA PUBLIC KEY-----\nAQID\n-----END RSA PUBLIC KEY-----\n";
        let der = extract_pem_der(
            pem,
            "-----BEGIN RSA PUBLIC KEY-----",
            "-----END RSA PUBLIC KEY-----",
        )
        .unwrap();
        assert_eq!(der, vec![1, 2, 3]);
    }

    #[test]
    fn falls_back_to_treating_the_whole_string_as_base64_with_no_markers() {
        let der = extract_pem_der("AQID", "-----BEGIN X-----", "-----END X-----").unwrap();
        assert_eq!(der, vec![1, 2, 3]);
    }

    // --- Signature payload layout -------------------------------------------

    /// The exact fixture used to build the outside oracle below: pairwise
    /// distinct sender/session UUIDs, index, salt and timestamp, a non-empty
    /// content string, and two distinct 256-byte last-seen entries (ascending
    /// vs. descending byte ramps) — chosen so a transposition of any two
    /// same-typed adjacent fields, or a swap between the two last-seen
    /// entries, would change the output.
    fn oracle_link() -> SignedMessageLink {
        SignedMessageLink {
            index: 7,
            sender: Uuid::from_bytes([
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0x00, 0xaa, 0xbb, 0xcc,
                0xdd, 0xee, 0xff,
            ]),
            session_id: Uuid::from_bytes([
                0xaa, 0xbb, 0xcc, 0xdd, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
                0x00, 0x11, 0x22,
            ]),
        }
    }

    fn oracle_last_seen() -> [[u8; SIGNATURE_BYTES]; 2] {
        let mut a = [0u8; SIGNATURE_BYTES];
        let mut b = [0u8; SIGNATURE_BYTES];
        for i in 0..SIGNATURE_BYTES {
            a[i] = i as u8;
            b[i] = (255 - i) as u8;
        }
        [a, b]
    }

    /// `build_signature_payload`'s output must equal the byte string produced
    /// by hand from the clause-by-clause spec — this is the layout check with
    /// no crypto involved, independent of the RSA oracle below.
    #[test]
    fn payload_layout_matches_the_hand_expanded_spec() {
        let link = oracle_link();
        let payload = build_signature_payload(&link, "Hello, Lodestone!", 1_700_000_000, 1_234_567_890_123, &oracle_last_seen());
        let payload_hex = payload.iter().map(|b| format!("{b:02x}")).collect::<String>();
        // Produced independently in Python from the same clause-by-clause
        // spec (`Ints.toByteArray(1) || link || salt || epoch-seconds ||
        // len(content) || content || len(last_seen) || each entry`) — see
        // this module's doc comment.
        assert_eq!(payload_hex, "0000000111223344556677889900aabbccddeeffaabbccdd112233445566778899001122000000070000011f71fb04cb000000006553f1000000001148656c6c6f2c204c6f646573746f6e652100000002000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedfe0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfefffffefdfcfbfaf9f8f7f6f5f4f3f2f1f0efeeedecebeae9e8e7e6e5e4e3e2e1e0dfdedddcdbdad9d8d7d6d5d4d3d2d1d0cfcecdcccbcac9c8c7c6c5c4c3c2c1c0bfbebdbcbbbab9b8b7b6b5b4b3b2b1b0afaeadacabaaa9a8a7a6a5a4a3a2a1a09f9e9d9c9b9a999897969594939291908f8e8d8c8b8a898887868584838281807f7e7d7c7b7a797877767574737271706f6e6d6c6b6a696867666564636261605f5e5d5c5b5a595857565554535251504f4e4d4c4b4a494847464544434241403f3e3d3c3b3a393837363534333231302f2e2d2c2b2a292827262524232221201f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100");
    }

    /// The strongest evidence available here: an RSA-2048 key pair generated
    /// with `openssl genpkey`, and the *exact same payload bytes* signed
    /// independently with Python's `cryptography` library (OpenSSL-backed —
    /// no code or authorship in common with this crate or with the `rsa`
    /// crate). If either the payload layout or the RSA signing primitive were
    /// wrong here, this would not match. What it cannot check is a real
    /// vanilla server accepting a message signed this way — see the module
    /// doc.
    #[test]
    fn sign_matches_an_independently_generated_oracle() {
        let private_key = test_private_key();

        let link = oracle_link();
        let payload = build_signature_payload(
            &link,
            "Hello, Lodestone!",
            1_700_000_000,
            1_234_567_890_123,
            &oracle_last_seen(),
        );
        let signature = sign_payload(&private_key, &payload).unwrap();

        // `cryptography.hazmat.primitives.asymmetric.padding.PKCS1v15()` +
        // `hashes.SHA256()`, signing the identical `payload` bytes with the
        // identical key (loaded from the same PKCS#8 DER above).
        let expected_hex = "d894846409624b65ebe2ae84bb153e50fa49bf2bd5bcbb10d21aca75a8d03ecce478ec13c7e344a958704747cfca35053a6784e1072439bff181e397495f9e614b82b9d6ee0bbea11be6c984cf9c7e363a63ce2a87ee47b0f878f1ffba199cf35665b6a81fa84cb704c9e1f3e12a6fed0be9e5661115fe1a5dab8d722b8b879a17a37014f41953405800e5f72e21727c555ad16c8350a26471dc10802d3cb923adbb84ff838c74783edfc49051efd50d2bc1c4965b7215ce7b70f77657891f3ee83d7a47c9268f0fcc90eb781bd91c19a2944311b4359218fd9ae929c2f617620dda51552849f91c9ad5ba4e724322d828b7c7955c15f6928736e3254d8fe46e";
        let signature_hex = signature.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(signature_hex, expected_hex);

        // The verifier must accept its own signer's output against the same
        // public key (derived from the private key here purely for the
        // round-trip; `verify_signature` in production is always called with
        // a public key that arrived over the network, never one derived
        // locally from a private key we should never have for anyone but
        // ourselves).
        let public_key = RsaPublicKey::from(&private_key);
        let public_der = {
            use rsa::pkcs8::EncodePublicKey;
            public_key.to_public_key_der().unwrap().into_vec()
        };
        assert!(
            verify_signature(
                &public_der,
                &link,
                "Hello, Lodestone!",
                1_700_000_000,
                1_234_567_890_123,
                &oracle_last_seen(),
                &signature,
            )
            .unwrap()
        );
    }

    /// A control for the verifier: flipping one byte of a verified-good
    /// signature must fail. Without this, `verify_signature` returning `Ok(x)`
    /// for any `bool` `x` would pass the positive test above vacuously if the
    /// comparison were backwards.
    #[test]
    fn verification_rejects_a_tampered_signature() {
        let private_key = test_private_key();
        let public_key = RsaPublicKey::from(&private_key);
        let public_der = {
            use rsa::pkcs8::EncodePublicKey;
            public_key.to_public_key_der().unwrap().into_vec()
        };

        let link = oracle_link();
        let mut signature = sign_payload(
            &private_key,
            &build_signature_payload(&link, "hi", 1_700_000_000, 42, &[]),
        )
        .unwrap();
        assert!(
            verify_signature(&public_der, &link, "hi", 1_700_000_000, 42, &[], &signature).unwrap()
        );
        signature[0] ^= 0xFF;
        assert!(
            !verify_signature(&public_der, &link, "hi", 1_700_000_000, 42, &[], &signature).unwrap()
        );
    }

    // --- Chain / session state ------------------------------------------

    #[test]
    fn link_advance_increments_index_and_keeps_identity() {
        let sender = Uuid::from_u128(1);
        let session = Uuid::from_u128(2);
        let root = SignedMessageLink::root(sender, session);
        assert_eq!(root.index, 0);
        let next = root.advance().unwrap();
        assert_eq!(next.index, 1);
        assert_eq!(next.sender, sender);
        assert_eq!(next.session_id, session);
    }

    #[test]
    fn link_advance_stops_at_i32_max() {
        let link = SignedMessageLink {
            index: i32::MAX,
            sender: Uuid::from_u128(1),
            session_id: Uuid::from_u128(2),
        };
        assert!(link.advance().is_none());
    }

    /// A `ChatSession`'s successive `sign` calls must produce **different**
    /// link indices (0, then 1) even for identical message content — the
    /// index is what makes replay/reordering detectable, so a session that
    /// silently reused index 0 would defeat the whole chain.
    #[test]
    fn successive_signs_advance_the_chain_index() {
        let private_key = test_private_key();
        let key_pair = ChatKeyPair {
            private_key,
            public_key_der: vec![],
            key_signature: vec![1],
            expires_at_millis: i64::MAX,
            refreshed_after_millis: i64::MAX,
        };
        let sender = Uuid::from_u128(9);
        let mut session = ChatSession::new(sender, key_pair);

        let (sig1, index1) = session.sign("same text", 1, 1, &[]).unwrap().unwrap();
        let (sig2, index2) = session.sign("same text", 1, 1, &[]).unwrap().unwrap();
        assert_eq!(index1, 0);
        assert_eq!(index2, 1);
        assert_ne!(sig1, sig2, "identical content at different chain positions must sign differently");
    }

    #[test]
    fn due_refresh_compares_against_the_supplied_clock() {
        let key_pair = ChatKeyPair {
            private_key: test_private_key(),
            public_key_der: vec![],
            key_signature: vec![1],
            expires_at_millis: 1000,
            refreshed_after_millis: 1000,
        };
        // `ProfileKeyPair.dueRefresh` is `refreshedAfter.isBefore(now)`, and
        // Java's `Instant.isBefore` is strict (`<`, not `<=`): at the exact
        // boundary `refreshedAfter == now`, neither instant is *before* the
        // other, so refresh is not yet due. This was the wrong expectation
        // the first time this test was written — worth keeping the comment
        // as the reminder that a round number here is a guess, not a given.
        assert!(!key_pair.due_refresh(999), "not yet due");
        assert!(!key_pair.due_refresh(1000), "not due at the exact boundary — isBefore is strict");
        assert!(key_pair.due_refresh(1001), "overdue");
    }

    // --- Response parsing (JSON shape only; no network) ---------------------

    /// A realistic response shape, including the sibling `publicKeySignature`
    /// field the real API also sends (per public documentation) — this must
    /// be ignored in favour of `publicKeySignatureV2`, which is the field
    /// `parse_key_pair_response` reads (see this module's doc comment on
    /// why `publicKeySignatureV2`, not the plain-named sibling, is correct).
    #[test]
    fn parses_a_realistic_response_and_ignores_the_v1_signature_field() {
        let mojang_priv_pem = "-----BEGIN RSA PRIVATE KEY-----\n\
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDtM+q+4UwoW3cZQ8TVkfa9TfGd\n\
xpl13PlfNei77mmWz+kLCxeOXpF2hX/VXSoxj3yBjjhtGHZB59eX0VW2zw+G913ZMmtT+9phKBA9\n\
BOID4c4hNpz852wJ5sp2pFOyrrg47UTrakey9iQT+ckO4qfeMR13NTDP44cLFBwa1/ot80Fwq00x\n\
g5KHJK6WeWmjPayc+lf3FSPC+cNOaOJ3oaWK16b2LFqvzwwkl53e0yyHFgffA5AdClVJgZc7pEDS\n\
cO0zLHLqe8ySrbsJyZ9PQSTNC7cmXkJPQjlYJJ2M4/+HJRtjY/CQyP5C7sTdu/Lhn1nUawhj74Eg\n\
yvg8HXeysPeZAgMBAAECggEBAMg+0ee+jupq/MpJWbvqc2Awks7dP+QuXh8whX9Rr7XvYw89l+9K\n\
ioaCAP8AnYQlW7iLdbszsXHF5U13HWMsvjD0VzfqxoypyxvGFJ9OpfcdA0Uqs7EVNTHOshEifL4V\n\
ndQBCfOrT0gXXzG15zQ3x/tdf0CJmOGHdRO3MFrBBaUPXJgVcGCWyKK9/p+uV9lolnQprotiuctX\n\
6nI5hYAX7PG1XFJlPAW5k9DLE4W31+8YFiJgsS/WTRAsvjs7zJefGwUNE0+86ylREEmSvHWqjS6p\n\
gxf7REZed0208kTHC1P8aGP9nnrHZfiKBDtxt2usRbG00Whf9NVTOZBeC9ExKjkCgYEA948Wr8q0\n\
lFVMZ7xtu5Dx8Mvjvz2Bl5wclX27qrqeu7T3aGnP2EwVSQW5xUB/KpYpxoMFiJIy9cVqo1XTVege\n\
7i8WsGRK+D9xpd6QEhME79nIbltmxTVP9Ue9foBev0S0QM5n1Qk6L4hKUnvadwQ1Ow6XoPejGcu2\n\
BhYzywUPrJsCgYEA9UpuTzZgMg7CVCIRH6Ze8jNP56GADXYB8BH5hSuaKO67PukLa/iqSo38w1uZ\n\
SVLvNgLxts5Q+pinSglJlZ8mRrLVFI8qkcIgj/qZKpVP0mfOuBYu/DNkX0VO4nG1pBSKgT1dmUiV\n\
VAvBfgbUHeG1vVEENKh0NbSHnswL84z8XdsCgYBpnapYJWsVPa7zMvi95QDTcqkfleYMAJZRUOsX\n\
07aU7of/C+WYqh0Kol63QOUADkCUaKGbuoPzRt5QAPXA2N8ZTw2nA6LYdnjOAz4D+AlLKubP7j7S\n\
NASA6LJ3ndzOTUl5vJWf1ef1D3hl6GE0FZ+AKqGWExCKmNZ3klFWdDpTsQKBgFG9FttApHep4WoF\n\
3Czu1O7i2Hq4n6Jcs7KbWsncyMdhHnaNVCgLujuT6ynyiTcc8ufNvVyMjgGkAwMx6xp36Vpf14+9\n\
UZM23ID+IjJFhU75FrLTeZ7DRWxV/T6KY9wkmC8PEvS0ckaKkFT904uNnnFS4RLnG6qV2Se6mTT0\n\
w1hHAoGADIwcasJrU/5xnBPICA6fu43x6dk1/v+GeRLz0N0aVADsj7tInJ+7pHV1/NrHaGONJKIQ\n\
0uWIKxVdHufDmYVUKY0Oh6wzS/m5Z2tmxK24z0UJyXvAu67ETx5QUhqH63i5km9a2Au+zkwGXBBg\n\
6Bvh7kWCpm322pipbRs6hKc7klQ=\n\
-----END RSA PRIVATE KEY-----\n";
        let mojang_pub_pem = "-----BEGIN RSA PUBLIC KEY-----\n\
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA7TPqvuFMKFt3GUPE1ZH2vU3xncaZddz5\n\
XzXou+5pls/pCwsXjl6RdoV/1V0qMY98gY44bRh2QefXl9FVts8Phvdd2TJrU/vaYSgQPQTiA+HO\n\
ITac/OdsCebKdqRTsq64OO1E62pHsvYkE/nJDuKn3jEddzUwz+OHCxQcGtf6LfNBcKtNMYOShySu\n\
lnlpoz2snPpX9xUjwvnDTmjid6Glitem9ixar88MJJed3tMshxYH3wOQHQpVSYGXO6RA0nDtMyxy\n\
6nvMkq27CcmfT0EkzQu3Jl5CT0I5WCSdjOP/hyUbY2PwkMj+Qu7E3bvy4Z9Z1GsIY++BIMr4PB13\n\
srD3mQIDAQAB\n\
-----END RSA PUBLIC KEY-----\n";
        let resp = KeyPairResponse {
            key_pair: KeyPairData {
                private_key: mojang_priv_pem.to_owned(),
                public_key: mojang_pub_pem.to_owned(),
            },
            public_key_signature_v2: "AQID".to_owned(), // decodes to [1, 2, 3]
            expires_at: "2024-01-01T00:00:00Z".to_owned(),
            refreshed_after: "2023-12-31T00:00:00Z".to_owned(),
        };
        let key_pair = parse_key_pair_response(resp).unwrap();
        assert_eq!(key_pair.key_signature, vec![1, 2, 3]);
        assert_eq!(key_pair.expires_at_millis, 1_704_067_200_000);
        // A day before `expires_at` — pairwise-distinct from it, so a
        // transposition of the two fields in the parse would be visible.
        assert_eq!(key_pair.refreshed_after_millis, 1_703_980_800_000);
        assert!(!key_pair.public_key_der.is_empty());
    }

    #[test]
    fn an_empty_signature_is_rejected_rather_than_silently_accepted() {
        let resp = KeyPairResponse {
            key_pair: KeyPairData {
                private_key: "garbage".to_owned(),
                public_key: "garbage".to_owned(),
            },
            public_key_signature_v2: String::new(),
            expires_at: "2024-01-01T00:00:00Z".to_owned(),
            refreshed_after: "2024-01-01T00:00:00Z".to_owned(),
        };
        assert!(parse_key_pair_response(resp).is_err());
    }
}
