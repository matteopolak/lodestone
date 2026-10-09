# Plugin capability audit: what a Java plugin can do here, per side and per tier

## What it is

A capability-by-capability check of "any Bukkit/Paper/Fabric plugin is portable to this framework", with a verdict per capability for the client's native `bevy_ecs` tier, the client's WASM host and the server. The headline: the client's native tier is at or near parity except wire-level packet mutation, the WASM tier is substantially narrower, and the server has native per-tick systems on in-memory and persistent primary worlds but no event adjudication, plugin commands on the dedicated binary, or runtime plugin discovery. Since real Bukkit plugins are mostly server plugins, the answer for the deployment that matters most is "not yet".

## How it works

### Two constraints

Both tiers must express the same feature, or the WASM tier states a ceiling (a single-threaded sandboxed guest cannot host a resumable off-thread search or anything borrowing a `World`). The WASM ABI is `wit/lodestone-plugin.wit`: `on-tick`, `on-task`, `on-command`, `on-verdict` plus `log`, filesystem and scheduler imports, all capability-gated.

`EcsHandle` (`Arc<parking_lot::RwLock<World>>`) is not reentrant: nested guards deadlock silently. `lodestone_ecs::hold_read`/`hold_write` keep a per-thread ledger and panic naming both sites, but only for guards taken through them. Each capability falls in one class, strongest first: **omitted** (the plugin is handed nothing that reaches the lock, e.g. a `VetoFn` gets a `Copy` `VerbContext`), **typed closure** (no `World` parameter, e.g. `AsyncTaskPool::spawn`), **ledger** (runtime tripwire), **unguarded** (a comment asks nicely, e.g. nested `MobHandle::with`). A plugin depending only on `lodestone-ecs` has no route to an `EcsHandle`; the escape hatch is depending on `lodestone-shell` and calling `Sim::ecs()`.

### Verdicts

Legend: done, partial, gap, ceiling (by design).

| capability | client, native | client, WASM | server |
|---|---|---|---|
| observe typed events | done: `GameEvent(ClientEvent)` | partial: few event kinds | partial: plugin `Message` types via `App::add_message`; no built-in gameplay bus (`dispatch_play_packet` applies inline) |
| cancel an event | done: `ActionVetoes`, six verbs, all asked in production | done: `on-verdict`, gated by `veto:actions` | gap: `TickSet::Adjudicate` has no proposal/verdict systems; `CraftingStationHooks` is the one Allow/Deny/Replace hook |
| priority, `MONITOR` | done (`EventPriority`; monitor checked against access set) | done (manifest `priority`; monitor manifests reject write capabilities) | gap |
| delayed/repeating tasks | done: `TaskScheduler` | done: `scheduler` import | done: `ServerTaskScheduler` on the primary `GameTick` |
| async task + hand-back | done: `AsyncTaskPool` | ceiling | done: `spawn_with_handback` |
| register command, tab-complete, permissions | done: `CommandRegistry`, `PermissionStore` | partial: `commands:register` roots only; no argument schema | partial: dedicated server installs `CommandDispatch::none()`, so plugin commands and node permissions do not exist there |
| read a block | done: `ChunkWorld` | partial: `world:read`, at most 128 states | done: `ChunkSource::block_state` |
| write a block | done (`set_block_with_physics(.., true)` queues a pass nobody drains) | gap (placement via `place-block` only) | partial: `ChunkSource::set_block` is not replicated to connected players |
| custom generator/dimension/structures | ceiling | ceiling | done: `ChunkGenerator`, `DimensionRegistry`, `place_structure_live` |
| modify/spawn entities | done (spawn local-only) | gap | done: `MobHandle::with`, `spawn_mob`, `PlayerRegistry` |
| AI goals | ceiling | ceiling | done: `SimMob::add_goal` |
| custom menu | done, local-only, one at a time | gap | gap: no open-to-remote-player path; `PlayerInventory` is a connection-task local |
| custom items/recipes/station hooks | done | gap | done: `CraftingStationHooks` |
| per-entity/chunk data | partial: live stores plus `PluginDataStore` | partial: `data:persistent` records | gap: nothing plugin-keyed saved by the world save |
| config/data dir, database | done / trivial | read-only / ceiling (no network import) | done |
| packet observation | done: `RawPacket`, `OutboundRawPacket` buses | done: `observe:packets` | partial: `ServerProtocol` decorator, version-locked |
| packet mutation/cancel | partial: `EgressFilters` over `ClientAction`; five direct `send_action` paths bypass it | gap | partial: decorator can drop, rewrite or append `ServerDirective::Send` |
| raw byte injection | ceiling, permanent | ceiling | via decorator, untested |
| hot reload, panic isolation | ceiling (trusted native code) | done: `PluginHost::load_file`, trap/fuel/memory limits | ceiling |
| shipped in the binary | done | done: windowed client loads `plugins/` | compiled-in native via `dedicated_server_app`; no runtime discovery |

