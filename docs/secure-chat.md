# Secure chat: session keys and per-message signatures

## What it is

The client-side half of vanilla's secure chat: fetching the Mojang-issued RSA
chat-signing key pair ([`lodestone_auth::fetch_key_pair`]) and signing
outgoing chat messages with it ([`lodestone_auth::ChatSession`]), plus the
wire shapes needed to carry a session announcement and a signed message
( [`ChatSessionUpdate`], [`ChatCommandSigned`] in
`crates/protocol/v770/src/packets/game.rs`), **and** the receiving half:
keeping another player's announced session
( [`RemoteChatSessionData`] in `crates/protocol/v770/src/packets/player_info.rs`,
carried through to [`lodestone_model::event::ChatSessionInfo`] and
[`lodestone_game::tablist::RemoteChatSession`]) and verifying their signed
messages against it ([`Driver`]'s `emit`, via [`verify_signature`]).

Before this, none of it existed: every outgoing chat message went out via
`ChatMessage::unsigned`, which hard-codes `salt: 0, signature: None`, and
`INITIALIZE_CHAT` was parsed only far enough to stay byte-aligned and then
thrown away. A server with `enforce-secure-profile=true` silently drops every
message this client sends.

**What landed and what did not**, in the terms `CLAUDE.md`'s "nothing is done
until something on screen changes" rule asks for. The chain is now traced
end to end; every link up to the wire is verified locally, and the last one
is explicitly not:

| link in the chain | status |
|---|---|
| account signed in | pre-existing — the shell already resolves an authenticated [`lodestone_auth::Session`] and threads it into [`lodestone_client::ClientBuilder::online_session`] before every join |
| session key acquired | done — [`Driver`] calls [`lodestone_auth::fetch_key_pair`] on [`ClientEvent::Login`] when `auth_session` is `Some`, and builds a [`lodestone_auth::ChatSession`] from the result |
| session announced to the server | done — the same `Login` handler emits [`ClientAction::AnnounceChatSession`], which v770's adapter encodes as `chat_session_update` |
| message signed over the real last-seen chain | done — [`Driver::maybe_sign_chat`]/[`sign_chat_action`] turns an outgoing [`ClientAction::SendChat`] into [`ClientAction::SendSignedChat`], signing over [`lodestone_game::chat_ack::LastSeenTracker`]'s *current* window (`generate_and_apply_update`), not a stale or cached one |
| signature on the wire in the right field order | done — v770's adapter encodes `SendSignedChat` into `ChatMessage` with a real signature and non-zero timestamp/salt/ack fields; `crates/protocol/v770/tests/chat_dispatch.rs`'s `send_signed_chat_encodes_millis_timestamp_and_ack_fields_in_order` asserts the exact byte layout, and `announce_chat_session_encodes_uuid_millis_key_then_signature` does the same for `chat_session_update` |
| offline play still sends unsigned | done — `chat_session` stays `None` without an `auth_session` (or if the key fetch fails), and `maybe_sign_chat` passes `SendChat` through unchanged in that case; every pre-existing `SendChat` test in `lodestone-client`'s `tests/driver.rs` exercises exactly that path and stayed green |
| a real server accepting a signed message | **still unverified** — reaching it needs an *online-mode* server and a real Mojang-issued key, and the local oracle runs offline. See "The chat-validation kick" below for what a real server *did* tell us. |

## The chat-validation kick, and why signing is off by default

**Signing is gated behind `LODESTONE_SECURE_CHAT` and defaults to off**
([`SECURE_CHAT_ENV`] in `lodestone-client/src/driver.rs`). It was turned on by
default when this first landed, and a real server disconnected the repo owner
with *"Chat message validation failure"*. That mitigation is separate from the
fix below and is still in force.

**The fault was not in the signature.** `build_signature_payload` was
re-derived independently from `PlayerChatMessage.updateSignature` →
`SignedMessageLink.updateSignature` → `SignedMessageBody.updateSignature` →
`LastSeenMessages.updateSignature` and is byte-for-byte correct, including the
epoch-**seconds** timestamp. Vanilla never emits
`multiplayer.disconnect.chat_validation_failed` for a bad signature at all:
`ServerGamePacketListenerImpl` raises it from exactly two places, both
`LastSeenMessagesValidator.ValidationException` — `unpackAndApplyLastSeen` and
`handleChatAck`. It is an **acknowledgement-bookkeeping** failure, not a
cryptographic one.

**What actually broke.** Both peers must count the same messages in the
last-seen window, and both count *signed* ones only — the server calls
`addPending` under `if (signature != null)`, and vanilla's client mirrors that
null check around `markMessageAsProcessed` in
`ChatListener.handlePlayerChatMessage`. This client had no such guard: v770's
adapter reports `ack: Some(..)` for **every** `PLAYER_CHAT` with an empty
`signature` when the wire's optional signature is absent, and the driver fed
all of them to the tracker. Every unsigned `PLAYER_CHAT` — a player with no
chat session, or *this client's own message echoed back while it sent
unsigned* — advanced our offset past a server count that never moved.

**Why signing exposed a pre-existing bug.** `ChatMessage::unsigned` hard-codes
`last_seen_offset: 0`, an empty bitset and `checksum: 0`, and `0` is vanilla's
`LastSeenMessages.Update.IGNORE_CHECKSUM`. Unsigned chat therefore transmitted
no real acknowledgement at all and could never trip the validator; the drift
accumulated invisibly. The signed path transmits the real offset, bitset and a
real non-zero checksum, so the very first signed message published the drift.

**Measured against the live vanilla 26.2 oracle** (offline mode, `:25570`),
with a raw probe sharing no code with this tree:

| probe | server's own log |
|---|---|
| `chat_ack` with `offset = 5`, nothing tracked | `Advanced last seen window by 5 messages, but expected at most 0` → `lost connection: Chat message validation failure` |
| send one unsigned chat, read the echo | `PLAYER_CHAT` echo carried **no signature** (`[Not Secure] <probe> …`), i.e. exactly the message class we were miscounting |
| then `chat_ack` with `offset = 1` | `Advanced last seen window by 1 messages, but expected at most 0` → same disconnect |

One unsigned message is enough. The fix is the missing guard, placed where
vanilla places it — in the driver's `ClientEvent::Chat` arm, not in the
adapter, because the decoder should report the packet rather than judge it (and
the incoming-verification path wants `ack` present for unsigned messages so it
can mark them unverified).

**Before flipping the default back on**, the remaining unverified link is
whether a real server accepts our *signature*. That needs an online-mode
server and a genuine Mojang key; the offline oracle cannot answer it, and no
test here may reach the session servers or the owner's keychain.

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
(link-ordering enforcement, expiry, or a per-sender chain state machine).
**Now called**, in [`Driver`]'s `emit`: on a `PLAYER_CHAT`-shaped
`ClientEvent::Chat` it looks the sender's announced session up (via
`SharedState::chat_session_of`, reading the `ChatSessionInfo` the tab-list
fold now retains — see the wire-shapes section below for the retention half
of this, which was the actual gap), rebuilds the exact `SignedMessageLink`
from `ChatAckInfo::message_index`/the sender/the session id, and calls this
function. `ChatAckInfo::verified` carries the result back out; an unmarked
message (no signature, unknown sender, or a failed check) gets a
`[Not Secure] ` tag prepended to its text — see "What is still not built"
below for the link-ordering/expiry/`MODIFIED` pieces this does not cover.

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
  messages need to be verified against.
  **This decode was never the actual gap** — `PlayerInfoEntry` had carried it
  correctly for a while. The real block was one layer up: `adapter::player`'s
  `PLAYER_INFO_UPDATE` arm converted `PlayerInfoEntry` into the canonical
  `lodestone_model::event::PlayerListEntry` field by field, and that struct
  had no `chat_session` field to receive it — so the key reached this crate
  and then had nowhere to go, and no consumer could ever look a sender up.
  Fixed by adding `ChatSessionInfo` there and `RemoteChatSession` on
  `lodestone_game::tablist::PlayerListEntry`, folded with the same
  "`None` means this delta didn't mention it, keep the existing value" rule
  `properties` already uses.

