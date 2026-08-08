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

### The neighbourhood is resolved once, into a padded grid (issue #542)

`mesh_fluids` used to call back through `FluidSectionView` for **every** probe —
around fifty per fluid cell, each redoing three `split16`s, three range checks, a
snapshot-slot index and a `PalettedContainer::get` bit-unpack, re-resolving the
same neighbour many times within one cell and again from every adjacent cell. That
measured **13,709 instructions per fluid cell**, 58.8% of the whole client chunk
path ([client chunk cycles](./client-chunk-cycles.md), `DESIGN.md` §12.120).

It now resolves the neighbourhood **once**, into
`lodestone_render::fluid_grid::FluidGrid` — an 18³ array of 16-bit packed cells
(fluid kind, amount, `falling`, occludes, overlay), 11,664 bytes, L1-resident. The
grid spans `-1..=16` on each axis, which is exactly the mesher's reach. Three
things about it are load-bearing:

- **`FluidSectionView::cell_at` is the fill primitive**, answering all three of
  `fluid_at`/`occludes_at`/`overlay_at` in one call. Its default composes the three,
  so every existing implementor keeps working; `SnapshotFluidView` **overrides** it
  to share the single `get_block`. That override is not optional — without it a
  *fluid-free* section costs 2.9× what it did before the grid existed, measured.
- **`partial_occluder_y_range_at` is deliberately not in the grid** — two `f32`s do
  not pack, and it is consulted at most four times per *surface* cell. It stays a
  live call.
- **`mesh_fluids` is generic over the view, not `&dyn`.** Fifty calls per cell
  through a trait object inline nowhere. The `?Sized` bound keeps
  `mesh_fluids(&dyn FluidSectionView)` compiling.

Per-cell cost after that plus the biome-tint memo below: **6,629 instructions**,
2.07× lower, and the output is byte-identical — see
`crates/lodestone-render/tests/fluid_mesh_identity_gate.rs`.

### The largest single term was the biome table, not the neighbourhood

Issue #542's diagnosis named the fifty virtual calls. Measured, **46% of the
per-cell cost was `water_tint_at`**: vanilla's radius-2 biome blend is 25 samples
per cell, each resolving a biome *name* and then calling
`lodestone_assets::tint::biome_effects`, which is a **linear scan of 66
`(&str, BiomeEffects)` entries with a string compare per entry**. One
`water_tint_at` cost 6,263 instructions, of which **97.8% was that scan**.

`NamedBiomeTint` (`crates/lodestone-render/src/biome_tint.rs`) now carries a
four-entry memo keyed on the `&'static str`'s data pointer and length — four
because a radius-2 box can straddle a four-way biome junction and a one-entry memo
thrashes there. A pointer miss on an equal string is slow, never wrong.

**The root fix is still open and belongs in `lodestone-assets`:** `BIOME_EFFECTS`
is already alphabetically sorted, so `biome_effects` could be a `binary_search_by`
— six compares instead of ~33 — which would fix every caller rather than this one.
Beyond that, the 25 samples of adjacent cells overlap in 20 of 25 columns, so a
separable sliding-window sum over `blend_box` would be bit-exact (vanilla's
per-channel floor division happens once, at the end) and roughly 5× again. Neither
is done.

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
  corner heights are `8/9`, not `1.0`, so `height == 1.0F` is false almost
  always. Water under stone still draws its surface into the `1/9`-block gap.
  `mesh_fluids` matches this: a fully-occluding neighbour above only culls the
  top face when **every** corner height is already `1.0` (a same-fluid column
  one cell short of the ceiling) — see `up_occluded` in `mesh_fluids`. This is
  exact for the `Shapes.block()` fast path vanilla itself takes for a plain
  opaque cube; the partial-occluder `else` branch above it is not modelled (see
  "Known gaps", partial occluders).

### Which texture

- **top**, level surface (`flow == [0, 0]`) → `*_still`.
- **top**, flowing → `*_flow`, with the UV quad rotated by
  `atan2(flow.z, flow.x) - π/2` and sampled at ±0.25 about the sprite centre.
- **bottom** → `*_still`.
- **sides** → `*_flow` by default. It is sampled over `u ∈ [0, 0.5]` and
  `v ∈ [(1 - h)/2, 0.5]` — one quarter of the sprite, magnified 2×, with the
  streaks running vertically. This is why a fluid side face reads as a waterfall:
  that is genuinely what vanilla draws there.
