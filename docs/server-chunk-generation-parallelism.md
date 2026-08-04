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
or when, cannot change what it contains. `ChunkSource: Send + Sync`
(`chunk.rs:216`) is the compiler-checked half of that — it's what lets
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
  should call directly.
- Do not touch `lodestone-worldgen` to "help" this — the RNG-determinism
  property this relies on already lives there, in the generator's own
  positional seeding, and it needed no change for this to be safe.

### Watch, don't pre-optimise

Two contention points a prior review flagged, both currently harmless:

- The shared climate `Density` trees hit `Cache2DSlot(Mutex<Option<(x,z,f64)>>)`
  (`lodestone-worldgen`'s `density/mod.rs:140`) from every worker thread. At 16
  climate samples/chunk today that's negligible; if biomes ever go 3-D
  (1536 samples/chunk), the single-entry cache will thrash under concurrent
  access. Not worth restructuring ahead of that.
- `OverworldGenerator::column` deep-clones the full `final_density` box-tree
  per chunk (`overworld.rs:239`) — allocation churn per chunk per thread. An
  `Arc`-per-worker restructure is only worth it if a profile actually shows
  this as hot; it wasn't measured to be so here.

`OverworldChunkSource::edits` (`chunk.rs:289`, a `Mutex<HashMap>`) is held only
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
- **Speedup**: recorded into the gitignored `bench-results/generation.jsonl` by
  `examples/bench_worldgen.rs` (`parallel_speedup_vs_serial`, unit `x`) — run it
  yourself with `cargo run --release -p lodestone-server --example
  bench_worldgen`. Per `CLAUDE.md`'s evidence standard this repo measures ratios
  on a shared, variably-loaded machine, never an absolute-ms threshold; treat
  any single number here as a spot check, not a regression gate.

## Dependencies

- `lodestone_worldgen::overworld::OverworldGenerator` — the actual generator
  being parallelized; its purity-per-chunk and positional RNG seeding are load
  -bearing preconditions this file does not re-derive, only relies on.
- `std::thread::scope` (std, no external crate) — the same mechanism
  `examples/bench_worldgen.rs` already used for its own parallel measurement.
