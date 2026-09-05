# Plugin framework: the capability audit

## What this is

This roadmap records the capability contract for native `bevy_app::Plugin` extensions and
the sandboxed WASM tier. It distinguishes the ECS substrate from the harder question:
whether a real Bukkit, Paper, or Fabric extension can be ported with its required
behaviour intact. The supporting architecture is described by
[`../architecture.md`](../architecture.md) and [`../plugin-api.md`](../plugin-api.md).

The tracker records ownership and status. This document records the durable work
decomposition, dependency order, permanent ceilings, and observable completion gates.

## How to audit a capability

Check the real tree, not only a design document. A capability is **done** only when a
shipped application reaches it; **partial** when the missing reach or behaviour is named;
**gap** when its primitive is absent; and **ceiling** when the contract intentionally
excludes it. Update this document with [`../plugin-api.md`](../plugin-api.md) whenever a
capability changes.

The audit sources are the public API docs, `crates/lodestone-ecs/src/{sets,schedules,player,session}.rs`,
`crates/lodestone-model/src/{adapter,action}.rs`, and real consumer plugins such as
`crates/plugins/lodestone-nav`. `TickSet` provides the ordered
`Input, Intent, Physics, Predict, Animate, Send` phases; `LookIntent` provides the
insert-to-take-control/remove-to-release convention for plugin movement control.

## Capability inventory

### Events and scheduling

| capability | status | completion gate or remaining work |
|---|---|---|
| Typed event subscription | done (native) | `GameEvent(ClientEvent)` is a bevy `Message`, read through `MessageReader<GameEvent>`; every `ClientEvent` variant reaches the single write site. |
| Raw inbound packet observation | done (native) | `RawPacketBusPlugin` publishes an opt-in `RawPacket` message with the connection state, packet id, and exact payload before version-specific decoding. The version-locked route remains documented in [`../plugin-packet-decorators.md`](../plugin-packet-decorators.md). |
| Action cancellation | done (native) | `ActionVetoes` asks a priority-keyed predicate before effects are computed; first `Deny` wins for break, place, damage, inventory click, movement, and interaction. |
| Priority and monitor phase | done (native), bounded | `EventPriority` chains all public schedules. Monitor rejects mutable `World` access; deferred `Commands` mutation remains the boundary to document and test. |
| Plugin-defined events | partial | Define a convention and provide a worked example; a bevy `Message` already works for statically linked plugins. |
| Delayed and repeating tasks | done (native) | `TaskScheduler::{schedule_once, schedule_repeating, cancel}` runs exactly from `run_due_tasks` in `TickSet::Input`. |
| Off-tick work with hand-back | done (native) | Client plugins use `AsyncTaskPool::{spawn, spawn_with_handback}` (inline on `wasm32`); native server plugins use `ServerTaskScheduler::spawn_with_handback` with bounded hand-back admission. |

### Commands and permissions

| capability | status | completion gate or remaining work |
|---|---|---|
| Plugin command registration | done in the integrated shell; gap on dedicated server | Route `CommandRegistry` and `PluginCommand` through the dedicated server instead of `CommandDispatch::none()`. |
| Argument types and suggestions | done in the integrated shell; gap on dedicated server | Expose the existing `lodestone-command` argument and suggestion surface through the dedicated server. |
| Per-node permissions | done in the integrated shell; gap on dedicated server | Make `PluginCommand::permission` and `require_permission` reachable in the dedicated process. |
| Command composition | blocked | Build the server command dispatcher and share its argument-type library with plugin commands. |
| Permission nodes and resolution | done in the integrated shell; partial on dedicated server | `PermissionStore`, `PermissionRegistry`, and `PermissionResolver` provide dotted nodes, wildcard matching, defaults, groups, inheritance, and specificity/tier/negation precedence. Add a dedicated-server surface. |
| Permission-provider delegation | gap | Introduce a resolver-trait seam so one plugin can supply another plugin's permission decision. |

### World, entities, and inventories