- **sides against a `HalfTransparentBlock` or `LeavesBlock`** → the fluid
  model's **overlay** material (`block/water_overlay`) instead, and the quad
  gets **no** back face (`addBackFace = !isOverlay`). `bake_fluid` implements
  this (`FluidGeometry::side_overlay`, the `overlay: Option<SpriteUv>`
  parameter) and `mesh_fluids` wires it through `FluidSectionView::overlay_at`
  — but the live shell mesher doesn't override that method yet, so on a real
  server every neighbour still reads as "not overlay" until it does. See
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
- **…but if the stray face appears only along *chunk boundaries*, it is not a
  culling bug at all** → the neighbourhood the mesher was handed was incomplete,
  so `occludes_at`/`fluid_at` answered for air that had simply not arrived. That
  is issue #389, it lives in the snapshot rather than in `mesh_fluids`, and it has
  its own doc: [section mesh invalidation](./section-mesh-invalidation.md). The
  tell is that it heals when you walk towards it.
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
- **Gotcha: a probe outside `-1..=16` now silently reads air in release.** Every
  neighbourhood read goes through `FluidGrid::get`, which `debug_assert`s the range.
  If you add a probe that reaches two cells out, a debug build panics and a release
  build renders the wrong thing — so **run the new logic once in debug** before
  trusting a release measurement of it.
- **Gotcha: touching `mesh_fluids`'s output means regenerating a committed golden,
  and that is a decision, not a chore.** `fluid_mesh_identity_gate.rs` holds
  FNV-1a digests of 13 scenes' meshes, produced by the *pre-#542* implementation.
  Fluid rendering carries deliberate deviations from vanilla, so the gate cannot
  tell "more correct" from "different": if your change is meant to be cost-only and
  the gate fires, the change is wrong. Only regenerate
  (`LODESTONE_REGEN=1`) for a reviewed, intended output change, and say in the
  commit which scenes moved and why.
- **Gotcha: a new `FluidSectionView` implementor gets `cell_at` for free and it
  will be slow.** The default composes three probes. If your view resolves a block
  state to answer them, override `cell_at` — that is where the grid's cost lives.

## Known gaps

Issue #18 tracked five divergences from `FluidRenderer`, none of them the
reported shoreline bug. Re-verified 2026-08-04 against the same 26.2
`client-src` cited throughout this doc: **all five are now closed.** The
overlay material's live-shell hop landed in `385b4fee` (see below — this
section previously said "not yet reachable from a live server", which was
true when written but is stale now: `SnapshotFluidView::overlay_at` exists in
`crates/lodestone-shell/src/mesher.rs` today). Partial occluders — the one gap
this doc used to call "still open" — now has a scoped fix for the single-box,
full-footprint case (`dirt_path`, `farmland`, slabs, snow layers); the general
multi-box case (stairs, fences, walls) remains unmodelled and is now the
documented boundary of this feature rather than an open TODO.

- **Closed — the up face is no longer culled by a solid block above.**
  `mesh_fluids`'s `up_occluded` now matches `isFaceOccludedByState`'s
  `Shapes.block()` fast path exactly: `direction != Direction.UP || height ==
  1.0F` (`FluidRenderer.java:38`) only culls the top face when every corner
  height is already `1.0`, which needs a same-fluid column stacked one cell
  short of the neighbour — never true for an ordinary source surrounded by air,
  whose corners sit at `8/9`. Water under stone now draws its surface into the
  `1/9` gap, matching vanilla. What's *not* ported is the `else` branch below
  the fast path (a partial-shape neighbour occluding the top face) — folded
  into the partial-occluders gap below, since it needs the same voxel-shape
  machinery.
