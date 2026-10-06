# Integrated world-generation dispatch

## What it is

The integrated server generates chunk columns on native workers while keeping
the async connection and tick tasks serviceable. A bounded blocking pool owns
generation requests; a persistent Rayon pool executes their parallel immutable
work. A process-wide semaphore limits admitted requests, so simultaneous
players wait asynchronously instead of growing an unbounded queue.

## How it works

`ColumnPipeline` keeps wire order in its own queue and admits one batch at a
time: a single column first, then up to `generation_window` columns. A batch
takes one dispatcher permit through `try_spawn` and runs on Tokio's blocking
pool; inside it, `run_ordered` fans the columns out over the Rayon pool and
collects them in request order. Even a one-column batch goes through the pool,
so the batch's own blocking thread never adds a busy thread beside the pool's
workers. Results return through oneshot channels, so awaiting a slow ordered
head never blocks the runtime. Cancelling a request lets its column finish but
drops its result before it reaches the ready queue.

The synchronous ordered-batch seam, called outside a dispatch job, reserves
the smaller of the batch size and the dispatcher budget before entering Rayon;
if an unrelated async producer already owns every permit, it runs the batch
serially. A nested batch from an admitted job reuses Rayon without acquiring a
second permit.

The initial-spawn search uses the same handoff through the server runtime seam:
native joins submit the synchronous probe to this dispatcher, while the browser
keeps its single-threaded path inline. The blocking work never occupies the
network runtime workers.

Join encodes run one at a time per connection (`OrderedJoinEncodes`): each
encode reads the source as it stands when it runs, so the next is admitted only
after the previous one is delivered. Light settlement also runs on this pool
and admits one destination per connection; tick changes use resident terrain
only, while direct edits may complete a cold footprint on the worker. Before
sending a light result, the connection checks that its destination is still
delivered.

Each connection tracks a column's requested stage separately from the stage
successfully written to its transport. A shaped request can select an already
complete resident column during packet preparation; that Full delivery prevents
a second whole-column packet when the player later enters its generation band.
The receipt follows the actual column selected for encoding, including the
fallback column when initial lighting returns `NoLight`. Opaque preencoded
packets do not authenticate a stage and preserve the conservative reservation
behavior.

A column receives a new connection-local incarnation whenever it enters the
view. Pending encodes and acknowledgement-gated batches retain that token, so
forgetting and reentering the same coordinate cannot deliver an old packet or
promote the new residency. Dimension resets preserve the incarnation counter.
Encoding and queue admission never count as delivery: the transport write must
succeed before the receipt advances, and a lower-stage receipt cannot erase a
previously reserved Full request. Native and browser send paths use the same
receipt checks.

Several connections share this same bounded permit gate. Admission is a
non-blocking try-operation: when the pool is saturated, a caller keeps its job
queued and yields from an async service point instead of holding a semaphore
waiter or blocking the runtime. Active blocking jobs never exceed the
native worker budget. Results remain emitted in queue order, independent of
completion order, and the deterministic source digest is unchanged by
dispatch. Dropping an admitted result handle cancels work that has not started;
in-progress source calls finish safely but suppress delivery.

The dispatcher changes where work runs, not the `ChunkSource` call or its
generated content; scheduler and batch gates check exact output digest and
ordering while multiple jobs are active.

Threaded browser builds submit owned packet preparation to their initialized
compute pool through `owned_compute`, one job at a time; the job keeps its
permit until the connection owner accepts the result. See
[Browser world-generation worker](browser-worldgen-worker.md) for role timings
and serial-fallback boundaries.

This bounds dispatch-side CPU and queue pressure; it does not make a shared
world-store coordinate lease nonblocking. Tick-side wiring must keep its lease
handoff separate rather than synchronously waiting on a generation-held lease.

The browser uses the same `ColumnPipeline` and ordered target queue. Its
batches run as local tasks on the worker and generate their columns one after
another, so a long column is a non-yielding span and must be included in
non-yield-span measurements. The server worker currently encodes
the resulting columns before sending them over a byte-credit-limited
`MessagePort`, while the client worker decodes and meshes them. The block-update sender shares the
same connection loop as chunk streaming, so a write awaiting transport credit
also postpones new chunks. Measure generation, light settlement, wire delivery,
client receipt, and mesh presentation separately; generation throughput alone
does not predict when terrain becomes visible.

The worker progress port carries `targetX` and `targetZ` for generation stages
and sampled `wire-delivered` events. Wire events use session zero for the
connection-wide stream: `admitted` is delivered plus outstanding targets,
`completed` is cumulative delivered targets, and `queued` is the outstanding
count. They measure transport completion, not client mesh presentation.

## How to change it

Change the handoff in `crates/lodestone-server/src/worldgen_dispatch.rs` or the
target-aware wrapper in `crates/lodestone-server/src/spawn.rs`, and keep the
result channel tied to the worker closure. Update
`join_scheduler::ColumnPipeline` only if the ordering or cancellation contract
changes. The join encode slot lives in `join_scheduler::OrderedJoinEncodes`; admitting a
second encode before the first is delivered would let it read a source the first is still
changing.
Keep stage receipts tied to the selected packet column and the successful
`send_encoded_column` write. Adding an encoding path requires either carrying
that exact stage or retaining the opaque conservative path; rereading the
source after encoding does not identify the bytes that were encoded.
Native bounded producers use `try_spawn`; retain an `Err(job)` and
await `wait_for_capacity` before retrying. `spawn` remains only for callers
that explicitly choose asynchronous admission. Keep `run_worldgen_jobs` on
the same dispatcher/pool; replacing it
with a fresh scoped thread fan-out recreates cross-player oversubscription. Do
not move generator state behind the dispatcher: `ChunkSource` is already the
thread-safe seam, and generated content must remain independent of worker
completion order.

The gates
`concurrent_pipelines_share_a_core_budget_and_preserve_content_digest` and
`concurrent_offloaded_batches_share_rayon_workers_and_content` check global
backpressure plus exact output content/order. Dispatch overhead is separate
from the eventual decorated-chunk throughput target.

## Configuration

Native worker count defaults to `max(available_parallelism - 1, 1)`;
`LODESTONE_WORLDGEN_WORKERS` can override it with a positive integer.
`generation_window` follows that worker count and keeps its floor of two;
saturation is backpressure rather than queue growth. The Rayon budget does not
include blocking batch coordinators or the shell's two network workers, so
measure total CPU contention before increasing the override. WASM has no native
blocking dispatcher; its threaded artifact uses the bounded owned-compute pool.
Join encodes are admitted
one at a time per connection, independently of render distance.

## Dependencies

The dispatcher uses Rayon and Tokio oneshot channels, and is consumed by
`join_scheduler::ColumnPipeline` plus the offloaded batch helpers in
`chunk.rs`. It relies on the `ChunkSource: Send + Sync` contract and does not
depend on generator internals beyond that seam.