### The producer — `lodestone_client::Driver`

This is the piece that used to not exist, so it gets its own section rather
than a bullet.

- [`ClientAction`] (`crates/lodestone-model/src/action.rs`) gained two new
  variants rather than extending `SendChat`'s existing fields: `SendChat`
  itself is unchanged (so every existing caller and test — including ones in
  files this work does not own — keeps compiling with no edits), and
  [`ClientAction::SendSignedChat`]/[`ClientAction::AnnounceChatSession`] carry
  the signed payload and the session announcement respectively.
  `crates/lodestone-model/src/adapter.rs`'s `ClientActionKind`/`From` impl is
  an *exhaustive* intra-crate match, so both variants needed an arm there too
  — the compiler catches a missed one, but only inside `lodestone-model`.
- v770's `adapter/serverbound.rs` gained matching encode arms:
  `SendSignedChat` → `ChatMessage` with a real `signature`/`timestamp`/`salt`
  and non-zero ack fields (same packet id as unsigned `SendChat`), and
  `AnnounceChatSession` → `ChatSessionUpdate`. The legacy adapters (v47/v340/
  v735) need no change — their `encode_action` matches end in a wildcard
  `_ => Ok(None)`, so an unrecognised new variant just fails to encode there,
  exactly like every other action they don't implement.
