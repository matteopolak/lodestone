# Filled map rendering

## What it is

Drawing a `minecraft:filled_map`'s actual picture — the vanilla `MapColor` palette, a per-map 128×128
texture built from the colour bytes `SessionMaps` folds, and the quads that show it in the hand and in
an item frame (issue #184).

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
  world-space mesh. Item frames are `HangingEntity` and have **no renderer of their own** (explicitly
  out of issue #23's block-entity scope), so a framed map draws its picture with no frame border. The
  picture is the part a player is looking at.

## How to change it, and the gotchas

**`minecraft:map_id` is not decoded, and that is the one real gap.** `lodestone_model::ItemComponents`
has no field for it, and `read_component_patch`'s `other =>` arm cannot skip an unmodelled payload — so
a filled map's `map_id` component actually **truncates the rest of that packet**, exactly the failure
mode the `minecraft:trim` arm was added to fix. Until it is modelled the same way:

* `Sim::map_source` takes an `Option<i32>` and `None` means "the lowest-numbered map the server has
  sent". Right in the overwhelmingly common one-map case, wrong picture when a player carries two.
* Both call sites pass `None`. When the component lands they pass `Some(id)` and nothing else changes;
  `prepare_framed_maps` then becomes a group-by-id returning one `(mesh, texture)` pair per map.

**The source must be re-installed every frame.** It captures a *snapshot* of `SessionMaps`, so one
installed at login would show a map frozen at whatever the server had sent by then and never fill in.

**`stats.filled_maps_drawn` is the counter that separates the two failure modes.** A map whose grid is
entirely `MapColor.NONE` draws a fully transparent quad, so "unexplored" and "never reached the
pipeline" produce the same number of visible pixels.

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
* `MAP_ITEM_DATA` decode in `crates/protocol/v770/src/adapter.rs`, which produces
  `ClientEvent::MapItemData`.
