# World-generation memory traffic counters

## What it is

The `gen-counters` instrumentation reports bounded, aggregate software-cache and representation traffic for world generation. It makes cache hits, misses, recomputation, logical payload reads/writes, scratch-pool reuse, and retained scratch-buffer bytes visible without allocating an event record in a hot loop.

## How it works

`lodestone_worldgen_core::counters::snapshot` exposes fixed arrays indexed by `CACHE_NAMES` and `MEMORY_BOUNDARY_NAMES`. The cache arrays cover the cell, slot, and leaf memo stores. The representation arrays count attempted logical lookups and payload bytes at the block-field, cell-cache, slot-cache, leaf-memo, and dense block-grid boundaries; a cache miss counts one lookup but zero returned payload bytes.

Cache computation counters are bumped at the existing miss branches. The direct-mapped leaf memo reports displacement as an eviction; the dense and hashed stores do not evict entries. There is no wait counter because these core caches are thread-local and have no blocking miss path. `NoiseChunkSampler` queries count as block-field reads. The dense materialization and height-map loops report complete-column scans or conversions with their logical cell counts.

Bounded scratch grids precompute bit shifts when their X/Z and Y lattice steps are powers of two. Dense cache indexing then shifts the non-negative coordinate deltas instead of performing three integer divisions; unusual geometries retain the general Euclidean-division path. The sampler's declared bounds are the contract that makes those deltas non-negative.

Scratch acquisition distinguishes a free-list reuse from a fresh scratch instance. Dense vector capacity growth contributes to cumulative logical allocation bytes and retained bytes; the retained high-water mark is updated with a relaxed atomic. Hash-table allocator metadata is intentionally excluded because its implementation capacity is not a stable payload-size contract.

The Overworld fill walk derives each solid-top height while producing the
density field, so the surface input does not trigger a second top-down read of
the full column. In the seed-42 embedded census this reduced complete scans
from 75 to 50 and scanned cells from 5,747,387 to 4,103,876, with the content
digest unchanged.

The production pre-ore path carries fill results as packed `u16` kind codes.
Surface replacements use a disjoint tagged range in the same field, so the
original fill class needs no parallel byte array and there is no intermediate
whole-column kind-to-state conversion. Materialisation decodes each code in
z, x, y order and rewrites its cell to a dense palette index in place, while
surface and vein precedence remains inside the same ordered callback. This
removes the second full-column block carrier that the former `Vec<BlockKind>`
to `DenseBlockGrid` handoff required;
the legacy `fill_stage` adapter still decodes packed output for shape/parity
callers that explicitly request the enum field.

Both ordered materialisation constructors keep a direct table from canonical
state id to local palette index. The table is populated lazily, so first-seen
palette order remains unchanged while repeated cells avoid a hash probe.

The production pre-ore path also keeps a request-local 384-bit ocean-floor
occupancy set for each centre XZ column. Materialisation sets the baseline bits,
and carver or structure writes update only the affected bit after comparing the
old and new predicates. Final heights read the highest set bit in the six words
for touched columns, so exact post-mutation results no longer probe the dense
grid vertically. The fixed 12 KiB sidecar is discarded with the prefix; it is
not shared across chunks or retained in a generator cache. Legacy test adapters
without the sidecar retain the scalar recount control.

The cell fill's vertical aquifer path now carries the already-evaluated global
fluid status into its branch and block conversion. Non-positive densities no
longer evaluate that same Y-only picker twice; positive densities retain their
short-circuit path and all aquifer status, barrier, and RNG work is unchanged.

These are software representation counters, not CPU cache-level measurements. They cannot determine whether a load came from L1, L2, L3, or DRAM, nor can they count physical memory traffic. Use the platform hardware-counter workflow (for example, Instruments or `perf`) for those questions and correlate it with a counter-enabled run rather than treating the two measurements as interchangeable.

