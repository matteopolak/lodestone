# Block outline and interaction shapes

## What it is

Per-block-state `BlockStateBase.getShape` (the **outline**) and
`BlockStateBase.getInteractionShape`, for protocol 776 (Minecraft 26.2), dumped
from the real 26.2 server and committed as generated tables.

The outline shape is what **block selection** uses. It is a third thing, neither
the collision shape ([`collision_shapes`](../crates/protocol/v770/src/collision_shapes.rs))
nor fluid presence — 50.9% of the 32,366 block states have an outline that
differs from their collision shape.

## Why it is needed

Two things depended on not having it:

1. **The kelp fix** (`13a1d3a`) used *"has baked model quads"* as a proxy for
   "non-empty outline shape" in `LiveCollision::is_pickable`
   (`crates/lodestone-shell/src/collision.rs`). Structurally sound — fluids are
   the one thing vanilla does not draw through the model pipeline — but a proxy,
   and the doc comment there says so.
2. **Selection boxes are a full unit cube for everything**, including slabs,
   stairs and kelp. Only **3,328 of 32,366** states are actually a full cube.

## How it works

### The three getters diverge at the base class

| getter | default | source |
| --- | --- | --- |
| `getShape` (outline) | `Shapes.block()` | `BlockBehaviour.java:323-325` |
| `getCollisionShape` | `hasCollision ? state.getShape(…) : Shapes.empty()` | `BlockBehaviour.java:327-329` |
| `getInteractionShape` | `Shapes.empty()` | `BlockBehaviour.java:295-297` |

So every `noCollission()` block — kelp, seagrass, torches, cobweb, redstone wire,
fire, every plant — has **no collision and a real outline**. That is 5,282 states.

Block picking is `Entity.pick`, which clips with `ClipContext.Block.OUTLINE` and
`ClipContext.Fluid.NONE` (`Entity.java:2012-2017`), and
`ClipContext.Block.OUTLINE` *is* `BlockStateBase::getShape` (`ClipContext.java:57`).
Hence:

- `LiquidBlock.getShape` → `Shapes.empty()` (`LiquidBlock.java:145-147`), so open
  water and lava are never targeted — picking cannot be "the cell is not empty";
- `KelpBlock`'s is `Block.column(16, 0, 9)` (`KelpBlock.java:24`) and
  `SeagrassBlock`'s `Block.column(12, 0, 12)` (`SeagrassBlock.java:29`) —
  non-empty, so both are targetable despite hardcoding `getFluidState` to water.
  Picking cannot be `!is_water` either;
- `WebBlock` overrides no shape getter at all, so cobweb outlines to a **full unit
  cube** while colliding with nothing. The single cleanest demonstration that the
  two censuses are not interchangeable;
- walls build their outline with `makeShapes(16.0F, 14.0F)` and their collision
  with `makeShapes(24.0F, 24.0F)` (`WallBlock.java:66-67`), so a wall's outline
  tops out at `y = 1.0` while its collision reaches `y = 1.5`. Selecting through
  the collision shape would draw the box half a block above the wall.

### The interaction shape refines the hit *face*; it does not add a hit

Its one caller is `BlockGetter.clipWithInteractionOverride`
(`BlockGetter.java:82-94`): it clips the **outline** first, and only if that hit
does it clip the interaction shape and — when that hit is nearer — substitute its
`Direction` into the outline's hit, **keeping the outline's hit location**. It can
never make an unpickable block pickable and never moves the hit point.

Only four block families override it in 26.2: the cauldron family
(`cauldron`, `water_cauldron`, `lava_cauldron`, `powder_snow_cauldron`), `hopper`,
`scaffolding` and `composter` — 8 distinct shapes including the empty one.
`only_four_block_families_have_an_interaction_shape` pins the set.

### The dump

`crates/protocol/v770/oracle-java/OutlineShapeOracle.java` boots the real 26.2
server and walks `Block.BLOCK_STATE_REGISTRY`, dumping both shapes'
`toAabbs()`. Unlike `lodestone-physics`'s `ShapeOracle` (whose 5.7 MB
`shape_java.txt` is gitignored), this dumper **de-duplicates in the JVM**, by
exact `Double.doubleToRawLongBits` list identity, so the anchor is 422 KB and can
be committed:

```text
C <stateCount>
B <firstStateIdOfBlock> <blockName>
S <O|X> <shapeIndex> <boxCount> [minX minY minZ maxX maxY maxZ]...   (raw double bits, hex)
P <O|X> <startStateId> <shapeIndex>...                              (256 per line, ascending)
```

