# Item frame rendering

## What it is

How `minecraft:item_frame` and `minecraft:glow_item_frame` reach the screen: the frame's own
wooden body, and whatever hangs in it — an ordinary item, a `minecraft:special` rig, or a
filled map. Four producers in four files, sharing one pose chain.

## How it works

### The frame is a block model, and that is why it had no producer

`ItemFrameRenderer` has **no `bakeLayer` call**. Its body is `state.frameModel`, a
`BlockStateModel` resolved through `BlockModelResolver.updateForItemFrame` →
`BlockStateDefinitions.getItemFrameFakeState` — a throwaway `StateDefinition` over
`Blocks.AIR` carrying only `BlockStateProperties.MAP`, picked plain-or-glowing by entity
type. So an item frame is a *block model at an entity's position*, the same shape as a
falling block, a primed TNT and a piston head, and **not** a `ModelPart` rig.

`crates/lodestone-assets/src/entity_models.rs` therefore omits `item_frame` deliberately, and
`model_for_type(EntityType::ItemFrame)` answers `None`. That was correct, and for a long time it
also meant nothing anywhere drew a frame: `resolve_animated` skips any type path with no baked
model, silently. The body's producer is now `gpu/moving_blocks.rs`'s `merge_item_frames`, beside
the other four block-model-posed-by-hand producers.

There is no block *state id* to reach the geometry by, because `minecraft:item_frame` is not a
registered block — so it does not go through `merge_moving_block`. The jar nonetheless ships real
`assets/minecraft/blockstates/item_frame.json` and `glow_item_frame.json` files (each a two-way
`map=false`/`map=true` variant switch over `block/template_item_frame`), which is what lets
`BlockModels::build` bake all four through the ordinary `BlockBaker::bake_block` path. They are
stored as a fixed four-entry table (`BlockModels::item_frame_quads`, keyed by
`block_models::item_frame_slot(glow, map)`), snapshotted into `CrackResolver::from_models`
alongside the per-state quads so they survive `BlockModels` being dropped.

### The pose chain, and the two derivations that cancel

Everything is posed relative to `lodestone_render::entity::item_frame_space`:

```text
T(feet + dir · 0.46875) · Rx(pitch) · Ry(180 − yaw)
```

Its origin is the **centre of the block the frame hangs in**, and its `+z` points *into* the wall
behind the frame.

Two things about that expression are easy to get wrong and invisible when you do.

**`dir · 0.46875`.** `ItemFrame.createBoundingBox` puts the entity's own position
`0.46875` *behind* the block centre (`Vec3.atCenterOf(pos).relative(direction, -0.46875)`), and
`ItemFrameRenderer.submit`'s `translate(direction.step * 0.46875)` exists purely to undo that.
The dispatcher's `getRenderOffset`/`translate(-renderOffset)` pair also cancels exactly and is
therefore absent here rather than ported as two steps.

**`180 − yaw`.** The renderer derives its angles from the frame's `Direction`, which is not on
the wire. `ItemFrame.setDirection` derives the entity's `yRot`/`xRot` from that same `Direction`,
and those *are*. Composing the two eliminates it:

| direction | renderer | entity |
|---|---|---|
| horizontal | `xRot = 0`, `yRot = 180 − dir.toYRot()` | `xRot = 0`, `yRot = dir.get2DDataValue() * 90` |
| vertical | `xRot = −90 · step`, `yRot = 180` | `xRot = −90 · step`, `yRot = 0` |

`Direction.toYRot()` **is** `get2DDataValue() * 90`, and the vertical row's entity `yRot` is `0`,
so `180 − yaw` covers both rows and the pitch passes through unchanged. Dropping the `180 −`
still produces a frame flat against a wall — just the wrong wall, back plate facing the room,
with the contents hidden behind it.

`item_frame_facing_step` is that rotation applied to local `−z`, which is the frame's real
`Direction` by construction (the model's back plate is at local `+z`, spanning `z = 15.5..16` in
`template_item_frame`). Deriving it rather than tabulating it matters: a bare `Ry(yaw)` applied to
`+z` gives `(sin yaw, 0, cos yaw)` where the truth is `(−sin yaw, 0, cos yaw)` — **the two agree
at yaw 0 and 180 and are opposite at 90 and 270**, so any gate that only ever probes a north or a
south wall passes under both readings. The framed-map path lifted along the wrong one for exactly
that reason, and its own gate could not see it.

