# Tick scheduling: random ticks, scheduled ticks, block entities, and profiling

## What it is

The foundation every per-block-tick feature (crop growth, gravity blocks, fluid flow, fire spread,
the redstone family) is built on: a vanilla-shaped random-tick scheduler, a scheduled-tick queue for
"run again in N ticks" behavior, and a neighbor-update propagator with vanilla's own ordering and
cascade shape. It also covers how block-entity ticking is bounded to chunks that are actually
resident rather than scanning an ever-growing registry unconditionally, and the instrumentation
used to measure where the tick loop's and the world generator's own time actually goes.

## How it works

### Random ticks

Each server tick, a fixed number of random positions is drawn per eligible 16-row section, exactly
matching vanilla's own selection and its position-generating LCG (a level-local generator distinct
from the one used for a block's own random behavior once a position is picked). Section eligibility
— whether a section has any randomly-ticking block at all — is tracked as a maintained running
count updated incrementally as blocks change, rather than answered by scanning all 4,096 cells of
every section on every tick; the earlier scanning approach measured as the overwhelming majority of
the tick thread's time and was the direct cause of chunk delivery starving during a join. The
handlers that actually run for a drawn position (grass turning to dirt or vice versa, crop and
sapling growth, leaf decay) are transcribed from each block's own real predicate rather than
approximated — an earlier "is the block above bare air" proxy for grass survival was a shipped,
owner-visible bug, because real decorative surface cover like short grass is not air but has no
collision either, so every decorated grass patch silently died on its first random tick.

### Scheduled ticks

Two queues, block before fluid, drain every world tick in that fixed order. Every entry whose
trigger tick has arrived runs in `(trigger tick, priority, insertion order)`, with the whole due set
collected before any callback runs — so a tick scheduled while processing this tick's batch cannot
itself run in the same batch. A second schedule for a position/kind pair already pending is a silent
no-op, matching vanilla's dedup behavior for the same case.

Both halves are physically partitioned by chunk column. `ChunkScheduledTickQueue` routes each tick to
the column that owns its position, while its outer owner assigns one shared insertion sequence and
merges only each local queue's due head. Thus a fluid flow or a block reaction that schedules its next
step over a chunk border cannot gain or lose priority because of map traversal or the target column's
key. The current tick task still executes that merged sequence serially; local storage is an ownership
and hand-off boundary, not permission to run columns concurrently.

Block callbacks receive `ScheduledTickQueueAccess`, rather than a particular column's queue. It is the
explicit hand-off for a redstone reaction that schedules work in another owner and for a reversing
piston that must remove its own uncommitted finish tick. The interface routes by position, preserves
the world insertion sequence, and limits `take_matching` to that position's owner, so it cannot cancel
a neighbouring column's pending work. The real `IntegratedServer` tick loop consumes this block queue;
the focused controls cover positive and negative chunk coordinates, global equal-time ordering, and
the piston cancellation negative control.

### Neighbor-update propagation

A fixed visitation order (west, east, down, up, north, south) and a depth-first cascade: notifying
one neighbor whose own state change triggers further notifications resolves that whole sub-cascade
before the next sibling direction is ever notified, capped by a maximum chain length. Gravity blocks
(sand and gravel settling once unsupported) and the redstone family (dust, torches, repeaters,
comparators, observers) are both real production consumers of this exact mechanism and inherit its
ordering guarantee unchanged. A notification that would land outside the currently-ticked chunk's
own footprint is silently skipped for now — a known, deliberately-accepted limitation shared by
every consumer of this primitive, not something each new consumer needs to solve separately.

### A self-deadlock the scheduled-tick queue's own lock made possible

**Holding a lock across a call into a subsystem that can call back into the very same lock is a
self-deadlock waiting for the right input, and this queue had exactly that shape.** The tick loop
holds the scheduled-tick queues behind one lock for the whole span of a tick that reads and mutates
the world — and on a persistent world, reading the world can trigger loading a chunk from disk,
which restores that chunk's own previously-saved pending ticks back into the very same queue
structure. Restoring used to try to take the identical, non-reentrant lock a second time from
inside the first acquisition, which parks the tick thread on itself permanently: total,
deterministic, and reached the moment a world tick first touched any column that already existed on
disk with a pending tick recorded — with no error, no disconnect, and no panic, just a client stuck
loading forever. The fix stages a loaded chunk's restored ticks behind a **second**, separate lock,
merged into the real queues only from inside the original lock's own held region — with a fixed
lock order (the live queues' lock always taken before the staging lock, never the reverse) so
nothing can re-derive the original deadlock through a different call path. A brand-new world never
exercised this at all, which is why every gate built against a fresh or in-memory world stayed green
regardless — the discriminating input is specifically a **saved** chunk that also carries a pending
tick, and every existing test happened to use worlds with neither.

The broader callback-held-handle order is checked in debug and test builds by
`lodestone_server::lock_order`: the scheduled queue is before the staged queue, block-entity
registry, and mob simulation. It is a thread-local diagnostic rather than a runtime coordinator,
so release builds add no lock-tracking contention. A new callback-held handle must be placed in
that order before it can be acquired from scheduled work.

### Block-entity ticking is gated by residency, not by distance

Block entities (hoppers foremost, since only that kind actually probes world state each tick) used
to be ticked from one flat, ever-growing registry scanned unconditionally at full rate regardless of
where any player was. The originally suspected mechanism — that ticking gets slower the farther a
player walks from spawn — turned out to be false; distance itself is flat, since a single far-away
block entity only ever costs one real chunk-generation call for the whole session, not one per tick.
The real mechanism was a hard capacity threshold: as the registry's set of distinct block-entity-
bearing chunks grows past the chunk cache's own size, a cyclic scan through a bounded cache
eventually touches every entry between two visits to the same one, so the miss rate jumps from
effectively zero to effectively total once that threshold is crossed — turning an otherwise-cheap
per-tick scan into hundreds of full chunk regenerations every single tick. The fix ties each
block entity's tick to whether its own chunk is actually resident in the cache right now (a plain,
non-generating lookup, deliberately not one that extends that chunk's own residency just for being
checked — a lookup must not buy a column life it did not otherwise earn) and skips ticking it
entirely when it is not, mirroring vanilla's own behavior of only ticking block entities belonging
to a currently loaded chunk. The registry itself still has **no eviction of its own entries** — a
block entity's simulated state must be able to keep advancing the moment its chunk becomes resident
again, exactly as if it had never left, which is a different property from whether it gets ticked on
any given pass.

