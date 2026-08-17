//! Server-side chat-session bookkeeping: the *decode* and *signature
//! verification* halves of secure chat.
//!
//! ## What it is
//!
//! [`ServerChatSession`] is the per-connection record of a client's announced
//! chat-signing session (`chat_session_update` → `RemoteChatSession.Data`),
//! plus the chain position this server tracks against it. [`decide`] is the
//! policy: given an incoming `chat` packet and (if any) the sender's
//! announced session, it decides whether the message should be broadcast,
//! mirroring `SignedMessageChain.Decoder`/`SignedMessageChain.Decoder.unsigned`
//! (`.cache/mc/26.2/src/net/minecraft/network/chat/SignedMessageChain.java`).
//!
//! ## What is deliberately not built here
//!
//! * **The announced public key's Mojang provenance is never checked.**
//!   Vanilla validates `chat_session_update`'s `key_signature` against
//!   Mojang's own Services signing key before adopting a session
//!   (`Services::profileKeySignatureValidator`, fetched from
//!   `https://api.minecraftservices.com/publickeys` at server boot) — this
//!   crate has no such fetch. [`decide`] therefore only proves a message was
//!   signed by *whoever's* private key matches the *announced* public key; it
//!   cannot prove that key was ever issued to the account it claims. A
//!   verified message here is evidence against tampering in transit or reuse
//!   across sessions, not evidence of the sender's real identity — narrower
//!   than what `enforce-secure-profile=true` promises on a real vanilla
//!   server. See `docs/player-chat.md` for the consequence this has for that
//!   setting's default here.
//! * **No real `player_chat` relay.** A verified message is still broadcast
//!   as an unsigned `system_chat`, exactly like every other message (see
//!   `docs/player-chat.md`'s "signing decision" section) — `verified` is
//!   computed and then, today, only used to decide accept-vs-reject, not
//!   carried to other clients. A peer cannot independently verify anything
//!   about a message it receives from this server, regardless of whether the
//!   original sender signed it.
//! * **No `chat_ack`/last-seen bookkeeping.** Both are consequences of the
//!   point above: since this server never sends a signed `player_chat`,
//!   nothing built with `LastSeenMessages` from other players ever appears
//!   on the wire, so a real client's own outgoing last-seen window stays
//!   permanently empty (`offset=0`, an all-zero bit set, checksum `0` —
//!   vanilla's own `LastSeenMessages.Update.EMPTY`/`IGNORE_CHECKSUM`
//!   shape). [`decide`] relies on exactly this: it always verifies against an
//!   empty `last_seen` list rather than reconstructing one from a signature
//!   cache this crate does not keep.
//! * **Out-of-order timestamp rejection and the `MISSING_PROFILE_KEY` case
//!   for a signature with no announced session** are folded into the same
//!   "reject, do not disconnect" path rather than modelled as separate
//!   vanilla `DecodeException` variants — see [`ChatDecision`]'s own doc.
//!
//! ## How to change it
//!
//! Adding Mojang provenance checking means fetching and caching
//! `https://api.minecraftservices.com/publickeys` (native-only, like every
//! other `lodestone-auth` network call) and verifying `key_signature` against
//! one of the returned keys before constructing a [`ServerChatSession`] at
//! all — see [`ServerChatSession::new`]'s call site in
//! `crate::server`'s `ServerBound::ChatSessionAnnounced` arm.
//!
//! Building the real relay (`encode_player_chat`, a `MessageSignatureCache`
//! consumer, and real `chat_ack` bookkeeping) is out of scope here; see
//! `docs/player-chat.md`.
//!
//! ## Configuration
//!
//! `server.properties`' `enforce-secure-profile` — [`crate::properties`] parses
//! it, `crate::PlayerRegistry::set_enforce_secure_profile` applies it, and
//! [`decide`]'s `enforce_secure_profile` parameter is what actually consults
//! it.
//!
//! ## Dependencies
//!
//! `lodestone_auth::{SignedMessageLink, verify_signature}` (native-only,
//! matching every other RSA-touching path in this crate — see
//! `Cargo.toml`'s comment on the `lodestone-auth` dependency). On `wasm32`
//! [`decide`] still compiles and still tracks chain state, but never reports
//! a message as verified, matching this crate's existing "wasm32 receives
//! degraded chat" gap (`docs/player-chat.md`'s "Chat on wasm32" section).

use uuid::Uuid;

