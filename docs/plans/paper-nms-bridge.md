# Backing Paper's NMS calls with Rust: census and feasibility

## What it is

This feasibility census establishes what it would take to run real, unmodified Bukkit/Spigot/Paper
plugin jars against this server through Rust-backed compatibility classes. The verdict is
**viable only as the last plugin, not the first**: every seam the JVM tier needs is a seam the
public bevy-plugin API must expose anyway, none of those seams is reachable today, and the JVM
tier itself should not start until the adjudication window and player registry exist.

This plan is a current-state design and evidence guide. Symbol references identify durable
integration boundaries; re-check their current consumers before implementation.

## The question, reframed

The compatibility layer is not a special server subsystem. Two architectural constraints define it:

1. **The server ECS architecture uses `bevy_ecs` in `lodestone-server`**
   (`docs/dedicated-server.md` and [`server-ecs-migration.md`](./server-ecs-migration.md)).
   `ServerApp::bootstrap` builds the server `World`, installs `ServerCorePlugin`, and
   `IntegratedServer` moves that world into the tick task. It is held
   by the tick task **with no lock at all**, and a scheduled packet-apply whose `Adjudicate` set is
   the only place an event handler can veto or modify an action before it applies.
2. **The plugin API and the internal API are the same surface**
   (`docs/dedicated-server.md`). Core game
   systems should themselves become bevy plugins where it makes sense — physics as a plugin, so a
   headless bot run can omit it.

So the right question is no longer "how do we build a JVM bridge into the server?" but: **what
must the public bevy-plugin API expose so that an external, unprivileged plugin could implement
the JVM compat layer?** The Java tier is one plugin among several — it hosts a JVM, exposes an
NMS-shaped class surface to Paper's own bytecode, and translates every NMS call into the same
public API every other plugin uses. If it needs a privileged back door, the public API is not
finished; that is a defect in the surface, exactly the capability-boundary rule stated in
`docs/plugin-api.md`.

