# Coded structure pieces

## What it is

The half of the structure engine whose blocks are Java statements rather than an `.nbt` template —
`crates/lodestone-worldgen/src/structure/coded.rs`, issue #514's phase S5. It ports
`StructurePiece`'s block-writing helpers and `ScatteredFeaturePiece`'s two ground-height rules, and on
top of them the piece generators for **`swamp_hut`**, **`desert_pyramid`** and **`jungle_pyramid`**.

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

The four trap-room chests are placed as **blocks** now, at the alcove floors, overwriting the `air` the
alcove loop wrote there. Their loot tables and vanilla's `nextLong()` roll seeds ride on
`StructurePiece::loot`; see `jungle_pyramid` below for why that list exists and what still has to read it.

`afterPlace` itself needed no deviation: its candidate set is the whole piece's, its shuffle is a
positional fork at the piece box centre, and only the *writes* are clipped by `chunkBB`. Two details are
the specification and both are silent when wrong:

| detail | value | the wrong reading |
|---|---|---|
| the candidate set's order | `SortedArraySet(Vec3i::compareTo)` — **y, then z, then x** | insertion order, which the shuffle then permutes differently |
| how many are suspicious | `nextInt(5, 8)` = `5 + nextInt(3)`, i.e. **5..=7** | `nextIntBetweenInclusive(5, 8)`, i.e. 5..=8 |

### `jungle_pyramid`, and the one number that is the whole specification

368 Java statements, two tripwire/dispenser traps, a sticky-piston puzzle and two chests. The only new
machinery it needed was `generateBox`'s `BlockSelector` overload — `Builder::generate_box_selected` —
plus `generate_air_box`, `create_chest` and `create_dispenser`.

**`MossStoneSelector` draws one `nextFloat()` per position in the box, before the write**, so a box of
`n` positions consumes `n` draws whether or not each write lands in the decorating chunk. Summed over
the 43 selector call sites with the loops expanded that is **1,522** draws, and with the orientation
`nextInt(4)` and four `nextLong()`s (two `next(32)` each) the piece's total stream advance is **1,531**.
That number is the gate: a temple whose selector is only consulted for served positions, or which
re-seeds between its two halves, is still temple-shaped.

Its `random` is vanilla's *decoration* stream — per chunk, so chunk-order dependent, the same ambiguity
`desert_pyramid` has. Every draw comes out of the structure's own per-chunk stream here instead, in
vanilla's order and count, which makes the temple a pure function of `(seed, chunk)`. Ledgered as
`coded:decoration_random`.

Two smaller facts, both invisible when wrong:

| fact | why it matters |
|---|---|
| `createChest` calls `level.setBlock` **directly**, `createDispenser` goes through `placeBlock` | the dispenser's `facing` is mirrored/rotated with the piece and the chest's is not |
| vanilla's chest `facing` comes from `reorient`, which reads the four horizontal neighbours' render-solidity *as written so far* | there is no block-state read on `StartContext` and no solidity table in this crate, so a coded chest keeps `facing=north`. Ledgered as `coded:chest_reorient` — cosmetic, and the only coded-piece property knowingly not vanilla's |

### Where a coded piece's loot goes

`StructurePiece::loot` is a `Vec<CodedLoot>` of `(pos, table, seed)`. It is a side list rather than a
field on `CodedBlock` because a pyramid is ~7k blocks and four of them are chests.

**Nothing reads it yet, and that is the one open gap here.** `lodestone_server::structure_loot` resolves
a *template* piece's loot by re-reading the raw `.nbt` bytes for `structure_block` DATA markers; a coded
piece has no template, so that pass structurally cannot see these. Ledgered as `coded:chests`. Note this
is a **narrower** claim than the row it replaced, which said worldgen had no block entities and no loot
tables — both of those exist (`overworld::block_entities`, and the server's roller plus
`structure_loot`), and the marker path for shipwreck / igloo / ocean ruin has been rolling real loot
since #337.

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
| jungle temple | the same exactly-equal-to-predicted count over a signature set with no other source in a jungle (`chiseled_stone_bricks`, `cobblestone_stairs`, `lever`, `repeater`, `sticky_piston`, `dispenser`, `tripwire`/`_hook`, `chest`), control **0**. `mossy_cobblestone` is deliberately excluded — the temple's commonest block, but not uniquely its |
| coded containers | each piece's `loot` list carries vanilla's table ids in vanilla's order, four **distinct** roll seeds (two equal seeds would mean a re-seed), and a chest/dispenser block surviving in the piece's *final* state at that position |
| reproducibility | two independently constructed generators produce byte-identical block lists |
| ledger | `swamp_hut`, `desert_pyramid` and `jungle_pyramid` are **absent** from the ledger; `mineshaft`, `stronghold`, `monument` are present, and — as of S8 — so is `minecraft:ruined_portal_nether` in place of the six overworld `ruined_portal*` ids, which landed via the template engine (`worldgen-structure-templates.md`), not this one; every `coded:` deviation row is present; `template:data_markers` is **gone** and `template:block_entity_nbt` carries its measured 132 |

