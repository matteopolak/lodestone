# Plugin framework: the capability audit

## What it is

The capability contract for native `bevy_app::Plugin` extensions and the sandboxed WASM tier, concentrating on what is still open: whether a real Bukkit, Paper or Fabric extension can be ported with its required behaviour intact. Architecture is in [architecture](../architecture.md) and [plugin API](../plugin-api.md); this records dependency order, permanent ceilings and observable completion gates.

## How it works

### Auditing a capability

Check the real tree, not a design doc. A capability is done only when a shipped application reaches it, partial when the missing reach or behaviour is named, a gap when the primitive is absent, and a ceiling when the contract excludes it. Update this with [plugin API](../plugin-api.md) when one changes. Sources: `crates/lodestone-ecs/src/{sets,schedules,player,session}.rs`, `crates/lodestone-model/src/{adapter,action}.rs` and consumers like `crates/plugins/lodestone-nav`. `TickSet` orders `Input, Intent, Physics, Predict, Animate, Send`; `LookIntent` is insert-to-take-control, remove-to-release.

### Shipped (native unless noted)

Typed events (`GameEvent(ClientEvent)` read via `MessageReader`), raw packet observation (`RawPacketBusPlugin`, bounded `OutboundRawPacketBusPlugin`; observation-only, see [plugin packet decorators](../plugin-packet-decorators.md) for the version-locked mutation route), `ActionVetoes` cancellation, `EventPriority` with monitor phase (monitor rejects mutable `World`; deferred `Commands` is the boundary to test), `TaskScheduler` and `AsyncTaskPool`/`ServerTaskScheduler` hand-back, block queries and bulk edits (`lodestone-worldedit`), entity mutation, spawn and despawn (negative plugin ids versus non-negative wire ids), server AI goals (`SimMob::add_goal`), custom items and recipes, `CraftingStationHooks` (Allow/Deny/Replace), `ChunkGenerator`/`DimensionRegistry`/`place_structure_live`, config and data directories, `PluginKeybinds`, `CameraOverride`, and in the integrated shell plugin commands, argument types, per-node permissions and the permission store (dotted nodes, wildcards, groups, specificity precedence). WASM: `lodestone-wasm-host` with fuel, memory, filesystem-root and trap gates; the curated `wit/lodestone-plugin.wit` ABI (events, actions, typed command schemas, `log`, confined `fs:read`/`fs:write`, task scheduling); discovery of cwd-relative `plugins/` through the real shell `Sim` (browser excluded, Wasmtime is native-only).

### Open work

| capability | state | what remains |
|---|---|---|
| Plugin-defined events | partial | A convention and worked example (a bevy `Message` already works for static plugins). |
| Commands, arguments, permissions on the dedicated server | gap | Route `CommandRegistry`/`PluginCommand` instead of `CommandDispatch::none()`, expose argument and suggestion surfaces, make `PluginCommand::permission`/`require_permission` reachable, and add a permission surface there. |
| Command composition | blocked | A server command dispatcher sharing its argument-type library with plugin commands. |
| Permission-provider delegation | gap | A resolver-trait seam so one plugin supplies another's decision. |
| Block writes on the server | partial | Drain the neighbour-physics pass; replicate direct plugin writes to connected players. |
| Custom entity types | partial | Shared server registry for stable custom-type identity (client disguises work). Add a spawn-objection seam only for a concrete plugin. |
| Attributes and item components | partial | Prove wire visibility of attribute writes; audit the component write path. |
| Remote custom menus | gap | Server container model and container-open packet reach (`Menus::open_local` is one local menu). |
| Crafting hooks | done | Isolate hook failures before calling the connection task robust. |
| Plugin metadata | partial | `EntityDataStore`/`ChunkDataStore` are in-memory; `lodestone-plugin-support::durable_data::PluginDataStore` has bounded, versioned, generation-qualified records and snapshot/restore, but world and player save paths do not use it yet. |
| Outbound action filtering | partial on server | `EgressFilters` runs at `ActionQueue` drain; five direct `send_action` sites bypass it for wire ordering, and `egress_hook_coverage.rs` must enumerate exactly those five. |
| World-space drawing | partial | `ExtractSet::Debug`/`DebugLines` are a precedent, not a general draw API. |
| Native manifest, dependencies, load order | gap | Ordering and soft-dependency conventions (install stays a Cargo dependency plus rebuild). |
| Native failure isolation | open design | A caught panic can leave `World` partly mutated: fail the process or provide a transactional boundary. |
| Versioned plugin ABI | partial | Turn the prose policy in `plugin-api.md` into enforced compatibility checks. |
| WASM ABI breadth | narrow | Broader block and entity coverage before claiming parity. |
| Reentrancy | partial | `EcsHandle::hold_read`/`hold_write` turn some deadlocks into panics, but the ledger cannot see direct guard acquisition. `lodestone-plugin-support::reentrancy` supplies the watchdog and dependency-graph harness; extend the unrepresentable-by-construction boundary to every entry point so an `Arc` clone cannot bypass it, and settle the outbound-action shape (`ActionQueue` versus `MessageWriter<SendAction>`) before adding send paths. |

