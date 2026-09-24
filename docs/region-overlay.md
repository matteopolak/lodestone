# Region decoration overlay

## What it is

`lodestone_worldgen::feature::region_view::RegionView` provides an overlay-first
read/write surface over the source chunk grids used by decoration. Its scratch
overlay is a bounded, lazily allocated page directory with packed local
coordinates and generation-stamped cells, recycled only through the current
thread's free list.

## How it works

`RegionView` checks its overlay before consulting the source grid, so a later
feature sees a block written by an earlier feature in the same pass. Normal
views use the 3×3 local window `[-16, 32)`; the wide read context uses
`[-32, 48)`. The vegetation grid's default overlay envelope is `[-32, 48)`
and `[-64, 384)` vertically, which covers the current decoration footprints.
Constructors pass the actual vertical range to the overlay, so cells outside a
view's read/write range remain misses.

The coordinate offset is packed into a `u32` using 10 bits each for local X and
Z and 12 bits for Y. Four-by-four-by-sixteen cells form a page. The page slot
directory is bounded by the configured window and pages are allocated only for
written cells. Each cell stores the current generation beside its state id;
reusing a scratch buffer advances the generation instead of clearing every
page. A stale physical cell therefore cannot become a logical hit after a
view is dropped, even when a new view rebases its local origin onto the same
packed slot.

`set_id` updates the overlay and appends the key to the insertion-order write
log. Repeated writes update the stamped cell but retain one overlay key and
retain every write-log event. `seed_read_id` updates only the overlay, so a
seeded source value shadows reads without entering the write log. Overlay
iteration is used only by consumers that sort the full `(x, z, y)` key before
folding values back into a palette; page allocation order is never served.

The ignored release test
`feature::region_view::tests::benchmark_direct_overlay_against_fast_map`
executes a warmed mixed read/write stream against the former
`FastMap<(i32, i32, i32), StateId>` shape and prints both per-round and
per-operation timings. Run it as:

```text
cargo test -p lodestone-worldgen \
  feature::region_view::tests::benchmark_direct_overlay_against_fast_map \
  --lib --release -- --ignored --nocapture
```

The test is diagnostic rather than a threshold gate; compare runs on the same
quiet host and keep the workload and trial count fixed.

## How to change it

The implementation and its controls live together in the `scratch` module in
`crates/lodestone-worldgen/src/feature/region_view.rs`. Keep the page dimensions
and bit widths consistent with `pack_unchecked`, `unpack`, and `location`.
Any new constructor must pass bounds that contain every key it can seed or
write. Use the `*_in_bounds` accessors only after the caller has established
that contract; the checked accessors are the safe boundary for tests and new
callers.

The tests cover overlay-first reads, seam routing, write-log order, seeded-read
exclusion, stale generation reuse, rebased packed-key aliasing, a fixed
content digest against a FastMap reference, and full-key sorting. Extend those
controls before changing the page layout or the write-log contract.

## Configuration

There are no runtime flags or environment variables. `KEEP` bounds the number
of recycled overlay and write-log buffers per thread. The coordinate windows,
page dimensions, and packed bit widths are compile-time constants in
`region_view::scratch`.

## Dependencies

The overlay stores the worldgen crate's `StateId` values and is consumed by
`RegionView` and the vegetation grid. Source reads continue to use
`DenseBlockGrid`; no source grid or overlay storage is shared between threads.
The parity test uses `FastMap` and `sha2` only as development-time comparison
tools; production overlay reads do not depend on a hash map.
