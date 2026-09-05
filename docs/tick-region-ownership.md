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
claiming workers can run concurrently. Entities, natural-spawn planning, world
border, game rules, time, weather and other cross-column work remain global.
For a named populated scene,
`FollowArea::candidate_region_workload` groups the same selected chunks into an
observer-supplied spatial edge using Euclidean division. Its sorted cell counts,
total, and largest-cell count establish whether that scene is spatially spread
out or concentrated before any worker or lock is introduced. Candidate cells
are measurements only; they do not alter the chunk-owner assignment or
simulation order.

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

When recording a candidate report, name the populated scene and the explicit
edge passed to `candidate_region_workload`. Compare total chunks, the number of
non-empty cells, and the largest-cell count across clustered and spread-out
scenes. This is spatial workload evidence, not an MSPT measurement: use it to
decide whether a later profile deserves a regionisation experiment, never as a
claim that a particular edge is the right size for coalescing chunk owners.

## Configuration

None. Chunk ownership has no tunable size. A candidate edge remains an explicit
measurement argument, while any useful multi-chunk worker size depends on
profiling a populated, named workload and has not been selected.

## Dependencies

`tick_area::FollowArea` supplies the live chunk set. `tick::run_tick_loop`
consumes its ownership sequence for random ticks and thunder decisions. The
regionised ticking design document records the prerequisites for changing this
serial ownership seam into concurrent execution.