### The four producers

| what | where | pose |
|---|---|---|
| the frame body | `gpu/moving_blocks.rs`'s `merge_item_frames` | `item_frame_body_matrix` (block models are corner-origin, hence the `T(−0.5, −0.5, −0.5)`) |
| an ordinary item | `gpu/world_items.rs`'s `merge_framed_items` | `framed_item_mesh` → `framed_item_matrix` |
| a `minecraft:special` item | `gpu/entity_passes.rs`'s `framed_special_item` | `framed_item_matrix` |
| a filled map | `gpu/maps.rs`'s `prepare_framed_maps` | `framed_map_pose` |

The contents sit at `T(0, 0, lift)` inside frame space, `lift` being `0.4375` for a visible frame
and `0.5` for an invisible one (vanilla's two `translate` calls). Because frame space's `+z`
points into the wall, that puts a framed item `0.46875 − 0.4375 = 0.03125` blocks in front of the
entity's own position — **not** the `0.90625` you get by adding the two translates, which floats
the item a full block out in front of the frame where it reads as an unrelated placement bug.

Then `Rz(rotation · 45°)`, then `scale(0.5)`, then the item's own `display.fixed`.
`ItemDisplayContext.FIXED` is the context `extractRenderState` resolves in — the single easiest
thing to get wrong, because the *dropped* item on the same code path is `Ground`, and reusing it
lays a framed sword flat.

### Light

`ItemFrameRenderer` uses two different numbers from one sample, which is why
`entity_passes.rs` exposes two functions:

* `item_frame_light` — the body, and a plain frame's contents.
  `getBlockLightLevel` floors the **block** nibble at `5` for a glow frame
  (`GLOW_FRAME_BRIGHTNESS`); the sky nibble passes through.
* `framed_content_light` — the contents. A glow frame substitutes `15728880` (sky 15, block 15)
  outright, so what it holds is at full brightness while its own body is only dim-but-visible.

The probe is the entity's own position rather than an eye-height offset: an item frame's eye
height is `0.0`, and `createBoundingBox`'s `−0.46875` leaves the entity inside the air cell it
hangs in rather than in the wall.

### The rotation, end to end

`ItemFrame.DATA_ROTATION` is **index 10**, not 8 — `HangingEntity.DATA_DIRECTION` takes 8 (an
alignment-only `DIRECTION` this decoder consumes and does not surface, since the direction is
recoverable from yaw/pitch) and `ItemFrame.DATA_ITEM` takes 9. Index 10's `INT` is shared with
`Display.DATA_POS_ROT_INTERPOLATION_DURATION_ID`, and no `entity_census` column separates a frame
from a display entity — neither is living, neither is a mob — so it needs its own
`MetadataClass::ItemFrame`.

```text
SET_ENTITY_DATA index 10 INT
  → v770 packets/metadata.rs, gated on MetadataClass::ItemFrame
  → EntityMetadataUpdate::item_frame_rotation
  → lodestone_ecs::entity::ItemFrameRotation      (apply_entity_metadata, the per-entity router)
  → EntityDraw::item_frame_rotation               (extract_entity_draws, bridged through EntityIndex)
  → framed_item_matrix's Rz
```

The stack itself needs none of this: `ItemFrame.DATA_ITEM` is an `ITEM_STACK` and therefore
self-identifying by serializer, handled before the index match runs. That is why a chest in a
frame drew long before its rotation did.

## How to change it, and the gotchas

* **Adding a field to `EntityDraw` is not free.** It is constructed by literal in about twenty
  fixtures across `crates/lodestone-shell/{src,tests}`, and `extract_entity_draws` is at
  `bevy_ecs`'s **16-parameter `SystemParam` ceiling** — a seventeenth top-level `Query` fails to
  compile with an `in_set` "method not found" error a hundred lines away from the parameter that
  caused it. Nest it into the existing `(tameds, vehicles, armor_stands, item_frame_rotations)`
  tuple instead.
* **`item_frame_rotation` is a `u8`, not an `Option`.** Vanilla's accessor default is `0`, an
  upright item, and a frame always draws its contents — so unlike `block_state` there is no
  absent case a consumer would draw differently.
* **A framed map is an either/or with the item branch**, in vanilla (`state.mapId != null`
  returns before the item branch) and here (`merge_framed_items` skips `filled_map`).
* **`FRAMED_MAP_LIFT` is derived, not tuned.** `0.46875 − 0.4296875`, where `0.4296875` is the map
  plane's own local `z` (`translate(0, 0, 0.4375)` then `translate(0, 0, −1)` at the `0.0078125`
  map scale). The previous hand-picked `0.03` was 1/1000 of a block *behind* the `item_frame_map`
  model's back plate — harmless for exactly as long as nothing drew that plate.
