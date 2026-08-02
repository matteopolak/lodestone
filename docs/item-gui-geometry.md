# Item GUI geometry (3-D block items in a slot)

## What it is

The **geometry half** of drawing a block item as an isometric mini-block in a
hotbar/inventory slot. It bakes every item whose inventory icon is a 3-D model
into quads at asset-load time, and provides the matrices that pose those quads
into a GUI slot.

It draws nothing itself. It produces:

* `BlockModels::item_quads(item)` / `BlockModels::item(item)` — baked
  `Vec<BakedQuad>` plus the item's `display.gui` transform and `gui_light`;
* `item_render::gui_item_pose(rect_px, transform)` — model space → GUI pixel space;
* `item_render::gui_ortho(w, h)` — GUI pixel space → clip space;
* `models::mesh_item_quads(quads, pose, gui_light)` — quads + pose → `ModelMesh`.

The output is byte-compatible `ModelVertex` geometry, so the GUI item pass is the
**existing** `ModelPipeline` with its existing four bind groups, a different
`view_proj`, and nothing else new. No second pipeline, no second shader, no
second atlas.

Measured on the real 26.2 `client.jar`: **752 of 1537** items yield a 3-D model
icon, and **all of them bake with zero missing sprites**.

## How it works

### The chain, end to end

```
assets/<ns>/items/<id>.json          (item definition, a selector tree)
  └─ ItemIconBuilder::icon_with(id, &GuiItemContext)
       └─ IconPart::Model { model, transform, gui_light }
            └─ ModelResolver::resolve(model)  ->  ResolvedModel
                 └─ bake_model_with(.., &atlas, ModelTransform::default(), &BakeOptions::default())
                      └─ Vec<BakedQuad>  ->  BlockModels::items[item] = ItemGeometry
                                                  │
  draw time (no ResourceManager needed):          │
    display_matrix(&geometry.transform) ──────────┤
    gui_item_pose(rect_px, &geometry.transform)   │
    mesh_item_quads(&geometry.quads, pose, gui_light)  ->  ModelMesh
    gui_ortho(target_w, target_h)  ->  the pipeline's `view_proj`
```

All resolving and baking happens **once, inside `BlockModels::build`**, because
`ModelResolver` needs a live `ResourceManager` and nothing downstream keeps one.

### `GuiItemContext`, not `DefaultItemContext`

A handful of items (`spyglass`, `trident`, the spears, every bundle) branch on
`minecraft:display_context` at the top of their definition, with a `gui` case
naming the flat sprite and the *fallback* naming the in-hand 3-D model.
`DefaultItemContext` never answers a `select`, so it silently takes the wrong
(in-hand) branch. `GuiItemContext` answers `"gui"`, which is what an inventory
slot is.

Measurable consequence: only **1** model item is `gui_light: front`
(`decorated_pot`). Under the old default context the spyglass's in-hand model
leaked in as a second one.

### Why item geometry lives on `BlockModels`

"`BlockModels` holding item geometry" is a stretch; the honest framing is
**"everything baked against this atlas"**. It buys three things:

1. **One atlas.** A block item's faces *are* block textures. Baking here reuses
   the stitched `Atlas` the terrain path already uploads — no second GPU upload.
2. **One tint palette.** A tinted item (grass block, oak leaves) interns through
   the same `TintPalette` as the block state, so the hotbar icon and the world
   block resolve to the *same* palette slot and cannot drift apart.
3. **One pipeline.** See above.

### Tint resolution for an item

`bake_model` emits the raw model `tintindex`. The state loop rewrites it via
`registry.resolve(state_id)`; an item has no state id, so the block identity is
derived from the item id (`minecraft:grass_block` → block `minecraft:grass_block`)
and the properties are empty — an inventory icon is always the default state:

```rust
let kind = vanilla_tint_kind(&block, raw, &BTreeMap::new());
quad.tint_index = tints.color(kind).map(|rgb| i32::from(palette.intern(rgb)));
```

Measured: **11** item models carry a `tintindex`; only **8** are live
(`grass_block` + the 7 tinted leaves). `cherry_leaves`, `pale_oak_leaves` and
`stonecutter` carry an index but `vanilla_tint_kind` returns `None` — carrying an
index is not the same as being tinted.

### The pose

```
display_matrix = T(clamp(translation/16, ±5)) · Rx · Ry · Rz · S(clamp(scale, ±4)) · T(-0.5,-0.5,-0.5)
```

Vanilla's `ItemTransform.apply` pushes translate → rotate → scale onto the pose
stack, then `ItemRenderer` pushes `translate(-0.5, -0.5, -0.5)`. A `PoseStack`
**right-multiplies**, so the innermost operation is the centring: the model is
centred on the origin *first*, then scaled, then rotated, then translated. The
observable invariant is that the cube's centre maps to exactly `translation/16`
whatever the rotation — `centring_is_innermost_so_the_cube_centre_lands_on_the_translation`
asserts it.

Rotation is JOML `Quaternionf.rotationXYZ`, i.e. `Rx · Ry · Rz` — not the reverse.