`B` lines carry only block boundaries (1,196 of them). The generator expands them
to a per-state name and reconciles all 32,366 against
`block_states::block_name`, which is generated from `blocks.json` — a second,
independently produced artifact.

Committed at `crates/protocol/v770/tests/support/outline_shape_jvm.txt`.

### Regenerating

```bash
CACHE="$(cd .cache/mc/26.2 && pwd)"
HERE="$(cd crates/protocol/v770/oracle-java && pwd)"
docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
  CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
  cp /oracle/OutlineShapeOracle.java /work/ && javac -cp "$CP" -d /work /work/OutlineShapeOracle.java
  java -cp "/work:$CP" OutlineShapeOracle'
# copy stdout over tests/support/outline_shape_jvm.txt, keeping the `#` header, then:
LODESTONE_REGEN=1 cargo test -p lodestone-v770 --test outline_shapes \
    committed_tables_match_dump -- --ignored --nocapture
```

### The tables and the lookup

`crates/protocol/v770/src/generated/outline_shapes.rs`, same memory design as the
collision table: pure rodata, zero heap, O(1) by id. **860 distinct outline
shapes** and **8 distinct interaction shapes** over 32,366 states, each family a
`[u16; 32_366]` index (~63 KiB) into a de-duplicated shape table.

`crates/protocol/v770/src/outline_shapes.rs` exposes `outline_boxes(u32)` and
`interaction_boxes(u32)`, both `Option<&'static [BlockAabb]>`. An **empty slice is
a meaningful answer** — the state exists and cannot be targeted — and is distinct
from `None`, which means the id is not a state this version knows.

Version-free consumers reach both through
`VersionAdapter::{block_outline, block_interaction}`
(`crates/lodestone-model/src/adapter.rs`), beside `block_collision`.

## Gotchas

### 1. Two shapes are context-dependent and resolve to their default form

`getShape` takes a `CollisionContext`; the census passes
`CollisionContext.empty()` and `EmptyBlockGetter.INSTANCE`, which is exactly what
vanilla's own shape cache does (`getOcclusionShape` →
`state.getShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)`,
`BlockBehaviour.java:287-289`). Two knowable consequences:

- **`minecraft:light` outlines to nothing.** Its shape is
  `context.isHoldingItem(Items.LIGHT) ? Shapes.block() : Shapes.empty()`
  (`LightBlock.java:66-68`). The table's answer (empty) is the correct
  *not*-holding-a-light answer; a client wanting the held-light behaviour must
  special-case it above the table.
  `light_blocks_outline_to_nothing_because_the_census_holds_no_item` pins this so
  nobody "fixes" it to a cube. Note that `barrier` *is* a full cube with no
  context involved, and `structure_void` is a small centred cube
  (`[0.3125 … 0.6875]`) — so "invisible" is not the discriminator,
  `isHoldingItem` is.
- **`minecraft:scaffolding`** reports its standing rather than its descending
  shape.

### 2. Do not clamp to the unit cube

Outline coordinates span `-0.25..=1.25` (`pitcher_crop` reaches below zero).
`outline_boxes_escape_the_unit_cube` pins the extremes.

### 3. `BlockAabb` is `f32`, and four coordinates are not exactly representable

`f32` is lossless for every value the *collision* census uses. The outline census
uses 34 distinct coordinates, of which four — `0.3333333125`, `0.3958333125`,
`0.6041666875`, `0.6666666875`, all from `minecraft:lectern` and nothing else —
are rounded. The error is under `3e-9` blocks, i.e. ~5 nanometres of selection box
on a lectern. `f32_narrowing_is_lossless_except_for_the_lectern` pins both the set
of affected blocks and the bound, so a future version introducing a *materially*
inexact coordinate fails there rather than quietly losing precision.

`f32` is kept so all three shape seams share one box type; the alternative was a
second, `f64`, box type beside `BlockAabb`.

### 4. Blocks with an *empty* outline are real and must stay untargetable

`air`/`cave_air`/`void_air`, `water`, `lava`, `bubble_column`, `light`,
`moving_piston`, three `pitcher_crop` states, and — per wall block — the two
states with `up=false` and all four sides `NONE`, which fold to `Shapes.empty()`
(`WallBlock.java:75-86`; exactly two because that shape function ignores
`WATERLOGGED`).

## Evidence

`committed_tables_match_the_committed_dump` compares every box of both families
for all 32,366 states through the public accessors against the committed server
dump. `dump_block_boundaries_match_the_block_state_table` reconciles the whole
state→block-name mapping against the `blocks.json`-derived table — a second,
independent artifact.

