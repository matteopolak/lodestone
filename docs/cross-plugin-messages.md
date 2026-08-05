# Cross-plugin custom messages

## What it is

The pattern a plugin uses to publish its own message type so an unrelated plugin can subscribe
**without depending on the publisher's crate** — Bukkit's `pluginManager.callEvent(MyOwnEvent)` plus a
listener in a plugin that has never heard of the publisher. `lodestone_ecs::plugin_message` carries the
one piece of machinery it needs, and three toy crates under `crates/plugins/` prove it end to end.
Issue #107.

## How it works

### What was actually missing

Issue #107's own analysis is correct, and it means this is mostly a *convention* problem. A native
plugin is an `impl bevy_app::Plugin` added at compile time, so any plugin can already
`#[derive(Message)]` a type and another can read it with `MessageReader` — `bevy_ecs` does not restrict
this. What was missing was the pattern, an example proving it, and one real ergonomic blocker.

### The three-crate shape

```text
      lodestone-shop-api        <- the public message type, and nothing else
        ^              ^
        |              |
  lodestone-shop   lodestone-shop-stats
   (publisher)       (subscriber)
```

The subscriber depends on **`-api`**, never on the publisher. `lodestone-shop-stats/Cargo.toml`'s
`[dependencies]` contains `lodestone-shop-api` and no `lodestone-shop`, and
`crates/plugins/lodestone-shop-stats/tests/cross_plugin_message.rs` proves the message still arrives.

An `-api` crate is cheap — one message type, one registration plugin, no logic — and it is the unit a
third party actually wants to depend on.

**The alternative issue #107 floats, one shared `lodestone-plugin-messages` crate everybody opts into,
is deliberately not what landed.** It would be a single file every plugin author has to get their type
merged into, which is the opposite of "without a compile-time dependency". Per-family `-api` crates need
no coordination at all.

### The one real blocker: duplicate registration

`bevy_app` **panics** on a duplicate `add_plugins` — measured, not assumed:
`Error adding plugin …: plugin was already added in application`. So both the publisher and the
subscriber registering the type they share is a startup crash, and **neither side can know whether the
other is installed** — which is exactly the situation cross-plugin messaging exists for. A subscriber
installed *without* its publisher still needs `Messages<T>` to exist, or its `MessageReader<T>` panics
on a missing resource.

Two things make it work, and both are load-bearing:

- `PluginMessageAppExt::add_plugin_message::<T>()` checks `is_plugin_added` first, so every interested
  party can declare the type and the first one wins.
- A family's `-api` plugin returns `is_unique() == false`, so `add_plugins(ShopApiPlugin)` from two
  different plugins is not a duplicate.

Nobody has to document "the publisher registers it" — a rule that fails the moment a subscriber is
installed alone.

### Writing one

```rust,ignore
// ---- lodestone-shop-api/src/lib.rs : the published contract ----
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::prelude::Message;
use lodestone_ecs::plugin_message::PluginMessageAppExt;

#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShopPurchase { pub item: u32, pub coins: u32 }

#[derive(Debug, Default)]
pub struct ShopApiPlugin;

impl Plugin for ShopApiPlugin {
    fn build(&self, app: &mut App) { app.add_plugin_message::<ShopPurchase>(); }
    fn is_unique(&self) -> bool { false }   // both halves add it
}
```

The publisher writes with `MessageWriter<ShopPurchase>`, the subscriber reads with
`MessageReader<ShopPurchase>`, and the two order against each other with `EventPriority` — the
cross-plugin anchor published from `lodestone-ecs` precisely so two crates that have never heard of
each other can both name it (`crates/lodestone-ecs/src/sets.rs`).

### Aging is not optional

`bevy_ecs`'s `Messages<T>` needs periodic `Messages::update()` or it grows without bound.
`crates/lodestone-ecs/src/events.rs` already learned this for `GameEvent`; the same applies to every
plugin-defined message, so `PluginMessagePlugin` carries that aging system generically at
`TickSet::Send` (last in `GameTick`) rather than leaving each plugin author to rediscover it. A reader
in `Update` or `Extract` inherits the caveat `events.rs` documents: it still works, but the buffer is
only trimmed on `GameTick`'s cadence.

## How to change it, and the gotchas

- **`lodestone-shop-api` starts with `lodestone-shop`.** A `contains("lodestone-shop")` check on a
  manifest reports the *required* dependency as the *forbidden* one — a gate failing in the
  safe-looking direction. `tests/dependency_direction.rs` compares dependency keys for exact equality
  for this reason, and has a test whose only job is to pin that distinction.
- **A dependency-direction claim rots silently.** Someone adds `lodestone-shop` to `[dependencies]` to
  reach one helper, every behavioural test still passes, and the property the crates exist to
  demonstrate is gone. That is why the direction is a *test* and not a comment.
- **`tests/dependency_direction.rs` needs its own control.** An always-empty parser would make "no
  forbidden dependency" vacuously true, so `the_parser_does_find_the_publisher_under_dev_dependencies`
  requires it to find `lodestone-shop` under `[dev-dependencies]` and nowhere else.
- **Know what that gate is blind to.** It reads two manifests and answers one question. It cannot see a
  longer transitive chain through a new crate, says nothing about the rest of `crates/plugins/`, and
  does not check that the message arrives (that is the other test file). `cargo xtask check-isolation`
  is the reachability-shaped tool; a plugin's own test is not.
- **Derive macros need `bevy_ecs` as a direct dependency.** `#[derive(Message)]` emits absolute
  `bevy_ecs::` paths, so a message crate needs `bevy_ecs = { workspace = true }` in its own manifest,
  not just `lodestone-ecs`. Same trap `docs/plugin-api.md`'s "how to change it" documents for
  `Resource`.
- **Native tier only.** Two guest WASM modules cannot share a Rust type, so none of this applies to the
  WASM host — that needs its own cross-plugin messaging story, tracked separately in this epic. Stated
  rather than assumed to generalise.

## Configuration

None. A server owner adds whichever plugins they want, in any order; the toy crates check both orders.
None of the three is wired into the shipped client, the same status as `crates/plugins/lodestone-nav`,
`lodestone-autopilot` and `lodestone-event-logger` (`docs/plugin-api.md` §Configuration — there is no
plugin-loading mechanism yet).

## Dependencies

`crates/lodestone-ecs/src/plugin_message.rs` needs only `bevy_app`, `bevy_ecs` and this crate's own
`GameTick`/`TickSet`. The three toy crates depend on `lodestone-ecs`, `bevy_ecs`, `bevy_app` and — for
the two halves — `lodestone-shop-api` by path. No root `Cargo.toml` edit was needed: `crates/plugins/*`
is a workspace glob.

## See also

- [`docs/plugin-api.md`](./plugin-api.md) §"The plugin event bus and cross-plugin priority ordering" —
  `GameEvent` and `EventPriority`, the *built-in* event bus this is the plugin-defined counterpart of.
- [`crates/plugins/README.md`](../crates/plugins/README.md) — what belongs in `crates/plugins/`.
