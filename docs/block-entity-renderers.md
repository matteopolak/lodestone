# Block entity renderers

## What it is

The render path for blocks whose visible geometry is not (fully) their own block model: chests, skulls, signs, banners, shields, bells, shulker boxes, lecterns, campfires, decorated pots, conduits, beacons, piston heads, spawners, vaults, end portals/gateways and similar. It also covers two adjacent families: block models drawn away from their own cell (falling blocks, piston heads, primed TNT) and flat ground-plate blocks (carpets, pressure plates, rails) whose flicker was a sampling issue.

## How it works

### Dispatch layers

| layer | crate/file | owns |
|---|---|---|
| geometry | `lodestone-assets::block_entity_models` | cuboid rigs as `EntityModelDef`s (`CubeDef`/`PartDef`/`bake_entity_parts`, the baker entity models use) |
| renderer | `lodestone-render::block_entity` (facade over `model_families/`, `model_set/`, `batching.rs`) | placement matrices, per-part pose overrides (lid, lock, flag, bell body), material-to-sheet resolution, batching key |
| GPU | `lodestone-shell::gpu::block_entities` (+ `gpu::moving_blocks`, `gpu::world_items`) | pipeline, mesh upload, texture bind groups |
| source | `lodestone-shell::block_entities::{snapshot,scanner,render_inputs}` | world state to typed `*Spawn`/`*Source`, per-tick animation clocks |

A `Sim::*_source()` is captured fresh every frame (never installed once, or every lid/flag/pattern freezes at one partial tick) and installed on `RenderState`; `prepare_block_entities` resolves it into instances for `EntityPipeline`, which is generic over `(model, texture)`, so most new types need only a resolver arm. `model_families` owns constants, placement/animation math and baking helpers, `model_set` the baked corpus, spawn inputs and resolvers, `batching` texture identity, frustum culling and draw batches; the `lodestone_render::block_entity` API is unchanged.

The shell source facade splits by data lifetime: `snapshot` does the shared camera-scoped chunk read (validated state ids plus packed light), `scanner` world-backed candidate/debug scans, `render_inputs` converts the snapshot to typed spawns without touching the world. The facade re-exports them under `crate::block_entities::*`.

### Sparse world membership

`World::block_entity_chunks` walks a maintained set of positions with decoded records and yields borrowed `(ChunkPos, &LoadedChunk)`; all production render and animation gathers use it (state/light snapshot, NBT item families, structure outlines), so empty columns cost nothing. Only membership is indexed; records, states, NBT, light and neighbour checks stay live reads.

`World::load`, `unload`, `set_block_entity` and `sync_block_entity` maintain the set. `World::get_mut` returns a `LoadedChunkMut` guard that reconciles membership on drop (bind it `mut`; pass `&mut guard` where `&mut LoadedChunk` is expected; drop before querying the world again). The position is admitted before the borrow is lent, so a forgotten guard cannot hide a new record; the iterator filters the conservative empty entry. Add index maintenance for every new record mutation path. `chest_candidates` stays for fixtures selecting chunk subsets.

The frame snapshot's `BlockEntityScanCounts` (`loaded_chunks`, `candidate_chunks`, `records_visited`) describe the shared snapshot, reported once per second with frame profiling (browser stream included).

Snapshots carry raw state numbers (imported or protocol-local values can be outside the census). Resolvers validate at ingress and keep `lodestone_data::block_states::StateId`; the lectern source is the reference shape (out-of-census candidates are skipped before `lectern_spawn`).

### Which seam a type uses

