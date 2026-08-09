# Steady-state view streaming and the connection-loop stall watchdog

## What it is

The server-side path that sends a walking player their newly-visible chunk
columns, and the watchdog that measures how long `serve_play`'s `select!` loop
goes without servicing the socket. Together they close the second half of the
*"chunk gen is too slow, and if I move around I eventually get Timed Out"*
report: the join burst was fixed by
[`join-scheduler.md`](join-scheduler.md), the **move** kept the identical
defect, and the visible symptom of it was a keep-alive disconnect the server
issued to itself.

## How it works

### The defect, precisely

`serve_play` is one `tokio::select!` in a loop. Exactly one arm runs per pass,
so for the whole duration of that arm the connection reads nothing and writes
nothing — the socket is unserviced even though the task is alive and the runtime
is healthy.

`ViewTracker::recenter` used to be `async` and used to produce finished packet
bytes. On every chunk boundary the player crossed it awaited
`generate_and_encode_columns_offloaded` over the entire newly-visible strip —
`2r + 1` columns, 33 at `view_radius = 16`, and a whole `(2r + 1)²` square on a
jump — before returning a single directive. Offloading to the blocking pool had
moved the *work* off the core thread but left **one suspension point covering
all of it**, so the connection task was parked for the full strip.

Three consequences, and the third is the one the owner saw:

1. no packet the player sent during that window was serviced (dig, hurt,
   container click);
2. no keep-alive challenge could be *written*;
3. no keep-alive **reply** could be *read*, so `pending_keep_alive` stayed
   `Some` across the window and the next `keep_alive_tick` disconnected a client
   that had answered promptly.

### The fix: one streaming path for join and move

`ViewTracker::recenter` and `ViewTracker::set_view_radius` are now synchronous
and return **coordinates**, not directives (`ViewUpdate::added`). The connection
task's cost for a boundary crossing is one `HashSet` difference and a sort.

`send_view_update` then decides where those coordinates go:

| condition | path |
|---|---|
| `JoinChunkStream::accepts_enqueue()` — the `Windowed` arm, i.e. every production caller | `ColumnPipeline::enqueue`; `serve_play`'s existing `select!` branch drains it one column per pass |
| `Ringed` (a borrowed, non-`'static` source — protocol tests) or no stream at all (`wasm32`) | generate + encode inline under `awaiting_chunk_batch_ack`, the unchanged pre-fix path |

So a move now behaves exactly like a join: the same primed sliding window, the
same nearest-first key (`join_scheduler::view_order_key`), and
`ColumnPipeline::reprioritise` re-keys the not-yet-started columns as the player
keeps walking — which a per-move fan-out could not do, because it had already
committed to its whole batch.

Two supporting changes made that possible:

* `JoinChunkStream::windowed` no longer collapses an empty pipeline to
  `Drained`, and neither does `next`. `Drained` carries no source, window or
  encoder, so a stream that resolved to it could never be re-fed. `is_done()`
  already reports emptiness from `remaining()`, so the `select!` branch is
  disabled either way and nothing spins.
* `ColumnQueue::extend` appends to the **back** of the pop order and then
  re-keys, so arrival order never displaces priority.

### The encode inside the offload was serial

`generate_and_encode_columns_offloaded` fanned generation out over scoped
threads, joined them all, and *then* walked the joined `Vec` calling
`encode_chunk` one column at a time on a single thread. At the ≈2.4 ms per
column `ChunkEncoder` carries, a 33-column strip paid ≈80 ms of unavoidably
single-threaded work no matter how many cores generated it.

`chunk::map_columns_parallel` is the fix: a per-column transform applied inside
the worker that generated the column. `generate_columns_parallel` is now a thin
wrapper over it with an identity transform, and the encoding variant passes
`encoder.encode_chunk`. Peak memory drops with it — one column per worker
instead of the whole strip held live purely to iterate afterwards.

### The watchdog, and what it is denominated in

`LoopStallWatch` times **arm bodies**, not the interval between passes. Each
arm body calls `watch.enter()` first and `watch.pass("<arm name>")` last, and it
keeps three things: the worst body seen with the arm that owned it, the sum of
every body past `STALL_FLOOR` (one server tick), and a `tracing::warn!` at
`STALL_REPORT` (200 ms) naming the arm as it happens.

**Timing pass-to-pass instead was tried and is wrong.** The interval between two
passes is mostly time parked in `select!` waiting for a timer or a packet — the
loop is idle there, not stalled, and the socket is being serviced by definition.
Under a `start_paused` runtime, where the clock jumps straight to the next timer
deadline whenever nothing is runnable, that measurement reported the whole
keep-alive interval as a stall and suppressed the very timeout
`silent_client_is_disconnected_after_keep_alive_timeout` gates. The test caught
it; nothing else would have.

