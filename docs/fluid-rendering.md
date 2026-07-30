# Fluid rendering

## What it is

How a water or lava cell becomes quads: the vanilla-derived surface math
(`crates/lodestone-assets/src/fluid.rs`), the neighbourhood the mesher gathers for
it (`mesh_fluids` in `crates/lodestone-render/src/models.rs`), and — the part that
has already been wrong once and is the reason this doc exists — **which faces get
emitted at all**.

This is a different question from [fluid classification](./fluid-classification.md).
That doc answers "does this state carry water?". This one answers "given that it
does, what do we draw?".

## How it works

Vanilla does not render fluids through the block-model pipeline; their blockstate
models are empty. `net/minecraft/client/renderer/block/FluidRenderer.java` builds
the surface at mesh time from the cell's neighbourhood. (Note for anyone working
from older notes: in 26.2 the class is **`FluidRenderer`**, not
`LiquidBlockRenderer`, and the still/flow/overlay sprites plus the tint source live
on a `FluidModel` record.)

We split it in two:

- `lodestone_assets::fluid` owns the **math and the UV/winding layout** —
  `own_height = amount / 9`, `render_height`, `corner_height`'s weighted average,
  `flow_horizontal` (vanilla `FlowingFluid.getFlow`), the flow angle, and
  `bake_fluid`, which turns a resolved `FluidGeometry` into `BakedQuad`s. It is
  pure and knows nothing about the world.
- `mesh_fluids` owns the **neighbourhood**: it fills in the four corner heights,
  the flow vector and the `FaceSet` by querying a `FluidSectionView`
  (`fluid_at` / `occludes_at` / `light_at` / `fluid_sprites`). The live
  implementation is `SnapshotFluidView` in
  `crates/lodestone-shell/src/mesher.rs`, reading real state ids out of the
  3×3×3 section snapshot.

### Which face is emitted (read this before changing anything)

Straight from `FluidRenderer.tesselate`, and note how few of these are the same
predicate:

| face | vanilla condition |
|---|---|
| up | `!isNeighborSameFluid(self, above)` **and** `!isFaceOccludedByNeighbor(UP, min(corners), aboveState)` |
| down | `shouldRenderFace(…, DOWN, below)` **and** `!isFaceOccludedByNeighbor(DOWN, 0.8888889, belowState)` |
| sides | `shouldRenderFace(…, dir, neighbourFluid)` **and** `!isFaceOccludedByNeighbor(dir, max(h0, h1), neighbourState)` |

where `shouldRenderFace = !isNeighborSameFluid(self, neighbourFluid) && !isFaceOccludedBySelf(ownState, dir)`.

`isFaceOccludedByState` is the interesting one:

```java
VoxelShape occluder = state.getFaceOcclusionShape(direction.getOpposite());
if (occluder == Shapes.empty())  return false;
if (occluder == Shapes.block())  return direction != Direction.UP || height == 1.0F;
return Shapes.blockOccludes(Shapes.box(0,0,0, 1,height,1), occluder, direction);
```

Two consequences worth having in your head:

- For a **fully occluding** neighbour, a horizontal face is *always* culled
  (`direction != UP` short-circuits), regardless of the fluid's height. A pool
  walled in solid blocks emits **only** its top surface.
- The **up** face is *not* culled by a solid block above, because the fluid's
  corner heights are `8/9`, not `1.0`, so `height == 1.0F` is false. Water under
  stone still draws its surface into the `1/9`-block gap. Our `mesh_fluids` culls
  it — a known, deliberate divergence, listed under "Known gaps" below.

### Which texture

- **top**, level surface (`flow == [0, 0]`) → `*_still`.
- **top**, flowing → `*_flow`, with the UV quad rotated by
  `atan2(flow.z, flow.x) - π/2` and sampled at ±0.25 about the sprite centre.
- **bottom** → `*_still`.
- **sides** → `*_flow`, *always*. It is sampled over `u ∈ [0, 0.5]` and
  `v ∈ [(1 - h)/2, 0.5]` — one quarter of the sprite, magnified 2×, with the
  streaks running vertically. This is why a fluid side face reads as a waterfall:
  that is genuinely what vanilla draws there.
- **sides against a `HalfTransparentBlock` or `LeavesBlock`** → the fluid model's
  **overlay** material (`block/water_overlay`) instead, and the quad gets **no**
  back face (`addBackFace = !isOverlay`). We do not implement this yet; see
  "Known gaps".

## The shoreline bug (2026-07), and what it teaches

**Report:** on a live 26.2 server, water "shows the 'flowing down' effect on the
edges that touch non-water blocks which is weird and shouldnt happen".

**It was a culling bug, not a texture-choice bug.** The `*_flow` sprite on a side
face is correct vanilla behaviour; the defect was that the face existed at all.

