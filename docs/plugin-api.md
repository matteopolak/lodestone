# The plugin API

## What it is

The surface a third-party plugin uses to extend Lodestone: a native, compiled-in `bevy_ecs` plugin tier with the same power as internal engine code, and a sandboxed WASM host that loads capability-gated guests from disk at runtime.

There is no separate "internal API": refusing a capability to plugins refuses it to engine code. The only privileged internals are the network socket/driver task and the GPU device, queue and pipelines.

## How it works

### Two tiers

| | native `bevy_ecs` plugin | WASM host |
|---|---|---|
| power | everything native code can do | curated capability ABI: events in, actions out |
| trust | fully trusted, unsandboxed | untrusted-safe, capability-gated |
| loading | `App::add_plugins`, compiled in | a `.wasm` dropped in a directory |
| filesystem / network | unrestricted | denied unless granted |
| stability | pinned to the workspace `bevy_ecs` | WIT-defined ABI, versioned independently |

Work needing a resumable off-thread search over an owned snapshot (a pathfinder) belongs on the native tier: every per-tick query across the WASM boundary is a host call.

### Registration (native tier)

`lodestone_app::client_app()` returns an unfinalised, renderer-free, version-free `App`. A consumer adds plugins to it, then hands it to a runner: headless via `spawn_session` and `lodestone_client::ClientBuilder::ecs`, or rendered via `Sim::client_app()` plus `Sim::from_app`. For the windowed game, a downstream crate composes from `Sim::client_app()` and calls `lodestone_shell::run_with_app(app, config)` instead of `run(config)`; only `Mode::Window` accepts an `App` this way.

- A plugin belongs in `client_app()` only if it is version-free and renderer-free; anything needing shell-internal block/net/gpu types goes in `Sim::client_app()`.
- `client_app()` installs plugins, never session-scoped resources (chunk store, mesh pool, version adapter); the runner inserts those after adoption.
- The session entity is spawned once via `lodestone_app::spawn_session`; a reconnect re-inserts the whole component set so nothing leaks from the old session.

### Schedules and ordering anchors

| schedule | cadence | public sets, in order |
|---|---|---|
| `NetIngest` | once per driver iteration | `IngestSet::{Drain, Apply, Index}` |
| `GameTick` | 20 Hz, at most 10 catch-up | `TickSet::{Input, Intent, Physics, Predict, Animate, Send}` |
| `Update` | per frame | `FrameSet::{Input, Interpolate, Camera}` |
| `Extract` | per frame, last | `ExtractSet::{Terrain, Entities, Debug, Hud}` |

Plugins order against sets, never system functions, so internal systems stay renameable. `FrameSet::Camera` is empty; to take over the drawn frame insert `CameraOverride { position, yaw, pitch }` (remove it to hand back). It affects pixels only, not collision or audio. `lodestone-key-toggle`'s `CameraTogglePlugin` is the reference consumer. `EventPriority` orders two third-party plugins against each other.

### The intent doctrine

Every player-verb seam (`MovementIntent`, `LookIntent`, `BreakIntent`, `PlaceIntent`) follows five clauses; copy them for new verbs.

1. **Observation vocabulary, never wire vocabulary.** `BreakIntent { pos, face }` carries what a mouse ray carries, with no sequence number or dig state.
2. **One system owns each machine.** Dig and placement state machines, the prediction counter and the break cooldown are private to the shell's `drive_mining`/`drive_placement`. Plugins depend on `lodestone-ecs`, not the shell.
3. **Refusal is observable.** `BreakOutcome`/`PlaceOutcome` are always present with typed rejections. `PlaceOutcome::generation` bumps per resolved attempt.
4. **Human input outranks installed intent, per verb**, with no handshake.
5. **Lifecycle encodes verb shape.** A dig is continuous (`BreakIntent` stays until removed); a placement is one-shot (the shell removes `PlaceIntent` when an attempt resolves).

`MovementIntent(MovementInput { forward, strafe, jump, sneak, sprint })` is analog and never clamped before physics. `LookIntent { yaw, pitch }` is separate from the camera, applied before physics reads yaw, and absent by default. `SelectSlotIntent(0..=8)` is one-shot with no outcome component; `lodestone-hotbar-lock` is its reference consumer, and `lodestone-block-jobs` is the reference producer for break/place.

Gap: a human dig does not populate `BreakIntent`/`BreakOutcome`, so a plugin cannot observe it, but `drive_mining` consults `ActionVetoes` for both human and intent digs, so a protection plugin's `Deny` still stops a real player.

### What stays privileged