For the vanilla block pose (`rotation [30, 225, 0]`, `scale 0.625`) the posed unit
cube has half-extents `x=0.44194, y=0.49160, z=0.53898`, i.e. **14.14 × 15.73 px**
in a 16 px slot: it fills the slot without overflowing. The three faces turned
towards the viewer are **up, east and north** — which is why a furnace (whose
`block/orientable` front is the *north* face) shows its opening in the inventory.

### GUI meshing

`mesh_item_quads` deliberately is **not** `mesh_models` with a one-block view:

* **`cullface` is ignored.** A slot has no neighbours; a quad culled against "the
  block to the north" would vanish for no reason. All quads are emitted and the
  pipeline's back-face culling removes the far ones.
* **Positions are transformed, not offset** — the emitted vertices are already in
  GUI pixel space.
* **Light is fixed** at `GUI_ITEM_LIGHT = 0xF0` (sky 15, block 0), so the shader's
  light term evaluates to exactly `1.0` — `get_brightness(1)` is `1` and
  `notGamma(1)` is `1`, see [light-ramp.md](./light-ramp.md). A GUI item is
  full-bright regardless of where the player is standing, and that exactness is
  why replacing the old linear ramp left every GUI item gate byte-identical.
* **`gui_light` rides in the `ao` slot**: `Side` keeps the per-face directional
  constants (`face_shade`, shared with the world path), `Front` flattens every
  face to `1.0`.

## How to change it

### Gotcha 1 — the winding invariant is "match the world camera", NOT "positive determinant"

This is the one that will bite. A GUI pose needs a Y-flip because screen Y grows
downward, and `S(16, -16, 16)` has **negative determinant**, which reverses
triangle winding. `ModelPipeline` culls back faces with `FrontFace::Ccw`. If the
compensating flip is wrong, the block renders **inside-out** — you see the
interior of the three far faces, which still looks like a plausible isometric cube
in a screenshot. That is exactly the bug class that survives visual review.

The correct invariant is:

> `sign(det(gui_ortho * gui_item_pose)) == sign(det(Camera::view_projection()))`

and that sign is **negative**, not positive. `glam::camera::rh::proj::directx::perspective`
itself has a negative determinant, so the world path — which renders correctly
today through this same pipeline with these same outward-wound quads — is
negative. Coding to "positive overall" ships the inside-out bug.

Concretely, in this crate:

| matrix | determinant | why |
| --- | --- | --- |
| `gui_item_pose` | **negative** | `S(w, -h, d)` — one flip |
| `gui_ortho` | **positive** | y-flip *and* z-flip (GUI z grows toward the viewer) — two flips |
| product | **negative** | matches `Camera::view_projection()` |

**Do not assert a determinant polarity from memory.** The test
(`item_render::tests::winding_matches_the_world_camera`) *derives* the
front-facing sign: it points a real `Camera` at an outward-wound cube from `+Z`,
takes the signed screen area of the visibly-front face, and requires the GUI
matrix to reproduce that sign for `{Up, East, North}` and the opposite for
`{Down, West, South}`.

It then checks the **other half**, which a determinant cannot express: the faces
that survive culling must also be the **nearest** ones under `CompareFunction::Less`.
Culling and depth must agree, or the depth test hides exactly what culling kept.
`item_geometry_gate::a_posed_item_lands_inside_its_slot_and_keeps_its_winding`
runs the same check on real baked stone geometry.

If you change `gui_ortho`'s z mapping, `gui_item_pose`'s z scale, or the
pipeline's `front_face`/`cull_mode`, re-read those two tests before trusting a
screenshot.

### Gotcha 2 — the `/16` and both clamps live in `item_render.rs`, not the parser

`DisplayTransform` stores the **raw JSON numbers**. `parse_display_transform`
(`lodestone-assets/src/model.rs`) is deliberately verbatim and
`lodestone-assets/tests/icon.rs` asserts exactly that. Vanilla's `ItemTransform`
deserializer multiplies translation by `1/16` then clamps to `±5`, and clamps
scale to `±4`; `display_matrix` applies all three.

Keep it that way. A parsed field that silently means "the JSON value ÷ 16" is
worse than one that means the JSON value, and it would make the parser's own
tests lie. Do not move the conversion into the parser, and do not edit that test.

### Gotcha 3 — `ModelTransform::default()` is correct, and is not the item's pose

`bake_model_with` takes a `ModelTransform`, which is the **blockstate placement**
rotation. An item has no blockstate, so it is always `default()`. The item's pose
is the `DisplayTransform`, applied at draw time by `display_matrix`. Passing the
display transform to the baker would bake the pose into block-local quads and
break the world/GUI sharing.

Use the same `BakeOptions` the terrain path uses (currently `default()`, i.e.
`uv_inset_texels: 0.0`) so GUI and world sample the atlas identically.

### Known limitation — composite items with a per-part `transformation` (the beds)

**16 items are baked incompletely: every colour of bed.** They are named in
`BlockModels::item_bake_misses()`.

`assets/minecraft/items/<colour>_bed.json` is a `minecraft:composite` of two
models — `block/<c>_bed_head` and `block/<c>_bed_foot` — where the *second* part
carries a per-part `transformation`:

