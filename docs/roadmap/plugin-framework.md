# Plugin framework: the capability audit

## What this is

The decomposition behind epic [#77](https://github.com/matteopolak/lodestone/issues/77):
a capability-by-capability audit of what a real Bukkit/Paper/Fabric plugin does, checked
against what a native `bevy_app::Plugin` (and, where it exists only on paper today, the
WASM host) can do in this codebase *right now*, with a gap and an issue number attached
to every row that is not fully closed. Epic [#7](https://github.com/matteopolak/lodestone/issues/7)
owns the ECS substrate that makes any of this possible
([`../bevy-migration.md`](../architecture.md), [`../world-unification.md`](../architecture.md));
this doc and its 49 sub-issues own whether that substrate adds up to **capability parity**
with the Java ecosystem, which is a different and harder question than "does the ECS
exist."

**The claim under test, stated so it can be falsified:** *any Bukkit/Paper/Fabric plugin
should be portable to this framework — not approximately.* That is not a wish list, it is
a test with a pass/fail per capability, and the honest answer (§"Verdict" below) is that
it does not pass today, in one specific and non-negotiable way.

## Method

Every capability below was checked against the actual tree, not against what a design doc
says should exist — the two disagree in one important place (§"A stale claim found and
fixed" below). Sources read in full: [`../plugin-api.md`](../plugin-api.md),
[`../bevy-migration.md`](../architecture.md), [`../world-unification.md`](../architecture.md),
[`../entity-components.md`](../player-simulation.md), [`../local-player-components.md`](../player-simulation.md),
[`../session-components.md`](../player-simulation.md), `crates/lodestone-ecs/src/{sets,schedules,player,session}.rs`,
`crates/lodestone-model/src/{adapter,action}.rs`, `crates/plugins/lodestone-nav` (the one
real, 75-test clean-room plugin), and the existing issue tracker (`gh issue list --state
all --limit 200`, to avoid duplicating #20, #35, #36, #37, #38, #46, #48, #67, all of
which this doc references rather than re-files).

## A stale claim found and fixed

`docs/plugin-api.md`'s own "four concrete gaps" section, at the time this audit started,
stated that the `TickSet::Intent` ordering anchor and a `LookIntent` component distinct
from the camera were **still missing**, each backed by a `grep` that returned empty.
Re-running those same greps against the current tree returns real hits: `TickSet` has six
variants today (`Input, Intent, Physics, Predict, Animate, Send`), and
`crates/lodestone-ecs/src/player.rs` defines `LookIntent` with the
insert-to-take-control/remove-to-release idiom already established for other intent
components. Both landed in commit `0d82ab4` ("feat: close the ingest seam, three plugin
ABI pieces, and Tier-1 wiring"), whose own message names closing exactly these two items —
the commit that fixed the gap knew it was fixing a documented gap, and the doc was never
updated. Filed as [#180](https://github.com/matteopolak/lodestone/issues/180)
(`stale-record`), and this table below reflects the *current* tree, not the stale doc.
This is exactly the failure class `CLAUDE.md` names as the most expensive in this repo —
true when written, false since, and not wrong-looking on inspection — and it directly
changed this audit's verdict on ordering-anchor coverage from "gap" to "closed."

## The capability audit

Status legend: **done** (real, verified against the tree) · **partial** (some of the
capability exists, concretely stated what's missing) · **gap** (nothing exists) ·
**ceiling** (will not exist by design; stated why).

### Events

| Java capability | our status | gap | issue |
|---|---|---|---|
| `@EventHandler` subscription to a typed event | done (native tier) | `GameEvent(ClientEvent)` is a bevy `Message` read with `MessageReader<GameEvent>`, installed for every shipped `App` by `ServerBrandChannelPlugin` in `lodestone_app::client_app`; the one write site pushes every `ClientEvent` with no `match`, so a new variant cannot miss the bus | [#104](https://github.com/matteopolak/lodestone/issues/104) |
| Raw packet visibility (ProtocolLib-class, receive side) | partial | `RawPacket` message specified (`bevy-migration.md` §5.1), never built on the sanctioned shared-crate surface; a version-locked decorator route exists outside it — see [`../plugin-packet-decorators.md`](../plugin-packet-decorators.md) | [#104](https://github.com/matteopolak/lodestone/issues/104) |
| Cancellation (`setCancelled`) | partial (native tier) | `ActionVetoes` (`crates/lodestone-ecs/src/veto.rs`) asks a per-verb predicate before the effect is computed, priority-keyed, first `Deny` wins; four of six declared verbs are asked in production (`BlockBreak`, `BlockPlace`, `EntityDamage`, `PlayerMove`), `InventoryClick`/`PlayerInteract` remain deferred | [#101](https://github.com/matteopolak/lodestone/issues/101) (design) |
| Cancellation of the concrete high-value verbs (break/place/damage/click/move) | partial | break/place/damage/move are wired (`ActionVetoes`, above); click and interact are the two verbs still deferred | [#109](https://github.com/matteopolak/lodestone/issues/109) |
| `EventPriority` (LOWEST..MONITOR) | done (native tier) | `EventPriority::{Lowest, Low, Normal, High, Highest, Monitor}` (`crates/lodestone-ecs/src/sets.rs`) is `.chain()`ed into all four public schedules | [#105](https://github.com/matteopolak/lodestone/issues/105) (design) |
| MONITOR (guaranteed-last, read-only) | done (native tier), with a known blind spot | enforced structurally — a system with any mutable `World` access fails to register in that tier, checked against bevy's per-system access metadata; a `Monitor` system queuing a deferred `Commands` mutation is the one hole this check cannot see | [#110](https://github.com/matteopolak/lodestone/issues/110) |
| Custom/plugin-defined events | partial | any bevy `Message` type already works for two plugins compiled into one binary; no documented convention or worked example | [#107](https://github.com/matteopolak/lodestone/issues/107) |
| ~400 Paper event types | gap (by extension) | not enumerable as one issue; each is an instance of the event-bus + cancellation primitives above once those exist | tracked via #101/#104/#109 |

### Scheduler

| Java capability | our status | gap | issue |
|---|---|---|---|
| `runTaskLater` / `runTaskTimer` | done (native tier) | `TaskScheduler::{schedule_once, schedule_repeating, cancel}` (`crates/lodestone-ecs/src/scheduler.rs`), fired on an exact tick schedule from `run_due_tasks`, an exclusive system in `TickSet::Input` | [#113](https://github.com/matteopolak/lodestone/issues/113) |
| `runTaskAsynchronously` + main-thread hand-back | done (native tier) | `AsyncTaskPool::{spawn, spawn_with_handback}` (`crates/lodestone-ecs/src/async_task.rs`) is the general API — a parameterless off-tick closure, inline on `wasm32`; `lodestone-nav`'s search is one caller of it, not a hand-built one-off | [#114](https://github.com/matteopolak/lodestone/issues/114) |
| Folia-style region threading | **ceiling, permanently** | one `World`, one thread, one clock, by design (`world-unification.md`) — not a gap, a decided permanent contract; see "Decision records" below | [#116](https://github.com/matteopolak/lodestone/issues/116) (closed) |

### Commands

| Java capability | our status | gap | issue |
|---|---|---|---|
| Register a plugin command | done (client native tier), gap (dedicated server) | `CommandRegistry`/`PluginCommand`, `PluginCommandsPlugin` installed by `Sim::client_app`, reached from the wire through the shell's `EcsCommandSink`; `lodestone-dedicated-server` installs `CommandDispatch::none()`, so a plugin command on the binary a server operator runs is still a gap | [#118](https://github.com/matteopolak/lodestone/issues/118) |
| Argument types + tab completion | done (client native tier), gap (dedicated server) | `lodestone-command`'s argument types and `commands::suggest`, same reach/gap split as above | [#119](https://github.com/matteopolak/lodestone/issues/119) |
| Permission per command node | done (client native tier), gap (dedicated server) | `PluginCommand::permission`/`require_permission` gate a node or a whole subtree, pruned the same way vanilla's own command-tree permission recursion does; same dedicated-server gap | [#122](https://github.com/matteopolak/lodestone/issues/122) |
| `/execute` interop for plugin commands | blocked | depends on #48 (server-side Brigadier dispatcher, Tier 4, not this epic's to build) | [#123](https://github.com/matteopolak/lodestone/issues/123) |

Note: vanilla command UX (#46) and the vanilla dispatcher (#48) are **not** duplicated
here — they are the non-plugin surface. #118–123 are the plugin *extension point* into
whatever #46/#48 eventually build, and were scoped explicitly to share an argument-type
library with #48 rather than diverge.

### Permissions

| Java capability | our status | gap | issue |
|---|---|---|---|
| Permission nodes, wildcards, defaults | done (client native tier), partial (server) | `PermissionStore`/`PermissionRegistry`/`PermissionResolver`/`Permissions` (`crates/lodestone-ecs/src/permissions.rs`): dotted nodes, check-time wildcard matching (LuckPerms-style, not Bukkit's declare-time expansion), per-node defaults. Server-side, only the five vanilla permission levels plus ops/whitelist/bans exist (`docs/server-commands.md`); node permissions live only in `lodestone-ecs`, reachable only through the sink | [#125](https://github.com/matteopolak/lodestone/issues/125) |
| Op-level / per-player / group resolution | done (client native tier) | groups, inheritance (cycle-terminating), and the specificity/tier/negation precedence order are all resolved in `PermissionResolver`; same server-side gap as above | [#127](https://github.com/matteopolak/lodestone/issues/127) |
| Delegating to a permissions plugin (the real-world default) | gap | no resolver-trait seam exists to delegate to a *different* plugin's resolution; `PermissionResolver` is the one resolver, not a trait a plugin can substitute | folded into #125's design |

On the client native tier this is no longer the pure gap the rest of this section
describes. What remains is narrower: resolver substitution, and the dedicated server
having no node-permission surface at all.

### World and block access

| Java capability | our status | gap | issue |
|---|---|---|---|
| Get block (with/without version lock) | **done** | `VersionAdapter::{block_collision, block_name, block_outline, block_interaction}`, `lodestone_model::block_physics` — real, closed in `24af787` | — |
| Set block, with/without physics | done (client native tier), partial (server) | `ChunkWorldWrite`, `set_block_with_physics`, `fill_region`/`fill_region_capturing` (`crates/lodestone-world/src/world.rs`); the `physics: true` neighbour pass is queued but the pass that drains it does not exist, so a physics-true write is a physics-false write today. Server-side, `ChunkSource::set_block` and `place_structure_live` land the edit but are **not replicated to a connected player** — the only tick→connection block-change path is fed by tick systems, not by a plugin's direct write | [#129](https://github.com/matteopolak/lodestone/issues/129) |
| Bulk edits (WorldEdit-class) | done (client native tier) | `crates/plugins/lodestone-worldedit` is the worked example, over the batched-write primitive above; undo/redo is the plugin's own problem, same as real Paper | [#131](https://github.com/matteopolak/lodestone/issues/131) |
| Custom world generator / biome provider | done (server) | `lodestone_worldgen::generator::ChunkGenerator` — a real plugin-facing trait, server-side; a ceiling client-side (terrain is the server's) since `lodestone-worldgen` stays deliberately not a system | [#132](https://github.com/matteopolak/lodestone/issues/132) (design) |
| Custom dimension registration | done (server), primary world only | `DimensionRegistry` (`crates/lodestone-server/src/plugin_dimension.rs`) | [#134](https://github.com/matteopolak/lodestone/issues/134) |
| Structure placement | done (server) | `place_structure_live` (`crates/lodestone-server/src/structure_placement.rs`) | [#136](https://github.com/matteopolak/lodestone/issues/136) (parked) |

### Entity manipulation

| Java capability | our status | gap | issue |
|---|---|---|---|
| Modify an existing entity (position, health, equipment, ...) | **done** | real, plugin-writable components, reach the screen next `Extract` | — |
| Spawn / despawn | done, local-only (client native tier); done, cross-player-visible (server) | `lodestone_ecs::entity_spawn::{spawn_entity, despawn_entity}`, id-safe by construction (plugin ids strictly negative, wire ids non-negative); server-side `IntegratedServer::{spawn_mob, despawn_mob}`, with no place yet for a *second* plugin to object to a spawn | [#138](https://github.com/matteopolak/lodestone/issues/138) |
| Custom entity types | done (disguise-as-vanilla-type) | `CustomEntityRegistry` (`crates/lodestone-ecs/src/entity_spawn.rs`) client-side; server-side any vanilla key works as a disguise, but with no shared registry yet | [#140](https://github.com/matteopolak/lodestone/issues/140) |
| Attribute modification | partial (needs verification) | `Attributes` is plugin-writable; whether a write reaches the wire the way `Position` does is unverified — no client→server attribute-set packet exists in the protocol, so this is a server-side-only write today; server-side, reachable through `SimMob` but unaudited | [#141](https://github.com/matteopolak/lodestone/issues/141) |
| AI-goal manipulation | done (server) | `SimMob::add_goal(priority, Box<dyn Goal>)` (`crates/lodestone-server/src/mobs/mod.rs`) is a real, plugin-reachable AI extension seam — "no AI exists at all" no longer describes this codebase; a ceiling client-side (AI is server-only simulation state) | [#141](https://github.com/matteopolak/lodestone/issues/141) (design) |
| NBT / DataComponent read-write | partial | item component patches exist and are read; plugin write-path unaudited | [#143](https://github.com/matteopolak/lodestone/issues/143) |
| Disguises (packet-level) | gap | depends on the packet-interception design (#156) resolving in the plugin's favour — see Packets below | tracked via #156 |

### Inventories and items

| Java capability | our status | gap | issue |
|---|---|---|---|
| Custom inventory/menu (`createInventory`) | done, local-only (client native tier); gap (server) | `lodestone_game::menus::Menus::open_local` — one menu at a time, refuses to close a server container behind the player's back. Server-side, a plugin still cannot open a menu on a remote player: the container-open packet family and a real server-side container model are both unbuilt | [#145](https://github.com/matteopolak/lodestone/issues/145) |
| Custom items / item components | done (client native tier) | `CustomItemRegistry` (`crates/lodestone-game/src/custom_item.rs`) | [#147](https://github.com/matteopolak/lodestone/issues/147) |
| Runtime recipe registration | done (client native tier) | `RecipeRegistryExt::add_recipe` (`crates/lodestone-ecs/src/recipes.rs`) | [#148](https://github.com/matteopolak/lodestone/issues/148) |
| Anvil/loom/smithing hooks | done (server) | `CraftingStationHooks` (`crates/lodestone-server/src/plugin_crafting.rs`) covers anvil, grindstone, smithing, loom, and stonecutter with an Allow/Deny/Replace verdict — but runs inline on the connection task, so a hook that panics takes that player's connection down | [#150](https://github.com/matteopolak/lodestone/issues/150) (parked) |

### Persistence

| Java capability | our status | gap | issue |
|---|---|---|---|
| `PersistentDataContainer` / metadata | partial | `EntityDataStore`/`ChunkDataStore` (`crates/plugins/lodestone-plugin-support/src/persistent_data.rs`) exist, but are in-memory only by their own module doc — the "non-persistent half" of Bukkit's container | [#152](https://github.com/matteopolak/lodestone/issues/152) |
| Surviving a restart | blocked | world persistence (Anvil format) is itself unbuilt Tier-4 work; the in-memory half (parity with Bukkit's `Metadatable`) is unblocked and should ship now | [#152](https://github.com/matteopolak/lodestone/issues/152) |
| Plugin config files / data directory | gap | no convention; should reuse whatever #67 (existing issue, data-dir de-duplication) settles rather than adding a third implementation | [#153](https://github.com/matteopolak/lodestone/issues/153) |
| Database access from a plugin | **done, trivially** | native tier is unrestricted `std`; a plugin can already open a SQLite file or a Postgres connection like any Rust program. No issue needed. | — |

### Packet-level access

| Java capability | our status | gap | issue |
|---|---|---|---|
| ProtocolLib-class read/modify/cancel/inject, inbound | partial, decided ceiling on the sanctioned surface | `RawPacket` (read-only, off by default) is specified, unbuilt; observation-only is the permanent decision for the shared, version-free crates (a mutate/cancel/inject-at-the-wire trait was considered and rejected — see [`../plugin-api.md`](../plugin-api.md)). A version-locked route around this ceiling exists — see [`../plugin-packet-decorators.md`](../plugin-packet-decorators.md) | [#156](https://github.com/matteopolak/lodestone/issues/156) (design) |
| Outbound mutation/cancellation | done (client native tier), partial (server) | `EgressFilters` (`crates/lodestone-ecs/src/egress.rs`) inspects, replaces, or suppresses a `ClientAction` at the `ActionQueue` drain; five direct `send_action` paths bypass it (`egress_hook_coverage.rs` enumerates them). Server-side, the same `ServerProtocol` decorator that answers the row above can drop, rewrite, or append a `ServerDirective::Send`, version-locked | [#157](https://github.com/matteopolak/lodestone/issues/157) |

### The escape hatch

| Java capability | our status | gap | issue |
|---|---|---|---|
| NMS/internals for whatever the plugin API doesn't cover | **done, and better than Java's** | a plugin may depend on a version crate directly (it's a leaf crate) — version-locks it exactly like NMS reflection version-locks a Paper plugin, except ours is a compile-time `Cargo.toml` fact, not a runtime `ClassNotFoundException` | [#159](https://github.com/matteopolak/lodestone/issues/159) (docs only — no code needed) |

### Client-side plugin surface

| Java capability | our status | gap | issue |
|---|---|---|---|
| World-space custom rendering (waypoints, overlays) | partial | exactly one real instance: `ExtractSet::Debug` + `DebugLines`, landed in `0d82ab4` — a debug line pipeline, not general-purpose | [#161](https://github.com/matteopolak/lodestone/issues/161) |
| Input interception | done (client native tier) | `PluginKeybinds` (`crates/lodestone-ecs/src/input.rs`) claims a physical key in `Consume` mode (nothing below it in the input chain sees the key) or `Observe` mode; an open chat/menu/container always outranks a claim. `lodestone-key-toggle` is a real consumer plugin | [#162](https://github.com/matteopolak/lodestone/issues/162) |
| Camera control | partial (needs verification) | third-person toggle is real and shipped; whether a plugin can *drive* the pose (spectator/cinematic) rather than only observe it is unaudited | [#164](https://github.com/matteopolak/lodestone/issues/164) |
| Custom shaders / replace the render pipeline | **ceiling, by design** | `lodestone-render` carries no bevy dependency and never will (4-bind-group floor, winding-sign invariant); a plugin never gets a `wgpu::Device` | [#165](https://github.com/matteopolak/lodestone/issues/165) (docs the ceiling) |

### Lifecycle and tooling

| Java capability | our status | gap | issue |
|---|---|---|---|
| Manifest + load order + dependencies | gap | "install a plugin" means "add a `Cargo.toml` dependency and rebuild"; Cargo gives crate-level resolution for free, `add_plugins` ordering and soft-deps do not exist as a convention yet | [#166](https://github.com/matteopolak/lodestone/issues/166) (design) |
| Error isolation (one bad plugin ≠ dead server) | gap, and possibly a **ceiling** | `catch_unwind` around a system risks leaving the `World` in a half-mutated state, arguably worse than crashing; may be a documentation answer ("a plugin panic is exactly as fatal as an internal one, by design — the trust model already says fully trusted") rather than a mechanism | [#168](https://github.com/matteopolak/lodestone/issues/168) (design) |
| Hot reload | **ceiling** | Rust has no stable ABI across compiler versions; a reloaded `.so` gets different `TypeId`s for "the same" component, silently breaking every `Query`. Not achievable for the native tier as designed — an argument *for* prioritizing WASM if this is a real requirement | [#169](https://github.com/matteopolak/lodestone/issues/169) (docs the ceiling) |
| Versioned ABI / what breaks across versions | partial | the policy is written down in prose (`plugin-api.md`); nothing enforces it | [#170](https://github.com/matteopolak/lodestone/issues/170) |

### Native vs. WASM (both tiers must express the same features)

| Java capability | our status | gap | issue |
|---|---|---|---|
| WASM host existing at all | done | `lodestone-wasm-host` (`wasmtime`, `wit-component`), `PluginHost::{with_fuel, with_memory_limit, with_filesystem_root, load_file}` — a real, `cfg(not(target_arch = "wasm32"))` host, one export (`on-tick(list<event>) -> list<action>`) over three event kinds and three actions today | [#172](https://github.com/matteopolak/lodestone/issues/172) |
| Capability ABI (queries + actions) | done, narrow | `wit/lodestone-plugin.wit` plus the lift/lower ABI module in `capability.rs`; narrow relative to the native tier (three event kinds, three actions, `log`/`fs:read` imports only) rather than absent | [#173](https://github.com/matteopolak/lodestone/issues/173) |
| Manifest / capability declaration | done | `PluginHost::new(policy)` with `default_policy()` granting everything except `fs:read`, plus a per-plugin `plugin.toml` | [#175](https://github.com/matteopolak/lodestone/issues/175) |
| Verified sandbox (the actual selling point of this tier) | done | trap/fuel/memory limits are three real denial gates in `capability.rs`/`host.rs`, not merely specified | [#176](https://github.com/matteopolak/lodestone/issues/176) |

The WASM tier's own remaining gap is not "does it exist" but "how much of the native
tier's capability surface does it express": roughly a tenth, per
[`../plugin-capability-audit.md`](../plugin-capability-audit.md) — no scheduler
import, no command registration, no block read/write, no entity spawn, and (outside
this crate's own shipped shell, since nothing calls `load_directory` there) not wired
into the client the game ships.

### The correctness constraint underneath all of it

| Concern | our status | gap | issue |
|---|---|---|---|
| `EcsHandle` reentrancy — detected | **done, partially** | `hold_read`/`hold_write` panic instead of hanging; ledger only sees guards taken through those two functions ([#20](https://github.com/matteopolak/lodestone/issues/20), tracked separately for `lodestone-client`'s own ~12 direct call sites) | — |
| `EcsHandle` reentrancy — **unrepresentable** for a plugin author who never read the docs | gap | this is the brief's own top-priority ask, and it is a real design question with no clean answer yet (a plugin can always `Arc::clone` around any wrapper) | [#177](https://github.com/matteopolak/lodestone/issues/177) (design) |
| A test harness a third-party plugin author can run against their own plugin | gap | one bespoke test (`mining_deadlock.rs`) pins one historical bug; nothing reusable | [#179](https://github.com/matteopolak/lodestone/issues/179) |
| `ActionQueue` (shipped) vs. `MessageWriter<SendAction>` (specified) | open question | both work; the shape decision affects every downstream outbound-action issue (#157, #109) | [#181](https://github.com/matteopolak/lodestone/issues/181) (design) |

## Decision records

Closed design questions, written down once so they are not re-derived. Each quotes the owner's own
words rather than paraphrasing them, per this repo's own standard for what counts as a decision on
record.

### #116 — Folia-style region threading is not a goal; closed

[#116](https://github.com/matteopolak/lodestone/issues/116) asked, as a decision record rather
than code: is region-based parallelism ever a goal for this project, and if not, say so on the
record so nobody reopens it. The answer already existed, split across two sibling issues in the
same epic, and this section is that answer collected in one place.

**[#341](https://github.com/matteopolak/lodestone/issues/341)'s scope decision** (Java plugin
compatibility, targeting Paper) states it directly, as the owner's own issue comment:

> **Do not target Folia.** It is a separate fork with regionised multi-threading, and its
> threading model conflicts directly with our single `RwLock<World>` — see the reentrancy
> constraint in the issue body, which is already the hardest part of this design. Folia would
> multiply it rather than help.

That settles the *plugin-compatibility* question: a Folia-authored plugin that assumes
region-sharded scheduling is out of scope, the same way a plugin too old for modern Paper is out of
scope by the same issue's own reasoning.

**[#342](https://github.com/matteopolak/lodestone/issues/342) (regionised server ticking, filed as
a later performance track) records the internal counterpart**, and is explicit that the two
statements are not in tension:

> #341 says do **not** target Folia as a *plugin-compatibility* target. That is a different
> statement, and both hold:
>
> - **Supporting Folia's plugin threading contract** — plugins written to assume regionised
>   scheduling. Out of scope; it multiplies #341's hardest problem.
> - **Our own server adopting regionised ticking** — an internal performance architecture. This
>   issue, and legitimate.
>
> They interact in a useful direction: our single `RwLock<World>` is what blocks both today. If
> this lands, Folia-style plugin threading becomes *possible* to reconsider, where today it is
> structurally excluded.

So: two separate axes, correctly not conflated. **This project may, later and for measured
performance reasons (§342's own sequencing: tick loop → single-threaded parity → benchmarks →
profile → only then decide), regionise its own server tick loop internally.** That is unrelated to
whether this project ever backs Folia's plugin-facing threading contract, which it will not.

**Reaffirmed as permanent for the plugin ABI, independent of whether #342 ever lands:** one
`bevy_ecs::World`, one `GameTick` schedule, one 20 Hz accumulator
(`docs/world-unification.md`'s §4.1(c): "It now holds **one** [`World`]... and that one `World`
carries **one** `GameTick` schedule driven by **one** 20 Hz accumulator"). Every clause in this
doc's doctrine — and every clause in [`../plugin-api.md`](../plugin-api.md)'s intent doctrine —
is written assuming a single writer, a single schedule, and a single ordered tick. §342's own
"what it costs" section is explicit that regionisation would need either one `World` per region or
a provably-partitioned single `World`, and that "global ordering disappears" is one of the costs,
not a side effect it avoids. If #342 ever lands, it changes the **server's own** internal ticking
architecture; it does not retroactively change what a plugin can assume about the client-side
`GameTick` this framework is built on, and it does not reopen #341's Folia answer. A contributor
reading #342 as a reason to revisit either should read this record first.

**Closed:** [#116](https://github.com/matteopolak/lodestone/issues/116), pointing here.

## Port-feasibility analysis

Eight real, well-known Paper/Fabric plugin archetypes, scored against the audit above —
this is the test that catches gaps an API-shaped enumeration misses, because a real
plugin needs several capabilities *at once*, in a specific combination.

| archetype | needs | verdict today | verdict once this epic's issues land |
|---|---|---|---|
| **Protection plugin** (WorldGuard-class: claim a region, veto breaks/places/PvP inside it) | block-break/place cancellation, permission nodes, persistent per-chunk region data | **portable on the integrated/LAN-hosting shell, not on the standalone dedicated server** — `ActionVetoes` (break/place/damage all wired) and `PermissionStore`/`PermissionResolver` both exist and are reachable through `lodestone-ecs`, which the shell's own client-and-server binary links and the standalone `lodestone-dedicated-server` binary does not; region data itself is in-memory only (#152) either way | portable on both binaries once the dedicated server gets the same `lodestone-ecs` reach as the shell (tracked under #118/#125's dedicated-server rows), and once #152 persists past a restart |
| **Economy plugin** (Vault-class: per-player balance, transaction events) | persistent per-player data, custom events other plugins subscribe to, commands with permissions | **portable in-memory on the integrated/LAN-hosting shell only** — commands and permissions are both done there (same dedicated-server gap as the row above); a custom-event convention is still only "any bevy `Message` compiles" with no documented pattern (#107) | portable for the in-memory half everywhere the dedicated server gets `lodestone-ecs` reach; balance *surviving a restart* is still blocked on world/player persistence (Tier 4) |
| **Minigame plugin** (a lobby/countdown/arena manager) | scheduler (delayed/repeating tasks), custom inventories (kit-select GUI), command registration, cancellation of movement/damage during specific phases | **portable on the integrated/LAN-hosting shell, not on the standalone dedicated server** — `TaskScheduler`, `Menus::open_local`, `CommandRegistry` and `ActionVetoes` (break/place/damage/move) all exist and are reachable there; the same `lodestone-dedicated-server` gap as the row above blocks the deployment shape (many remote players on a standalone process) this archetype actually assumes | portable on both binaries once the dedicated server reaches `lodestone-ecs`; this and the protection plugin remain the strongest candidates for a first real ported example |
| **World editor** (WorldEdit-class) | bulk block read/write with undo, region selection, its own command tree | **portable client-side/singleplayer, not for a remote player** — `set_block_with_physics`/`fill_region`/`fill_region_capturing` and the worked `lodestone-worldedit` plugin are real; server-side, `ChunkSource::set_block`/`place_structure_live` land the edit but are not replicated to a connected player, so a plugin write is invisible to anyone but the editor themself in a hosted game | portable for a remote player once the block-change replication path (#129) reaches plugin-driven writes, not only tick-driven ones; undo/redo and region selection stay the plugin's own problem, as on real Paper |
| **Anti-cheat plugin** (movement/combat legitimacy checking) | raw movement/combat packet visibility both directions, high-priority cancellation before the action resolves, per-player flagging with persistence | **portable at the escape-hatch's cost, not on the sanctioned surface** — a `ServerProtocol` decorator (see [`../plugin-packet-decorators.md`](../plugin-packet-decorators.md)) reads, drops, rewrites or appends both directions' wire traffic with no reentrancy hazard, because none of the trait's 64 methods take a `World`/`EcsHandle` argument at all — this is a wire-layer seam, not an ECS one. Cancellation and permission-gated commands are both done (see above). The remaining cost is real: version-locked, unsandboxed, and only reachable by compiling the decorator into the server binary itself, the same way `#159`'s NMS-style escape hatch is — not something a dynamically-loaded (least of all WASM) plugin can supply to an already-built server | *(resolved above — see the escape-hatch cost, not a further issue)* |
| **Holograms / disguise plugin** (fake entities and floating text via packets no real entity backs) | outbound packet injection (spawn/metadata packets the server never sent), or the client-only-cosmetic-entity path | **portable, both paths** — a *client-side-only* hologram (visible to the local player only) is achievable via #138/#140/#161; a *server-broadcast* disguise visible to other real players is the same escape-hatch shape as the anti-cheat row: a decorator's batch-returning methods (e.g. `welcome_message`) can already be shown appending one extra `ServerDirective` the wrapped protocol never sent on its own, and the same shape extends to any `Vec`-returning outbound method | *(resolved above — see the escape-hatch cost, not a further issue)* |
| **Client-side HUD mod** (a Fabric-class minimap/waypoint/info overlay) | custom draw buffer, input interception, no server involvement at all | **input interception is done; general-purpose drawing is the remaining gap** — `PluginKeybinds` (`crates/lodestone-ecs/src/input.rs`) claims a key in `Consume`/`Observe` mode with a real consumer plugin; `DebugLines` is still the only drawing precedent and is debug-shaped, not general-purpose (#161) | **portable**, and the cheapest archetype in this table — the one remaining capability is additive to the existing `Extract`/`FrameSet` seams and touches no ceiling |
| **Pathfinding bot** (a Baritone-class navigator) | analog movement intent, a debug-geometry channel, per-tick collision queries against an owned snapshot, resumable multi-tick search state | **done today**, and it is not hypothetical — `lodestone-nav` (75 tests) plus the missing `lodestone-autopilot` shell (tracked separately, [#38](https://github.com/matteopolak/lodestone/issues/38)) is the one archetype this codebase has already built for real. `TickSet::Intent`, `LookIntent` and `ExtractSet::Debug` — the three gaps `docs/plugin-api.md` named as prerequisites — all closed in `0d82ab4` (see the stale-record note above) | already portable on the **native** tier; confirmed **not** portable on the WASM tier as a matter of architecture, not missing work — `docs/plugin-api.md`'s own cost analysis shows a stateless per-query capability call cannot host a resumable 20,000-node search cheaply, so this archetype is the concrete proof that native and WASM are not substitutes for each other |

**What this table adds beyond the capability list:** the anti-cheat and disguise rows were
the open finding when this audit first ran — #156 named a design question with no proposed
resolution. That question is answered now, by executable proof rather than argument:
`crates/versions/26.2/tests/server/server_protocol_decorator_escape_hatch.rs` and its
client-side twin show a `ServerProtocol`/`VersionAdapter` decorator dropping, rewriting and
appending traffic in both directions, and neither trait's methods take a `World`/
`EcsHandle` argument, so none of this touches the net thread's inline-apply-under-write-lock
behavior at all (`world-unification.md`'s "can ingest stall the frame" section) — it runs
entirely below the ECS seam #177 is about. The resolution is exactly the shape the original
audit predicted as possible: **achievable, at a documented cost**, not "not achievable." The
cost is real and is the same one `#159`'s NMS-style escape hatch already pays — version
lock, no sandbox, and compiled into the server binary rather than installed into one — see
[`../plugin-packet-decorators.md`](../plugin-packet-decorators.md) for the full account.
What is *not* resolved is whether that capability can ever reach a dynamically-loaded
plugin, particularly the WASM tier: crossing a component boundary with a concrete,
version-specific wire type is the thing `bevy-migration.md` §5 forecloses for shared
crates, and nothing about the escape hatch changes that — it works precisely because it
does not cross that boundary.

## The ordered plan

Not a schedule — a dependency order. Four waves, each unblocking the next:

1. **Foundational primitives with no prerequisites, several already flagged P0 by the
   brief.** The event bus (#104), the reentrancy-unrepresentable design (#177) and its
   test harness (#179), the permission-node system (#125), the block write API (#129), the
   entity spawn/despawn API (#138), the scheduler (#113), and the stale-doc fix (#180).
   Nothing else in the epic can start in earnest without at least the event bus and the
   permission system, and #177 should land *before* any issue that adds a new
   plugin-facing entry point into the `World`, per the brief's own framing that ergonomics
   here is a correctness property, not a nice-to-have.
2. **The event-cancellation design decision (#101), and everything that names it as a
   dependency** — the concrete cancelable verbs (#109), monitor priority (#110), priority
   ordering (#105). This is the single highest-leverage decision in the epic: it gates the
   protection-plugin and minigame archetypes, which are otherwise the cheapest real wins
   available.
3. **Commands, world/entity write APIs, inventories, persistence** — mechanical once the
   foundations exist, each independently shippable, most with no cross-dependency on each
   other (#118→#119→#122, #131 on #129, #145, #147, #152, #153).
4. **The one open-ended track, which can run in parallel with everything above but should
   not block a v1:** the WASM host (#172→#173→#175→#176, an epic-sized effort in its own
   right) — no longer the packet-interception design (#156), which is answered: a
   `ServerProtocol`/`VersionAdapter` decorator gets bidirectional interception at a
   documented, version-locked, unsandboxed cost, tested in
   `crates/versions/26.2/tests/server/server_protocol_decorator_escape_hatch.rs` and its
   client-side twin (see the Verdict section below). What #156 should still resolve is
   narrower: whether that capability can ever reach a dynamically-loaded plugin, above all
   the WASM tier. Lifecycle/tooling design issues (#166, #168, #169, #170) are cheap to
   resolve early since most of them are documentation-shaped decisions, not implementation,
   and resolving them early avoids a contributor rediscovering the same question
   mid-implementation.

## Verdict: is "port any Java plugin" achievable as stated?

**No, not as stated, and the qualification is narrow and specific rather than a broad
hedge.** Of the roughly 15 Bukkit/Paper/Fabric capability families audited above, 13 are
either already real, cheaply closeable by an issue filed in this epic, or a *documented
ceiling* this project is explicitly right to accept (no `wgpu::Device` for a plugin, no
hot reload for the native tier, no novel wire-protocol types). Those ceilings do not
violate the spirit of the claim — Java plugins hit equivalent ceilings too (no plugin
replaces the JVM's renderer either; there isn't one to replace).

**The qualification that was open, and is now resolved:** packet-level interception in the
direction that matters for anti-cheat and true disguise plugins (outbound mutation/
injection, both directions) turned out to be buildable without reopening the reentrancy
hazard this epic's top-priority issue exists to close — a `ServerProtocol`/`VersionAdapter`
decorator does it at the wire layer, where neither trait ever hands a method a `World` or an
`EcsHandle`, so there is nothing to reenter. `crates/versions/26.2/tests/server/
server_protocol_decorator_escape_hatch.rs` and its client-side twin are the executable proof:
drop, rewrite and append, both directions, no ECS access. The resolution costs exactly what
[`../plugin-packet-decorators.md`](../plugin-packet-decorators.md) documents — version lock,
no sandbox, compiled into the server binary rather than installed into one — the same price
`#159`'s NMS-style escape hatch already pays. What is **not** resolved, and is a genuinely
open question rather than a closed one dressed up as open: whether that capability can ever
reach a *dynamically-loaded* plugin, above all the WASM tier, without leaking a
version-specific wire type across the boundary `bevy-migration.md` §5 treats as inviolable
for shared crates. The escape hatch sidesteps that question rather than answering it — it
works precisely because it never crosses the boundary in the first place.

**Restated as the honest version of the claim:** *any Bukkit/Paper/Fabric plugin whose
capability needs are drawn from the other fourteen families is portable, once the 49
sub-issues here land.* The fifteenth family — direct, bidirectional packet manipulation — is
now known to be portable too, for a plugin willing to be compiled into the server binary
directly and to accept a version lock; the asterisk that remains is narrower than before,
and belongs to the dynamically-loaded (WASM-tier) case only, which #156 should now scope to
that question specifically rather than to buildability in general.

## See also

- [`../plugin-api.md`](../plugin-api.md) — the surface as a specification, including the
  now-corrected gap list (see [#180](https://github.com/matteopolak/lodestone/issues/180)).
- [`../plugin-packet-decorators.md`](../plugin-packet-decorators.md) — the version-locked
  escape hatch this doc's Verdict and port-feasibility sections lean on, with the executable
  proof for every verb in both directions.
- [`../bevy-migration.md`](../architecture.md) — the staged ECS plan; §6/§6.1 are the
  plugin-API and two-tier sections this doc's audit checks against the real tree.
- [`../world-unification.md`](../architecture.md) — the lock-discipline section every
  reentrancy-adjacent issue in this epic (`#156`, `#157`, `#177`, `#179`) must be read
  against before implementation starts.
- [`../baritone-port.md`](../autonomous-navigation.md) — the one archetype in the port-feasibility
  table that is already real, and the source of the WASM-cost analysis this doc leans on
  for the native-vs-WASM verdict.
- [`./README.md`](./README.md) — the roadmap index; epic #7 (substrate) vs. epic #77
  (capability parity) is explained there and repeated here because conflating the two is
  the most likely misreading of this doc.
