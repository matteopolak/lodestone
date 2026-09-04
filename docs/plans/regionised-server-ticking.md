# Plan: regionised server ticking

## What it is

A design document — not an implementation — for splitting the server's single-threaded world
tick into independently-ticked regions (Folia's model: groups of nearby chunks, each ticked on
its own thread, with explicit hand-off for anything crossing a boundary), so throughput can
scale past one core the way vanilla structurally cannot. It is deliberately a **later** item:
this doc grounds that timing in the tick-loop architecture and profiling instrumentation that
now actually exist, rather than in intuition. Written 2026-08-16 against a re-verified tree —
every claim below was
checked against `crates/lodestone-server/src/tick.rs`, `docs/tick-and-worldgen-profiling.md`,
and `docs/plans/server-ecs-migration.md` for this pass, not inherited from an external tracker.

**This is not an implementation plan with phases ready to dispatch**, unlike
`docs/plans/server-ecs-migration.md`. Section ["Preconditions"](#preconditions-and-why-none-are-fully-met-yet)
below is explicit that the gating sequence is tick loop exists →
MSPT/TPS accounting → single-threaded parity → server-tick benchmarks → *profile* → decide — has
reached "a tick loop and a profiler exist" and nothing past that. What follows is the design this
repo would execute *if and when* profiling justifies it, written now so the next re-read
has more than intuition to work from, and so the region model's hardest questions (data
partitioning, cross-region hand-off, lock ordering) are worked through once rather than
re-derived under time pressure when the "profile" step finally lands a number that says "yes."

## Preconditions, and why none are fully met yet

The required sequencing: **server tick loop exists → MSPT/TPS accounting → single-threaded
parity → server-tick benchmarks → profile → decide.** Re-verified here:

