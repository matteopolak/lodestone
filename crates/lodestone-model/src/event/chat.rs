//! Chat, signing, and player identity payloads.

/// The semantic kind of an incoming chat component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatKind {
    /// Player or signed chat message.
    Chat,
    /// System message.
    System,
    /// Game information, such as action-bar text.
    GameInfo,
}

/// A signed player-chat acknowledgement input.
///
/// Only signed player chat carries this. System chat, disguised chat, and older
/// protocols should use `None` on [`ClientEvent::Chat`]. A filtered message still
/// carries `Some(Self { was_shown: false, .. })`: it advances the vanilla
/// last-seen window and burns an acknowledgement offset even though it did not
/// render to the user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatAckInfo {
    /// Raw message signature bytes. Empty means this message carried no
    /// signature at all —
    /// the common case on a server with signed chat disabled, and by itself
    /// enough to treat the message as unverified.
    pub signature: Vec<u8>,
    /// Server-global signed-chat index.
    pub global_index: i32,
    /// Whether the message was shown to the user after filtering.
    pub was_shown: bool,
    /// This message's position in the sender's signing chain, the wire's
    /// `index` field on `PLAYER_CHAT`.
    /// Needed, together with the sender's announced chat-session id
    /// ([`PlayerListEntry::chat_session`]), to reconstruct the exact
    /// signing-chain link `lodestone_auth::verify_signature` hashes —
    /// verification cannot be attempted without it.
    pub message_index: i32,
    /// The signed body's own timestamp, epoch **milliseconds** — the wire
    /// unit. The signature payload
    /// itself is built over epoch **seconds**
    /// (`lodestone_auth::chat_session::build_signature_payload`'s
    /// `timestamp_epoch_seconds` parameter); converting is the verifier's
    /// job, not this struct's — carrying the wire unit verbatim is what
    /// keeps that conversion a single, visible `/ 1000` at the one call site
    /// that needs it, rather than an implicit unit change baked into a field
    /// name.
    pub timestamp_millis: i64,
    /// The signed body's random salt.
    pub salt: i64,
    /// The raw signed message content, verbatim — **not** [`ClientEvent::Chat`]'s
    /// own `text`, which may be the server's *decorated* form
    /// (`unsigned_content`) instead. Verification must hash exactly what the
    /// sender signed, so this is kept alongside the decorated text rather
    /// than reconstructed from it.
    pub raw_content: String,
    /// The resolved last-seen signature chain this message was built over,
    /// already resolved against the
    /// connection's signature cache — see `read_last_seen_packed`), each
    /// entry 256 raw signature bytes.
    pub last_seen: Vec<Vec<u8>>,
    /// Whether this message's signature was checked against the sender's
    /// announced public key and found valid.
    ///
    /// **Populated by the client driver, not by the wire decoder** — the
    /// adapter that builds this struct has no access to the per-player
    /// public-key store, only the driver's read-model does (see
    /// `lodestone_client::driver`'s `emit` handling of `ClientEvent::Chat`).
    /// Every adapter constructs this `false` (fail-closed: unverified until
    /// proven otherwise, never trusted by default), and the driver may raise
    /// it to `true` after a successful `lodestone_auth::verify_signature`
    /// call. An empty `signature` (no signature at all) is left `false` and
    /// is never attempted — the same rule the real client's own trust
    /// evaluation applies for an unsigned message.
    pub verified: bool,
}

/// A packed message signature.
///
/// The wire form is either a full 256-byte signature (for a signature the
/// client has not cached yet) or an index into the last-seen signature
/// cache. The v26-2 adapter resolves `Cached` references against its
/// per-connection signature cache before emitting — dropping the event on a
/// miss — so a `ChatMessageDeleted` normally carries a `Full` signature; the
/// `Cached` variant remains for adapters that pass the wire form through
/// unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PackedMessageSignature {
    /// A full 256-byte signature.
    Full(Vec<u8>),
    /// An index into the last-seen signature cache.
    Cached(i32),
}

/// The anchor point used by the player look at packet's
/// `EntityAnchorArgument.Anchor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LookAnchor {
    /// Anchor at the entity's feet.
    Feet,
    /// Anchor at the entity's eyes.
    Eyes,
}

/// A player's announced chat-signing session (`RemoteChatSession.Data`):
/// their session UUID and Mojang-issued public key, as broadcast by
/// `INITIALIZE_CHAT` and carried per-entry on [`PlayerListEntry`].
///
/// `key_signature` (Mojang's own signature over `public_key`) is
/// deliberately not carried this far — nothing downstream re-verifies it
/// against Mojang's key; only the *server* does, per
/// `crates/versions/26.2/src/packets/player_info.rs`'s
/// `RemoteChatSessionData` doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSessionInfo {
    /// This player's chat-session UUID — half of the `SignedMessageLink`
    /// every one of their signed messages is hashed against.
    pub session_id: uuid::Uuid,
    /// DER-encoded (X.509 `SubjectPublicKeyInfo`) RSA public key, verbatim —
    /// what `lodestone_auth::verify_signature` parses.
    pub public_key: Vec<u8>,
    /// Public-key expiry, epoch milliseconds.
    /// Not enforced by anything here yet — the real client's own chat-trust
    /// evaluation checks this same expiry against the wall clock, which is
    /// the check this field would feed.
    pub expires_at: i64,
}

/// One entry of a player profile's property multimap, as `ADD_PLAYER` carries it.
///
/// The one that matters is `minecraft:textures`, whose `value` is base64 of a JSON
/// blob holding the skin URL and its model declaration. **Two traps live in that
/// blob rather than here**, both recorded because they cost time:
///
/// * the wide player model is spelled **`default`**, not `wide`. Reading it as
///   `wide` resolves *every* skin as wide, including slim ones, and the only
///   symptom is slightly-too-thick arms — no error and no blank texture.
/// * the payload's shape is **not** in the decompiled client; it lives in the
///   authlib jar's constant pool.
///
/// Nothing here parses or validates the value: it is server-supplied, and on an
/// online-mode server it is Mojang-signed, which is what [`Self::signature`] is
/// for. A consumer that trusts the URL should check the signature first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProperty {
    /// Property name, e.g. `textures`.
    pub name: String,
    /// Property value. Base64 for `textures`.
    pub value: String,
    /// Mojang's signature over the value, present only in online mode.
    pub signature: Option<String>,
}
