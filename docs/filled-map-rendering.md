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

### One depth variant, no map shader, no fifth bind group

The map quad draws through `ModelPipeline::for_map_surface`, a depth-state variant of the ordinary
model pipeline, with **group 1 swapped** from the stitched block atlas to the map's own texture. It
does not add a shader or bind-group layout: the model shader already spends all four
of wgpu's guaranteed `max_bind_groups` (camera / atlas / palette / anim), so a fifth group for a map
texture would validate on this Mac (which reports 8) and crash at startup on any 4-group adapter.

Vanilla supplies physical `1.01 / 128` separation and orders the frame body through
`VIEW_OFFSET_Z_LAYERING_FORWARD`. Lodestone preserves the physical separation but expresses the
ordering as two raster-depth variants under one shared view-projection: one step for the frame body,
two constant steps for the map. Both variants carry the same slope term: only the relative constant
may differ, because a doubled map slope term makes its separation grow with projected slope and creates
a curved/triangular floating edge at grazing angles. Applying a second scaled camera matrix to the body
made the two surfaces round independently at large world coordinates; the trace showed their projected
order reverse with camera and FOV changes. The raster layers are the forward-depth backend equivalent,
not world-space nudges.

Groups 0, 2 and 3 are the frame's existing shared bind groups, so a map draw adds exactly one texture,
one bind group and one draw call.

### Retained textures and meshes

`MapState::color_revision` advances only when a colour patch changes that map's pixels. `MapSource`
propagates `(map_id, color_revision, Arc<colors>)`, so `MapRenderCache` can retain the converted RGBA
texture, upload and bind group by that exact identity without hashing 16 KiB every frame. A new revision
releases only the superseded texture for that map; unrelated maps stay resident. The cache belongs to
`RenderState`, so recreating a device/session or target format cannot reuse stale wgpu handles.

The held quad is retained for an unchanged equip height. Framed batches retain the last exact visible
sequence of frame id, map id, pose bits, rotation, invisibility and sampled light. A map-pixel update therefore
rebinds only its texture; appearing, disappearing, moving, rotating or relighting a frame rebuilds the
affected batch. `RenderState::map_cache_counters` exposes conversion, upload/bind-group and mesh-build
counts for a live profile. `mip_level_count` is 1 and the sampler is `Nearest`, matching vanilla's
`DynamicTexture` — linear filtering smears terrain edges that are one pixel wide by design.

`ItemFrameRenderer` places visible map contents at local `z=.4375` but invisible-frame contents at
`.5`; after the frame transform the latter is `1/16` block closer to the wall. `framed_map_pose` keeps
the existing room-facing separation from the frame plate and applies that difference along the actual
frame normal, not world Z. Therefore invisibility is vertex data and an exact mesh-cache key member,
not merely a material choice.

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
  clear that back plate. The picture's plane has an explicit 1/64-block separation in the frame's own
  outward-normal direction from `template_item_frame_map`'s room-facing plate. The frame body and map
  share one view-projection and receive the ordered raster-depth steps described above (see
  `docs/item-frame-rendering.md`).
  The CPU broad phase uses vanilla's wall-offset, direction-shaped item-frame AABB plus
  `EntityRenderer.shouldRender`'s `0.5` inflation. A symmetric box around the packet anchor is not
  equivalent: the anchor is an attachment-block corner rather than the entity centre, and its
  missing room-facing edge made the whole map disappear at grazing camera angles.

  `tests/framed_map_pixels.rs` also pins an invisible frame at the live server's
  large world coordinates and renders one fixed, near-edge camera pose at FOV
  `30`, `64` and `110`. It compares each picture against the same invisible
  frame with no map rather than trusting a submitted counter. The Metal control
  produced 13,972, 3,366 and 648 picture pixels respectively, proving that FOV
  alone does not drop a room-facing quad through clip, winding or depth state.

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

`Ry(180°)` is not a transcription of vanilla's `Axis.ZP.rotationDegrees(180)`. Vanilla lays its quad
out with `v` growing **up**, where `map_quad_mesh` grows it down; on the `z == 0` plane every vertex of
this quad lands, `Rz(180) · diag(1, -1, 1)` and `Ry(180)` are the same map, and only `Ry(180)` also
turns the face outward. Substituting `Rz(180)` draws the picture upside-down *and* back-to-front.