| capability | status | completion gate or remaining work |
|---|---|---|
| Block queries | done | `VersionAdapter::{block_collision, block_name, block_outline, block_interaction}` and `lodestone_model::block_physics` are the version-safe read surface. |
| Block writes | done in the integrated shell; partial on server | Drain the queued neighbour-physics pass and replicate direct plugin writes to connected players. |
| Bulk edits | done in the integrated shell | `lodestone-worldedit` exercises `fill_region` and `fill_region_capturing`; undo/redo remains plugin-owned. |
| Custom generation, dimensions, and structures | done on server, with limits | `ChunkGenerator`, `DimensionRegistry` (primary world), and `place_structure_live` are the extension seams. Terrain generation remains server-owned. |
| Existing entity mutation | done | Writable components reach the next extraction pass. |
| Spawn and despawn | done locally and server-visible | Negative plugin IDs and non-negative wire IDs prevent collisions; add a spawn-objection seam only if a concrete plugin needs one. |
| Custom entity types | partial | Client disguises work; provide a shared server registry for stable custom-type identity. |
| Attributes and item components | partial | Prove wire visibility of attribute writes and audit the item-component write path. |
| AI goals | done on server | `SimMob::add_goal(priority, Box<dyn Goal>)` is the server simulation extension seam. |
| Remote custom menus | gap | Build a server container model and container-open packet reach; local `Menus::open_local` is intentionally limited to one local menu. |
| Custom items and recipes | done in the integrated shell | `CustomItemRegistry` and `RecipeRegistryExt::add_recipe` are the extension points. |
| Crafting station hooks | done on server | `CraftingStationHooks` supports Allow/Deny/Replace for supported stations; isolate hook failures before treating the connection task as robust. |

### Persistence, packets, rendering, and lifecycle

| capability | status | completion gate or remaining work |
|---|---|---|
| Plugin metadata | partial | `EntityDataStore` and `ChunkDataStore` are in-memory. Persistence requires the world/player persistence layer. |
| Config/data directory | gap | Establish one shared convention rather than another per-plugin directory implementation. |
| Database access | done (native) | Native plugins may use normal Rust database libraries. |
| Shared packet observation | done (native), observation-only ceiling | `RawPacketBusPlugin` provides read-only, opt-in `RawPacket` messages before adapter decoding; the version-free surface cannot mutate, cancel, or inject wire data. |
| Version-locked packet mutation | done at the escape-hatch layer | A `ServerProtocol` decorator can drop, rewrite, or append directives. It is compiled into the server, unsandboxed, and version-locked; see [`../plugin-packet-decorators.md`](../plugin-packet-decorators.md). |
| Outbound action filtering | done locally; partial on server | `EgressFilters` operates at `ActionQueue` drain. Five direct `send_action` sites bypass it for wire ordering; `egress_hook_coverage.rs` must enumerate exactly those five and fail when the set changes. |
| Internal version-crate access | done, deliberately version-locked | A native plugin may depend on a version leaf crate directly. This is a compile-time compatibility choice, not a dynamic plugin API. |
| World-space drawing | partial | `ExtractSet::Debug` and `DebugLines` are a precedent, not a general drawing API. |
| Input interception | done (native) | `PluginKeybinds` supports Consume and Observe modes; open UI takes priority. |
| Camera control | done (native) | `CameraOverride` replaces only the drawn frame; `lodestone-key-toggle::CameraTogglePlugin` drives and releases a fixed pose through the composed `Sim` path, with a real `Sim::render_camera` control. |
| Render-pipeline replacement | ceiling | `lodestone-render` has no bevy dependency and plugins do not receive a `wgpu::Device`; renderer constraints remain renderer-owned. |
| Native manifest, dependencies, and load order | gap | Define ordering and soft-dependency conventions; static installation remains a Cargo dependency plus rebuild. |
| Native failure isolation | open design | A caught panic can leave `World` partially mutated. Decide whether fully trusted native plugins fail the process or provide a transaction-safe boundary. |
| Native hot reload | ceiling | Rust has no stable component ABI across reloads; changed `TypeId`s invalidate queries. |
| Versioned plugin ABI | partial | Convert the prose policy in `plugin-api.md` into enforceable compatibility checks. |

### WASM