/// One connection's announced chat-signing session
/// (`ServerboundChatSessionUpdatePacket` → `RemoteChatSession.Data`), plus the
/// verification chain position this server tracks against it.
///
/// Mirrors `SignedMessageChain`'s `nextLink` cursor, narrowed to what a
/// verifier (rather than an encoder) needs: this struct only ever checks a
/// signature against `next_index`, it never produces one.
#[derive(Debug, Clone)]
pub struct ServerChatSession {
    session_id: Uuid,
    expires_at_millis: i64,
    public_key_der: Vec<u8>,
    /// Mojang's signature over `public_key_der` (`publicKeySignatureV2`),
    /// carried but never checked — see this module's own doc for why.
    #[allow(dead_code)]
    key_signature: Vec<u8>,
    /// Next expected `SignedMessageLink.index`. `None` once the chain is
    /// broken (an invalid or out-of-order signature), mirroring
    /// `SignedMessageChain.Decoder::setChainBroken` — a broken chain stays
    /// broken until a fresh `chat_session_update` replaces this session
    /// wholesale, exactly like vanilla's `resetPlayerChatState` swapping the
    /// whole `signedMessageDecoder` rather than repairing one.
    next_index: Option<i32>,
}

impl ServerChatSession {
    /// A freshly announced session, chain rooted at index 0
    /// (`SignedMessageLink.root`).
    #[must_use]
    pub fn new(session_id: Uuid, expires_at_millis: i64, public_key_der: Vec<u8>, key_signature: Vec<u8>) -> Self {
        Self {
            session_id,
            expires_at_millis,
            public_key_der,
            key_signature,
            next_index: Some(0),
        }
    }

    #[must_use]
    fn is_expired(&self, now_millis: i64) -> bool {
        self.expires_at_millis <= now_millis
    }
}

/// The outcome of folding one incoming `chat` packet through the sender's
/// announced session (or lack of one). Narrower than vanilla's
/// `SignedMessageChain.DecodeException`'s five named cases — everything that
/// is not a clean accept collapses into [`Reject`](Self::Reject) with a
/// human-readable reason, since this crate's own `encode_system_chat` takes
/// plain text rather than a translation key (see `ServerBound::Chat`'s own
/// doc for why an unverifiable signature must never be silently upgraded to
/// "sent").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatDecision {
    /// Broadcast it. `verified` is `true` only when a real signature checked
    /// out against the sender's announced key — see this module's own doc
    /// for what that does and does not prove, and for why it is not (yet)
    /// carried any further than this decision.
    Accept {
        /// Whether the signature actually verified (`false` for unsigned
        /// chat accepted because enforcement is off).
        verified: bool,
    },
    /// Drop the message — do not broadcast it. Mirrors vanilla's
    /// `handleMessageDecodeFailure`: logged and reported back to the sender
    /// alone, never a disconnect (that is `CHAT_VALIDATION_FAILED`'s job, a
    /// different failure this crate does not reach — see
    /// `docs/secure-chat.md`'s "chat-validation kick" for why that one is
    /// acknowledgement bookkeeping, not a signature problem).
    Reject {
        /// A short, human-readable reason — not one of vanilla's
        /// `chat.disabled.*` translation keys, since nothing in this crate's
        /// serverbound-reply path resolves those.
        reason: &'static str,
    },
}