**Vanilla's map render type culls back faces.** An earlier version of this section said it did not, and
that claim was carried forward into a live-defect hypothesis. `MapRenderer.render` submits through
`RenderTypes.text(texture)`, whose `RenderPipelines.TEXT` builds on a snippet that never calls
`withCull`, and `RenderPipeline.Builder.build` defaults to `this.cull.orElse(true)`. Its depth state is
the shared `DepthStencilState.DEFAULT` — `GREATER_THAN_OR_EQUAL` (our `LessEqual` under forward depth),
depth writes on, **no polygon offset** — and its colour target blends `BlendFunction.TRANSLUCENT` with
no alpha test, where Lodestone's variant is opaque with a `0.5` cutout. That last difference is inert
for a map: every `MapColor` is either fully opaque or `NONE`'s alpha `0`.

The in-plane turn is `(rotation % 4) · 90°`, not `rotation · 45°`: `ItemFrameRenderer`'s map branch is
`rotation % 4 * 2` eighths, so a map only ever hangs at a right angle and the odd half-steps fold onto
the even ones. An ordinary framed item does use all eight.

### A glow frame lights its own map

`ItemFrameRenderer.getLightCoords` substitutes a near-full-bright packed value for a *glow* frame's
contents instead of the sampled world light, and the map branch and the item branch pass **different**
constants: `15728880` for an item (sky 15, block 15) and `15728850` for a map — the latter being
`ItemFrameRenderer.BRIGHT_MAP_LIGHT_ADJUSTMENT` (30) below the first, which is just under two levels of
the packed block channel. The frame's own body takes a third number again, a block-light *floor* of
`GLOW_FRAME_BRIGHTNESS` (5) over the sampled value.

Lodestone spells those as `gpu/entity_passes.rs`'s `item_frame_light` (the body),
`framed_content_light` (a framed item) and `gpu/maps.rs`'s `framed_map_light` (a framed map, sky 15 /
block 13). The map one was missing for as long as framed maps have existed: `prepare_framed_maps`
sampled the world directly, so a glow-framed map in an unlit room drew black while an ordinary item in
the frame beside it drew bright. The two helpers it needed were already written and correct — the
producer simply never called them, and `GLOW_ITEM_FRAME_TYPE_PATH`'s own doc comment already described
the wiring, which is exactly why nobody counted its callers.

`tests/framed_map_pixels.rs`'s `only_a_glow_framed_map_lights_itself_in_an_unlit_room` is the wiring
gate; the unit test beside `framed_map_light` proves only the arithmetic. It renders both frame kinds
in a `sky = 0`, `block = 0` world against a real stone wall and measures the mean channel inside the
picture: glow **120.3** against plain **11.7**. Under the producer's previous expression both arms
measured 11.7. Note the fixture's own trap, which fired on the first run: `RenderState`'s entity light
source is unset by default and an unset source answers **full bright everywhere**, so without
`set_entity_light_source` both arms measured 120.3 and the gate proved nothing.

### The map's depth contest against its wall is *not* the live item-frame defect

`tests/framed_map_pixels.rs`'s `a_framed_map_survives_the_depth_test_against_its_attachment_wall` was
written to reproduce a live report of framed maps z-fighting on a visible frame and vanishing on an
invisible one. It does not reproduce, and that is the finding.

Every other arm in that file — and every sign, text and block-entity pixel gate in the suite — renders
against an **empty world**. A framed map's whole physical separation from the surface behind it is
`1.01 / 128` of a block (7.9 mm), so a fixture with nothing in the depth buffer cannot observe the one
contest the report is about. This arm builds a real stone attachment wall, hangs the frame on it, and
measures the picture's ink against the identical scene with the wall block removed. Measured, 36
configurations, **every one byte-identical between the walled and wall-free worlds**: both frame kinds,
at the origin and at the reporting server's own coordinates (`1970, 73, 3811`), at 2/8/24 blocks, at
0°/45°/75°/85° off the frame normal, FOV 110. Head-on at 2 blocks the picture measures 1,168 px against
a projected size of ~1,136 px, so the frame's own border eats nothing either.

So neither the attachment wall nor the frame's front plate removes a single pixel at any tested view,
and the remaining live suspects are elsewhere: something the fixture still holds fixed (one frame
rather than 61; no fluids, particles or translucent geometry in the scene), or the report is not a
depth fight at all. Do not spend another depth-bias constant on it without a fixture that fails first.

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