- **Version types** never cross into the crates a plugin depends on; reach them only by depending on a version crate directly, accepting version-locking.
- **The GPU** is never in the ECS. A plugin draws through Extract-time channels, never a `wgpu::Device`. Texture substitution is solved separately by resource packs.

### Reading and writing state

| kind | examples | notes |
|---|---|---|
| entity, local-player and session/HUD components | `Position`, `Health`, `PhysicsState`, `SelectedSlot`, scoreboard, boss bar | see [Player simulation](./player-simulation.md) |
| chunk world | `ChunkWorld` (read), `ChunkWorldWrite` (only write route) | `Clone` handle over one `lodestone_world::World` |
| block read/write | `block_state_at`, `set_block_with_physics`, `fill_region` | see bulk edits below |
| outbound intent | `ActionQueue(Vec<ClientAction>)`, drained each tick | never push `BlockAction`/`UseItemOn` with a fabricated prediction sequence |
| resources | `WorldTime { age, time_of_day }` | |

Per-entity attribute writes and AI-goal manipulation are not reachable from a client plugin: there is no client-to-server attribute packet and goals are server-only state.

### Draw channels and input

`DebugLines` and `PluginBillboards` are resources a system in `ExtractSet::Debug` appends to each frame (cleared before the next). A billboard is camera-facing with a tint or an atlas name. `PluginKeybinds` claims a physical key in `Consume` mode (nothing below sees it) or `Observe`; open chat/menu/container outranks a claim, and a claim outranks a gameplay binding. `PhysicalKey` is a plain string so `lodestone-ecs` never depends on winit.

### Events

`GameEvent(pub ClientEvent)` is a bevy `Message` read with `MessageReader<GameEvent>`. It is gated by a marker resource and written with no `match`, so a new variant cannot miss the bus.

`EventPriority::{Lowest, Low, Normal, High, Highest, Monitor}` mirrors Bukkit and is chained into all four schedules. A native Monitor system must pass `assert_monitor_system_is_read_only` (rejects mutable `World` access and `Commands`). WASM manifests enforce the same: a `monitor` guest cannot request `act:*`, `world:write`, `veto:actions` or `commands:register`, and a malformed monitor manifest is rejected before compile.

`RawPacketBusPlugin` and `OutboundRawPacketBusPlugin` give opt-in, observation-only raw packet streams (the outbound one has per-tick packet and byte bounds and drops only the observer copy when full). Mutate/cancel/inject at the wire was rejected: inbound events apply under the world write guard, so an interceptor needing `&mut World` would reintroduce reentrancy, and outbound mutation exists only inside a version-typed adapter. Use `ActionVetoes`/`EgressFilters` ([`packet-wiring.md`](./packet-wiring.md)), or depend on a version crate ([`plugin-packet-decorators.md`](./plugin-packet-decorators.md)).

### Cross-plugin messages and channels

Use a three-crate shape: `my-thing-api` (message type and a registration plugin), `my-thing` (publisher), and subscribers depending on `-api` only. `add_plugin_message::<T>()` checks `is_plugin_added` first and the `-api` plugin returns `is_unique() == false`, so two crates adding it do not panic. Messages age at `TickSet::Send`.

`add_plugin_channel::<T>()` specialises this to `custom_payload`: implement `PluginChannel` with a `CHANNEL` string, read with `MessageReader<T>`. Inbound runs before `EventPriority::Lowest`, outbound after `Monitor`. It installs the game-event bus itself and panics at build on a malformed channel string. A `decode` returning `None` is not an error. `add_outbound_plugin_channel::<T>()` caps a tick at 64 messages, 256 KiB total and 64 KiB per body (`add_outbound_plugin_channel_with_limits` to tighten); rejected bodies never enter `ActionQueue`, and `OutboundPluginChannelState::stats` counts them.

Server side, `lodestone_server::plugin_channels::{PluginChannelRegistry, PluginChannelHandler}` expose `register`, `dispatch` and `broadcast`; `LanConfig::plugin_channels` carries one registry into every accepted connection.

### Commands

A plugin fills the `CommandRegistry` resource in `Plugin::build` with an arena-shaped tree (`PluginCommand::new(name)`, nodes addressed by `NodeId`). Registration rejects a duplicate root or alias and a tree with no handler. Dispatch strips `/`, resolves aliases, parses through a permission filter, and walks the parsed path backwards for the nearest handler.

A denied node hides its whole subtree. It fails `dispatch` loudly naming the node, but is silently absent from tab-completion. A missing `Permissions` resource is a hard error. Argument primitives live in `lodestone-command`; `player_argument` is lenient, `choice_argument` strict.