- **Closed — back faces.** `bake_fluid` now emits `FluidRenderer.addFace`'s
  reversed-winding copy: unconditionally for every side face unless it's using
  the overlay material (`addBackFace = !isOverlay`,
  `FluidRenderer.java:310-318`), and for the top face when
  `FluidState.shouldRenderBackwardUpFace` says so
  (`FluidState.java:65-77`) — reproduced in `mesh_fluids` as
  `should_render_backward_up_face`, a 3×3 ring check at the cell directly above
  the fluid. One approximation: vanilla's ring test is `!isSame(fluidType) &&
  !isSolidRender()`; this reads `!isSolidRender()` off the existing
  `occludes_at` boolean rather than a separate solid-render query, which agrees
  for a plain opaque cube (the dominant case) and is the same approximation
  `flow_neighbor_at` already makes for `blocks_motion`/`isSolid`. Net visible
  effect: an ordinary open lake now draws its top surface **double-sided**
  (matches vanilla — the surface is visible from underwater looking up), and
  every open side face gets a reversed copy.
- **Closed — `0.001` z-fight insets.** `bake_fluid` now applies
  `FluidRenderer.java`'s `offs`/`bottomOffs`/side-inset constants itself: top
  corners pull down `0.001` whenever the top face draws (and that adjustment is
  visible to the side faces reading the same corner heights, exactly as in
  Java, where the mutation happens once before either reads it); side faces
  inset `0.001` off their block boundary; a side face's bottom edge — and the
  bottom face itself — sit at `y = 0.001` only when the bottom face is *also*
  drawn (`bottomOffs = renderDown ? 0.001F : 0.0F`), else flush at `y = 0`.
- **Closed, and now live.** Side faces against a vanilla `HalfTransparentBlock`
  or `LeavesBlock` (glass, every stained-glass colour, tinted glass, ice, blue
  ice, frosted ice, honey, slime, all eleven leaves types — scanned from
  `Blocks.java`, see `is_fluid_overlay_neighbor` in `block_models.rs`) bake
  against `block/water_overlay` with no back face, via `bake_fluid`'s
  `overlay: Option<SpriteUv>` parameter and `FluidGeometry::side_overlay`.
  `FluidSectionView` gained `overlay_at(x, y, z) -> bool` (default `false`, so
  every existing implementation keeps compiling and keeps its old behaviour).
  **This doc previously said the live shell hadn't overridden it** —
  `SnapshotFluidView::overlay_at` in `crates/lodestone-shell/src/mesher.rs`
  now exists (landed `385b4fee`, forwarding to `BlockModels::fluid_overlay`
  exactly as sketched here), so a real server's water now draws the overlay
  material against glass/leaves instead of `*_flow` with a stray back face.
- **Closed for the scoped case — partial occluders.**
  `isFaceOccludedByState`'s third branch (`Shapes.blockOccludes(box(0,0,0,1,h,1),
  occluder, dir)`, `FluidRenderer.java:44`) needed real voxel shapes: a
  `dirt_path` or `farmland` bank occludes an `8/9`-high water face in vanilla,
  and did not here, so those banks drew a spurious side face.

  **Correction to this doc's own previous citation**: it used to point at
  `lodestone-data`'s `collision_shapes` module as "exactly the missing
  geometry". That is the wrong shape family. Vanilla's
  `Block.getOcclusionShape` is `state.getShape(EmptyBlockGetter.INSTANCE,
  BlockPos.ZERO)` — the **outline** getter
  (`crates/lodestone-data/src/outline_shapes.rs`'s own module docs quote this
  exact call, and independently: `BlockBehaviour.java`'s three shape getters
  disagree at their defaults, and 50.9% of 26.2's 32,366 states have an
  outline that differs from their collision shape). `collision_shapes` answers
  a genuinely different question (can an entity stand on it) and would have
  been silently wrong for any state where the two diverge. The correct source
  is `lodestone_data::outline_shapes::outline_boxes(state_id)`.

  Read `Shapes.blockOccludes` directly
  (`.cache/mc/26.2/client-src/net/minecraft/world/phys/shapes/Shapes.java`) —
  it is not the general voxel-grid slice-and-compare this doc previously
  guessed at from the method's shape, it is short enough to derive exactly:

  ```java
  public static boolean blockOccludes(shape, occluder, direction) {
     if (shape == block() && occluder == block()) return true;
     if (occluder.isEmpty()) return false;
     axis = direction.getAxis();
     first  = direction.getAxisDirection() == POSITIVE ? shape : occluder;
     second = direction.getAxisDirection() == POSITIVE ? occluder : shape;
     op = direction.getAxisDirection() == POSITIVE ? ONLY_FIRST : ONLY_SECOND;
     return fuzzyEquals(first.max(axis), 1.0) && fuzzyEquals(second.min(axis), 0.0)
         && !joinIsNotEmpty(sliceAt(first, axis, far), sliceAt(second, axis, near), op);
  }
  ```

  For a **single box spanning the full `x`/`z` footprint of its cell**
  (`dirt_path`, `farmland`, slabs, snow layers — "flat, height-only-reduced"
  shapes), every slice along a horizontal axis has the same cross-section, so
  this collapses to a pure height comparison: the neighbour occludes the
  fluid's side face (at test height `h = max` of the two corners on that edge)
  iff the occluder's own `y`-range satisfies `min_y <= 0` **and**
  `max_y >= h`. A box that starts above `y = 0` (a raised platform) or stops
  short of `h` leaves a gap and does not occlude — both are executed negative
  controls, not just described (see "Tests").

  **Landed in this pass**, split the way the doc's own dependency boundary
  requires (`lodestone-render`/`lodestone-assets` cannot depend on
  `lodestone-data` — see "Dependencies"):

  - `lodestone_assets::fluid::full_footprint_y_range(&[BlockAabb]) -> Option<(f32, f32)>`
    — the pure shape reduction: `None` unless there is exactly one box and it
    spans the full `0..=1` extent on `x` and `z`, else `Some((min_y, max_y))`.
  - `FluidSectionView::partial_occluder_y_range_at(x, y, z) -> Option<(f32, f32)>`
    (default `None`, same compatibility shape as `overlay_at`) and `mesh_fluids`
    now calls it for all four horizontal directions, culling when
    `min_y <= 0 && max_y >= max(corner_a, corner_b)` on top of the existing
    `occludes_at` boolean.

  **This IS live.** `SnapshotFluidView` in
  `crates/lodestone-shell/src/mesher.rs` implements it, and this doc said "not yet
  live" long after it landed — verified 2026-08-07 while working #542. The body is
  exactly the patch this section used to propose:
  ```rust
  fn partial_occluder_y_range_at(&self, x: i32, y: i32, z: i32) -> Option<(f32, f32)> {
      let (dx, lx) = split16(x);
      let (dy, ly) = split16(y);
      let (dz, lz) = split16(z);
      if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
          return None;
      }
      let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
      let boxes = lodestone_data::outline_shapes::outline_boxes(id)?;
      lodestone_assets::fluid::full_footprint_y_range(boxes)
  }
  ```
  `mesher.rs` already depends on `lodestone_data` (it calls
  `lodestone_data::shade_brightness::occludes_ambient_light` a few dozen lines
  above `SnapshotFluidView`), so it needed no new dependency, only the method.
  `path_bank` in `fluid_mesh_identity_gate.rs` is the scene that exercises it, and
  the gate asserts a non-zero partial-occluder census so it cannot go vacuous.

  **Still open — the general multi-box case.** A shape with holes or a partial
  footprint (stairs, fences, walls, panes) needs the real voxel-grid
  slice-and-compare (`VoxelShape.getFaceShape`, `VoxelShape.java:197`, and
  `Shapes.joinIsNotEmpty`'s boolean-grid merge over both shapes' full
  coordinate lists, not just their boundary box), which is a materially bigger
  port than the scoped closed-form above. `full_footprint_y_range` returns
  `None` for these (falling back to today's boolean `occludes_at`), which is
  the same honestly-scoped boundary this doc drew before landing the scoped
  case — left undone rather than shipped half-verified.

## Configuration

None of its own. Needs the vanilla resource pack `BlockResources::load(true)`
resolves (`LODESTONE_ASSETS`, else the highest-sorting complete pack under
`.cache/mc/<ver>`). The jar-backed gates below additionally need
`generated/reports/blocks.json`; they are `#[ignore]`d and fail closed rather than
skipping.

