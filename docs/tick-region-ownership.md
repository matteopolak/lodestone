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

## Configuration

None. There is intentionally no region-size setting: a useful size depends on
profiling a populated, named workload and has not been selected.

## Dependencies

`tick_area::FollowArea` supplies the live chunk set. `tick::run_tick_loop`
consumes it through `FollowArea::chunks()`. The regionised ticking design
document records the prerequisites for changing this single-owner seam.
