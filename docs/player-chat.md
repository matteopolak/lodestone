# Player chat

## What it is

The inbound half of chat: a player types a message, our server decodes it, and every
connected player sees it. Before #469 the two halves of chat were in opposite states — we
could say things to a player (`system_chat` was complete and well-tested) and a player
could say nothing to us, because serverbound `minecraft:chat` was never decoded at all.

## How it works

Four hops, each in a different crate:

1. **Decode** — `crates/protocol/v770/src/server_protocol.rs`, the `play::serverbound::CHAT`
   arm. It reuses `packets::game::ChatMessage`, the *same* struct `adapter.rs` already
   encoded for the client half, so decode and encode are pinned to one another rather than
   to a hand-copied layout. Produces `ServerBound::Chat { message, timestamp_millis, salt,
   signature }` — the acknowledgement block (offset/bit set/checksum) is still decoded to
   find the end of the frame and then dropped, for the reason "The signing decision" below
   gives, but the rest of the signed payload now survives.
2. **Decide** — `crate::chat_session::decide` (`crates/lodestone-server/src/chat_session.rs`)
   folds the message through the sender's announced session, if any — see "The signing
   decision" below. A rejected message stops here: it is reported back to the sender alone
   (`encode_system_chat`) and never reaches step 3.
3. **Publish** — `dispatch_play_packet`'s `ServerBound::Chat` arm fills an
   `outgoing_chat` out-parameter for an accepted message; `serve_play` reads it back and
   calls `PlayerRegistry::say(username, message)`. This is the same read-back idiom
   `player_pos`/`player_rot` already use, and for the same reason: the caller owns both the
   username and the registry, and `dispatch_play_packet` already takes 25+ parameters.
4. **Broadcast** — `PlayerRegistry` (`crates/lodestone-server/src/players.rs`) holds a
   bounded, append-only `VecDeque<ChatLine>` plus a `chat_base` sequence number.
5. **Drain** — each connection keeps its own `chat_cursor` and calls
   `PlayerRegistry::chat_since` on the existing `container_sync_tick` (50 ms), encoding each
   line with `ServerProtocol::encode_system_chat`.

### Why the chat log lives on `PlayerRegistry`

Chat is the first thing this crate carries that is genuinely a **push**. The roster and the
entity stream are *pulls* — a connection rediscovers them by diffing "what is true now"
against "what I last sent" — which is why `players.rs`'s own module doc explains that no
broadcast channel was needed. That reasoning is still correct about everything it was
written about, and does not extend to "Alice said hello".

The registry was chosen over a new feed because it is **already** the one object every LAN
connection shares, and is **already** reachable from `serve_play` via
`EntitySource::players()` — a defaulted trait method that exists precisely so shared state
could be added without changing `serve_connection`'s signature. A new `ChatFeed` would have
needed a new parameter on seven entry points plus a third relay copy in
`IntegratedServer::bind`.

### Cursors, not drain-all

`BlockTickFeed`/`ExplosionFeed` are `drain_all`: the first reader takes everything and a
second sees nothing. That is fine for their single-consumer use and **fatal** for a
broadcast — each message would reach whichever connection's timer fired first and nobody
else. Chat therefore uses a per-reader cursor over a shared log, the shape `tick.rs`'s own
doc names as the right one for a growing subscriber count.

Cursors are **absolute sequence numbers**, not indices, so trimming the front of the bounded
window cannot silently rewind a reader — a lagging cursor is snapped forward, dropping the
overflow, rather than replaying messages the client already displayed. A joining connection
starts at `chat_cursor()` (the current end) so it is not replayed the backlog.

### The sender receives their own message

Checked against the jar, not assumed: `PlayerList.broadcastChatMessage`
(`PlayerList.java:738-753`) loops `for (ServerPlayer player : this.players)` with no sender
exclusion, and a vanilla client does not echo its own chat locally — it waits for the
server. Excluding the sender would make their own messages invisible to them.

## The signing decision

**Decode and signature verification now exist; the outbound relay is still unsigned.** Three
different claims used to collapse into one "chat has no secure profile" statement; they now
have three different answers.

`chat_session_update` is decoded (`ServerBound::ChatSessionAnnounced`) and held per connection
as `crate::chat_session::ServerChatSession` — a session id, key expiry and the announced
public key, plus the chain position this server tracks against it. A `chat` packet's
`timestamp`/`salt`/`signature` now survive decoding too, and `crate::chat_session::decide`
verifies a signed message against the announced key with
`lodestone_auth::verify_signature` before it is allowed anywhere near step 3 above. See that
module's own doc comment for the exact accept/reject rules (mirroring
`SignedMessageChain.Decoder`/`.unsigned`) and, more importantly, for what verification here
does **not** establish: the announced public key's Mojang provenance is never checked (no
fetch of Mojang's Services signing key), so a verified message proves the sender holds the
private key matching whatever they announced, not that the key was ever issued to their real
account. That is narrower than what a real vanilla server's `enforce-secure-profile=true`
promises — see `crate::properties`'s own doc comment for why this crate's default for that
key is deliberately `false`, not vanilla's real `true`.

