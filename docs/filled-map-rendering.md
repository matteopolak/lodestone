# Filled map rendering

## What it is

Drawing a `minecraft:filled_map`'s actual picture — the vanilla `MapColor` palette, a per-map 128×128
texture built from the colour bytes `SessionMaps` folds, and the quads that show it in the hand and in
an item frame.

## How it works

Three layers, and the split is the same one `lodestone-game/src/maps.rs` already documents: the store
keeps raw *packed* vanilla colour bytes and deliberately refuses to resolve them, because the palette
is presentation.

| layer | lives in | does |
|---|---|---|
| palette | `lodestone-render/src/map_item.rs` | `map_color_rgba` / `map_texture_rgba` / `map_quad_mesh` |
| texture | `lodestone-shell/src/gpu/maps.rs` | `map_texture_bind_group` |
| geometry | `lodestone-shell/src/gpu/maps.rs` | `prepare_held_map`, `prepare_framed_maps` |

A packed byte is `id << 2 | brightness` (`MapColor.getPackedId`). The high six bits index a 62-entry
base table transcribed from the 26.2 jar; the low two pick one of four brightness modifiers applied as
an **integer** `channel * modifier / 255` (`ARGB.scaleRGB(int)`). The modifier order is `LOW, NORMAL,
HIGH, LOWEST` — **not ascending**, because `LOWEST` is id `3`. Sorting the table by brightness inverts
every terrain contour on the map.

Id `0` is `MapColor.NONE`, whose `calculateARGBColor` short-circuits to literally `0` — alpha zero, so
an unexplored map is a *hole* rather than a black square.

### No map pipeline, no map shader, no fifth bind group

The map quad draws through the ordinary `ModelPipeline` with **group 1 swapped** from the stitched
block atlas to the map's own texture. That is not a shortcut: the model shader already spends all four
of wgpu's guaranteed `max_bind_groups` (camera / atlas / palette / anim), so a fifth group for a map
texture would validate on this Mac (which reports 8) and crash at startup on any 4-group adapter.

Groups 0, 2 and 3 are the frame's existing shared bind groups, so a map draw adds exactly one texture,
one bind group and one draw call.

### The texture is rebuilt per frame on purpose

`map_texture_bind_group` creates a texture, uploads it and builds a bind group every frame a map is
visible. A cache keyed by map id would need `&mut self` (the whole render path is `&self`) *and* would
miss most frames anyway: the server streams a fresh column patch as the holder walks, so a walking
player's map changes on most ticks. At 64 KB and one or two visible maps that is not worth the
invalidation bug. `mip_level_count` is 1 and the sampler is `Nearest`, matching vanilla's
`DynamicTexture` — linear filtering smears terrain edges that are one pixel wide by design.

### The two draw sites

* **In hand** — `FirstPersonHand::Map`, forked *before* the ordinary item branch in
  `prepare_first_person_hand`. Vanilla forks the same way (`renderArmWithItem` tests
  `MapItem.isFilledMap` and calls `renderMap`); falling through draws `item/filled_map`'s flat blank
  sprite, which looks like a working map until you notice it has no terrain on it. The pose is
  `renderTwoHandedMap` + `renderMap`'s `scale(0.38)`, and it takes the same `inverse_arm_height`
  equip dip every other held item does.
* **In an item frame** — `prepare_framed_maps` walks the `EntityDraw` slice for
  `item_frame`/`glow_item_frame` carrying a `filled_map` and concatenates one quad each into a single
  world-space mesh. The frame's own border and back plate are drawn separately, as a *block model*
  posed by hand — see `docs/item-frame-rendering.md`, which also explains why `FRAMED_MAP_LIFT` has to
  clear that back plate. The map draw uses `ModelPipeline::for_map_surface`, a second relative
  polygon-bias step beyond the frame body's `for_surface` step, so the picture is unambiguously in
  front of the frame's front texture without a world-space lift.

### `framed_map_pose` is built from the frame's own facing, and this was wrong twice

```text
T(feet + dir · FRAMED_MAP_LIFT) · item_frame_facing(yaw, pitch) · Rz(rot · 90°) · Ry(180°) · S
```

`Ry(yaw)` applied to the quad's local `+z` gives `(sin yaw, 0, cos yaw)`; the frame's real `Direction`
(`item_frame_facing_step`, from `ItemFrame.setDirection`) is `(-sin yaw, 0, cos yaw)`. **They agree at
yaw 0 and 180 and are opposite at 90 and 270.** The pose used to be `Ry(yaw) · Rx(-pitch)`, so on every
east- and west-facing wall the picture's front face pointed *into* the wall, and the model pipeline
culls back faces — the map drew **zero pixels** while the wider `item_frame_map` body still drew around
the hole. Measured in `crates/lodestone-shell/tests/framed_map_pixels.rs`: 11,236 differing pixels at
yaw 0 and 180 against **0** at yaw 90 and 270 before the fix, 11,236 at all four after. The unit
neuter in `gpu/maps.rs` reports **4 of 6** facings wrong, because the two vertical ones (a frame on a
floor or a ceiling) were broken by the same expression's pitch sign and composition order.

The lesson generalises past this file. The *translation* had already been corrected for exactly this
coincidence — the lift is applied on the left, in world space, precisely because the picture's own `+z`
is the wrong axis — and the gate written alongside that fix asserts only where the picture's **centre**
lands. A centre passes under either reading of the orientation, so half the fix went in and the other
half was invisible. When a coincidence is what hid a bug, check every quantity that shares it, not the
one you were looking at.

