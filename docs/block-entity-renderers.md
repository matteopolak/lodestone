# Block entity renderers

## What it is

The render path for blocks whose visible geometry is not (fully) described by their own block model —
chests, skulls, signs, banners, shields, bells, shulker boxes, lecterns, campfires, decorated pots,
conduits, beacons, piston heads, spawners, vaults, end portals/gateways, and related types — plus two
adjacent families that live in this same corner of the renderer: block models drawn away from their own
cell (falling sand, piston heads, primed TNT) and flat "ground-plate" blocks (carpets, pressure plates,
leaf litter, rails) whose flicker problems turned out to be a mipmap/sampling issue rather than a
geometry one.

## How it works

### The dispatch shape

Four layers, version-free until the last:

| layer | crate/file | owns |
|---|---|---|
| geometry | `lodestone-assets::block_entity_models` | ported `EntityModelDef`s (cuboid rigs), built from `CubeDef`/`PartDef`/`bake_entity_parts` — the same baker entity models use |
| renderer | `lodestone-render::block_entity` | placement matrices, per-part pose overrides (lid/lock/flag/bell body), material→sheet resolution, batching key |
| GPU | `lodestone-shell::gpu::block_entities` (+ `gpu::moving_blocks`, `gpu::world_items` for the two reuse families below) | pipeline selection, mesh upload, texture bind groups |
| source | `lodestone-shell::block_entities` | world state → typed `*Spawn`/`*Source`, plus any per-tick animation clock |

Per-frame flow mirrors the entity path elsewhere in this codebase (extract from world/ECS state →
build instance data → batch → draw): a `Sim::*_source()` is captured fresh every frame (never a
one-shot install — a stale source freezes every lid/flag/pattern at whatever partial-tick it was
captured on) and installed on `RenderState`; `prepare_block_entities` (or the moving-blocks/world-items
equivalents) resolves it into instances and hands them to `EntityPipeline`, which is generic over
`(model, texture)` — most new types need **no new pipeline**, only a new resolver arm.

World snapshots still carry raw state numbers because imported or protocol-local values can fall
outside the built-in census. Renderer-specific resolvers must validate those numbers at snapshot
ingress and retain `lodestone_data::block_states::StateId` while reading canonical names and
properties. The lectern source is the reference shape: an out-of-census candidate is skipped before
`lectern_spawn`, while a valid non-lectern remains a normal typed negative match.

Most types are a cuboid rig through `EntityPipeline`, batched with everything else in
`prepare_block_entities`. Several other seams exist, chosen by what the vanilla renderer actually
does — the cheap discriminator is **"does the vanilla constructor call `bakeLayer`?"**: if it does not,
the type poses *existing* models rather than owning a mesh. It then belongs either to the item-model
pipeline via `prepare_item_geometry` (campfire, brushable block, shelf — posing existing *item*
models), or to `gpu/moving_blocks.rs`'s shared `MovingBlock` seam (piston head, falling blocks, primed
TNT — posing existing *block* models drawn away from their own cell: any producer builds
`{state_id, transform, light}` and all converge on one shared mesh and one draw call). A third shape is
real block-model geometry the terrain mesher already draws, with a display nested inside it (mob/trial
spawner reusing the ordinary *mob* `EntityPipeline`; vault reusing the dropped-item pipeline) — no
`BlockEntityModelSet` work, no new GPU pass; copper golem statue is the inverse of this, real
block-model geometry around a statue that is itself a genuine cuboid rig on the ordinary batcher. Last,
a dedicated procedural pass with no baked mesh at all (beacon's light beam, end portal/gateway's
screen-space star field), whose own shader is reused by the end gateway's teleport beam via a second
texture bind group rather than a parallel pass.

Ordinary chests/skulls have *zero-element* block models — a total absence, so before this machinery
landed a chest was a hole in the world no terrain-drawn metric could see. Signs are the opposite trap:
their board **is** a real block model and the renderer is a **text-only** pass — porting sign geometry
here would draw a second board inside the one the mesher already produces.