### Profiling the tick loop and world generation

Two independent, per-phase/per-stage timing instruments exist for finding where time actually goes,
built specifically to capture the **tail** of a distribution (a worst window, not just an average)
after this repo's own history of a real timeout being misdiagnosed from a mean that hid the one slow
window that mattered. The tick loop is split into a small number of coarse phases at boundaries
chosen specifically to avoid adding a timing checkpoint inside the one region that already holds the
scheduled-tick lock across a large span of code — only one phase covers that whole region, since it
is also the only phase that can trigger a real chunk generation mid-tick and is therefore the one
most likely to show a real stall. World generation is profiled per internal stage (shape, carving,
ore placement, vegetation, and so on) as percentiles across a batch of columns, bypassing the
generator's own internal caches so every profiled column pays its full, uncached cost rather than
some columns landing on a cache hit that makes a stage look artificially cheap. Both instruments are
validated with a control that must read as exactly zero (an idle world under a paused, deterministic
clock, where nothing measured could possibly do any real work) rather than merely "small", since a
duration-based instrument has no other way to prove its own boundary isn't leaking time from
somewhere it shouldn't.

**Which stage dominates world generation depends entirely on which real-world condition is being
measured, and the two conditions give different, equally correct answers.** Generating a whole
cold, never-before-touched region is dominated by decoration (trees, grass, and similar
placement-heavy work); generating one more column at the edge of an already-explored area — the
condition an ordinary walking player actually produces almost all the time — is instead dominated by
ore placement, because decoration's own cost is largely a function of neighboring context that a
steady-state column has usually already paid for. An optimization effort aimed at ordinary play
should be judged against the steady-state condition, not the cold-region one, even though the
cold-region number is the more dramatic-looking figure.

## How to change it

- **Adding a new randomly-ticking block**: extend the per-block dispatch and the section-eligibility
  classification together — a block that's added to one but not the other either never ticks or is
  drawn for but never handled.
- **Adding a real scheduled-tick producer**: call the scheduling primitive from wherever a block
  decides "run again in N ticks", the same way vanilla's own tick-scheduling call works; it does not
  care what value type it schedules.
- **Changing tick ownership**: preserve the outer queue's one global insertion sequence and due-head
  merge. Do not drain columns in coordinate order or assign an insertion counter per column: either
  change can reorder equal-time updates crossing a column border. Keep block reactions behind
  `ScheduledTickQueueAccess`; directly retaining a local queue would bypass the cross-owner hand-off
  and make piston interruption ambiguous.
- **Adding a real neighbor-update producer**: call the propagator once per mutated position with a
  callback that performs the mutation and returns any further single-target notifications that
  mutation itself triggers — never call the propagator recursively from inside that callback, since
  its own explicit stack already handles cascading and a nested call would double-count the chain
  limit.
- **Widening the area that gets ticked at all**: this needs a real per-tick, multi-column terrain
  cache first; the current tick area is deliberately small and reuses the same fixed radius the mob
  simulation already tracks, rather than introducing a second "which chunks are loaded" concept.
- **Never let a lock guarding one of these queues be held across a suspension point or a second,
  independent acquisition of itself** — see the self-deadlock note above; stage-and-merge, don't
  nest the same lock.
- **Do not give the block-entity registry an eviction path tied to chunk-cache eviction** — a cache
  eviction is not the same event as "this world state no longer exists," and conflating them would
  silently stop a block entity's simulated state from resuming correctly once its chunk becomes
  resident again.
- **Adding a new tick-loop phase or worldgen stage to the profiler**: keep the boundary at a clean
  section transition outside any held lock, and keep the instrument's own zero-cost idle-world
  control passing — a boundary that accidentally spans part of the wait between ticks, or leaks a
  previous tick's timestamp forward, will not read as exactly zero anymore.

## Configuration

- The random-tick draw count per section is vanilla's own game-rule default; this crate has no live
  game-rule registry backing it yet, so it is a fixed constant rather than something read live.
- The scheduled-tick queues' own per-tick processing cap and the neighbor-update chain-length cap
  are both transcriptions of vanilla's own constants.
- The tick-loop profiler's soft "over budget" threshold is one shared constant across every phase,
  deliberately coarse rather than separately tuned per phase, since no loaded-server measurement
  yet justifies three different numbers.
- The block-entity residency check has no separate configuration; it simply reads whatever bound
  the underlying chunk cache is already configured with (see `docs/chunk-lifecycle.md`).

## Dependencies

- The chunk source/store seam for random ticks' world reads and for the block-entity residency
  check (see `docs/chunk-lifecycle.md`).
- The persistence layer for saving and restoring scheduled ticks across a world reopen (see
  `docs/world-persistence.md`).
- The world generator, profiled directly by the worldgen half of the instrumentation described
  above; unmodified by any of this.
