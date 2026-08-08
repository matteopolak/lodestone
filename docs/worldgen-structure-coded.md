# Coded structure pieces

## What it is

The half of the structure engine whose blocks are Java statements rather than an `.nbt` template —
`crates/lodestone-worldgen/src/structure/coded.rs`, issue #514's phase S5. It ports
`StructurePiece`'s block-writing helpers and `ScatteredFeaturePiece`'s two ground-height rules, and on
top of them the piece generators for **`swamp_hut`** and **`desert_pyramid`**.

## How it works

### The shape of the problem

Vanilla's coded generators write into the world *as they walk*, from inside `postProcess`, which runs
once per chunk the piece intersects. They read heights and existing blocks at arbitrary positions, and
they freely write across chunk borders — `chunkBB` is passed down to every `placeBlock` and silently
discards anything outside it. Whichever chunk generates first decides any value that was memoised on
the piece (`HPos`, `hasPlacedChest[]`, `spawnedWitch`).

Our chunks are generated independently and memoised, so none of that is available. This is the same
wall S2 hit for template piece Y (`docs/worldgen-structure-templates.md`), one step further along.

### The seam

A `coded::Builder` accumulates the piece's **whole** block list eagerly, at start time, against
`StartContext`. `StructurePiece::blocks` carries it, and `structure_place_stage` clips it per chunk
through `DenseBlockGrid::set`, which ignores an out-of-box write.

```text
Builder::new(west, 64, north, orientation, w, h, d)   // makeBoundingBox + setOrientation
builder.lowest_ground_height(ctx, offset)?            // or average_ground_height
builder.generate_box(...) / place(...) / fill_column_down(ctx, ...)
builder.finish("minecraft:tedp")                      -> StructurePiece
```

`chunkBB` disappears from every signature. A write vanilla would have skipped is recorded here and
clipped later, which is the same set of blocks in the same **last-write-wins** order — and the order is
load-bearing: a pyramid carves its corridors by writing `air` over sandstone it placed two statements
earlier.

### Three things that are easy to get wrong

- **Local coordinates are not world coordinates.** `getWorldX(x, z)` / `getWorldZ(x, z)` depend on the
  piece's orientation: a NORTH piece's local Z counts *down* from the box maximum, and an X-axis
  orientation swaps the two horizontal extents in `makeBoundingBox` as well. An orientation bug builds
  a complete, plausible structure in a box that no longer matches its own `BB`.
- **`setOrientation` is a table, not a rule.** SOUTH mirrors `LEFT_RIGHT` and does not rotate; WEST does
  both; EAST only rotates; NORTH neither. So a real `Mirror` reaches `BlockState::mirror` for the first
  time in this engine, which is why the stair `shape` remap landed with this unit — it had been an inert
  ledger row (`template:mirrored_shape`) for as long as everything used `Mirror.NONE`.
- **The two ground rules are different functions.** `updateHeightPositionToLowestGroundHeight` (the
  pyramid) scans the whole piece box and is chunk-independent in vanilla too.
  `updateAverageGroundHeight` (the hut) averages over *the intersection of the box with the decorating
  chunk*, and is not. See below.

### `swamp_hut`'s height, and why it is not vanilla's

There is no single vanilla answer to reproduce. `updateAverageGroundHeight` averages the heightmap over
the box ∩ decorating chunk, so a hut spanning two chunks has **two** vanilla answers and vanilla picks
whichever chunk ran first, memoising it into `HPos`. That is a real order dependence in vanilla, not an
artefact of our pipeline.

`Builder::average_ground_height` averages over the **whole box**. That choice is:

- identical to vanilla whenever the piece lies inside one chunk — a 7×9 hut placed from the chunk's min
  corner spans at most two chunks, so this is a real fraction of cases;
- the area-weighted mean of vanilla's per-chunk answers, so never outside their range;
- a pure function of `(seed, chunk)`, which is the property the whole engine rests on. A hut whose Y
  depended on visit order would shear at a chunk border — and would still be sheared after a reload,
  since only one `HPos` is persisted.

Ledgered as `coded:average_ground_height`.

### `desert_pyramid`, and the two draws that are not ours

`postProcess` reads `level.getRandom()` twice — for the cellar's sand/sandstone `variant` boolean and
for each collapsed-roof cell's `nextFloat() < 0.33`. That is the *decorating region's* stream, so
vanilla's own answer is chunk-order dependent again. Both are **position-seeded** here
(`Mth.getSeed(pos)`), exactly as every processor draw already is. Ledgered as `coded:region_random`.

