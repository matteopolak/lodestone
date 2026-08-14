# Plan: migrating `lodestone-server` onto its own `bevy_ecs::World` (issue #433)

## What it is

The phased migration plan that turns [`docs/server-ecs.md`](../server-ecs.md)'s decision record into
dispatchable work: how today's `Arc<Mutex<_>>`-and-`tokio::spawn` server becomes a tick-thread-owned,
unlocked `bevy_ecs::World` whose core subsystems are themselves bevy plugins. Written 2026-08-04
against a re-verified tree; the state census in "The census" below is the core deliverable, and
every phase names its files, its choke-point patch, its gate, its negative control, its performance
gate, and the downstream epic it unblocks.

This plan does **not** redesign the architecture. Two `World`s, no lock on the server's, Fabric's
client/server entrypoint split, and clause 4's inversion are settled by `docs/server-ecs.md` and
issue [#433](https://github.com/matteopolak/lodestone/issues/433). Read that document first.

## Verified current state (read this before trusting any phase below)

Everything here was re-checked against the tree for this pass, per CLAUDE.md rule 2. Line numbers
drift under concurrent editing — treat them as "was here at 2026-08-04", and re-grep the symbol.

**Confirmed as the decision record describes:**

- `crates/lodestone-server/Cargo.toml` has **no** `bevy_app`, `bevy_ecs`, or `lodestone-ecs`
  dependency. Nothing has landed. `lodestone-world` appears only in `Cargo.toml`, under
  `[dev-dependencies]` — so the migration adds it as a *real* dependency, which is a genuine new
  graph edge, not a promotion of an existing one.
- `ChunkSource::set_block` takes **`&self`** (`crates/lodestone-server/src/chunk.rs`), with
  interior mutability via `OverworldChunkSource.edits: Mutex<HashMap<(i32, i32), ChunkColumn>>`
  (`chunk.rs`). That `&self` is the mechanism that makes the shipped straddle possible: any
  connection task can mutate shared terrain with no scheduling boundary.
- Both inline straddles are exactly where the decision record says. `apply_block_action` calls
  `source.set_block(pos.x, pos.y, pos.z, AIR)` on a confirmed `StopDestroy` and then immediately
  encodes the corrective `block_update`; `apply_attack` reaches
  `mobs.with(|sim| sim.attack(...))`, dispatched from
  `dispatch_play_packet` (all in `server.rs`).
- `bevy_app`/`bevy_ecs` are pinned `version = "0.19", default-features = false, features = ["std"]`
  at root `Cargo.toml`, with no `multi_threaded` on any target. So
  `World::run_schedule(...)` is a plain synchronous call on the calling thread and there is no
  second executor for tokio to reconcile with.

**Staleness corrections — claims that were true when written and are not now:**

- **`docs/server-tick-loop.md` needs no correction note; it already has one.** The dispatch brief for
  this plan asked me to flag that document as still carrying the superseded "do not link an ECS into
  the server" recommendation. It does not: §45 is a full, dated reversal
  (`## Recommendation reversed: `lodestone-ecs` is now linked into the server`) that walks all three
  original blocking legs and records which are void and which are preserved. **No edit is needed, and
  a future agent should not add a second correction note on top of the existing one.** Recorded here
  because a brief that says "flag the stale doc" is itself the stale claim — the exact shape rule 2
  is about.
- **`docs/server-ecs.md`'s `CorePlugin` citation in `plugin.rs` had a stale line range.** `CorePlugin`
  inserts `FrameClock` via `init_resource`, and the `Update`
  `FrameSet::{Input, Interpolate, Camera, Terrain}` chain follows it. The *claim* is correct
  and load-bearing; only the line range rotted. More importantly, `CorePlugin` also inserts
  **`WorldTime`** and **`LockHolds`**, which the decision record's gotcha does not mention — see
  "The server's core plugin" below, because that changes what a server core plugin may reuse.
- **`crates/lodestone-ecs/src/{events.rs,plugin.rs,sets.rs}` are uncommitted in-flight work by
  another agent right now** (`events.rs` untracked; the other two modified). `EventPriority` and the
  `GameEventBus` are part of that landing. Phases below depend on their *shape*, deliberately not on
  their line numbers, and Phase 0 must re-read all three immediately before writing.
- **`docs/plans/world-state.md` already exists and already found two of the straddles below**
  (per-connection `WorldAdminState`, and wall-clock-since-join game time). This plan does not restate
  its 26.2 constant research and must not contradict its placement decisions; it owns the *migration
  mechanics*, that plan owns the *world-state features* built on top. Where the two touch, this plan
  defers.

**Three defects the census surfaced, now filed — phases below reference them:**

- **Players are never spawned as
  entities.** There is no player registry, no player-entity concept, and no broadcast path, so two
  LAN players cannot see each other. This is the single most consequential census finding, because it
  means Phase 4 must **create** the player-entity concept rather than migrate an existing one. Every
  "simulation"-classified per-connection scalar in the census below has nowhere to go until it exists.
- **LAN hosting spawns no tick loop.** `IntegratedServer::bind` never calls `run_tick_loop`; its only
  caller is `open_in_memory_with_mobs`. So over LAN, mobs do not tick, block entities do not tick,
  and random ticks do not run. Anything the migration "moves into `GameTick`" is moved into a
  schedule that, on the LAN path, **nothing runs** — the island risk named in "Islands" below.
- **`ChunkSource` has two dangerous trait defaults.** `set_block`'s default body is
  `let _ = (x, y, z, name);` (`ChunkSource::set_block`, `chunk.rs`) — a **silent no-op**, so an implementor that forgets to
  override it discards every edit with no error, which is an island factory in the purest sense.
  `block_state`'s default (`ChunkSource::block_state`, `chunk.rs`) **regenerates the entire owning column and clones a
  `String`** per single-block query. The migration multiplies that call count (adjudication reads
  terrain before applying), so it is a named subject of the perf gates below, not a footnote.

## The census

This is the deliverable a phase list is worthless without. Three axes per row:

- **Target shape** — ECS `Resource`, component on an entity, or stays plain Rust.
- **Class** — `simulation` (two connections must agree, or it must advance with none attached) or
  `replication` (per-connection cursor, reconstructible from authoritative state × cursor), per
  `docs/server-ecs.md`'s never-straddle invariant.
- **Plugin candidacy** — the owner's widened scope: core systems should *become* plugins where that
  makes sense. **(a)** becomes a plugin and is **omittable** (the deployment still works without it);
  **(b)** becomes a plugin but is **load-bearing** (omitting it breaks the server); **(c)** stays
  internal, with a stated reason.

### A. Shared mutable state, reachable from connection tasks

Eleven pieces. These are the ones the unlocked-`World` design has to *remove* a lock from.

| # | state | site | wrapper | mutated by | target shape | class | plugin |
|---|---|---|---|---|---|---|---|
| 1 | `MobSim<'static>` via `MobHandle` | `mobs/mod.rs` | `Arc<Mutex<_>>` | tick task (`MobSim::tick`) **and** connection task inline (`apply_attack`, `server.rs`) | components on mob entities + `Resource` for spawn RNG | simulation | **(b)** `MobAiPlugin` |
| 2 | `Vec<EntitySnapshot>` via `LiveMobSource` | `mobs/mod.rs` | `Arc<Mutex<_>>` | tick publishes, connections read (`EntitySource::snapshots`) | stays plain — becomes the publish side of the snapshot channel | replication | **(c)** it *is* the replication seam |
| 3 | `BlockEntityRegistry` via `BlockEntityHandle` | `block_entities.rs` | `Arc<Mutex<_>>` | tick (`tick_all`) **and** connection inline (insert on place, remove on break, read for `container_state`) | components on `BlockPos`-keyed entities | simulation | **(b)** `BlockEntityPlugin` + four sub-plugins |
| 4 | `OverworldChunkSource.edits` | `chunk.rs` | `Mutex<HashMap<…>>` behind `Arc<S>` | connection inline (`set_block` ×3) and tick (random ticks) | **stays plain `Arc<dyn ChunkSource>`** | simulation | **(c)** read-mostly service both sides need synchronously — see "The tokio seam" |
| 5 | `TickClock.{tick_count,last_mspt_micros,overrun_count}` | `tick.rs` | `Arc<AtomicU64>`×3 | tick writes, `tick_stats()` reads | `Resource` in the `World`, `Arc<TickClock>` retained as the published read side | replication (instrumentation) | **(c)** |
| 6 | `TickClock.history` | `tick.rs` | `Mutex<VecDeque<u64>>` | as above | as above | replication | **(c)** |
| 7 | `BlockTickFeed` | `run_tick_loop` local, `tick.rs` | `Arc<Mutex<Vec<…>>>` | tick publishes; **exactly one** connection drains (`drain_all`) | **rebuilt** as per-connection broadcast egress | replication | **(c)**, but must be replaced — see below |
| 8 | `ExplosionFeed` | `run_tick_loop` local, `tick.rs` | `Arc<Mutex<Vec<Detonation>>>` | as above | as above | replication | **(c)**, same |
| 9 | `shutdown` | `ShutdownSignal`, `integrated.rs` | `Arc<Notify>` | shell triggers, tasks `select!` | stays plain | replication | **(c)** lifecycle, not state |
| 10 | worldgen `SETTINGS` | `worldgen_data.rs` | `OnceLock<Value>` | init-once | stays plain | neither | **(c)** immutable cache |
| 11 | mob `INDEX` | `mobs/` | `OnceLock<HashMap<String,u32>>` | init-once | stays plain | neither | **(c)** immutable cache |

**Rows 7 and 8 are a shipped LAN bug, not merely a shape to migrate.** `drain_all` is
`std::mem::take` — single-consumer by construction, and both types' own doc comments admit it is
"correct today" only because `open_in_memory_with_mobs` spawns exactly one connection task per feed.
Over LAN with two players, the first to drain consumes the other's block updates and explosions. The
migration must not port this shape forward; Phase 3 replaces it.

### B. Tick-task-local state (`run_tick_loop` function locals)

Six pieces, currently invisible to every connection because they are stack locals with no lock at
all. These are the **cleanest** conversions in the whole migration and therefore the right content
for the earliest phases.

| # | state | wrapper | target shape | class | plugin |
|---|---|---|---|---|---|
| 12 | `block_ticks: ScheduledTickQueue<String>` | plain local | `Resource` | simulation | **(b)** `ScheduledTickPlugin` |
| 13 | `fluid_ticks: ScheduledTickQueue<String>` | plain local | `Resource` | simulation | **(b)** same plugin |
| 14 | `random_ticks: RandomTickScheduler` | plain local | `Resource` | simulation | **(a) omittable** — `random_tick_speed 0` is a legitimate config, so a server must work with the plugin absent |
| 15 | `game_tick: u64` | plain local | `WorldTime` `Resource` (reuse `lodestone_ecs::WorldTime`) | simulation | **(c)** |
| 16 | `next_tick_at`, `last_overload_warning_at` | plain locals | **stay in the driver loop, never in the `World`** | neither | **(c)** — clock policy, per `docs/server-ecs.md` reason (a) |
| 17 | `tick_area: (RangeInclusive<i32>, RangeInclusive<i32>)` | argument | `Resource` | simulation | **(c)** |

Row 16 is the one to get right: the accumulator and the overload-forgiveness branch **must not**
become `World` state. `docs/server-ecs.md`'s whole reason (a) for two `World`s is that the server's
catch-up policy differs from the client's, and putting the accumulator in the `World` invites a
future agent to unify them.

### C. Per-connection state (`serve_play` locals, `server.rs`)

Twenty-four pieces, all threaded as `&mut` into `dispatch_play_packet`. This is where the
never-straddle invariant does real work, and it produces a much sharper result than the decision
record's four-timer table: **twelve of the twenty-four are simulation state living in a connection
task.**

| state | site | class | target |
|---|---|---|---|
| `state: State` | arg | replication | stays — protocol state machine |
| `pending_keep_alive` | `serve_play` local | replication | stays |
| `next_keep_alive_id` | `serve_play` local | replication | stays |
| `keep_alive_tick` | `serve_play` local | replication | stays |
| `time_sync_tick` | `serve_play` local | replication | stays |
| `container_sync_tick` | `serve_play` local | replication | stays |
| `container_sync: ContainerSync` | `serve_play` local | replication | stays |
| `streamer: EntityStreamer` | arg | replication | stays |
| `view: ViewTracker` | arg | replication | stays |
| `awaiting_chunk_batch_ack` | `serve_play` local | replication | stays |
| `pending_chunk_batches` | `serve_play` local | replication | stays |
| `chunks_sent` | arg | replication | stays |
| `next_window_id` | `serve_play` local | replication | stays — vanilla's counter is per-`ServerPlayer` |
| **`play_start`** | `serve_play` local | **simulation** | `WorldTime` `Resource` |
| **`vitals: PlayerVitals`** | `serve_play` local | **simulation** | component on the player entity |
| **`vitals_tick`** | `serve_play` local | **simulation** | folds into `GameTick` |
| **`fall: FallTracker`** | `serve_play` local | **simulation** | component |
| **`inventory: PlayerInventory`** | `serve_play` local | **simulation** | component |
| **`player_pos`** | `serve_play` local | **simulation** | component |
| **`sprinting`** | `serve_play` local | **simulation** | component |
| **`pending_break`** | `serve_play` local | **simulation** | component |
| **`admin: WorldAdminState`** | `serve_play` local | **simulation** | `Resource` |
| **`username`** | arg | **simulation** (identity) | component |
| `open_container` | `serve_play` local | **split** | window id → stays; "this block is open, and by how many viewers" → component |

**Every row above exists twice.** `serve_play` is forked on `wasm32` (native and a
wasm counterpart), and the fork re-declares the whole local set — `pending_keep_alive`,
`pending_break`, `sprinting`, `player_pos`, `vitals`,
`fall`, and so on. The two bodies share `dispatch_play_packet` but not their state.
So **every §C migration is two edits, not one**, and a phase that migrates only the native fork leaves
browser singleplayer on the old shape with a green `cargo check` — `cargo check --workspace` does not
cross-compile, so nothing in `just health` would say so. `scripts/wasm-check.sh` is the only command
that would, and it is not in `just health`. Phase 4 must run it explicitly.

Two of these are shipped bugs, both already recorded in `docs/plans/world-state.md` and cited here
because they are what the migration exists to fix:

- **`play_start` makes world time per-connection.** `ticks_since(play_start)` (`ticks_since`, `server.rs`)
  derives the broadcast game time from *this connection's* join instant. Two LAN
  players see two different times of day, and neither matches `run_tick_loop`'s own `game_tick`
  counter, which reaches no wire at all — rule 1's island in its purest form.
- **`WorldAdminState` makes difficulty and game rules per-connection.** Constructed fresh per serve,
  so one LAN player's `random_tick_speed` change is invisible to another.

`open_container` being genuinely split is worth its own note, because it is the row most likely to be
migrated wrongly. The window id is a per-connection protocol handle and must stay; the *viewer count*
that drives a chest's lid animation is simulation state two connections must agree on. Migrating the
whole row either way produces a defect.

### Census summary

**Twenty-two pieces of shared or straddling state**: 11 lock-wrapped shared (§A), 6 tick-local (§B),
and 12 simulation-classified per-connection scalars (§C) that need a player entity to live on, of
which 5 (`play_start`, `admin`, and the `open_container` split) are world-scoped rather than
player-scoped. Nine `Arc<Mutex<_>>`/`Arc<Atomic*>` wrappers disappear; two (`ChunkSource`, the
shutdown `Notify`) stay by design.

## The tokio seam — the hard part

**Do not cite the client as precedent for this.** Checked directly: the client's async→`World` seam is
`SharedState::apply` (`crates/lodestone-client/src/state.rs`), and it takes
`lodestone_ecs::hold_write` — the `EcsHandle` write lock — pushes one event into `IngestQueue`, and
then **runs the entire `NetIngest` schedule inside that same critical section**. Its own doc comment
says so: "`apply` runs *inline in the driver task*, before `events.send(event).await`, so blocking
here stops the socket being read." There is an `IngestQueue`, so it *looks* like queue-then-drain, but
the drain is synchronous in the producer under a lock. That is the documented wart. The server must
not reproduce it, and a phase that says "same as the client" has reproduced it.

### The shape

One `mpsc` channel per direction, with the boundary at `serve_connection_inner`'s signature.

**Inbound (connection → tick thread): `tokio::sync::mpsc::UnboundedSender<Proposal>`.**

```
Proposal { connection: ConnectionId, body: ProposalBody }
```

`ProposalBody` is a new enum in a new `crates/lodestone-server/src/ecs/proposal.rs`, one variant per
simulation-affecting packet — `BreakBlock`, `PlaceBlock`, `Attack`, `MoveTo`, `ContainerClick`,
`SetGameRule`, `SetDifficulty`, `ClientCommand`. Unbounded deliberately: a bounded channel makes the
connection task `.await` on the tick thread, which reintroduces cross-thread stall potential — the
precise hazard class `docs/server-ecs.md` says the server design "gets to not pay." Backpressure, if
it is ever needed, belongs as a per-connection proposal budget enforced *in* the drain system, not as
channel capacity.

**Outbound (tick thread → connection): `tokio::sync::broadcast::Sender<Egress>` plus a per-connection
`Receiver`.** This is what replaces rows 7 and 8. `broadcast` rather than a per-connection `mpsc`
because the tick thread must not know how many connections exist, and because `broadcast::Receiver`
gives each connection its own independent cursor — which is exactly the "reconstructible from
(authoritative state × that connection's cursor)" property the replication class is defined by. A
lagging receiver's `RecvError::Lagged` is a real signal (this connection missed updates and needs a
resync), not an error to swallow.

**Where the boundary lives.** `serve_connection_inner` loses `block_entities: &BlockEntityHandle`,
`mobs: &MobHandle`, `block_ticks: &BlockTickFeed`, and `explosions: &ExplosionFeed` — four arguments
— and gains two: `proposals: &ProposalSender` and `egress: &mut EgressReceiver`. The `&S: ChunkSource`
argument **stays**, for the reason below.

### What happens to a connection task that needs a synchronous answer

Three cases, and they have different answers. This is the part a phase brief must state explicitly,
because getting it wrong is how the migration deadlocks or stalls.

1. **Terrain reads — answered synchronously, off the `World`.** Chunk streaming (`ViewTracker`
   recenter, `set_view_radius`) and the vitals submersion test call `source.column(...)` /
   `source.block_state(...)` directly, and they must keep doing so. Worldgen is deterministic and
   pure per column (`chunk.rs`'s `generate_columns_parallel` is safe for exactly that reason), so
   `Arc<dyn ChunkSource>` stays a shared read-mostly service that both the tick thread and every
   connection task read concurrently. **This is why row 4 stays plain, and it is the single most
   important shape decision in the seam:** pulling terrain into the `World` would force chunk
   streaming through the queue and make every connection's chunk send wait on a tick boundary.
   The cost is that `set_block` remains reachable from a connection task at the *type* level — so the
   gate that forbids it is a source scan, not the type system (see Phase 2).
2. **Confirmations and corrections — answered asynchronously, and that is correct.** The
   `block_update` that `apply_block_action` sends today, the `set_health` after drowning damage, the
   `EXPLODE` packet: none of these are synchronous answers, they are corrective echoes against a
   client that already predicted the outcome. Vanilla sends them tick-scheduled too. Moving them
   behind the queue adds at most one tick (50 ms) of latency to a *correction*, which is the
   adjudication window's whole purpose.
3. **The one genuinely synchronous case: opening a container.** `container_state(block_entities, pos)`
   must produce the chest's contents *now*, because the `OPEN_SCREEN` and the initial
   `CONTAINER_SET_CONTENT` go out together. Answer: a **oneshot reply channel carried in the
   proposal** — `ProposalBody::OpenContainer { pos, reply: oneshot::Sender<ContainerSnapshot> }`. The
   connection task `.await`s that oneshot, which is bounded by one tick period, and the tick thread
   never blocks. This is the only place in the design where a connection task waits on the tick
   thread, and a phase adding a second one needs to justify it. Do **not** solve this with a shared
   read lock on the `World`; that is the client's wart, and it reintroduces the entire
   lock-discipline hazard class the design exists to avoid.

## Islands the migration could create

CLAUDE.md rule 1: built, tested, reaching nothing. Nine-plus confirmed instances. The migration's
specific exposures:

**1. The `GameTick` schedule that nothing runs (highest risk).** Because `IntegratedServer::bind`
spawns no tick loop, every system a phase registers is dead on the LAN path. A hermetic test that
builds an `App` and calls `run_schedule(GameTick)` passes regardless. **Every phase's gate must
assert against a driver that production actually spawns, not against a hand-built `App`** — and
Phase 3 exists to make LAN spawn one.

**2. The server's own terminal-no-op arm.** `dispatch_play_packet`'s match on `ServerBound` is
exhaustive today — no bare `_ =>` — but it ends in a *grouped* no-op arm,
`ServerBound::Handshake { .. } | … | ServerBound::Ignored => {}` (`dispatch_play_packet`, `server.rs`), with a
second in `serve_connection_inner` (`server.rs`). Adding a variant fails to compile, which is good; adding your variant *to that
group* compiles and silently discards, which is the island. The deeper factory is one layer down:
`crates/protocol/v770/src/server_protocol.rs`'s `State::Play if packet_id == …` arms fall through to
`ServerBound::Ignored`, so a packet that decodes to `Ignored` is dropped with no trace. That is a
**two-file join** — the packet id in the adapter, the arm in `server.rs` — exactly like the
clientbound serverbound-decode axis `cargo xtask connectedness` measures.

**3. `ChunkSource::set_block`'s silent-no-op default** (filed above). A migration that introduces a
new `ChunkSource` implementor — a test double, an ECS-backed source — gets edit-discarding for free
with no compile error.

### Making mis-routing a compile error

Mirror `lodestone-model`'s `route()` table exactly, because it already solved this problem for
`ClientEvent`. Its mechanism, verified: a `pub fn route(event: &ClientEvent) -> Route` whose match is
**exhaustive with no catch-all**, returning a `Route` struct of `bool` flags per consumer; `Route::NOWHERE`
is a legal answer that must be typed on purpose; `Route::is_island()` reports the rule-1 shape; and a
test `route_has_no_catch_all_arm` (`crates/lodestone-model/src/event.rs`) reads the file's own
source with `include_str!` and fails if a wildcard reappears — **with three controls**, two proving the
detector sees `_ => Route::NOWHERE` and `_ => todo!()`, and one proving it does not misread an
ordinary `{ .. }` struct pattern.

The server's equivalent, owned by Phase 2:

```rust
// crates/lodestone-server/src/ecs/route.rs
pub struct ServerRoute { pub proposal: bool, pub replication: bool, pub adjudicated: bool }
pub fn route(packet: &ServerBound) -> ServerRoute  // exhaustive, no catch-all
```

Plus the same source-scanning test and the same three controls. The property this buys: a new
`ServerBound` variant cannot reach `main` without someone deciding, in one line and on purpose,
whether it is a proposal, pure replication, or adjudicated — and `is_island()` names the ones that are
decoded and consumed by nothing.

## Sequencing: does subsystem-to-plugin conversion happen during or after the state migration?

**Strictly after, per subsystem — but interleaved across subsystems.** Subsystem X's plugin
conversion may not share a phase with X's state migration; it may run concurrently with subsystem Y's
state migration. This is the most consequential call in this plan, so here is the reasoning rather
than the verdict alone.

Three arguments, the third decisive:

1. **A plugin boundary drawn around a mutex encodes the mutex.** Convert `MobSim` into `MobAiPlugin`
   while it is still `Arc<Mutex<MobSim>>` and the plugin's public surface is "a system that takes
   `Res<MobHandle>` and locks it." The state migration then changes that surface to
   `Query<&mut Mob>`. The plugin ABI churns twice, and the second churn breaks any plugin written
   against the first — for a plugin API whose entire premise is stability, that is the worst
   available ordering.
2. **`SimMob::add_goal(priority, Box<dyn Goal>)` (`mobs/mod.rs`) is the only `dyn` extension surface
   in the crate.** It is a *pre-ECS* plugin seam, and it works. So mob AI already has a usable
   extension point, which removes the urgency argument for converting it early and leaves the
   ordering free to be chosen on other grounds.
3. **The perf gate cannot attribute a regression when two variables move at once.** The owner's
   constraint is "do not sacrifice performance", measured. Moving `MobSim` out from behind a mutex
   changes its cost (one fewer lock per tick, worse cache locality across components); wrapping it in
   a plugin changes its cost (schedule dispatch, system-param resolution). Do both in one phase and a
   +15% MSPT reading is unattributable, so the only available response is to revert the whole phase.
   One variable per phase is what makes a perf gate actionable rather than decorative.

The exception, stated so nobody has to guess: **Phase 0's `ServerCorePlugin` is a plugin from the
start**, because there is no pre-plugin version of "install the schedules" to migrate from.

## Performance gates

Every conversion phase carries one. The rules, because the existing bench corpus has a trap in it:

- **`crates/lodestone-entity/benches/mob_tick.rs` structurally cannot see this migration.** It
  benches `NavigatingMob::tick` — pure `lodestone-entity`, *below* the ECS seam. Scheduling overhead
  is invisible to it, so a green `mob_tick` after Phase 7 proves nothing about Phase 7. This is
  CLAUDE.md's **world species** of vacuous test: the source is exemplary and the flaw is that it is
  pointed at a scene that cannot exercise the change. The same applies to
  `lodestone-physics/benches/movement_integration.rs` for the physics-plugin work.
- **`lodestone-server` has no bench target and neither does `lodestone-ecs`.** So Phase 1 must add
  `crates/lodestone-server/benches/world_tick.rs` (criterion 0.8, `harness = false`, matching
  `docs/roadmap/benchmarks.md`), measuring **one full `run_tick_loop` iteration** — the scheduled
  path, end to end. That is the only instrument that can see a scheduling regression. It is a
  deliverable of Phase 1, not a follow-up.
- **Predict, do not merely assert a direction.** Per CLAUDE.md's magnitude species: state the expected
  MSPT before running, from constants outside the code under test, and require the measurement to land
  on it. For Phase 7 the prediction is bounded: bevy without `multi_threaded` dispatches systems as
  direct calls, so per-tick overhead should be *below* the one `Mutex` lock/unlock pair it replaces —
  predict **≤ ±5% MSPT at 100 mobs**, and treat a >5% regression as a phase blocker, not a note.
- **`block_state`'s regenerate-and-clone default is a named subject.** Adjudication reads terrain
  before applying, so call counts rise. Any phase that adds a terrain read inside `GameTick` reports
  the delta in `block_state` calls per tick alongside its MSPT figure.
- **`scripts/wasm-size.sh` is the binary-size gate**, run at Phase 0 and again at the end.

### Binary size, measured

`docs/server-ecs.md` raises `bevy_app`/`bevy_ecs` wasm growth as a non-blocking cost. Measured rather
than repeated, in two throwaway `wasm32-unknown-unknown` crates outside the repo, both with `web/`'s
exact release profile (`opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`,
`strip = true`), the bevy one running a real `App` with two components, two systems, a custom
`ScheduleLabel` and a resource so dead-code elimination cannot remove it:

| | raw | gzip | brotli-11 |
|---|---|---|---|
| baseline (no bevy) | 152 B | 166 B | 128 B |
| + `bevy_app` + `bevy_ecs` 0.19, `default-features = false, features = ["std"]` | 360,581 B | 133,016 B | 102,441 B |
| **marginal cost** | **+352 KiB** | **+130 KiB** | **+100 KiB** |

`web/` **does** link `lodestone-server` (`web/Cargo.toml`) and does **not** link `lodestone-ecs` or
bevy today, so this cost is real and lands in the shipping browser bundle. Against
`scripts/wasm-size.sh`'s enforced ceiling of 1,600,000 B gzip and its recorded baseline of 1.21–1.24
MiB gzip, adding ~130 KiB leaves roughly **10–12% headroom, down from ~20–25%**. It fits, and it eats
about half the remaining margin.

Two honesty caveats on that number, in both directions: it is an **over**-estimate because a real
build shares allocator and panic machinery with the rest of the bundle, and an **under**-estimate
because `lodestone-server` will monomorphise far more `Query`/`SystemParam` types than two systems do.
Treat it as the right order of magnitude and **re-run `scripts/wasm-size.sh` at Phase 0**, which is
the actual gate; do not treat the table as the prediction.

## Phases

Each is independently landable and leaves `main` green. "Choke point" means a patch to a file brokered
through the orchestrator — for `crates/lodestone-server/src/lib.rs`, always the exact `mod` line.

### Phase 0 — dependency and `ServerCorePlugin`, wired to nothing

**Owns:** `crates/lodestone-server/Cargo.toml`, new `crates/lodestone-server/src/ecs/mod.rs`,
`src/ecs/plugin.rs`, `src/ecs/schedules.rs`.
**Choke point:** `lib.rs` — add `mod ecs;` after `mod chunk;`. One line.
**Content:** add `bevy_app`/`bevy_ecs` (`workspace = true`) and promote `lodestone-world` +
`lodestone-game` out of `[dev-dependencies]`. Define `ServerCorePlugin`: `init_resource::<WorldTime>`,
`init_schedule(NetIngest)`, `init_schedule(GameTick)`, and the server's own set chains. **Do not
install `CorePlugin`** — and note the decision record's gotcha is *incomplete*: `CorePlugin` inserts
`WorldTime`, `FrameClock` **and `LockHolds`**. `WorldTime` is reusable; `FrameClock` is a lie
server-side; `LockHolds` is worse than a lie, because it is the meter for a lock the server does not
have, and a `LockHolds` reading of zero on the server would look like a measurement.
**Gate:** a test that builds the server `App` and asserts `World::contains_resource::<FrameClock>()`
is `false` and `contains_resource::<WorldTime>()` is `true`; plus `schedule.initialize()` succeeds
under `ambiguity_detection: LogLevel::Error`.
**Negative control:** the same test with `CorePlugin` installed instead must **fail** the `FrameClock`
assertion. Run it and watch it fail.
**Perf gate:** `scripts/wasm-size.sh` — record the number, compare to the table above.
**This phase is deliberately an island for exactly one phase.** Say so in the commit message.
**Unblocks:** everything.
**Re-read before writing:** `crates/lodestone-ecs/src/{plugin.rs,sets.rs,events.rs}` — all three are
another agent's uncommitted work.

### Phase 1 — the tick loop drives the `World`; tick-locals become `Resource`s

**Owns:** `src/tick.rs`, `src/ecs/plugin.rs`, new `crates/lodestone-server/benches/world_tick.rs`.
**Choke point:** `Cargo.toml` `[[bench]] name = "world_tick"`, `harness = false`.
**Content:** `run_tick_loop` gains `app: &mut App` and calls
`app.world_mut().run_schedule(GameTick)` once per iteration, at the position `mobs.with(MobSim::tick)`
occupies today. Census rows 12–15 and 17 move into `Resource`s. **Row 16 stays a driver local** —
`next_tick_at` and the overload branch do not enter the `World`. `lodestone_ecs::runner.rs`'s
`Runner::Headless` is the reference for the accumulator shape, not a dependency to add.
**Gate:** an integrated test driving the real `open_in_memory_with_mobs` that asserts `tick_count`
advances **and** that a `Resource`-backed counter incremented by a `GameTick` system advances in
lockstep with it.
**Negative control:** delete the `run_schedule` call; the resource counter must freeze while
`tick_count` keeps advancing. That divergence is the island detector.
**Perf gate:** establish the `world_tick` baseline. First measurement, so no regression threshold yet
— but publish the number in the commit message so Phase 7 has an anchor.
**Unblocks:** scheduled-tick producers; chunk lifecycle gains a place
to hang a per-tick chunk system.

### Phase 2 — the proposal queue and the `Adjudicate` set

**Owns:** new `src/ecs/proposal.rs`, `src/ecs/route.rs`, `src/ecs/adjudicate.rs`; edits to
`src/server.rs` (`serve_connection_inner`, `serve_play`, `dispatch_play_packet`).
**Choke point:** `lib.rs` — `pub use ecs::{Proposal, ProposalBody, ServerRoute, route};`.
**Content:** the inbound `mpsc` and the oneshot reply for `OpenContainer`, per "The tokio seam". The
`route()` table with its no-catch-all test and three controls. Sets chained
`Drain → Adjudicate → Apply` inside `GameTick`. Move **`apply_attack`** across first — it is the
smallest straddle, mutates one subsystem, and needs no reply channel.
**Gate:** `exactly_one_system_writes_block_edits`, mirroring
`crates/lodestone-controller/src/ecs.rs`'s `exactly_one_system_writes_movement_intent` exactly:
build the real plugin set, promote `ambiguity_detection` to `LogLevel::Error`, and call
`schedule.initialize(world)`. **Copy its recorded gotcha verbatim** — do not run the app first, because
an already-built schedule is not rebuilt and `initialize` returns `Ok` without consulting the new
settings, which is precisely how the assertion goes vacuous. Plus a source scan asserting
`server.rs` contains no `source.set_block(` and no `mobs.with(` call (the type system cannot express
this, since `ChunkSource::set_block` takes `&self` by design).
**Negative control:** two. A rogue second `MovementIntent`-style writer of block edits, added
unordered in the same set, must make `initialize` return `Err`. And the source scan must report a hit
when handed a one-line fixture containing `source.set_block(`.
**Perf gate:** `world_tick` within ±5% of Phase 1's baseline, plus the per-tick `block_state` call
count.
**Unblocks:** the adjudication window, hence server plugins at all; the player-entity broadcast path.

### Phase 3 — LAN gets a tick loop; the feeds become broadcast egress

**Owns:** `src/integrated.rs`, `src/tick.rs`, new `src/ecs/egress.rs`.
**Choke point:** none beyond `lib.rs` re-exports.
**Content:** `IntegratedServer::bind` spawns `run_tick_loop`. `BlockTickFeed`/`ExplosionFeed` (rows 7,
8) are replaced by `broadcast::Sender<Egress>` with a per-connection `Receiver`; `RecvError::Lagged` is
handled as a resync signal, not swallowed.
**Gate:** two LAN connections; a block change produced by the tick loop must reach **both**. The
existing single-consumer shape fails this by construction, which makes it its own negative control —
and that is the strongest gate in the plan, because it is a test that could not have passed before.
**Negative control:** additionally, with the tick loop not spawned, the same test must report zero
block updates on both connections.
**Perf gate:** `world_tick` within ±5%; plus fan-out cost at 1, 2 and 8 receivers, since broadcast
clones per receiver.
**Unblocks:** players seeing each other (needs a working broadcast path); all of
`docs/plans/world-state.md`, which currently has nowhere to tick.

### Phase 4 — player entities

**Owns:** new `src/ecs/player.rs`; edits to `src/server.rs`, `src/vitals.rs`, `src/fall.rs`,
`src/inventory.rs`.
**Choke point:** `lib.rs` — `pub use ecs::player::{PlayerId, PlayerEntity};`.
**Content:** **creates** the player-entity concept. On reaching `State::Play`, a proposal spawns a
server entity; census §C's twelve simulation rows become its components; `VITALS_TICK_INTERVAL`'s
per-connection timer is deleted in favour of a `GameTick` system. This is the phase
`docs/server-ecs.md`'s four-timer table was pointing at when it called `VITALS_TICK_INTERVAL` the
exception.
**Gate:** two connections; player A's vitals must be readable from the server `World` while player B
is connected, and A attacking B must reduce **the server's** copy of B's health.
**Negative control:** with the spawn proposal removed, the same query must find zero player entities
and the attack must be a no-op.
**Perf gate:** `world_tick` at 0, 1 and 8 player entities.
**Unblocks:** player entities directly; the mob AI roster, because a mob needs a player entity to target
through a `Query` rather than a per-connection position scalar.

### Phase 5 — world-scoped state: clock and admin

**Owns:** `src/ecs/world_state.rs`; edits to `src/server.rs`.
**Content:** `play_start`/`ticks_since` deleted; the broadcast game time comes from the
`WorldTime` `Resource` Phase 1 created. `WorldAdminState` becomes a `Resource`.
**Coordination:** `docs/plans/world-state.md` owns the 26.2 semantics (registry-driven `WorldClock`,
`advance_time` gating, the 59-rule table). **This phase moves the storage; that plan lands the
behaviour.** Dispatch them to the same agent or strictly in this order.
**Gate:** two LAN connections receive the **same** game time, and a `random_tick_speed` change from
one is visible to the other.
**Negative control:** with the `Resource` reverted to a per-connection local, the same test must show
divergent times.
**Perf gate:** none — moves scalars, adds no per-tick work. Say so rather than running a vacuous one.
**Unblocks:** the rest of the world state epic — see the downstream-epics table above.

### Phase 6 — block entities become components

**Owns:** `src/block_entities.rs`, `src/furnace.rs`, `src/hopper.rs`, `src/composter.rs`,
`src/brewing.rs`, new `src/ecs/block_entity.rs`.
**Content:** row 3 loses its mutex; `BlockPos`-keyed entities carry `Furnace`/`Hopper`/`Composter`/
`BrewingStand` components. The `OpenContainer` oneshot from Phase 2 becomes a real `Query`.
**Gate:** a furnace placed over LAN by one player, observed cooking by another.
**Negative control:** with the tick system unregistered, the furnace must not advance.
**Perf gate:** `world_tick` at 0, 16 and 256 block entities, within ±5% of Phase 1 per entity.
**Unblocks:** container UI work; `docs/block-entities.md`'s remaining gap.

### Phase 7 — mob sim becomes components, then `MobAiPlugin`

**Owns:** `src/mobs.rs`, `src/mob_spawn.rs`, new `src/ecs/mob.rs`.
**Content:** **two commits, and the split is the point** (see "Sequencing"). 7a moves `MobSim`'s
population into components and deletes the mutex, keeping the call sites plain functions. 7b wraps the
systems in `MobAiPlugin`. `SimMob::add_goal(priority, Box<dyn Goal>)` (`mobs/mod.rs`) stays as the
`dyn` goal seam.
**Gate:** mobs tick over LAN and are visible to two connections; `MobAiPlugin` omitted → zero mob
movement, with everything else still serving.
**Negative control:** for 7b, a rogue second writer of mob position must fail the ambiguity check.
**Perf gate:** the load-bearing one. `world_tick` at 10 and 100 mobs, **separately after 7a and after
7b**, predicted ≤ ±5% each. Do **not** rely on `lodestone-entity`'s `mob_tick` bench — it is below the
seam and cannot see either commit.
**Unblocks:** the mob AI roster.

### Phase 8 — the plugin surface: cancellation and priority

**Owns:** `src/ecs/adjudicate.rs`, new `docs/server-plugin-api.md`.
**Content:** `Cancelled` on a proposal; `EventPriority`'s `Lowest..Monitor` chain configured into the
server's schedules (reusing `lodestone-ecs`'s `EventPriority` once that in-flight work lands);
clause 3's refusal-as-corrective-packet path made explicit.
**Gate:** a test plugin ordered before `Adjudicate`'s consumer vetoes a break, and the client receives
a corrective `block_update` restoring the original block.
**Negative control:** the same plugin ordered *after* the consumer must fail to prevent the break —
which proves the ordering is what does the work, not the veto call.
**Perf gate:** `world_tick` with 0 and 8 no-op observer plugins installed.
**Unblocks:** the plugin framework epic.

### Parallel track P — physics as an omittable client-side plugin

Fully concurrent with Phases 0–8: it touches **no** `lodestone-server` file.

**Owns:** `crates/lodestone-ecs/src/player.rs`, `crates/lodestone-controller/src/ecs.rs`.
**The blocker nobody has named: physics is not separable today.** `LocalPlayerPlugin`
(`crates/lodestone-ecs/src/player.rs`) bundles the `TickSet::Physics` chain
(`apply_creative_flight_input`, `player_physics`, `cancel_flight_on_landing`,
`pin_passenger_to_vehicle`) together with `apply_look_intent`, `tick_attack_strength`,
`clear_debug_lines`, and seven `init_resource` calls including `ActionQueue` and `Egress`. Omitting
physics today means also losing look intent, the attack-strength cooldown, and the action queue. So
this track's real content is **splitting `LocalPlayerPlugin`** into `PlayerStatePlugin` (resources +
`TickSet::Intent` + `TickSet::Animate`) and `PlayerPhysicsPlugin` (the physics chain).

