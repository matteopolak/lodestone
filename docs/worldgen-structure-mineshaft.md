# Mineshafts, and eager piece generation

## What it is

The port of `MineshaftStructure` and all four `MineshaftPieces` types —
`crates/lodestone-worldgen/src/structure/mineshaft.rs` — and with it the structure engine's second
piece-building mode: **eager** generation, where a structure's whole piece tree is built *before* its
generation point is known. Two structures ride on it, `minecraft:mineshaft` and
`minecraft:mineshaft_mesa`, which differ only in three block states.

## How it works

### Why this is a second engine and not a third generator

Every coded structure that landed before this one is a `SinglePieceStructure`: one box, one
`postProcess`, and a `findGenerationPoint` that draws no RNG. That is what lets the rest of the engine
keep piece generation **lazy** — `StructureKind::find_stub` produces a position, the caller applies the
biome filter, and only a survivor reaches `generate_pieces`. Vanilla depends on that order: a
biome-rejected candidate must consume no RNG, or every later structure at the seed moves.

A mineshaft is the opposite on both axes, and the two facts compound.

- **The pieces come first.** `findGenerationPoint` returns `Either.right(builder)`. It grows the tree,
  and only then does `moveBelowSeaLevel` (or, for the mesa variant, a surface probe at the union box's
  centre) decide how far down the whole set moves. The start's Y *is* that shift, so the generation point
  cannot be computed without the tree.
- **The tree is grown in two passes.** `addChildren` recurses to depth 8, testing each candidate box
  against every piece placed so far, and the vertical shift happens after all of them exist. Blocks
  cannot be resolved during that pass because every one of them would move.

So `find_stub` does the whole job and `Stub::Eager` carries the finished `Vec<StructurePiece>` across the
biome check. `generate_pieces` matches on the **stub**, not on the kind, so a future eager structure that
forgets to add its arm fails loudly rather than silently returning no pieces.

```text
mineshaft::generate(cx, cz, ctx, wood, blocking, random)
    random.next_double()                    // discarded; shifts the whole stream
    Shaft::room + add_children(...)         // boxes only, depth <= 8, collision-tested
    move_below_sea_level / mesa surface pick
    into_pieces(ctx, random)                // postProcess, in list order, one shared View
  -> (Vec<StructurePiece>, start position)
```

### `Shaft` — `StructurePiecesBuilder`, narrowed