**A block entity's placement is not one convention.** At least three coexist: corner-anchored with a
pivot (`block_entity_placement_matrix`, chest/skull-ground shape), centre-pivot (shulker, decorated
pot — which also carries vanilla's own extra 180° term), and plain `T * R * S` for geometry that is
already origin-anchored like an entity skeleton (banner). Copying the wrong one produces a plausible,
wrong pose — half-turn or upside-down — that a screenshot alone does not catch.

### Banners and shields (pattern rendering)

Vanilla does **not** composite pattern layers into one texture. It draws the *same* mesh once per
layer: a base-colour mask tinted by the base dye, then up to 16 pattern masks (each a stencil sprite,
tinted by that layer's own dye), in the stored order — `lodestone_render::banner_pattern` returns
exactly that ordered draw list and is pure, GPU-free colour/ordering logic with no dependency on
`lodestone-game`. A banner's 16+1 layers all reuse **one** mesh (the flag) — same geometry, only tint
and mask vary — so they need a small ordered, unbatched, alpha-blended draw list on top of the
ordinary `(model, texture)` batcher (translucent + depth-write-off draws are order-dependent and
cannot ride that batcher). A decorated pot's four sides are the opposite shape — distinct diffuse
textures on distinct quads, never blended — so despite the surface-level similarity ("multiple
textures on one instance") it needs no new mechanism at all, just four ordinary instances.

A shield reuses the same pattern-stack function with a different mesh/orientation: one mesh
(`plate`+`handle`), always an item (no shield block entity, no world-placement transform), and the
pattern pass re-tints the *whole* mesh rather than one named part the way a banner's masks paint only
its `"flag"` part.

Gotchas that generalize: a schema field (an item model's `"transformation"`) can be declared on a
base/ancestor record and read only on the leaf that needs it most — model it as an accumulated
root-to-node chain, not an `Option` on one variant, or every node that inherits rather than declares
its own transform silently drops it (measured against the real jar: 14 of 91 `special` nodes, 16 of
2131 `model` leaves, rely on inheritance). Such a drop is invisible at the draw site and looks like a
texture/blend bug when the affected geometry only differs from its sibling on one hidden face. Tint
and mask multiply in **gamma** space, matching every other tint in this codebase — never pre-convert to
linear before returning a packed colour. An exact composited byte through alpha blending cannot be
predicted from the textbook formula on every backend — bracket it (order-dependence, distance from
both anchors) rather than asserting one.

### Moving block models

A block's own model geometry, drawn at a transform rather than in its home cell — vanilla's
`submitMovingBlock`. It is a seam with several intended producers (falling sand/gravel, piston heads,
primed TNT), not a falling-block-only feature. The dispatcher (`gpu/moving_blocks.rs`) is intentionally
tiny: any producer builds `MovingBlock { state_id, transform, light }`; geometry comes from the crack
pass's own per-state baked-quad snapshot (so any state resolves, with no per-block table to go stale);
everything converges on one shared mesh upload and one draw call, `cullface` ignored (no neighbours to
occlude against) and shading always the flat per-face directional constant, never a model's own
`gui_light`.

A block-entity's pose anchors at its cell **corner**; an entity's pose anchors at its cell **centre** in
x/z — copying an entity-shaped pose function for a block-shaped producer (or vice versa) shifts
everything by half a cell, plausible-looking but wrong. A moving-piston head emits **two** separate
requests (head + base) from one push, and only the head's is offset — folding both into one transform
makes the piston look like it swallows itself. Its progress clock is client-simulated from a single
wire value seeded once, not re-derived every packet, and an *absent* tracked position must read as "not
currently moving" — never default it to `0.0`, which here is the most-displaced state, not the neutral
one.

### Ground plates

Thin, sub-block-height decals (carpets, snow layers, pressure plates, rails, lily pads, leaf litter,
redstone dust) are ordinary baked block-model geometry through the ordinary mesher and pipeline, not
special-cased anywhere. Some models are genuinely **degenerate** (`from.y == to.y`, a coincident
up/down face pair on one plane); back-face culling resolves that correctly and the two faces never
fight.

Reported "z-fighting"/flicker in this family measured out as **not** a depth-precision problem —
separation from the floor beneath clears comfortably to ~100 blocks, and even a fully coplanar control
plate doesn't speckle per-pixel, it flips wholesale by draw order (a stability question, not a
precision one). The real mechanism was **cutout alpha filtering under minification**: vanilla
supersamples cutout sprites (`sampleRGSS`/`sampleNearest`) rather than a plain bilinear tap, which
under-paints a minified cutout surface by roughly 60% at a grazing angle; a second bug compounded it —
each sprite's own `.png.mcmeta` can select a per-sprite mip-coverage strategy that was being parsed but
never threaded through the atlas builder, so 45 of 102 block sprites built their mip chain wrong.
Neither is fixable with a depth bias — a report that looks like z-fighting in this family wants the
sampler and per-sprite mip strategy checked first, not a bias tuned in.

### Cauldrons

Cauldron block models combine an opaque body/rim with an inset, separately-textured liquid surface.
Rather than a fluid-level renderer, this is a mesh-routing exception: the mesher tags
`cauldron`/`water_cauldron`/`lava_cauldron` and excludes them from whole-model translucent routing even
though the liquid sprite carries partial alpha, so the ordinary cutout depth test keeps the opaque body
in front of the inset liquid where they overlap. Add a state here only when one baked model genuinely
mixes an opaque enclosure with an internal partially-alpha surface — don't broadly reclassify
translucent blocks (stained glass, ice) this way; they still need the sorted, depth-write-off pass.

## How to change it

**The most important trap in this whole subsystem: a thing that is invisible until touched is a
missing server-side record, not a client draw bug.** The server only creates a block-entity record
for the ~12 types it actually simulates behaviour for (furnaces, hoppers, containers…). A type that
exists purely so the client can *draw* something — a skull, a banner, a decorated pot — needs a record
synthesized at **load** time too, not only at placement time; otherwise a saved chunk loads with the
correct block state and an empty block-entity list, and the block only starts drawing the moment a
player interacts with it (which triggers a client-side synthesis off the block state alone). Before
concluding a renderer is broken, check whether the record exists at all — every block-state write in
this codebase must call `World::sync_block_entity` (create/keep/replace/remove based on what the new
state owns vs. what the existing record is), and a write path that skips it reproduces this exact
symptom. This generalizes past load: any route that writes a block state — a decoded packet, a locally
predicted placement — needs the same call, and "this one surely doesn't need it" is exactly the
reasoning that has produced the bug before.

**Scene-state traps, all the same shape: before blaming the renderer, check whether the block/world
state you actually gave it is the state you assumed** — query the placed state back, don't infer it
from the command you issued. Rotation/facing can determine which face the camera sees, not just which
way a rig points, so a block can look broken (flat, untextured, wrong colour) from one angle and
correct from another, by design. A waterlogged (or similarly independent) state flag can control a
translucent overlay entirely separately from the block-entity renderer — an overlay that looks wrong
may mean the scene had a state you didn't intend, not that the render is broken. Multi-block pairing
(a large chest's left/right half) is derived from `facing` plus a clockwise/counterclockwise rule;
getting that axis wrong orphans one half silently, because each half still resolves as *a* valid
chest, just not paired with the one you expect. And a name-keyed NBT/metadata schema is unsafe across
types that reuse one field name for a different purpose and type (an `Age` that is ticks-alive on one
type, breeding-age on another) — exclude a field only because decode did not actually consume it,
never because its name matches a static table.

### Adding a block-entity type

For the common cuboid-rig case: a `*_model()` builder plus an entry in `BLOCK_ENTITY_MODELS`
(`lodestone-assets`); a texture-stem resolver in `lodestone-render::block_entity`, added to the
combined preload list (skip this and every instance draws every frame with no bind group); a `*Spawn`
input struct and a `resolve_*` on `BlockEntityModelSet`; a gather arm in `shell::block_entities` and a
prepare arm in `gpu.rs`. No new pipeline needed — `EntityPipeline`'s draw loop is already generic over
`(model, texture)`. If the vanilla renderer has no `bakeLayer` call, skip step one and route through
the item-model or moving-block seam instead (see above).

Other gotchas, each held by a real test in the crates named above: part names are the *only* handle an
animation override has (`"lid"`, `"lock"`, `"flag"`, `"bell_body"`) — a rename silently freezes the
animation while the mesh still draws, so a coverage-only gate stays green. Batch keys must be
`(model, texture)` keyed by **texture stem**, not model name, or a mesh shared across materials (a
chest reused by every wood type) draws every instance in one material. No fifth bind group — the
model/entity shaders are already at wgpu's 4-bind-group floor, so new per-draw data goes into an
existing group or the vertices, or it validates locally and crashes at startup on any adapter
reporting the floor. A per-frame world source (lid state, animation phase) must be re-captured every
frame, never installed once at connect, or every dependent animation freezes at that moment's partial
tick. And there are two consumers of a block-entity's mesh, not one: the in-world draw and the
GUI/held-item icon path, which reuses the same vertices through a different placement and a
*different* texture binding (items sample the stitched atlas, not a standalone entity sheet) — a mesh
change must be checked against both, since a type correct in the world can still be wrong in the hand.

## Configuration

Nothing here is user-facing. The values that matter are ported vanilla constants (view distance
cutoffs, animation tick rates, sheet dimensions) — keep the number when porting one and cite the
vanilla source class/method, not a "measured on such-and-such date" narrative. Ground-plate sampling
additionally reads `mipmapLevels` (rebuilds the block atlas at a new mip depth) and each sprite's own
`.png.mcmeta` `texture` section (per-sprite mip strategy) — a resource pack can change a sprite's
downsample behavior without touching its base texture.

## Dependencies

`lodestone-assets` (`entity::{CubeDef, PartDef, EntityModelDef, PartPose, Affine, bake_entity_parts}`,
shared verbatim with entity models; `block_entity_models`; atlas/mipmap machinery for ground plates);
`lodestone-render` (`entity_pipeline::EntityPipeline` and its instance/tint upload path, `block_entity`,
`banner_pattern`, `mesh_moving_block_quads`, `CrackResolver::state_quads`,
`block_models`/`model_pipeline`/`models.rs`); `lodestone-world` (`BlockEntity`,
`LoadedChunk::block_entities`, `World::sync_block_entity`, and for signs `sign_text::{SignText,
SignSide, SignDyeColor}`); `lodestone-data` (`block_states`, `block_entity_types` — the state→type
census the sync path creates records from — `light_props`); `lodestone-shell`
(`gpu::{block_entities, moving_blocks, world_items, sources}`, `block_entities.rs`'s gathers and
animation clocks, `resources`, `mesher.rs`).

## Related

[`entity-rendering.md`](./entity-rendering.md) (cuboid-rig/animation machinery this reuses),
[`gpu-module-layout.md`](./gpu-module-layout.md) (bind-group budget/pass ordering),
[`gui-item-rendering.md`](./gui-item-rendering.md) (the second consumer of this geometry, for GUI slots).
Vanilla reference
(26.2 decompiled source): `BlockEntityRenderers` is the registration list — read it directly rather
than trusting a summary, including this one's.