**And the split is not a clean cut.** `pin_passenger_to_vehicle` sits *in* the physics chain but is
not physics — it is `Entity.positionRider`, and its own comment records that it must run last because
it writes the transmitted `on_ground` for a passenger. A bot with physics omitted still needs pinning
to a vehicle whose position arrives over the network. So `pin_passenger_to_vehicle` belongs in
`PlayerStatePlugin`, ordered after `TickSet::Physics`, which is *empty* when the physics plugin is
absent. Anyone doing this work should expect that ordering edge to be the fiddly part.

**Gate:** with `PlayerPhysicsPlugin` **omitted**, a headless bot that writes `PhysicsState` itself
still emits `ClientAction::Move` carrying the position it wrote — the plugin-free bot works.
**Negative control:** with `PlayerPhysicsPlugin` **installed** and the bot writing nothing, `y` must
decrease under gravity across ten ticks. That control is what proves the first assertion is about
omission rather than about a bot that was never simulated anyway.

**What a headless physics gate can and cannot prove.** It can prove plugin omission and installation
change what reaches `ClientAction::Move`, because that path is pure ECS with no GPU. It **cannot**
prove anything about rendering, and this is a live trap here rather than a hypothetical: `--headless`
renders through `mesh_simple`, whose `ao` is corner-occlusion only, while live terrain uses
`mesh_models`' per-face `face_shade` constants — a colour fix was once verified against `--headless`,
measured byte-identical, and declared inert against **the one scene in the tree that structurally
cannot exercise it** (CLAUDE.md's *world* species). So: build this gate as a **plain `App` +
`Runner::Headless` test with no `GpuContext` at all**, not as a `Mode::Headless` shell run. If it needs
a `--headless` render, it is measuring the wrong thing.
**Perf gate:** `movement_integration` is below the seam and cannot see the split; measure instead that
`GameTick` with `PlayerPhysicsPlugin` omitted is *cheaper* than with it installed — a prediction whose
sign is known, which is the cheapest honest gate available here.
**Unblocks:** headless-bot work; `crates/plugins/lodestone-autopilot`.

## Concurrency graph

```
Phase 0 ──┬─> Phase 1 ──┬─> Phase 2 ──┬─> Phase 3 ──┬─> Phase 4 ──> Phase 8
          │             │             │             │
          │             └─> Phase 5   ├─> Phase 6   └─> (Phase 4 also gates #225 via 7)
          │                (needs 1)  │
          │                           └─> Phase 7a ──> Phase 7b
          │
Track P ──┘  (independent of 0; listed here only because it shares reviewers)
```

**Strictly serial:** 0 → 1 → 2. Phase 2 needs Phase 1's schedule to hang `Adjudicate` in, and Phase 1
needs Phase 0's `App`. 7a → 7b is serial *by design*, per "Sequencing".

**Concurrent after Phase 2:** Phases 3, 5, 6 and 7a touch disjoint files (`integrated.rs` /
`world_state.rs` / `block_entities.rs` + the four block-entity files / `mobs.rs`) and can run in
parallel with four agents. Phase 4 needs Phase 3's broadcast path.

**Fully concurrent throughout:** Track P.

**Choke-point contention.** `crates/lodestone-server/src/lib.rs` is patched by Phases 0, 2 and 4 —
one `mod` line and two `pub use` lines, brokered. `src/server.rs` (110 KB) is touched by Phases 2, 4
and 5: **serialise those three or assign them to one agent**, because it is the file most likely to be
mid-keystroke. `src/tick.rs` is touched by Phases 1 and 3.

## Downstream epics, per phase

| phase | unblocks |
|---|---|
| 0 | everything (no epic directly) |
| 1 | #307/#308 producers; chunk lifecycle #289/#292/#293/#297 |
| 2 | #77 plugin framework (the adjudication window); #438's path |
| 3 | #438; all of world state #340 (nothing ticks on LAN today) |
| 4 | **#438**; mob AI roster #225 (mobs need a player entity to target) |
| 5 | #323, #327, #328 (children of #340) |
| 6 | container UI; `docs/block-entities.md`'s third gap |
| 7 | **#225** |
| 8 | **#77** |
| P | headless bots; `lodestone-autopilot` |

## How to change it, and the gotchas

- **Re-verify before routing around anything above.** This plan's own §"Verified current state" found
  the dispatch brief's `server-tick-loop.md` claim already fixed and a decision-record line citation
  rotted. Line numbers here will drift; symbols will not.
- **Never build a control out of a shell pipeline**, and never let `rtk` near one. Count with a
  program that reads the file. A control that prints nothing is a failure to run, not a pass.
- **A phase gate must run against a driver production spawns.** A hand-built `App` in a test cannot
  distinguish "system registered and running" from "system registered and never run", which is
  precisely the migration's dominant island risk while `bind` spawns no tick loop.
- **The `ambiguity_detection` gate goes vacuous if you run the app first.** Copy
  `lodestone-controller`'s comment along with its code.
- **Do not put the tick accumulator in the `World`.** Census row 16. It is the mechanism of
  `docs/server-ecs.md`'s reason (a) for two `World`s.
- **Do not add a second place where a connection task waits on the tick thread.** One oneshot, for
  container open, justified in "The tokio seam". A shared read lock on the server `World` is the
  client's documented wart and reintroduces the whole lock-discipline hazard class.
- **`--headless` is not a general-purpose verification environment.** It renders through
  `mesh_simple`, not `mesh_models`, and has already produced one vacuous gate that way.

## Configuration

None. No feature flag gates this migration: `bevy_app`/`bevy_ecs` become unconditional dependencies of
`lodestone-server` on every target, matching `lodestone-ecs`. A `server-plugins` feature was
considered and rejected — a feature-gated `World` would mean two server architectures to test, and
CLAUDE.md's own record of `live-inventory` sitting broken behind a non-default feature for a session
is the argument against.

## Dependencies

- `bevy_app`, `bevy_ecs` — root `Cargo.toml`, `0.19`, `default-features = false`,
  `features = ["std"]`, never `multi_threaded`. Measured wasm cost above.
- `lodestone-world`, `lodestone-game` — promoted from `[dev-dependencies]` (root `Cargo.toml`) to real
  dependencies. Both version-free; `cargo xtask check-isolation` is the standing enforcement.
- `tokio::sync::{mpsc, broadcast, oneshot}` — all in the wasm-safe `sync` feature the crate's
  `wasm32` target block already selects, so the seam adds no new wasm surface.
- **In-flight, not yet committed:** `crates/lodestone-ecs/src/{events.rs,plugin.rs,sets.rs}`.
  `EventPriority` (Phase 8) and `GameEventBus` come from there. Re-read before writing.

## See also

- [`docs/server-ecs.md`](../server-ecs.md) — the decision record this plan implements. Read first.
- [`docs/server-tick-loop.md`](../server-tick-loop.md) — the loop Phase 1 threads a schedule through.
  Its §45 reversal is current; do not add a second correction note.
- [`docs/plans/world-state.md`](./world-state.md) — the sibling plan. Owns the world-state features
  Phase 5 provides storage for; owns the 26.2 constants this plan does not restate.
- [`docs/plugin-api.md`](../plugin-api.md) — the five clauses, and clause 4's server-side inversion.
- [`docs/world-unification.md`](../world-unification.md) — the client's one-`World` migration, and the
  lock-discipline machinery the server deliberately does not build.
- This migration's own tracking issue unblocks the no-player-entities defect, the plugin framework
  epic, the world state epic, and the mob AI roster epic — see the downstream-epics table above for
  the issue numbers.