```json
{ "type": "minecraft:model", "model": "minecraft:block/black_bed_foot",
  "transformation": { "left_rotation": [0,0,0,1], "right_rotation": [0,0,0,1],
                      "scale": [1,1,1], "translation": [0,0,1] } }
```

`lodestone-assets/src/item_model.rs` **never parses `transformation`**, and
`IconPart::Model` has no field to carry it. So the offset that positions the foot
behind the head is simply unavailable downstream. Given that:

* concatenating both parts would stack the foot *inside* the head and z-fight —
  strictly worse than the alternative;
* so `BlockModels::build` bakes the **first part only** (the head) and records
  each of the 16 beds in `item_bake_misses()` with the root cause.

**The real fix**, in order:

1. Parse `transformation` in `item_model.rs` (a quaternion `left_rotation`, a
   `scale`, a quaternion `right_rotation`, and a `translation` — vanilla's
   `Transformation`, decomposed TRS form).
2. Add a field for it on `IconPart::Model` in `lodestone-assets/src/icon.rs`.
3. Change `ItemGeometry` to hold all parts (or pre-apply each part's
   transformation to its quads at bake time, which keeps `ItemGeometry` flat and
   costs nothing at draw time), and drop the composite note.

Both steps 1 and 2 are in `lodestone-assets`, so this cannot be fixed from
`lodestone-render` alone. Nothing else in vanilla 26.2 hits this path — the beds
are the only composite model items.

### Adding a new source of GUI geometry

If you need geometry for something that is not reachable from an item definition,
follow the precedent in `build_complete_atlas`: seed its textures explicitly
(alongside the fluid and crack-stage seeding), then bake it in `BlockModels::build`
so it shares the atlas and the palette. Tolerate `BakeError::SpriteMissing`
per-entry into a report rather than failing the build — a resource pack will
reintroduce that class.

## Configuration

There is none — no env vars, no flags, no feature gates. The constants that
encode a convention (all `pub`, all re-exported from the crate root):

| constant | value | meaning |
| --- | --- | --- |
| `item_render::UNITS_PER_BLOCK` | `16.0` | model JSON translation units per block |
| `item_render::TRANSLATION_LIMIT` | `5.0` | vanilla's translation clamp, in blocks (post-`/16`) |
| `item_render::SCALE_LIMIT` | `4.0` | vanilla's scale clamp |
| `item_render::GUI_DEPTH_HALF_RANGE` | `1000.0` | half the GUI z range `gui_ortho` maps into `0..1` depth |
| `models::GUI_ITEM_LIGHT` | `0xF0` | the full-bright packed light byte |

**Atlas seeding** is the one build-time behaviour worth calling out.
`build_complete_atlas` seeds textures from `assets/<ns>/blockstates/**` *plus*
every item model's textures. Blockstate coverage alone already reaches **767 of
768** model parts (measured); the seeding exists for the single genuine leftover:
`minecraft:structure_block`, whose blockstate names four *mode-specific* models
(corner/data/load/save), so the plain `block/structure_block` texture its item
model uses is reachable from no blockstate at all. With seeding: **0 misses**.

## Dependencies

* **`lodestone-assets`** — `ItemIconBuilder` + `GuiItemContext` (item definition →
  `IconPart`), `ModelResolver` (model id → `ResolvedModel`), `bake_model_with`
  (→ `BakedQuad`), `vanilla_tint_kind`, `Atlas`/`AtlasBuilder`.
  Requires a live `ResourceManager` over a vanilla pack stack.
* **`lodestone-model`** — `Identifier` (for `vanilla_tint_kind`),
  `BlockStateRegistry`.
* **`glam`** — `Mat4`/`Vec3`. Public in the API (`gui_item_pose` returns `Mat4`).
* **`lodestone-render` internals** — `block_models::TintPalette` (shared with the
  state loop), `block_resolver::DefaultTints`, `models::ModelVertex`/`ModelMesh`,
  `model_pipeline::ModelPipeline` (consumer), `camera::Camera` (winding reference
  in tests only).

No GPU is required by anything in `item_render.rs` or `mesh_item_quads` — both are
fully unit-tested headlessly.

## Files and tests

| path | role |
| --- | --- |
| `crates/lodestone-render/src/item_render.rs` | `display_matrix`, `gui_item_pose`, `gui_ortho` + unit tests |
| `crates/lodestone-render/src/block_models.rs` | item discovery, atlas seeding, baking, `ItemGeometry` |
| `crates/lodestone-render/src/models.rs` | `mesh_item_quads`, `face_shade`, `GUI_ITEM_LIGHT` |
| `crates/lodestone-render/tests/model_census.rs` | `item_model_coverage` — coverage/tint/`gui_light` census |
| `crates/lodestone-render/tests/item_geometry_gate.rs` | end-to-end gate through `BlockModels::build` |

Jar-backed tests are `#[ignore]`d and need `.cache/mc/26.2/client.jar`:

```
cargo test -p lodestone-render --lib
cargo test -p lodestone-render --test item_geometry_gate -- --ignored --nocapture
cargo test -p lodestone-render --test model_census -- --ignored item_model_coverage --nocapture
```
