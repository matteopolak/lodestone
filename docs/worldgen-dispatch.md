# Integrated world-generation dispatch

## What it is

The integrated server generates chunk columns on native workers without starving the async connection and tick tasks. A bounded blocking pool owns generation requests, a persistent Rayon pool runs their parallel immutable work, and a process-wide permit gate makes simultaneous players wait asynchronously instead of growing an unbounded queue.

## How it works

- `ColumnPipeline` keeps wire order in its own queue and admits one batch at a time: one column first, then up to `generation_window`. A batch takes one dispatcher permit via `try_spawn`, runs on Tokio's blocking pool, and fans out over Rayon through `run_ordered`, collecting results in request order. Even a one-column batch goes through Rayon.
- Results return through oneshot channels, so awaiting a slow ordered head never blocks the runtime. Cancelling a request lets the column finish but drops its result; dropping an admitted handle cancels work that has not started.
- The synchronous ordered-batch seam, outside a dispatch job, reserves `min(batch size, dispatcher budget)` permits before entering Rayon, or runs serially if async producers own every permit. A nested batch from an admitted job reuses Rayon without a second permit.
- Admission is a non-blocking try: a saturated caller keeps its job queued and yields. Active blocking jobs never exceed the worker budget. Output order and the deterministic source digest are independent of completion order and of dispatch.
- The initial-spawn probe goes through the same handoff on native; the browser keeps its single-threaded inline path.
- Join encodes run one at a time per connection (`OrderedJoinEncodes`), because each encode reads the source as it stands. Light settlement uses the pool too, one destination per connection: tick changes use resident terrain only, direct edits may complete a cold footprint, and the connection rechecks the destination is still delivered before sending.
- **Stage receipts.** A connection tracks a column's requested stage separately from the stage written to its transport. A resident complete column chosen during packet preparation counts as a Full delivery, which prevents a second whole-column packet later. The receipt follows the column actually encoded (including the fallback when initial lighting returns `NoLight`); opaque preencoded packets carry no stage and keep the conservative reservation. Encoding and queue admission are not delivery: the transport write must succeed first, and a lower-stage receipt never erases a reserved Full.
- **Incarnations.** Each time a column enters the view it gets a new connection-local incarnation; pending encodes and ack-gated batches hold that token, so leaving and re-entering a coordinate cannot deliver an old packet or promote the new residency. Dimension resets keep the counter. Native and browser send paths use the same checks.
- Threaded browser builds submit owned packet preparation to their compute pool through `owned_compute`, one job at a time, holding the permit until the connection owner accepts the result ([browser worldgen worker](browser-worldgen-worker.md)).
- This bounds dispatch CPU and queue pressure; it does not make a shared world-store coordinate lease nonblocking, so tick-side code must not synchronously wait on a generation-held lease.
- **Browser.** The same `ColumnPipeline` runs batches as local tasks on the worker, generating columns one after another, so a long column is a non-yielding span. The server worker encodes columns before sending over a byte-credit `MessagePort`; the block-update sender shares that loop, so a write awaiting credit also postpones chunks. Measure generation, light settlement, wire delivery, client receipt and mesh presentation separately. The worker progress port carries `targetX`/`targetZ` and sampled `wire-delivered` events on session zero (`admitted` = delivered plus outstanding, `completed` = cumulative delivered, `queued` = outstanding), measuring transport completion rather than presentation.

## How to change it

- Change the handoff in `crates/lodestone-server/src/worldgen_dispatch.rs` or the target-aware wrapper in `crates/lodestone-server/src/spawn.rs`, keeping the result channel tied to the worker closure. Touch `join_scheduler::ColumnPipeline` only if ordering or cancellation changes.
- Never admit a second join encode before the first is delivered.
- Tie stage receipts to the selected packet column and the successful `send_encoded_column` write. A new encoding path must carry the exact stage or stay opaque; rereading the source afterwards does not identify the encoded bytes.
- Bounded native producers use `try_spawn`: keep `Err(job)` and `await wait_for_capacity` before retrying. `spawn` is only for callers choosing asynchronous admission.
- Keep `run_worldgen_jobs` on the same dispatcher and pool; a fresh scoped thread fan-out recreates cross-player oversubscription. Do not move generator state behind the dispatcher: `ChunkSource` is already the thread-safe seam.
- Gates: `concurrent_pipelines_share_a_core_budget_and_preserve_content_digest` and `concurrent_offloaded_batches_share_rayon_workers_and_content` check global backpressure plus exact content and order.

## Configuration

- Worker count defaults to `max(available_parallelism - 1, 1)`; `LODESTONE_WORLDGEN_WORKERS` overrides it with a positive integer.
- `generation_window` follows that count with a floor of two. The Rayon budget excludes blocking coordinators and the shell's two network workers, so measure total CPU contention before raising the override.
- WASM has no native blocking dispatcher; the threaded artifact uses the bounded owned-compute pool.

## Dependencies

Rayon and Tokio oneshots; consumed by `join_scheduler::ColumnPipeline` and the offloaded batch helpers in `chunk.rs`. Relies only on the `ChunkSource: Send + Sync` contract.
