# Cross-seam decoration consistency

## What it is

Why a tree that straddles a chunk border comes out whole rather than sliced flat
along it, and what still does not. The 3×3 decoration driver serves chunk `C` by
re-running all nine of `C ± 1`'s own decoration passes and keeping only what lands
in `C`, so a tree standing in `A` and spilling into `B` is **computed twice** — once
with `A` as the centre and once with `B`. Each drive supplies the half in its own
chunk. Nothing about the driver forces those two computations to agree, and when
they disagree the served world keeps one half and drops the other. That is the
"trees cut off at chunk borders" defect the owner reported in-game, and this doc is
the record of which of its two causes is closed and which is not.

## How it works

The invariant the architecture needs is narrow and easy to state:

> **Source chunk `S`'s decoration pass must be a function of `(world seed, S)` and
> of undecorated terrain — never of which column happens to be the centre.**

Vanilla does not need this invariant, because vanilla runs each chunk's FEATURES
stage exactly **once** and persists the spill into its neighbours. Confirmed by
symbol: `ChunkPyramid`'s `FEATURE_PYRAMID` registers `ChunkStatus.FEATURES` with
`blockStateWriteRadius(1)` and `ChunkStatusTasks.generateFeatures` as its task, so the
single `WorldGenRegion` handed to that task spans the chunk plus its radius-1
neighbours and a feature genuinely writes into them directly — there is no second
computation to disagree with. We recompute per centre instead of writing once and
persisting, so we do need the invariant above. Two things violated it. Both were
measured on a flat 5×5 synthetic
world across all 66 bundled biomes, counting *truncated seam rows* — rows where some
drive placed one canopy across the border and the stitched served field lost a half
(see [Gates](#gates)):

| arm | truncated rows |
|---|---|
| as shipped before this work | **94** |
| read neighbourhood widened to 5×5 (landed) | **44** |
| per-source write isolation only | 75 |
| both | **0** |

### Cause 1 — the read neighbourhood was the *centre's*, not the source's (fixed)

`VegGrid` routed reads through a nine-slot table over `centre ± 1`; a column outside
it had no slot and answered `StateId::AIR`. But a source at offset `(-1, 0)` reads up
to `VEG_PADDING` (8) blocks past its own west edge — chunk offset `-2` from the
centre — so *where the air boundary fell depended on which column was the centre*. A
height probe or ground check that answered air instead of terrain changes one
placement attempt, and because every attempt of a feature draws from one shared
per-feature RNG stream, that shifts every later attempt in the same source. The
divergence therefore shows up well away from the region edge, including on the tree
sitting on the seam.

Fixed by widening the **read** neighbourhood to 5×5
(`region_view::WIDE_RADIUS`/`wide_source_slot`, `VegGrid::sources` is 25 slots).
Decoration still runs for the inner 3×3, writes are still bounded by `VegGrid`'s own
footprint, and only the centre is folded back — nothing about ownership changed.

The 16 rim chunks carry **pre-ore** terrain, the inner 3×3 **post-ore**. That split
is what makes the fix free: a column's pre-ore closure was *already* exactly this 5×5
(`overworld::COLUMN_CLOSURE_RADIUS = 2`, and `store::open_view` already pinned it), so
all 25 are already memoised for every served column. Measured, one cold column, with
`--features gen-counters`:

| counter | before | after |
|---|---|---|
| `pre_ore_computed` | 25 | **25** |
| `post_ore_computed` | 9 | **9** |
| `pre_ore_hits` | 66 | **82** |

Zero additional stage computation; exactly **+16 memoised-store hits per column**
(confirmed at 8×8: `pre_ore_hits` 1340 → 2364 = +1024 = +16 × 64, both
`*_computed` unchanged). Asking for `post_ore_world` on the rim instead would have
widened the closure to 7×7 — 49 pre-ore chunks per column rather than 25 — for a
difference that **cannot move either heightmap**, because ore placement *replaces*
blocks rather than adding or removing them, so the topmost non-air `y` is identical
either way. The approximation is limited to a state *identity* read landing on a cell
an ore blob replaced, at least 16 blocks from the chunk being served.

### Cause 2 — nine sources share one write overlay (open)

`apply_vegetal_decoration_step_3x3_per_source` runs the nine passes into one
`VegGrid`, and reads consult that grid's overlay first — deliberately, because
vanilla's heightmaps update as decoration places blocks. So source `S`'s pass sees
whichever *other* sources ran before it, and both that set and its order are decided
by the centre: with `A` as centre, four chunks precede `A`; with `B` as centre, one
does. Their spill reaches a few blocks into `S`, and one changed read cascades through
the RNG stream exactly as above.

Giving each source its own overlay (merged in source order, last writer winning)
takes the truncation count to **0**. It is **not landed**, because it regresses JVM
FULL3X3 parity past `vegetation_parity.rs`'s own measured bound — identity mismatches
against `vegetation_savanna_neg30_15_jvm.txt` go **1 → 7** where the bound is 3, and
0 → 3 and 0 → 1 at two other fixtures. Vanilla really does mutate one shared level
across the passes, so isolation moves us away from it.

**That is a genuine architectural incompatibility, not a missing patch.** Exact
vanilla behaviour is order-dependent; a recompute-per-centre architecture is only
correct if it is order-*in*dependent. Pick one:

1. **Isolate writes.** Seams become coherent; accept a measured FULL3X3 divergence and
   re-baseline `vegetation_parity.rs` with the new numbers recorded.
2. **Persist decoration instead of recomputing it** — a per-chunk decoration overlay in
   the staged store, written once, read by every neighbour. Vanilla's own model, so
   both properties hold at once; much larger change, and it needs the store to hold a
   *writable* per-chunk product, which today's "one writer per chunk grid" rule forbids.
3. **Leave it.** Chosen. The count moves every time a previously-`Unsupported`
   placer starts drawing for real (each one reshuffles the shared overlay's RNG
   stream for every biome sharing its decoration step) or a new tree family gains
   its first seam-straddling canopy — both are legitimate, budget-moving events, not
   regressions. Most recently: the mega-jungle/giant-spruce/jungle-bush and fancy-oak/
   `FallenTreeFeature` placers landing took it from the prior pin to 162 with no new
   seam-crossing structure involved, then cherry and mangrove trees landing (the
   first real placers `cherry_grove` and `mangrove_swamp` have ever had) took it to
   **314** truncated rows across **nine** biomes — up from three, concentrated in
   `cherry_grove` and `mangrove_swamp` themselves (new), plus `bamboo_jungle`,
   `forest`, `jungle`, `old_growth_birch_forest`, `old_growth_pine_taiga` and
   `old_growth_spruce_taiga`. `old_growth_birch_forest` in particular moved off its
   former zero guarantee for the first reason above, not the second — see
   `vegetation_seam_consistency.rs`'s own `MEASURED_TOTAL`/`EXPECTED`/`FIXED_TO_ZERO`
   doc comments for the full per-landing breakdown and the re-measurement method
   (isolated `git worktree` checkouts, one commit at a time).

`DESIGN.md` §12.118 carries the original measurements; the re-baseline above is
recorded in the test file itself per this repo's rule that a floor is a measurement,
not a tolerance.

## Gates

`crates/lodestone-worldgen/tests/vegetation_seam_consistency.rs`:

- `the_fixture_contains_seam_straddling_canopies` — the hard precondition. The fixture
  measures **3,437** border-crossing canopy rows across **21** biomes (up from 1,777
  across 19 as more tree families gained real placers). This crate's own
  `tests/support/worldgen_data` tree carries only `plains` and `savanna` and **both
  measure zero truncations at every arm**, so the gate reads the bundled production
  data at `crates/lodestone-server/assets/worldgen` (tracked repo state, read off disk
  — no dependency on `lodestone-server` the crate). A plains fixture here would be the
  *world* species of vacuous test: unreadable from the assertions.
- `a_canopy_crossing_a_chunk_border_is_served_whole` — the claim. Predicts the exact
  per-biome residual for nine biomes, asserts `birch_forest` at exactly zero, and
  prints a bounding box on failure.
- `narrow_read_neighbourhood_is_what_truncates` — **the control, and it fires.** Same
  binary, same fixture, one variable: the 16 rim chunks are handed `None`, reproducing
  the nine-slot read table. Measures 621 against the fixed arm's 314, and requires
  `FIXED_TO_ZERO` biomes to be non-zero under it.
- `region_view.rs`'s `wide_source_slot_routes_every_local_column_in_the_five_by_five`
  and `the_narrow_table_answers_air_where_the_wide_one_answers_a_rim_chunk` — the
  routing table against an independently written band walk, plus the control that the
  narrow table really did answer air over the rim.

**What the fixture cannot tell you.** The world is *flat*, which is what makes every
biome's trees place in quantity — but it also means every height probe answers the
same value everywhere, so the fixture suppresses one channel real terrain has: a
height read that lands on a slope. The counts here are therefore a **lower bound** on
what a real world does, not an estimate of it. Both causes above are mechanisms of the
driver rather than of the terrain, so the fixture is sound evidence about *them*; it is
not a frequency estimate for a played world.

Unchanged and still green: all 13 parity binaries, `lodestone-server`'s
`a_canopy_spans_the_chunk_seam_in_both_served_chunks`,
`column_is_byte_identical_across_two_independently_constructed_generators`,
`parallel_generation_is_deterministic_and_matches_serial` and
`plains_grass_patch_attempt_count_matches_the_placement_json`. The fixture-driven
parity suites are byte-identical by construction: they build their grid with
`VegGrid::with_footprint` and `seed`, which has no source table, so the routing change
cannot reach them.

## How to change it, and the gotchas

- **The floors in the gate are measurements, not tolerances.** If a change moves them,
  re-measure and record the new number with the reason. Do not delete the test — that
  is exactly what happened to this coordinate space's previous gate (see
  `VegGrid`'s own doc comment).
- **`REGION_MIN`/`REGION_MAX` did not change and must not be widened casually.** They
  size the ore engine's `RegionHeights` table and `OreInput::region_local`'s clamp.
  The vegetation read radius is a separate constant for that reason.
- **Ore has the same structural defect and it is untouched.** `RegionView` still routes
  through the nine-slot `source_slot`, and `apply_ore_step_3x3_per_source` still shares
  one overlay across nine sources, so an ore blob straddling a seam is inconsistent the
  same way. Not measured, and not the reported symptom (ore is underground), but it is
  the same class and should not be assumed fixed.
- **A read that answers air where terrain exists is never a safe default here.** It
  looks conservative and is not: it changes an RNG stream, and the effect surfaces
  somewhere other than where the wrong read happened.

## Configuration

None. `region_view::WIDE_RADIUS` (2) and `feature::VEG_PADDING` (8) are constants; the
gate's expected values are constants in the test file.

## Dependencies

`lodestone_worldgen::feature::region_view` (routing), `feature::vegetation::VegGrid`
(the read/write medium), `overworld::decorate::vegetation_stage` (supplies the 25
sources), `overworld::store` (memoises them). The gate additionally reads
`crates/lodestone-server/assets/worldgen`.
