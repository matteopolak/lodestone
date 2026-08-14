# Server chunk generation: fanned out over scoped threads (issue #414)

## What it is

`crates/lodestone-server/src/chunk.rs`'s `generate_columns_parallel` runs a batch
of `ChunkSource::column()` calls across `std::thread::scope` worker threads and
hands the results back in the same order the coordinates were given. It replaces
the serial, one-column-at-a-time loop the integrated server used to run for both
places a connection asks for a burst of terrain: `serve_connection`'s initial
`view_radius` join, and `ViewTracker::recenter`'s "chunks that entered the view"
diff (both in `crates/lodestone-server/src/server.rs`).

## How it works

**The generator was already safe to parallelize; only the consumer was serial.**
`OverworldGenerator::column` is pure per chunk — every piece of per-chunk mutable
state (`NoiseChunkSampler`'s caches, `AquiferSystem`'s) is built inside the call,
and every RNG a generator touches is positionally seeded
(`set_decoration_seed`/`set_feature_seed`/`setLargeFeatureSeed` per source chunk,
`fork_positional`/`from_hash_of` — see `lodestone-worldgen`'s own doc comments).
There is no shared RNG stream anywhere in that crate, so results are
order-independent by construction: which OS thread generates column `(3, -2)`,
or when, cannot change what it contains. `ChunkSource: Send + Sync`'s trait bound
(`chunk.rs`) is the compiler-checked half of that — it's what lets
`generate_columns_parallel` share `&S` across worker threads at all — and
`examples/bench_worldgen.rs` had already been sharing a generator across
`std::thread::scope` workers for a while before anything in the server used it.

`generate_columns_parallel(source, coords)`:

1. Splits `coords` into `available_parallelism()` contiguous chunks.
2. Spawns one scoped thread per chunk, each calling `source.column()` in a plain
   loop over its slice.
3. Joins every thread and concatenates the per-thread `Vec<ChunkColumn>` results
   back together — because the split was contiguous and joins happen in the same
   order threads were spawned, the returned `Vec` is aligned index-for-index with
   the input `coords`, regardless of which thread actually finished first.

Callers still walk that returned `Vec` in the fixed input order to encode and
send — the parallelism is confined to *generation*; the wire is written
sequentially, in an order fixed before generation starts. Two call sites, two
different coordinate-ordering stories:

- **`serve_connection`'s initial burst**: coordinates are the same
  `cz`-outer/`cx`-inner walk the loop always used, collected into a `Vec` up
  front. Deterministic by construction — no set involved.
- **`ViewTracker::recenter`'s `added` list**: this one previously came straight
  out of `HashSet::difference(...).collect()`, whose iteration order already
  wasn't stable run-to-run (`RandomState` reseeds per process) even before this
  change. Parallel generation would have added a *second*, independent source of
  order variance on top of that. Fixed by sorting `added` before generating —
  see the comment at the call site in `server.rs`.

## Parallel is not the same as non-blocking

`generate_columns_parallel` closed the **throughput** axis and nothing else. Its
final `std::thread::scope` join blocks the calling thread until every worker
finishes, and the shell builds the server's runtime with
`tokio::runtime::Builder::new_current_thread()`
(`crates/lodestone-shell/src/net.rs`) — so the connection task and
`tick::run_tick_loop` share **one** thread. Blocking it blocked every task in the
process, and every chunk-boundary crossing in singleplayer dropped one or more
50 ms world ticks.

`chunk.rs`'s `generate_columns_offloaded` closes the **latency** axis by wrapping
the same fan-out in `tokio::task::spawn_blocking`.

**Do not "simplify" it to `tokio::task::block_in_place`.** That needs no
signature change and looks strictly cheaper. It **panics** on a current-thread
runtime, which is the runtime production builds — so it would panic in
singleplayer, not merely fail a test. Measured directly on a
`new_current_thread` runtime rather than read off the docs:

| call | result |
|---|---|
| `block_in_place` | panics: `can call blocking only when running on the multi-threaded runtime` |
| `spawn_blocking` | `Ok` |
| 10 ms timer ticks during a 300 ms `spawn_blocking` | **25** |
| 10 ms timer ticks during a 300 ms inline block | **0** |

`spawn_blocking` works on a current-thread runtime because the blocking pool is a
separate set of threads from the core thread, and it stays correct on a
multi-thread runtime — so a future thread split cannot invalidate
it.

### `SourceRef`, and why the public API did not change

`spawn_blocking` requires a `'static` closure, so the source cannot be borrowed
across it. The obvious consequence is changing `serve_connection`'s `source: &S`
to `Arc<S>` — which would break every off-limits `crates/protocol/v770/tests/*`
call site, the same constraint that already produced three differently-named
`serve_connection*` wrappers in `server.rs`.

`server.rs`'s `SourceRef<'a, S>` avoids that. It is a two-variant enum threaded
through the private dispatch chain, so both shapes share one body:

| arm | generation | used by |
|---|---|---|
| `Shared(&'a Arc<S>)` | offloaded, never blocks the runtime | every production caller in `integrated.rs` |
| `Borrowed(&'a S)` | blocking, the original behaviour | `&S`-shaped test call sites |

Two consequences worth keeping:

- **`mod server` is private and `lib.rs` re-exports only `serve_connection`**, so
  the new `serve_connection_shared` / `serve_connection_with_mob_events_shared`
  entry points are `pub(crate)` and this change cost **no public API change and no
  `lib.rs` patch at all**.
- **The `Borrowed` arm is the permanent negative control.** It is not dead
  weight: `chunk.rs`'s `offloaded_generation_lets_a_timer_task_keep_running`
  drives the blocking path as its second arm and requires it to starve the timer
  completely. Delete the arm and the gate stops distinguishing anything.

## How to change it

- The fan-out lives entirely in `chunk.rs`'s `generate_columns_parallel`; both
  call sites in `server.rs` just build a coordinate list, call it, then zip the
  result back against the coordinates for encoding. Do not call
  `source.column()` directly in a loop at a new call site without going through
  this function, or you've reintroduced the serial path it replaced.
- **The ordering discipline is the part that must not regress.** Any new caller
  must fix its coordinate order *before* calling `generate_columns_parallel` and
  must encode/send using that same fixed order afterward — never the order
  results happen to arrive in, and never a `HashSet`'s iteration order. The
  determinism test below exists to catch a caller that gets this wrong.
- `generate_columns_parallel` is `pub(crate)` — it's an internal detail of how
  this crate serves chunks, not a public API `lodestone-server`'s consumers
  should call directly. Same for `generate_columns_offloaded`.
- **New batch call sites should reach generation through `SourceRef::generate`,
  not either function directly.** That is what keeps a caller on the offloaded
  path automatically instead of depending on whoever wired it remembering to.
- Do not touch `lodestone-worldgen` to "help" this — the RNG-determinism
  property this relies on already lives there, in the generator's own
  positional seeding, and it needed no change for this to be safe.

### The ceiling is the machine, not a lock — measured, §12.132

Both contention points a prior review flagged here have since been resolved, and
neither was what limited throughput:

- The `Cache2DSlot(Mutex<Option<(x,z,f64)>>)` in the shared `Density` trees **is
  gone**. It was measured at a 0.12% hit rate over a 289-column burst and deleted;
  removing it moved the burst's cycles-per-column ratio from 4.19× to 4.35×, i.e.
  not at all. See `worldgen-density-engine.md`.
- `OverworldGenerator::column`'s per-chunk deep clone of `final_density` was
  replaced by an `Arc<Graph>` in U4 (`worldgen-density-engine.md`).

What actually limits the burst, from `crates/lodestone-server/tests/join_parallel_efficiency.rs`
on the 10-core reference machine (6 P-cores + 4 E-cores), 289 columns, fresh
generator per arm:

| window | wall | speedup | cycles/col vs serial | IPC | pool parked |
|---|---|---|---|---|---|
| serial | 9.55 s | 1.00× | 1.00× | 5.33 | — |
| 4 | 4.78 s | 2.00× | 1.01× | 5.27 | 32% |
| 8 | 3.67 s | **2.60×** | 1.26× | 4.25 | 32% |
| 10 (`P`, current) | 4.28 s | 2.23× | 1.96× | 2.73 | 29% |
| 20 (`2P`, until §12.132) | 6.43 s | 1.49× | 4.39× | 1.23 | 24% |

**Instructions retired are flat to 1.4% across every arm**, so none of that is
redundant work — it is the same instructions taking more cycles.

It is **not a lock**, and the discriminator is the shape of the curve rather than a
bespoke control: a lock contended by 20 workers is contended by 4, so it would inflate
cycles at every window, whereas cache capacity costs nothing until the working sets stop
fitting. Cycle inflation at a window of 4 measured **1.01–1.15× over five runs** against
2.6–4.4× at 20, growing super-linearly in between — a threshold, not a constant tax.
`a_small_window_shows_no_lock_on_the_shared_generator` is that assertion, with the widest
window's own reading as its control. (Two purpose-built shared-vs-private-generator
controls were tried and both withdrawn; §12.132 records why, and the second is a good
example of a control whose premise was false in the safe-looking direction.)

So the mechanism is cache capacity: each in-flight column carries a multi-megabyte
working set, and past roughly the core count they stop fitting together.

Two consequences for anyone tuning this:

- **Do not raise the window to buy throughput.** The curve is a U with a steep
  right-hand side. `join_scheduler::generation_window` is now `available_parallelism`,
  not twice it.
- **The remaining headroom is the ~30% of pool capacity parked** on the staged
  store's per-entry `OnceLock` — a spatially contiguous window means adjacent columns
  want the same pre-ore entries, so `window - 1` workers can be waiting on the one
  that is computing. `store::wait_stats()` counts and times those parks; it reads
  exactly 0 single-threaded, which is its calibration.

`OverworldChunkSource::edits` (`chunk.rs`, a `Mutex<HashMap>`) is held only
for a lookup/insert per column and isn't a real bottleneck at the concurrency
levels here (one lock acquisition per column, `available_parallelism()`-wide
fan-out).

## Configuration

None — worker count is `std::thread::available_parallelism()`, not configurable.
No feature flag: this is always the code path, not an opt-in.

## Verification

- **Determinism control**: `chunk::tests::parallel_generation_is_deterministic_and_matches_serial`
  in `crates/lodestone-server/src/chunk.rs` generates a real, RNG-bearing 6-chunk
  patch (seed 42, a coordinate count that does not divide evenly across
  `available_parallelism()`, to expose an off-by-one batch-boundary bug if one
  existed) through `generate_columns_parallel` 8 times and asserts each repeat's
  byte-serialized content (`column_bytes`: `min_y`, `height`, palette, block
  grid, biome quarts) is identical to a serial baseline. Verified as non-vacuous
  by temporarily shuffling the coordinate order fed to the parallel path only —
  the assertion failed every time, confirming it actually detects a divergence
  rather than passing regardless.
- **Chunk count**: the same test asserts `parallel.len() == coords.len()` on
  every repeat — a fan-out that silently dropped a chunk (e.g. an off-by-one in
  the batch split) would fail here even if the content of every chunk it *did*
  return were correct.
- **Speedup**: use `crates/lodestone-server/tests/join_parallel_efficiency.rs`
  (`cargo test --release -p lodestone-server --test join_parallel_efficiency --
  --ignored --nocapture`), which builds a **fresh generator per arm** and compares
  instructions retired alongside wall clock.

  `examples/bench_worldgen.rs`'s `parallel_speedup_vs_serial` is **not** comparable
  to it and should not be quoted: it runs its parallel arm on the generator the
  serial arm just warmed, so the parallel arm answers ~83% of every column out of the
  store (§12.130) and is measuring a different, much cheaper program. That is why its
  standing 2.4–2.9× and this file's 2.60× are not the same quantity.

  Per `CLAUDE.md`'s evidence standard this repo measures ratios on a shared,
  variably-loaded machine, never an absolute-ms threshold — and prefers a counter to
  a duration, which is why instructions retired is the comparator and wall clock is
  the accompaniment.

## Dependencies

- `lodestone_worldgen::overworld::OverworldGenerator` — the actual generator
  being parallelized; its purity-per-chunk and positional RNG seeding are load
  -bearing preconditions this file does not re-derive, only relies on.
- `std::thread::scope` (std, no external crate) — the same mechanism
  `examples/bench_worldgen.rs` already used for its own parallel measurement.
