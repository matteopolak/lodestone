# Plugin capability audit: what a Java plugin can do here, per side and per tier

## What it is

A capability-by-capability audit of the claim "any Bukkit/Paper/Fabric plugin is portable to this
framework", checked against the tree rather than against design documents, with a verdict per
capability for each of the three surfaces a plugin can land on — the client's native `bevy_ecs`
tier, the client's WASM host, and the server — and, for every gap, what closing it costs and which
of the two constraints (both tiers must express the same feature; `EcsHandle` is not reentrant) it
has to satisfy. It ends with a decomposition into sub-issues ordered by what unblocks the most.

The headline: **the client's native tier is at or near parity for every capability family except
wire-level packet mutation, the client's WASM tier remains substantially narrower, and the server's
bevy-shaped plugin surface now reaches in-memory and persistent primary worlds, including the
standalone binary's compile-time application builder**. Native systems run on the real `GameTick`,
but event adjudication, commands, permissions, async hand-back, and runtime plugin discovery remain
gaps. Since real Bukkit/Paper plugins are overwhelmingly server plugins, the answer for the
deployment that matters most remains "not yet".

## How it works

### Method

Every row below names the symbol that implements the capability, and was checked by reading that
symbol, not by reading a doc that mentions it. Where a capability is claimed wired, the production
call site is named too, because a mechanism nothing calls is the island defect this repo names as its
most expensive, and a crate's own tests cannot distinguish "registered and running" from "registered
and never run". Where a search found nothing, the search is stated so a reader can widen it.

Sources: `docs/plugin-api.md`, `docs/plugin-server-capabilities.md`, `docs/plugin-entity-api.md`,
`docs/plugin-crafting-hooks.md`, `docs/plugin-worldgen-api.md`, `docs/packet-wiring.md`,
`docs/server-commands.md`, `docs/java-plugin-bridge.md`, `docs/plans/paper-nms-bridge.md`,
`docs/plans/server-ecs-migration.md`, `docs/roadmap/plugin-framework.md`; the crates
`lodestone-ecs`, `lodestone-app`, `lodestone-shell` (`sim/build.rs`, `interact.rs`, `net.rs`,
`sim/actions.rs`, `sim/step.rs`), `lodestone-controller`, `lodestone-client` (`state.rs`,
`builder.rs`), `lodestone-server` (`ecs/`, `integrated.rs`, `tick.rs`, `command.rs`, `players.rs`,
`plugin_channels.rs`, `plugin_crafting.rs`, `plugin_dimension.rs`, `structure_placement.rs`,
`chunk.rs`, `protocol.rs`), `lodestone-wasm-host` (`wit/lodestone-plugin.wit`, `capability.rs`,
`conductor.rs`), `lodestone-dedicated-server`, and every crate under `crates/plugins/`.

### The two constraints, restated as checkable properties

**Both tiers must express the same features.** A compiled-in native plugin has no sandbox
(`add_plugins` is `dlopen`-equivalent trust); the WASM host is the sandboxed tier, and neither
substitutes for the other. So for a capability to count as "portable", it has to be expressible on
the WASM tier as well as the native one — and where the WASM tier structurally cannot host it (a
resumable off-thread search over an owned snapshot; anything needing a `World` borrow), that has to
be a stated ceiling rather than an omission. The WASM ABI today (`wit/lodestone-plugin.wit`) exports
`on-tick(list<event>) -> list<action>` and `on-task(task-id, token) -> list<action>` over three event
kinds (`chat`, `health-changed`, `blocks-changed`) and three actions (`send-chat`, `send-command`,
`swing-arm`), plus `log`, `fs:read`, and capability-gated scheduler imports. Every "WASM" cell below
is judged against that.

**`EcsHandle` is `Arc<parking_lot::RwLock<World>>` and is not reentrant.** Of the four
guard-nesting combinations, three deadlock always and the fourth whenever a writer is queued, with
no panic and no log line. `lodestone_ecs::hold_read`/`hold_write` keep a per-thread ledger and
panic naming both sites instead — but only for guards taken through those two functions;
`EcsHandle` is a type alias, so the inherent `.read()`/`.write()` cannot be intercepted. So for
each capability the question is: **through what argument could a plugin author's code reach the
lock a second time, and what stops them?** The answers fall into four classes, from strongest to
weakest, and each row below is labelled with one:

| class | what stops a second guard | example |
|---|---|---|
| **omitted** | the plugin is handed no value that can reach the lock — unrepresentable | a bevy system's `&World`/`&mut World`, a `VetoFn`'s `VerbContext` |
| **typed closure** | the closure signature has no `World`/`EcsHandle` parameter | `AsyncTaskPool::spawn`'s `FnOnce() -> T` |
| **ledger** | a runtime tripwire panics on the second guard (only through `hold_*`) | `Sim::ecs()` + `hold_write` |
| **unguarded** | nothing; a comment asks nicely | `MobHandle::with` nested inside `MobHandle::with` |

The distinction that does the most work: a plugin depending only on `lodestone-ecs` has **no route
to an `EcsHandle` at all** — nothing in that crate inserts one as a resource, and no
`Res<EcsHandle>`/`ResMut<EcsHandle>` exists anywhere in the workspace (searched: every `.rs` under
`crates/`). `EcsHandle` is re-exported so a *host* can name it; a plugin's `Plugin::build` and its
systems never see one. That closes the whole class by omission for the sanctioned surface, and the
escape hatch (depend on `lodestone-shell`, call `Sim::ecs()`) is what the ledger exists for.

