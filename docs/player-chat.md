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
   to a hand-copied layout. Produces `ServerBound::Chat { message }`.
2. **Publish** — `dispatch_play_packet`'s `ServerBound::Chat` arm fills an
   `outgoing_chat` out-parameter; `serve_play` reads it back and calls
   `PlayerRegistry::say(username, message)`. This is the same read-back idiom
   `player_pos`/`player_rot` already use, and for the same reason: the caller owns both the
   username and the registry, and `dispatch_play_packet` already takes 25 parameters.
3. **Broadcast** — `PlayerRegistry` (`crates/lodestone-server/src/players.rs`) holds a
   bounded, append-only `VecDeque<ChatLine>` plus a `chat_base` sequence number.
4. **Drain** — each connection keeps its own `chat_cursor` and calls
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

**Chat is accepted and broadcast unsigned.** This is a stated choice, not an omission.

The wire packet carries a timestamp, a salt, an optional 256-byte signature and a last-seen
acknowledgement block. All of it is *decoded* — the layout must be read to find the end of
the frame — and then **dropped**. Nothing is verified.

That is the honest position because this crate has no session-key infrastructure to verify
against: it never handles `chat_session_update`, holds no player public keys, and reports
`enforcesSecureChat = false` in its own status response. Carrying an unverifiable signature
further into the server would be strictly *worse* than dropping it, because a later reader
could mistake its presence for validation.

Consequences, stated plainly:

* Messages go out as **`system_chat`**, not `player_chat`. The `chat.type.text` decoration
  (`"<%s> %s"`) that a vanilla client would apply from the chat-type registry is applied by
  us instead, in `ChatLine::rendered`.
* There is no `DELETE_CHAT`, no report chain, and no "Not Secure" indicator.
* The acknowledgement `offset` is dropped, for the same reason `ChatAckInfo` is unreachable
  from the WASM plugin ABI: the sequence counter belongs to whoever drives the connection,
  and a second writer forks it. **Do not reopen that.**

Verifying signatures and emitting real `player_chat` is a separate, larger piece of work. It
would need `chat_session_update` handling, a public-key store, `MessageSignatureCache`
(already decoded-shape-complete and tested but unconsumed, a declared island on #436), and
an `encode_player_chat` on the `ServerProtocol` trait, which does not exist today.

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

None. No env vars, no flags.

## Dependencies

* `crates/protocol/v770` — the decode arm and `encode_system_chat`. Only `v770` implements
  `ServerProtocol`, so it is the only family that can host chat.
* `PlayerRegistry` / `PlayerAwareSource` (`crates/lodestone-server/src/players.rs`), reached
  through `EntitySource::players()`.

Gated by `crates/protocol/v770/tests/server_chat_broadcast.rs`: two real connections against
one shared world, with the serverbound frame hand-built from the 26.2 packet definition and
B's reply decoded by the pre-existing client-side decoder.