- [`Driver`] (`lodestone-client/src/driver.rs`) owns the session lifecycle:
  - On [`ClientEvent::Login`], if `auth_session` is `Some` (online-mode join)
    and no `chat_session` exists yet, it calls [`fetch_key_pair`], builds a
    [`ChatSession`] from the result, and pushes
    `ClientAction::AnnounceChatSession` as an auto-action — the same
    mechanism `emit` already uses for keep-alive/pong/cookie auto-responses.
    A failed fetch is logged and degrades to unsigned chat for the rest of
    the session; it does not end the connection.
  - `handle_action` runs every outgoing action through
    [`Driver::maybe_sign_chat`], which turns a plain `SendChat` into
    `SendSignedChat` when `chat_session` is `Some`, and leaves every other
    action (and `SendChat` with no session) untouched.
  - The actual signing is [`sign_chat_action`], a **free function** (not a
    `Driver` method) so it can be unit-tested without a live `Connection`:
    it calls `chat_tracker.generate_and_apply_update()` — the *same* call
    vanilla's `LocalPlayer.sendChat` makes — so the signature covers the
    real, current last-seen window and the wire's ack fields are the same
    update. A plain unsigned `SendChat` never calls this, so sending
    unsigned cannot perturb a later signed message's chain.
  - The salt is derived from `Uuid::new_v4()`'s random bytes rather than
    pulling in a second RNG dependency; the wire timestamp is
    `lodestone_time::epoch_duration()` (millis), and the signature payload's
    seconds value is `timestamp_millis / 1000` — one clock read, one
    division, so the two units cannot drift apart at the call site the way
    two independent reads could.
  - Nothing threads through `crates/lodestone-shell/src/net.rs`: the shell
    already resolves an authenticated session and calls
    `ClientBuilder::online_session` for every online-mode join (that wiring
    predates this work), so `Driver.auth_session` is already populated
    whenever it should be — the driver reacting to its own existing field is
    what closes the loop, not a new shell call site.

## How to change it

**What is still not built**, for whoever picks this up next:

- `ClientAction::SendCommand` stays unsigned — there is no `ChatCommandSigned`
  producer. That packet needs per-argument signing against a command tree the
  server declares signable (`ArgumentSignatures.signCommand`), which is a
  distinct and larger feature than message signing, not a small extension of
  [`sign_chat_action`].