The chain, from the symptom down:

1. A pond's bank is `grass_block`. Vanilla culls the water side face against it,
   because `GRASS_BLOCK`'s `BlockBehaviour.Properties` call neither
   `noOcclusion()` nor `noCollision()`, so `canOcclude` stays `true` and
   `initCache` gives it a full-block occlusion shape.
2. We reported `occludes == false` for it, so nothing was culled.
3. And it got worse than one stray face: `FluidRenderer.getHeight` returns `-1.0`
   for a *solid* non-fluid neighbour, which `addWeightedHeight` **drops from the
   average entirely**, whereas a non-solid one contributes `0.0` and drags the
   corner down. Reporting the bank as non-occluding therefore also **tilted** the
   rim of the surface and made `flow_horizontal` non-zero, so the *top* face
   switched to the animated `*_flow` sprite too. One wrong bit, three visible
   symptoms — which is why the report reads as "the flowing effect at the edges"
   rather than "an extra quad".

**Why `occludes` was false.** It was `is_full_cube(quads) && layer == Solid`, and
`grass_block[snowy=false]` breaks both halves:

- its model lays four `grass_block_side_overlay` decals over the six faces of a
  full cube, so it bakes **ten** quads and `is_full_cube`'s exactly-six test fails;
- that decal's sprite is binary alpha (measured over the real PNG: exactly
  `{0, 255}`, versus `grass_block_side`'s uniform `255`), and `block_layer` takes
  the *most transparent* sprite across all quads, so the whole block landed on
  `Cutout`.

Both halves are the same underlying mistake: **occlusion was treated as a property
of the block, derived from textures.** Vanilla derives it from neither. It is a
hand-set `Properties` flag, and it is Java — it appears in no data report, and
`blocks.json` carries only `definition` / `properties` / `states`.

**The fix** (`face_occlusion` in `crates/lodestone-render/src/block_models.rs`) is
to ask the question **per face**: a face occludes when some quad covers that whole
boundary square (`quad_is_full_face` — `cullface` equal to its own facing,
coplanar, spanning 1×1) *and* the sprite that quad samples is fully opaque.
`StateModel::occludes` becomes the AND of the six. Grass block's six boundary
faces are each covered by an opaque sprite, so it occludes; leaves and glass cover
all six with a cutout/translucent sprite, so they do not.

One block defeats that rule and needs the **hollow-shell veto**: `powder_snow` is
six thin shells (`[0,15.998,0]..[16,16,16]` and its five mirrors) drawn on *both*
sides with an opaque sprite. Its outward faces do sit on the boundary, but vanilla
marks it `noOcclusion()`, and the reason is detectable — a model that draws its own
interior is see-through from inside, so culling the block behind it opens a hole.
The tell is a quad whose facing is the *opposite* of its `cullface`; such a quad
vetoes occlusion on the face it lines.

### What was measured

- Over all 32,366 states of 26.2, the complete set whose occlusion changes is
  **`{grass_block[snowy=false]}`**, and **zero** states *lose* occlusion — so the
  change cannot open a hole anywhere. Without the hollow-shell veto the set is
  `{grass_block[snowy=false], powder_snow}`; the veto fires on exactly
  `powder_snow` and on no block that occluded under the old rule.
- An 8×8×8 pond with real `grass_block` banks, meshed through the real
  `mesh_fluids`: **0** side faces and 64 level top quads with the fix; **256** side
  faces and only 100 level top quads (the rest tilted) with the pre-fix rule.

### A belief that turned out false

"Vanilla is not colour-managed, and water is tinted, so this is probably a shade or
tint problem" and "we are probably picking the wrong sprite for side faces" were
both wrong, and both are the kind of wrong that survives review. The side sprite
*is* `*_flow`; the top sprite selection *is* right; `bake_fluid` matches
`FluidRenderer.tesselate` UV-for-UV. A synthetic pool walled in blocks that were
*told* to occlude meshed to exactly vanilla's 64 quads on the first try. The defect
was one layer down, in what `BlockModels` said about a block that is not water at
all — which is why the only test that could see it had to be built on the **real
jar's** `grass_block`, not on a hand-written view.

## How to change it

- **A fluid face is drawn where vanilla culls it, or vice versa** → this is almost
  never in `bake_fluid`. Start at `occludes_at` for the *neighbour*, then at the
  `emit` closure in `mesh_fluids`. Print the neighbour's `StateModel.face_occludes`
  before theorising.
- **Occlusion is wrong for a block** → `face_occlusion` in
  `crates/lodestone-render/src/block_models.rs`. Re-run the census idea from
  "What was measured": enumerate every state, diff old rule against new, and look
  at the whole flip set. A rule change that flips one intended block and 200 others
  is not a fix.
