# Plugin registration — how a consumer gets their plugin into the client

## What it is

The seam that lets a crate outside this repo register a `bevy_app::Plugin` into Lodestone's client,
headless *or* rendered. `lodestone-app`'s `client_app()` returns the composed but unfinalised `App`;
the caller `add_plugins` on it and hands the result to a runner. This is milestone zero of
[`docs/plans/runtime-plugin-loading.md`](plans/runtime-plugin-loading.md), and every later milestone
there — the wasm host included — arrives through it.

## How it works

Three crates, one registration call:

```
lodestone-app          client_app() -> App     the six version-free plugins
   |
   +-- headless        ClientBuilder::ecs(world, session)
   |
   +-- rendered        Sim::client_app() -> App    ( = client_app() + the shell's three )
                       Sim::from_app(app, config) -> Sim
```

A headless consumer:

```rust
let mut app = lodestone_app::client_app();
app.add_plugins(MyPlugin);
let session = lodestone_app::spawn_session(&mut app, PlayerState::at(feet, yaw));
let world = std::sync::Arc::new(RwLock::new(std::mem::take(app.world_mut())));
// ... lodestone_client::ClientBuilder::new(..).ecs(world, session)
```

A consumer of the graphical client:

```rust
let mut app = lodestone::sim::Sim::client_app();
app.add_plugins(MyPlugin);
let sim = lodestone::sim::Sim::from_app(app, config);
```

`Sim::new` **is** those two calls. That is the point: the shell registers `CorePlugin` and friends
through the identical function a consumer calls, so there is no private composition path left to
drift. The owner's standing principle — there must not be an "internal API" and a "plugin API"
separately ([`plugin-api.md`](plugin-api.md)) — is satisfied structurally here rather than by
convention.

### What is composed where, and why the split is not the one the plan predicted

| plugin | crate | owns |
|---|---|---|
| `CorePlugin` | `lodestone-ecs` | the schedules and their set chains, `FrameClock`, `WorldTime` |
| `LocalPlayerPlugin` | `lodestone-ecs` | `TickSet::Physics` |
| `ControllerPlugin` | `lodestone-controller` | `TickSet::Input`, `TickSet::Send` |
| `SessionHudPlugin` | `lodestone-ecs` | `TickSet::Animate` |
| `IngestPlugin` | `lodestone-ecs` | the net thread's per-entity fold |
| `SessionPlugin` | `lodestone-ecs` | the net thread's local-player-scalar fold |
| `EntityInterpPlugin` | **`lodestone-shell`** | render-side entity interpolation |
| `TerrainPlugin` | **`lodestone-shell`** | the chunk store, mesh queues, `heal_dirty_columns` |
| `InteractPlugin` | **`lodestone-shell`** | pick target, interaction predictors, particle emitter |

The plan expected all nine to compose in `lodestone-app`, on the evidence that none of `mesher.rs`,
`interact.rs` or `entities.rs` contains a `wgpu` type. **That evidence still holds and is still not
sufficient**, because it is about the wrong axis. What blocks the move is *shell-internal* coupling:

| plugin | blocked by |
|---|---|
| `TerrainPlugin` | `crate::blocks::{ShellClassifier, id}`, `crate::net::NetClient` |
| `InteractPlugin` | fourteen items imported from `crate::sim` — a cycle with the type that adopts the `App` |
| `EntityInterpPlugin` | nothing in code; its five `crate::sim` mentions are all prose. Movable, and left in place because moving 4,700 lines in isolation buys no gate |

This is a finding worth keeping rather than a shortfall: it is why the acceptance gate is
`cargo tree`, not a grep for `wgpu`. A headless consumer therefore gets no terrain mesher, no pick
target and no render-side interpolation — correct, since all three exist to feed a renderer.

### The gates, and what they measured

