# In-place region decoration

## What it is

The read/write medium the worldgen decoration stages use to reach across a 3×3
chunk neighbourhood without copying it. Vanilla's `blockStateWriteRadius(1)` at
the FEATURES stage lets a feature placed in one chunk write into its neighbour, so
both the `UNDERGROUND_ORES` and `VEGETAL_DECORATION` drivers run all nine of a
column's source chunks against one shared block field. Until Unit 7 of
[`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md) that field was a
*materialised copy* of the neighbourhood, rebuilt on every served column;
[`RegionView`](../crates/lodestone-worldgen/src/feature/region_view.rs) and
`VegGrid::with_sources` route reads to whichever source chunk owns the column
instead, holding writes in a sparse overlay, which took the copy count from ~2.85M
cells per column to zero.

## How it works

Two types, one coordinate convention, one routing function.

**`feature/region_view.rs`** holds the shared routing and the ore driver's medium:

- `source_slot(lx, lz) -> Option<usize>` maps a **centre-relative local** column
  to one of nine source slots, or `None` outside the driven region. This is the
  only place the 3×3 routing is written.
- `RegionView<'a>` is `[Option<&'a DenseBlockGrid>; 9]` plus a
  `HashMap<(lx, y, lz), StateId>` overlay. A read consults the overlay, then the
  owning source grid at absolute coordinates, then answers air. A write goes to
  the overlay only.

**`feature/vegetation/grid.rs`**'s `VegGrid` is the absolute-coordinate adapter
over the same idea, with the census, the `dirty` write log and the incremental
heightmap scans vegetal decoration needs. Its `blocks` map used to be a full copy
of the neighbourhood and is now the overlay; `sources` is
`[Option<Arc<DenseBlockGrid>>; 9]`, routed through the same `source_slot`.

Why the two differ in how they hold a source: the ore driver's neighbours arrive
as `Arc<PreOreResult>` (a tuple), and an `Arc` cannot be projected onto one of its
fields without unstable API, so `RegionView` borrows. Vegetation's neighbours
arrive as `Arc<DenseBlockGrid>` straight from `post_ore_world`, so `VegGrid` holds
the `Arc` and needs no lifetime parameter — which keeps the lifetime out of every
signature in Unit 8's placement engine.

**Overlay-first is a parity requirement, not an optimisation.** Vanilla's
heightmaps update as decoration places blocks, so a tree placed earlier in a step
must be visible to a later `MOTION_BLOCKING`/`WORLD_SURFACE` probe in the same
step. Merging the writes after the pass would answer stale.

**Sources are read-only.** A neighbour's product is an `Arc` snapshot shared with
every other in-flight column that has the same neighbour, and the rewrite plan's
parallel model requires each chunk's grid to have exactly one writer — its own
serve task. That is why writes live in the overlay and the *caller* folds them
into the one grid it owns.

**What each stage still copies**, named so nobody has to re-derive it:

| cost | size | why it stays |
|---|---|---|
| one `Vec<u16>` clone of the centre's post-ore grid | 98,304 cells | the store's copy is shared; the served chunk must be private |
| `ore_stage`'s fold-back | the cells ore wrote | only the centre's own writes are served |
| `vegetation_stage`'s fold-back | the `dirty` list | same |
| the `OCEAN_FLOOR_WG` gather | 2,304 `i32`s | the driver reads it by *clamped* region-local key, so a view would have to reproduce the clamp — 0.26% of the volume that went away |

There is **no shared buffer pool**, and there must never be one: a pool behind a
lock would re-create the contention
[`worldgen-staged-store.md`](./worldgen-staged-store.md) exists to delete, and
`4307b59` is the measured scar. The `thread_local` free-list this section named as
the one acceptable form of reuse **now exists** — Unit 19 built it, and both media's
containers are recycled through it, which is what took a warm column's 30 residual
allocations to 0. See
[`worldgen-decoration-scratch-reuse.md`](./worldgen-decoration-scratch-reuse.md);
the constraint it had to satisfy is still per-thread-never-shared, unchanged.

## How to change it, and the gotchas

**The nine sources are decorated over a 3×3, but vegetation *reads* over a 5×5, and
that difference is load-bearing.** Every one of the nine can write into the centre, so
each source's pass has to be a function of that source alone or the chunks either side
of a seam compute different versions of the same tree and the served world keeps one
half. `VegGrid::sources` therefore has 25 slots (`region_view::wide_source_slot`), the
16 rim chunks carrying pre-ore terrain at no extra pipeline cost. The remaining
violation — the nine passes sharing one write overlay — is open and cannot be closed
without a JVM-parity trade. Read
[`worldgen-seam-consistency.md`](./worldgen-seam-consistency.md) before touching either
driver's neighbourhood or its overlay.

**Fold-back order decides the served palette.** A `DenseBlockGrid` appends to its
local palette in first-write order and that palette goes on the wire, so *the
order writes are replayed in is world-visible*. The two stages differ and are not
interchangeable:

- `ore_stage` folds in `(y, lz, lx)` **scan** order, because the full-box walk it
  replaced visited cells that way. Applying only the changed cells is
  byte-identical to that walk because an unchanged cell's state came out of the
  centre grid itself and so is already in its palette — therefore every state new
  to the palette sits at a written cell, and the new states' first-write sequence
  is unchanged. `RegionView::centre_writes_in_scan_order` sorts by the full key
  over unique `HashMap` keys, so the order is total and does not depend on
  iteration order (the `RandomState` trap `overworld/mod.rs`'s module doc records).
- `vegetation_stage` folds in **write** order, because the `dirty` `Vec` it
  replays always did.

`column_is_byte_identical_across_two_independently_constructed_generators` is what
notices if either changes.

**RNG order must not move.** Same drivers, same `dx`-outer/`dz`-inner source
order, same depth-first recursion, same draws. Only the container changed. A
batching or reordering "optimisation" of the decoration walk is instant desync.

**This is the coordinate space of the repo's worst worldgen defect.** `VegGrid`
once stored *and exposed* local coordinates while the placement engine handed it
absolute `BlockPos`es, so vegetation reached **zero blocks in every served chunk
with the unit suite green** — and the gate that caught it was later deleted while a
doc comment went on naming it. So:

- Local coordinates on `RegionView`, absolute on `VegGrid`, and nothing mixes the
  two. Do not add an absolute-coordinate method to `RegionView`.
- `source_slot` uses `div_euclid(16)`, not `/ 16`. Local coordinates are negative
  across the western/northern third (`REGION_MIN` is -16) and truncating division
  maps both `-1` and `-16` to offset `0`, routing the whole western neighbour into
  the centre.
- `RegionView::over_sources` and `VegGrid::with_sources` **fill their slots
  through `source_slot`**, so the fill convention cannot drift from the lookup
  convention. Do not index `sources` directly.
- A boundary-write control is permanent here. See below.

**A useful non-obvious fact about what the padding does.** Narrowing
`VegGrid::in_bounds_local` from the padded region to `0..16` — the historical bug's
own shape — changes **no served byte**. When chunk `E` is the centre, its western
neighbour's pass runs against `E`'s own grid origin, so that neighbour's spilled
canopy already lands at local `x = 0, 1`, and the fold-back drops everything
outside the centre anyway. `VEG_PADDING` therefore governs **intra-pass reads and
the census**, not what reaches the wire. A seam control aimed at that bound would
be premise-false.

**Fixtures and production resolve to the same read path, deliberately.** A parity
fixture is naturally one sparse map over the whole region rather than nine
per-chunk fields, so `RegionView::over_region_grid` points all nine slots at that
one grid with origin `(0, 0)`. Reads still go through `source_slot`, so the JVM
fixtures exercise the routing rather than a second implementation of it. Likewise
`VegGrid::seed_id` still writes into the overlay, so every vegetation fixture
works unchanged with no sources at all. Keep it that way — a fixture transport
that bypassed the routing would be the *world* species of vacuous test.

## Gates

| gate | where | what it holds |
|---|---|---|
| `stitch_cells == 0` | `crates/lodestone-server/tests/in_place_decoration_counters.rs` | the acceptance criterion, against a pre-U7 hypothesis of 8,847,360, with a detector control that bumps the counter by hand |
| `a_canopy_spans_the_chunk_seam_in_both_served_chunks` | `crates/lodestone-server/tests/decoration_seam_spill.rs` | the boundary-write control: a swamp canopy at the `(-9,18)|(-8,18)` seam present on both sides — 20 crossing rows, 24 orphan leaves with no trunk in their own chunk |
| `source_slot_routes_every_local_column_to_the_chunk_that_owns_it` | `region_view.rs` unit tests | routing, exhaustive over the padded region against a hand-written band table, with a negative control showing the truncating form misrouting |
| `a_view_answers_cell_for_cell_what_the_stitched_copy_answered` | same | the differential control: the deleted copy algorithm is rebuilt and compared over all 12 × 48 × 48 positions |
| `column_is_byte_identical_across_two_independently_constructed_generators` | `lodestone-server` lib | palette determinism across the fold-backs |
| `feature_parity`, `vegetation_parity` | `lodestone-worldgen` tests | the JVM anchors, now driving the view |

Both controls were observed **failing** before being trusted: with `source_slot`
using `/ 16`, the seam control drops from 20 crossings to 0 and 5 of the 9
`region_view` unit tests fail. The counter binary is its own binary because these
counters are process-global atomics — see
[`worldgen-staged-store.md`](./worldgen-staged-store.md) for the shared-binary
measurement that forced that rule.

## Configuration

None of its own. The region bound comes from `feature::REGION_MIN`/`REGION_MAX`,
the vegetation footprint adds `feature::VEG_PADDING` on each side, and the vertical
bound is the generator's `min_y`/`height`. Two debug-only escape hatches survive
from before: `LODESTONE_ORE_SINGLE_SOURCE_DEBUG` and
`LODESTONE_VEG_SINGLE_SOURCE_DEBUG` restrict decoration to the centre's own pass
(the latter by giving the grid no neighbour sources), matching the single-source
scope of `ComposedChunkOracle.java` and `VegetationOracle.java`. Neither is on
`column()`'s normal path. The `gen-counters` feature gates the counter the
acceptance criterion reads.

## Dependencies

`dense_grid::DenseBlockGrid` for the sources, `interner` for the `StateId`↔`&str`
shim the string-taking accessors still need, and
[`worldgen-staged-store.md`](./worldgen-staged-store.md)'s store for the `Arc`
snapshots the views borrow. Consumed by `overworld/decorate.rs`'s `ore_stage` and
`vegetation_stage`, and by `feature/mod.rs`'s ore engine. Unit 8's placement engine
(`feature/vegetation/{config,tree,place}.rs`) was **not** changed by this unit and
still sees `VegGrid`'s unchanged public API.