`randomCollapsedRoofPos` and `afterPlace`'s shuffle fork the **world** seed positionally, and the piece
generator sits three layers below the start predicate that holds the seed; a fixed fork seed is used, so
those two picks are position-dependent but seed-independent. Ledgered as `coded:pyramid_roof_seed` —
threading the seed down is the faithful fix and is a one-line change to `desert_pyramid_pieces`' signature.

`afterPlace` itself needed no deviation: its candidate set is the whole piece's, its shuffle is a
positional fork at the piece box centre, and only the *writes* are clipped by `chunkBB`. Two details are
the specification and both are silent when wrong:

| detail | value | the wrong reading |
|---|---|---|
| the candidate set's order | `SortedArraySet(Vec3i::compareTo)` — **y, then z, then x** | insertion order, which the shuffle then permutes differently |
| how many are suspicious | `nextInt(5, 8)` = `5 + nextInt(3)`, i.e. **5..=7** | `nextIntBetweenInclusive(5, 8)`, i.e. 5..=8 |

## How to change it

- **To add a coded structure**: add a `StructureKind` variant, parse its `type`, and write its
  generator against `Builder`. Nothing else in the engine changes — `StructurePiece::blocks` and the
  placement stage already exist.
- **`fill_column_down` is the only helper that reads the world**, through
  `StartContext::is_replaceable_at` (air or fluid in the pre-surface `_WG` column). It defaults to
  "solid everywhere" on the trait, so an implementor that does not override it gets one-block
  foundations rather than runaway ones.
- **A generator that needs the *world* seed** has to have it threaded from `generate_pieces`; today only
  the pyramid wants it and it is ledgered instead.
- **`StructurePiece::blocks` is `Option<Arc<Vec<CodedBlock>>>`**, not a bare `Vec`: a start is cloned
  into every chunk that references it, and a pyramid is ~5,300 entries.

## Configuration

None. Both structures come entirely from `worldgen/structure/{swamp_hut,desert_pyramid}.json` and
`worldgen/structure_set/{swamp_huts,desert_pyramids}.json`, already bundled.

## Evidence

`crates/lodestone-worldgen/tests/structure_coded_place.rs`, at the vanilla oracle world's seed
(−195764831) against the real bundle, with the **structure-free resolver over identical data** as the
control:

| arm | asserts |
|---|---|
| pyramid | the world holds **exactly** the count of signature blocks the piece's own last-write-wins map carries (403 at the gated chunk), and the control holds **0** |
| hut | > 80 hut-only blocks, control **0** |
| reproducibility | two independently constructed generators produce byte-identical block lists |
| ledger | `swamp_hut` and `desert_pyramid` are **absent** from the ledger; `jungle_pyramid`, `mineshaft`, `stronghold`, `monument`, `ruined_portal` are present; all four `coded:` deviation rows are present |

Neither structure appears in the oracle world's generated area, so the chunks come from the placement
engine (itself gated against that oracle by S1), walked outward in rings until the biome filter lets one
through: **234** candidate cells for the pyramid and **211** for the hut, which is why the chunks are
recorded as constants — a bounded search reports "not implemented" for a working generator.

Unit arms in `coded.rs` cover the four orientation coordinate mappings, `makeBoundingBox`'s axis swap,
`generateBox`'s edge/fill split (a 3-cube is 26 edge blocks and one interior), `Facing::random`'s single
draw in `Direction.Plane.HORIZONTAL` order, and the 2D data values (SOUTH is 0, not NORTH).

## What is still coded and not here

`jungle_pyramid` (368 lines, and the only one needing `generateBox`'s RNG `BlockSelector` overload),
`mineshaft` (1,386), `stronghold` (1,766), `monument` (1,988), `ruined_portal` (its own vertical
placement plus `spreadNetherrack`'s 29×29 cross-chunk apron), `nether_fossil`, `fortress`, `end_city`,
`mansion`. Each is named on `StructureRegistry::unsupported`. The infrastructure in this file is what
they were all blocked on; what remains for `jungle_pyramid` is transcription plus one helper.

## Dependencies

`StartContext` (heights and column contents), `structure::template::BlockState` (the mirror/rotate
transform `placeBlock` applies), and `overworld::structures::structure_place_stage` (the clip).
