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
The synchronous ordered-batch seam reserves the complete dispatcher budget
before entering Rayon; if an unrelated async producer already owns any permit,
it runs the batch serially instead of adding another Rayon queue behind the
active work. A nested batch invoked by an admitted dispatcher worker reuses the
same Rayon's work-stealing workers directly. Its parent permit remains the
admission boundary, so Nether prewarming and sparse End dependency generation
can occupy idle workers without acquiring an impossible second full-pool lease.
The batch helper's indexed iterator runs on this same persistent Rayon pool
after the full budget is reserved; it does not create a scoped thread set or a
second blocking pool. The join path avoids a whole-batch reservation by
admitting one column per window slot, which keeps its global backpressure and
ordered-emission accounting explicit.

The initial-spawn search uses the same handoff through the server runtime seam:
native joins submit the synchronous probe to this dispatcher, while the browser
keeps its single-threaded path inline. This prevents a join's result wait from
depending on Tokio's blocking-pool completion path and does not introduce a
second world-tick executor.

Several connections share this same bounded permit gate. Admission is a
non-blocking try-operation: when the pool is saturated, a caller keeps its job
queued and yields from an async service point instead of holding a semaphore
waiter or blocking the runtime. Active world-generation calls never exceed the
native worker budget. Results remain emitted in queue order, independent of
completion order, and the deterministic source digest is unchanged by
dispatch. Dropping an admitted result handle cancels work that has not started;
in-progress source calls finish safely but suppress delivery.

The dispatcher changes where work runs, not the `ChunkSource` call or its
generated content; scheduler and batch gates check exact output digest and
ordering while multiple jobs are active.

The End source also exposes a spatial batch seam. It forms the union of every
requested column's three-by-three immutable input window, generates each unique
coordinate once on this pool, then decorates and attaches structure metadata in
the caller's original order. Five adjacent columns therefore need 21 immutable
base worlds instead of 45. Mutable feature writes stay ordered because their
source order is part of generated content.

For the serial/wasm batch entry point, those unique inputs use one rectangular
disabled-aquifer sampler. Its density scratch spans the full input rectangle,
so adjacent chunks share interpolation corners and flat-cache values while
each point keeps the scalar evaluator's operation order. Exact scalar-versus-
batch column bytes are the compatibility gate. Native End batches now select
the same rectangle path whenever their dependency union is complete; sparse
requests retain the per-coordinate worker fan-out. On the bounded release
8x8 adjacent fixture (100 dependencies), this reduced base generation from
0.722 s to 0.273 s and the complete production batch from 76.6 to 201.9
chunks/s. The rectangle products enter the per-coordinate cache, so overlapping
moving views still reuse exact base products and remain bounded to one
render-distance dependency footprint.

The Nether source applies the same separation to its wider five-by-five
immutable pre-decoration prefix, while keeping mixed decoration and output in
request order. It submits prewarm work only when at least eight unique prefix
coordinates are absent; smaller partial misses stay scalar because dispatcher
overhead costs more than the available work. A fully cached batch returns to
the scalar path before sorting or dispatch. The threshold is a measured
constant from the adjacent 8x8 production benchmark, not a correctness bound.

This bounds dispatch-side CPU and queue pressure; it does not make a shared
world-store coordinate lease nonblocking. Tick-side wiring must keep its lease
handoff separate rather than synchronously waiting on a generation-held lease.

The browser path is unchanged: it has no native pool or semaphore and generates
one column at a time with a browser task yield between columns. The borrowed
source arm uses the same yielding wrapper, so a target-neutral caller cannot
accidentally reach the native-only Rayon helper.

## How to change it

Change the handoff in `crates/lodestone-server/src/worldgen_dispatch.rs` or the
target-aware wrapper in `crates/lodestone-server/src/spawn.rs`, and keep the
result channel tied to the worker closure. Update
`join_scheduler::ColumnPipeline` only if the ordering or cancellation contract
changes. Native bounded producers use `try_spawn`; retain an `Err(job)` and
await `wait_for_capacity` before retrying. `spawn` remains only for callers
that explicitly choose asynchronous admission. Keep `map_columns_parallel` on
the same dispatcher/pool; replacing it
with a fresh scoped thread fan-out recreates cross-player oversubscription. Do
not move generator state behind the dispatcher: `ChunkSource` is already the
thread-safe seam, and generated content must remain independent of worker
completion order.

Extend `EndGenerator::columns_spatial_batch` and
`EndChunkSource::generate_batch` when another immutable End stage becomes
shareable. Keep dependency products indexed by coordinate and final output in
requested order. Do not move `EndDecoration::apply_region` into the fan-out
until its cross-source mutation stream is an explicit ordered product.
The End source's complete-rectangle test is deliberately a throughput choice,
not a correctness condition: keep the dispatcher fallback for sparse or
non-rectangular requests, and retain the scalar-versus-batch byte gate when
changing either branch.

The gates
`concurrent_pipelines_share_a_core_budget_and_preserve_content_digest` and
`concurrent_offloaded_batches_share_rayon_workers_and_content` check global
backpressure plus exact output content/order. The release-only join efficiency
sweep remains the source for measured 1/2/4/8 window comparisons. Dispatch
overhead is separate from the eventual decorated-chunk throughput target.

The ignored `lodestone-server` gate
`production_lifecycle_generation_is_identical_at_1_2_4_and_8_workers` covers
the other boundary: it loads one fixed adjacent batch through the real
Overworld, Nether, and End sources, admits every coordinate through
`ChunkLifecycleHandoff`, and compares packet-relevant column components against
a one-worker baseline at 2, 4, and 8 workers. The comparison includes palette
order, section indices, three-dimensional biomes, block entities, and the
stored motion-blocking map. It reports the first coordinate and component
whose bytes differ, rather than accepting a reduction that could hide a
cross-chunk spill. Run it in release when changing worldgen or lifecycle
admission:

```text
cargo test --release -p lodestone-server --lib \
  production_lifecycle_generation_is_identical_at_1_2_4_and_8_workers \
  -- --ignored --nocapture
```

## Configuration

No environment variable or runtime flag changes the pool. Native worker count
is `max(available_parallelism - 1, 1)`, reserving one hardware thread for the
runtime and authoritative tick. `generation_window` follows that worker count
and keeps its floor of two; saturation is backpressure rather than queue
growth. WASM has no native pool and keeps the serial platform-specific path.

## Dependencies

The dispatcher uses Rayon and Tokio oneshot channels, and is consumed by
`join_scheduler::ColumnPipeline` plus the offloaded batch helpers in
`chunk.rs`. It relies on the `ChunkSource: Send + Sync` contract and does not
depend on generator internals beyond that seam.