- **Landed since — key refresh is now polled.** `Driver`'s `ClientEvent::KeepAlive`
  arm (the same tick surrogate the last-seen flush already piggybacks on — this
  driver has no client tick of its own) checks
  `ChatKeyPair::due_refresh` against `lodestone_time::epoch_duration()` and
  re-`fetch_key_pair`s when due. Whether vanilla keeps the same session UUID and
  chain position across a refresh, or starts a fresh session, was unread as of
  this writing and is **still** unread — this took the conservative reading
  rather than resolve the question: a refreshed key becomes a brand-new
  `ChatSession` (fresh session id, chain reset to link 0), announced exactly
  like the join-time one via a second `ClientAction::AnnounceChatSession`. A
  failed refresh is logged and the *stale* key keeps signing — not a fallback
  to unsigned chat, which would immediately trip the exact last-seen-window
  mismatch this doc's "chat-validation kick" section above documents for an
  unsigned message sent mid-session. Whoever resolves the
  `AccountProfileKeyPairManager`/`LocalChatSession` question should revisit
  whether the fresh-session choice was the right one.
- **Landed since — the receive side.** `Driver`'s `emit` now calls
  [`verify_signature`] against the sender's retained `ChatSessionInfo` and
  surfaces the result (`ChatAckInfo::verified`, and a `[Not Secure] ` text
  tag on failure — see "How it works" above). What is still missing from
  vanilla's own `ChatTrustLevel`/`SignedMessageChain.Decoder`:
  - **link-ordering enforcement and key expiry.** A message whose
    `SignedMessageLink.index` is out of order, or whose key has passed
    `ChatSessionInfo::expires_at`, still verifies here if the RSA check
    passes — vanilla's decoder would reject both independently of the
    signature.
  - **the `MODIFIED` trust level.** Every unverified message reads as
    vanilla's `NOT_SECURE`; `SECURE` vs `MODIFIED` (whether the *displayed*
    text still contains what was actually signed) is not distinguished.
  - **a per-sender chain state machine.** Verification is stateless per
    message; there is no `RemoteChatSession` object tracking a sender's
    chain across messages the way `SignedMessageChain.Decoder` does.

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
Whether a given join signs its chat is entirely a function of whether
[`lodestone_client::ClientBuilder::online_session`] was called before
`connect`/`connect_with` — there is no separate toggle, and none is planned:
an online-mode account signs, offline play does not.

## Dependencies

- `rsa` 0.9 (native-only) — RSA-2048 key parsing (PKCS#8 private,
  X.509 SPKI public) and `SHA256withRSA`/PKCS#1 v1.5 signing and
  verification. Already a dependency of `lodestone-net` for the encryption
  handshake; now also a direct dependency of `lodestone-auth` for the same
  reason.
- `sha2` (native-only, `oid` feature) — the digest `Pkcs1v15Sign` signs over.
- `base64`, `serde`/`serde_json` — already crate dependencies, reused for PEM
  body decoding and the `/player/certificates` response shape.
- `lodestone-time` (`lodestone-client`, new) — the wire timestamp's portable
  wall clock (`epoch_duration()`). Not `std::time::SystemTime::now()`
  directly: that compiles for wasm32 and panics at runtime, and this crate's
  own `wasm-check.sh` confinement rules already ban `Instant::now(`
  crate-wide with an empty allowlist — see that crate's `Cargo.toml` comment
  on the new dependency.

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

**The wiring's own evidence, gathered this pass**: `crates/protocol/v770/tests/chat_dispatch.rs`'s
`send_signed_chat_encodes_millis_timestamp_and_ack_fields_in_order` asserts
`ClientAction::SendSignedChat`'s encoded bytes field-by-field against a
hand-built expected buffer (message, millis timestamp, salt, signature
presence + 256 bytes, offset, 3-byte ack, checksum — pairwise-distinct
values throughout), and `announce_chat_session_encodes_uuid_millis_key_then_signature`
does the same for `AnnounceChatSession` → `chat_session_update`.
`lodestone-client/src/driver.rs`'s
`sign_chat_action_signs_over_the_real_last_seen_chain_with_correct_units`
builds a throwaway session (via the test-only
[`ChatKeyPair::for_tests`]) and independently re-verifies the produced
signature with [`verify_signature`] against the *real* last-seen entry the
tracker held — and asserts the same signature does **not** verify against an
empty window, the transposition control for "signs over the real chain, not
`&[]`". A second test asserts two sends of identical text produce different
signatures (the chain index advanced). None of this touches a real account,
keychain, or network endpoint — `ChatKeyPair::for_tests` exists precisely so
it doesn't have to.