Permanent ceilings: render-pipeline replacement (`lodestone-render` has no bevy dependency and plugins get no `wgpu::Device`), native hot reload (no stable Rust component ABI; changed `TypeId`s invalidate queries), observation-only version-free packet access, version-locked internal crate access (compile-time choice, not a dynamic API), and region-sharded plugin scheduling (plugins keep one `World`, one ordered `GameTick`, one 20 Hz accumulator; internal server parallelism must not change the single-writer contract).

### Port feasibility

| archetype | verdict | required next |
|---|---|---|
| Protection | integrated shell only | Dedicated command and permission reach, persistent region data. |
| Minigame | integrated shell only | Same, plus remote menu opening for kit and lobby UI. |
| Economy | in-memory integrated shell only | Custom-event convention, dedicated reach, restart persistence. |
| World editor | local/singleplayer | Replicate plugin block writes to remote players. |
| Anti-cheat, server-visible disguises | version-locked escape hatch only | Keep the compiled-in, unsandboxed cost explicit. |
| HUD mod | input ready, drawing not | General draw-buffer API. |
| Pathfinding bot | native tier ready | Stay native (resumable multi-tick search does not fit stateless WASM calls). |

### Dependency order

1. Extend the reentrancy boundary across every entry point and define event conventions (correctness prerequisites).
2. Finish cancellation, priority ordering and monitor enforcement, proven by a protection or minigame plugin.
3. Give the dedicated server command, permission, block-write, entity, inventory and persistence reach, each demonstrated from a real remote client, not an integrated-shell test.
4. Expand the WASM ABI once native semantics are stable (the sandbox is a separate tier, not a substitute).

## How to change it

Place a new extension point in the capability family it consumes, state whether it is native, WASM, integrated-shell or dedicated-server reachable, and add an observable consumer gate. Do not mark a helper done until a shipped application invokes it. Update the port-feasibility table when an archetype's capability union changes.

## Configuration

Native plugins are Cargo dependencies linked into the application (`lodestone-ecs`, `lodestone-server`, and a version leaf crate only when accepting a version lock). WASM plugins use `plugin.toml` and `PluginHost` policy; fuel, memory and filesystem-root limits govern the sandbox, and `fs:read` is not granted by default.

## Dependencies

`wit/lodestone-plugin.wit`, `wasmtime`, `wit-component`; `lodestone-ecs` for native scheduling and reentrancy. See also [plugin API](../plugin-api.md), [plugin packet decorators](../plugin-packet-decorators.md), [architecture](../architecture.md), [autonomous navigation](../autonomous-navigation.md) and the [roadmap index](./README.md).