`server.properties`' `enforce-secure-profile` is wired (`ServerProperties::enforce_secure_profile`
→ `PlayerRegistry::set_enforce_secure_profile`, consulted by `decide`): once a client has
announced a session, an unsigned message from it is always rejected regardless of this flag
(there would be nothing to check an unsigned message against); before any session is
announced, this flag decides whether unsigned chat is accepted (off, the default) or rejected
(on). A rejection is reported back to the sender alone via `encode_system_chat`, mirroring
vanilla's `handleMessageDecodeFailure` — logged, replied to, never a disconnect. That
disconnect (`multiplayer.disconnect.chat_validation_failed`) is a different failure this
crate does not reach: vanilla raises it only from last-seen acknowledgement bookkeeping, which
is the next item.

**Still broadcast unsigned to every peer, and nothing here changes that.** A verified message
is still relayed as `system_chat`, not `player_chat` — verification only gates whether *this
server* accepts a message, not whether another client can independently verify it too.
Consequences, stated plainly:

* Messages go out as **`system_chat`**, not `player_chat`. The `chat.type.text` decoration
  (`"<%s> %s"`) that a vanilla client would apply from the chat-type registry is applied by
  us instead, in `ChatLine::rendered`.
* There is no `DELETE_CHAT`, no report chain, and no "Not Secure" indicator.
* The acknowledgement `offset` (`chat_ack`/`chat`'s trailing last-seen block) is decoded far
  enough to find the end of the frame and then still dropped — for the same reason
  `ChatAckInfo` is unreachable from the WASM plugin ABI: the sequence counter belongs to
  whoever drives the connection, and a second writer forks it. **Do not reopen that.** This is
  also why `crate::chat_session::decide` is sound verifying against an *always-empty*
  last-seen list rather than reconstructing one from a signature cache: since this server
  never sends a signed `player_chat`, a real client's own outgoing last-seen window can never
  contain anything else, by construction.

Real `player_chat` relay — so a peer can verify a message too, not just this server — is a
separate, larger piece of work. It would need `MessageSignatureCache` (already
decoded-shape-complete and tested but unconsumed — a declared island) actually consumed on the
relay path, real per-connection last-seen bookkeeping (the acknowledgement machinery named
above, currently dead by design), and an `encode_player_chat` on the `ServerProtocol` trait,
which does not exist today.

## How to change it

* **Adding a chat consumer** — read from `PlayerRegistry::chat_since` with your own cursor.
  Never `drain`; see "Cursors, not drain-all".
* **Changing the rendered form** — `ChatLine::rendered` in `players.rs`. If you ever emit a
  real `player_chat`, that decoration moves back to the client and this method should go.
* **Chat on `wasm32`** — the *publish* half works; the *drain* half does not, because it
  rides `container_sync_tick` and that target has no `tokio::time`. A `wasm32`-served
  connection's messages reach others; it receives none itself. This is the same documented
  gap `vitals` and `sync_open_container` already have there.
* **Log capacity** is `CHAT_LOG_CAPACITY` (256) in `players.rs`. It is bounded because it is
  process-lifetime shared state nothing truncates.

### Gotchas

* Empty/whitespace-only messages are dropped rather than broadcast — vanilla's client will
  not send one, so a frame carrying one is malformed rather than meaningful.
* Chat is delivered on a 50 ms timer, not synchronously with the inbound packet. A test that
  reads B's socket immediately after A writes will race it; wait for quiet.
* `ServerBound::Chat` is *not* `ServerBound::ChatCommand`. The command half (#48/#464) goes
  to the Brigadier dispatcher and replies **only to the caller**; chat goes to everyone.

## Configuration

`server.properties`' `enforce-secure-profile` (`ServerProperties::enforce_secure_profile`,
default `false` — a deliberate divergence from vanilla's real default; see that module's own
doc comment). Applied once at server startup via
`PlayerRegistry::set_enforce_secure_profile`; `crates/lodestone-dedicated-server/src/main.rs`
is the one production caller. No other env vars or flags.

## Dependencies

* `crates/protocol/v770` — the decode arms (`CHAT`, `CHAT_SESSION_UPDATE`, `CHAT_ACK`) and
  `encode_system_chat`. Only `v770` implements `ServerProtocol`, so it is the only family that
  can host chat.
* `PlayerRegistry` / `PlayerAwareSource` (`crates/lodestone-server/src/players.rs`), reached
  through `EntitySource::players()` — also where `enforce_secure_profile` lives, since it is
  server-wide policy rather than per-connection state.
* `crate::chat_session` (`crates/lodestone-server/src/chat_session.rs`) — the per-connection
  announced session and the accept/reject policy; see its own module doc for what it verifies
  and what it deliberately does not (Mojang key provenance, real `player_chat` relay,
  `chat_ack` bookkeeping).
* `lodestone_auth::{SignedMessageLink, verify_signature}`, native-only, matching every other
  RSA-touching path in this crate.

Gated by `crates/protocol/v770/tests/server_chat_broadcast.rs`: two real connections against
one shared world, with the serverbound frame hand-built from the 26.2 packet definition and
B's reply decoded by the pre-existing client-side decoder — including
`enforcement_rejects_an_unsigned_message_and_replies_only_to_the_sender`, which turns
`PlayerRegistry::enforce_secure_profile()` on and asserts the rejected message never reaches
the other connection and the sender alone is told why. `crate::chat_session`'s own unit tests
cover the signature-verification decision table hermetically, including a forged-signature
case that breaks the chain for every message after it until a fresh session is announced.
