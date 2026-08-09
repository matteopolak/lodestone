# Off-task chunk encode

## What it is

`ChunkEncoder` is the seam that moves protocol column encode off the connection
task and into the blocking worker that generated the column, so the task that
owes a player a reply to their block break never spends 62 M instructions
encoding terrain first.

## How it works

Three pieces, all in `lodestone-server` plus one v770 override:

| piece | file |
|---|---|
| `ChunkEncoder` trait + `ServerProtocol::chunk_encoder()` | `crates/lodestone-server/src/protocol.rs` |
| `ColumnPayload` + `ColumnPipeline::encoding_with` | `crates/lodestone-server/src/join_scheduler.rs` |
| `generate_and_encode_columns_offloaded` (the walk path) | `crates/lodestone-server/src/chunk.rs` |
| `encode_column` (the one branch left) | `crates/lodestone-server/src/server.rs` |
| `impl ChunkEncoder for V770ServerProtocol` | `crates/protocol/v770/src/server_protocol.rs` |

`ServerProtocol` is reached as `&P` throughout `crate::server`, and
`spawn_blocking` needs a `'static` closure. Widening every entry point to
`Arc<P>` would break every `&P`-shaped call site — the same constraint that
produced `SourceRef`. So the protocol hands out an *owned* encoder instead:
`chunk_encoder()` returns `Option<Arc<dyn ChunkEncoder>>`, and the pipeline
clones that `Arc` into each per-column worker.

Two producers were encoding on the connection task, and both moved:

- **the join burst.** `ColumnPipeline::next` used to hand back a `ChunkColumn`;
  it now hands back a `ColumnPayload`, and with an encoder attached the worker
  does `source.column(cx, cz)` *and* `encoder.encode_chunk(...)` before the
  column is dropped. The connection task receives finished frame bytes.
- **the walk path**, which runs on every chunk boundary the player crosses — a
  strip of `2r + 1` columns, 33 at `view_radius = 16`. This was
  `ViewTracker::build_batch` calling `generate_and_encode_columns_offloaded`;
  it is now `send_view_update` enqueueing into the join pipeline, so the strip
  encodes on the same per-column workers the join uses. See
  [`server-view-streaming.md`](server-view-streaming.md) for why offloading the
  work was not enough on its own, and
  `generate_and_encode_columns_offloaded` for the fallback the borrowed-source
  and `wasm32` paths still take.

**The encode inside that fallback used to be serial**, which is worth recording
because it looked like it had been parallelised. `generate_columns_parallel`
fanned generation out over scoped threads, the closure joined them all, and
*then* walked the joined `Vec` calling `encode_chunk` one column at a time on a
single thread — ≈80 ms for a 33-column strip no matter how many cores generated
it, which is the whole cost the offload existed to remove, relocated rather than
removed. `chunk::map_columns_parallel` now applies the encode inside the worker
that generated the column, and `generate_columns_parallel` is a thin wrapper over
it with an identity transform. Peak memory fell with it: one column per worker
instead of the whole strip held live purely to iterate afterwards.

`ColumnPayload::Column` is the fallback arm, and it is the pre-existing path
rather than a degenerate one: `chunk_encoder()` defaults to `None`, so every
legacy family and every test protocol in the workspace still encodes on the
caller's task, byte-for-byte as before. `encode_column` in `server.rs` is the
only place that branches on which arm a payload is on.

### Why this cannot change the wire

Emission order is fixed by `ColumnQueue` at **spawn** time, not by completion
order — `ColumnPipeline::next` awaits the front of its in-flight queue *by
reference* and pops only once the column is in hand. Moving the encode into the
worker therefore changes who runs it and never when the bytes go out. The
existing ordering gates (`join_scheduler_gates.rs`,
`the_shared_arm_streams_the_view_outward_too`) still pass unchanged, which is the
check that this held.

### The measurement

Wall clock reproduces to ~10.8% on this machine and several agents compile
concurrently, so the gate is a **counter**:
`serve_play.rs`'s `column_encode_never_runs_on_the_connection_task` counts
encodes that ran on a thread which has served an inbound packet.

| arm | encodes on the connection thread (view radius 9, 361 columns) |
|---|---|
| no `ChunkEncoder` (the defect, and the live control) | **361** |
| encode in the generating worker | **0** |

and at the moment a play packet sent before the first chunk was read got its
reply: **361 → 0**. The discriminator is a `thread_local` flag set by
`ServerProtocol::decode` itself, not a captured thread id — `decode` only runs on
the connection task, and `#[tokio::test]`'s current-thread runtime (the same
flavour `lodestone-shell/src/net.rs` builds) means that task cannot migrate while
the blocking pool is a disjoint set of threads.
`control_without_an_off_task_encoder_every_column_is_encoded_on_the_connection_task`
is the live negative control: not a neutered copy, but the shape a protocol
without an encoder still has, driven through the same `serve_connection` body.

## How to change it

- **Adding the encoder to another family**: implement `ChunkEncoder` on the
  `ServerProtocol` type (it must be `Send + Sync + 'static`; `V770ServerProtocol`
  is a unit struct, so `Arc::new(*self)` costs one allocation per join), make
  `ServerProtocol::encode_chunk` delegate to it, and return `Some` from
  `chunk_encoder()`. **Do not write a second encode body** — the contract is
  byte-identical output, and one body is the only way to guarantee it.
- **A stateful protocol** would need the encoder to own `Arc` clones of whatever
  it reads. Nothing in the seam requires the encoder to *be* the protocol.
- **wasm32** has no blocking pool, so `ColumnPipeline::next` there deliberately
  returns `ColumnPayload::Column` even with an encoder attached: there is no
  worker to move the work to, and claiming otherwise would be a lie about where
  it ran.
- **Gotcha**: `ColumnPipeline::next` is a `select!` branch in `serve_play` and
  must stay cancel-safe. It awaits the front `JoinHandle` by reference and pops
  only after the payload is in hand; popping first drops the handle on
  cancellation and silently loses a column from the wire.
- **Gotcha**: a gate that reads the column out of a pipeline (there is one, in
  `join_parallel_efficiency.rs`, which `black_box`es `solid_count()` so the
  generation is not optimised away) must build the pipeline *without* an encoder,
  and should `expect` rather than skip — a silent skip would delete the work it
  is timing.

## Configuration

None. There is no flag: a protocol either offers an encoder or does not, and the
in-flight window is still `join_scheduler::generation_window()`
(`available_parallelism`, floored at 2).

## Dependencies

`tokio`'s blocking pool (`spawn_blocking`), `crate::join_scheduler`,
`crate::chunk::generate_columns_parallel`, and one `ServerProtocol` implementor
per family that wants the offload.