The compatibility policy is: **Paper only** (Bukkit/Spigot follow from Paper's bytecode);
**partial NMS coverage is fine, but failures must be loud** —
an unimplemented member throws immediately, naming itself, never returns `null`/default/no-op;
**census is a prioritisation tool**, ranked by reference count across real plugin jars; **do not
target Folia**; **keep it modern** — current Paper line and runtime naming scheme, no legacy shims; **settle
licensing before writing code** (Paper is GPL-3.0; classload interception plus a user-supplied
Paper jar avoids redistributing derived bytecode).

## The census

### Sizing

The Java side is grounded in a local census of the 26.2 implementation corpus: **4,839 internal
source files**. The surface Paper's implementation layer touches is dominated by six internal
types:

| what it is | public+protected members |
|---|---|
| the base entity type | 560 (4,183 lines) |
| the dedicated-server singleton | 211 |
| the server-side player type | 175 |
| the world/level interface | 154 |
| the server-side level type | 146 |
| the item-stack type | 120 |

These numbers are scope context, not usage counts: they are members *declared*, not members Paper
*references*. A bytecode census that scans Paper's constant pool is required to turn them into a
ranked worklist.

26.2-specific signature drift a plan written from older knowledge would get wrong: the menu-click
entry point's third parameter is now a structured input object, not the older simple click-type
enum; the block-destroy-acknowledgement entry point gained a string exit-id parameter; and **the
item-stack type has no save/parse methods any more** — serialization is purely codec-based, with
the generic save layer built on a value-output/value-input pair, and the base entity type's own
save/load methods take that same value-output/value-input pair, with no NBT compound type in the
signature.

### Reachability buckets

Each seam is classified by what the *public bevy-plugin API* offers the compat plugin:

- **(a)** — expressible today through the public plugin API.
- **(b)** — needs a public seam that is planned or enumerable through the server ECS migration,
  player registry, or native plugin API work.
- **(c)** — structurally out of scope as currently designed; stated why.

**The headline: bucket (a) is empty.** `ServerApp` and `ServerCorePlugin` build the internal
server `App` and `World`, but no supported public plugin-registration seam exposes them to an
external plugin. Every row below that has a Rust mechanism still lands in (b), because the
mechanism is crate-internal with no public seam in front of it.

### The seam table

| # | capability needed | Rust today | gap | bucket |
|---|---|---|---|---|
| 1 | Block read: reading a block's state at a position returns an interned flyweight object (~25 precomputed fields plus a face-sturdiness cache) compared **by identity** in the compatibility target | The server's world model is canonical block-state **`String`s** in a per-column `Vec<String>` palette — `lodestone-server` does **not** depend on `lodestone-world` (dev-dep only, `crates/lodestone-server/Cargo.toml`); the client separately uses `u32` state ids via `lodestone_ecs::ChunkWorld` | Two disconnected Rust representations, neither identity-interned; a shim needs a stable palette-id ↔ interned-Java-object mapping, not a struct copy | (b) |
| 2 | Block write: setting a block's state at a position, with a flags bitmask controlling update/notification behaviour | `set_block` exists only inside `apply_block_action` (`crates/lodestone-server/src/server.rs`), inline in the connection task, veto-free | No public write API server-side; scheduled application in the server ECS design is the required seam | (b) |
| 3 | Chunk access: a synchronous load-or-generate call for a chunk at given coordinates, up to a given generation stage | `lodestone-worldgen` generates columns; no on-demand public chunk API, no load/unload lifecycle a plugin can observe | Worldgen is a bit-exact oracle library; adding a plugin seam must preserve that contract | (b) |
| 4 | Entity manipulation: the base entity type (560 members), a level-level add-entity call, a server-side hurt call, teleport, remove | `MobSim` (`crates/lodestone-server/src/mobs/mod.rs`); the **only** dynamic-dispatch extension point in the crate is `SimMob::add_goal(priority, Box<dyn Goal>)` (`mobs/mod.rs`) | No public spawn/remove/mutate seam | (b) |
| 5 | Player object model: the server-side player type (175 members, including its connection and open-container-menu fields), a server-wide player-list broadcast, lookup by name | **No player entities, no player registry, no broadcast.** Every send is against one `&mut Connection<T>` owned by one connection task; the singleplayer server retains no handle to any connection. | The whole Bukkit player/broadcast-message/online-players surface requires player entities and a registry | (b) |
| 6 | Inventory/menus: a click entry point (slot, button, a structured input, the clicking player), a broadcast-changes call, a listener-registration list | Per-connection container sync (`CONTAINER_SYNC_INTERVAL`, `server.rs`); block entities server-side are four Rust structs — `Furnace`, `Hopper`, `Composter`, `BrewingStand` — no chests | No synthetic-menu path; container state must be reclassified from per-connection replication to simulation state | (b) |
| 7 | Items: the item-stack type (120 members) over an identity-keyed, copy-on-write component map layered on a shared prototype | `lodestone_model::ItemStack` is a **closed struct of known fields** (`crates/lodestone-model/src/item.rs`, `ItemComponents` in the same file); components a build does not model are **dropped** | A custom-item-data plugin round-trips components we discard — silently lossy, which violates the loud-failure rule by construction | **(c)** until `ItemComponents` carries unknown components opaquely; then (b) |
| 8 | Scheduling: the dedicated-server singleton extends a reentrant main-thread task queue; its execute/submit calls are gated on a same-thread check — the main-thread contract Bukkit's scheduler is built on | `ServerApp::bootstrap` installs `ServerCorePlugin` and runs `ServerBoot`; `IntegratedServer` owns the resulting `World`, but `GameTick` is not yet driven by `run_tick_loop` | Bukkit's delayed/repeating-task scheduler needs a public registration seam and a live game-tick schedule | (b) |
| 9 | Packet send: a per-connection packet-listener send call that hands off to the network-connection layer, with network-library types (a channel-future listener, a byte buffer) in the signature | `ServerDirective` (`crates/lodestone-server/src/protocol.rs`) carries only `Send`/`SetState`/`SetCompression`/`None`; `ServerProtocol` (`protocol.rs`) is a closed, hand-enumerated encoder list, one method per packet, defaulted no-op bodies, ~24 implemented (`crates/versions/26.2/src/server_protocol.rs`'s `impl ServerProtocol for V770ServerProtocol`) | *Typed* sends can shim onto encoders once a player registry supplies addressing. *Arbitrary* packet objects and pipeline injection: see row 11 | (b) |
| 10 | NBT/serialization: a generic compound-tag container, plus a codec-based value-output/value-input save layer | NBT lives in `lodestone-core` (`NbtTag` `:100`, `Nbt` `:173`, `Compound(Vec<(String, Nbt)>)`) and **`lodestone-server` never touches it** — block entities are plain Rust structs; NBT block entities exist client-side only | A compound-tag shim can wrap `lodestone_core::Nbt` cheaply, but the server has no NBT-shaped state for it to address | (b) |
| 11 | Raw packet interception (ProtocolLib-class): network-library pipeline injection via reflection into the connection's channel | Inbound `ServerBound` (`protocol.rs`) is a closed 21-variant enum ending in `Ignored` with **no raw-packet passthrough**; no network-library dependency, no channel, no pipeline exists to inject into | This remains structurally out of scope; `docs/roadmap/plugin-framework.md` identifies it as not currently known to be buildable | **(c)** |
| 12 | Events: **the compatibility target has no general internal event bus** — its listener-shaped hooks are container-local or synchronised-data-specific. The Bukkit event surface comes from Paper patches (see the cut line below) | `ServerApp` and `ServerCorePlugin` provide a bootstrapped, production-owned `World`, but no event bus, cancellation, or hook registration. `dispatch_play_packet` still applies actions inline. | Every Bukkit event needs an `Adjudicate` window in the live game-tick schedule | (b) — **the load-bearing row** |
| 13 | Commands: Brigadier dispatcher, per-node permission predicate | `lodestone-command` (1,388 lines) is a self-declared island with zero consumers; its `Node.permission: Option<NodeId>` field is read by nothing | Needs server-side command dispatch and plugin registration | (b) |
| 14 | Permissions/ops: the compatibility target's command permission check; Bukkit permission nodes on top | **No permission model or op system exists at all** — `dispatch_play_packet`'s `ChangeGameMode` arm says so explicitly; the check is deliberately skipped and every connection is treated as the singleplayer owner (`apply_difficulty_change`'s own doc comment) | Needs permissions and operator state | (b) |

**Census size: 14 seam categories. Bucket (a): 0. Bucket (b): 12. Bucket (c): 2** (raw-packet
interception; lossless item components — the second is fixable by opening `ItemComponents`, the
first remains an open design question). A Rust *mechanism* exists behind roughly
five of the fourteen (block write, mob sim, container sync, typed packet send, the tick loop),
but in every case it is crate-internal, veto-free, and reachable by nothing outside
`lodestone-server`. A standalone JNI invocation spike now exists under
`crates/plugins/lodestone-jvm-bridge/spike/invocation/`; it is deliberately excluded from the
production workspace. The production bridge still has no JVM linkage or startup path.

## Finite delivery decomposition

The compatibility work is complete only when these seven bounded domains meet their acceptance
gates. This replaces an open-ended sequence of individual native getters with a finite closure
model:

| domain | census rows | completion evidence |
|---|---:|---|
| world, chunks, and blocks | 1-3 | Stable state identity, authoritative writes and update flags, explicit generation policy, and measured batched region access |
| entities and players | 4-5 | Generation-safe lifecycle plus production-observable spawn, removal, teleport, damage, messaging, and supported mutation |
| inventories, items, menus, and serialization | 6-7, 10 | Authoritative menu state and lossless unknown-component handling or loud refusal |
| scheduler, commands, and permissions | 8, 13-14 | Deterministic tick scheduling, bounded sync hand-back, command dispatch, and permission predicates |
| events and adjudication | 12 | Supported-event census with differential ordering, cancellation, mutation visibility, and exception isolation |
| typed packets and interception boundary | 9, 11 | Real addressed typed sends and an explicit, tested raw-interception decision without fake channel objects |
| real-plugin conformance | all | Reproducible user-supplied runtime harness, machine-readable differential results, and unchanged JVM-disabled cost |

Each domain must enumerate the supported members it owns. Anything outside that enumeration throws
immediately with its exact member identity. The final conformance domain selects maintained,
unmodified plugins spanning world editing, permissions or economy, and broad server behavior; a
green unit suite inside the bridge is not completion evidence by itself.

## The cut line: where Paper's bytecode stops and ours starts

**This is the centrepiece finding.** Paper's bytecode provides a wrapper layer (a player wrapper
delegating to an internal counterpart, a world wrapper doing the same), but exposing Rust-backed
internal objects does not provide an event bus, listener priorities, or cancellation semantics.
Bukkit events fire **inside Paper's patched internal method bodies**, not above the compatibility
target's internals. Paper's patch set for the block-break/game-mode handling path inserts its
event construction there:
Paper inserts its own event-construction calls and a real `org.bukkit.event.block.BlockBreakEvent`
directly into the internal block-break-handling, block-destroy and use-item entry points, checks
the event's cancelled flag inline, and
sends the corrective block-update packet from inside the same body. The 26.2 behavioural reference
has **no general internal event bus** — so no event-firing seam "falls out of" the compatibility
layer.

Consequence: backing internal *leaves* (block storage, entity fields, sends) with Rust does not buy
the event bus, and the compat plugin must choose one of two cuts:

1. **Drive Paper's patched game-logic bodies from the adjudication window** — call
   the internal block-break-handling entry point in the JVM and let Paper's patched body fire the
   event, check cancellation, and call down into the internal block-write shim. This gets
   Paper semantics verbatim, and it is a trap: the JVM then *re-executes game logic
   our Rust server also implements* — two simulations of one world, with the JVM's copy calling
   back into Rust leaves on hot paths (a block-state read in a loop is exactly where per-call JNI
   overhead is catastrophic), and every behavioural divergence between the two is a
   consistency bug with no owner.
2. **Fire Bukkit events from our adjudication system** — the compat plugin's own adjudication
   system constructs the Bukkit event objects (playing the role Paper's own internal
   event-construction helper plays), dispatches them through Paper's real event bus (its listener
   registration — Paper's bytecode, unmodified), reads the verdict, and returns allow/deny to the
   `Adjudicate` set. Paper's bytecode is used for the wrapper layer, the event bus, the plugin
   loader, and the Bukkit API classes; Paper's patched *NMS game-logic bodies are never driven*.
   The Rust server remains the only simulation.

**Cut 2 is the only viable shape.** Its cost is honest: the set of events we fire, and their
exact firing order and field semantics, becomes *our* responsibility to match Paper's. The
acceptance test runs the same plugin against our server and real Paper and diffs behaviour; it is
the instrument that measures that responsibility. Cut 2
also collapses the NMS surface we must back: not "everything Paper's server internals touch,"
but "everything Paper's *wrapper classes* touch when a plugin calls the Bukkit API" plus
"everything plugins reach via NMS directly" — which is what the bytecode census, when run,
should count (scan the CraftBukkit wrapper classes and a corpus of real plugin jars, not the
whole of `paper-server`).

**Evidence asymmetry, stated plainly:** the local runtime bundle contains **no
Bukkit/CraftBukkit/Paper source** — zero files matching `org.bukkit`; its bundled `org` libraries
are limited to `apache`, `joml`, `jspecify`, and `slf4j`. Every claim in this document about Paper's side —
the patch mechanism, Paper's own internal event-construction helper, the wrapper structure,
GPL-3.0 licensing, Paper
using its current runtime naming scheme — rests on Paper's public repository
([PaperMC/Paper](https://github.com/PaperMC/Paper)), **not on a local
artifact**. The Java-side census above is verified locally; the Paper-side claims are external
and should be re-verified against a real Paper 26.2 jar as step one of any implementation.

## The JNI/FFI boundary, answered

The server's `World` is **tick-thread-owned with no lock at all**
(`docs/dedicated-server.md`). That is *stricter* and *simpler* at once — there is no
lock to deadlock on, and also no lock to hand a foreign thread. The analysis under the new model:

- **Dispatch happens on the tick thread, inside an exclusive system.** bevy gives any ordinary
  plugin `&mut World` via an exclusive system — public API, no privilege. The compat plugin
  registers one in the `Adjudicate` window; Java event handlers run synchronously inside it,
  exactly as Bukkit handlers run synchronously on the main thread. While a handler runs, JNI
  upcalls (a shim's native methods) service NMS calls against a scoped thread-local pointer to
  the `World`, valid only for the duration of the dispatch — set on entry, cleared on exit.
  Bukkit's synchronous read-your-writes contract (`block.setType(STONE)` then
  `block.getType() == STONE`) holds *for free*, because the imperative call really did mutate the
  authoritative `World` mid-dispatch. This mirrors the compatibility target's contract: its
  same-thread check gating its main-thread task queue's execute/submit calls is the same affinity
  rule with a queue behind it.
- **Off-thread access throws; it cannot block.** A Bukkit async task calling a world method gets
  an `IllegalStateException` naming the thread — Paper's own behaviour for async world access.
  It must *never* wait for the tick thread: the tick thread does not yield mid-tick, so a
  blocking wait is a deadlock by construction. `callSyncMethod`/scheduler hand-back maps onto a
  queue drained by the compat plugin's own scheduled system — the same shape as the client's
  `ActionQueue`.
- **Object identity and lifetime.** Plugins hold `Player`/`World`/`Block` references across
  ticks. Shim objects wrap **handles, never pointers**: `bevy_ecs::Entity` is already a
  generational index, the required fail-gracefully-when-gone shape — a stale handle throws,
  naming the entity. The block-state flyweight needs more: the target runtime
  and plugin code compare states **by identity** (`==`), so the Java side must intern one shim
  block-state object per state, keyed by a stable id — and our tree currently has *two* disconnected state
  models to key against (the server's canonical `String` palette and the client's `u32` ids; see
  seam 1). The interning registry forces that unification question early, which is a benefit in
  disguise.
- **Exceptions vs panics.** A Java exception thrown by a handler is caught at the dispatch
  boundary, logged, and the handler skipped — Bukkit's own contract. A Rust panic must never
  unwind across a JNI frame (undefined behaviour), so every `extern` boundary wraps in
  `catch_unwind` and rethrows as a Java `RuntimeException`; the half-mutated-`World` concern this
  raises is shared with native plugins, not a new class. Note `unsafe_code = "deny"` is workspace-wide but
  binds only crates opting into workspace lints. An external plugin crate chooses its own
  workspace-lint settings, which is what makes a JNI crate under
  `crates/plugins/` legal at all.
- **What is structurally impossible**, not merely hard: (1) **network-library pipeline injection** —
  the target runtime's per-connection send call carries network-library types in its signature, and
  we have no channel, no pipeline, no byte-buffer type; a shim can accept *typed* packets and translate
  to `ServerDirective::Send` where an encoder exists, but a ProtocolLib-class plugin reflecting
  into the connection's channel has nothing to find, and `ServerBound`'s closed 21-variant enum with
  no raw passthrough means inbound interception is equally closed. (2) **Synchronous world access from a foreign thread** — no lock exists to take;
  throw is the only correct answer. (3) **Lossless custom item components** through today's
  closed `ItemStack` struct — dropped components are a silent corruption, which the loud-failure
  rule forbids; until `ItemComponents` carries unknown components opaquely, item-meta-heavy
  plugins must be *refused loudly*, not half-supported.
- **Marshalling cost.** Under cut 2, JNI calls are plugin-initiated — event dispatch (only for
  events with a registered Java listener, materialising objects lazily) and Bukkit API calls.
  That is low-frequency relative to simulation. The exception is WorldEdit-class bulk access,
  which needs a batched shim (region snapshot into the JVM, one crossing) rather than per-block
  calls — the same conclusion `docs/plugin-api.md`'s WASM cost analysis reached for the
  pathfinder, one boundary over.

## The intent doctrine is the conformance spec, not an obstacle

The intent doctrine maps Bukkit's imperative API onto the server model, clause by clause
(`docs/dedicated-server.md` and [`server-ecs-migration.md`](./server-ecs-migration.md)):

1. **Observation vocabulary** — a Bukkit event *is* observation vocabulary: `BlockBreakEvent`
   carries a block and a player, not a packet. The compat plugin translates NMS calls into the
   same world-fact vocabulary at the shim boundary.
2. **One system owns each machine** — holds. The compat plugin's exclusive system is serialized
   in the schedule and explicitly ordered; its imperative writes go through the same public write
   API the owning systems use. Ambiguity detection (`LogLevel::Error`, already the client's
   standard) is the gate that keeps this honest.
3. **Refusal is always observable** — the loud-failure rule is this clause applied to the compat
   layer: an exception naming the specific unimplemented member (e.g. the server-level chunk-source
   accessor) is refusal-made-observable for a calling plugin, and the corrective packet
   (the dedicated-server action boundary) is refusal-made-observable for the remote client.
4. **Server-side, the plugin outranks the client** — the dedicated-server model
   is *exactly* Bukkit's cancellation model: `event.setCancelled(true)` is a plugin overruling a
   remote client's proposal in the adjudication window. Bukkit's `LOWEST..MONITOR` priorities map
   onto system ordering within `Adjudicate`.
5. **Lifecycle encodes verb shape** — mostly not applicable, as the migration plan already
   concluded: a server plugin is authoritative, not a wisher; the adjudication window is what
   matters.

**What genuinely resists wish-shaping**, named rather than smoothed over: (i) mid-handler
imperative mutation that subsequent handlers and the eventual apply must observe in Paper's exact
order — solvable only because dispatch is synchronous on the tick thread, and the firing-order
contract becomes ours to match (the diff-against-real-Paper harness is the gate); (ii) raw packet
mutation (seam 11) — not a doctrine tension, an unresolved design boundary that stays open
as it does for native plugins; (iii) plugins that spawn threads and touch the world — refused by
throw, same as Paper.

## Relationship to the server ECS and native plugin surface

The server ECS migration is the substrate for this compatibility layer, not an alternative to it.
The public native plugin API must exist first; the JVM tier is one ordinary consumer of that API.
Every bucket-(b) row depends on the same server capabilities. Build them in this order:

1. A server `App`, `GameTick` schedule, scheduled packet application, and `Adjudicate` set
   (`docs/dedicated-server.md` and [`server-ecs-migration.md`](./server-ecs-migration.md)).
2. Player entities, a registry, and broadcast, because nearly every Bukkit API call resolves a
   `Player`.
3. Server-side event construction, cancellation, and ordered listener dispatch.
4. Permissions, scheduling, public block writes, spawn/despawn, persistence, and menus.
5. A decision on raw packet interception. The JVM tier inherits that native boundary and adds
   nothing to it.

The JVM tier will add an in-process host, bytecode-census-driven shim generation, an
interning/handle registry, event construction, class-loader interception, and a behaviour-diff
harness against real Paper. This is strictly additive work for one payoff native plugins cannot
provide: running *unmodified Java jars*. Deferring it does not delay any native prerequisite.

### Executed mechanism slice

The standalone invocation spike at
`crates/plugins/lodestone-jvm-bridge/spike/invocation/` now exercises the smallest JNI-to-Rust
world round trip without entering the production graph. A synthetic plugin is loaded through a
platform-parented class loader, its native method is registered at runtime, and the callback is
serviced through `service_with_world` while a real ECS world is borrowed for one request. The
returned value includes a seeded world fact (`WORLD:present`); after the JVM call, the harness acquires
the same world's write guard again. That pair is the executable proof that the callback saw the
real world and did not leave its guard held across the foreign call.

The control and failure arms remain part of the same runner: real-versus-shim class loading,
unregistered native methods, a dropped or silent servicer, callback panic translation, opaque
handle invalidation, and bounded nested callbacks. Run it with
`crates/plugins/lodestone-jvm-bridge/spike/invocation/run.sh`; the ordinary production workspace
continues to link no JVM and starts no Java runtime.

## Verdict, and the dispatchable next step

**Verdict: viable-for-subset, strictly downstream.** Viable under cut 2 for the plugin
archetypes whose needs are bucket (b) — protection, economy, permissions, minigames, world
editing (batched). Not viable, pending the raw-interception decision, for anti-cheat and
packet-injection archetypes — the same two rows `docs/roadmap/plugin-framework.md`'s
port-feasibility table already flags for native plugins. Not startable now: bucket (a) is empty
because the server has no plugin registration point at all.

**The single strongest piece of evidence:** the event-bus finding. The behavioural reference has
no general event bus (verified locally, 4,839 files); our server has no event bus, no cancellation, and
no hook registration of any kind (verified locally — `dispatch_play_packet` applies inline,
veto-free); Bukkit's event bus lives in Paper's patched NMS bodies (verified against Paper's
public patch file). Event and cancellation semantics must therefore be built natively first, as
the `Adjudicate`
window, regardless of whether a JVM ever attaches.

**Dispatchable now:** build the native prerequisites above. The bytecode census and a standalone
classloader/JNI/port invocation spike now exist, so the remaining work can be decomposed into
measured mechanism slices without wiring a JVM into the production graph.

**The next measurement:** obtain a real Paper jar for
the current line plus 3–5 real plugin jars (one protection, one economy, one kitchen-sink); walk
each method's `Code` instructions for internal member operations, retain constant-pool symbols as
separate descriptor/bootstrap context, and rank static instruction sites by count and field direction. Check
the result against this document's seam table. First verify that Paper has a 26.2 release using
its current runtime naming scheme; the local runtime bundle cannot establish that.

**The smallest production vertical slice, when prerequisites exist:** one real, unmodified
protection-style plugin jar (compiled against the Bukkit API, *not* against our shims — evidence
must originate outside the code under test) whose `BlockBreakEvent` listener cancels breaks
inside a region. JVM in-process, Paper's plugin loader and event bus running Paper's own
bytecode, our adjudication system firing the event, one internal seam backed
(the server-level/block-state-read seam, for the listener's region check). **Gate:** a break inside
the region leaves the server's world unchanged and a corrective block update reaches the wire; a
break outside applies. **Negative control:** the same run with the listener unregistered must
break the block inside the region — proving the veto path, not the apply path, is what the gate
measures. **Loudness control:** the plugin calling any unimplemented compatibility member must
produce an exception naming the member — asserted in the harness, not described.

## The closing argument: our own systems are the proof

Core systems should themselves become bevy plugins where it makes sense. Physics is the worked
example a headless run omits and is the strongest available test of everything above; it needs no
JVM. The compat layer's entire premise is that the public
plugin API is sufficient to implement a server subsystem from outside. **We can prove or refute
that premise with our own code first**: if `MobSim` (or physics) can be re-registered through
the same public `App` seam an external plugin would use — same schedule labels, same world
access, no crate-private back door — the API is demonstrated sufficient by construction, and the
Java tier is just one more plugin. If it cannot, the API is not finished, and we will have found
that out on a subsystem we control, with Rust error messages, instead of three layers deep in a
JNI stack trace under someone else's plugin jar. Sequence the conversion of one internal system
into a plugin *before* wiring production JVM code; treat any privilege it turns out to need as a
defect in the surface, per the doctrine's own rule.

## Sources

External (no local Paper artifact exists to verify against — see "Evidence asymmetry"):

- [PaperMC/Paper patches](https://github.com/PaperMC/Paper/tree/main/paper-server/patches) —
  Bukkit event calls, cancellation checks, and corrective sends are inserted into internal method
  bodies.
- [PaperMC/Paper](https://github.com/PaperMC/Paper) — patch-based architecture, GPL-3.0.

Local: the 26.2 implementation census, the Lodestone tree,
`docs/dedicated-server.md`, [`server-ecs-migration.md`](./server-ecs-migration.md),
`docs/plugin-api.md`, and `docs/roadmap/plugin-framework.md`.
