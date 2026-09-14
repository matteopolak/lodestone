# World-generation allocation gates

## What it is

The focused vegetation allocation tests separate container recycling from
allocations made by the placement path. They are diagnostic gates: a warm pass
must not grow the pooled scratch containers, while the total allocation gate
remains strict until every placement-side allocation is removed.

## How it works

`vegetation_allocs.rs` uses a counting global allocator for a cold pass, a
warm pass, four warm scene sizes, and a drained-free-list control. It also reads
`region_view::scratch_misses` and `scratch_free_list_lengths` on the same test
thread. Source-less fixtures intentionally own two overlays (`blocks` and the
seeded baseline) and one write log, so a dropped fixture returns `(2, 1)` to
the scratch free-list. The control drains those buffers and observes three
container misses on the next fixture, while the recycled arm observes zero.

Vegetation writes resolve borrowed configured states directly. Synthesized finite
states are formatted into a thread-local buffer and committed by interned ID, so
steady-state placement does not allocate a temporary string per write.

The leaf-distance worklist clears its queues in place and gives each collision
bucket a small initial capacity when it is first built. This keeps later tree
shapes from reallocating scratch storage while retaining the same bucket order.

Ordinary production columns retain a bounded immutable replay-context cache. Its
key includes the generator seed, generator configuration identity, and target
chunk. The cache holds at most 128 contexts and 16 MiB of estimated owned
storage; eviction is LRU and never changes output, only whether a context is
rebuilt. Cache hits, misses, evictions, entry count, and retained bytes are
reported by the generation benchmark.

`vegetation_column_allocs.rs` covers served production columns. It gates only
scratch misses, not the total allocation count, because returned column output
buffers are an allowed constant-size cost. Run both binaries in release mode
with `gen-counters`; the column test is ignored by default.

## How to change it

Keep allocation windows free of fixture setup, census resets, and result
construction. Reset the scratch-miss counter before constructing the medium
whose lifecycle is under test; resetting after construction hides misses.
When a fixture gains another owned medium, update the expected free-list shape
from the constructor ownership rather than loosening an allocation bound.
Preserve output and write-count equality controls when changing the placement
path.

## Configuration

The gates require `--release --features gen-counters`. The served-column gate
also requires `-- --ignored --nocapture`; it uses the embedded generator data
and seed 42. The scratch free-list is thread-local, so test-order or
cross-thread observations are not valid substitutes for a same-thread control.
The replay-context limits are compile-time constants in the overworld generator;
the benchmark records their observed usage but does not alter them.

## Dependencies

The tests depend on `lodestone-worldgen` vegetation placement, the
`feature::region_view` scratch instrumentation, the state interner, and the
embedded world-generation data supplied by `lodestone-server`.

## Diagnostic interpretation

The strict total-allocation gate covers placement-side allocations as well as
container growth. A zero scratch-miss result therefore does not imply a zero
allocation result: returned column buffers are an allowed constant-size cost,
and placement code must still avoid rebuilding state objects or other temporary
values. The output and write-count controls are the authority when optimizing
either path.