### Scheduler and async work

`TaskScheduler::schedule_once(delay_ticks, f)` and `schedule_repeating(delay, period, f)` return a `TaskId`; `cancel(id)` removes it. Closures take `&mut World` (sound: `run_due_tasks` is an exclusive system) and must be `Send + Sync`. `schedule_once(0, f)` and `(1, f)` both fire next tick, and tasks scheduled from a callback start next tick. It anchors at `TickSet::Input`.

`AsyncTaskPool::spawn_with_handback(work, |result, world| ..)` runs the hand-back on the tick thread; `spawn(work) -> PendingTask<T>` is a component polled with `try_take()`. The off-tick closure takes no parameters, so nothing hands it a lock. Capturing an `EcsHandle` and blocking from a pool worker panics via the reentrancy ledger; a raw `handle.read()` or an unmarked thread still hangs. On wasm32 both run inline (`runs_work_inline()`).

### Persistent data

`lodestone-plugin-support` provides a per-plugin data directory with typed JSON config (missing or corrupt loads as `T::default()`), and namespaced in-memory stores `EntityDataStore`/`ChunkDataStore` of opaque `serde_json::Value` (never a static field list, which silently drops unlisted fields). The live stores have no automatic eviction on despawn; a plugin must clear its entries on a despawn `GameEvent`.

`durable_data::PluginDataStore` is the storage-neutral durable half:

- Key: validated plugin namespace plus bare key plus scope (plugin, world UUID, player UUID, or entity UUID with lifecycle generation, so a reused runtime id cannot read a prior occupant).
- Record: opaque blob plus schema version. Limits: 1 MiB per blob, 256 UTF-8 bytes per namespace or key, 16,384 records per restore, 16 MiB JSON snapshot.
- `snapshot`/`restore_entries` are the backend boundary; `unload_scope` drains memory without deleting; `migrate` allows monotonic schema upgrades only. `load_snapshot_file`/`save_snapshot_file` write via a synced temp sibling and rename. Server save-path wiring is not done.

The WASM host's `PluginHost::with_filesystem_root(parent)` gives each plugin an isolated `<parent>/<plugin-name>` directory (the shell uses `<lodestone-auth-data>/plugins`). Writes are capped at `MAX_PLUGIN_FILE_BYTES` (1 MiB), atomic, cannot follow symlinks or escape the directory; an empty write deletes. The `data:persistent` import (default-denied) exposes `get`/`set`/`delete` over the same scopes with 16-byte identities and persists to a `lodestone-plugin-data.json` sidecar when a root is set.

### Bulk world edits

`lodestone_world::World` offers `block_state_at`, `set_block_with_physics(x, y, z, state, physics)` (returns the previous state for undo; `physics: true` queues the six neighbours for an update pass that does not exist yet), and `fill_region`/`fill_region_capturing`, which group writes by chunk column. `crates/plugins/lodestone-worldedit` is a worked second plugin on this API: `EditSession` holds a `ChunkWorldWrite` plus capped undo/redo sharing one replay helper, and `WorldEditPlugin` drains queued fills once per `GameTick`.

### The WASM plugin host

`crates/lodestone-wasm-host` embeds `wasmtime`, loads a component from disk, and drives it through a WIT ABI.

```
plugin.toml --parse--> Manifest --requested capabilities--+
plugin.wasm --sniff--> component ----------------------+  |
                                          PluginHost::load_file
                          Linker gets ONLY the granted imports
Messages<GameEvent> --lift--> list<event> --> guest.on-tick --+
host tick --> due guest tasks --> guest.on-task --------------+--> list<action> --lower--> ActionQueue
```

