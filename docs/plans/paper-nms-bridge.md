# Backing Paper's NMS calls with Rust: census and feasibility

## What it is

The feasibility census issue [#341](https://github.com/matteopolak/lodestone/issues/341) asked
for before any design: what it would take to run real, unmodified Bukkit/Spigot/Paper plugin jars
against this server by supplying `net.minecraft.*`-shaped classes backed by Rust. The verdict is
**viable only as the last plugin, not the first**: every seam the JVM tier needs is a seam the
public bevy-plugin API must expose anyway, none of those seams is reachable today, and the JVM
tier itself should not start until the adjudication window and player registry exist.

This is a plan, read-only against the tree as of 2026-08-04. Nothing here is implemented, and
several cited files are mid-edit by concurrent agents — treat `file:line` references as samples
taken on that date, not durable coordinates.

## The question, reframed

The issue's original framing was "back Paper's NMS calls with Rust, rather than reimplementing
Bukkit" — a bridge as a special subsystem. Two decisions made since then reframe it:

1. **[#433](https://github.com/matteopolak/lodestone/issues/433) is decided** — option 1, adopt
   `bevy_ecs` in `lodestone-server` (`docs/server-ecs.md`, commit `f0d22a1`, decision record only;
   nothing in `crates/lodestone-server/` implements it yet). The server gets its own `World`, held
   by the tick task **with no lock at all**, and a scheduled packet-apply whose `Adjudicate` set is
   "the single strongest architectural argument for the whole migration."
2. **The owner's standing rule is that the plugin API and the internal API are the same thing**
   (`docs/server-ecs.md` §motivating constraint), extended by a further scope point: core game
   systems should themselves become bevy plugins where it makes sense — physics as a plugin, so a
   headless bot run can omit it.

So the right question is no longer "how do we build a JVM bridge into the server?" but: **what
must the public bevy-plugin API expose so that an external, unprivileged plugin could implement
the JVM compat layer?** The Java tier is one plugin among several — it hosts a JVM, exposes an
NMS-shaped class surface to Paper's own bytecode, and translates every NMS call into the same
public API every other plugin uses. If it needs a privileged back door, the public API is not
finished; that is a defect in the surface, exactly the frame `docs/plugin-api.md`'s "two
consequences" section already mandates.

Owner decisions from the #341 comment thread that bind this plan: **Paper only** (Bukkit/Spigot
fall out of Paper's own bytecode); **partial NMS coverage is fine, but failures must be loud** —
an unimplemented member throws immediately, naming itself, never returns `null`/default/no-op;
**census is a prioritisation tool**, ranked by reference count across real plugin jars; **do not
target Folia**; **keep it modern** — current Paper line, Mojang-mapped, no legacy shims; **settle
licensing before writing code** (Paper is GPL-3.0; classload interception plus a user-supplied
Paper jar avoids redistributing derived bytecode).

## The census

### Sizing

The Java side is grounded in the decompiled Mojang-mapped 26.2 source at `.cache/mc/26.2/src`
(the only version under `.cache/mc/` with decompiled source at all): **4,839 `.java` files under
`net/minecraft`**. The surface Paper's implementation layer touches is dominated by six classes:

| class | public+protected members |
|---|---|
| `net.minecraft.world.entity.Entity` | 560 (4,183 lines) |
| `net.minecraft.server.MinecraftServer` | 211 |
| `net.minecraft.server.level.ServerPlayer` | 175 |
| `net.minecraft.world.level.Level` | 154 |
| `net.minecraft.server.level.ServerLevel` | 146 |
| `net.minecraft.world.item.ItemStack` | 120 |

These numbers are the honest scope figure the bucket counts below should be read against — and
they are members *declared*, not members Paper *references*; the bytecode census the issue asks
for (scan Paper's constant pool) is still the instrument that turns this into a ranked worklist.

26.2-specific signature drift a plan written from older knowledge would get wrong:
`AbstractContainerMenu.clicked`'s third parameter is a `ContainerInput` object, not the older
`ClickType`; `ServerPlayerGameMode.destroyAndAck` gained an `exitId` String parameter; and
**`ItemStack` has no `save`/`parse`** — serialization is purely codec-based (`MAP_CODEC`,
`CODEC`, `STREAM_CODEC`), with the generic save layer being `ValueOutput`/`ValueInput`
(`TagValueOutput`), and `Entity.save`/`load` take `ValueOutput`/`ValueInput` with no
`CompoundTag` in the signature.

### Reachability buckets

Each seam is classified by what the *public bevy-plugin API* offers the compat plugin:

- **(a)** — expressible today through the public plugin API.
- **(b)** — needs a public seam that is planned or enumerable (a #77 child, #438, or a phase of
  the #433 migration).
- **(c)** — structurally out of scope as currently designed; stated why.

**The headline: bucket (a) is empty.** `lodestone-server` has no `bevy_app::App` at all yet —
there is no server-side plugin registration point, so *nothing* is reachable by an external
plugin today. Every row below that has a Rust mechanism still lands in (b), because the mechanism
is crate-internal with no public seam in front of it.

### The seam table

| # | Java seam (26.2 signature) | Rust today | gap | bucket |
|---|---|---|---|---|
| 1 | Block read: `Level.getBlockState(BlockPos) → BlockState`; `BlockState` is an interned flyweight (`BlockBehaviour.BlockStateBase extends StateHolder<Block, BlockState>`, ~25 precomputed `private final` fields, `boolean[] faceSturdy` cache) compared **by identity** in vanilla code | The server's world model is canonical block-state **`String`s** in a per-column `Vec<String>` palette — `lodestone-server` does **not** depend on `lodestone-world` (dev-dep only, `crates/lodestone-server/Cargo.toml`); the client separately uses `u32` state ids via `lodestone_ecs::ChunkWorld` | Two disconnected Rust representations, neither identity-interned; a shim needs a stable palette-id ↔ interned-Java-object mapping, not a struct copy | (b) |
| 2 | Block write: `Level.setBlock(BlockPos, BlockState, int flags)` | `set_block` exists only inside `apply_block_action` (`crates/lodestone-server/src/server.rs`), inline in the connection task, veto-free | No public write API server-side (client-side counterpart is [#129](https://github.com/matteopolak/lodestone/issues/129)); the #433 migration's scheduled apply is the planned seam | (b) |
| 3 | Chunk access: `ServerChunkCache.getChunk(x, z, status, create)` — synchronous load-or-generate | `lodestone-worldgen` generates columns; no on-demand public chunk API, no load/unload lifecycle a plugin can observe | Worldgen is a bit-exact oracle library ([#132](https://github.com/matteopolak/lodestone/issues/132) flags whether a plugin seam is even compatible with that guarantee) | (b) |
| 4 | Entity manipulation: `Entity` (560 members), `ServerLevel.addFreshEntity(Entity) → boolean`, `Entity.hurtServer`, teleport, remove | `MobSim` (`crates/lodestone-server/src/mobs/mod.rs`); the **only** dynamic-dispatch extension point in the crate is `SimMob::add_goal(priority, Box<dyn Goal>)` (`mobs/mod.rs`) | No public spawn/remove/mutate seam; server-side counterpart of [#138](https://github.com/matteopolak/lodestone/issues/138) | (b) |
| 5 | Player object model: `ServerPlayer` (175 members, `connection`/`containerMenu` fields), `PlayerList` broadcast, lookup by name | **No player entities, no player registry, no broadcast.** Every send is against one `&mut Connection<T>` owned by one connection task; `IntegratedServer::bind` retains no handle to any connection. Filed as [#438](https://github.com/matteopolak/lodestone/issues/438); `docs/server-ecs.md`'s vitals row records that a player is not yet server-`World` state | The whole Bukkit `Player`/`Bukkit.broadcastMessage`/`getOnlinePlayers` surface sits behind #438 | (b) |
| 6 | Inventory/menus: `AbstractContainerMenu.clicked(int, int, ContainerInput, Player)`, `broadcastChanges`, `containerListeners` | Per-connection container sync (`CONTAINER_SYNC_INTERVAL`, `server.rs`); block entities server-side are four Rust structs — `Furnace`, `Hopper`, `Composter`, `BrewingStand` — no chests | No synthetic-menu path (client counterpart [#145](https://github.com/matteopolak/lodestone/issues/145)); container state is replication-classified per-connection today, needs the simulation/replication reclassification of #433 | (b) |
| 7 | Items: `ItemStack` (120 members) over `PatchedDataComponentMap` — an **identity-keyed** fastutil `Reference2ObjectMap<DataComponentType<?>, Optional<?>>` patch, copy-on-write over a `prototype` | `lodestone_model::ItemStack` is a **closed struct of known fields** (`crates/lodestone-model/src/item.rs`, `ItemComponents` in the same file); components a build does not model are **dropped** | An `ItemMeta`/custom-NBT plugin round-trips components we discard — silently lossy, which violates the loud-failure rule by construction | **(c)** until `ItemComponents` carries unknown components opaquely; then (b) |
| 8 | Scheduling: `MinecraftServer extends ReentrantBlockableEventLoop<TickTask>`; `execute`/`submit` gated by `BlockableEventLoop.isSameThread()` — the main-thread contract Bukkit's scheduler is built on | `run_tick_loop` (`crates/lodestone-server/src/tick.rs`) is `pub(crate)`, 8 fixed concrete params, hardcoded straight-line body — no way to register per-tick work | Bukkit `runTaskLater`/`runTaskTimer` ([#113](https://github.com/matteopolak/lodestone/issues/113) client-side) have nowhere to attach; the #433 schedule **is** the planned registration mechanism | (b) |
| 9 | Packet send: `ServerGamePacketListenerImpl.send(Packet<?>)` → `Connection.send(Packet<?>, ChannelFutureListener, boolean flush)` — netty (`ChannelFutureListener`, `ByteBuf` in `Packet.codec`) is in the signature | `ServerDirective` (`crates/lodestone-server/src/protocol.rs`) carries only `Send`/`SetState`/`SetCompression`/`None`; `ServerProtocol` (`protocol.rs`) is a closed, hand-enumerated encoder list, one method per packet, defaulted no-op bodies, ~24 implemented (`crates/protocol/v770/src/server_protocol.rs`'s `impl ServerProtocol for V770ServerProtocol`) | *Typed* sends can shim onto encoders — (b), behind #438 for addressing a player. *Arbitrary* `Packet` objects and pipeline injection: see row 11 | (b) |
| 10 | NBT/serialization: `CompoundTag`, codec-based `ValueOutput`/`ValueInput` save layer | NBT lives in `lodestone-core` (`NbtTag` `:100`, `Nbt` `:173`, `Compound(Vec<(String, Nbt)>)`) and **`lodestone-server` never touches it** — block entities are plain Rust structs; NBT block entities exist client-side only | A `CompoundTag` shim can wrap `lodestone_core::Nbt` cheaply, but the server has no NBT-shaped state for it to address | (b) |
| 11 | Raw packet interception (ProtocolLib-class): netty pipeline injection via reflection into `Connection.channel` | Inbound `ServerBound` (`protocol.rs`) is a closed 21-variant enum ending in `Ignored` with **no raw-packet passthrough**; no netty, no channel, no pipeline exists to inject into | Inherits [#156](https://github.com/matteopolak/lodestone/issues/156)/[#157](https://github.com/matteopolak/lodestone/issues/157)'s unresolved design — the one place `docs/roadmap/plugin-framework.md`'s audit says "not currently known to be buildable" | **(c)** |
| 12 | Events: **there is no event bus anywhere in `net/minecraft`** — the only listener-shaped hooks in vanilla are `AbstractContainerMenu.containerListeners` and `SyncedDataHolder`. The entire Bukkit event surface is Paper's own patches (see the cut line below) | **No event bus, no cancellation, no hook registration of any kind server-side.** `dispatch_play_packet` (`server.rs`) matches `ServerBound` and calls `apply_*` helpers inline with no interception point; `apply_block_action` (`server.rs`) breaks unconditionally — `set_block(AIR)` → `reg.remove` → `encode_block_update`; `apply_attack` (`server.rs`) calls `sim.attack` directly | Every Bukkit event needs the `Adjudicate` window, which exists only as design in `docs/server-ecs.md` | (b) — **the load-bearing row** |
| 13 | Commands: Brigadier dispatcher, per-node permission predicate | `lodestone-command` (1,388 lines) is a self-declared island with zero consumers; its `Node.permission: Option<NodeId>` field is read by nothing | Server-side dispatch is [#48](https://github.com/matteopolak/lodestone/issues/48); plugin registration [#118](https://github.com/matteopolak/lodestone/issues/118) | (b) |
| 14 | Permissions/ops: vanilla `Permissions.COMMANDS_GAMEMASTER` checks; Bukkit permission nodes on top | **No permission model or op system exists at all** — `dispatch_play_packet`'s `ChangeGameMode` arm says so explicitly; the gamemaster check is deliberately skipped and every connection is treated as the singleplayer owner (`apply_difficulty_change`'s own doc comment) | Entirely behind [#125](https://github.com/matteopolak/lodestone/issues/125)/[#127](https://github.com/matteopolak/lodestone/issues/127) | (b) |

**Census size: 14 seam categories. Bucket (a): 0. Bucket (b): 12. Bucket (c): 2** (raw-packet
interception; lossless item components — the second is fixable by opening `ItemComponents`, the
first inherits #156's genuinely-open design question). A Rust *mechanism* exists behind roughly
five of the fourteen (block write, mob sim, container sync, typed packet send, the tick loop),
but in every case it is crate-internal, veto-free, and reachable by nothing outside
`lodestone-server`. **No JNI exists anywhere in the workspace** — no `jni`/`jni-sys` crate, no
`libjvm` link; the JVM tier starts from zero.

## The cut line: where Paper's bytecode stops and ours starts

**This is the centrepiece finding, and it kills the issue's cleanest framing.** #341's design
premise is that "Paper's own bytecode does all the Bukkit translation for us — including the
event bus, listener priorities, and cancellation semantics." That is true of the *wrapper* layer
(`CraftPlayer` → `ServerPlayer`, `CraftWorld` → `ServerLevel`). It is **not** true of the event
bus, because Bukkit events do not fire from a layer above NMS — they fire from **inside Paper's
patched NMS method bodies**. Verified against Paper's own patch file
([`paper-server/patches/sources/net/minecraft/server/level/ServerPlayerGameMode.java.patch`](https://github.com/PaperMC/Paper/blob/main/paper-server/patches/sources/net/minecraft/server/level/ServerPlayerGameMode.java.patch)):
Paper inserts `CraftEventFactory.callPlayerInteractEvent(...)` and
`new org.bukkit.event.block.BlockBreakEvent(...)` directly into
`handleBlockBreakAction`/`destroyBlock`/`useItemOn`, checks `event.isCancelled()` inline, and
sends the corrective `ClientboundBlockUpdatePacket` from inside the same body. Corroborated from
the vanilla side: the 26.2 decompile has **no event bus anywhere in `net/minecraft`** — so there
is no vanilla seam that firing events "falls out of."

Consequence: backing NMS *leaves* (block storage, entity fields, sends) with Rust does not buy
the event bus, and the compat plugin must choose one of two cuts:

1. **Drive Paper's patched game-logic bodies from the adjudication window** — call
   `ServerPlayerGameMode.handleBlockBreakAction` in the JVM and let Paper's patched body fire the
   event, check cancellation, and call down into `Level.setBlock` shims. This gets
   vanilla-plus-Paper semantics verbatim, and it is a trap: the JVM then *re-executes game logic
   our Rust server also implements* — two simulations of one world, with the JVM's copy calling
   back into Rust leaves on hot paths (`getBlockState` in loops is exactly where per-call JNI
   overhead is catastrophic), and every behavioural divergence between the two is a
   consistency bug with no owner.
2. **Fire Bukkit events from our adjudication system** — the compat plugin's own adjudication
   system constructs the Bukkit event objects (playing the role Paper's `CraftEventFactory`
   plays), dispatches them through Paper's real event bus (`SimplePluginManager`/listener
   registration — Paper's bytecode, unmodified), reads the verdict, and returns allow/deny to the
   `Adjudicate` set. Paper's bytecode is used for the wrapper layer, the event bus, the plugin
   loader, and the Bukkit API classes; Paper's patched *NMS game-logic bodies are never driven*.
   The Rust server remains the only simulation.

**Cut 2 is the only viable shape.** Its cost is honest: the set of events we fire, and their
exact firing order and field semantics, becomes *our* responsibility to match Paper's — the
acceptance test the owner already specified (run the same plugin against our server and real
Paper, diff the behaviour) is precisely the instrument that measures that responsibility. Cut 2
also collapses the NMS surface we must back: not "everything Paper's server internals touch,"
but "everything Paper's *wrapper classes* touch when a plugin calls the Bukkit API" plus
"everything plugins reach via NMS directly" — which is what the bytecode census, when run,
should count (scan the CraftBukkit wrapper classes and a corpus of real plugin jars, not the
whole of `paper-server`).

**Evidence asymmetry, stated plainly:** there is **no Bukkit/CraftBukkit/Paper source anywhere
under `.cache`** — zero files matching `org.bukkit`; `.cache/mc/26.2/libraries/org` holds only
`apache`, `joml`, `jspecify`, `slf4j`. Every claim in this document about Paper's side —
the patch mechanism, `CraftEventFactory`, the wrapper structure, GPL-3.0 licensing, Paper
running Mojang-mapped at runtime — rests on Paper's public repository
([PaperMC/Paper](https://github.com/PaperMC/Paper)) and the owner's issue text, **not on a local
artifact**. The Java-side census above is verified locally; the Paper-side claims are external
and should be re-verified against a real Paper 26.2 jar as step one of any implementation.

## The JNI/FFI boundary, answered

The issue's threading analysis was written against the client's `Arc<RwLock<World>>` and named
JNI reentrancy "the easiest possible way to reintroduce" the silent-deadlock bug. The #433
decision changes the ground: the server's `World` is **tick-thread-owned with no lock at all**
(`docs/server-ecs.md` §"no lock at all"). That is *stricter* and *simpler* at once — there is no
lock to deadlock on, and also no lock to hand a foreign thread. The analysis under the new model:

- **Dispatch happens on the tick thread, inside an exclusive system.** bevy gives any ordinary
  plugin `&mut World` via an exclusive system — public API, no privilege. The compat plugin
  registers one in the `Adjudicate` window; Java event handlers run synchronously inside it,
  exactly as Bukkit handlers run synchronously on the main thread. While a handler runs, JNI
  upcalls (a shim's native methods) service NMS calls against a scoped thread-local pointer to
  the `World`, valid only for the duration of the dispatch — set on entry, cleared on exit.
  Bukkit's synchronous read-your-writes contract (`block.setType(STONE)` then
  `block.getType() == STONE`) holds *for free*, because the imperative call really did mutate the
  authoritative `World` mid-dispatch. This mirrors Java's own contract exactly:
  `BlockableEventLoop.isSameThread()` gating `execute`/`submit` is the same affinity rule with a
  queue behind it.
- **Off-thread access throws; it cannot block.** A Bukkit async task calling a world method gets
  an `IllegalStateException` naming the thread — Paper's own behaviour for async world access.
  It must *never* wait for the tick thread: the tick thread does not yield mid-tick, so a
  blocking wait is a deadlock by construction. `callSyncMethod`/scheduler hand-back maps onto a
  queue drained by the compat plugin's own scheduled system — the same shape as the client's
  `ActionQueue`.
- **Object identity and lifetime.** Plugins hold `Player`/`World`/`Block` references across
  ticks. Shim objects wrap **handles, never pointers**: `bevy_ecs::Entity` is already a
  generational index, which is precisely the fail-gracefully-when-gone shape the issue asked
  for — a stale handle throws, naming the entity. `BlockState` needs more: vanilla and plugin
  code compare states **by identity** (`==`), so the Java side must intern one shim `BlockState`
  object per state, keyed by a stable id — and our tree currently has *two* disconnected state
  models to key against (the server's canonical `String` palette and the client's `u32` ids; see
  seam 1). The interning registry forces that unification question early, which is a benefit in
  disguise.
- **Exceptions vs panics.** A Java exception thrown by a handler is caught at the dispatch
  boundary, logged, and the handler skipped — Bukkit's own contract. A Rust panic must never
  unwind across a JNI frame (undefined behaviour), so every `extern` boundary wraps in
  `catch_unwind` and rethrows as a Java `RuntimeException`; the half-mutated-`World` concern this
  raises is the same one [#168](https://github.com/matteopolak/lodestone/issues/168) already
  owns for native plugins, not a new class. Note `unsafe_code = "deny"` is workspace-wide but
  binds only crates opting into workspace lints — an external plugin crate sets its own
  (`docs/plugin-api.md` §"two Stage-1 constraints"), which is what makes a JNI crate under
  `crates/plugins/` legal at all.
- **What is structurally impossible**, not merely hard: (1) **netty pipeline injection** — the
  `Connection.send(Packet<?>, ChannelFutureListener, boolean)` signature carries netty types, and
  we have no channel, no pipeline, no `ByteBuf`; a shim can accept *typed* packets and translate
  to `ServerDirective::Send` where an encoder exists, but a ProtocolLib-class plugin reflecting
  into `Connection.channel` has nothing to find, and `ServerBound`'s closed 21-variant enum with
  no raw passthrough means inbound interception is equally closed (this is #156/#157 restated,
  not dodged). (2) **Synchronous world access from a foreign thread** — no lock exists to take;
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

The original framing treated "Bukkit is imperative, our doctrine is wishes" as the crux tension.
Under the reframe it mostly dissolves, clause by clause (`docs/server-ecs.md` §"How the intent
doctrine changes server-side" did the server-side mapping; this maps Bukkit onto it):

1. **Observation vocabulary** — a Bukkit event *is* observation vocabulary: `BlockBreakEvent`
   carries a block and a player, not a packet. The compat plugin translates NMS calls into the
   same world-fact vocabulary at the shim boundary.
2. **One system owns each machine** — holds. The compat plugin's exclusive system is serialized
   in the schedule and explicitly ordered; its imperative writes go through the same public write
   API the owning systems use. Ambiguity detection (`LogLevel::Error`, already the client's
   standard) is the gate that keeps this honest.
3. **Refusal is always observable** — the loud-failure rule is this clause applied to the compat
   layer: `UnsupportedOperationException("ServerLevel#getChunkSource not implemented by
   lodestone")` is refusal-made-observable for a calling plugin, and the corrective packet
   (`docs/server-ecs.md` clause 3) is refusal-made-observable for the remote client.
4. **Server-side, the plugin outranks the client** — the inversion `docs/server-ecs.md` records
   is *exactly* Bukkit's cancellation model: `event.setCancelled(true)` is a plugin overruling a
   remote client's proposal in the adjudication window. Bukkit's `LOWEST..MONITOR` priorities map
   onto system ordering within `Adjudicate` ([#105](https://github.com/matteopolak/lodestone/issues/105)/[#110](https://github.com/matteopolak/lodestone/issues/110)
   are the client-side designs to mirror).
5. **Lifecycle encodes verb shape** — mostly not applicable, as `docs/server-ecs.md` already
   concluded: a server plugin is authoritative, not a wisher; the adjudication window is what
   matters.

**What genuinely resists wish-shaping**, named rather than smoothed over: (i) mid-handler
imperative mutation that subsequent handlers and the eventual apply must observe in Paper's exact
order — solvable only because dispatch is synchronous on the tick thread, and the firing-order
contract becomes ours to match (the diff-against-real-Paper harness is the gate); (ii) raw packet
mutation (seam 11) — not a doctrine tension, a #156 design hole, and it stays open here exactly
as it does for native plugins; (iii) plugins that spawn threads and touch the world — refused by
throw, same as Paper.

## Relationship to the server ECS migration and the wider plugin-API epic

**#433 no longer offers options — it is decided, and #341 is strictly downstream of the
decision.** The three-way choice (server ECS / bespoke hooks / defer to #341) resolved to the
first: `docs/server-ecs.md` exists precisely so server-side plugin capability is *native first*.
That demotes #341 from "the answer to server-side plugins" (#433's option 3) to "one consumer of
the native surface" — the correct reading of the owner's rule that the compat layer is an
ordinary plugin.

**#77 is not the alternative to compare against; it is the substrate #341 runs on.** The issue's
original framing pitted NMS-backing against "reimplementing Bukkit in Rust" (epic #77, **50
sub-issues, 3 completed, 48 open** under `cluster/plugin-api`; #341 is itself a child of #77).
Under the reframe there is no either/or: every bucket-(b) row above resolves to a #77 child, a
#438-shaped registry, or a #433 migration phase, and the JVM tier consumes them all. The
load-bearing children, in dependency order for this plan specifically:

1. The #433 migration itself — server `App`, `GameTick` schedule, scheduled packet-apply, the
   `Adjudicate` set (design in `docs/server-ecs.md`; five queued phases, none implemented).
2. [#438](https://github.com/matteopolak/lodestone/issues/438) — player entities, a player
   registry, broadcast. Nearly every Bukkit API call resolves a `Player`.
3. Server-side event/cancellation counterparts of [#101](https://github.com/matteopolak/lodestone/issues/101)/[#104](https://github.com/matteopolak/lodestone/issues/104)/[#109](https://github.com/matteopolak/lodestone/issues/109),
   and priority ordering ([#105](https://github.com/matteopolak/lodestone/issues/105)/[#110](https://github.com/matteopolak/lodestone/issues/110)).
4. [#125](https://github.com/matteopolak/lodestone/issues/125)/[#127](https://github.com/matteopolak/lodestone/issues/127)
   permissions (the audit's "single largest pure gap"), [#113](https://github.com/matteopolak/lodestone/issues/113)
   scheduler, [#129](https://github.com/matteopolak/lodestone/issues/129)/[#131](https://github.com/matteopolak/lodestone/issues/131)
   block write, [#138](https://github.com/matteopolak/lodestone/issues/138) spawn/despawn,
   [#152](https://github.com/matteopolak/lodestone/issues/152) persistence,
   [#145](https://github.com/matteopolak/lodestone/issues/145) menus.
5. [#156](https://github.com/matteopolak/lodestone/issues/156)/[#157](https://github.com/matteopolak/lodestone/issues/157)
   packet interception — unresolved for native plugins too; the JVM tier inherits whatever they
   decide and adds nothing to the decision.

Scope comparison, honestly quantified: #77's native path is ~48 open issues of Rust surface.
#341 adds, *on top of that same surface*: an in-process JVM host, a shim-class generator driven
by the bytecode census (the six-class core above is ~1,400 members before Paper's wrapper needs
narrow it), an interning/handle registry, the event-construction layer replacing
`CraftEventFactory`, classloader interception, and a behaviour-diff harness against real Paper.
It is strictly additive work for one payoff native plugins cannot deliver: running *unmodified
Java jars*. Whether that payoff is worth the tier is the owner's call; this census's finding is
only that it costs nothing to *defer* — every prerequisite is work already scheduled for native
parity, so deferring #341 loses no time on its critical path.

## Verdict, and the dispatchable next step

**Verdict: viable-for-subset, strictly downstream.** Viable under cut 2 for the plugin
archetypes whose needs are bucket (b) — protection, economy, permissions, minigames, world
editing (batched). Not viable, inheriting #156's open question, for anti-cheat and
packet-injection archetypes — the same two rows `docs/roadmap/plugin-framework.md`'s
port-feasibility table already flags for native plugins. Not startable now: bucket (a) is empty
because the server has no plugin registration point at all.

**The single strongest piece of evidence:** the event-bus finding. Vanilla `net/minecraft` has
no event bus (verified locally, 4,839 files); our server has no event bus, no cancellation, and
no hook registration of any kind (verified locally — `dispatch_play_packet` applies inline,
veto-free); Bukkit's event bus lives in Paper's patched NMS bodies (verified against Paper's
public patch file). So the one thing #341 was supposed to get "for free" — event and
cancellation semantics — is the one thing that must be built natively first, as the `Adjudicate`
window, regardless of whether a JVM ever attaches.

**#341 should stay open**, re-scoped: it is the census-and-slice tracker for the JVM tier, a
child of #77, downstream of #433's migration and #438. Do not decompose it into sub-issues until
the bytecode census exists (the owner's own instruction).

**Dispatchable now (and needed regardless of #341):** the #433 migration phases and #438. Those
are the real next steps and are already queued.

**Dispatchable for #341 itself, when wanted:** the bytecode census. Obtain a real Paper jar for
the current line plus 3–5 real plugin jars (one protection, one economy, one kitchen-sink); scan
constant pools and method refs for `net.minecraft.*` members; rank by reference count; check the
result against this document's seam table. First fact to verify: that Paper has a Mojang-mapped
26.2 release at all — asserted by the issue, unverifiable from `.cache`.

**The smallest end-to-end vertical slice, when prerequisites exist:** one real, unmodified
protection-style plugin jar (compiled against the Bukkit API, *not* against our shims — evidence
must originate outside the code under test) whose `BlockBreakEvent` listener cancels breaks
inside a region. JVM in-process, Paper's plugin loader and event bus running Paper's own
bytecode, our adjudication system firing the event, one NMS seam backed
(`ServerLevel`/`Level.getBlockState` for the listener's region check). **Gate:** a break inside
the region leaves the server's world unchanged and a corrective block update reaches the wire; a
break outside applies. **Negative control:** the same run with the listener unregistered must
break the block inside the region — proving the veto path, not the apply path, is what the gate
measures. **Loudness control:** the plugin calling any unimplemented NMS member must produce
`UnsupportedOperationException` naming the member — asserted in the harness, not described.

## The closing argument: our own systems are the proof

The owner's scope point — core systems should themselves become bevy plugins where it makes
sense, physics being the worked example a headless run omits — is the strongest available test
of everything above, and it needs no JVM. The compat layer's entire premise is that the public
plugin API is sufficient to implement a server subsystem from outside. **We can prove or refute
that premise with our own code first**: if `MobSim` (or physics) can be re-registered through
the same public `App` seam an external plugin would use — same schedule labels, same world
access, no crate-private back door — the API is demonstrated sufficient by construction, and the
Java tier is just one more plugin. If it cannot, the API is not finished, and we will have found
that out on a subsystem we control, with Rust error messages, instead of three layers deep in a
JNI stack trace under someone else's plugin jar. Sequence the conversion of one internal system
into a plugin *before* the first line of JVM code; treat any privilege it turns out to need as a
defect in the surface, per the doctrine's own rule.

## Sources

External (no local Paper artifact exists to verify against — see "Evidence asymmetry"):

- [Paper's `ServerPlayerGameMode.java.patch`](https://github.com/PaperMC/Paper/blob/main/paper-server/patches/sources/net/minecraft/server/level/ServerPlayerGameMode.java.patch) —
  Bukkit event calls, cancellation checks and corrective sends inserted into NMS method bodies.
- [PaperMC/Paper](https://github.com/PaperMC/Paper) — patch-based architecture, GPL-3.0.

Local: `.cache/mc/26.2/src` (decompiled Mojang-mapped 26.2), the Lodestone tree as of
2026-08-04, `docs/server-ecs.md`, `docs/plugin-api.md`, `docs/roadmap/plugin-framework.md`,
issues #341 (body + 4 owner comments), #433, #77, #438.
