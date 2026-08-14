# Secure chat: session keys and per-message signatures

## What it is

The client-side half of vanilla's secure chat: fetching the Mojang-issued RSA
chat-signing key pair ([`lodestone_auth::fetch_key_pair`]) and signing
outgoing chat messages with it ([`lodestone_auth::ChatSession`]), plus the
wire shapes needed to carry a session announcement and a signed message
( [`ChatSessionUpdate`], [`ChatCommandSigned`] in
`crates/protocol/v770/src/packets/game.rs`) and to keep (rather than discard)
another player's announced session ( [`RemoteChatSessionData`] in
`crates/protocol/v770/src/packets/player_info.rs`).

Before this, none of it existed: every outgoing chat message went out via
`ChatMessage::unsigned`, which hard-codes `salt: 0, signature: None`, and
`INITIALIZE_CHAT` was parsed only far enough to stay byte-aligned and then
thrown away. A server with `enforce-secure-profile=true` silently drops every
message this client sends.

**What landed and what did not**, in the terms `CLAUDE.md`'s "nothing is done
until something on screen changes" rule asks for:

| link in the chain | status |
|---|---|
| session key fetched from Mojang | done — [`lodestone_auth::fetch_key_pair`] |
| message signed with the right byte layout | done — [`lodestone_auth::ChatSession::sign`], independently checked (see below) |
| signature carried in the right wire field order | done for `chat_session_update`/`chat_command_signed`'s *shapes*; `ChatMessage`'s shape was already correct |
| a live connection actually calling any of the above | **not done** — no `ClientAction` producer exists anywhere in the tree that supplies signing material, so `ChatSessionUpdate`/`ChatCommandSigned` currently have **zero encoders that ever run** and `ChatMessage` is still always sent via `ChatMessage::unsigned`. This is the island this doc's own future editor should close first — see "How to change it" |
| a real server accepting a signed message | **unverifiable from this repo** — no test here reaches a real session server or a real game server |

## How it works

### Session key acquisition — `lodestone_auth::chat_session`

[`fetch_key_pair`] `POST`s `https://api.minecraftservices.com/player/certificates`
with the Minecraft-services access token ([`flow::Session::access_token`],
the same token [`flow::join_server`] uses) as a bearer credential, and parses
the response into a [`ChatKeyPair`]:

