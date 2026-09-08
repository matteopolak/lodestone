# Integrated world-generation dispatch

## What it is

The integrated server generates chunk columns on native workers while keeping
the async connection and tick tasks serviceable. One reusable Rayon pool is
shared by join streams, view batches, and background seed work. A process-wide
semaphore admits at most the pool's worker count, so simultaneous players wait
asynchronously instead of multiplying native workers or growing an unbounded
queue.

## How it works

`ColumnPipeline` keeps wire order in its own queue and submits one job per
window slot to the shared Rayon pool. Results return through Tokio oneshot
channels, so awaiting a slow ordered head never blocks the runtime. Batch paths
submit indexed Rayon work to that same pool, so they share the same CPU budget.

Several connections share this same process-wide semaphore. Their work can
still overlap, but active world-generation calls never exceed the native worker
budget. Results remain emitted in queue order, independent of completion order,
and the deterministic source digest is unchanged by dispatch.

The dispatcher changes where work runs, not the `ChunkSource` call or its
generated content; scheduler and batch gates check exact output digest and
ordering while multiple jobs are active.

The browser path is unchanged: it has no native pool or semaphore and generates
one column at a time with a browser task yield between columns. The borrowed
source arm uses the same yielding wrapper, so a target-neutral caller cannot
accidentally reach the native-only Rayon helper.

## How to change it

Change the handoff in `crates/lodestone-server/src/worldgen_dispatch.rs` and
keep the result channel tied to the worker closure. Update
`join_scheduler::ColumnPipeline` only if the ordering or cancellation contract
changes. Keep `map_columns_parallel` on the same dispatcher/pool; replacing it
with a fresh scoped thread fan-out recreates cross-player oversubscription. Do
not move generator state behind the dispatcher: `ChunkSource` is already the
thread-safe seam, and generated content must remain independent of worker
completion order.

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
`chunk.rs`. It relies on the `ChunkSource: Send + Sync` contract and does not
depend on generator internals beyond that seam.