**Not verified, and not claimable from this repo**: whether a real vanilla
server accepts a message signed this way. That needs a live join this repo's
tests never make — the same limitation every other network-touching part of
`lodestone-auth` already documents (see `docs/accounts.md`'s "What's verified
and what isn't"). Tracing the chain by hand: account signed in → session key
acquired → session announced → message signed over the real last-seen chain →
signature on the wire in the right field order are all verified above; server
acceptance is the one link nothing here can reach.

[`lodestone_auth::fetch_key_pair`]: ../crates/lodestone-auth/src/chat_session.rs
[`fetch_key_pair`]: ../crates/lodestone-auth/src/chat_session.rs
[`lodestone_auth::ChatSession`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatSession`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatSession::sign`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatKeyPair`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatKeyPair::due_refresh`]: ../crates/lodestone-auth/src/chat_session.rs
[`ChatKeyPair::for_tests`]: ../crates/lodestone-auth/src/chat_session.rs
[`build_signature_payload`]: ../crates/lodestone-auth/src/chat_session.rs
[`verify_signature`]: ../crates/lodestone-auth/src/chat_session.rs
[`flow::Session::access_token`]: ../crates/lodestone-auth/src/flow.rs
[`flow::join_server`]: ../crates/lodestone-auth/src/flow.rs
[`lodestone_auth::Session`]: ../crates/lodestone-auth/src/flow.rs
[`lodestone_auth::login`]: ../crates/lodestone-auth/src/login.rs
[`lodestone_auth::flow`]: ../crates/lodestone-auth/src/flow.rs
[`lodestone_auth::server_hash`]: ../crates/lodestone-auth/src/hash.rs
[`ChatSessionUpdate`]: ../crates/protocol/v770/src/packets/game.rs
[`ChatCommandSigned`]: ../crates/protocol/v770/src/packets/game.rs
[`ArgumentSignatureEntry`]: ../crates/protocol/v770/src/packets/game.rs
[`RemoteChatSessionData`]: ../crates/protocol/v770/src/packets/player_info.rs
[`lodestone_model::event::ChatSessionInfo`]: ../crates/lodestone-model/src/event.rs
[`lodestone_game::tablist::RemoteChatSession`]: ../crates/lodestone-game/src/tablist.rs
[`PlayerInfoEntry::chat_session`]: ../crates/protocol/v770/src/packets/player_info.rs
[`lodestone_game::chat_ack`]: ../crates/lodestone-game/src/chat_ack.rs
[`lodestone_game::chat_ack::LastSeenTracker`]: ../crates/lodestone-game/src/chat_ack.rs
[`ClientAction`]: ../crates/lodestone-model/src/action.rs
[`ClientAction::SendSignedChat`]: ../crates/lodestone-model/src/action.rs
[`ClientAction::AnnounceChatSession`]: ../crates/lodestone-model/src/action.rs
[`Driver`]: ../crates/lodestone-client/src/driver.rs
[`Driver::maybe_sign_chat`]: ../crates/lodestone-client/src/driver.rs
[`sign_chat_action`]: ../crates/lodestone-client/src/driver.rs
[`ClientEvent::Login`]: ../crates/lodestone-model/src/event.rs
[`lodestone_client::ClientBuilder::online_session`]: ../crates/lodestone-client/src/builder.rs

[`SECURE_CHAT_ENV`]: ../crates/lodestone-client/src/driver.rs