### Events and cancellation

Client native: the single `GameEvent` write site pushes every `ClientEvent` with no `match`, so a new variant cannot miss the bus. A `Deny` from `ActionVetoes` leaves the predictor untouched and sends nothing. All six verbs are asked at sites `crates/lodestone-ecs/tests/veto_coverage.rs` scans: block break in `lodestone_shell::interact::drive_mining` (human and `BreakIntent` path alike, so a protection plugin can veto a real player's dig; only observation through the intent components is missing), place in `drive_placement`, damage in `Sim::attack_entity`, move in `lodestone_controller::ecs::send_player_input`, inventory click in `SharedState::menu_click`, interact in `Sim::use_item_live`. `Monitor` systems get `&World` and cannot take a second guard; a deferred `Commands` mutation is the hole the access check cannot see.

Client WASM: `on-verdict` runs synchronously through the same broker with a copied context. The first `deny` wins in load order; a trap or fuel exhaustion denies and unloads that guest. `Monitor` is not enforceable at runtime.

Server: no cancellation. `dispatch_play_packet` matches `ServerBound` and calls `apply_*` inline on the connection task, and `apply_block_action` breaks unconditionally. The intended design is a proposal queue drained in `TickSet::Drain`, vetoed in `TickSet::Adjudicate` and applied in `TickSet::Apply` (`docs/plans/server-ecs-migration.md`, `docs/plugin-server-capabilities.md`). `CraftingStationHooks` runs inline on the connection task, so a panicking hook kills that player's connection.

Server locks are unguarded: `MobHandle` is a `std::sync::Mutex`, so `MobHandle::with` nested in itself (natural when `spawn_mob` is called from a goal) deadlocks with no diagnostic; `Terrain263ChunkSource.edits` is another such lock. The ledger sees only `EcsHandle`; likewise `ChunkWorld` (`Arc<RwLock<lodestone_world::World>>`, a second lock class) can ABBA-deadlock with the ECS guard.

### Scheduler

Client native: `TaskScheduler` fires from `run_due_tasks` in `TickSet::Input`, using the driver's own world guard. `AsyncTaskPool` marks worker threads so `Ledger::enter` reports a guard taken from a worker as its own defect; on `wasm32` both run inline. Client WASM: one-shot and repeating callbacks return guest-local handles; delay zero and one both mean the next host tick, period zero clamps to one, same-tick callbacks run in handle order.

Server: `ServerCorePlugin` installs `ServerTaskScheduler` and `run_server_tasks` in `TickSet::Drain`. Callbacks receive the tick task's `&mut World` (omitted class). Delays exclude `ServerBoot`, zero normalises to one, equal deadlines run in registration order. `spawn_with_handback` admits bounded worker work whose result closure runs on the tick owner before due callbacks; shutdown rejects new work. Embedders pass a `ServerApp` built with `ServerApp::bootstrap_with` to the `IntegratedServer::open_*` application leaf; the standalone binary does the same at compile time.

### Commands and permissions

`lodestone-command` is the shared tree substrate (server built-ins, plugin registry, client tab-completion). Singleplayer path: `ClientAction::SendCommand` to `ServerBound::ChatCommand` to the built-in tree, falling through to `CommandDispatch`, the shell's `EcsCommandSink` in `lodestone_shell::net`, `hold_write` on the client's `EcsHandle`, then `lodestone_ecs::commands::dispatch` against `CommandRegistry` (a `Permissions` resource is mandatory). So a "server plugin command" in singleplayer is a client plugin command; on the dedicated server there is no `World` for the sink, which is why only the five-level vanilla model plus ops/whitelist/bans exists there ([server commands](server-commands.md)). Handler reentrancy is ledger-class.

### World, entities, inventories

Client-side world editing is complete for a WorldEdit-class plugin (`crates/plugins/lodestone-worldedit`). Server-side a plugin owns the `ChunkSource` it passes to `IntegratedServer::open_*` and can `set_block` from any thread, but nothing tells connected players: the only tick-to-connection block path is `BlockTickFeed`, fed by tick systems and drained single-consumer. A protection restore or paste is invisible to everyone online until they re-receive the column; a multi-consumer change feed drained by each `ViewTracker` is the fix.

Client entity modification is plain `Query` mutation over `lodestone_ecs::entity` components; client spawn ids are strictly negative. Server-side `PlayerRegistry` carries position, rotation, game mode, experience, effects and chat/swing feeds; a player's inventory is a `serve_play` local reachable only through the `player_data` save/load path. No second plugin can object to a spawn (the adjudication window again). Client menus use `lodestone_game::menus::Menus::open_local`; the server cannot open a menu on a remote player (container-open packets and the click echo are unbuilt).

### Persistence

Per-plugin data dir and typed JSON config are shipped. `PluginDataStore` offers bounded versioned blobs keyed by plugin, world, player or entity UUID plus lifecycle generation, with deterministic snapshots; restore rejects unknown versions, duplicate keys and oversized blobs, and `unload_scope` drains memory without deleting backend data. The server save path has no adapter writing these records. `ItemComponents::custom_data` round-trips as raw network NBT on the client; whether the server's item save carries it is unverified (no `custom_data` hit in `player_data.rs` or `inventory.rs`). WASM `fs:read`, `fs:write` and `data:persistent` are linker-enforced; chunk scopes and scope-unload callbacks are not in the contract.

### Packet interception

Client packet interception is observation-only by decision. `RawPacket` carries an owned copy of the payload before decoding; `OutboundRawPacket` carries the exact adapter output after decorators and before framing. Both default to 256 packets and 1 MiB per tick per direction; exhaustion drops only the observer copy and counts it. `WasmHostPlugin` selects zero limits when no guest holds `observe:packets` ([WASM raw packets](wasm-raw-packets.md)). Neither tier can mutate, cancel, reorder or inject.

Version-locked escape hatches: on the client, `ClientBuilder::new` takes a `Box<dyn VersionAdapter>`, so a decorator around the adapter sees every packet in both directions (headless bots only; the windowed shell builds its own adapter). On the server, `IntegratedServer` is generic over `ServerProtocol`; a decorator sees every inbound payload and can drop, rewrite or append outbound `ServerDirective::Send`. It cannot inject a new inbound action since `ServerBound` is closed. That is the honest answer to "can an anti-cheat be ported": yes, native, server-side, version-locked.

### Internals escape hatch

Client native: depend on a version crate (`packets`, `adapter` are `pub`) or on `lodestone-shell` for `Sim::ecs()`; `lodestone-plugin-support::reentrancy` provides `assert_ecs_only_dependency_graph` and `assert_schedule_completes_under_write_guard`. Client WASM: none, by design. Server: embed `IntegratedServer` and hold `world_state`, `players`, `mobs`, `tickets`, `portals`, `save_now`, `level_dat`, `block_ticks`, plus your `ChunkSource` and `ServerProtocol`. The JVM tier (`crates/plugins/lodestone-jvm-bridge`) is shaped so `WorldPort` holds only a `SyncSender` and a `Duration`; a Java handler's worst case is a timeout. It has no `jni` dependency yet (see [Java plugin bridge](java-plugin-bridge.md)).

### Port feasibility by archetype

| archetype | client native | client WASM | server |
|---|---|---|---|
| protection | portable, regions lost on restart | no veto path for data | no veto, no durable data |
| economy | in-memory only | no commands/write | not on dedicated server (no commands) |
| minigame | portable | not portable | not portable |
| world editor | portable | not portable | edits land, nobody online sees them |
| anti-cheat | decided ceiling | not portable | via `ServerProtocol` decorator |
| hologram/disguise | local-only cosmetic | not portable | via the decorator |
| client HUD mod | portable (`DebugLines`, `PluginBillboards`, `PluginKeybinds`, `CameraOverride`) | no draw action | n/a |
| pathfinding bot | portable (`lodestone-autopilot`) | ceiling by cost | n/a |

## How to change it

- Re-run the census before trusting any "gap": cheap discriminators are a `Res<T>` in a shipped plugin tuple, a `VerbContext::X` constructor in a non-test file, a `run_schedule` call in a production driver.
- A server capability landing ahead of the ECS migration should use the shared `Allow`/`Deny`/`Replace`, first-non-`Allow`-wins, priority-ordered verdict shape so it can move onto `ServerProposal` unchanged.
- Never hand a callback a `World`, `EcsHandle` or anything reaching either; `VetoFn`, `EgressFilters`, `CraftingStationHook` and async closures are sound only because they cannot re-enter.
- Extending `Ledger` to key on any `Arc` address, not just `EcsHandle`, is the smallest change that makes nested `MobHandle::with` panic instead of hang.
- The WASM ABI grows in three places: the `.wit` world, the lift/lower in the host's ABI module (a new action is a compile error, a new event is not, since `ClientEvent` is `#[non_exhaustive]`), and a `Capability`. Never grant an import-column capability in the default policy.
- Land a native capability with a WASM counterpart, or a sentence saying why a guest structurally cannot host it.

### Open work, by what unblocks most

1. Server proposal queue, `TickSet::Adjudicate` and `ServerProposal`/`ProposalVerdict` (event bus, cancellation, priority; also the JVM tier's route to Bukkit event semantics).
2. Split `lodestone-ecs` into a substrate crate and a client-vocabulary crate so `EventPriority`, `TaskScheduler`, `CommandRegistry`, `Permissions` and verdict types are one definition on both sides.
3. Replicate plugin block writes to connected players (independent of 1).
4. Player entities with a plugin-reachable inventory.
5. Plugin commands and node permissions on the dedicated server (a `CommandSink` over a server-side registry).
6. WASM break lifecycle, `fs:write`/commands, `Monitor` enforcement.
7. Server save-path adapter for `PluginDataStore`, and a typed WASM scope ABI; custom item data through save/load.
8. A ledger for the other lock classes, or a non-nestable `MobHandle::with`.
9. Open a plugin menu on a remote player (container-open packets, click echo).
10. Tests proving a wrapped `VersionAdapter`/`ServerProtocol` sees and appends traffic.
11. JVM tier JNI spike, after 1 and 4.

## Configuration

Native tier: `App::add_plugins`, no manifest. WASM tier: `PluginHost::new(policy)` with `default_policy()` withholding `fs:read` and `schedule:tasks`, plus `with_fuel`, `with_memory_limit`, `with_filesystem_root` and per-plugin `plugin.toml`. Server: `LanConfig` (`commands`, `plugin_channels`, `resource_packs`, access lists) and the `IntegratedServer::open_*` arguments; native server plugins are a compile-time builder choice in the standalone binary.

## Dependencies

- Client native: `lodestone-ecs` (with `bevy_app`/`bevy_ecs` for derives), optionally `lodestone-plugin-support`, `lodestone-world`, a version crate or `lodestone-shell`.
- Client WASM: `lodestone-wasm-host` (`wasmtime`, `wit-component`; not buildable for `wasm32`), guests on `wit-bindgen`.
- Server: `lodestone-server` (it links `bevy_app`/`bevy_ecs` directly, not `lodestone-ecs`), `lodestone-worldgen` for generators.
- JVM tier: `lodestone-jvm-bridge` depends on `lodestone-ecs` only.

See also: [plugin-api](plugin-api.md), [plugin-server-capabilities](plugin-server-capabilities.md), [packet-wiring](packet-wiring.md), [server-ecs-migration plan](plans/server-ecs-migration.md), [java-plugin-bridge](java-plugin-bridge.md), [roadmap/plugin-framework](roadmap/plugin-framework.md).
