# Moving block models

## What it is

The render path for a block's own geometry drawn somewhere other than its own cell —
vanilla's `SubmitNodeCollector.submitMovingBlock`. It is a **seam with more than one
intended producer**, not a falling-block feature: falling sand and gravel use it, and so
does `PistonHeadRenderer` — the piston head and the block it pushes.

## How it works

`crates/lodestone-shell/src/gpu/moving_blocks.rs` is the whole of it. A producer builds
`MovingBlock { state_id, transform, light }` and hands it to `merge_moving_block`; that
is the entire vocabulary.

```text
producer  →  MovingBlock  →  CrackResolver::state_quads(state_id)
                          →  mesh_moving_block_quads(quads, transform, light)
                          →  one shared ModelMesh  →  one GpuModelMesh  →  one draw call
```

### Why these renderers cannot use the entity pipeline

`FallingBlockRenderer` and `PistonHeadRenderer` share a shape the rest of the entity
pass does not have: **no `bakeLayer` call in the vanilla constructor.** They own no
cuboid mesh at all — they pose existing *block models*. So they cannot go through
`lodestone_render::EntityPipeline` however entity-shaped they look, and that is why the
piston head was left unbuilt when the block-entity renderers landed: the machinery it
needed is this file.

**That is also the cheap discriminator for any new block-entity renderer.** Ask whether
the vanilla constructor calls `bakeLayer`. If it does, it owns a mesh and needs a cuboid
rig in `lodestone-assets`. If it does not, it poses existing models, and it belongs either
here (block models) or on the item path (item models — the campfire).

### The two producers differ only in where their requests come from

| producer | input | source |
|---|---|---|
| `merge_falling_blocks` | the `&[EntityDraw]` slice `render` is already handed | a **network entity** |
| `merge_piston_heads` | `MovingPistonSource` | a **block entity**, polled like every other in `sources.rs` |

Neither touches a pipeline, a bind group or a draw call. Both end at
`merge_moving_block`.

`gpu/world_items.rs` (dropped items, held items, campfire items) is the **precedent**
for reaching the model pipeline from outside the terrain pass, not a place to put this.
An item model is posed by a `display` transform and lit full-bright or per-drop; a
moving block is posed in world space and lit from the world. Two different geometry
sources with two different rules, sharing a pipeline.

### The pipeline and the four bind groups

`lodestone_render::ModelPipeline` — the same one the terrain sections use, with the same
camera / atlas / palette / animation groups. That is **wgpu's four-group floor** and
there is no room for a fifth: a five-group shader validates on an adapter reporting 8
and fails on any 4-group adapter, which is a startup crash for other people and never
for us. If a producer needs more per-draw data, fold it into an existing group or into
the vertices.

Positions are baked in **world space**, so this binds the shared origin arena's reserved
zero slot and needs no camera write of its own — the same trick the item pass uses.

### Geometry source

`CrackResolver::state_quads(state_id)`. The crack pass already snapshots every state's
baked quads while `BlockModels` is still borrowable (the live renderer does not keep the
model set or its atlas resident past construction), so **any** block state resolves and
there is no per-block table here to rot. The type keeps its crack-flavoured name because
the crack pass is still its only owner; a reader looking for block geometry at draw time
should look there.

Empty quads means "draw nothing", not an error: air and every `RenderShape.INVISIBLE`
block legitimately bake no faces, which is `FallingBlockRenderer.submit`'s own
`getRenderShape() == RenderShape.MODEL` guard reached by a different route.

### Meshing

`lodestone_render::mesh_moving_block_quads`, a sibling of `mesh_item_quads`:

| | `mesh_models` (terrain) | `mesh_item_quads` (GUI) | `mesh_moving_block_quads` |
|---|---|---|---|
| `cullface` | honoured against neighbours | ignored | **ignored** |
| positions | offset by cell | transformed by pose | transformed by pose |
| shade | per-face + smooth AO | per-face, or flat for `gui_light: front` | **per-face, always** |
| light | sampled per corner | fixed `GUI_ITEM_LIGHT` | **one supplied byte** |

`cullface` is ignored because a moving block has no neighbours; a quad dropped because
"the block to the north occludes it" would leave a hole in mid-air. Shade is always the
per-face directional constant with no `GuiLight` branch, because `submitMovingBlock` goes
through the block renderer, which has no notion of a model's `gui_light` — flattening it
makes a falling sand block read as a uniformly-lit cube, which looks like a lighting bug.

Tint and shade multiply in **gamma** space, in the shader, exactly as terrain does: this
path shares the shader, so it inherits that for free. Do not pre-multiply anything here.

### One buffer, one draw call

Each request's placement is folded into its **vertex positions** by the transform, so
there is no per-instance matrix to batch on and no shared geometry between two different
block states. Every request concatenates into a single `GpuModelMesh` — one upload and
one draw per frame however many moving blocks exist.

## How to change it

### Adding a producer (this is the point)