The palette is a `const` table; the sample density and pose constants are `const` in `gpu/maps.rs`.

For a live item-frame disappearance report, launch with `RUST_LOG=maps=debug`. To retain the edge
trace after terminal output is truncated, set `LODESTONE_MAP_DIAG_FILE=/absolute/path/map.log`; the
file is truncated at startup and flushed after every transition, and does not require `RUST_LOG`. Set
`LODESTONE_MAP_TRACE_ENTITY=<entity-id>` to follow one known frame; otherwise the observer follows the
framed map whose projected centre is closest to the crosshair (within 64 blocks and a small screen-edge
margin). A score margin keeps that choice stable when two maps nearly cross. `prepare_framed_maps` emits one
line only when that entity changes candidate/cull/source/submit state, including one absence edge, and the
opaque pass emits one when its submitted/drawn state changes. Camera position, orientation, FOV and aspect are
context fields, not raw parts of either change key, so ordinary movement cannot produce per-frame output. The gather
line includes the tracked entity id/type, pose yaw/pitch and rotation, item/map id, invisibility,
per-frame source resolution, quad normal and whether its one-block broad-phase bounds intersect the
frustum. It also records the map and comparison surface's eye/clip coordinates at the centre and four
corners, plus categorical clip/depth-order and projected-winding states. Visible frames compare against their rendered plate;
invisible image billboards compare against the attachment wall plane. These fields distinguish an
unresolved upstream `MAP_ITEM_DATA`, a normal frustum-edge cull, a submitted-versus-drawn mismatch and a
depth-order change without modifying culling, depth state or map placement.

Six native-only, process-start switches can then eliminate one branch at a time; any non-empty value
other than `0` enables its switch. They never change the default renderer and do not exist on wasm.
The three depth axes are separate on purpose — a single combined switch changed the comparison, the
write and the polygon offset together, so a live run under it settled nothing:

| switch | removes | interpretation when the map returns |
| --- | --- | --- |
| `LODESTONE_MAP_DISABLE_FRUSTUM_CULL=1` | only `prepare_framed_maps`' CPU item-frame frustum test | the frame reached `EntityDraw`, but the map's wall-offset AABB is the offending boundary |
| `LODESTONE_MAP_DISABLE_BACKFACE_CULL=1` | only the map pipeline's back-face cull | the quad was prepared but its projected winding/facing is wrong; this does not affect the frame body |
| `LODESTONE_MAP_DISABLE_DEPTH=1` | the map pipeline's depth test, its depth write **and** its polygon offset | the quad was prepared and rasterized but was displaced by one of those three; it will deliberately paint through world geometry while enabled. **This arm cannot say which** — use the three below |
| `LODESTONE_MAP_DISABLE_DEPTH_TEST=1` | only the comparison (`Always` in place of `LessEqual`) | the quad genuinely loses a depth comparison against something already in the buffer |
| `LODESTONE_MAP_DISABLE_DEPTH_WRITE=1` | only the depth write | the quad wins its own test and is then overdrawn by something that lost to the depth *it* wrote |
| `LODESTONE_MAP_DISABLE_DEPTH_BIAS=1` | only `MAP_SURFACE_DEPTH_BIAS` | the polygon offset is displacing the picture. Note the measured prediction is that this arm makes the defect **worse or unchanged**: the bias only ever moves a fragment toward the eye, and an over-large one clamps rather than discarding (`docs/coplanar-overlay-depth.md`). If it *fixes* anything, that measurement is wrong |

Combine switches only after testing them singly. The trace already names the earlier boundaries:
`candidates=0` means the input `EntityDraw` slice contains no filled-map frame before any renderer cull;
`candidates>0` with no `selected` only means no frame was close enough to the observer's tracking region
(set `LODESTONE_MAP_TRACE_ENTITY` for an exact frame); `selected` with `source=Unresolved` is a missing
`MAP_ITEM_DATA`; `in_frustum=false` is CPU culling; and `submitted=true` followed by no visible pixels is
in the GPU branch. Chests, signs and other true block entities are not `EntityDraw`s and do not use this
map diagnostic or its cull path; an absent block entity therefore needs a separate producer/packet
investigation.

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
