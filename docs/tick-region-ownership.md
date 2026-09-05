# Tick region ownership

## What it is

`lodestone_server::tick_region::TickRegionPlan` makes the ownership of every
chunk selected for a server tick explicit. The current plan assigns every
selected chunk to the smallest region possible, its own `TickOwner::Chunk`, but
the server executes those owners serially while parity and populated-world
profiling remain prerequisites for concurrent workers.

## How it works

`tick_area::FollowArea` first builds its stable, duplicate-free chunk set from
player anchors (or its playerless fallback). It then stores that set in a
`TickRegionPlan`, which attaches a `TickOwner::Chunk { cx, cz }` to every entry.
The production tick loop consumes `FollowArea::owned_chunks()` for the
per-chunk random-tick and thunder-decision passes. This is an execution
boundary, not a concurrency claim: the iterator retains the old canonical
order, so its random-number draws and visible results cannot move merely by
introducing ownership.

The plan rejects a duplicated list before assigning ownership while preserving
the producer's visit order. Stable order matters because random-tick draws
consume chunks in that order; a duplicate would advance one chunk twice in a
tick. This check makes the one-owner-per-chunk invariant testable now and
provides the input contract a future partitioner must preserve.

`TickRegionPlan::owner_workloads` reports the current real ownership (one
single-chunk workload per selected column). `FollowArea::spawnable_chunks`
consumes that report on the live tick path, so the count cannot become an
unobserved parallel data structure. Scheduled queues and block entities now
have their own chunk-local ownership seams: `ChunkScheduledTickQueue` keeps
pending ticks at their target chunk and merges due heads into one serial order,
while `BlockEntityRegistry::tick_plan` assigns its tick-start snapshot to
chunk owners. It executes one owner batch at a time and returns a
`BlockEntityTickEffectBatch` for each owner, including an empty batch when no
visible write occurred. `tick::apply_block_entity_effect_batches` is the sole
central consumer: it applies each batch and publishes its furnace changes in
the established owner order. This makes the message boundary real without
claiming workers can run concurrently. The ambient entity-effect phase uses the
same shape: `MobSim::take_ambient_sound_effect_batches` groups each emitted
effect under `EntityTickOwner::Chunk`, and
`tick::apply_entity_effect_batches` is the only publisher to the connection
feed. Entity simulation itself remains serial in its existing vector order.
Effects carry that original sequence because entities from two chunks may be
interleaved; the central publisher restores it rather than changing behavior to
owner-major order. Negative positions use `floor` followed by Euclidean chunk
division, so an entity at `x = -0.5` belongs to chunk `-1`, not chunk `0`.
Chunk lifecycle has the same explicit smallest owner before the cache crosses
its source boundary. `ChunkLifecyclePlan` assigns each on-demand load and each
selected cache release to `ChunkLifecycleOwner::Chunk { cx, cz }`; `ChunkStore`
consumes it for the real `column_at` and `unload` calls used by every
`IntegratedServer`. Ticket transitions remain demand-driven: becoming resident
does not pre-generate a column, and becoming unresident only releases a column
that is actually cached. An eviction batch is bounded by current cache entries,
deduplicated, and ordered `(cx, cz)` before `ChunkSource::unload`, so the
hash-map iteration behind a ticket delta cannot make negative-column unload
order vary. This is a serial hand-off, not an unload worker or an I/O change.
Entities outside this ambient-effect hand-off, natural-spawn planning, world
border, game rules, time, weather and other cross-column work remain global.
For a named populated scene,
`FollowArea::candidate_region_workload` groups the same selected chunks into an
observer-supplied spatial edge using Euclidean division. Its sorted cell counts,
total, and largest-cell count establish whether that scene is spatially spread
out or concentrated before any worker or lock is introduced. Candidate cells
are measurements only; they do not alter the chunk-owner assignment or
simulation order.

`chunk_owner_profile::SCENE_NAME` is the separate deterministic populated
workload for this ownership seam. Its eight resident chunk owners each receive
a furnace, one due block tick and one due fluid tick; 64 cows are distributed
across those owners so periodic ambient effects return through entity-owner
batches. The `profile-harness` feature gives the standard run a paused runtime
that advances exactly 128 tick periods. `TickStats::owner_work` accumulates due
block/fluid entries and the block-entity/entity batch and effect counts at the
live central hand-offs, so the profile proves its work crossed those boundaries
rather than merely being seeded in a fixture.

