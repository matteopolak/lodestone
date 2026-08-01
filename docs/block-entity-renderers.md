# Block entity renderers

**Issue:** [#23](https://github.com/matteopolak/lodestone/issues/23) — still open; chest is
landed, the other eleven types are not.

## What it is

The cuboid rigs vanilla's `BlockEntityRenderer`s draw for blocks whose **block model does not
describe them**. Today: chests (single, double left, double right; every material; the lid
animation).

This is not a nice-to-have layer over an existing box. A 26.2 chest has **no block model at all** —
`assets/minecraft/blockstates/chest.json` points at `block/chest`, and that file is verbatim:

```json
{ "textures": { "particle": "minecraft:block/oak_planks" } }
```

Zero elements. Every visible triangle of a chest comes from `ChestRenderer`, so before this landed a
chest was a **hole in the world**, and no terrain metric could see it: `sections_drawn`,
`total_quads` and every pre-existing pixel gate are byte-identical with and without chests drawing.
That is why chest was first rather than sign.

**The converse is the trap, and it is easy to get backwards from memory: a 26.2 sign *is* a real
block model.** `blockstates/oak_sign.json` maps all 16 `rotation` values to `block/oak_sign_rot_N`
models with genuine geometry, and `StandingSignRenderer`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/StandingSignRenderer.java`)
declares **no model whatsoever** — only text transformations. So there is deliberately no sign
*geometry* here; porting one would draw a second board inside the one the terrain mesher already
produces. Sign block entities are a **text pass**, and that pass is not built (see
[What is not built](#what-is-not-built)).

## How it works

Four layers, version-free until the last:

| layer | file | what it owns |
|---|---|---|
| geometry | `crates/lodestone-assets/src/block_entity_models.rs` | the ported `EntityModelDef`s |
| renderer | `crates/lodestone-render/src/block_entity.rs` | placement, lid pose, material→sheet, batching |
| GPU | `crates/lodestone-shell/src/gpu/block_entities.rs` | pipeline, meshes, texture bind groups |
| source | `crates/lodestone-shell/src/block_entities.rs` | world → `ChestSpawn`, and the lid clock |

### The consumer chain, end to end

Two links already existed and reached **nothing**. Both are marked; if you are extending this, they
are the shape of failure to expect.

```
level_chunk_with_light ─► BlockEntity::decode_list  ─► LoadedChunk.block_entities
block_update           ─► World::sync_block_entity  ─┤       ▲
section_blocks_update  ─► World::sync_block_entity  ─┤       │
block_entity_data      ─► World::set_block_entity   ─┘       │
                                                    was DEAD: zero shell call sites
                                                             │
                       shell/block_entities.rs::chest_spawns ┘
                        (chest_candidates ─► chest_spawn)
                                    │
BLOCK_EVENT ─► v770 adapter ─► ClientEvent::BlockEvent
                                    ▲ was DEAD: fell through net.rs `forward`'s
                                    │ terminal `_ =>` arm — decoded-but-stranded
                       net.rs NetUpdate::BlockEvent
                                    │
                       sim.rs poll_net ─► Sim::chest_lids  (ticked in Sim::step)
                                    │
                       Sim::block_entity_source()  ── installed every frame ──┐
                                                                              ▼
                              app.rs ─► RenderState::set_block_entity_source
                                                                              │
                    gpu.rs::prepare_block_entities ─► plan_block_entities ────┘
                                    │
                    gpu.rs render_inner, inside the block pass ─► draw_indexed
```

**`ingest::handles_event` needed no new arm.** Checked rather than assumed, because that switch is
this repo's island factory: `SharedState::apply` forwards only ECS-handled events, and block events
travel the shell's own `ClientEvent` stream, so the ECS routing switch is not on this path at all.
The same holds for #374 below: `sync_block_entity` is a `WorldSink` call inside the adapter, not an
event, so none of the three routers is involved.

### There are **four** creation routes, not two — issue [#374](https://github.com/matteopolak/lodestone/issues/374)

The first version of that diagram listed only `level_chunk_with_light` and `block_entity_data`. Both
links were accurate, and the pair read as exhaustive. It was not, and the gap was visible in play: a
**freshly placed chest was invisible** while still opening.

In vanilla, **writing a block state is what creates the block entity** — no packet involved
(`LevelChunk.java:341`, `blockEntity = ((EntityBlock)newBlock).newBlockEntity(pos, state)`), and
`block_entity_data` is only ever *data for an entity that already exists* (its handler
`ClientPacketListener.java:1476` calls `getBlockEntity(pos, type)` and **drops** the payload when
nothing is there). Our `block_update` / `section_blocks_update` arms wrote the state and stopped, so
a placed chest had a state, no record, and `chest_candidates`' `for be in &chunk.block_entities` loop
never saw it. Interaction kept working the whole time because it resolves from the block state, which
is exactly why the bug reads as "the renderer is broken" and is not.

The fix is `World::sync_block_entity(x, y, z, Option<block_entity_type>)` in `lodestone-world`, a port
of `LevelChunk.setBlockState`'s tail, called immediately after every state write:

| new state | existing record | outcome |
|---|---|---|
| owns type `T` | none | **create** with `T` and `Nbt::End` |
| owns type `T` | type `T` | **keep**, NBT included (`isValidBlockState`) |
| owns type `T` | type `U ≠ T` | **replace**, NBT cleared ("Found mismatched block entity") |
| owns nothing | any | **remove** |

**The removal row matters as much as the creation row.** Without it, breaking a chest leaves a stale
record and this pass keeps drawing a chest in empty air — the same defect pointing the other way.

`lodestone-world` cannot resolve a state id itself (it would need `lodestone-data`, and
`lodestone-data → lodestone-model → lodestone-world` makes that a cycle), so the **caller** passes
the type and the version-specific answer comes from `lodestone_data::block_entity_types` — a census
walked out of the real jar, because neither `blocks.json` nor `registries.json` carries the
state→type pairing. See [`lodestone-data-crate.md`](lodestone-data-crate.md).

`block_entity_data` still creates on a miss, deliberately unlike vanilla: vanilla can afford to drop
because it has `pendingBlockEntities` to promote from later and we do not, and the two failure modes
are not symmetric. An orphan record whose block state is not a chest resolves to no material in
`chest_spawn` and draws **nothing**, so creating is inert; dropping would lose server data we cannot
ask for again.

### Geometry, and the one difference from entity models

The bake is shared with entities verbatim — `CubeDef` / `PartDef` / `bake_entity_parts`, and
`entity::push_part_quads`' winding rule (made `pub(crate)` rather than copied: a chest whose winding
disagreed with the mobs beside it has exactly the armour-layer failure mode that function's doc
describes). **Placement is the difference, and it is total:**

| | entity | block entity |
|---|---|---|
| model space | Y-**down** | Y-**up** |
| placement | `entity_model_matrix`: `translate(feet) · rotY(180°−yaw) · scale(−s,−s,s) · translate(0,−1.501,0)` | `block_entity_placement_matrix`: `translate(pos) · rotateAround(−yaw, ½,0,½)` |
| anchor | the entity's feet | the block's corner |

`ChestRenderer.submit`'s *entire* prologue is one
`Matrix4f().rotationAround(Axis.YP.rotationDegrees(-facing.toYRot()), 0.5F, 0.0F, 0.5F)` — no flip
and no lift, because the chest's texels are already block-space: `bottom` spans y `0..10` texels and
the `lid` pivot at y `9` puts the closed lid's top at `14/16`, the real chest height. Feeding a chest
through the entity matrix buries it 1.5 blocks down, upside down.

`det(placement) == +1` for every facing (translation ∘ rotation, no handedness flip), which is *why*
the winding rule transfers unchanged. It is **measured**, not asserted from "rotations are positive"
— see `placement_preserves_orientation`.

### The lid

Vanilla applies **three** transforms, in three different classes. Collapsing any pair of them is
right at the endpoints and wrong everywhere between.

1. `ChestLidController.tickLid()` — ramps `openness` by **±0.1 per tick**, clamped `0..=1`, so a lid
   takes exactly 10 ticks. Ported in `shell/block_entities.rs::ChestLids::tick`.
2. `ChestLidController.getOpenness(a)` — `lerp(a, oOpenness, openness)`, the partial-tick
   interpolation. `ChestLids::openness`.
3. `ChestRenderer.submit` eases the progress (`open = 1-open; open = 1-open³`, a cubic ease-out),
   then `ChestModel.setupAnim` turns it into an angle (`lid.xRot = -(open·π/2)`, and
   `lock.xRot = lid.xRot`). `chest_lid_openness` and `chest_lid_x_rot` — deliberately two functions.

`lid` and `lock` are **siblings** sharing pivot `offset(0, 9, 1)`, not parent and child; nesting the
lock composes the pivot twice and puts it 9 texels too high.

### Sheets

Keyed by **texture stem**, not model name. A trapped chest shares the single-chest *mesh* and differs
only in bind group, so the batch key is `(model, texture)`; keying textures by model — as
`EntityRenderer::textures` correctly does, because a mob's sheet *is* determined by its model — draws
every trapped chest in plain oak.

22 stems: 7 materials × 3 halves, plus one half-independent ender sheet. `chest_texture_stems()`
derives that list from the *same match* the renderer resolves through, so a material cannot be added
without its sheets.

**Ender is half-independent on purpose.** `Sheets.chooseSprite` returns the single
`ENDER_CHEST_LOCATION` for every `ChestType`, and the jar ships only `entity/chest/ender.png` — no
`ender_left`/`ender_right` exist. A uniform suffix rule names a missing file and the chest falls back
to nothing, which reads as a broken renderer rather than a missing texture.

26.2 stitches these into `textures/atlas/chest.png` and submits a `SpriteId`. We bind the individual
PNGs instead: each sprite **is** the whole 64×64 sheet, so the model's own UVs (normalised against
64×64 by the bake) address a direct upload identically, and the atlas would only add a UV remap.

## How to change it

### Adding a block-entity type

1. A `*_model()` builder plus a `BlockEntityModelEntry` in `BLOCK_ENTITY_MODELS`.
2. A texture-stem resolver in `render/block_entity.rs` and its entry in the preload list.
3. A `*Spawn` input struct and a `resolve_*` on `BlockEntityModelSet`.
4. A gather arm in `shell/block_entities.rs` and a prepare arm in `gpu.rs`.

You do **not** need a new pipeline. Everything draws through `EntityPipeline`, and the draw loop is
already generic over `(model, texture)`.

### Gotchas, each of which has a test holding it

- **`visible_faces` is indexed by `entity::FACE_ORDER` `[Down, Up, West, North, East, South]`**, not
  by `Direction`'s discriminant. Off-by-index deletes the chest's *front* face instead of its seam,
  which still passes any "does a chest draw" gate. Held by
  `double_halves_omit_exactly_the_seam_face`, which asserts *by direction*, not by quad count.
- **Part names are the animation's only handle.** `BlockEntityMesh::index_of` resolves `"lid"` and
  `"lock"` by name; renaming either silently freezes the lid shut — the mesh still draws, so a
  coverage-only gate stays green. Held by
  `lid_and_lock_share_the_pivot_the_animation_rotates_about`.
- **South is yaw `0`** (`Direction.toYRot()`), not `Direction`'s declaration order
  (down/up/north/south/west/east), which is a quarter-turn error on every chest in the world.
  Held by `facing_rotates_the_front_of_the_chest_to_the_named_side`, which locates the *latch* after
  rotation — a `+yaw`/`−yaw` swap passes every bounds and determinant assertion.
- **`Affine` → `Mat4` is a transpose.** `Affine::m[i][j]` is row `i`, column `j`;
  `Mat4::from_cols_array_2d` takes **columns**. Feeding rows in as columns gives the inverse
  rotation, which for a lid looks like it opening *into* the chest and is easy to misread as a sign
  error in `chest_lid_x_rot`.
- **The entity lift is `+1.501` in world space, not `−1.501`.** The flip is applied *after* the
  translate, so the negative comes out positive. `placement_does_not_flip_or_lift` failed on its
  first run for exactly this; it compares against the real `entity_model_matrix` rather than
  restating its expression.
- **Do not use `entity_anim::Skeleton`.** It animates by *slot* (head, limb table) and classifies a
  chest as `AnimFamily::Static` — i.e. a permanently shut lid. Block entities take direct per-part
  pose overrides, composed through `lodestone_assets::entity::Affine::of_pose` so the `rotationZYX`
  order cannot drift from the bake's.
- **Do not nest the world read lock.** `chest_spawns` calls `loaded_chunks()` *before* taking the
  guard and drops the guard before sampling light. `std::sync::RwLock` gives no re-entrancy
  guarantee — a nested read may deadlock once a writer is queued, which on this world happens every
  time a chunk packet lands. That failure mode appears under load and never in a test.
- **No fifth bind group.** `wgpu`'s default `max_bind_groups` is 4 and the model shader spends all
  four. This pass reuses `EntityPipeline` (two groups) and adds a second bind group over the
  *existing* group-0 layout, the same trick the first-person hand pass uses. A fifth group compiles
  on an 8-group M5 and crashes at startup for everyone at the floor.
- **The source must be re-installed every frame.** It captures this frame's partial tick and a
  snapshot of the lid map. A one-shot install at connect draws every lid frozen at the fraction of a
  tick the session happened to join on.
- **Every block-state write must call `World::sync_block_entity`.** #374 was one write path that
  did not, and the whole rule is that there are no exceptions — "this one cannot need it" is the
  reasoning that produced the bug. The one live gap left is `Sim::set_block_world`, the **demo-world**
  editor, which still writes bare block states; it is harmless today only because the only values it
  is ever passed are `PLACE_BLOCK` (stone), air and water, none of which own a block entity. Closing
  it is a one-line addition and should happen the moment that world can contain a chest.

### There are two consumers of this geometry, not one

The world pass documented here is the first. The **GUI item-icon path is the second**, and it is
issue [#369](https://github.com/matteopolak/lodestone/issues/369) — **landed for chest** in
`d683a29`. It used to draw nothing, because `IconPart::Special` — where vanilla's
`ChestSpecialRenderer` goes — was an empty match arm in
`crates/lodestone-shell/src/hud/item_icon.rs`, and `block_models.rs`'s
`collect_item_model_parts` filters every non-`Model` part out at bake time so no chest ever
enters `BlockModels::items`. It now routes through `special_icon_geometry` into a third
`EntityPipeline` pass; see [`gui-item-icons.md`](gui-item-icons.md) for the consumer chain and
the per-`kind` table. The assessment below is what that work was built on and was confirmed
correct in the doing, so it is kept in the indicative rather than rewritten.

Vanilla shares one `ChestModel` between the two: `ChestSpecialRenderer.Unbaked.bake` calls
`context.entityModelSet().bakeLayer(ChestRenderer.LAYERS.select(chestType))` — literally the same
layer definition the block-entity renderer bakes — with `openness` defaulting to `0.0`. So the
geometry here **is** the right source for both, and a change to the models must be checked against
both call sites. Do not assume a chest that looks right in the world looks right in the hand.

**What is and is not reusable, assessed rather than hoped:**

- **Reusable as-is: the vertices, indices and part hierarchy.** Both passes consume
  `ModelVertex::vertex_layout()`, so there is no re-bake and no repacking.
  `BlockEntityMesh::part_transforms(placement, overrides)` already takes an **arbitrary** placement
  matrix and is public — that is the seam. `gui_item_pose(rect, display.gui)` slots in exactly where
  `block_entity_placement_matrix` goes in the world, and vanilla applies the identical
  `ItemTransform` + `-0.5` centring composition. An item chest is `openness = 0`, so there are no
  lid overrides and no animation to drive.
- **Not reusable: the texture binding.** The chest's UVs are `[0,1]` against the standalone 64×64
  `entity/chest/normal.png`; the GUI item pass binds the **stitched block atlas**, which contains
  nothing under `textures/entity/`, and it spends **all four** bind groups
  (camera+origin / atlas / palette / anim). Routing a chest through `ModelIcons` would sample
  arbitrary block texels.

So #369 is **not** a re-bake and **not** a UV remap. Stitching the 22 chest stems into the block
atlas and remapping the baked UVs would give the chest two texture paths, fight mip-4 atlas
mipmapping on a 64×64 entity sheet, and need either 22 mesh variants or a per-draw UV offset the
model shader has no slot for. The cheap route is the one the world path already proves: draw it with
`EntityPipeline`, which spends only **2** bind groups, consumes the same vertex layout, is
double-sided (so the GUI pose's negative determinant is a non-issue) and is depth-tested/writing,
matching the depth attachment `IconRenderer::draw_models` already clears — recorded inside that
existing pass. That is the route taken, and it came in at **three** files plus a gate (no
`lodestone-assets` change, no `app.rs` change, no shader change), because the placement seam and
the `draw_models` pass were both already there to be reused. The same shape covers shulker boxes
the day their model lands.

Two fidelity caveats that route carries, now observed rather than predicted: the entity shader
lights from a fixed direction with derivative-reconstructed normals rather than `gui_light`'s
per-face constants, so a GUI chest is shaded like the world chest rather than like a model item —
measured as top/bottom band means of `90.3`/`101.0` in the gate, i.e. the horizontal face is
*dimmer* than the sides here, the opposite of a `gui_light: Side` model item. And
`IconPart::Special`'s flat-sprite `base` fallback stays unused, which turned out to be true of the
**whole family and not just chest**: all ten special `base` models ship no `elements` and no
`layer0`, only a *block* `particle` texture, so there was never a flat sprite to fall back to. See
[`gui-item-icons.md`](gui-item-icons.md#known-gaps).

One thing the assessment did **not** predict, and worth carrying to the next block-entity type:
`IconRenderer::draw_models`' early return was `if count == 0` on the *model* stream's vertex
count. A slot holding only a chest makes that zero, so the new pass would have been attached, fed
and never run — the same island one layer down. When you add the second special `kind`, check the
guard before the geometry.

## Configuration

Nothing user-facing. The values that matter are all ported constants:

| constant | value | source |
|---|---|---|
| `block_entities::VIEW_DISTANCE` | `64.0` blocks | `BlockEntityRenderer.getViewDistance()`, compared against `Vec3.atCenterOf(pos)` — the block **centre**, not its corner |
| `LID_SPEED` | `0.1` / tick | `ChestLidController.tickLid()` |
| chest sheet size | 64×64 | all three `ChestModel` layers' `LayerDefinition.create(mesh, 64, 64)` |
| `EXPECTED_SHEETS` (gate) | `22` | derived from `chest_texture_stems()` |

Sheets load from `client.jar` via `resources::load_block_entity_textures`, fail-open: no pack means
chests **draw nothing** rather than a synthetic placeholder. That asymmetry with mob sheets (which do
get a placeholder) is deliberate — a flat-magenta mob reads as "this sheet is missing", but a
flat-magenta chest-shaped box reads as a renderer bug. `RenderStats::block_entity_sheets_loaded` is
what distinguishes the two from outside.

## Proof

`crates/lodestone-shell/tests/chest_block_entity_pixels.rs` — three `#[ignore]`d GPU gates:

```bash
cargo test -p lodestone-shell --test chest_block_entity_pixels -- --ignored --nocapture
```

The expected rect is projected from the **real baked vertices** of the real corpus mesh, through the
*same* `Camera::view_projection` the render call uses and the *same* `part_transforms` the draw uses
— never a remembered literal. Failure output prints a **bounding box**, not a percentage.

Measured green:

| gate | measurement |
|---|---|
| chest draws | rect `x137..183 y98..144`; fill **89.6%**; changed bbox `x138..181 y98..142`, entirely inside |
| lid animates | band above the closed silhouette: closed **0 px**, open **1504 of 1504** |
| arm is elsewhere | arm bbox `x247..319 y169..239`, disjoint from the chest rect |

**The negative control was watched failing.** The island was simulated exactly — planning left
intact so `block_entities_drawn` stayed at `1`, and the mesh upload dropped, which is the precise
shape of this repo's eleven confirmed instances:

```
the chest fills only 0.0% of its own projected rect (0 of 2209 px).
Subject's non-sky bbox: Rect { x0: 247, y0: 169, x1: 319, y1: 239 }
an open lid painted only 0 px in the 1504 px band ... Changed bbox: None
```

Note what the first failure printed: the only thing painting was the **first-person bare arm**. That
is the false control `CLAUDE.md` records, and it is caught by construction —
`the_first_person_arm_is_somewhere_else` *locates* the arm and asserts it is disjoint from the chest
rect, so the sibling gates' clean-control premise is a measurement rather than a hope.

Both other gates assert their own premise before the thing they measure: the lid gate fails loudly
if the open lid does not project above the closed chest (which would make its pixel assertion vacuous
rather than failing), and the draw gate fails if the chest projects to under 900 px.

Unit tests: 6 in `lodestone-assets`, 17 in `lodestone-render`, 10 in `lodestone-shell`. Note that all
33 are a **closed loop** with respect to the shell pass — none of them calls
`prepare_block_entities`, so every one would stay green with the draw deleted. Only the pixel gates
can see that.

### #374: the creation half

`chest_block_entity_pixels.rs` hands `RenderState` a synthetic `ChestSpawn`, so it is silent about
where spawns come from and stayed green throughout #374.
`crates/lodestone-shell/tests/placed_chest_block_entity_pixels.rs` starts one layer earlier — a real
`World` with a real loaded chunk, written through the `WorldSink` seam (`set_block` then
`sync_block_entity`, the exact pair the adapter's `BLOCK_UPDATE` arm calls), then the **real** shell
gather (`chest_candidates` + `chest_spawn`), then the real `RenderState::render`:

```bash
cargo test -p lodestone-shell --test placed_chest_block_entity_pixels -- --ignored --nocapture
```

| frame | world write | measured |
|---|---|---|
| subject | `set_block(chest)` + `sync_block_entity(Some(1))` | rect `x137..183 y98..144`, fill **89.6%** (1980/2209) |
| pre-fix control | `set_block(chest)` **only** | **0 px** in that rect; 0 spawns gathered |
| removed | then `set_block(air)` + `sync_block_entity(None)` | **0 px**; pixel-identical to the never-had-a-chest frame |

The middle row is #374 reproduced verbatim as a permanent control — a *world state* rather than a
deleted line of code, so it cannot rot. Its changed bbox is `x138..181 y98..142`, entirely inside the
rect, and the arm sits at `x247..319 y169..239`, re-measured disjoint.

**The negative control was watched failing**, twice and at two layers:

- the pixel gate with its subject switched to the pre-fix write —
  `assertion left == right failed  left: 0  right: 1` on `block_entities_drawn`, with
  `subject_spawns = []`; and
- the two `sync_block_entity` calls temporarily deleted from `adapter.rs`, which fails **three** of
  `crates/protocol/v770/tests/block_updates.rs`' world-backed gates on real `BLOCK_UPDATE` /
  `SECTION_BLOCKS_UPDATE` packet bytes:
  `a placed chest must gain a block-entity record from the state alone ... left: []`.

Those v770 gates are what join the pixel gate to the wire: they dispatch real packet bytes into a real
`World` and assert the resulting records, in both directions and for the bulk path (whose
`section << 4 | rel` reconstruction has its own negative-coordinate gate, because getting it wrong
puts the record 16 blocks away, where it still exists and still fails to draw). Each asserts its state
write landed **before** asserting anything about block entities — every seam here is a documented
no-op for an absent chunk, so a fixture that forgot to load one would read as a broken feature.

Note that `a_repeated_block_update_keeps_the_nbt_block_entity_data_delivered` passes with or without
the fix: it guards the `Kept` branch (a re-sent chest state must not wipe contents `block_entity_data`
delivered — the server re-sends `block_update` for a chest whenever a neighbour makes it a double), not
#374 itself.

## What is not built

Eleven of the twelve types on #23, in the order the issue puts them:

- **Signs** — text only (the board is a block model). Needs: `SignText` NBT decode (`messages`,
  `color`, `has_glowing_text` per `SignText.DIRECT_CODEC`), the transforms from
  `StandingSignRenderer` (`RENDER_SCALE 0.6666667`, `TEXT_OFFSET (0, 0.33333334, 0.046666667)`,
  scale `±0.010416667`, the wall offset `(0, -0.3125, -0.4375)`, 16 `RotationSegment` steps, front
  and back), `MAX_TEXT_LINE_WIDTH 90` / `TEXT_LINE_HEIGHT 10`, and the dye rule
  (`ARGB.scaleRGB(color, 0.4)` normally; full `DyeColor.textColor` plus full-bright plus an outline
  when glowing, with `BLACK_TEXT_OUTLINE_COLOR = -988212` substituted for black). The substrate
  exists: `gpu/nametag.rs` already draws world-space text as coloured quads from a `RasterFont`,
  including its own two depth passes. Colour must multiply in **gamma** space.
- Beds, banners (layered patterns from the `banner_patterns` atlas), item frames, shulker boxes
  (`shulker_boxes` atlas, 16 dyes), the enchanting-table book, bells, conduits, end crystals,
  decorated pots (`decorated_pot` atlas).

Also unbuilt for chests specifically: the `BrightnessCombiner` that makes a double chest's two halves
share one light sample, and the `SpecialDates.isExtendedChristmas()` clock behind
`chest_material_with_season` (the function is ported and tested; nothing calls it with `true`).

## Dependencies

- `lodestone-assets` — `entity::{CubeDef, PartDef, EntityModelDef, PartPose, Affine, bake_entity_parts}`,
  `Image::decode_png`, `ResourceManager`/`ZipSource` for the jar.
- `lodestone-render` — `entity::{push_part_quads, PartRange}`, `entity_pipeline::{EntityPipeline,
  GpuEntityModel, EntityCameraUniform, upload_instances}`, `camera::Frustum`, `models::ModelVertex`.
- `lodestone-world` — `BlockEntity`, `LoadedChunk::block_entities`, `ChunkColumn::get_block`,
  `World::sync_block_entity` / `BlockEntitySync`.
- `lodestone-data` — `block_states::{block_name, properties}` for the material and the
  `facing`/`type` properties; `block_entity_types::block_entity_type` for the state→type census the
  block-update path creates records from.
- `lodestone-shell` — `net::{SharedHandle, entity_light_at}`, `resources::asset_root`.

## Related

- [`entity-rendering.md`](./entity-rendering.md) — the cuboid-rig machinery this reuses.
- [`gpu-module-layout.md`](./gpu-module-layout.md) — the bind-group budget and pass ordering.
- Vanilla reference: `.cache/mc/26.2/client-src/net/minecraft/client/{model/object/chest/ChestModel,
  renderer/blockentity/{ChestRenderer,BlockEntityRenderDispatcher,AbstractSignRenderer,
  StandingSignRenderer},renderer/Sheets}.java`.
