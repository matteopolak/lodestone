# Integrated world-generation dispatch

## What it is

The integrated server generates chunk columns on native workers while keeping
the async connection and tick tasks serviceable. One reusable Rayon pool is
shared by join streams, view batches, and background seed work.

## How it works

`ColumnPipeline` keeps wire order in its own queue and submits one job per
window slot to the shared Rayon pool. A process-wide semaphore admits at most
the pool's worker count, so simultaneous players wait asynchronously instead
of multiplying native workers or growing an unbounded queue. Results return
through Tokio oneshot channels, so awaiting a slow ordered head never blocks
the runtime. Batch paths use indexed Rayon work on the same pool, so they share
the same CPU budget.

Emission order remains queue order, independent of completion order. The
dispatcher changes where work runs, not the `ChunkSource` call or its generated
content; scheduler and batch gates check exact output digest and ordering while
multiple jobs are active.

The browser path is unchanged: it has no native pool and generates one column
at a time with a browser task yield between columns.

## How to change it

Change the handoff in `crates/lodestone-server/src/worldgen_dispatch.rs` and
keep the result channel tied to the worker closure. Update
`join_scheduler::ColumnPipeline` only if the ordering or cancellation contract
changes. Keep `map_columns_parallel` on the same dispatcher/pool; replacing it
with a fresh scoped thread fan-out recreates cross-player oversubscription.

The gates
`concurrent_pipelines_share_a_core_budget_and_preserve_content_digest` and
`concurrent_offloaded_batches_share_rayon_workers_and_content` check global
backpressure plus exact output content/order. The release-only join efficiency
sweep remains the source for measured 1/2/4/8 window comparisons. Dispatch
overhead is separate from the eventual decorated-chunk throughput target.

## Configuration

No environment variable or runtime flag changes the pool. Native Rayon chooses
its worker count from available parallelism. The pipeline window remains the
scheduler's per-connection admission policy, while the shared pool is the
process-wide CPU boundary.

## Dependencies

The dispatcher uses Rayon and Tokio oneshot channels, and is consumed by
`join_scheduler::ColumnPipeline` plus the offloaded batch helpers in
`chunk.rs`. It relies on `ChunkSource: Send + Sync` and does not depend on
generator internals beyond that seam.