## Dependencies

- `lodestone-assets` — `fluid::{bake_fluid, corner_height, flow_horizontal,
  full_footprint_y_range, …}`, `BlockBaker`, the stitched `Atlas` (fluid
  sprites, and `block/water_overlay`, are seeded explicitly, since no
  blockstate references them). `full_footprint_y_range` takes
  `&[lodestone_model::BlockAabb]` directly rather than depending on
  `lodestone-data`, so the crate boundary below still holds — the shell passes
  the boxes in.
- `lodestone-render` — `BlockModels` (classification, per-face occlusion,
  sprite rects, and `fluid_overlay(state_id)` — the
  `HalfTransparentBlock`/`LeavesBlock` name-list classification), `mesh_fluids`,
  `ModelPipeline::for_fluid`, `FluidSectionView::partial_occluder_y_range_at`.
- `lodestone-shell` — `SnapshotFluidView` / `mesh_snapshot_fluids`, the live
  neighbourhood. Implements `FluidSectionView::overlay_at` (landed `385b4fee`),
  `partial_occluder_y_range_at`, and — since #542 — `cell_at`, the `FluidGrid`
  fill primitive that shares one `get_block` across all three probes.
- `lodestone-data` — `outline_shapes::outline_boxes`, the real per-state
  jar-dumped **outline** geometry the partial-occluders fix needs. **Not**
  `collision_shapes` — see "Known gaps" for why the two disagree for about
  half of all 26.2 states and why that distinction matters here.