### The verdict table

Legend: **done** (real, with a named production call site) · **partial** (exists, with the missing
piece named) · **gap** (nothing) · **ceiling** (will not exist by design; stated why).

| capability | client, native | client, WASM | server |
|---|---|---|---|
| observe a typed event | **done** — `GameEvent(ClientEvent)` via `MessageReader`, off by default, installed for every shipped `App` by `ServerBrandChannelPlugin` in `lodestone_app::client_app` | partial — 3 event kinds of ~110 | partial — plugin-defined `Message` types use `App::add_message`, independent readers, and tick-owned retention; no built-in gameplay event bus, and `dispatch_play_packet` applies inline |
| cancel an event (`setCancelled`) | **done** — `ActionVetoes` for all six declared verbs, all asked in production | **gap** — no verdict-shaped export | **gap** — `TickSet::Adjudicate` runs but has no proposal or verdict systems; `CraftingStationHooks` is the one Allow/Deny/Replace hook |
| priority order across plugins | **done** — `EventPriority::{Lowest..Monitor}` chained into all four schedules | partial — manifest `priority` orders guests; nothing else | **gap** |
| `MONITOR` read-only | **done** — checked against bevy's per-system access set; blind to deferred `Commands` | **gap** | **gap** |
| sync delayed/repeating tasks | **done** — `TaskScheduler::{schedule_once, schedule_repeating, cancel}` | **done** — `scheduler::{schedule-once, schedule-repeating, cancel}` returns guest-local handles and dispatches `on-task` on host ticks | **done, native** — `ServerTaskScheduler::{schedule_once, schedule_repeating, cancel}`, drained by `ServerCorePlugin` on the production primary world's `GameTick` |
| async task + main-thread hand-back | **done** — `AsyncTaskPool::{spawn, spawn_with_handback}`; inline on wasm32 | **ceiling** — single-threaded guest by design | **done, native** — `ServerTaskScheduler::spawn_with_handback` returns `Send` values through a bounded hand-back queue drained on the primary `GameTick` owner |
| register a command | **done** — `CommandRegistry`/`PluginCommand`, `PluginCommandsPlugin` in `Sim::client_app`, reached from the wire through the shell's `EcsCommandSink` | partial — `send-command` invokes; nothing registers | partial — `CommandSink` seam exists; the dedicated server installs `CommandDispatch::none()`, so every plugin command is refused there |
| tab completion / argument types | **done** — `lodestone-command` argument types, `commands::suggest` | **gap** | as above |
| permission nodes, wildcards, defaults, groups, delegation | **done** — `PermissionStore`, `PermissionRegistry`, `PermissionResolver`, `Permissions` resource | **gap** | partial — native `AccessHandle::set_permission_provider` delegates the five connection levels by UUID before Play; node/wildcard permissions remain client-side through the sink, and existing connections retain their resolved level |
| read a block | **done** — `ChunkWorld`, `VersionAdapter::block_*` | partial — `blocks-changed` deltas only; no query | **done** — `ChunkSource::block_state` on the source the plugin constructed |
| write a block, with/without physics | **done** — `ChunkWorldWrite`, `set_block_with_physics`, `fill_region*`; the `physics: true` neighbour pass is queued but not run | **gap** | partial — `ChunkSource::set_block`, `place_structure_live`; **not replicated to a connected player** (see below) |
| custom generator / dimension / structures | ceiling client-side (terrain is the server's) | ceiling | **done** — `ChunkGenerator`, `DimensionRegistry` (primary world only), `place_structure_live` |
| modify an entity | **done** — every `lodestone_ecs::entity` component is plugin-writable | **gap** | **done** — `MobHandle::with` + `SimMob`; players via `PlayerRegistry::{set_position, set_game_mode, set_experience, push_effect, …}` |
| spawn / despawn | **done**, local-only — `entity_spawn::{spawn_entity, despawn_entity}` | **gap** | **done** — `IntegratedServer::{spawn_mob, despawn_mob}`, no veto point |
| custom entity types | **done** — `CustomEntityRegistry` (disguise) | **gap** | partial — any vanilla key as disguise; no shared registry |
| AI goals | ceiling (server state) | ceiling | **done** — `SimMob::add_goal(priority, Box<dyn Goal>)` |
| attribute writes | partial — `Attributes` readable; no client→server write exists in the protocol | **gap** | partial — reachable through `SimMob`, unaudited |
| custom menu (`createInventory`) | **done**, local-only — `Menus::open_local`, one menu at a time | **gap** | **gap** — no open-to-remote-player path; `PlayerInventory` is a connection-task local |
| inventory click veto | **done** — `SharedState::menu_click` asks before prediction and direct send | **gap** | **gap** |
| custom items / recipes / station hooks | **done** — `CustomItemRegistry`, `RecipeRegistryExt::add_recipe` | **gap** | **done** — `CraftingStationHooks` (anvil, grindstone, smithing, loom, stonecutter) |
| per-entity / per-chunk key-value data | partial — `EntityDataStore`/`ChunkDataStore` are in-memory by their own module doc | **gap** — `fs:read` only, no write | **gap** — `NbtStorageHandle` has no save path in `live_save`; nothing plugin-keyed is written to disk |
| plugin config file / data dir | **done** — `lodestone-plugin-support::{paths, config}` | partial — read-only | **done** for an embedding crate (plain `std`) |
| database access | **done**, trivially (unrestricted `std`) | **ceiling** — no network import exists, by design | **done**, trivially |
| inbound packet observation | **done** — decoded `GameEvent` plus opt-in `RawPacket` observation; `RawPacketBusPlugin` publishes the connection state, packet id, and exact payload before version-specific decoding | partial — 3 kinds | partial — a `ServerProtocol` decorator sees every `decode(state, id, payload)` call, version-locked |
| outbound packet mutation / cancel | partial — `EgressFilters` over `ClientAction` at the `ActionQueue` drain; five direct `send_action` paths bypass it (`egress_hook_coverage.rs`) | **gap** | partial — the same decorator sees every `Vec<ServerDirective>` it returns, so it can drop, rewrite, or **append** a `ServerDirective::Send`, version-locked |
| raw byte injection | **ceiling** — decided observation-only, permanently | ceiling | reachable through the decorator, untested and unsandboxed |
| the NMS-equivalent escape hatch | **done** — depend on a version crate (`packets`, `adapter` are `pub`), or on `lodestone-shell` for `Sim::ecs()` | **ceiling** — an import not in the WIT world is absent from the linker | **done** — embed `IntegratedServer` and hold the `ChunkSource`, `WorldStateHandle`, `PlayerRegistry`, `MobHandle`, `PluginChannelRegistry` it hands out |
| hot reload | ceiling (no stable Rust ABI) | **done** — a `.wasm` file on disk, `PluginHost::load_file` | ceiling |
| panic isolation | ceiling by decision — trusted code, fatal | **done** — trap/fuel/memory limits, three denial gates | ceiling |
| registered in the shipped binary | **done** — `run_with_app` / `WindowApp::new_with_app` | **done, native windowed client** — `run_windowed_with_app` installs the conductor and calls `load_directory` for cwd-relative `plugins/`; absent and denied-plugin controls reach the real shell `Sim` | **done, compiled-in native** — `dedicated_server_app` feeds the application through `open_persistent_server` and `IntegratedServer::open_persistent_with_mobs_and_commands_and_server_app` into the persistent primary tick task; no runtime discovery |

Read by column: the native client column is done or ceiling in every row but one (durable per-entity
data). The WASM tier now has shipped discovery and scheduling, but remains narrower than native in
verdicts, intents, commands, world/entity access, drawing, and durable writes.
The server column has real capability in exactly the rows where a hand-built registry could be
bolted onto plain function calls — worldgen, entity spawn, crafting hooks, plugin channels, player
registry — plus native per-tick systems in explicitly configured in-memory and persistent primary
worlds, including delayed/repeating task handles and cancellation. It still lacks the event,
event-cancellation, async hand-back, and command surfaces those systems need
for a broadly portable server plugin.

### Events and cancellation, the hard half

**Server plugin-defined observations** use Bevy's typed messages directly. `ServerCorePlugin`
maintains all `App::add_message` registrations before `TickSet::Drain` on the real primary tick
task, preserving a message until its second subsequent maintenance boundary. Separate plugin readers
have independent cursors; no frame schedule or world lock is required. This closes message lifetime
and cross-plugin observation wiring, while built-in gameplay proposals and cancellation remain gaps.

**Client, native.** Observation is `GameEvent`, and the one write site pushes every `ClientEvent`
with no `match`, so a new variant cannot miss the bus. Cancellation is `ActionVetoes`: a plugin
registers a `VetoFn` per `Verb`, priority-keyed; the engine asks *before* the effect is computed, so
a `Deny` leaves the predictor untouched and nothing reaches the wire. All six verbs are asked in
production, at the sites `crates/lodestone-ecs/tests/veto_coverage.rs` scans for:
`VerbContext::BlockBreak` in `lodestone_shell::interact::drive_mining`, `VerbContext::BlockPlace`
in `drive_placement`, `VerbContext::EntityDamage` in `Sim::attack_entity`, `VerbContext::PlayerMove`
in `lodestone_controller::ecs::send_player_input`, and `VerbContext::InventoryClick` in
`SharedState::menu_click`, plus `VerbContext::PlayerInteract` in `Sim::use_item_live`. The latter
asks once before selecting any effect-producing interaction arm and reuses its entity-first/block-
second/air-last target snapshot through the commit. The coverage test accounts for every declared
verb, so one cannot silently lose its wiring or be added without an explicit status.

One correction to `docs/plugin-api.md`'s intent-doctrine section, which says a plugin "has no way
to veto a human's" dig because the human path never asks the intent seam: that is true of
`BreakIntent` and false of the veto. `drive_mining` asks `ActionVetoes` before advancing the dig
state machine on **both** the human and the `BreakIntent` path — `via_intent` only decides whether
`BreakOutcome` is written. A protection plugin's `Deny` stops a real player's dig. The remaining
gap on that seam is narrower than the doc states: the human path cannot be *observed* through
`BreakIntent`/`BreakOutcome`, but it can be vetoed.

Reentrancy: **omitted**. A `VetoFn` receives a `Copy` `VerbContext` and nothing else, because three
of the six ask sites are plain `Sim` methods already inside a guard. A plugin needing world state to
decide keeps an `Arc` its own system refreshes each tick. `EgressFilters` callbacks are shaped the
same way for the same reason. A `Monitor`-tier system gets `&World` from the runner and cannot take
a second guard; a `Commands` mutation from that tier is the one hole the access check cannot see.

**Client, WASM.** A guest is asked nothing; it only receives the tick's events after the fact. There
is no export through which the host could obtain a verdict at an ask site. Adding one is
reentrancy-safe by construction (the guest has no `World`, and the conductor's `PluginHost` is a
resource the ask site can reach through `&self`), but it means a second guest export beside
`on-tick`, called synchronously from inside `drive_mining` and `Sim::attack_entity`, and a fuel
budget per ask rather than per tick. Priority is the manifest's `priority` field, ordering whole
guests; `Monitor` is unenforced because a guest returning an empty action list is indistinguishable
from one that read nothing.

**Server.** There is no event bus, no cancellation, and no hook registration of any kind on the
packet path: `dispatch_play_packet` in `lodestone_server::server` matches `ServerBound` and calls
`apply_*` helpers inline on the connection task, and `apply_block_action` breaks unconditionally.
The design that answers this — a proposal queue drained into `TickSet::Drain`, vetoed in
`TickSet::Adjudicate`, applied in `TickSet::Apply`, with server-side clause 4 inverted so the
plugin outranks the remote client — is fully written (`docs/plans/server-ecs-migration.md`
Phases 2 and 8, `docs/plugin-server-capabilities.md`'s `ServerProposal`/`ProposalVerdict`). The
primary tick task now runs `GameTick`, and an in-memory embedder can register systems into it, but
the proposal queue, event types, and verdict consumers still do not exist. `CraftingStationHooks`
is the one server hook with a verdict, and it runs **inline on the connection task**, so a hook
that panics takes that player's connection down — a property the panic-isolation decision
("native panics are fatal") does not cover, since it is fatal to one connection rather than the
process.

Reentrancy on the server is a different lock, not the same one. The design promise is that the
server `World` has no lock at all, so once systems exist in `Adjudicate` a plugin's `&mut World`
is omitted-class safe. Until then, every server-side plugin call is **unguarded**: `MobHandle` is
`Arc<std::sync::Mutex<MobSim>>`, and `MobHandle::with` nested inside `MobHandle::with` — which is
the natural way to write "spawn a mob from inside a goal that inspects another mob", since
`IntegratedServer::spawn_mob` is itself a `with` — deadlocks on `std`'s non-reentrant mutex with no
ledger to name the sites. `OverworldChunkSource.edits` is a `Mutex` taken inside `set_block` and
`column`. The scheduled-tick queue already had exactly this defect in production
(`docs/tick-scheduling.md`, "A self-deadlock the scheduled-tick queue's own lock made possible"):
a chunk load inside the tick's held region tried to restore pending ticks into the same lock. The
server has the hazard class and none of the client's instrumentation for it.

### The scheduler

**Client, native**: `TaskScheduler` fires on an exact tick schedule from `run_due_tasks`, an
exclusive system in `TickSet::Input`; the closure's `&mut World` is the driver's own guard one frame
deeper, never a second lock (**omitted**). `AsyncTaskPool` takes a parameterless off-tick closure
(**typed closure**), marks every worker thread, and `Ledger::enter` calls
`assert_not_in_async_worker` before the reentrancy check so a guard taken from a worker is reported
as its own defect rather than as ordinary contention. A plugin can still defeat this by capturing an
`EcsHandle` clone and calling raw `.read()` from the worker — which requires the escape hatch,
since the sanctioned surface never hands one out. On `wasm32` both functions run inline.

**Client, WASM**: the capability-gated scheduler import creates one-shot and repeating callbacks,
returns guest-local opaque cancellation handles, and passes a guest-defined token back through
`on-task`. Delay zero and one both mean the next host tick, repeat period zero is clamped to one, and
same-tick callbacks run in handle order. The async half remains a ceiling for a single-threaded guest.

**Server**: a native embedder can install a system in `GameTick` through
`ServerApp::bootstrap_with` and pass that application to
`IntegratedServer::open_in_memory_with_mobs_and_server_app`; the tick task owns the extracted
`World` and runs the system in deterministic schedule order. Persistent embedders and the
standalone binary use the corresponding application-injection leaf and the same primary tick path.
`ServerCorePlugin` installs `ServerTaskScheduler` and `run_server_tasks` in `TickSet::Drain`.
Callbacks receive the tick task's own `&mut World` and an opaque cancellation handle (**omitted**:
no second world lock is exposed). Delays exclude `ServerBoot`, zero delay/period normalize to one,
and equal deadlines run in registration order. A callback can cancel another due callback, stop its
own repetition, or enqueue work for a later tick. The scheduler remains a resource throughout
dispatch. `spawn_with_handback` admits bounded parameterless worker work and runs its result closure
on that same tick owner before due synchronous callbacks. `cancel_async` discards a pending result;
shutdown rejects new work and leaves running work with no callback route into a stopped world.

### Commands and permissions

`lodestone-command` is the shared tree substrate with three consumers (the server's built-ins, the
plugin registry, the client's tab-completion decode), so `docs/plans/paper-nms-bridge.md`'s claim
that it is "a self-declared island with zero consumers" is stale. The plugin path, end to end in
singleplayer: `ClientAction::SendCommand` → wire → `ServerBound::ChatCommand` → the server's
built-in tree, falling through to `CommandDispatch` → the shell's `EcsCommandSink` in
`lodestone_shell::net` → `hold_write` on the **client's** `EcsHandle` →
`lodestone_ecs::commands::dispatch` against `CommandRegistry`, with a `Permissions` resource whose
absence is a hard error rather than an ungated fallback. `PluginCommandsPlugin` is installed by
`Sim::client_app`, so the registry exists in the shipped client;
`crates/lodestone-shell/tests/interaction/client_app_installs_command_registry.rs` is the gate.

Two consequences worth stating plainly. First, a "server plugin command" in singleplayer is a
**client** plugin command: the handler runs against the client's `World`, under a write guard taken
on the server's connection task. That is ledger-covered (it goes through `hold_write`) and
thread-correct (the ledger is per-thread, and the tick thread is not inside a guard while a
connection task holds one — they contend, which is the ordinary case). Second, **on the dedicated
server there is no `World` for the sink to run against**, and `lodestone-dedicated-server`
installs `CommandDispatch::none()` — so plugin commands, node permissions, and tab-completion for
plugin nodes do not exist on the binary a Bukkit server operator would actually run. What exists
server-side is the five-level vanilla model plus ops/whitelist/bans (`docs/server-commands.md`).

Reentrancy for a command handler: **ledger**, via the sink's `hold_write`. The handler receives
`CommandInvocation<'w>` holding `&mut World`; a handler that calls a `ClientHandle` accessor panics
naming both sites rather than hanging.

WASM: a guest can `send-command` (invoke) and nothing else.

### World and block editing

Client-side the surface is complete for a WorldEdit-class plugin (`crates/plugins/lodestone-worldedit`
is the worked example) with one named hole: `set_block_with_physics(.., true)` queues the six
neighbours for a pass that does not exist, so client-side physics-true writes are physics-false
writes with a queue nobody drains. The read/write split (`ChunkWorld` versus `ChunkWorldWrite`) is
type-enforced. `ChunkWorld` is `Arc<std::sync::RwLock<lodestone_world::World>>` — a *second* lock
class, with a documented `World → chunks` order that nothing checks; a system holding
`ChunkWorldWrite`'s guard that then calls into anything taking the ECS guard is an ABBA deadlock the
ledger cannot see, because the ledger only knows about `EcsHandle`.

Server-side, a plugin owns the `ChunkSource` it passed to `IntegratedServer::open_*`, so it can call
`set_block(&self, ..)` from any thread at any time — and **nothing tells a connected player**. The
only tick→connection block-change path is `BlockTickFeed`, which is fed by tick systems (gravity,
random ticks, fire), not by `ChunkSource::set_block`, and whose `drain_all` is `mem::take` — single
consumer by construction, so a second LAN player loses updates the first drained. The reference test
for live placement (`crates/plugins/lodestone-void-world/tests/…`) places its marker *before* any
client joins, so it proves the store took the edit and proves nothing about a player who was already
looking at that chunk. A protection plugin restoring a griefed block, or a WorldEdit paste, is
invisible to everyone online until they re-receive the column. This is the highest-value server gap
that does not require the ECS migration to fix: a change feed on the source that every connection's
`ViewTracker` drains is a replication concern and can land ahead of Phase 3's broadcast egress.

WASM: no block read query and no write action. `blocks-changed` is a delta stream, which is the
right shape for a redstone watcher and the wrong one for a plugin that needs "what is at (x, y, z)".

### Entities

Client-side, "modify" is ordinary `Query` mutation over `lodestone_ecs::entity`'s components and
reaches pixels the next `Extract` because `lodestone_shell::entities::fold_entities` walks
`EntityIndex` generically. Spawn/despawn are local-only, id-safe by construction (plugin ids are
strictly negative; wire ids are non-negative), and a server-visible spawn is a ceiling under the
packet decision. Server-side, `spawn_mob`/`despawn_mob` are real and cross-player-visible, `SimMob`
is fully mutable through `MobHandle::with`, and `SimMob::add_goal` is a genuine AI extension seam —
`docs/roadmap/plugin-framework.md`'s "no AI exists at all" row is stale. What is missing is any
place for a *second* plugin to object to a spawn (`docs/plugin-server-capabilities.md`'s one
concrete hole), which is the adjudication window again.

Players server-side: `PlayerRegistry` exists (`crates/lodestone-server/src/players.rs`) with
position, rotation, game mode, experience, effects, chat and swing feeds, and entity ids from
`PLAYER_ENTITY_ID_BASE`, so `docs/plans/paper-nms-bridge.md`'s "no player entities, no player
registry, no broadcast" is stale. A player's **inventory** is not on it: `PlayerInventory` is a
`serve_play` local, reachable by a plugin only through the save/load path in `player_data`. A
Bukkit `player.getInventory().addItem(..)` has no equivalent on the server today.

### Inventories

Client-side a plugin opens its own menu with `lodestone_game::menus::Menus::open_local`, which
reclaims the player's inventory into the screen and refuses to close a server container behind the
player's back. One menu at a time. Server-side a plugin cannot open a menu on a remote player — the
container-open packet family and the click echo against a `lodestone-server` container model that
"barely exists" (the menu issue's own closing note) are both unbuilt. The client inventory-click
veto is asked before prediction inside `SharedState::menu_click`; a denial leaves the menu untouched
and sends no action despite that path bypassing `ActionQueue`.

### Persistence

The per-plugin data directory and typed JSON config are durable and shipped. The per-entity and
per-chunk key-value stores are explicitly the "non-persistent half" of Bukkit's persistent data
container, in their own module doc. Nothing on either side writes a plugin-keyed value to disk:
`live_save` has no `nbt_storage` path, and `player_data::to_nbt` serialises a closed struct. The
one opaque blob that does round-trip is `ItemComponents::custom_data` on the client — kept as raw
network-NBT bytes precisely so a lobby hotbar item stamped by a Java plugin does not truncate the
packet — and whether the server's own item save path carries it through is unverified (searched
`player_data.rs` and `inventory.rs` for `custom_data`: no hit). A durable tier has to keep the
"one opaque blob per key" property `docs/plugin-api.md` records, because a schema that consults a
static field list is the shape that silently drops data elsewhere in the tree.

WASM: `fs:read` exists and is enforced by the linker (an ungranted import is absent, so a guest
referencing it fails to instantiate); there is no `fs:write`, so a guest cannot persist anything.

### Packet interception

The decided shape is observation-only in both directions, permanently, on the client: inbound
events apply inline under the world's write guard, so an interceptor wanting `&mut World` there is
the reentrancy shape everything else makes unrepresentable, and outbound byte mutation only exists
inside a version-typed adapter. `EgressFilters` is the outbound ceiling — `ClientAction`, never
bytes — and it is bypassed by five direct `send_action` sites the coverage gate enumerates. Two
archetypes stay out of reach for good: anti-cheat and a disguise visible to *other* players.

Two escape hatches change that picture at the cost of version-locking, and neither is documented as
such. On the client, `lodestone_client::ClientBuilder::new` takes a `Box<dyn VersionAdapter>`, and
a decorator implementing `VersionAdapter` around `V770Adapter` sees every `handle_packet` and every
`encode_action` in both directions — a headless bot gets ProtocolLib-class visibility this way; the
windowed shell builds its own adapter and offers no such seam. On the server, `IntegratedServer`
is generic over `ServerProtocol`, whose `decode(state, packet_id, payload) -> ServerBound` and
`Vec<ServerDirective>`-returning encoders are exactly the two directions: a decorator around
`V770ServerProtocol` observes every inbound payload and can drop, rewrite, or **append** a
`ServerDirective::Send` to any outbound batch. That is server-visible disguise and outbound
injection, reachable today by any crate embedding the server, unsandboxed, exercised by no test,
and bounded by `ServerBound` being a closed enum — a wrapper can see an unknown inbound packet's
bytes but cannot inject a new kind of inbound *action*. This does not reopen the decided ceiling
for the shared crates; it is the version-crate route the ceiling already permits, and it should be
written down as the honest answer to "can an anti-cheat be ported": yes, version-locked, native,
server-side only.

### The NMS-equivalent escape hatch

What an NMS call buys a Java plugin is unmediated access to the server's own object graph: the
internal player behind the wrapper, the internal level behind the world, the raw block-state
flyweight, a connection's send path. The audit question is whether an equivalent exists per tier,
and whether it is honest about its cost.

- **Client, native**: yes, twice over. A version crate is a leaf, so a plugin may depend on
  `lodestone-v26-2` directly and use its `packets` and `adapter` modules — version-locked at the
  `Cargo.toml` level, which is strictly better than a runtime class-not-found. A plugin may depend
  on `lodestone-shell` and hold `Sim::ecs()` — the one route to an `EcsHandle`, and the one place
  the ledger rather than omission is the guard. `lodestone-plugin-support::reentrancy` ships both
  halves of the check an author can run: `assert_ecs_only_dependency_graph` (the static half:
  prove the manifest has no route) and `assert_schedule_completes_under_write_guard` (the runtime
  half: a watchdog that leaks a wedged thread on purpose rather than joining it).
- **Client, WASM**: none, correctly. The tier's whole value is that an import not in the WIT world
  is not in the linker.
- **Server**: yes — embed `IntegratedServer` and you hold everything it hands out
  (`world_state`, `players`, `mobs`, `tickets`, `portals`, `save_now`, `level_dat`,
  `block_ticks`) plus the `ChunkSource` you built and the `ServerProtocol` you wrapped. What is
  *not* there is a `&mut World`, because the server `World` never ticks, so "everything internal
  code can do" is currently "everything the connection task and the tick task can do through their
  mutexes", which is where the unguarded deadlocks live.
- **The JVM tier** (`crates/plugins/lodestone-jvm-bridge`) is the shape that makes real Bukkit jars
  runnable: `WorldPort` holds a `SyncSender` and a `Duration` and no field that can reach a lock,
  so a Java handler's worst case is a reported timeout; the identity slot map saturates rather than
  wraps. It has no `jni`, no `libjvm`, and per its own plan cannot start until the adjudication
  window and player registry exist — the second of which does.

### Port feasibility by archetype

| archetype | client native | client WASM | server |
|---|---|---|---|
| protection (veto break/place/PvP in a region, persistent regions) | portable except regions do not survive a restart | not portable (no veto) | not portable (no veto, no durable data) |
| economy (balances, transaction events, commands) | portable in memory; balances do not survive a restart | not portable (no commands, no write) | not portable on the dedicated server (no commands); embeddable with the plugin's own DB |
| minigame (scheduler, kit menus, commands, phase-gated cancellation) | portable | not portable | not portable |
| world editor (batched edits, undo, own commands) | portable | not portable | edits land, nobody online sees them |
| anti-cheat | not portable (decided ceiling) | not portable | portable via a `ServerProtocol` decorator, version-locked |
| hologram / disguise | local-only cosmetic | not portable | server-visible via the decorator, version-locked |
| client HUD mod | portable (`DebugLines`, `PluginBillboards`, `PluginKeybinds`, `CameraOverride`) | not portable (no draw action) | n/a |
| pathfinding bot | portable (`lodestone-autopilot`) | ceiling, by cost | n/a |

### Stale claims in existing docs, corrected here

- `docs/roadmap/plugin-framework.md` records "gap" for the event bus, priorities, scheduler, command
  registry, permissions, block write, spawn/despawn, persistent data, custom menus, the WASM host,
  input interception, and AI goals. Every one of those exists on the client native tier (symbols
  above), and the AI seam exists on the server. Its verdict section, which rests on those rows,
  is stale in the same direction; its port-feasibility table is superseded by the one above.
- `docs/dedicated-server.md`'s "Server-side ECS" section says `lodestone-server` links `bevy_ecs`
  "via `lodestone-ecs`". `crates/lodestone-server/Cargo.toml` depends on `bevy_app`/`bevy_ecs`
  directly and says in its own comment that it deliberately does **not** link `lodestone-ecs`.
- `docs/plugin-api.md`'s cross-plugin messages section says the server-side plugin-facing API over
  `custom_payload` "does not exist yet". `lodestone_server::plugin_channels::{PluginChannelRegistry,
  PluginChannelHandler}` exist, with `register`, `dispatch`, and `broadcast`, and `LanConfig`
  carries the registry into every accepted connection.
- `docs/plugin-api.md`'s intent-doctrine "known gap" overstates itself: a human dig can be
  **vetoed** (`drive_mining` asks before `via_intent` is consulted); it cannot be *observed* through
  the intent components.
- `docs/plans/paper-nms-bridge.md` predates `PlayerRegistry`, the `lodestone-world` dependency in
  `lodestone-server`, and the three consumers of `lodestone-command`; its seam table's rows 5, 1
  and 13 are stale in the "Rust today" column. Its cut-line finding and its verdict are not.

## How to change it, and the gotchas

- **Re-run the census before trusting any "gap" above.** This audit found eleven stale gap rows in
  the previous one; the next reader should expect the same drift. The cheap discriminators are
  named per row: a `Res<T>` in a shipped plugin tuple, a `VerbContext::X` constructor in a
  non-test file, a `run_schedule` call in a production driver.
- **A server capability that lands ahead of the ECS migration should still be shaped for it.**
  Every hand-built server registry (`CraftingStationHooks`, `DimensionRegistry`,
  `PluginChannelRegistry`) is correct on its own and collectively inconsistent, because each solved
  its problem before there was a shared place to solve the general one. A new one should use the
  `Allow`/`Deny`/`Replace`, first-non-`Allow`-wins, priority-ordered shape all three plugin-facing
  verdict types already share, so it can be re-homed onto `ServerProposal` without changing its
  plugin-facing contract.
- **Never hand a callback a `World`, an `EcsHandle`, or anything reaching either** — the soundness
  argument for `VetoFn`, `EgressFilters`, `CraftingStationHook` and the async closure is that they
  cannot re-enter; one "just this once" overload deletes it.
- **The ledger sees one lock.** `ChunkWorld`, `MobHandle`, `OverworldChunkSource.edits` and the
  scheduled-tick queues are separate locks with documented orders and no tripwire. Extending
  `Ledger` to key on any `Arc` address rather than on `EcsHandle` specifically is the smallest
  change that would make a nested `MobHandle::with` panic instead of hang.
- **The WASM ABI grows in three places and the compiler catches one**: the `.wit` world, the
  lift/lower in the host's ABI module (a new *action* is a compile error, a new *event* is not,
  because `ClientEvent` is `#[non_exhaustive]`), and a `Capability` if none covers it. Never grant
  an import-column capability in the default policy.
- **Both tiers, or say which one cannot.** A capability landing on the native tier without a WASM
  counterpart needs either the counterpart or a sentence in the ceiling column saying why a guest
  structurally cannot host it. "Not yet" is the answer that made the WASM column what it is.

## Configuration

None of its own. The native tier is `App::add_plugins` with no manifest; the WASM tier is
`PluginHost::new(policy)` with `default_policy()` withholding `fs:read` and `schedule:tasks`, plus
`with_fuel`, `with_memory_limit`, `with_filesystem_root`, and a per-plugin `plugin.toml`; the server
is `LanConfig` (`commands`, `plugin_channels`, `resource_packs`, access lists) and the constructor
arguments of `IntegratedServer::open_*`. `lodestone-dedicated-server` passes
`CommandDispatch::none()`. Native server registration is a compile-time builder choice in the
standalone binary; no property or runtime directory chooses native plugins.

## Dependencies

- Client native: `lodestone-ecs` (+ `bevy_app`/`bevy_ecs` as direct dependencies for derives),
  optionally `lodestone-plugin-support`, `lodestone-world`, a version crate (version-locking), or
  `lodestone-shell` (the `Sim::ecs()` escape hatch).
- Client WASM: `lodestone-wasm-host` (`wasmtime`, `wit-component`; `cfg(not(target_arch =
  "wasm32"))` — it cannot run inside a browser build), guest crates on `wit-bindgen` and the
  vendored `.wit`.
- Server: `lodestone-server` (path dependency, as `lodestone-crafting-warden` and
  `lodestone-void-world` do), `lodestone-worldgen` for a generator, a version crate for a
  `ServerProtocol` decorator.
- JVM tier: `lodestone-jvm-bridge` → `lodestone-ecs` only; no `jni`, by a manifest gate.

## Sub-issue decomposition

Ordered by what unblocks the most. Each names the side and the tier it must land in. Sizes are
stated against this repo's record of estimates running ~5× optimistic; "large" means a multi-phase
plan, not a sprint.

1. **Server: drive the server `World` from the tick loop and expose plugin registration.** Thread
   `&mut World` into `run_tick_loop`, run `GameTick` once per iteration, and give every
   `IntegratedServer::open_*` (and `LanConfig`) a way to accept a caller-composed `ServerApp` so a
   crate can `add_plugins` before the `World` is handed to the tick task. Server, native. Large.
   Unblocks every other server row.
2. **Server: the proposal queue, `TickSet::Adjudicate`, and `ServerProposal`/`ProposalVerdict`.**
   Move `apply_attack` then `apply_block_action` behind the queue; `EventPriority` chained into the
   server schedules; a corrective packet as the observable refusal. Server, native. Large. This is
   the event bus, cancellation and priority rows for the server in one piece, and the JVM tier's
   only route to Bukkit event semantics.
3. **Both: split `lodestone-ecs` into a substrate crate and a client-vocabulary crate**, so
   `EventPriority`, `TaskScheduler`, `CommandRegistry`, `Permissions` and the verdict types are one
   definition on both sides instead of a server re-port. Both sides, native. Medium; the server's
   schedule labels are already written to become re-exports.
4. **Server: replicate plugin block writes to connected players.** A change feed on the
   `ChunkSource` that every connection's view tracker drains; multi-consumer, unlike
   `BlockTickFeed`. Server, native. Medium, and independent of 1–2.
5. **Server: player entities with a plugin-reachable inventory.** `PlayerRegistry`'s scalars plus
   `PlayerInventory` become components on a player entity. Server, native. Large.
6. **Server: async hand-back** onto the server `World`. Delayed/repeating synchronous callbacks and
   cancellation already run through `ServerTaskScheduler`; worker completion delivery remains absent.
7. **Server: plugin commands and node permissions on the dedicated server** — a `CommandSink`
   backed by a server-side `CommandRegistry` rather than the client's `World`. Server, native.
   Medium; depends on 3.
8. **WASM: the intent half of the ABI** — install/remove-shaped break/place/move/look plus an
   outcome poll. Client, WASM. Medium.
9. **WASM: a verdict export**, called synchronously at the six ask sites, with its own fuel budget.
   Client, WASM. Medium; this is what makes a protection plugin expressible on the sandboxed tier.
10. **WASM: commands, `fs:write`, and `Monitor` enforcement.** Delayed/repeating scheduling with
    cancellation and native-windowed shell discovery are shipped. Client, WASM. Medium, three small
    pieces.
11. **Both: durable per-entity/per-chunk plugin data**, one opaque blob per namespaced key,
    through the world save on the server and the plugin data directory on the client. Both sides,
    native (WASM follows via `fs:write`). Medium; the server half needs the save path to carry an
    opaque section it does not model.
12. **Both: a reentrancy ledger for the other lock classes** — `MobHandle`, `ChunkWorld`, the chunk
    edit cache — or a type-level shape for `MobHandle::with` that cannot nest. Both sides, native.
    Small to medium.
13. **Client native: `RawPacket`**, inbound, observation-only, off by default. **Shipped** through
    `RawPacketBusPlugin` and the driver's pre-decode publication point; focused unit and hermetic
    driver tests preserve the exact state, id, and payload. Client, native.
14. **Server: custom item data through save/load** — verify and, if missing, carry
    `custom_data` through `player_data::to_nbt`/`from_nbt`. Server, native. Small to medium.
15. **Server: open a plugin menu on a remote player.** Needs the container-open packet family and
    the click echo. Server, native. Large.
16. **Both: document the `VersionAdapter` and `ServerProtocol` decorators as the version-locked
    packet escape hatch**, with one test each proving a wrapped protocol sees and can append
    traffic. Both sides, native. Small.
17. **JVM tier: the JNI spike** — one shim class, one native method through `WorldPort`, one trivial
    plugin. Server, native. Medium, and only after 1, 2 and 5.

## See also

- [`plugin-api.md`](./plugin-api.md) — the client-side surface this audit checks row by row.
- [`plugin-server-capabilities.md`](./plugin-server-capabilities.md) — the five shipped server
  capabilities scored against the intent doctrine, and the `ServerProposal` design.
- [`packet-wiring.md`](./packet-wiring.md) — `ActionVetoes` and `EgressFilters`, and the direct
  `send_action` sites the egress hook cannot see.
- [`plans/server-ecs-migration.md`](./plans/server-ecs-migration.md) — the phased plan sub-issues
  1, 2, 5 and 6 belong to.
- [`plans/paper-nms-bridge.md`](./plans/paper-nms-bridge.md) and
  [`java-plugin-bridge.md`](./java-plugin-bridge.md) — the JVM tier, its census, and its
  threading design.
- [`roadmap/plugin-framework.md`](./roadmap/plugin-framework.md) — the earlier audit this one
  supersedes on status, and whose decision records (no Folia; one `World`, one schedule, one
  accumulator) still stand.
