# Integrated world-generation dispatch

## What it is

The integrated server generates chunk columns on native workers while keeping
the async connection and tick tasks serviceable. A bounded blocking pool owns
generation requests; a persistent Rayon pool executes their parallel immutable
work. A process-wide semaphore limits admitted requests, so simultaneous
players wait asynchronously instead of growing an unbounded queue.

## How it works

`ColumnPipeline` keeps wire order in its own queue and submits bounded jobs to
Tokio's blocking pool. A production job can own a cohort of up to 64 targets.
Results return through oneshot channels,
so awaiting a slow ordered head never blocks the runtime. A request can hold a
region lease while its immutable batch runs on the Rayon compute pool. Keeping
the blocking lease wait off Rayon prevents it from occupying a worker needed
by another request's nested batch.
The synchronous ordered-batch seam reserves the complete dispatcher budget
before entering Rayon; if an unrelated async producer already owns any permit,
it runs the batch serially. A nested batch from an admitted request reuses
Rayon without acquiring a second permit. Indexed collection preserves the
request's result order. The join path retains backpressure and ordered emission.
The region wait belongs to the cohort, not its first target: cancelling one
target leaves the reservation active for surviving siblings. When every target
is cancelled, the pending reservation is removed so later overlapping work can
proceed.

Sources declaring `ResidentCohortDelivery::Terminal` can emit full resident
columns without turning the remaining cold targets into a buffered batch.
The cold subset keeps the same dependency region and incremental writer fences;
the join queue restores wire order from the original slot indices. The default
`AfterSettlement` policy, which `Terrain263ChunkSource` keeps, preserves batch
publication for sources whose later writers can change a resident target.
Checkpoint-only reused outputs and mixed
shaped/full requests also retain batch publication. Native and cooperative
browser admission apply the same guard; changing a source's write ownership
requires revisiting its delivery policy.

The initial-spawn search uses the same handoff through the server runtime seam:
native joins submit the synchronous probe to this dispatcher, while the browser
keeps its single-threaded path inline. The blocking work never occupies the
network runtime workers.

The 26.2 protocol can encode both generated packet snapshots and existing source-backed columns,
and settle tick or direct-edit lighting on this pool. Native joins admit up to four independent
packet-snapshot encodes at once and emit their results in stream order. An existing source-backed
column fences the window because its lighting can update retained world state; it settles only
after earlier snapshots and before later admissions. The window also bounds retained snapshot
halos. Light
settlement admits one destination per connection; tick changes use resident terrain only, while
direct edits may complete a cold footprint on the worker. Before sending a light result, the
connection checks that its destination is still delivered and its retained snapshot is current.

Each connection tracks a column's requested stage separately from the stage
successfully written to its transport. A shaped request can select an already
complete resident column during packet preparation; that Full delivery prevents
a second whole-column packet when the player later enters its generation band.
The receipt follows the actual column selected for encoding, including the
fallback column when initial lighting returns `NoLight`. Opaque preencoded
packets and legacy detached encoders do not authenticate a stage and preserve
the conservative reservation behavior.

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

Threaded browser cohorts submit owned admission, Overworld feature bodies and
packet preparation to their initialized compute pool through `owned_compute`.
These roles share one permit through worker completion and owner acceptance.
Feature bodies transfer the existing epoch and revision log without copying;
the owner alone projects writes, advances the canonical cursor and publishes.
Full and sparse completion share the same typed body with native orchestration
workers and the cooperative serial browser driver. Cancellation cannot turn a
lost epoch into scalar regeneration or skip a writer needed by a live sibling.
See [Browser world-generation worker](browser-worldgen-worker.md) for role
timings, packet fences and serial-fallback boundaries.

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

The browser uses the same production generation session, cohort publication,
and ordered target queue. Its Overworld source admits at most 16 nearby targets
after the singleton center and publishes stable outputs incrementally. A
threaded build splits pristine prefix preparation into disjoint four-chunk-wide
jobs on its initialized worker pool. Native retains eight-chunk-wide prefix
sharing, and the serial browser uses the same job boundary without parallelism.
All three collect shaped carriers in request order; both browser artifacts use
the cooperative mutable stage driver. The threaded browser awaits owned compute
jobs rather than joining the pool on its connection owner. Serial prefix and
feature bodies still run inline and must be included in non-yield-span
measurements. The server worker currently encodes
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
changes. The join encode window lives in `join_scheduler::OrderedJoinEncodes`; keep
its serial fence if changing the window size or adding another payload kind.
Keep stage receipts tied to the selected packet column and the successful
`send_encoded_column` write. Adding an encoding path requires either carrying
that exact stage or retaining the opaque conservative path; rereading the
source after encoding does not identify the bytes that were encoded.
Native bounded producers use `try_spawn`; retain an `Err(job)` and
await `wait_for_capacity` before retrying. `spawn` remains only for callers
that explicitly choose asynchronous admission. Keep `map_columns_parallel` on
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
include blocking cohort coordinators or the shell's two network workers, so
measure total CPU contention before increasing the override. WASM has no native
blocking dispatcher; its threaded artifact uses the bounded owned-compute pool.
The native join encode window
is `clamp(worker_count - 1, 1, 4)`; it reserves capacity for generation and
limits retained snapshots independently of render distance.

## Dependencies

The dispatcher uses Rayon and Tokio oneshot channels, and is consumed by
`join_scheduler::ColumnPipeline` plus the offloaded batch helpers in
`chunk.rs`. It relies on the `ChunkSource: Send + Sync` contract and does not
depend on generator internals beyond that seam.
