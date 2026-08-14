# Server-initiated resource pack push (issue #334)

## What it is

How Lodestone's integrated server pushes a resource pack to a connected player —
the download URL, SHA-1 hash, required flag, and optional prompt that a client
shows on its accept/decline screen. The client-side **decode** of the packet
landed first; this issue is the server half: the version-free
vocabulary struct, the `ServerProtocol` seam method, its `v770` encoder, and the
feed a host publishes a push into. Part of the server-plumbing epic (#339).

## How it works

The full wire path, in the order a push travels:

1. **Publish** — a host calls
   [`ResourcePackPushFeed::publish`](crate::server::ResourcePackPushFeed) with a
   [`ResourcePackPush`](crate::protocol::ResourcePackPush). This is the same call
   a future `IntegratedServer` config or command surface makes; no inbound packet
   ever produces a push.
2. **Drain** — `serve_play`'s `container_sync_tick` timer arm (the same 50 ms
   `CONTAINER_SYNC_INTERVAL` that drains the block/explosion/weather feeds) pulls
   the feed and encodes each push.
3. **Encode** — `V770ServerProtocol::encode_resource_pack_push` (hand-written,
   play id 81) emits the frame.
4. **Decode** — a real client turns it into
   `ClientEvent::ResourcePackPushed { id, url, hash, required, prompt }`
   (`crates/lodestone-model/src/event.rs`) and shows the accept/decline
   screen.
5. **Reply** — the client sends `ClientAction::ResourcePackResponse { id,
   response: ResourcePackResponseKind }` (`crates/lodestone-model/src/action.rs`).
   The server decode-then-drops it (`play::serverbound::RESOURCE_PACK` →
   `ServerBound::Ignored`, mirroring vanilla's `ServerPacketListenerImpl` which
   accepts any response), so a reply never drops the connection.

### The wire layout

`ClientboundResourcePackPushPacket`, which the encoder writes directly (there is
no packet struct to derive `Encode` — the client only ever *decodes* this packet):

| field | wire form |
|---|---|
| `id` | raw 16-byte UUID |
| `url` | VarInt-prefixed UTF-8 string |
| `hash` | VarInt-prefixed UTF-8 SHA-1 (lowercase hex) |
| `required` | bool |
| `prompt` (optional) | bool `has_prompt`, then network-NBT chat component |

The encoder mirrors `V770Adapter`'s decode arms (both `configuration`, id 9, and
`play`, id 81, read it with `read_network_nbt` + `Text::from_nbt`), so it is the
mirror-side specification, in the same "no existing struct" style as
`encode_system_chat`.

### Why the push arrives in Play, not Configuration

Vanilla pushes during Configuration (its `ServerResourcePackConfigurationTask`).
This crate's `begin_configuration` is a static `Vec` with no arguments to carry a
pack, and the feed's drain point is `serve_play`'s timer — so the push reaches
the client after the configuration handoff. Both `v770` decode arms are
wire-identical, so the play-phase push (id 81) is what the current wiring can
emit; a configuration-phase push is a documented follow-up that needs a
state-carrying call site.

### The version-free seam

Everything server-side is version-free in `crates/lodestone-server`:

- [`ResourcePackPush`](crate::protocol::ResourcePackPush) — the vocabulary
  struct, mirroring the wire record exactly (owned `String`s, so a push can ride
  a feed across a task boundary).
- `ServerProtocol::encode_resource_pack_push` — a defaulted method returning
  `ServerDirective::None`, so a protocol family with no resource-pack support
  behaves exactly as before. Only `v770` implements `ServerProtocol`, so only
  26.2 pushes packs today.
- The `Box<P>` forwarding impl (every seam method must be forwarded — a defaulted
  method that is not silently answers `None`), the `Numbered` spy, and the
  boxed-equality test with an `assert_ne!(…, ServerDirective::None)` control.

### The feed and its entry point

`ResourcePackPushFeed` is `Arc<Mutex<Vec<ResourcePackPush>>>` with
`publish`/`drain_all`, the same idiom as `BlockTickFeed`/`ExplosionFeed`/
`WeatherFeed` — and the same **single-consumer** caveat: exactly one connection
task per feed instance (singleplayer spawns one task per feed). Vanilla's push is
broadcast-shaped (every connection must receive it), so multi-connection
broadcast needs a cursor-shaped feed like `PlayerRegistry::chat_since`
rather than a drain.