* **Three of the four producers above read `EntityDraw::item`, and for their whole existence that
  field was `None` for an item frame.** `extract_entity_draws` narrowed the recorded stack with
  `(kind.0.as_ref() == ITEM_ENTITY_TYPE_PATH).then(..)`, so `merge_framed_items`,
  `prepare_framed_maps`, `framed_special_item` and (outside this family)
  `merge_thrown_item`'s wire-preferred arm all read `None` forever. The one producer that does **not**
  read it is `merge_item_frames`, the body — which is exactly why the symptom was reported as "the
  frame draws and its contents do not". `prepare_framed_maps` had **never once run**, which means the
  `framed_map_pose` correction that landed with it was fixing geometry no player had seen. `ItemStacks` is only written for an entity
  whose metadata carried the `ITEM_STACK` serializer, so the type test bought nothing; every draw
  site gates on its own type anyway. **If you add a fifth producer, check what supplies its input
  and count the production sites that assign that input a non-default value** — the pixel gates
  below cannot ask, because each builds its own `EntityDraw` with `item: Some(..)` by hand.
  `tests/live_framed_item_wire.rs` is the one that can: it goes through `Sim::entity_draws`, the
  accessor `app/redraw.rs` calls, against a frame a real server placed.
* **Two pixel gates cover this**, and neither can be replaced by a counter:
  `tests/item_frame_pixels.rs` (the body, an ordinary item, the invisible split, the glow variant)
  and `tests/special_item_world_pixels.rs` (a framed shield). Note the comparison both need: a
  frame body **fills its own silhouette**, so "differs from the empty scene" is the same number
  for a frame holding a shield and a frame holding nothing (measured: 6320 both). The contents can
  only be seen by diffing against the *empty frame*.

## What is deliberately not ported

* **`submitWithZOffset`'s `outlineColor` argument.** Its depth offset is now carried by the
  shared `ModelPipeline::for_surface` polygon-bias state: both the frame body and its map picture
  use the same negative `(slope = -1, constant = -10)` offset toward the camera. This is a
  depth-buffer-unit separation, so it remains effective at grazing angles without changing the
  derived `FRAMED_MAP_LIFT` geometry.
* **The `map=true` variant is selected from the held item's id**, not from a resolved `MapId`.
  Vanilla asks `entity.getFramedMapId(itemStack)` and falls back to the plain frame when map data
  has not loaded. This client still selects the wider border for any `minecraft:filled_map`, but
  now retains that stack's decoded `map_id` separately for the map-texture pass; an unavailable
  `MAP_ITEM_DATA` payload skips only the picture, not the body.
* **No tint rewrite on the body's quads.** Neither `#wood` (`block/birch_planks`) nor `#back`
  (`block/item_frame`, `block/glow_item_frame`) carries a `tintindex`, so there is no raw index
  for the state loop's palette pass to translate. A resource pack that added one would draw
  untinted — the same shortfall an untinted moving block already has.
* **A framed stack draws one copy.** `ItemFrameRenderer`'s item branch draws it once whatever the
  count, unlike a drop's `submitMultipleFromCount`.

## Configuration

None. The geometry comes from the loaded resource pack's `blockstates/item_frame.json` and
`blockstates/glow_item_frame.json`; a pack that ships neither makes
`CrackResolver::item_frame_quads` empty, which every producer treats as "draw nothing".

## Dependencies

* `lodestone-assets` — `BlockBaker::bake_block`, the stitched block atlas.
* `lodestone-render` — `block_models::item_frame_slot`, `CrackResolver`, `entity::item_frame_*`,
  `entity::framed_item_*`, `mesh_moving_block_quads`, the `ModelPipeline`.
* `lodestone-v770` — the index-10 decode and `MetadataClass::ItemFrame`.
* `lodestone-ecs` — `entity::ItemFrameRotation`, `ingest::apply_entity_metadata`.
* `lodestone-shell` — `gpu/moving_blocks.rs`, `gpu/world_items.rs`, `gpu/entity_passes.rs`,
  `gpu/maps.rs`.