The ignored single-worker production cohort benchmark also records source-state and shared-height reads by absolute chunk when `gen-counters` and `LODESTONE_WORLDGEN_BENCH_COUNTERS=1` are enabled. Its fixed-size owner table records distinct 16×16 horizontal lanes and, for state reads, distinct 4×8×4 cells. Overlay hits are excluded from source-state reads. The table reports overflow rather than silently dropping evidence; counter-enabled instruction and timing values are not comparable to an uninstrumented run.

For the seed-42 square cohort of 64 Full Overworld outputs, 144 immutable prefixes were computed for 64 requested chunks, 36 feature-owner padding chunks, and 44 read-only context chunks. The read-only ring received 29,937 state accesses across 3,006 of its 11,264 horizontal lanes and 1,359 of its 33,792 coarse cells. Its shared height products received 63,388 accesses across 5,604 lanes. This shows sparse state access but substantial height demand; it does not establish that a lazy outer ring would save enough work to justify replacing full-prefix generation.

## How to change it

Add a field to `Snapshot`, its feature-enabled atomic storage, `Default`, `reset`, and `snapshot`; add a feature-off no-op; then place the hook at the existing representation boundary. Use fixed arrays and aggregate increments. Keep payload-byte semantics separate from hardware cache-line traffic, and add a predictive counter test with a negative control that exercises the hook.

Column scans and conversions must be marked by the owning pipeline boundary because an arbitrary sequence of sampler point queries is not necessarily a complete-column operation. Do not infer CPU cache levels from these counters.

## Configuration

The `gen-counters` feature is disabled by default and is forwarded by `lodestone-worldgen`. With it disabled, hooks inline to no-ops and `snapshot()` returns zero-valued counters. Measurements should call `reset()`, perform the bounded work, and then call `snapshot()`.

`cargo bench -p lodestone-worldgen --features gen-counters --bench generation` prints the cache, representation, scratch, scan, and conversion totals beside the existing stage counters. Counter-enabled timings are diagnostic only; use the same benchmark without `gen-counters` for performance comparisons.

## Embedded counter census

The ignored `embedded_counter_census` integration test runs one full embedded
Overworld column without Criterion or LTO and prints cache hits, misses,
computations and evictions; logical reads, writes and bytes; scratch reuse,
allocation, eviction and retained high-water marks; full scans and
conversions; plus the existing recomputation and stage counters. It runs a
second seed as an input control and requires both a changed block digest and
live terrain/materialization counters.

Run the focused diagnostic with:

```bash
CARGO_BUILD_JOBS=2 cargo test -p lodestone-worldgen --features gen-counters \
  --test embedded_counter_census -- --ignored --nocapture
```

The counter feature adds relaxed atomics to hot paths, so use this report to
understand work composition, not as a production throughput measurement.

The leaf memo is intentionally kept at 64 direct-mapped entries. On the same
embedded seed-42 column, the baseline produced 155,805 hits, 670,958 misses,
and 667,409 displacements. A controlled 256-entry build produced the identical
hit and miss totals and only reduced displacements to 667,240. The seed-43
control likewise kept its hit and miss totals unchanged (157,063 and 787,066)
while displacements moved from 783,600 to 783,335. The larger table therefore
does not remove leaf evaluation work; it only adds retained bytes to every
scratch, so it is not a useful optimization.

The one-entry negative control confirms that the existing memo is doing useful
work: seed 42 fell to 56,581 hits and rose to 770,182 misses, while seed 43 fell
to 57,479 hits and rose to 886,650 misses. The 64-entry table therefore removes
about 100,000 leaf computations per embedded column without widening the
scratch footprint.

## Dependencies

The counters depend only on the standard library and are linked into `lodestone-worldgen-core`, which is re-exported by `lodestone-worldgen`. The census additionally uses the embedded production server resolver and the dev-only `sha2` dependency. Hardware-level measurements require an external OS profiler and its permissions; no profiler is a runtime dependency of world generation.