| capability | status | completion gate or remaining work |
|---|---|---|
| Host and sandbox | done, narrow | `lodestone-wasm-host`, `PluginHost`, fuel, memory, filesystem-root, trap, and memory gates are real. |
| Capability ABI | done, narrow | `wit/lodestone-plugin.wit` has three event kinds, three actions, root command declaration and synchronous handling, `log`/`fs:read`, and delayed/repeating task scheduling with cancellation. Add typed command schemas and suggestions, block access, and entity actions before claiming feature parity. |
| Shipped application reach | done, native windowed client | The runner discovers cwd-relative `plugins/` through the real shell `Sim`; browser hosting remains excluded because Wasmtime is native-only. |

## Scheduling, reentrancy, and testability

Plugins target one `bevy_ecs::World`, one ordered `GameTick` schedule, and one 20 Hz
accumulator. Region-sharded plugin scheduling is out of scope. Internal server parallelism
may be evaluated from measurements, but it must not change the plugin-facing single-writer
and ordered-tick contract.

`EcsHandle::hold_read` and `hold_write` turn some reentrancy deadlocks into panics, but
the ledger cannot see direct guard acquisition. `lodestone-plugin-support::reentrancy`
already supplies the reusable watchdog and dependency-graph harness for plugin authors;
the bridge tests are one consumer and include a raw-read control that would otherwise wedge.
The remaining work is to extend the unrepresentable-by-construction boundary to every
plugin entry point so an `Arc` clone cannot bypass the rule. Resolve the public outbound-action shape
(`ActionQueue` versus `MessageWriter<SendAction>`) before expanding send paths.

## Port-feasibility gates

| archetype | current verdict | required next capability |
|---|---|---|
| Protection | Integrated shell only | Dedicated command and permission reach plus persistent region data. |
| Minigame | Integrated shell only | Dedicated command and permission reach, plus remote menu/container opening for kit and lobby UI. |
| Economy | In-memory integrated shell only | Custom-event convention, dedicated reach, and restart persistence. |
| World editor | Local/singleplayer | Replicate plugin-driven block writes to remote players. |
| Anti-cheat and server-visible disguises | Version-locked escape hatch only | Keep the compiled-in, unsandboxed cost explicit; dynamically loaded access remains open. |
| HUD mod | Input is ready; drawing is not | General-purpose draw-buffer API. |
| Pathfinding bot | Native tier is ready | Keep it native: resumable multi-tick search does not fit the current stateless WASM calls. |

## Dependency order and completion criteria

1. Extend the existing reentrancy harness and unrepresentable boundary across every plugin
   entry point, and define event and custom-event conventions. These are correctness
   prerequisites for new entry points.
2. Complete cancellation semantics, priority ordering, and monitor enforcement. A protection
   or minigame plugin is the integration proof.
3. Give the dedicated server command, permission, block-write, entity, inventory, and
   persistence reach. Each must be demonstrated from a real remote client, not only an
   integrated-shell test.
4. Expand the WASM ABI after native semantics are stable. The sandbox is a separate tier,
   not a substitute for native extensions.

## See also

- [`../plugin-api.md`](../plugin-api.md) — API contract and native/WASM policy.
- [`../plugin-packet-decorators.md`](../plugin-packet-decorators.md) — version-locked packet escape hatch.
- [`../architecture.md`](../architecture.md) — ECS and renderer constraints.
- [`../architecture.md`](../architecture.md) — lock discipline, tick ownership, and renderer constraints.
- [`../autonomous-navigation.md`](../autonomous-navigation.md) — native pathfinding consumer.
- [`./README.md`](./README.md) — roadmap index and track boundaries.

## How to change it

Place a new extension point in the capability family it consumes, state whether it is
native, WASM, integrated-shell, or dedicated-server reachable, and add an observable
consumer gate. Do not promote a helper or host implementation to **done** until a shipped
application invokes it. Update the port-feasibility table when an archetype's required
capability union changes.

## Configuration and dependencies

Native plugins are Cargo dependencies linked into the relevant application; their crate
dependencies may include `lodestone-ecs`, `lodestone-server`, and a version leaf crate
only when accepting a version lock. WASM plugins use `plugin.toml` and `PluginHost` policy;
fuel, memory, and filesystem-root limits govern the sandbox, and `fs:read` is not granted
by the default policy. The capability ABI depends on `wit/lodestone-plugin.wit`, `wasmtime`,
and `wit-component`; native scheduling and reentrancy depend on `lodestone-ecs`.
