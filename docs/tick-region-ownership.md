# Tick region ownership

## What it is

`lodestone_server::tick_region::TickRegionPlan` makes the ownership of every
chunk selected for a server tick explicit. The current plan has exactly one
owner, `TickOwner::Global`, because the server remains single-threaded while
parity and populated-world profiling are still prerequisites for partitioning.

## How it works

`tick_area::FollowArea` first builds its stable, duplicate-free chunk set from
player anchors (or its playerless fallback). It then stores that set in a
`TickRegionPlan`; the production tick loop reads `FollowArea::chunks()`, which
now delegates to the plan. The shape is therefore live even though the owner is
still global.

The plan rejects a duplicated list before assigning ownership while preserving
the producer's visit order. Stable order matters because random-tick draws
consume chunks in that order; a duplicate would advance one chunk twice in a
tick. This check makes the single-owner invariant testable now and provides the
input contract a future partitioner must preserve.

`TickRegionPlan::owner_workloads` reports the current real ownership (one
global owner and its selected-chunk count). `FollowArea::spawnable_chunks`
consumes that report on the live tick path, so the count cannot become an
unobserved parallel data structure. For a named populated scene,
`FollowArea::candidate_region_workload` groups the same selected chunks into an
observer-supplied spatial edge using Euclidean division. Its sorted cell counts,
total, and largest-cell count establish whether that scene is spatially spread
out or concentrated before any worker or lock is introduced. Candidate cells
are measurements only; they do not alter the global owner or simulation order.

## How to change it

Do not add region workers or choose a region size in this module alone. First
complete the parity and named-scene profiling prerequisites in
`docs/plans/regionised-server-ticking.md`. A partitioning change must replace
the one global owner with non-overlapping owners, retain deterministic ordering
within each owner, and add an explicit cross-owner hand-off path before any
mutation may cross a boundary.

Keep `FollowArea` as the producer until the live tick loop obtains its work
through another production-consumed boundary. Any new producer must remove
duplicates before constructing a plan and preserve a deliberate visit order;
bypassing that check makes duplicate random ticks possible.

When recording a candidate report, name the populated scene and the explicit
edge passed to `candidate_region_workload`. Compare total chunks, the number of
non-empty cells, and the largest-cell count across clustered and spread-out
scenes. This is spatial workload evidence, not an MSPT measurement: use it to
decide whether a later profile deserves a regionisation experiment, never as a
claim that a particular edge is the right worker size.

## Configuration

None. There is intentionally no region-size setting: a candidate edge is an
explicit argument to a measurement, while a useful worker size depends on
profiling a populated, named workload and has not been selected.

## Dependencies

`tick_area::FollowArea` supplies the live chunk set. `tick::run_tick_loop`
consumes it through `FollowArea::chunks()`. The regionised ticking design
document records the prerequisites for changing this single-owner seam.