## Tests

Output identity, and the thing to run after **any** change to `mesh_fluids`
(`#[ignore]`d — needs `client.jar`):

- `crates/lodestone-render/tests/fluid_mesh_identity_gate.rs` — 13 scenes of real
  vanilla state ids (fully submerged, water-only surface, submerged air pocket,
  every `level` value, grass/glass/`dirt_path` banks, lava beside water,
  waterlogged, dry, single-cell puddles at both section corners), each meshed and
  digested against a golden produced by the **pre-#542** implementation. Carries two
  executed controls: an off-by-one-padding view (every out-of-section probe reads
  air) that must change 12 of the 13 scenes, and an FNV-1a single-bit-flip check.
  It also meshes every scene through *both* `cell_at` shapes — the default
  three-probe composition and the shared-state override production installs — and
  requires them to agree.

Hermetic (`cargo test -p lodestone-render --lib`):

- `fluid_grid::tests::the_shell_fill_is_bounded_by_the_fluid_bounding_box` — counts
  probes: exactly `4096 + 19` for a one-cell puddle, against a whole-shell fill's
  `4096 + 1736`. A count, not an inequality.
- `fluid_grid::tests::packing_round_trips_every_reachable_fluid_cell` — every
  `(kind, amount 1..=8, falling, occludes, overlay)` survives the 16-bit pack. The
  one way the grid could silently move a surface is by losing a bit of `amount`.
- `biome_tint::tests::the_effects_memo_reaches_the_table_once_per_blend_box` and
  `..._is_transparent_under_eviction` — the memo hits (25 samples leave exactly one
  entry) and stays correct when eight biomes cycle through its four slots.

- `models::tests::a_walled_pool_emits_only_its_level_top_surface` — 0 side faces
  and a level 8×8 surface (now 128 quads: every top quad is double-sided, see
  "Known gaps"), with the pre-fix occlusion answer executed as the
  negative control and asserted to produce side faces and sloped rim quads.
- `models::tests::shared_face_between_two_water_cells_is_not_emitted`,
  `lone_water_source_emits_a_surface_below_the_full_block`.
- `models::tests::water_under_a_solid_ceiling_still_draws_its_top_surface` —
  the up-face-culling fix, with the pre-fix whole-occludes rule checked
  (not just described) as the executed negative control.
- `models::tests::side_face_against_an_overlay_neighbor_uses_the_overlay_sprite_and_has_no_back_face`,
  `overlay_flag_without_an_overlay_material_falls_back_to_flow_with_a_back_face`
  — the overlay-material wiring through `mesh_fluids`/`bake_fluid`, and that a
  `None` overlay sprite (lava) restores the back face even if `overlay_at`
  reports true.
- `crates/lodestone-assets/tests/fluid.rs` — the `bake_fluid` UV/winding layout
  against hand-derived `FluidRenderer` values, plus the `0.001` inset
  (including the "no top face drawn → side reads the *un*-inset height"
  interaction), back-face winding (top and side), and overlay-sprite selection
  cases. Its `full_footprint_y_range` submodule covers a `dirt_path`-shaped
  single full-footprint box, rejection of multi-box shapes, rejection of a
  partial `x` or `z` footprint (a lone box that doesn't reach the boundary,
  the stairs/panes case), and that a genuine full cube still qualifies.
- `models::tests::a_tall_full_footprint_bank_culls_the_side_face_it_fully_covers`,
  `a_short_full_footprint_bank_does_not_cull_the_side_face`,
  `a_raised_full_footprint_occluder_does_not_cull_the_side_face` — the
  partial-occluder wiring through `mesh_fluids`, with the height and
  near-boundary conditions each checked as a magnitude (a bank just short of
  the fluid's real corner height, and one that starts above `y = 0`), not
  merely "an occluder is present".

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
