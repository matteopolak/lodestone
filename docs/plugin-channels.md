# Plugin channels (`custom_payload`)

## What it is

Typed, per-channel delivery of Minecraft's `custom_payload` (plugin-messaging)
packet to a plugin. A plugin declares a channel by implementing
`lodestone_ecs::plugin_channel::PluginChannel` on its own `#[derive(Message)]`
type, calls `add_plugin_channel::<T>()` in its `Plugin::build`, and reads decoded
`T`s with an ordinary `MessageReader<T>`. Issue #301.

## What was an island until the built-in channel landed

Everything below was true and tested for a whole release cycle while **no
production `App` in the workspace registered a single channel**. Every test in
`plugin_channel.rs` builds its own `App` and calls `add_plugin_channel` itself —
a closed loop — and `crates/plugins/lodestone-server-brand` was added by exactly
one integration test and was not a dependency of `lodestone-shell` or
`lodestone-app` at all.

That hid a second, larger island behind it. `GameEventBusPlugin` is opt-in and
`lodestone_client::SharedState` caches `game_event_bus_enabled` **once, at
construction**; only `PluginChannelPlugin::build` adds the bus. With no channel
anywhere in the shipped `App`, the `GameEventBus` resource was absent and
`push_to_game_event_bus` was skipped for **every `ClientEvent`**, not just for
`CustomPayload`. The diagram's second arrow was dead in production.

`lodestone_ecs::brand::ServerBrandChannelPlugin` — a built-in `minecraft:brand`
channel — is now installed by `lodestone_app::client_app()`, which is what
`Sim::client_app()` and every headless consumer start from. It is a near-duplicate
of `lodestone-server-brand`, on purpose: that crate is the *worked example* proving
a third party needs no privileged access, and `lodestone-app`'s manifest is a
closed allowlist (`crates/lodestone-app/tests/renderer_free_graph.rs`) so adding
it as a dependency would have to punch a hole in the gate that keeps a headless
graph small. See `lodestone_ecs::brand`'s module doc. **The two transcribe the
same one-line vanilla codec independently and nothing enforces agreement** — if
`BrandPayload` grows a field, fix both.

`crates/lodestone-app/tests/custom_payload_dispatch_is_installed.rs` is the gate,
with a `CorePlugin`-only negative control that must find neither resource.

**Still inert: the server half.** Inbound `custom_payload` is decoded and
dispatched in the live per-connection loop (Configuration and Play both), but
every production call site passes `PluginChannelRegistry::default()` — zero
handlers, empty broadcast queue. Open-to-LAN is the only path that *can* thread a
caller-supplied registry (`LanConfig::plugin_channels`), and the shell's sole
`LanConfig` construction takes the default; singleplayer's duplex path has no such
field at all. See `docs/open-to-lan.md` and `docs/server-plugin-channels.md`.

## How it works

```text
  server bytes ──▶ version adapter ──▶ ClientEvent::CustomPayload { channel, data }
                                              │
                              SharedState::apply → push_to_game_event_bus
                                              │
                                       GameEvent(ClientEvent)
                                              │
                              dispatch_plugin_channel::<T>   (GameTick,
                                                              before every
                                                              EventPriority tier)
                              filters on T::CHANNEL, calls T::decode
                                              │
                                        Messages<T>
                                              │
                                   the plugin's own system
```

`dispatch_plugin_channel` is scheduled `.before(EventPriority::Lowest)` rather
than *in* a tier, so a subscriber at any tier — including `Lowest` — sees this
tick's payloads on this tick. A dispatcher sharing a tier with its own
subscribers would be unordered against them.

`Messages<T>` registration and its per-tick aging are inherited from
[`cross-plugin-messages.md`](./cross-plugin-messages.md): `add_plugin_channel`
calls `add_plugin_message::<T>()`, so a channel type is an ordinary cross-plugin
message with the documented `TickSet::Send` aging point, and two plugins may
declare the same channel type idempotently.

### Outbound: from a plugin to the socket

```text
  a plugin writes T (a #[derive(Message)] type)
              │
  dispatch_plugin_channel_outbound::<T>     (GameTick,
                                             after every
                                             EventPriority tier)
  ClientAction::SendCustomPayload { channel, data: T::encode(msg) }
              │
           ActionQueue
              │
  Sim::drain_action_queue → version adapter → server bytes
```

`dispatch_plugin_channel_outbound` is scheduled
`.after(EventPriority::Monitor)` — the inverse anchor to the inbound
dispatcher's `.before(EventPriority::Lowest)`, so everything a plugin writes
this tick is queued this tick, even from a writer in the last tier. The sim
drains the `ActionQueue` right after `GameTick`, so the payload reaches the wire
on the same tick; nothing here touches a socket or a version crate. Register
with `add_outbound_plugin_channel::<T>()`, which installs `CorePlugin` (for the
`EventPriority` chain) and the `ActionQueue` if needed and, unlike
`add_plugin_channel`, does **not** install the game-event bus — sending has no
need of `SharedState`.

A type registered **both** ways shares one `Messages<T>` mailbox, so an inbound
payload it decodes is re-queued outbound on the same tick (echo). A plugin that
wants echo-free two-way messaging uses two types, one per direction.

### Two things `add_plugin_channel` does on your behalf, both load-bearing