`Ry(180°)` is not a transcription of vanilla's `Axis.ZP.rotationDegrees(180)`. Vanilla draws the map
through a no-cull render type and lays its quad out with `v` growing **up**, where `map_quad_mesh`
grows it down; on the `z == 0` plane every vertex of this quad lands, `Rz(180) · diag(1, -1, 1)` and
`Ry(180)` are the same map, and only `Ry(180)` also turns the face outward. Substituting `Rz(180)`
draws the picture upside-down *and* back-to-front.

The in-plane turn is `(rotation % 4) · 90°`, not `rotation · 45°`: `ItemFrameRenderer`'s map branch is
`rotation % 4 * 2` eighths, so a map only ever hangs at a right angle and the odd half-steps fold onto
the even ones. An ordinary framed item does use all eight.

## How to change it, and the gotchas

**`minecraft:map_id` *is* decoded; it is dropped one layer above the wire.** An earlier version of
this section said the component was unmodelled and truncated the rest of the packet. That was true when
it was written and is not now — v770's component-patch reader fills `ItemComponents::map_id` from the
VarInt `MapId`, alongside `minecraft:trim`. What is missing is the *carry*:
`extract_entity_draws` narrows a frame's `DisplayItem` stack to a bare `ResourceLocation` for
`EntityDraw::item`, and `HeldItemEquip` narrows the hand's the same way, so neither draw site has an id
to pass. Consequently:

* `Sim::map_source` takes an `Option<i32>` and `None` means "the lowest-numbered map the server has
  sent". Right in the overwhelmingly common one-map case, wrong picture when two maps are visible.
* Both call sites pass `None`. Closing this is a change to `EntityDraw`/`HeldItemEquip` — carry the id
  beside the item — not to `map_source`, which already takes it; `prepare_framed_maps` then becomes a
  group-by-id returning one `(mesh, texture)` pair per map.

**The integrated server has no maps at all.** `lodestone-server` contains no `MapItemSavedData`
equivalent, no map saved-data store, and no `MAP_ITEM_DATA` producer — grep it for `map_id` and the
only hit is a comment about weather. Vanilla pushes a framed map's contents from
`ServerEntity.sendChanges` every ten ticks to every player in the level (and a carried one from
`ServerPlayer`), so against a real server the contents arrive within half a second of the frame coming
into view. In singleplayer they never arrive, and the frame is drawn wide and empty forever. That is
a server-side feature gap, not a rendering fault, and the log line below is what tells the two apart.

**Every decline is logged, once per reason.** A frame holding a map whose contents have not arrived is
pixel-identical to a frame holding nothing, so `prepare_held_map` and `prepare_framed_maps` name their
reason through `note_map_skip` instead of returning a bare `None`: `NoModels`, `NoSource` (the shell
pushed no source this frame — `Sim::map_source` answers `None` off a live server), `NoContents` (no
`MAP_ITEM_DATA` folded for this map yet) and `NoUpload`. The latch is per `(site, reason)` and is
cleared by `note_map_drawn`, so a decline that returns after a working period is reported again rather
than swallowed. "No framed map is in view" is deliberately *not* logged — that is every frame of
ordinary play.

**The wider frame is selected from the item id, and vanilla selects it from the data.**
`BlockModelResolver.updateForItemFrame(model, isGlowFrame, state.mapId != null)` in vanilla, where
`state.mapId` is set only once `level.getMapData(id)` returns something. `merge_item_frames` keys the
`map=true` variant on `minecraft:filled_map` instead, so a framed map whose contents have not arrived
shows the wide border with nothing inside it — which is exactly what this subsystem was reported as
doing.

**The source must be re-installed every frame.** It captures a *snapshot* of `SessionMaps`, so one
installed at login would show a map frozen at whatever the server had sent by then and never fill in.

**`stats.filled_maps_drawn` is the counter that separates the two failure modes.** A map whose grid is
entirely `MapColor.NONE` draws a fully transparent quad, so "unexplored" and "never reached the
pipeline" produce the same number of visible pixels. `tests/framed_map_pixels.rs` uses that as an
executed negative control: an all-`NONE` grid must land on the no-source frame's pixels **exactly**,
which is what makes the painted arm's 11,236 px attributable to the picture rather than to the body.

**Not drawn:** the `MapDecoration` icons (the player arrow, banner markers) and vanilla's
`map_background` frame sprite. `SessionMaps` already carries the decorations, so this is an asset job —
the map-decorations atlas is not stitched — rather than a wiring one.

## Configuration

None. No env vars, no flags. The palette is a `const` table; the sample density and pose constants are
`const` in `gpu/maps.rs`.

## Dependencies

* `lodestone_game::maps::{MapStore, MapState, MAP_SIZE}` — the fold, via
  `lodestone_ecs::session::SessionMaps` and `Sim::maps`.
* `lodestone_render::{ModelPipeline, GpuModelMesh, ModelMesh, ModelVertex}` — the shared pipeline the
  quad draws through, and `texture::GpuAtlas` as the shape `atlas_bind_group` accepts.
* `MAP_ITEM_DATA` decode in `crates/protocol/v770/src/adapter/inventory.rs`, which produces
  `ClientEvent::MapItemData`.
* `lodestone_render::entity::{item_frame_facing, item_frame_facing_step}` — the single owner of "which
  way does this frame look", shared with the frame body's own `item_frame_body_matrix` so the picture
  and the border around it cannot disagree.
* `tracing`, for the decline diagnostics.