Neither structure appears in the oracle world's generated area, so the chunks come from the placement
engine (itself gated against that oracle by S1), walked outward in rings until the biome filter lets one
through: **234** candidate cells for the pyramid and **211** for the hut, which is why the chunks are
recorded as constants — a bounded search reports "not implemented" for a working generator. The jungle
temple's is grid ring **5**, far nearer than either, so a search bound tuned to the pyramid would have
found it and one tuned to ring 4 would not: the bound is a per-structure measurement, not a constant.
`find_the_nearest_start_chunks` is the `#[ignore]`d search that produced all three, and it walks
**placement cells** rather than chunks (one candidate per `spacing²`) using
`Placement::potential_structure_chunk` — production's own function, because a test helper that
re-derives the grid maths turns a failing gate into a hanging one.

Unit arms in `coded.rs` cover the four orientation coordinate mappings, `makeBoundingBox`'s axis swap,
`generateBox`'s edge/fill split (a 3-cube is 26 edge blocks and one interior), `Facing::random`'s single
draw in `Direction.Plane.HORIZONTAL` order, and the 2D data values (SOUTH is 0, not NORTH). The jungle
temple adds the stream-position arm above plus the cobble/mossy split as a **two-hypothesis** magnitude
test: `nextFloat() < 0.4F` selects cobblestone, so the inverted reading predicts 0.6, and both values are
computed from outside constants and the measurement is required to land on one. One continuous stream of
1,522 draws, never 1,522 fresh randoms — sequentially seeded LCGs are correlated in their first draw.

## What is still coded and not here

`stronghold` (1,766 lines), which needs **eager piece generation**; `monument` (1,988), pieces only;
`nether_fossil`, `fortress`, `end_city`, `mansion`. Each is named on
`StructureRegistry::unsupported`.

**`mineshaft` ×2 has landed, and it is not in this file.** Eager piece generation is a different
architecture, not a third generator on the `coded::Builder` seam, so it lives in
`structure/mineshaft.rs` — see [`worldgen-structure-mineshaft.md`](./worldgen-structure-mineshaft.md).
`stronghold` is the same shape and is the natural next user of `Shaft` and `View`.

**`ruined_portal` is not on this list any more, and it never really belonged on it — see S8's own
correction.** Its own vertical placement, template pick, rotation/mirror and processor chain (gold-gone,
lava swap, `block_age`, `protected_block`, `lava_submerged`, `blackstone_replace`) landed via the
**template** engine (`worldgen-structure-templates.md`), the same architecture shipwreck/ocean
ruin/igloo use, because a ruined portal *is* a template piece with extra processors — not a coded one.
What remains uncoded is only `spreadNetherrack`'s terrain skirt and the drip/vine passes, ledgered as
`coded:ruined_portal_terrain_skirt`.

**`buried_treasure` is on this file's list for a different reason, and its own fix landed a third way.**
It produces a start and a real bounding box, and used to place **zero blocks**:
`BuriedTreasurePieces.postProcess` walks a cursor down until the block *below* it is
sandstone/stone/andesite/granite/diorite, then writes up to five neighbours and one chest — a
**material** distinction (sand and sandstone are surface-rule products; granite/diorite/andesite are
ore-blob products) that does not exist yet at the eager start pass's pre-surface `_WG` stage, where every
solid block is one `Stone`. Neither `coded::Builder` (start-time, same pre-surface limit) nor the template
engine (no template) fit, so S8 added a third seam instead:
[`crate::structure::PieceRefinement`], which `structure_place_stage` runs against the chunk's **real**,
already-surfaced-and-carved grid at *placement* time. See `overworld::structures::place_buried_treasure_chest`
and `worldgen-structure-templates.md`'s own note on the three ways a piece now reaches the grid. The
`coded:buried_treasure_chest` ledger row is gone.

## Dependencies

`StartContext` (heights and column contents), `structure::template::BlockState` (the mirror/rotate
transform `placeBlock` applies), and `overworld::structures::structure_place_stage` (the clip).
