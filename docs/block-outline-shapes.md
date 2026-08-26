# Block outline and interaction shapes

## What it is

Per-block-state `BlockStateBase.getShape` (the **outline**) and
`BlockStateBase.getInteractionShape`, for protocol 776 (Minecraft 26.2), dumped
from the real 26.2 server and committed as generated tables.

The outline shape is what **block selection** uses. It is a third thing, neither
the collision shape ([`collision_shapes`](../crates/lodestone-data/src/collision_shapes.rs))
nor fluid presence — 50.9% of the 32,366 block states have an outline that
differs from their collision shape.

## Why it is needed

Two things depended on not having it:

1. **The kelp fix** (`13a1d3a`) used *"has baked model quads"* as a proxy for
   "non-empty outline shape" in `LiveCollision::is_pickable`
   (`crates/lodestone-shell/src/collision.rs`). Structurally sound — fluids are
   the one thing vanilla does not draw through the model pipeline — but a proxy,
   and the doc comment there says so.
2. **Selection boxes were a full unit cube for everything**, including slabs,
   stairs and kelp. Only **3,328 of 32,366** states are actually a full cube.
3. **So was the pick ray**, for longer — and invisibly, because it kept working
   *after* the drawn box was fixed (issue #375; see Consumption status §3).

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

`crates/lodestone-data/oracle-java/OutlineShapeOracle.java` boots the real 26.2
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

Committed at `crates/lodestone-data/tests/support/outline_shape_jvm.txt`.

### Regenerating

```bash
CACHE="$(cd .cache/mc/26.2 && pwd)"
HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
  CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
  cp /oracle/OutlineShapeOracle.java /work/ && javac -cp "$CP" -d /work /work/OutlineShapeOracle.java
  java -cp "/work:$CP" OutlineShapeOracle'
# copy stdout over tests/support/outline_shape_jvm.txt, keeping the `#` header, then:
LODESTONE_REGEN=1 cargo test -p lodestone-v770 --test outline_shapes \
    committed_tables_match_dump -- --ignored --nocapture
```

### The tables and the lookup

`crates/lodestone-data/src/generated/outline_shapes.rs`, same memory design as the
collision table: pure rodata, zero heap, O(1) by id. **860 distinct outline
shapes** and **8 distinct interaction shapes** over 32,366 states, each family a
`[u16; 32_366]` index (~63 KiB) into a de-duplicated shape table.

`crates/lodestone-data/src/outline_shapes.rs` exposes `outline_boxes(u32)` and
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

## Rendering: line thickness (issue #364)

This section is about `OutlineRenderer` in `crates/lodestone-shell/src/gpu/outline.rs`
— the GPU pass that draws the wireframe box from the boxes this module computes. It
used to draw each of the box's 12 edges as `PrimitiveTopology::LineList`, which
rasterizes at exactly **one physical pixel** regardless of resolution or DPI scale.
Reported live as "too dim to read comfortably" (issue #364); confirmed by pixel gate
to be a thickness problem, not a colour one — see below.

### What vanilla actually does, and what was mis-cited

The issue's first draft cited `LevelRenderer.java:744-756`, which is the F3-style
debug collision/occlusion/interaction shape dump (`SharedConstants.DEBUG_SHAPES`
branch of `submitHitOutline`) — colours at alpha `0.4` for black/green/blue. That is
**not** the block hit outline; it only draws with `F3+B` debug shapes enabled.

The real path is `LevelRenderer.submitBlockOutline` (`:706-729`) →
`submitHitOutline`'s **non-debug** branch (`:760`):
`submitNodeCollector.submitShapeOutline(poseStack, state.shape(), renderType, color, width, afterTerrain)`,
called with `color = ARGB.black(102)` (alpha ≈ 0.4) and
`width = gameRenderState().windowRenderState.appropriateLineWidth`, which is
`Window.getAppropriateLineWidth()` (`Window.java:569`):
`max(2.5, windowWidth / 1920 * 2.5)` — never thinner than **2.5 logical pixels**,
and scaling up with window/framebuffer width. That width is attached per-vertex via
`VertexConsumer.setLineWidth` (`ShapeOutlineFeatureRenderer.java:25-26`, the vertex
format element used by the `LINES` render pipeline,
`RenderPipelines.java:565` / `DefaultVertexFormat.POSITION_COLOR_NORMAL_LINE_WIDTH`)
and expanded into real screen-space quad geometry downstream — vanilla does not rely
on a GPU line-width parameter either, for the same portability reason noted in the
issue.

**Colour was already right.** Vanilla's real outline draws at alpha ≈ 0.4; this
pass's shader was already at 0.6 — more opaque, not less. Left unchanged.

**Depth setup needs one renderer-specific addition.** Vanilla's `LINES` pipeline
(`RenderPipelines.java:565`, the one the hit outline actually uses — not the
`LINES_DEPTH_BIAS` variant at `:572`) uses `DepthStencilState.DEFAULT`:
`GREATER_THAN_OR_EQUAL`, **zero** bias. Per `CLAUDE.md`'s reversed-Z note, that is
this engine's `LessEqual`. The screen-space ribbon and terrain mesh do not share a
vertex-generation path, however, so the inclusive predicate still ties at oblique
angles. `OutlineRenderer` keeps its `PAD = 0.002` world-space expansion and also
uses the shared camera-facing polygon bias (`slope = -1`, `constant = -10`): it
resolves a depth-buffer tie in ULPs rather than increasing `PAD` until the outline
visibly detaches from the block.

### The fix: screen-space-thickened quads

`OutlineRenderer` now submits each edge as **6 vertices (2 triangles)** rather than
a `LineList` segment. Every vertex carries its own position, the edge's *other*
endpoint, and a `side` (`-1.0`/`+1.0`). The vertex shader transforms both endpoints
to screen space, finds the on-screen perpendicular to the edge, and pushes the
vertex out along it by `half_width_px * side` — depth is preserved exactly (only
x/y move) so it still depth-tests like the original thin line. `half_width_px`
comes from a uniform written in `prepare`, using the same
`max(2.5, viewport_width / 1920 * 2.5)` formula as vanilla, driven by the render
target's real pixel size (`self.depth.width`/`height` in `gpu.rs`, passed in as
`prepare`'s new `viewport_px` argument).

### How to change it

- Line width lives in `outline.rs`'s `MIN_LINE_WIDTH_PX` / `LINE_WIDTH_REFERENCE_PX`
  constants — change both together if vanilla's own constant ever moves.
- The screen-space-expansion vertex shader is the whole trick; if you need a
  different visual weight, change `half_width_px` in `prepare`, not the shader.
- Gotcha: `viewport_px` must be the *render target's* pixel size, not a logical/DPI
  size — `gpu.rs` sources it from `self.depth.width`/`height`, which is set at both
  `RenderState::new` and `RenderState::resize`, so it always matches the surface
  actually being drawn to.

### Evidence

`crates/lodestone-shell/src/gpu.rs`'s inline `block_outline_draws_visible_edges`
(not, despite an earlier note, a file under `crates/lodestone-render/tests/`) proves
the outline changes pixels at all — it passes on both the old and new geometry and
so cannot distinguish "thin" from "thick".
`crates/lodestone-shell/tests/block_outline_thickness_pixels.rs` is the gate that
can: it isolates edge *thickness* in pixels from edge *length* (a naive "longest
changed run in any row" is dominated by the horizontal top/bottom edges, whose runs
span the whole box), by scanning a band of rows/columns clear of the perpendicular
edges. Measured on the pre-fix `LineList` code (executed via an isolated git
worktree checked out to the pre-fix `outline.rs`, same scene/camera/resolution as
the fixed build): thickness **2px both axes**, changed-pixel count **160**, bbox
`x149..170 y109..130` — the gate's own `>= 3` assertion **fails** against this
build, which is the executed proof the gate can actually detect "visible but too
thin" rather than merely "visible". After the fix: thickness **3px both axes**,
changed-pixel count **268**, bbox `x148..171 y108..131` — same target block and
camera, so the larger bbox/count is the outline's own line width extending outward,
not a different scene.

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
- `crates/lodestone-data/src/block_states.rs` — `block_name`, used as the
  cross-check artifact.
- `crates/lodestone-data/src/collision_shapes.rs` — read only by the divergence
  control, never as a fallback.

## Consumption status

All three consumers are wired. This section used to list two of them as gaps; the
history is kept because each one failed in a different way.

**1. Pickability (`196d385`).** `LiveCollision::outline_of`
(`crates/lodestone-shell/src/collision.rs`) reads
`self.version.block_outline(state)`, replacing the has-baked-quads proxy that
predated the census. One behaviour change landed with it: `minecraft:light` is
now **un**pickable when not holding a light item, matching vanilla — the proxy's
"no fluid ⇒ pickable" clause used to keep it targetable as a side effect of
having no baked model geometry.

**2. The selection box (`app.rs::session::WindowApp::install_outline_source`).**
`Sim::outline_shape_source` (`sim.rs`) hands `RenderState::set_outline_shape_source`
a `'static` closure over `VersionAdapter::block_outline`, and
`OutlineRenderer::prepare` draws the boxes it returns. An empty result still
falls back to a unit cube, which is correct for the demo palette — it has no
outline census and is all full cubes.

**3. The pick ray (issue #375).** This was the subtle one, and it is worth
recording *why* it survived §2 landing: the drawn box was right while the hit
test was wrong, which is the most convincing possible way for a bug to hide.
`raycast` took a per-**cell** `is_solid(x, y, z) -> bool`, so every pickable
block was a unit cube to the hit test regardless of what the census said. Leaf
litter — `1/16` of a block tall — stayed highlighted and stayed punchable with
the crosshair plainly above it, which is how it was reported, from play.

`crate::raycast::raycast` now takes `pick_boxes(x, y, z, &mut Vec<PickBox>)` and
clips the ray against each box with the same slab test `ray_aabb` uses
(vanilla's `AABB.clip`), nearest entry wins:

- `LiveCollision::pick_boxes` emits the census boxes, block-local; the demo
  adapter's emits one cube per pickable cell, which is exact for that palette;
- **the hit face comes from the box, not from the cell boundary** the DDA
  crossed. This is not cosmetic: placement is face-driven, so before the fix a
  ray angled down onto leaf litter reported the cell's south face and placed the
  block one cell *south* of the litter instead of on top of it;
- `RayHit` gained the exact entry point and distance. The cursor
  `use_item_on` carries is now `hit − blockPos` (vanilla's
  `writeBlockHitResult`) instead of the struck cube face's centre, and the
  entity-pick reach clamp uses the real entry distance instead of re-clipping a
  unit cube around the hit block;
- an empty box list still means unpickable, and there is deliberately no cube
  fallback for it — that is the right answer for air, water, lava and
  `minecraft:light`. A box the eye is already *inside* is skipped, matching
  `AABB.clip`, which is written in terms of face crossings.

The degraded tier is unchanged in kind: with no version census `outline_of` hands
back a full cube for anything with baked model geometry, so a version-free build
still targets blocks, just as coarsely as the whole client did before #375.