**It installs the game-event bus.** `GameEventBusPlugin` is opt-in and inserts a
marker resource `SharedState` checks *once, at construction*. Without it
`push_to_game_event_bus` never runs, `Messages<GameEvent>` never receives
anything, and a plugin that registered a channel would receive **zero payloads
forever** while compiling and ticking perfectly. `add_plugin_channel` therefore
adds the bus itself. Gate:
`plugin_channel::tests::channel_dispatch_requires_no_second_opt_in`.

**It parses `T::CHANNEL` once, at build, and panics if it is not a valid
namespaced identifier.** The alternative fails silently: a typo'd or
non-canonical constant simply never matches a payload, and you get a channel that
is registered, ticking and permanently empty. Control:
`a_malformed_channel_constant_panics_at_build`.

## How to change it

Write a plugin like `crates/plugins/lodestone-server-brand` (the worked example —
it consumes clientbound `minecraft:brand`, the one channel a real vanilla server
always sends):

```rust
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct ServerBrand { pub brand: String }

impl PluginChannel for ServerBrand {
    const CHANNEL: &'static str = "minecraft:brand";
    fn decode(data: &[u8]) -> Option<Self> { /* … */ }
}

impl Plugin for ServerBrandPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugin_channel::<ServerBrand>();
        app.init_resource::<ReportedServerBrand>();
        app.add_systems(GameTick, record_server_brand.in_set(EventPriority::Normal));
    }
}
```

Sending is the mirror. Implement
[`OutboundPluginChannel`](https://docs.rs/lodestone-ecs/latest/lodestone_ecs/trait.OutboundPluginChannel.html)
for a `#[derive(Message)]` type, register it with
`add_outbound_plugin_channel`, and write `T`s from any `GameTick` system:

```rust
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct BeaconRequest { pub id: u32 }

impl OutboundPluginChannel for BeaconRequest {
    const CHANNEL: &'static str = "example:beacon";
    fn encode(&self) -> Vec<u8> { self.id.to_be_bytes().to_vec() }
}

impl Plugin for BeaconPlugin {
    fn build(&self, app: &mut App) {
        app.add_outbound_plugin_channel::<BeaconRequest>();
        app.add_systems(GameTick, send_beacons.in_set(EventPriority::Normal));
    }
}
```

Gotchas:

- **Assert the resource, not the plugin.** `App::is_plugin_added::<T>()` stays
  true for a `build` that stopped inserting what consumers read.
  `PluginChannelState<T>::matched()` and `::rejected()` exist for gates:
  `rejected` separates "the payload never arrived" from "it arrived and the
  decoder refused", two failures that look identical from the subscriber's side.
- **`decode` returning `None` is not an error and never disconnects.** Vanilla's
  own fallback for an unparseable payload is `DiscardedPayload` — read and drop.
- **A plugin must depend on `bevy_ecs` directly**, not only on
  `lodestone-ecs`'s re-export: `#[derive(Message)]`/`#[derive(Resource)]` emit
  absolute `bevy_ecs::` paths. See [`plugin-api.md`](./plugin-api.md).
- **Do not link a protocol family from a plugin.** `lodestone-server-brand`
  hand-rolls a five-byte VarInt read rather than depend on `lodestone-v770`;
  the version crate is a dev-dependency of its gate only.

## What this deliberately does not do

**It does not touch the socket itself.** Outbound takes the same `ActionQueue`
every `ClientAction` uses: `dispatch_plugin_channel_outbound` queues
`SendCustomPayload` after `Monitor`, and the sim's `drain_action_queue` carries
it to the version adapter. The `minecraft:brand` our *client* announces travels
a different, already-wired path: `ClientAction::SendBrand`, produced by
`lodestone_client::driver` on entering `Configuration`. Note that gating means
legacy families never send either, since v47/v340/v735 never enter
`Configuration`.

**It does not change `route()`.** `lodestone_model::event::route` sends
`CustomPayload` to `Route::NOWHERE` and that stays true: nothing in the
ingest/session/shell pipeline consumes arbitrary plugin data. The plugin's stream
is the game-event bus, which is not part of `route`'s four consumers.

**It does not replace `lodestone_client::ChannelRegistry`.** That is the same
issue's *embedder*-facing half: a passive fold a caller who already owns the
event stream drives by hand, deliberately not wired into `Driver`. A plugin has
no such call site, which is why it could not use it — `ChannelRegistry` was
constructed nowhere outside its own tests. The two coexist; neither is layered on
the other.

**The server side is wired at the wire level.** Issue #335 added
`ServerBound::CustomPayload`, the defaulted
`ServerProtocol::encode_custom_payload` seam method, and the
`lodestone-server` registry/dispatch in `crates/lodestone-server/src/
plugin_channels.rs` — so a real client's serverbound `custom_payload` no longer
lands in `Ignored`. What still is **not** wired is the plugin-facing API on top
of that registry: reaching a plugin's `MessageReader<T>` from a registered
[`PluginChannelHandler`](https://docs.rs/lodestone-server/latest/lodestone_server/trait.PluginChannelHandler.html)
is issue #77's job. See [`server-plugin-channels.md`](./server-plugin-channels.md).

## Configuration

None. No env vars, no features. `add_plugin_channel` and
`add_outbound_plugin_channel` are the whole surface.

## Dependencies

`lodestone-ecs` (`events` for `GameEvent`/`GameEventBusPlugin`, `plugin_message`
for registration and aging, `player::ActionQueue` for outbound egress,
`sets::EventPriority` and `schedules::GameTick` for ordering) and
`lodestone-model` for `ClientEvent`/`ClientAction`/`ResourceKey`. No protocol
family, on either side of the seam.
