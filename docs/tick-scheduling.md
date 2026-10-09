# Tick scheduling: random ticks, scheduled ticks, block entities, and profiling

## What it is

The foundation for per-block-tick features (crop growth, gravity blocks, fluid flow, fire, redstone): a reference-shaped random-tick scheduler, a scheduled-tick queue for "run again in N ticks", and a neighbour-update propagator with the original ordering and cascade shape. It also covers how block-entity ticking is bounded to resident chunks and the instruments that measure where tick and worldgen time goes.

## How it works

### Random ticks

Each tick draws a fixed number of random positions per eligible 16-row section, matching the original selection and its level-local position LCG (distinct from the generator used for a block's own random behaviour). Eligibility (any randomly-ticking block in the section) is a maintained running count updated as blocks change, not a scan of all 4,096 cells per section per tick; scanning was the overwhelming majority of tick-thread time and starved chunk delivery during join. Handlers (grass spread and death, crops, saplings, leaf decay) are transcribed from each block's real predicate: an "is the block above bare air" proxy for grass survival was a shipped bug, since decorative cover like short grass is not air but has no collision, so decorated grass died on its first random tick.

### Scheduled ticks

Two queues, block before fluid, drain in that order every world tick. Every due entry runs in `(trigger tick, priority, insertion order)` with the whole due set collected before any callback, so a tick scheduled during the batch cannot run in it. A second schedule for a pending position/kind pair is a silent no-op.

- Live queues use `ScheduledTickKind` variants. The `TICK_*` family names remain canonical only at the persistence and legacy-fixture boundary, where the typed parser translates them; producers schedule the typed key.
- Both are partitioned by chunk column. `ChunkScheduledTickQueue` routes each tick to its owning column while the outer owner assigns one shared insertion sequence and merges only each local queue's due head, so a fluid or block reaction scheduling over a chunk border gains or loses no priority from map traversal or the target key. The tick task still executes that merged sequence serially; local storage is an ownership boundary, not permission to run columns concurrently.
- Block callbacks receive `ScheduledTickQueueAccess`, not a column's queue: it routes by position, preserves the world sequence and limits `take_matching` to that position's owner (so a reversing piston can remove its own uncommitted finish tick without cancelling a neighbour's). The `IntegratedServer` tick loop consumes it; controls cover positive and negative chunk coordinates, equal-time ordering and the piston cancellation.
- Generated columns enter the fluid queue through an exposure-aware admission pass on first becoming entity-ticking (worldgen writes a snapshot, so no placement or neighbour hooks fire). It mirrors the spread decision and schedules only liquid cells that can write into an adjacent destination; it preclassifies the palette by canonical state id, rejects uniform no-fluid sections before expansion, expands uniform liquid sections through the integer visitor, and gates immediate neighbours before the shape and slope walk, so cost is bounded by stored section indices plus the exposed boundary. The tick-area loop remembers admitted columns so a moving player does not requeue an ocean. Fluid environment follows the dimension (Nether lava has its faster drop-off and delay; Overworld and End use regular rules with each source's build-height extent).
- The live drain is resident-only. Before a callback enters a world-coordinate probe, the loop admits the column footprint from retained snapshots; a cold or busy footprint puts the due record back at its original trigger tick. Sources without a nonblocking edit ledger write only after a resident snapshot is confirmed. Mob, block-entity, lightning and falling-block handoffs use the same retry boundary, so streaming can delay visible work but never lose it or start generation on the clock thread.

### Neighbour updates

Fixed visitation order (west, east, down, up, north, south) and a depth-first cascade: a neighbour whose change triggers further notifications resolves that whole sub-cascade before the next sibling, capped by a maximum chain length. Gravity blocks and the redstone family inherit this ordering. A notification landing outside the currently ticked chunk footprint is skipped for now, a known accepted limitation shared by every consumer.

### A self-deadlock the queue's lock made possible

The tick loop holds the scheduled-tick queues behind one lock across a span that reads and mutates the world, and on a persistent world reading can load a chunk from disk, restoring that chunk's saved pending ticks into the same queue. Restoring used to take the identical non-reentrant lock again, parking the tick thread permanently: deterministic, silent, reached when a tick first touched any on-disk column with a pending tick, leaving the client loading forever. Fresh and in-memory worlds never exercised it. The fix stages restored ticks behind a second lock, merged into the real queues from inside the original held region, with a fixed order (live queues before staging, never the reverse).

`lodestone_server::lock_order` checks the broader callback-held-handle order in debug and test builds (scheduled queue before the staged queue, block-entity registry and mob simulation) as a thread-local diagnostic with no release cost. Place any new callback-held handle in that order before acquiring it from scheduled work.

### Block-entity ticking

Block entities (hoppers foremost, the only kind that probes world state each tick) were ticked from one ever-growing registry scanned unconditionally. The suspected "slower farther from spawn" was false (distance is flat). The real mechanism was a capacity threshold: once the registry's distinct block-entity chunks exceed the chunk cache size, a cyclic scan through a bounded cache misses every entry between revisits, jumping the miss rate from near zero to near total and costing hundreds of regenerations per tick. The fix ties each block entity's tick to its chunk being resident right now (a plain non-generating lookup that must not extend residency), skipping it otherwise. The registry has no eviction of its own: simulated state must resume as if it never left when the chunk returns.

`BlockEntityRegistry::tick_plan` snapshots the registry into one owner per chunk in `(chunk x, chunk z, local y, local z, local x)` order. Resident non-hopper entities leave the registry lock as immutable per-owner inputs; native worlds dispatch 128 or more entries across at most four bounded lanes, while browsers and smaller workloads use the same ordered serial interface. The central commit validates every owner slot, restores snapshot order, then replaces state and emits `BlockEntityTickEffect` messages for the world writer, so a fast worker cannot publish a furnace transition ahead of an earlier owner and no worker holds the registry mutex.

Hoppers stay serial: their vertical neighbours are mutable registry entries, and treating the three-entry operation as local would hide a cross-owner protocol in a shared lock. A future lateral container relation needs an explicit hand-off.

### Profiling

Two per-phase/per-stage instruments capture the tail (worst window, not mean), after a real timeout was misdiagnosed from a mean that hid the one slow window. The tick loop has a few coarse phases at boundaries chosen to avoid a checkpoint inside the region holding the scheduled-tick lock; one phase covers that whole region (the resident-gated cross-column work most likely to reveal a stall). Worldgen is profiled per stage (shape, carving, ores, vegetation, ...) as percentiles over a batch, bypassing the generator's caches so every column pays full cost. Both are validated with a control that must read exactly zero (an idle world under a paused deterministic clock).

Which worldgen stage dominates depends on the condition: a whole cold region is dominated by decoration; one more column at the edge of explored area (what walking produces) is dominated by ore placement, since decoration's cost mostly depends on neighbour context a steady-state column already paid for. Judge optimizations for ordinary play against the steady-state condition.

### Native executor isolation

Native integrated singleplayer runs `run_primary_tick_loop_with_weather` on the shell's two-worker network runtime, so a long synchronous tick can occupy one worker while the other services the connection and client driver. Terrain generation uses a separate bounded dispatcher; this does not shorten a tick phase. Before a primary connection publishes its first position, the tick-area fallback is empty (generating a cold origin square would race the first streamed column). Browsers keep the tick future on the event loop.

## How to change it

- New randomly-ticking block: extend the per-block dispatch and the section-eligibility classification together, or it never ticks or is drawn for but unhandled.
- Scheduled-tick producer: call the scheduling primitive wherever a block decides "run again in N ticks"; it is value-type agnostic.
- Tick ownership: keep the outer queue's one global insertion sequence and due-head merge; never drain columns in coordinate order or count per column (both reorder equal-time updates across a border). Keep reactions behind `ScheduledTickQueueAccess`.
- Neighbour-update producer: call the propagator once per mutated position with a callback that mutates and returns further single-target notifications; never call it recursively from the callback (the explicit stack handles cascades and nesting double-counts the chain limit).
- Widening the ticked area needs a multi-column terrain cache first; it deliberately reuses the mob simulation's fixed radius rather than a second "loaded chunks" concept.
- Never hold a queue lock across a suspension point or a second acquisition of itself; stage and merge.
- No eviction path for the block-entity registry tied to chunk-cache eviction; keep `tick_plan`'s serial order and route `BlockEntityTickEffect` through an owner-to-writer hand-off.
- New profiler phase or stage: keep the boundary at a clean transition outside any held lock and keep the idle-world zero control passing (a boundary spanning wait time or leaking a previous timestamp stops reading zero).
- Another long-running native world loop uses `spawn_world_tick_task` and the shared tick implementation, with its own phase budget and no synchronous terrain generation.

## Configuration

- `native_net_runtime`: two Tokio workers for integrated play; remote play is current-thread; the browser uses neither.
- The random-tick draw count per section, the per-tick processing cap and the neighbour chain cap are fixed constants transcribed from the reference defaults (no live game-rule registry yet).
- The profiler's soft over-budget threshold is one shared constant across phases.
- The block-entity residency check reads the chunk cache bound ([`chunk-lifecycle.md`](./chunk-lifecycle.md)).

## Dependencies

The chunk source/store seam (random-tick reads, residency check; [`chunk-lifecycle.md`](./chunk-lifecycle.md)); the persistence layer for saving and restoring scheduled ticks ([`world-persistence.md`](./world-persistence.md)); the world generator, profiled but unmodified.