| gate | where | result |
|---|---|---|
| renderer-free graph | `cargo tree -p lodestone-app -e normal,dev,build` | 329 crates, **0** matching `wgpu`/`winit` |
| — its negative control | the same command and grep against `lodestone-shell` | 448 crates, **12** matches |
| — durable guard | `crates/lodestone-app/tests/renderer_free_graph.rs` | direct-edge allowlist, with the shell's manifest as its own control |
| an external plugin reaches its goal | `crates/lodestone-app/tests/headless_consumer_registers_a_plugin.rs` | `AutopilotPlugin` registered through `client_app()` walks to `(5, 1, 0)`; control with the plugin absent moves < 0.01 blocks |
| the rendered client is driven | `crates/lodestone-shell/tests/rendered_client_takes_a_plugin.rs` | jump apex 1.25 blocks (vanilla's value); control 0.0000 |
| `Sim` unchanged | `cargo test -p lodestone-shell --no-fail-fast` | 1193 passed, 0 failed |
| version seam | `cargo check -p lodestone-shell --no-default-features` | clean |
| wasm ceiling | `just wasm-size` | gzip 844,527 B against the 1,600,000 B ceiling |

The 12-vs-0 pairing is the load-bearing part. A zero from a search is worth nothing without a
positive result from the same search, and this repo has a documented case of a search reporting
absence because the search itself was broken (`CLAUDE.md`, `rtk`).

The rendered gate's observable is worth reading before you change it: two weaker ones were run
first. `forward = 1.0` travelled 0.2000 blocks and stalled against demo-world terrain (the plugin was
already provably working — the control moved 0.0000 — but the assertion measured the fixture's
geometry); `forward + jump` travelled 6.4733 blocks while *climbing* 4.1661, so its apex predicted
nothing. Only a jump in place yields a magnitude the seam alone explains.

## How to change it, and the gotchas

**Adding a dependency to `crates/lodestone-app/Cargo.toml` is the way to break this.** The whole
property is that a headless consumer's graph stays renderer-free, and the only way it stops being so
is a new direct edge here. `renderer_free_graph.rs` fails on any name outside its allowlist and tells
you what to do; re-run the `cargo tree` measurement above either way, since the guard checks direct
edges and the property is transitive.

**A new plugin belongs in `client_app()` only if it is version-free and renderer-free.** If it needs
`crate::blocks`, `crate::net`, `crate::gpu` or `crate::sim`, it belongs in `Sim::client_app()`, added
on top. That is not a lesser tier — it is the same `add_plugins` call, one crate up.

**`client_app()` installs plugins, never session-scoped resources.** The chunk store, the mesh worker
pool, the particle sprite table and the version adapter each have to be built against something the
composer does not know: the block-id space of the world this session will hold, or the configured
protocol. Resources need no `Plugin::build`, so a runner inserts them after adoption with only the
`World` in hand — which is exactly why adoption can happen as late as it does.

**Spawn the session entity last, and through `lodestone_app`.** `spawn_session` /
`spawn_session_in` / `insert_session_component_sets` exist so the entity's component set has one
definition. There are two callers of the component-set list — a fresh spawn and a **reconnect**,
where `Sim::end_session` re-inserts both sets rather than resetting field by field — and a component
added to the spawn path but missed on reset leaks the old session into the new one. The stale
`ServerEntityId` in particular would misattribute the next session's mob effects to whichever entity
the new server assigns that id to first.

**Do not reach for a `Vec<Box<dyn Plugin>>` constructor argument.** It was considered in the plan and
rejected: it is a second registration vocabulary with less power (no plugin groups, no
`is_plugin_added` interrogation between additions), existing only to avoid exposing a type that is
already public one crate down.

## Configuration

None of its own. `Sim::from_app` honours `config.mode` exactly as `Sim::new` does — `Mode::Headless`
builds the offline demo world, everything else the live one — so it is a drop-in rather than a third
construction path with its own rules.

## Dependencies

`lodestone-app` depends on `lodestone-ecs`, `lodestone-controller`, `lodestone-physics` and
`bevy_ecs`, and on `lodestone-autopilot` + `lodestone-world` + `lodestone-model` as dev-dependencies
for the conformance gate. The autopilot edge is dev-only on purpose: it is LGPL-3.0-or-later while
the engine is MIT-OR-Apache-2.0 ([`crates/plugins/README.md`](../crates/plugins/README.md)
§Licensing), and the same reasoning is why `lodestone-shell`'s own rendered gate uses an equivalent
local plugin instead of the autopilot.

## See also

- [`plugin-api.md`](plugin-api.md) — the surface a plugin gets once registered, the five clauses of
  the intent doctrine, and the two privileged internals (the socket/driver task, and the GPU
  device/queue) that no plugin reaches.
- [`plans/runtime-plugin-loading.md`](plans/runtime-plugin-loading.md) — M0 is this document; M1–M6
  are the wasm host, all downstream of it.
- [`autonomous-navigation.md`](autonomous-navigation.md) — `lodestone-autopilot`, the conformance
  plugin, and the routes a user has to enable it.