- **Gotcha: a fluid gate needs a real boundary.** A lone water block, or a pool
  with no bank, structurally cannot exercise this bug. The unit test
  `a_walled_pool_emits_only_its_level_top_surface` and the gate
  `a_grass_banked_pond_draws_no_flowing_side_faces` both build an 8×8 pond with
  walls for exactly that reason.
- **Gotcha: `occludes_at` is doing three of vanilla's jobs.** `mesh_fluids` uses
  the one boolean for face culling (vanilla: `getFaceOcclusionShape`), for
  `neighbor_height`'s solid/air distinction (vanilla: `state.isSolid()`) and for
  `flow_horizontal`'s `blocks_motion` (vanilla: `state.blocksMotion()`). They agree
  for a plain solid cube and for air, which is why one boolean has been survivable,
  but they are three different predicates and a divergence will show up as a
  *sloped or animated surface*, not as a missing face.
- **Gotcha: there are two meshers.** `--headless` renders through `mesh_simple`,
  which has no fluid path at all. Anything about water must be verified through
  `mesh_fluids` / `mesh_snapshot_fluids`, which is what live terrain uses.

## Known gaps

Each is a real divergence from `FluidRenderer`, none of them the reported bug:

- **Partial occluders are not modelled.** `isFaceOccludedByState`'s third branch
  (`Shapes.blockOccludes(box(0,0,0,1,h,1), occluder, dir)`) needs real voxel
  shapes. A `dirt_path` or `farmland` bank occludes a `8/9`-high water face in
  vanilla (its occluder reaches `15/16`) and does not here, so those banks still
  draw a side face. Fixing it means giving `FluidSectionView` a directional,
  height-aware query and feeding it occlusion shapes — the per-face
  `StateModel::face_occludes` array is the first half of that seam.
- **The up face is culled by a solid block above.** Vanilla draws it (see above).
- **No overlay material.** Side faces against glass, ice, honey, slime or leaves
  should use `block/water_overlay` and omit the back face; we use `*_flow` with a
  back face.
- **No back faces.** `FluidRenderer.addFace` emits a reversed copy for side faces
  and, conditionally, for the top (`shouldRenderBackwardUpFace`). `bake_fluid`
  emits single quads, so a fluid surface seen from behind is invisible.
- **No `0.001` insets.** Vanilla nudges side faces inward and the top down by
  `0.001` to avoid z-fighting; `bake_fluid` leaves that to the mesher and the
  mesher does not do it.

## Configuration

None of its own. Needs the vanilla resource pack `BlockResources::load(true)`
resolves (`LODESTONE_ASSETS`, else the highest-sorting complete pack under
`.cache/mc/<ver>`). The jar-backed gates below additionally need
`generated/reports/blocks.json`; they are `#[ignore]`d and fail closed rather than
skipping.

## Dependencies

- `lodestone-assets` — `fluid::{bake_fluid, corner_height, flow_horizontal, …}`,
  `BlockBaker`, the stitched `Atlas` (fluid sprites are seeded explicitly, since no
  blockstate references them).
- `lodestone-render` — `BlockModels` (classification, per-face occlusion, sprite
  rects), `mesh_fluids`, `ModelPipeline::for_fluid`.
- `lodestone-shell` — `SnapshotFluidView` / `mesh_snapshot_fluids`, the live
  neighbourhood.

## Tests

Hermetic (`cargo test -p lodestone-render --lib`):

- `models::tests::a_walled_pool_emits_only_its_level_top_surface` — 0 side faces
  and a level 8×8 surface, with the pre-fix occlusion answer executed as the
  negative control and asserted to produce side faces and sloped rim quads.
- `models::tests::shared_face_between_two_water_cells_is_not_emitted`,
  `lone_water_source_emits_a_surface_below_the_full_block`.
- `crates/lodestone-assets/tests/fluid.rs` — the `bake_fluid` UV/winding layout
  against hand-derived `FluidRenderer` values.

Jar-backed, `#[ignore]`d:

- `crates/lodestone-render/tests/fluid_shoreline_gate.rs` —
  `a_grass_banked_pond_draws_no_flowing_side_faces`. Real `client.jar`, real
  `grass_block` and `water` state ids, real `mesh_fluids`; the pre-fix rule runs on
  the same scene as the executed negative control (256 side faces).
- `crates/lodestone-render/tests/block_models_gate.rs` —
  `occlusion_is_per_face_so_grass_block_occludes_despite_its_cutout_layer`, plus
  `oak_leaves` / `white_stained_glass` / `powder_snow` as the must-not-occlude
  controls.
- `crates/lodestone-render/tests/fluid_gate.rs` — the GPU translucency gate (the
  sea floor showing through water), unrelated to face selection.
