# Integrated world-generation dispatch

## What it is

The integrated server generates chunk columns on native blocking workers while
keeping the async connection and tick tasks serviceable. A single shared budget
limits all join, view-batch, and background seed dispatch to the machine's
available parallelism.

## How it works

`ColumnPipeline` keeps the wire order in its own queue and submits one job per
window slot to the shared Rayon pool. Results return through Tokio oneshot
channels, so awaiting a slow ordered head never blocks the runtime. Batch paths
submit their own indexed Rayon work to that same pool; they therefore cannot
multiply the core count when they overlap a join.

Several connections share this same process-wide semaphore. Their work can
still overlap, but active world-generation calls never exceed the native
worker budget. Results remain emitted in queue order, independent of completion
order, and the deterministic source digest is unchanged by dispatch.

The browser path is unchanged: it has no native blocking pool or semaphore and
generates one column at a time with a browser task yield between columns. The
borrowed source arm uses the same yielding wrapper, so a target-neutral caller
cannot accidentally reach the native-only Rayon helper.

## How to change it

Change the admission policy in `crates/lodestone-server/src/worldgen_dispatch.rs`
and keep the result handoff tied to the worker closure. Update
`join_scheduler::ColumnPipeline` only if the ordering or cancellation contract
changes. Do not move generator state behind the dispatcher: `ChunkSource` is
already the thread-safe seam, and generated content must remain independent of
worker completion order.

The unit gate
`concurrent_pipelines_share_a_core_budget_and_preserve_content_digest` checks
both multi-pipeline backpressure and exact output content/order. The existing
join efficiency sweep remains the source for release measurements across
windows; compare 1, 2, 4, and 8 worker configurations rather than inferring
throughput from a single host.

## Configuration

No environment variable or runtime flag changes the budget. Native Rayon uses
its available-parallelism worker pool. The pipeline window remains the
scheduler's separate per-connection backpressure policy, while the shared pool
is the process-wide CPU boundary.

## Dependencies

The dispatcher uses Rayon and Tokio oneshot channels, and is consumed by
`join_scheduler::ColumnPipeline` plus the offloaded batch helpers in
`chunk.rs`. It relies on the `ChunkSource: Send + Sync` contract and does not
depend on generator internals beyond that seam.