- The WIT `event`/`action` variants are a curated subset of the intent vocabulary; none hands out a `World` borrow. Guests return actions from `on-tick(events)` and `on-task(id, token)`, so one native conductor stays the single writer of `ActionQueue`. One conductor drives all guests in manifest `priority` then name order.
- **Import capabilities** (`fs:read`, `schedule:tasks`) are enforced by the `Linker`: the interface is absent, so an ungranted guest fails to instantiate. **Data-flow capabilities** (`observe:chat`, `act:chat`) are enforced by conductor code (events not lifted, actions refused, counted and logged). Anything dangerous must be an import.
- `on-verdict(context)` is the synchronous cancellation half, returning `allow` or `deny` for each action veto via `ActionVetoes`; first denial stops dispatch. A trap, fuel exhaustion or unrepresentable context denies and unloads the guest. Gated by `veto:actions`.
- `act:chat` is chat text only; commands need the default-denied `act:command`.
- `world:read` (default-denied, structural import `world-snapshot.read-blocks`): up to 128 positions per call, returning copied `option<u32>` state ids (`none` = unloaded, `some(0)` = air). The `ChunkWorld` lock is held only to copy. See [`wasm-world-snapshots.md`](wasm-world-snapshots.md).
- Narrow actions each have their own grant and carry minimal data, with the client owning protocol encoding: look/movement intent, placement, inventory click (menu slot plus button), `act:inventory-double-click`, `act:drop-selected-item`, `act:swap-offhand`, `act:release-use-item`, `act:stab`, `act:respawn`, `act:disconnect`. `observe:place` returns a generation-bounded result.
- Command registration and manifest dependencies work (a required dependency must load first). A multi-tick break claim and async equivalents are not in the ABI.
- The native windowed client installs the conductor before `WindowApp` adopts the `App` and scans `plugins/` via `PluginHost::load_directory`. Browser plugin support is out of scope since `wasmtime` cannot run in wasm32.

## How to change it

- **Ordering anchors are ABI.** Set and `EventPriority` variants are used only as labels. Adding is safe; renaming or removing breaks plugins. Add new variants to `CorePlugin`'s `configure_sets(...).chain(...)` or they carry no ordering.
- A `Resource` a plugin orders against must be `'static`-owned; a borrow-based subsystem cannot be smuggled in.
- Name-keyed data goes in a version-free function (`lodestone_model::block_physics`); state-keyed data goes behind `VersionAdapter`, since state ids renumber per protocol version.
- A plugin deriving `Resource`/`Component`/`Message`/`Plugin` needs `bevy_ecs`/`bevy_app` as direct dependencies on the same workspace entry, because the derive expands to an absolute `bevy_ecs::` path.
- A subscriber depending on the publisher instead of `-api` defeats the pattern while tests still pass.
- Prefer intents over raw `ClientAction` in `ActionQueue`.
- A WASM ABI change touches three places: the `.wit` world, the host ABI lift/lower (a new action is a compile error since the enum is exhaustive; a new event is not, as `ClientEvent` is `#[non_exhaustive]`), and a new `Capability` if none fits. Never put an import-column capability in the default policy.
- Never make the WASM host an unconditional dependency of the shell or `lodestone-app`; gate it `cfg(not(target_arch = "wasm32"))`.

## Configuration

**Native tier:** none; a plugin is a `Cargo.toml` dependency added with `App::add_plugins`. `GameEventBusPlugin`, `SchedulerPlugin`, `AsyncTaskPoolPlugin` and `PersistentDataPlugin` are opt-in and install `CorePlugin` if absent. The save owner must call `PersistentDataPlugin`'s snapshot/restore.

**WASM tier:**

- `PluginHost::new(policy)` takes a `CapabilitySet`; `default_policy()` withholds `fs:read`, `schedule:tasks`, `commands:register`, `act:look`, `act:movement`, `act:place` and `observe:place`.
- `with_fuel(n)` bounds per-tick instructions (fuel, not epoch preemption, which needs a watchdog); `with_memory_limit(n)` bounds memory; `with_filesystem_root(p)` is required in addition to `fs:*` grants.
- Each plugin is a subdirectory with `plugin.toml` (capabilities, event kinds, priority). A plain core wasm build suffices; the host encodes it into a component.
- The shell reads the cwd-relative `DEFAULT_PLUGIN_DIR` (`plugins/`); a missing directory means no plugins, and an invalid child is logged and skipped. Embedders call `lodestone_shell::wasm_plugins::install_from_directory`; a pre-installed `WasmHostPlugin` stays authoritative.

## Dependencies

- `lodestone-ecs`: `bevy_app`, `bevy_ecs` (no default features, `std`, never `multi_threaded`), `parking_lot`, `lodestone-model`, `uuid`. Never a version crate (enforced workspace-wide).
- `lodestone-plugin-support`: `lodestone-auth`, `lodestone-ecs`, `lodestone-world`, `serde`/`serde_json`.
- `lodestone-wasm-host`: `wasmtime` (pinned minor, no default features, not `wasmtime-wasi`), `wit-component`, `toml`/`serde`. The arrow points host to ECS only. Guest crates need `wit-bindgen` and the vendored `.wit` and are workspace-excluded.
- `cargo xtask check-connected` treats `lodestone-ecs` as non-allowlisted, so an unconsumed component set shows red.
- Related: [`architecture.md`](./architecture.md), [`autonomous-navigation.md`](./autonomous-navigation.md), [`plans/runtime-plugin-loading.md`](./plans/runtime-plugin-loading.md).