The negative control for "just reuse the collision table" is
`outline_differs_from_collision_for_half_of_all_states`, which measures the
divergence against the committed collision table and pins it: **16,484 of 32,366
states differ**, **5,282** have empty collision with a real outline, and **0** have
real collision with an empty outline (which vanilla's
`hasCollision ? getShape() : empty()` default makes impossible — so that third
number is itself a check that the two tables were not swapped).

`crates/protocol/v770/tests/prototype_shape_seams.rs` covers the *seam* rather
than the tables — every call bound as `&dyn VersionAdapter` first, including
`outline_seam_is_not_the_collision_seam` (cobweb: full-cube outline, empty
collision) and a pointer-identity check that the seam hands back the version
table's own rodata slice for all 32,366 states in both families, so a swapped pair
of accessors in the `impl` cannot pass.

Individual shapes hand-derived from the decompiled source and cited in the tests:
`Block.column(sizeXZ, minY, maxY)` is
`box(8 - sizeXZ/2, minY, 8 - sizeXZ/2, 8 + sizeXZ/2, maxY, 8 + sizeXZ/2)` in
sixteenths (`Block.java:176-184`), so kelp's `column(16, 0, 9)` is
`[0, 0, 0, 1, 9/16, 1]` and seagrass's `column(12, 0, 12)` is
`[2/16, 0, 2/16, 14/16, 12/16, 14/16]` — both match the dump exactly.
`SlabBlock.java:35-36, 59-65` gives BOTTOM/TOP/DOUBLE as
`column(16,0,8)`/`column(16,8,16)`/`Shapes.block()`; `AirBlock.java:29-32` gives
air's empty outline.

## Configuration

| knob | where | effect |
| --- | --- | --- |
| `--protocol <n>` | `Config::protocol` | which version family's census is resolved |
| `LODESTONE_REGEN=1` | env var on the `#[ignore]`d `committed_tables_match_dump` | regenerates the tables instead of asserting against them |

## Dependencies

- `lodestone_model::BlockAabb` — the version-free box type, shared with the
  collision seam.
- `lodestone_model::VersionAdapter::{block_outline, block_interaction}` — the only
  route a version-free consumer has without naming `lodestone-v770`.
- `crates/protocol/v770/src/block_states.rs` — `block_name`, used as the
  cross-check artifact.
- `crates/protocol/v770/src/collision_shapes.rs` — read only by the divergence
  control, never as a fallback.

## Consumption status

**1. `is_pickable` is done (`196d385`).** `LiveCollision::is_pickable`
(`crates/lodestone-shell/src/collision.rs`) now reads
`self.version.block_outline(state)` through the private `outline_of` helper,
replacing the has-baked-quads proxy this section used to describe. The one
behaviour change flagged below landed with it: `minecraft:light` is now
**un**pickable when not holding a light item, matching vanilla — the proxy's
"no fluid ⇒ pickable" clause used to keep it targetable as a side effect of
having no baked model geometry.

**2. The selection box is still a unit cube, but not for lack of a shape or a
render hook — both exist now, and only the wiring between them is missing.**

- `LiveCollision::outline_boxes_at(x, y, z) -> Vec<Aabb>` (`collision.rs`) returns
  the real per-block outline boxes in world space — a half-height box on a slab,
  kelp's thin column, and so on.
- `RenderState::set_outline_shape_source` and the `OutlineShapeSource` it installs
  (`crates/lodestone-shell/src/gpu.rs`) exist for exactly this: `RenderState`
  samples `self.outline_shape.sample(block)` and hands the boxes to
  `OutlineRenderer::prepare`, which already accepts a real shape (an empty slice
  falls back to a unit cube — correct for the demo palette, which has no outline
  census).
- **Nothing calls `set_outline_shape_source`.** `grep -rn set_outline_shape_source
  crates` finds only its own definition. Until some startup path (the shell's
  `app.rs`, where the live `RenderState` and the live `CollisionSource` both
  already exist) installs a closure over `LiveCollision::outline_boxes_at`, the
  renderer keeps drawing `OutlineShapeSource::default()`'s unit cube, and every
  slab/stair/kelp selection box over-selects at its edges exactly as before.

The census itself, and the shape/render seam it needed, are no longer the gap —
the missing piece is a single call at startup. Until that call exists, the
selection-box half of this census still reaches zero pixels — see CLAUDE.md on
islands.