The keep-alive arm reads `unserviced` and forgives an unanswered challenge
**only when the accounting shows a full `KEEP_ALIVE_INTERVAL` of stall inside
that window**. Vanilla gets this accounting for free: its
`keepConnectionAlive` runs on the server tick while its reads happen on a Netty
IO thread that never blocks on world generation, so "15 seconds elapsed" and
"15 seconds in which the client could have been heard" are the same number
there. Here they are not.

`keep_alive_tick` also switched from tokio's default
`MissedTickBehavior::Burst` to `Delay`. `Burst` makes up for missed ticks by
firing them back to back with **no delay in between**, so a pass that overran
two intervals resolved `tick()` twice in immediate succession: the first wrote a
challenge, the second found it unanswered in the same instant. The client was
given literally zero time to reply.

## How to change it

* **Adding a `select!` arm to `serve_play` means adding `watch.enter()` as its
  first statement and `watch.pass("name")` as its last.** Neither is enforced by
  the compiler. A missing `enter` makes the arm invisible to the watchdog (its
  `pass` is a silent no-op, by design, so an early `return` is not charged
  someone else's time); a missing `pass` leaves the timer open and the *next*
  arm's `enter` overwrites it.
* **Do not lend the stream to a loop that does not drain it.**
  `dispatch_play_packet` takes `Option<&mut JoinChunkStream<S>>` and the
  `wasm32` loop passes `None` for exactly this reason: it drains its join inline
  and then never looks at the stream again, so columns enqueued there would
  reach no wire — an island with a green test suite.
* **Ask `accepts_enqueue` before handing coordinates over.** `enqueue` takes
  ownership, so a refusal discovered afterwards leaves the fallback with nothing
  to send. This was a live bug for one iteration and five tests caught it.
* **A view update through the stream is not subject to
  `awaiting_chunk_batch_ack`**, because the join stream it shares is not:
  batches there are paced by `JOIN_STREAM_BATCH_COLUMNS` and the client's own
  `chunk_batch_received` rate estimate. Two flow-control regimes over one
  ordered queue is how the two paths drift.
* **Tests that read one batch after a slider move or a step now need to drain
  several.** A strip streams as a run of `JOIN_STREAM_BATCH_COLUMNS`-sized
  batches rather than one batch holding the lot. `view_radius_store_capacity.rs`
  uses `drain_join_view(client, expected)` for this; a single `drain_one_batch`
  counted 16 and failed a precondition while nothing was actually missing.

## Configuration

| knob | where | value |
|---|---|---|
| `STALL_FLOOR` | `server.rs` | one server tick (50 ms) — below this a pass is not a stall |
| `STALL_REPORT` | `server.rs` | 200 ms — logs immediately, naming the arm |
| `KEEP_ALIVE_INTERVAL` | `server.rs` | 15,000 ms, vanilla's own; also the forgiveness threshold |
| `JOIN_STREAM_BATCH_COLUMNS` | `server.rs` | 16 columns per batch marker |
| generation window | `join_scheduler::generation_window` | derived from `available_parallelism`, never from the view |

The stall logs go to the `lodestone_server::stall` tracing target, so they can be
raised or silenced without touching the loop.

## Dependencies

* `crate::join_scheduler` — `ColumnPipeline`, `ColumnQueue`, `JoinChunkStream`,
  `view_order_key`. See [`join-scheduler.md`](join-scheduler.md).
* `crate::chunk` — `map_columns_parallel` and the two offload wrappers over it.
* `crate::protocol::ChunkEncoder` — the off-task encode seam. See
  [`server-chunk-encode-offload.md`](server-chunk-encode-offload.md).
* `tokio::time` — `interval_at`, `MissedTickBehavior`, `Instant`. The whole
  watchdog and both timers are `cfg(not(target_arch = "wasm32"))`, matching
  `serve_play`'s existing native/wasm fork.

## The gates

| gate | file | what it would catch |
|---|---|---|
| `a_play_packet_is_serviced_before_the_last_chunk_of_a_move` | `tests/serve_play.rs` | the strip being generated inline again |
| `a_play_packet_is_serviced_before_the_last_join_chunk` | `tests/serve_play.rs` | the join burst blocking the play loop |
| `a_move_streams_the_new_columns_nearest_first` | `tests/serve_play.rs` | the wire order regressing to lexicographic |
| `silent_client_is_disconnected_after_keep_alive_timeout` | `tests/serve_play.rs` | forgiveness that never stops forgiving |
| `responsive_client_survives_multiple_keep_alive_intervals` | `tests/serve_play.rs` | the reverse |

The move gate's counter is the ordering, not a duration, because this machine's
wall clock reproduces to ~10.8%. The two hypotheses at `view_radius = 9` are
**361** columns before the reply (inline) and a handful (streamed); the bound is
40. Its negative control was run by pointing `dispatch_play_packet`'s stream
parameter at `None`, and it failed with *"the difficulty change was never
answered"* — the strongest form of hypothesis 1, while the join gate beside it
still passed, which is what makes the join gate demonstrably blind to this path.