| precondition | status |
|---|---|
| A real server tick loop exists | **Met.** `run_tick_loop`/`run_tick_loop_with_weather` in `tick.rs` (~4,100 lines) drive mobs, block entities, redstone, TNT, minecarts, boats, the dragon fight, weather and scheduled ticks at real 20 Hz, with `TickClock`/`TickStats` MSPT/TPS accounting and overrun handling built in. |
| MSPT/TPS accounting | **Met**, as part of the row above — `TickClock` already tracks per-tick duration and reports MSPT/TPS. |
| Single-threaded parity with vanilla | **Not measured.** No doc or test asserts "server tick matches vanilla, single-threaded" as an achieved milestone; `docs/server-gameplay-gap-census.md` and friends track ongoing per-feature parity, not a single completed checkpoint. This is the real gate (see below). |
| Server-tick benchmarks | **Partially met.** `crates/lodestone-server/benches/server_tick.rs` drives the real loop through two in-memory sweep points, asserts deterministic tick and cumulative phase-sample counts plus the rolling-window cap, and records the named worst phase. Its paused clock makes the per-phase duration tie a wiring control rather than a profile, so it is not a populated-world throughput benchmark. |
| Profile before architecting | **Partially met.** `tick.rs` has three `TickPhase` buckets with rolling percentile history, an over-budget counter, and a global worst-window tracker. `TickStats` snapshots every summary for consumers such as the benchmark — see ["What the profiler already shows"](#what-the-profiler-already-shows). This is real per-phase data, but only an **idle-world floor** has been measured; no live, populated-world reading exists yet (see that section's own caveat). |

**Bottom line: the gating sequence has not reached "profile a real populated world," which is
the step that would justify committing to regionisation over a cheaper alternative.** This doc's
job is to make the *next* profiling pass and the *eventual* region design cheaper, not to skip
either.

## What the profiler already shows

Two independent instruments now exist (`docs/tick-and-worldgen-profiling.md` is the full
writeup; this section extracts what a regionisation decision needs from it):

- **Tick loop, `TickPhase`** (`crates/lodestone-server/src/tick.rs`): three phases, split at lock
  boundaries rather than at even time or complexity — see
  ["Why three phases, and why the split matters for regionisation"](#why-three-phases-and-why-the-split-matters-for-regionisation)
  below. `ScheduledAndPhysics` is the only phase that can call `world.column()` (a block tick
  crossing a chunk boundary can trigger worldgen), which makes it the phase a keep-alive-timeout-
  shaped stall — this repo's own recorded incident — would show up in.
- **Worldgen, per-stage percentiles** (`crates/lodestone-worldgen/src/profile.rs`): ten-stage
  split of `OverworldGenerator::column`, aggregated into p50/p95/p99/max and a dominant-stage
  ranking.

**The idle-world floor measured so far** (`tick::tests::phase_durations_floor_cost_on_an_idle_world_under_real_time`):
all three phases sit two to three orders of magnitude below the 10ms soft budget on an
`EmptyWorld` with no players or mobs — the cost of the loop *existing*, not of it doing real
work. **This says nothing about which phase dominates a populated world**, which needs a live
oracle (`scripts/live-oracles/{creative,survival,terrain}.sh`) with players connected and the
phase summaries/worst window read through `TickStats` — not attempted by this doc or by the
profiling pass that built the instrument. That live reading is the single most
valuable next measurement before any region design decision, because it would say whether
`ScheduledAndPhysics` — the phase that can call into worldgen, and therefore the phase most
naturally cut along region boundaries — is actually where the server spends its time, or whether
(as the worldgen side below already shows for a *different* subsystem) the dominant cost depends
on the scene.

**The worldgen side's own correction is the load-bearing caution for this whole plan: "dominant
stage" is a scene-dependent answer, not a workspace constant.** A cache-cold 3×3 patch measured
`vegetation` at ~68% of total and `ore` at ~13%; the identical methodology against a **warm
5×5-neighbourhood** scene (steady-state frontier expansion, the shape of a real player pushing
outward) measured `ore` at ~29% instead, with a different ranking entirely. Both figures are
real and correctly measured — they answer different questions (cold-region generation vs.
steady-state frontier expansion), and citing either without naming its scene is the exact
failure `vram_bytes` demonstrated for a different counter (a per-frame *drawn* quantity reported
under a *residency* label, where a pure camera rotation moved the number 26% and the conclusion
drawn from it was backwards twice). **Any future tick-phase profiling pass for this plan must
name its scene** — idle world, one player exploring frontier, N players clustered, a redstone-
heavy build — and this doc's own table above is itself only the idle-world floor, not a
representative load.

### Why three phases, and why the split matters for regionisation

`run_tick_loop`'s body is roughly 1,000 lines, and the back two-thirds — scheduled block-tick
draining, fire/redstone/fluid propagation, random ticks, falling blocks, vehicles, TNT,
minecarts, dragons — all run inside one `scheduled.with(|queues| { … })` closure that holds the
scheduled-tick queue's mutex across its **whole** extent. This repo already has a recorded
self-deadlock from a re-entrant call into that same mutex (`ScheduledTickHandle::restore` →
`with` → `Mutex::lock`, triggered by a saved chunk carrying a pending tick, hanging the tick
thread before its first chunk batch). The phase boundaries were deliberately placed as bare
`Instant::now()` calls with no lock held and nothing called back into `scheduled`, specifically
to avoid scattering timestamps through that ~1,000-line contested region.

This is directly relevant to regionisation, not incidental: **`ScheduledAndPhysics`'s single
mutex over the whole scheduled-tick queue is exactly the kind of workspace-global lock a region
split needs to partition first**, and the self-deadlock precedent is a preview of the harder
version of that hazard section 4 below describes (N region locks with an ordering requirement,
rather than one non-reentrant lock).

## Current architecture census (what regionisation would actually partition)

Re-verified against the tree for this pass — **the single-`RwLock<World>` framing is stale**,
and worth correcting explicitly since a wrong architecture claim
here would misdirect every design decision downstream of it:

- **There is no single `RwLock<World>`.** `lodestone_world::World` (`crates/lodestone-world/src/world.rs`)
  is a plain `HashMap<ChunkPos, LoadedChunk>` with no lock of its own; locking happens at the
  server layer, and it is already **split across several independent handles**, not one global
  lock:
  - `ChunkStore<S>` (`crates/lodestone-server/src/chunk_store.rs`) — the resident-column cache —
    holds its data behind one `Mutex<Cache>` (a `HashMap<(i32, i32), Entry>`), plus a separate
    `TicketStoreHandle` for the chunk ticket graph.
  - `AccessLists`, `BlockEntityRegistry`, `WorldBorder`, `GameRules`, `ScheduledTickQueues`,
    `WorldState` each have their own `.with(|state| ...)`-style handle over their own mutex
    (`access.rs`, `block_entities.rs`, `border.rs`, `game_rules.rs`, `scheduled_tick.rs`,
    `world_state.rs`).
  - So today's architecture is already **N locks, not one** — closer to the *end state*
    regionisation would require ("N locks with an ordering requirement, which is how deadlocks
    are usually born") than to a single-lock starting point. This is genuinely useful: the
    lock-splitting half of
    regionisation's cost may already be partly paid, incidentally, by unrelated subsystem work
    — but it also means **the ordering discipline is already a live hazard today**, independent
    of regionisation, and the self-deadlock incident above is
    evidence it is not yet fully disciplined.
- **The ECS substrate has landed further than the last re-verification recorded, but
  still does not drive the tick loop.** `docs/plans/server-ecs-migration.md`'s own status note
  (2026-08-15): Phase 0 landed — `crates/lodestone-server/src/ecs/{mod,plugin,schedules,gate}.rs`
  exist, `ServerCorePlugin` installs `ServerTick`/`ServerTickWitness` and opens
  `ServerBoot`/`NetIngest`/`GameTick` — but **Phase 1 (the tick loop actually running the
  `GameTick` schedule) has not landed**: the `World` `ServerApp::bootstrap()` returns is bound
  once at construction and never threaded into `run_tick_loop`. `grep -n run_schedule
  crates/lodestone-server/src/tick.rs` is empty. So there is a `bevy_ecs::World` in the process,
  but the ~4,100-line hand-rolled `run_tick_loop` is still what actually executes every tick —
  regionisation today would partition *that* loop's own locks and data structures (the census
  above), not an ECS schedule's query system, because the schedule does not run the game yet.

**What this means for the region model below**: a region split's natural partition axis is
**`ChunkStore`'s resident-column set**, since that is where "which chunks does a region own" is
already represented as data. The subsystem-specific handles (border, game rules, scheduled
ticks, etc.) split less naturally — a world border and game rules are genuinely global concepts
in vanilla, and only the scheduled-tick queue and block-entity registry are naturally
per-chunk/per-region. Any future region design should expect **a mixed model**: some state
partitioned by region (scheduled ticks, block entities, chunk residency), some kept global with
its own cross-region access story (world border, game rules, difficulty) — not a single clean
"everything splits by region" architecture. Folia's own design has this same shape (global data,
regionised data, and an explicit third category for cross-region operations); it is not a defect
particular to this codebase.

## The region model: design questions, worked through

This section names the real semantic changes and works through *how* each would land, not just
that it would cost something.

### Partitioning: one `bevy_ecs::World` per region, or one partitioned `World`

Re-examined against the current state above: since **the ECS `World` does not drive gameplay yet**
(Phase 1 of the ECS migration is
still open), this question is not currently answerable against real code — it is a question
about a substrate that has not yet been asked to hold game state at all. The honest sequencing
is: land ECS Phase 1 (single `World`, single-threaded, driving the real tick) first, and let
*that* migration's own experience with query/resource partitioning inform whether N worlds or
one partitioned world is cheaper — building the region-partitioning answer against a `World`
that is not yet load-bearing risks answering a question the eventual real migration invalidates.

### Cross-region hand-off: what actually crosses a boundary

The concrete list, derived from what `ScheduledAndPhysics` already does in one un-partitioned
pass today: fire/redstone/fluid propagation (redstone dust and fluid flow are the most likely to
touch a boundary — a redstone line or a lava flow does not respect a region edge), falling
blocks and entities crossing into a neighbouring region's chunks, minecart/boat physics carrying
an entity across, TNT/explosion blast radii spanning a boundary, and the dragon fight (a single
global entity that cannot be regionised at all without a special case). **Any one of these
implies an explicit hand-off queue between regions**, analogous to `ScheduledTickQueues` today
but per-region-pair rather than global — this is real new complexity, not a detail to defer.

### Lock ordering: the `hold_read`/`hold_write` tripwire needs to become a real ordering assertion

The design already names this. Concretely, given the census above already has six-plus
independent locks (`ChunkStore`'s cache, ticket store, access lists, block entities, world
border, game rules, scheduled ticks, world state) **before** regionisation adds N more
per-region locks, the ordering discipline needed is not hypothetical future work — a real
ordering assertion (a debug-only "this thread already holds lock B, acquiring lock A after B is
allowed/forbidden" check, keyed by a fixed global order) would catch violations **today**, and
building it before regionisation adds more locks to order is strictly easier than building it
after. Recommend this as an independent, immediately-actionable piece of work regardless of
whether regionisation itself proceeds — it de-risks the eventual region work and pays for itself
against the self-deadlock class this repo has already hit once.

### Global vs. regionised state, concretely

| state | today's handle | region model |
|---|---|---|
| Resident chunk columns | `ChunkStore`'s `Mutex<Cache>` | Partitions naturally by `ChunkPos` region membership |
| Scheduled block ticks | `ScheduledTickQueues` | Partitions naturally; cross-region ticks (a fluid/fire spread reaching a neighbour) need the hand-off queue above |
| Block entities | `BlockEntityRegistry` | Partitions naturally by owning chunk |
| World border | `WorldBorder` | Stays global — vanilla's border is world-wide, not per-region |
| Game rules | `GameRules` | Stays global |
| World state (time, weather, difficulty) | `WorldStateHandle` | Stays global in vanilla terms, but Folia itself regionises *some* of this (per-region weather in Folia's own model) — worth a deliberate decision, not a default, if this plan is ever executed |
| Mobs/entities crossing regions | none today (single loop) | Needs the hand-off queue; a mob pathing across a boundary is the routine case, not an edge case |

## Sequencing, restated against today's state

The required sequencing holds; this is the same list annotated with what is actually done:

1. ~~Server tick loop exists~~ — **done**.
2. ~~MSPT/TPS accounting~~ — **done**.
3. **Single-threaded parity with vanilla — not measured; the real remaining gate.** No formal
   "matches vanilla, single-threaded" checkpoint exists. This should be a named, dated milestone
   (a doc or a test asserting it, the way this doc's own preconditions table wants to check
   against something concrete) before any region work starts, because once ticking is concurrent
   a behavioural divergence and a race look identical in a bug report.
4. **Server-tick benchmark applied to the tick loop — partially built.**
   `crates/lodestone-server/benches/server_tick.rs` drives `run_tick_loop` through equal-area
   empty and populated fixture worlds. It waits for the asynchronous world install, seeds exact
   zero and nonzero rosters through the live server API, verifies tick and phase-recorder counts,
   and rejects a population sweep that does not move chunk-source work. Its paused clock makes the
   phase timing a wiring control rather than a populated-world profile, so the repeatable
   instrument exists but the decision-quality scene in step 5 still does not.
5. **Profile a populated world, naming the scene — not done.** The idle-world floor above is not
   this step; it is the floor the real measurement subtracts from. Do this against at least two
   scenes (a single player exploring frontier, and a redstone/entity-dense build) given the
   worldgen side's own demonstrated scene-dependence, and read the phase summaries/worst window
   from the live clock's `TickStats` via the existing oracles.
6. **Only then decide.** If `ScheduledAndPhysics` dominates and the dominant cost within it is
   genuinely parallelisable across chunk regions (not, say, a single global bottleneck like the
   dragon fight or a redstone contraption at spawn that every player's region would contend for
   anyway), regionisation is the right lever. If it is not, close this plan with the measurement
   that says so: a performance change (or non-change) with no before/after number is not a
   result.

## Risks and gotchas

- **The self-deadlock precedent is not hypothetical for this plan — it is the shape of thing
  regionisation adds more of.** A lock held across a call into a subsystem that can call back
  into you (`scheduled.with` → `world.column` → `ScheduledTickHandle::restore` → `with` again)
  already happened once with one lock. N region locks with a hand-off queue between them is a
  strictly larger surface for the same class of bug, and the ordering-assertion recommendation
  above is the mitigation, not a nice-to-have.
- **The dragon fight, and any other genuinely-global singleton, cannot be naively regionised.**
  Folia's own model has an explicit answer for this (the entity's region "owns" it and other
  regions defer), but it is a real special case, not something the partition table above can wave
  past.
- **A redstone-dense build at spawn is the adversarial case for "regions parallelise the
  bottleneck."** If many players cluster in one region (spawn, a shared base), that region's
  single thread is still bound exactly as today's one thread is — regionisation helps *spread-out*
  load, not concentrated load, and the profiling in step 5 above should include a clustered scene
  specifically to check this before committing.
- **Do not let this plan's existence read as a decision already made.** Every section above is
  scaffolding for a *future* profiling-driven decision, and the honest current answer to "should
  we regionise" is still "not yet measured" — with the reasoning spelled out once so it does not
  need re-deriving.

## Configuration

None — this is a design document with no shipped code. Any future implementation would need its
own region-size constant (Folia's own default is a configurable radius around a "chunk owner"),
which is deliberately not guessed at here since it should come from the profiling in step 5, not
from an assumption.

## Dependencies

- `docs/plans/server-ecs-migration.md` — the ECS substrate this plan's partitioning question
  (one `World` per region vs. one partitioned `World`) is downstream of; Phase 1 there is a
  precondition for answering it against real code.
- `docs/tick-and-worldgen-profiling.md` — the two profiling instruments this plan's "profile
  first" step would extend to a populated-world reading.
- `docs/server-gameplay-gap-census.md` and friends — where single-threaded parity work is
  currently tracked (not as a single milestone yet; see "Sequencing" step 3).
- The plugin-compatibility scope remains distinct from this throughput-focused design; both
  constraints can hold at once.