Write a `merge_*` method beside `merge_falling_blocks` and call it from
`prepare_moving_blocks`, sharing the same `combined` buffer. There is a comment marking
the spot. You need no pipeline, no bind group, no buffer and no draw call.

`merge_piston_heads` is the worked example of a second producer, and the seam needed no
change to accept it — the added machinery was all on the *source* side: a `sources.rs`
struct (`MovingPistonSource`), a `gpu.rs` field, a `state.rs` setter, a
`block_entities.rs` gather, a per-tick clock, and one call site in `app/redraw.rs`.

### The piston head, and the two things about it worth knowing

`PistonHeadRenderer` is fully wired. Its own detail lives in
[`block-entity-renderers.md`](./block-entity-renderers.md); two facts belong here because
they are about this seam rather than about pistons:

* **A block entity's pose is at its cell corner, an entity's is at its centre in x/z.**
  So `piston_head_pose` has *no* `-0.5` shift where `falling_block_pose` must have one.
  Copying the falling-block pose slides every piston head half a cell diagonally, and it
  reads as a model-origin quirk. `the_piston_pose_is_not_the_falling_block_pose` is the
  gate that keeps the two apart.
* **A retracting source piston emits two requests, and only one is offset.** Vanilla's
  `submit` pops the translated pose before submitting the base, so the head slides and
  the base sits still. Folding both into one transform makes a sticky piston look like it
  is eating itself.

Culling uses a **two**-cell box around the block entity's cell, not the falling-block
producer's one-cell slack: the offset displaces geometry a full cell in either direction
along the push axis.

One thing to revisit if a producer draws a model with a random per-position offset:
`FallingBlockRenderer` passes `entity.getStartPos()` as `randomSeedPos` so the model does
not shimmer as it falls. Nothing here applies a random model offset at all, so there is
no observable difference for the three states that fall today.

### Changing the falling-block pose

`falling_block_pose` is its own function and gated, because it is the single most likely
thing here to be wrong in a way a screenshot does not obviously show. `submit` is
`poseStack.translate(-0.5, 0.0, -0.5)` on top of the entity's own pose, and **both** the
presence of the `x`/`z` shift and the *absence* of a `y` shift are load-bearing:

* `x`/`z`: the entity is at the block centre (`FallingBlockEntity.fall` spawns at
  `pos.getX() + 0.5`) and the quads are block-local `0..1`, so the `-0.5`s put local
  `(0,0,0)` back at the cell's corner.
* `y`: an entity's position is already its feet. Shifting it floats every falling block
  half a block high — the plausible symmetric mistake, and it reads as a model-origin
  quirk rather than a bug.

The gate evaluates both wrong poses and requires each to miss the correct one by more
than 0.4 blocks, so it is known to discriminate.

### The light probe

`FallingBlockRenderer.extractRenderState` reads light at
`BlockPos.containing(entity.getX(), entity.getBoundingBox().maxY, entity.getZ())` — the
**top** of the hitbox (`0.98` above the feet), not the feet. That matters at the moment
of landing: a probe at the feet is inside the cell the block is about to occupy and reads
the light of a solid block.

This calls `EntityLightSource::sample` directly rather than
`entity_passes::entity_light`, and both differences are deliberate: that helper resolves
its probe height from the entity *type*'s eye height (wrong rule for a block, and
`falling_block` has no eye height), and it force-lights an entity whose fire flag is set —
which a falling block never wants, because `FallingBlockEntity.displayFireAnimation`
returns `false`.

## Configuration

None. One constant, `FALLING_BLOCK_HEIGHT = 0.98`, which is
`EntityTypes.FALLING_BLOCK`'s own hitbox height.

`RenderStats::moving_blocks_drawn` counts what this pass emitted. It has its own counter
because a moving block is the only thing on screen that is *block* geometry at a
non-block position — it reaches neither `entities_drawn` (no cuboid rig) nor
`sections_drawn` (not in a chunk mesh). Without it, a falling block that drew nothing and
one that drew correctly produce byte-identical stats, which is the island shape this
crate has paid for nine times.

## Dependencies

* `lodestone_render::mesh_moving_block_quads` — the mesh primitive, next to
  `mesh_item_quads`.
* `lodestone_render::CrackResolver::state_quads` — the per-state baked-quad snapshot.
* `lodestone_render::ModelPipeline`, via `gpu/terrain.rs`'s `ModelRenderer` — pipeline,
  atlas, palette, animation bind groups and the origin arena.
* `EntityDraw::block_state` (`lodestone-shell`'s `entities.rs`) — the falling-block
  producer's input; see [`falling-blocks.md`](./falling-blocks.md) for how it gets there.
* `gpu/sources.rs`'s `EntityLightSource` — the world light sample.
* `gpu/sources.rs`'s `MovingPistonSource` plus `crate::block_entities::{PistonMoves,
  moving_piston_spawns, moving_piston_seeds}` — the piston producer's input.
* `lodestone_data::block_states::state_id` — how the piston gather resolves the
  *synthesised* head states `PistonHeadRenderer` builds (see
  [`block-entity-renderers.md`](./block-entity-renderers.md)).