The discriminator is whether the reference renderer owns a baked model layer. If not, the type poses existing models:
- item-model pipeline via `prepare_item_geometry` (campfire, brushable block, shelf);
- `gpu/moving_blocks.rs` `MovingBlock { state_id, transform, light }` (piston head, falling blocks, primed TNT; one shared mesh, one draw call);
- real terrain-meshed block geometry with a nested display (mob/trial spawner reusing the mob `EntityPipeline`; vault reusing the dropped-item pipeline; copper golem statue is the inverse: block geometry around a cuboid rig on the ordinary batcher);
- a dedicated procedural pass with no baked mesh (beacon beam, end portal/gateway star field; the gateway's teleport beam reuses that shader with a second texture bind group).

Chests and skulls have zero-element block models (a hole in the world before this existed). Signs are the opposite: the board is a real block model and the renderer is text-only; porting geometry would draw a second board. The sign-text pass caches vertices by position, content, light and outline, keeps only signs drawn that frame, and bounds retained geometry by `gpu::sign_text::MAX_SIGN_TEXT_VERTICES` (change it together with the near-first selection).

Placement is not one convention: corner-anchored with a pivot (`block_entity_placement_matrix`, chest and ground skull), centre-pivot (shulker, decorated pot with its extra 180 degree term), and plain `T * R * S` for origin-anchored skeletons (banner). A wrong choice gives a plausible half-turn or upside-down pose a screenshot misses. A block pose anchors at its cell corner and an entity pose at the cell centre in x/z; mixing shifts by half a cell.

### Banners, shields, pots

The reference draws the same mesh once per layer, not a composited texture: a base-colour mask tinted by the base dye, then up to 16 pattern masks tinted by their own dyes in stored order. `lodestone_render::banner_pattern` returns that ordered draw list (pure, GPU-free, no `lodestone-game` dependency). The 16+1 layers share one flag mesh and need an ordered, unbatched, alpha-blended list on top of the batcher (translucent depth-write-off draws are order-dependent). A shield uses the same function with one `plate`+`handle` mesh, always an item, and the pass tints the whole mesh rather than one part.

A decorated pot's four sides are distinct opaque textures on distinct quads: four ordinary instances. `block_entities::decorated_pot_sherds` reads `sherds` NBT as a compound of optional `back`, `left`, `right`, `front` stacks (the `id` selects the pattern; `count` and `components` do not) and also the older four-string list in `[back, left, right, front]` order; missing, invalid and `minecraft:brick` use the plain side; keep both forms in one order. Its top and bottom are zero-height planes in a double-sided pass, each emitting only its outward face (both at one depth flickers the rim).

Generalizing gotchas: a schema field such as an item model's `"transformation"` can be declared on an ancestor and read on the leaf, so model it as an accumulated root-to-node chain, not an `Option` on one variant (measured: 14 of 91 `special` nodes and 16 of 2131 `model` leaves rely on inheritance; the drop looks like a texture bug). Tint and mask multiply in gamma space; never pre-convert to linear. Exact composited bytes through alpha blending vary by backend, so bracket them.

### Moving block models

The dispatcher is deliberately tiny: any producer builds a `MovingBlock`; geometry comes from the crack pass's per-state baked-quad snapshot (any state resolves, no table to go stale); `cullface` is ignored and shading is the flat per-face directional constant, never a model's own `gui_light`. A moving piston emits two requests (head and base) from one push and only the head is offset (one transform makes it swallow itself). Its progress clock is client-simulated from one wire value seeded once, and an absent tracked position means "not moving", never `0.0` (the most-displaced state).

### Ground plates

Carpets, snow layers, pressure plates, rails, lily pads, leaf litter and redstone dust are ordinary baked block-model geometry. Some are degenerate (`from.y == to.y`, coincident up/down faces) and back-face culling resolves them. Reported "z-fighting" measured as not depth precision (separation clears to about 100 blocks; a coplanar control flips wholesale by draw order). The mechanism was cutout alpha under minification: the reference supersamples cutout sprites rather than a bilinear tap, which under-paints a minified cutout surface by about 60% at grazing angles, compounded by each sprite's `.png.mcmeta` mip-coverage strategy being parsed but not threaded through the atlas builder (45 of 102 block sprites built a wrong mip chain). Check the sampler and per-sprite mip strategy first, never a depth bias.

### Cauldrons

Cauldron models combine an opaque body with an inset, separately textured, partly alpha liquid. The mesher tags `cauldron`/`water_cauldron`/`lava_cauldron` and excludes them from whole-model translucent routing so the cutout depth test keeps the body in front. Add a state only when one baked model mixes an opaque enclosure with an internal partly-alpha surface; stained glass and ice still need the sorted depth-write-off pass.

## How to change it

**An invisible-until-touched block is a missing server-side record, not a draw bug.** The server only creates records for the dozen types it simulates. A purely visual type (skull, banner, decorated pot) needs a record synthesized at load too, or a saved chunk loads with the right state and an empty block-entity list and only draws after interaction triggers client synthesis. Every block-state write (decoded packet, predicted placement, anything) must call `World::sync_block_entity` (create, keep, replace or remove depending on what the new state owns).

**Scene-state traps**: query the placed state back rather than inferring it from the command. Facing can decide which face the camera sees, so a block can look broken from one angle by design. A waterlogged flag controls a translucent overlay independent of the renderer. Large-chest pairing derives from `facing` plus a clockwise/counterclockwise rule, and a wrong axis orphans one half silently. A name-keyed NBT schema is unsafe across types reusing a field name for a different purpose (an `Age` that is ticks-alive on one type and breeding age on another): exclude a field only because decode did not consume it.

### Adding a type

For a cuboid rig: a `*_model()` builder plus an entry in `BLOCK_ENTITY_MODELS` (`lodestone-assets`); a texture-stem resolver in `lodestone-render::block_entity` added to the combined preload list (skip it and every instance draws each frame with no bind group); a `*Spawn` input and `resolve_*` on `BlockEntityModelSet`; a gather arm in `shell::block_entities` and a prepare arm in `gpu.rs`. If the reference has no baked layer, route through the item-model or moving-block seam.

- Part names (`"lid"`, `"lock"`, `"flag"`, `"bell_body"`) are the only handle for animation overrides; a rename freezes the animation while the mesh still draws.
- Batch keys are `(model, texture stem)`, not model name, or a mesh shared across materials draws in one material.
- No fifth bind group: the shaders are at wgpu's 4-bind-group floor, so per-draw data goes in an existing group or the vertices.
- The GUI/held-item path reuses the vertices with a different placement and texture binding (items sample the stitched atlas), so check both consumers.

## Configuration

Nothing user-facing. Ported constants (view cutoffs, animation rates, sheet sizes) keep their number. Ground-plate sampling reads `mipmapLevels` (rebuilds the atlas at a new mip depth) and each sprite's `.png.mcmeta` `texture` section; a pack can change downsampling without changing the base texture.

## Dependencies

`lodestone-assets` (`entity::{CubeDef, PartDef, EntityModelDef, PartPose, Affine, bake_entity_parts}`, `block_entity_models`, atlas/mipmap machinery); `lodestone-render` (`EntityPipeline`, `block_entity`, `banner_pattern`, `mesh_moving_block_quads`, `CrackResolver::state_quads`, `block_models`/`model_pipeline`); `lodestone-world` (`BlockEntity`, `LoadedChunk::block_entities`, `World::sync_block_entity`, `sign_text`); `lodestone-data` (`block_states`, `block_entity_types`, `light_props`); `lodestone-shell` (`gpu::{block_entities, moving_blocks, world_items, sources}`, `block_entities.rs`, `resources`, `mesher.rs`). Related: [`entity-rendering.md`](./entity-rendering.md), [`gpu-module-layout.md`](./gpu-module-layout.md), [`gui-item-rendering.md`](./gui-item-rendering.md).