/// Decides what to do with one incoming `chat` packet, given (and possibly
/// updating) the sender's announced session.
///
/// Mirrors `ServerGamePacketListenerImpl.handleChat` →
/// `SignedMessageChain.Decoder.unpack`, narrowed as this module's own doc
/// describes. The four vanilla branches this reproduces, in the same order
/// vanilla checks them:
///
/// 1. No session announced yet: `SignedMessageChain.Decoder.unsigned` —
///    accept unsigned when `enforce_secure_profile` is off, otherwise reject
///    regardless of whether a signature happens to be present (there is
///    nothing to check it against).
/// 2. A session is announced but this message carries no signature: always
///    rejected (`MISSING_PROFILE_KEY`) — once a client has told the server it
///    can sign, an unsigned message from it is never accepted, independent of
///    `enforce_secure_profile`.
/// 3. The announced key has expired: rejected (`EXPIRED_PROFILE_KEY`).
/// 4. A signed message against a live session: verified against the
///    reconstructed [`lodestone_auth::SignedMessageLink`] with an always-empty
///    last-seen list (see this module's own doc for why that is sound here),
///    and the chain position advances on success or breaks on failure —
///    `INVALID_SIGNATURE`.
#[must_use]
pub fn decide(
    session: &mut Option<ServerChatSession>,
    sender: Uuid,
    enforce_secure_profile: bool,
    signature: Option<&[u8]>,
    content: &str,
    timestamp_millis: i64,
    salt: i64,
    now_millis: i64,
) -> ChatDecision {
    let Some(session) = session.as_mut() else {
        return if enforce_secure_profile {
            ChatDecision::Reject {
                reason: "no chat session announced, and this server requires one",
            }
        } else {
            ChatDecision::Accept { verified: false }
        };
    };
    let Some(signature) = signature else {
        return ChatDecision::Reject {
            reason: "a chat session is announced, but this message carries no signature",
        };
    };
    if session.is_expired(now_millis) {
        return ChatDecision::Reject {
            reason: "the announced chat session's key has expired",
        };
    }
    let Some(index) = session.next_index else {
        return ChatDecision::Reject {
            reason: "this chat session's signature chain is broken; rejoin to start a new one",
        };
    };
    let verified = verify(session, sender, index, signature, content, timestamp_millis, salt);
    if !verified {
        session.next_index = None;
        return ChatDecision::Reject {
            reason: "signature verification failed",
        };
    }
    session.next_index = index.checked_add(1);
    ChatDecision::Accept { verified: true }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn verify(
    session: &ServerChatSession,
    sender: Uuid,
    index: i32,
    signature: &[u8],
    content: &str,
    timestamp_millis: i64,
    salt: i64,
) -> bool {
    let Ok(signature) = <[u8; lodestone_auth::SIGNATURE_BYTES]>::try_from(signature) else {
        return false;
    };
    let link = lodestone_auth::SignedMessageLink {
        index,
        sender,
        session_id: session.session_id,
    };
    // The signed payload is built over epoch **seconds**
    // (`SignedMessageBody.updateSignature`); the wire packet's own timestamp
    // is epoch milliseconds — see `lodestone_auth::build_signature_payload`'s
    // doc for this exact hazard, already caught once on the client side.
    let timestamp_seconds = timestamp_millis / 1000;
    lodestone_auth::verify_signature(
        &session.public_key_der,
        &link,
        content,
        timestamp_seconds,
        salt,
        &[], // always-empty last-seen chain — see this module's own doc.
        &signature,
    )
    .unwrap_or(false)
}

/// No `lodestone-auth` on this target (native-only dependency — see this
/// module's own doc), so no signature can ever check out here. The chain
/// position bookkeeping in [`decide`] still runs identically; only the
/// cryptographic primitive is unavailable.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
fn verify(
    _session: &ServerChatSession,
    _sender: Uuid,
    _index: i32,
    _signature: &[u8],
    _content: &str,
    _timestamp_millis: i64,
    _salt: i64,
) -> bool {
    false
}

/// Current wall-clock time in epoch milliseconds, via this crate's existing
/// portable clock (`web_time`, already a dependency — see this crate's own
/// `Cargo.toml` comment on why `std::time::SystemTime::now()` is banned
/// crate-wide: it compiles and then traps on `wasm32`).
#[must_use]
pub fn now_millis() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    use super::*;

    /// The same fixed PKCS#8-DER RSA-2048 test key `lodestone-auth`'s own
    /// `chat_session` tests and `lodestone-client`'s `driver.rs` tests use
    /// (`openssl genpkey`-generated, no code or secrecy shared with anything
    /// real) — reused rather than generating a fresh key per test run, which
    /// would need `rsa`'s `rand_core` line wired through for no benefit here.
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

    /// A throwaway signer built from the fixed test key, mirroring
    /// `lodestone-client`'s own `test_chat_session` helper so this crate's
    /// verifier is exercised against the exact same signing code path a real
    /// client's `ChatSession` uses.
    fn signer(sender: Uuid) -> (lodestone_auth::ChatSession, Vec<u8>, Uuid) {
        let der = BASE64.decode(TEST_PRIVATE_KEY_DER_B64).expect("valid base64 fixture");
        let private_key = RsaPrivateKey::from_pkcs8_der(&der).expect("valid PKCS#8 DER fixture");
        let public_key = RsaPublicKey::from(&private_key);
        let public_key_der = public_key.to_public_key_der().expect("encode test public key").into_vec();
        let key_pair = lodestone_auth::ChatKeyPair::for_tests(
            private_key,
            public_key_der.clone(),
            vec![0xAA, 0xBB], // key_signature: opaque here, never checked — see module doc.
            i64::MAX,
            i64::MAX,
        );
        let session = lodestone_auth::ChatSession::new(sender, key_pair);
        let session_id = session.session_id();
        (session, public_key_der, session_id)
    }

    fn sign(
        signer: &mut lodestone_auth::ChatSession,
        content: &str,
        timestamp_millis: i64,
        salt: i64,
    ) -> Vec<u8> {
        let (signature, _index) = signer
            .sign(content, timestamp_millis / 1000, salt, &[])
            .unwrap()
            .expect("chain not exhausted");
        signature.to_vec()
    }

    #[test]
    fn a_real_signature_against_the_announced_key_verifies_and_broadcasts() {
        let sender = Uuid::from_u128(11);
        let (mut signer_session, public_key_der, session_id) = signer(sender);
        let mut session = Some(ServerChatSession::new(session_id, i64::MAX, public_key_der, vec![9, 9]));
        let signature = sign(&mut signer_session, "hello, lodestone", 1_700_000_000_000, 42);
        let decision = decide(
            &mut session,
            sender,
            true, // enforcement on: a verified signature must still pass
            Some(&signature),
            "hello, lodestone",
            1_700_000_000_000,
            42,
            0,
        );
        assert_eq!(decision, ChatDecision::Accept { verified: true });
    }

    /// The pairwise-distinct-fixture discipline this repo's evidence standard
    /// asks for: content, timestamp and salt are each individually wrong here
    /// relative to `signer`'s own fixture, not just "some field differs".
    #[test]
    fn a_forged_signature_is_rejected_and_breaks_the_chain() {
        let sender = Uuid::from_u128(22);
        let (mut signer_session, public_key_der, session_id) = signer(sender);
        let mut session = Some(ServerChatSession::new(session_id, i64::MAX, public_key_der, vec![]));
        // Sign one message for real, then present a *different* message under
        // that same (valid-shaped) signature — the forged-content case, not a
        // malformed-bytes case.
        let real_signature = sign(&mut signer_session, "real content", 1_700_000_001_000, 7);
        let decision = decide(
            &mut session,
            sender,
            false,
            Some(&real_signature),
            "forged content",
            1_700_000_001_000,
            7,
            0,
        );
        assert_eq!(decision, ChatDecision::Reject { reason: "signature verification failed" });
        // And the chain is now broken: even a *correctly* signed follow-up
        // message must not be accepted until a fresh session is announced.
        let good_signature = sign(&mut signer_session, "next message", 1_700_000_002_000, 8);
        let decision = decide(
            &mut session,
            sender,
            false,
            Some(&good_signature),
            "next message",
            1_700_000_002_000,
            8,
            0,
        );
        assert_eq!(
            decision,
            ChatDecision::Reject {
                reason: "this chat session's signature chain is broken; rejoin to start a new one"
            }
        );
    }

    #[test]
    fn unsigned_chat_is_accepted_unverified_when_enforcement_is_off() {
        let mut session = None;
        let decision = decide(&mut session, Uuid::from_u128(1), false, None, "hi", 0, 0, 0);
        assert_eq!(decision, ChatDecision::Accept { verified: false });
    }

    #[test]
    fn unsigned_chat_is_rejected_when_enforcement_is_on() {
        let mut session = None;
        let decision = decide(&mut session, Uuid::from_u128(1), true, None, "hi", 0, 0, 0);
        assert!(matches!(decision, ChatDecision::Reject { .. }));
    }

    /// Once a session is announced, an unsigned message is rejected
    /// regardless of `enforce_secure_profile` — the client told the server it
    /// can sign, so silently falling back to unsigned would let a compromised
    /// client mix signed and unsigned traffic at will.
    #[test]
    fn an_announced_session_rejects_an_unsigned_message_even_with_enforcement_off() {
        let mut session = Some(ServerChatSession::new(Uuid::from_u128(2), i64::MAX, vec![1, 2, 3], vec![]));
        let decision = decide(&mut session, Uuid::from_u128(1), false, None, "hi", 0, 0, 0);
        assert!(matches!(decision, ChatDecision::Reject { .. }));
    }

    #[test]
    fn an_expired_session_is_rejected() {
        let mut session = Some(ServerChatSession::new(Uuid::from_u128(3), 1_000, vec![1, 2, 3], vec![]));
        let decision = decide(
            &mut session,
            Uuid::from_u128(1),
            false,
            Some(&[0u8; 256]),
            "hi",
            0,
            0,
            2_000, // now_millis past expires_at_millis
        );
        assert_eq!(
            decision,
            ChatDecision::Reject {
                reason: "the announced chat session's key has expired"
            }
        );
    }

    #[test]
    fn now_millis_is_a_real_epoch_timestamp() {
        // A sanity floor, not a real oracle: any wall clock in 2024 or later
        // reads well past this. Catches a `0`/panicked fallback silently
        // taking over, not the exact value.
        assert!(now_millis() > 1_700_000_000_000);
    }
}