`Shaft` holds `Vec<Node>`, where a `Node` is a box, an optional orientation, a generation depth and a
`Kind` carrying the per-family facts (`has_rails`, `spider_corridor`, `sections`; `two_floored`; the
room's `childEntranceBoxes`). It offers exactly the four things a mineshaft asks of vanilla's builder:
`collides` (`findCollisionPiece`), `bounding_box`, `offset_vertically` and `move_below_sea_level`.

**The room and the crossing leave `orientation` null.** That is not an omission — it is what makes their
`postProcess` address blocks in **absolute** world coordinates, because `getWorldX/Y/Z` is the identity
for a null orientation. The corridor and the stairs call `setOrientation` and therefore work in local
coordinates with a mirror and a rotation applied to every state. A crossing stores its direction in a
*field* instead, which is why it can branch on direction while still writing absolutely.

### `View` — the world one `postProcess` pass reads

Six mineshaft helpers read the world and **branch** on what they find: `canBeReplaced`,
`isSupportingBox`, `placeSupportPillar`, `setPlanksBlock`, `placeDoubleLowerOrUpperSupport` and
`fillPillarDownOrChainUp`. In vanilla they read the level, which by then holds the terrain *plus*
whatever earlier pieces of the same start wrote.

`View` is that: pre-surface terrain from `StartContext::block_kind_at`, with an overlay of every block
this start has already written. One `View` serves the whole start, so a corridor's
`placeDoubleLowerOrUpperSupport` sees the floor planks the same corridor laid two statements earlier, and
a crossing's `placeSupportPillar` sees the stone above its ceiling. Independent per-piece block lists
would break both.

Predicates are derived from the four terrain kinds plus a small table over the eight states a mineshaft
writes:

| vanilla | here | note |
|---|---|---|
| `isAir()` | `Air`, or a written `air`/`cave_air` | the whole interior of a mineshaft is `cave_air` |
| `liquid()` | `Water`/`Lava`; a written block is never liquid | no mineshaft piece writes a fluid |
| `isReplaceableByStructures` | air or fluid | glow lichen and seagrass cannot exist pre-surface |
| `isFaceSturdy(UP)`, `isSolidRender`, `canSupportCenter` | `Stone`, or written planks/log/spawner | a **fence** is not sturdy, which is what stops a support column growing out of its own fence |
| `state.is(Block)` | written name equality | only the four wood/chain tests need it |

`canBeReplaced` is the override that matters most: it refuses to overwrite this mineshaft type's planks,
log, fence or `iron_chain`. Without it a corridor's `cave_air` sweep erases the woodwork of the corridor
it crosses, and the result is a mineshaft with no supports wherever two pieces touch.

### `StartContext::block_kind_at`

`is_replaceable_at` answers one bit — air-or-fluid — and three transcriptions need more:

- `isInInvalidLocation` walks a box's shell looking for `state.liquid()`, so a mineshaft that read air as
  liquid would refuse every piece it generated;
- `fillPillarDownOrChainUp` treats a liquid column as empty but stops at lava;
- `RuinedPortalPiece.canBlockBeReplacedByNetherrackOrMagma` tests lava and obsidian.

`block_kind_at` returns the four-way `BlockKind` the fill already computes, read out of the same
per-chunk cached `AquiferSystem` as the height probe. `is_replaceable_at` now **defaults to** it, so an
implementor supplying one gets the other and the two cannot disagree.

**It cannot answer a material question.** Pre-surface every solid block is one `Stone`: surface rules,
ore blobs and carvers all run after the eager start pass. That is why `buried_treasure`, whose walk
terminates on "the block below is sandstone/andesite/granite/diorite", is still ledgered rather than
placed — a `block_kind_at` there would terminate on the first iteration and put the chest on the beach.

### Rail shape, which closed a ledger row

A mineshaft corridor is the first thing in this engine to place a **rail** under a real transform: an
EAST/WEST corridor carries `Rotation::CLOCKWISE_90`, so a `north_south` rail must come out `east_west`.
`BlockState::{rotate, mirror}` gained `BaseRailBlock`'s two tables, transcribed rather than derived —
"rotate the two connected directions and re-canonicalise" needs a canonical-name pass of its own, and a
table lifted from the source cannot disagree with it. Both are keyed on the `shape` **value**, because
`shape` is spelled by two unrelated block families and the stair set
(`straight`/`inner_*`/`outer_*`) is disjoint from the ten rail shapes.

`template:mirrored_shape` is therefore **gone** from the ledger, and `structure_coded_place.rs` asserts
its absence.

## How to change it

- **The RNG order is the specification, and the two passes share one stream.** `addChildren`'s draws
  interleave with `createRandomShaftPiece`'s `nextInt(100)`, `findCorridorSize`'s `nextInt(3)` and the
  per-child recursion; `postProcess` then continues from the same stream. Reorderings that look like
  tidy-ups build a different mineshaft. Three specific traps:
  - the discarded leading `nextDouble()` exists **only** to shift the stream, and there is a test whose
    whole job is to fail if it is removed;
  - `findCrossing`'s `nextInt(4)` is drawn **before** the collision test, so it is spent on a rejected
    candidate; `findCorridorSize`'s is drawn once for up to three candidate lengths;
  - `maybePlaceCobWeb` consults `isInterior` **before** drawing, so a non-interior position costs no
    draw — while the spider spawner's `nextInt(3)` is drawn **before** its own `isInterior`. The
    asymmetry is vanilla's.
- **`generateMaybeBox` draws once per position, unconditionally**, as the leftmost operand of an `&&`
  chain. A version that skipped positions it could not write desynchronises everything after it.
- **The two column walks advance together.** `fillPillarDownOrChainUp` steps the downward and upward
  probes once each per iteration and checks the downward one first, so a pillar wins a tie against a
  chain at equal distance. Splitting them into two loops flips that.
- **Adding a piece kind** means a `Kind` variant, a `find_*` box function, an `add_children` arm and a
  `post_process` arm — and its `piece_id`, which is the `StructurePieceType` id vanilla persists.

## Deviations, all the same shape

Vanilla runs `postProcess` once **per decorating chunk**, with that chunk's own feature random, clipping
every read and write to `chunkBB`. A corridor spanning two chunks draws its cobwebs twice from two
unrelated streams and keeps whichever half landed; `isInInvalidLocation` can call the same piece invalid
in one chunk and valid in another. There is no single vanilla answer to reproduce, exactly as
`swamp_hut`'s average ground height had none. Resolved eagerly here, once:

| ledger row | what it records |
|---|---|
| `coded:region_random` | `postProcess`'s random is the structure's own stream, continuing after piece layout, rather than the decorating chunk's |
| `mineshaft:post_process_scope` | `isInInvalidLocation`, `hasSturdyNeighbours` and every `getBlock` see the whole piece instead of one chunk's slice; `structure_place_stage` clips instead of `chunkBB` |
| `mineshaft:pre_surface_world_reads` | every solid block is one `Stone`; `isFaceSturdy` is a table over the eight states a mineshaft writes rather than a solidity model |
| `coded:worldgen_entities` | the chest **minecart** and the spider spawner's `SpawnData`. The rail is placed and the loot table plus vanilla's `nextLong()` roll seed travel on `StructurePiece::loot`; only the entity is missing |

## Evidence

The strongest expected value for this unit was already on disk. The `mineshafts` structure set is
**closed** — `findGenerationPoint` returns `Optional.of(...)` unconditionally, so biome-valid implies
start-valid — which means this engine's start set for it is exactly vanilla's in *both* directions, not a
superset. So `mineshaft` and `mineshaft_mesa` join `CLOSED_SET_STRUCTURES` in
`tests/structure_placement_oracle.rs` and the vanilla-authored survival world's own **46** mineshaft
start chunks become the expectation:

| arm | result |
|---|---|
| positive | all **77** closed-set oracle starts reproduced at exactly their chunk (31 before, plus mineshaft's 46) |
| negative | **zero** extra closed-set starts over 4,080 generated window chunks; window census 18 |
| blocks | **97** signature blocks at oracle chunk (10, 24), **0** in a structure-free control over identical data |

The block arm's expected count is predicted from the *start* stage and measured at the *placement*
stage. It is predicted from `structure_starts_placed_in` — production's own reached-start list — and not
from "the start at this chunk": a mineshaft is ~160 blocks wide and the oracle world has a second start
three chunks away, so predicting from one start measured **59** against a true **97**. Re-deriving the
17×17 reach in the test would have been the same class of mistake as re-deriving a placement grid.

Unit arms in `mineshaft.rs` cover the exact piece and block counts at two seeds (101 pieces / 14,344
blocks, and a legitimately **lone room** at 1 piece / 435 blocks — the room's wall walks can all break on
their first draw, and a generator that could not do that would look healthier and be wrong), the
`nextDouble` stream shift, the `moveBelowSeaLevel` vs mesa-surface split as a two-hypothesis pair, the
blocking-biome veto against its own control, `canBeReplaced`, and the rail rotate/mirror tables including
a stair that must be left alone.

## Configuration

None. `mineshaft_type` comes from the structure document and `#minecraft:mineshaft_blocking` from the
bundled biome tag.

## Dependencies

`StartContext` (column heights and `block_kind_at`), `structure::coded::Facing` (the shared horizontal
direction and its 2D data values), `structure::template::BlockState` (the mirror/rotate transform
`placeBlock` applies) and `overworld::structures::structure_place_stage` (the per-chunk clip).
