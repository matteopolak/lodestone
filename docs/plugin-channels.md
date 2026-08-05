# Plugin channels (`custom_payload`)

## What it is

Typed, per-channel delivery of Minecraft's `custom_payload` (plugin-messaging)
packet to a plugin. A plugin declares a channel by implementing
`lodestone_ecs::plugin_channel::PluginChannel` on its own `#[derive(Message)]`
type, calls `add_plugin_channel::<T>()` in its `Plugin::build`, and reads decoded
`T`s with an ordinary `MessageReader<T>`. Issue #301.

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

### Two things it does on your behalf, both load-bearing

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

**It does not send.** Outbound is
`lodestone_model::ClientAction::SendCustomPayload`, which as of this writing has
**no producer anywhere in the workspace** — the `v770` encoder arm for it is
reachable only from outside the tree via `ClientHandle::send_action`. The
`minecraft:brand` our *client* announces travels a different, already-wired path:
`ClientAction::SendBrand`, produced by `lodestone_client::driver` on entering
`Configuration`. Note that gating means legacy families never send it, since
v47/v340/v735 never enter `Configuration`.

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

**The server side is not wired.** A real client's serverbound `custom_payload`
reaches `crates/protocol/v770/src/server_protocol.rs`, decodes as a
`BrandPayload`, and returns `ServerBound::Ignored` — so a vanilla client's brand
announcement is still dropped. Wiring it needs a `ServerBound` variant and a
`CommandSink`-shaped seam method in `lodestone-server`, whose module doc
(`crates/lodestone-server/src/command.rs:44-60`) already names `custom_payload`
as the next such method. Scoped as its own unit; see #301.

## Configuration

None. No env vars, no features. `add_plugin_channel` is the whole surface.

## Dependencies

`lodestone-ecs` (`events` for `GameEvent`/`GameEventBusPlugin`, `plugin_message`
for registration and aging, `sets::EventPriority` and `schedules::GameTick` for
ordering) and `lodestone-model` for `ClientEvent`/`ResourceKey`. No protocol
family, on either side of the seam.