The plumbing is reachable through a **new** entry point,
`serve_connection_with_resource_pack`, rather than a changed signature on
`serve_connection`/`serve_connection_with_commands` — those two are called
directly by `crates/protocol/v770/tests/*` and are off-limits. This is the
compatibility shape every feed-carrying variant in this crate follows.

## How to change it, and the gotchas

- **The encoder and `V770Adapter`'s decode are two views of one wire layout.**
  Change one and the round-trip gate (below) breaks; the encoder is the inverse
  of the decode, so keep `text_to_nbt`/`write_network_nbt` as the inverse of
  `read_network_nbt`/`Text::from_nbt`.
- **The 40-character hash cap is documented, not enforced.** Vanilla caps the
  hash via `ByteBufCodecs.stringUtf8(40)` (`MAX_HASH_LENGTH`); our encoder writes
  the field verbatim, so a host that publishes a longer hash produces a frame a
  *real* vanilla client cannot decode. The round-trip gate pins the 40-char edge
  so the cap's boundary stays exercised.
- **The `required` flag is load-bearing on the client.** A declined or failed
  download of a `required` pack disconnects the client — that behaviour is
  vanilla's, on the client side, and this issue only transmits the flag.
- **A push published before the handoff sits in the feed until play starts**, and
  a push published after the event is read is never observed — the test's
  publish-after-spawn point is deliberate, so a sloppy early-publish can't pass
  a gate that never waited for play.
- **No `ServerBound` variant was added** — the response decodes to the existing
  `ServerBound::Ignored`, so there is no directive-sequence change for
  choreography tests to absorb.
- **The `IntegratedServer` trigger is not wired.** This issue is the plumbing —
  the feed, encoder, drain, and entry point a host calls. Exposing it as a
  config field or a command on `IntegratedServer` is the follow-up; nothing calls
  `publish` today.

## Evidence

`crates/protocol/v770/tests/resource_pack_push.rs` — one end-to-end gate: a push
published into a `ResourcePackPushFeed` travels through `serve_play`'s timer
drain, the hand-written encoder, and a real in-memory `Connection`, and comes out
the other side as a real client's `ClientEvent::ResourcePackPushed` with every
field asserted byte-for-byte (id, url, the full 40-character hash, the `required`
flag, and the prompt's plain text). The client then replies
`ResourcePackResponse::Accepted` and the gate asserts the server task is *still
running* — a decode error in `serve_play`'s dispatch would finish it.

Nothing in the gate is a stand-in: the producer is `publish` (the call a config
surface makes), the drain is the real `container_sync_tick` arm, the encoder and
client are the real `v770` implementations over the real `memory_pair` transport.

**Not yet run**: the shared tree has been red from other agents' in-flight edits
throughout this issue, so the gate compiles nowhere yet. The encoder itself is
also covered by `protocol.rs`'s boxed-equality test, which asserts the `Box<P>`
forwarding matches the concrete implementation and that the default is not
silently emitted.

### What remains unproven

No real vanilla client has received our push and shown its accept/decline screen
— the gate drives our own client, not a JVM one. And the `IntegratedServer`
config/command trigger does not exist yet, so the push can only be emitted by
code that constructs a feed and calls `publish`.

## Dependencies

- `crates/lodestone-server/src/protocol.rs` — `ResourcePackPush` struct and the
  `ServerProtocol::encode_resource_pack_push` seam (default `None`).
- `crates/lodestone-server/src/server.rs` — `ResourcePackPushFeed`,
  `serve_connection_with_resource_pack`, and the `container_sync_tick` drain arm.
- `crates/protocol/v770/src/server_protocol.rs` — the hand-written encoder
  (`play::clientbound::RESOURCE_PACK_PUSH` = 81) and the serverbound response
  decode-then-drop arm.
- `lodestone-model` — `ClientEvent::ResourcePackPushed`,
  `ClientAction::ResourcePackResponse`, `ResourcePackResponseKind`, `Text`.
- `.cache/mc/26.2/src` — the decompiled `ClientboundResourcePackPushPacket` as
  the wire-shape reference.

## Related

- [`docs/resource-packs-screen.md`](./resource-packs-screen.md) — the client-side
  screen the push drives.
- Issue #294 — the client-side decode this encoder must mirror.
- Issue #339 — the server-plumbing epic this issue is part of.