- the private and public key strings are PEM-*shaped* but not standard PEM —
  Mojang wraps a PKCS#8-encoded private key in an `RSA PRIVATE KEY` header
  (PKCS#1's header text, PKCS#8's actual bytes) and an X.509
  SubjectPublicKeyInfo-encoded public key in an `RSA PUBLIC KEY` header. Both
  quirks are handled directly rather than assumed away.
- the response's signature field the client actually needs is
  `publicKeySignatureV2`, not the plain-named sibling `publicKeySignature` the
  API also returns — confirmed from `authlib-9.0.75.jar`'s
  `KeyPairResponse`/`KeyPairResponse$KeyPair` record definitions (there is no
  decompiled source for authlib in `.cache/mc/26.2`, so this was read directly
  out of the jar's constant pool and `@SerializedName` annotation rather than
  guessed).
- `expiresAt`/`refreshedAfter` are Java `Instant.toString()`-shaped strings,
  parsed with a small hand-rolled ISO-8601 parser (`parse_iso8601_millis`) —
  no `chrono`/`time` dependency was added for two fields.

[`ChatKeyPair::due_refresh`] mirrors `ProfileKeyPair.dueRefresh` —
`refreshedAfter.isBefore(now)` — and is the caller's signal to call
[`fetch_key_pair`] again.

### Per-message signing — `lodestone_auth::ChatSession`

[`build_signature_payload`] is the exact byte layout
`PlayerChatMessage.updateSignature` signs, hand-expanded clause by clause from
the decompiled 26.2 source (see the function's own doc comment for the
citation of each clause). The one detail most likely to get silently
transposed in a port: the signed payload's timestamp is **epoch seconds**
(`SignedMessageBody.updateSignature`'s `timeStamp.getEpochSecond()`), while
the *wire* packet's timestamp field (`ChatMessage.timestamp`,
`ServerboundChatPacket`) is epoch **milliseconds**. Both are `i64`, so this is
exactly the kind of adjacent-same-typed-field mistake that survives a
round-trip through your own code — see "What was and wasn't verified" below
for how this was checked.

[`ChatSession`] holds the signing state a real connection needs: the
client-generated session UUID, the [`ChatKeyPair`], and a `next_link` cursor
(`SignedMessageLink`) that advances on every [`ChatSession::sign`] call,
mirroring `SignedMessageChain.Encoder`. It returns `None` once the chain hits
`i32::MAX` (vanilla's own signal to start a new session).

[`verify_signature`] is the decode-side primitive (`SignatureValidator.from`
+ `MessageSignature.verify`) — it checks one signature against one public key
and payload. It does **not** implement the rest of `SignedMessageChain.Decoder`
(link-ordering enforcement, expiry, or a per-sender chain state machine), and
nothing calls it yet; see "What isn't built" below.

### Wire shapes — `crates/protocol/v770/src/packets/game.rs` and `player_info.rs`

- [`ChatSessionUpdate`] (serverbound `chat_session_update`) and
  [`ChatCommandSigned`]/[`ArgumentSignatureEntry`] (serverbound
  `chat_command_signed`) are new packet structs, wire-layout-correct against
  `ServerboundChatSessionUpdatePacket`/`RemoteChatSession.Data`/
  `ProfilePublicKey.Data` and `ServerboundChatCommandSignedPacket`/
  `ArgumentSignatures` respectively. `ChatMessage` (serverbound `chat`)
  already modelled the correct wire shape before this work — the gap was
  entirely in what filled it, not its layout.
- `player_info.rs`'s `PlayerInfoUpdate` decode used to skip `INITIALIZE_CHAT`
  (`skip_chat_session`/`skip_byte_array`) purely to stay byte-aligned. It now
  keeps the session as [`RemoteChatSessionData`] on
  [`PlayerInfoEntry::chat_session`] — the public key another player's
  messages would need to be verified against, once something calls
  [`verify_signature`] with it.

## How to change it

**The most important next step is wiring, not more crypto.** Everything above
is a working, tested library with **no caller**:

1. `ClientAction::SendChat`/`SendCommand` (`crates/lodestone-model/src/action.rs`)
   carry only `text`/`command` today — no timestamp, salt, signature, or
   last-seen data can reach the adapter through them. Extending these
   variants (or adding new ones, e.g. `ClientAction::AnnounceChatSession`) is
   the first change; it is in `lodestone-model`, a crate this work did not
   touch.
2. `crates/protocol/v770/src/adapter/serverbound.rs`'s
   `ClientAction::SendChat` arm currently always builds
   `ChatMessage::unsigned(text.clone())`. Once the action carries signing
   material, this arm (and new arms for `ChatSessionUpdate`/
   `ChatCommandSigned`) can encode it using the types this doc describes.
3. Something needs to *own* a live [`ChatSession`] per connection, know when
   to call [`fetch_key_pair`] (on join, and again whenever
   [`ChatKeyPair::due_refresh`] says so), and feed [`ChatSession::sign`] the
   last-seen acknowledgement state [`lodestone_game::chat_ack`]'s
   `LastSeenTracker` already maintains. That is a `lodestone-client`/
   `lodestone-shell`-side piece of state, not something this crate can hold —
   `lodestone-auth` has no connection lifecycle to hook into.
4. On the receive side: nothing today calls [`verify_signature`]. Building a
   "verified" badge needs the rest of `SignedMessageChain.Decoder` (ordering,
   expiry, a per-sender chain) — a distinct, later feature per this area's own
   history.

**Gotcha — the signature payload's timestamp is seconds, everything else on
the wire is milliseconds.** If you touch [`build_signature_payload`], keep
`timestamp_epoch_seconds` named that way; a caller passing the same
millisecond value it puts on the wire will produce a signature that verifies
against nothing.

**Gotcha — `rsa` in this crate is native-only for the same reason it already
was in `lodestone-net`**: `rsa` 0.9 pins `rand_core` 0.6/`getrandom` 0.2, a
different major than the rest of the tree, and chat signing needs the
native-only key fetch anyway. See `Cargo.toml`'s comment before changing this.

**Gotcha — `sha2`'s `oid` feature is required** for
`Pkcs1v15Sign::new::<Sha256>()`'s `AssociatedOid` bound; it is not part of
`sha2`'s default features and is enabled explicitly in this crate's
`Cargo.toml`.

## Configuration

No new environment variables or flags. Key acquisition reuses the same
Minecraft-services access token online-mode join already resolves via
[`lodestone_auth::login`]/[`lodestone_auth::flow`] — see `docs/accounts.md`.

## Dependencies

- `rsa` 0.9 (native-only) — RSA-2048 key parsing (PKCS#8 private,
  X.509 SPKI public) and `SHA256withRSA`/PKCS#1 v1.5 signing and
  verification. Already a dependency of `lodestone-net` for the encryption
  handshake; now also a direct dependency of `lodestone-auth` for the same
  reason.
- `sha2` (native-only, `oid` feature) — the digest `Pkcs1v15Sign` signs over.
- `base64`, `serde`/`serde_json` — already crate dependencies, reused for PEM
  body decoding and the `/player/certificates` response shape.

## What was and wasn't verified

**Outside source used**: no captured real signed-chat packet and no
published test vector exist for this Mojang-specific scheme, so the payload
layout was hand-expanded from the decompiled 26.2 source, clause by clause
(see [`build_signature_payload`]'s doc comment). The strongest check that
*was* possible: an RSA-2048 key pair generated with `openssl genpkey`
(outside this repo's toolchain), and the *exact same payload bytes* signed
independently with Python's `cryptography` library (OpenSSL-backed — no code
or authorship shared with this crate or with the `rsa`/RustCrypto crate) —
see `chat_session::tests::sign_matches_an_independently_generated_oracle`.
That test pins both the RSA-SHA256-PKCS1v15 primitive and the exact payload
byte layout against a genuinely independent implementation.

`lodestone_auth::server_hash` remains the standard this crate holds itself
to (Mojang's three published vectors); this is the closest equivalent that
was achievable for a scheme with no published vectors of its own.

**Not verified, and not claimable from this repo**: whether a real vanilla
server accepts a message signed this way. That needs a live join this repo's
tests never make — the same limitation every other network-touching part of
`lodestone-auth` already documents (see `docs/accounts.md`'s "What's verified
and what isn't"). Nothing here has been checked against a live server, and as
of this writing nothing in the tree even sends a signed message — see "How to
change it" above.

[`lodestone_auth::fetch_key_pair`]: ../crates/lodestone-auth/src/chat_session.rs
[`fetch_key_pair`]: ../crates/lodestone-auth/src/chat_session.rs
[`lodestone_auth::ChatSession`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatSession`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatSession::sign`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatKeyPair`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatKeyPair::due_refresh`]: ../crates/lodestone-auth/src/chat_session.rs
[`build_signature_payload`]: ../crates/lodestone-auth/src/chat_session.rs
[`verify_signature`]: ../crates/lodestone-auth/src/chat_session.rs
[`flow::Session::access_token`]: ../crates/lodestone-auth/src/flow.rs
[`flow::join_server`]: ../crates/lodestone-auth/src/flow.rs
[`lodestone_auth::login`]: ../crates/lodestone-auth/src/login.rs
[`lodestone_auth::flow`]: ../crates/lodestone-auth/src/flow.rs
[`lodestone_auth::server_hash`]: ../crates/lodestone-auth/src/hash.rs
[`ChatSessionUpdate`]: ../crates/protocol/v770/src/packets/game.rs
[`ChatCommandSigned`]: ../crates/protocol/v770/src/packets/game.rs
[`ArgumentSignatureEntry`]: ../crates/protocol/v770/src/packets/game.rs
[`RemoteChatSessionData`]: ../crates/protocol/v770/src/packets/player_info.rs
[`PlayerInfoEntry::chat_session`]: ../crates/protocol/v770/src/packets/player_info.rs
[`lodestone_game::chat_ack`]: ../crates/lodestone-game/src/chat_ack.rs