## How to change it

Do not add region workers or coalesce these chunk owners into a larger region
size in this module alone. First complete the parity and named-scene profiling
prerequisites in `docs/plans/regionised-server-ticking.md`. A concurrent
partitioning change must retain deterministic ordering within each owner and
add an explicit cross-owner hand-off path before any mutation may cross a
boundary. Reuse the block-entity shape: an owner returns a typed batch and the
central writer applies batches in a declared order. Do not give a worker a
second owner's `ChunkSource` just to avoid defining that message.

Keep `FollowArea` as the producer until the live tick loop obtains its work
through another production-consumed boundary. Any new producer must remove
duplicates before constructing a plan and preserve a deliberate visit order;
bypassing that check makes duplicate random ticks possible.

When extending entity ownership, do not publish from a chunk owner. Add a typed
batch to `MobSim`, retain an explicit source position and old serial sequence,
and make `tick` centrally consume it. Grouping alone is not parity: if owners
are interleaved in the serial simulation list, applying all of one owner's
effects before another's changes observable packet order. Add both a
negative-coordinate control and an interleaved-owner order control before a
future executor is allowed to run owners separately.

Keep lifecycle planning at the existing cache boundaries. A ticket becoming
resident is permission for a later demand load, not authority to generate in a
background task. A ticket or LRU eviction may call `ChunkSource::unload` only
after it has removed that retained cache entry, and must pass the selected
coordinates through `ChunkLifecyclePlan`; calling the source under the cache
lock would reintroduce the lock/I/O and re-entry hazards this boundary avoids.
If a future owner can release a column concurrently, define the source-facing
acknowledgement and persistence ordering first; the current plan only makes the
serial owner and canonical order observable.

When recording a candidate report, name the populated scene and the explicit
edge passed to `candidate_region_workload`. Compare total chunks, the number of
non-empty cells, and the largest-cell count across clustered and spread-out
scenes. This is spatial workload evidence, not an MSPT measurement: use it to
decide whether a later profile deserves a regionisation experiment, never as a
claim that a particular edge is the right size for coalescing chunk owners.

For the owner-boundary workload, run `just bench-chunk-owner-tick` first. It
constructs the live scene and rejects a missing phase sample, scheduled drain,
block-entity owner batch, or ambient entity owner batch before Criterion takes
its short sample. For a local call tree, build an explicit capture path and run
`just samply-chunk-owner-tick <capture>`. Inspect the scheduled-and-physics
phase beside `owner_work.scheduled_block_ticks` and
`owner_work.scheduled_fluid_ticks`; inspect mobs-and-items beside
`owner_work.block_entity_batches`, `entity_effect_batches`, and
`entity_effects`. A high phase cost with a missing or unexpectedly small count
is a fixture/wiring failure, not evidence for parallelization.

## Configuration

None. Chunk ownership has no tunable size. Lifecycle batches are bounded by the
existing cache selection rather than a new queue capacity. A candidate edge
remains an explicit measurement argument, while any useful multi-chunk worker
size depends on profiling a populated, named workload and has not been selected.
The profile entry point accepts an optional tick count; its normal 128-tick run
is fixed so counter comparisons name the same workload. The `profile-harness`
feature is required for the fast paused-clock path and is selected by both
`just` recipes; without it the finite example uses normal server sleeps.

## Dependencies

`tick_area::FollowArea` supplies the live chunk set. `tick::run_tick_loop`
consumes its ownership sequence for random ticks and thunder decisions. The
`chunk_store::ChunkStore` consumes lifecycle assignments around its real source
load/unload boundary, including the stores owned by `IntegratedServer`.
regionised ticking design document records the prerequisites for changing this
serial ownership seam into concurrent execution.

The profile scene uses `IntegratedServer`, `BlockEntityRegistry`, scheduled
tick queues and `MobSim`; it intentionally drives their ordinary production
tick path instead of a parallel test-only executor.
