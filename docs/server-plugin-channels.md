# Server-side plugin messaging: the channel registry and dispatch

Issue [#335](https://github.com/matteopolak/lodestone/issues/335), part of the
server-plumbing epic [#339](https://github.com/matteopolak/lodestone/issues/339).
The **wire-level** half of `custom_payload`: see
[`plugin-channels.md`](./plugin-channels.md) (issue #301) for the client/plugin-
facing half this sits underneath.

## What it is

The server side of Minecraft's custom plugin-message machinery, in
`crates/lodestone-server/src/plugin_channels.rs`: a registry of channels the
server has registered interest in, per-connection tracking of which channels
each client announced, and dispatch both ways on the wire. A client sends a raw
payload on a named channel and it reaches the server handler registered for
that channel; the server broadcasts a payload and every connection that
declared support for the channel receives it. **Deliberately not** the
plugin-facing API — that is issue #77's plugin framework; this is the
wire-level registry and dispatch seam it will sit on.

## How it works

Two types, both mirroring shapes this crate already established:

- **[`PluginChannelRegistry`]** — the shared, host-installed registry, cloned
  freely like [`CommandDispatch`] and [`PlayerRegistry`]. Inbound payloads on a
  registered channel are dispatched to that channel's
  [`PluginChannelHandler`] (cloned out of the lock before the call, so a handler
  that re-enters the registry cannot deadlock); payloads on an unregistered
  channel are silently dropped, exactly vanilla's `DiscardedPayload` fallback.
  It also carries the server→client broadcast queue — an append-only log with a
  per-connection cursor, the same shape `PlayerRegistry` uses for chat, so every
  connection reads every payload rather than whichever drained first.
- **[`ClientChannels`]** — one connection's declared-support set, populated from
  the client's `minecraft:register` / `minecraft:unregister` custom payloads
  (the historical vanilla control-channel format: a UTF-8 comma-separated list).
  It is the filter that decides whether a broadcast reaches a given client.

The wire seam is [`ServerBound::CustomPayload`] on the inbound side and the
defaulted `ServerProtocol::encode_custom_payload` on the outbound side — both
lift/emit **every** channel unchanged, and the register/unregister interpretation
lives in this crate rather than at the protocol seam, because a channel is a
channel: the payload format is version-free. `v770` implements both (decode arms
in Configuration and Play, plus the play-id encoder; the round-trip gate is
`crates/protocol/v770/tests/plugin_channels_round_trip.rs`).

Wiring: every pre-existing `serve_connection*` entry point passes a permanently
inert `PluginChannelRegistry::default()`, the compatibility shape this file
established for the feeds and `CommandDispatch`. A host that wants plugin
messaging reaches the new
[`serve_connection_with_plugin_channels`](https://docs.rs/lodestone-server/latest/lodestone_server/fn.serve_connection_with_plugin_channels.html)
entry point with a live registry. The outbound drain runs in `serve_play`'s
`container_sync_tick` timer arm (50 ms), alongside the resource-pack push and
chat drains — a broadcast is host-published, never inbound-packet-driven, so it
needs a timer to notice.

## How to change it

- **Add a server→client broadcast**: `registry.broadcast(channel, data)` — every
  connection that announced `channel` receives it on its next drain.
- **Handle an inbound channel**: `registry.register(channel, handler)` — a future
  #77 plugin does this at registration time; re-registering replaces the handler.
- **A payload that must reach one client only**: check the connection's
  `ClientChannels::supports(...)` and call `proto.encode_custom_payload(...)`
  directly rather than broadcasting.

Gotchas:

- The broadcast queue is bounded (256 entries) and the oldest is trimmed; a
  connection that falls far behind loses the overflow, never blocks the queue.
- `plugin_channel_cursor` starts at **0** (unlike chat, which starts at the log's
  current end): a broadcast published *before* a client joined is still owed to
  it — a client that announces `minecraft:brand` at join should get a brand
  payload that was already queued.
- On `wasm32`, the inbound half (dispatch) is wired identically, but the outbound
  broadcast drain rides `container_sync_tick`, which `tokio::time` gives that
  target none of — the same documented gap as the resource-pack and weather
  drains. A browser singleplayer world receives no broadcast until an inbound
  packet happens to flow.
- A channel the client never announced is a **skip, not a block**: the cursor
  advances past it, so one unregistered channel cannot stall the queue.
- Adding a `ServerBound` variant is gated by
  `crates/protocol/v770/tests/serverbound_wiring.rs`: every variant must be
  constructed inside v770's `decode`, or that test fails.

## Configuration

No env vars or config files. Channel names are `ResourceKey`s (namespace:path);
`minecraft:register` / `minecraft:unregister` are the reserved control channels
and are never dispatched to handlers.

## Dependencies

`lodestone-model`'s `ResourceKey`. No external services. `v770` provides the
only production `ServerProtocol` implementation of the encode/decode seams.

[`PluginChannelRegistry`]: https://docs.rs/lodestone-server/latest/lodestone_server/struct.PluginChannelRegistry.html
[`PluginChannelHandler`]: https://docs.rs/lodestone-server/latest/lodestone_server/trait.PluginChannelHandler.html
[`ClientChannels`]: https://docs.rs/lodestone-server/latest/lodestone_server/struct.ClientChannels.html
[`CommandDispatch`]: https://docs.rs/lodestone-server/latest/lodestone_server/struct.CommandDispatch.html
[`PlayerRegistry`]: https://docs.rs/lodestone-server/latest/lodestone_server/struct.PlayerRegistry.html
[`ServerBound::CustomPayload`]: https://docs.rs/lodestone-server/latest/lodestone_server/enum.ServerBound.html
