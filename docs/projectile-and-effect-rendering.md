# Projectile and effect rendering

## What it is

Draw paths for entities that are neither an ordinary mob rig nor a plain billboard: velocity-aligned projectiles (arrow, spectral arrow, trident), firework rockets, lightning bolts, paintings, item frames and their contents (including filled maps), and the three display entities (`text_display`, `item_display`, `block_display`). Each needs its own placement because none is posed like a living mob.

## How it works

### Projectiles

A code-built cuboid rig aligned to velocity: `projectile_model_matrix(pos, yaw, pitch, scale) = T(pos) * Ry(yaw - 90) * Rz(pitch) * S(scale)`. Unlike mob placement there is no Y-flip and no `+1.501` ground lift; these rigs are authored `+Y` up. `projectile_pitch_offset_deg(model_name)` selects this placement (`0.0` arrow and spectral arrow, `90.0` trident, `None` otherwise); a new rig without an arm there draws 1.5 blocks high and mirrored with every mesh test green.

Yaw and pitch come from the server's velocity-derived rotation (eased 20% per tick, quantised); the client simulates flight between reports and reconciles from each packet. Direction-accelerating projectiles add `acceleration_power` along movement each tick, apply inertia, then move; a power packet updates the rule without resetting pose or velocity (`0.0` stops the added speed, not the motion). A projectile's `yRot` sign is opposite to a mob's body yaw; treat it as one fact checked against a live server.

### Firework rockets

A billboarded item model with its own producer (no scale term, plus a rotation the shared billboard table has no column for). `attached` suppresses the draw while the rocket is an elytra boost; `shot_at_angle` composes a fixed three-axis rotation after the billboard orientation. `ITEM_STACK` is self-identifying by serializer, so it needs no index wiring.

### Lightning bolts

The server owns lifetime and publishes through `MobSim::snapshots` and `LiveMobSource`; an expired bolt is removed before the tick publishes, so the `EntityStreamer` emits its removal next pass. No synced data, so the wire carries only position and the shape seed is rolled independently per side; there is no captured-bytes oracle, so gates check structural invariants. Geometry is four concentric hollow tubes along one seeded random walk, rebuilt each frame and blended additively (`SRC_ALPHA, ONE`); alpha blending reads as dull grey because the base colour is dim blue-grey and the white comes from four passes stacking. No hitbox means no frustum culling; cost is bounded by a fixed buffer. Not ported: the mid-strike reseed (it needs per-bolt flash state); a bolt keeps one shape seeded from its entity id.

### Paintings

A flat slab per variant in its own pass. Facing comes from the ordinary yaw `HangingEntity` already writes. 51 variants reduce to 9 `(width, height)` shapes. The wire variant id indexes the registry's alphabetical order, not bootstrap registration order (settle against a captured `registry_data` fixture). The default variant is never sent, so the decoder synthesises it at spawn. The front face samples the variant sprite per cell; back and edge faces tile one shared texture (a single stretched quad is visibly wrong on large paintings). Light is sampled once per painting, a known gap for a torch-lit wall behind a big one.

### Item frames

The frame body is a block model baked at the entity position (like a falling block), not a `ModelPart` rig. Its contents (an item, a `minecraft:special` rig, a filled map) are four producers sharing `item_frame_space = T(floor(anchor) + (0.5,0.5,0.5)) * Rx(pitch) * Ry(180 - yaw)`. The anchor is the attachment block's centre (two offsets cancel), and the `180 -` term keeps the back plate from facing into the room. Rotation is its own metadata field behind a dedicated `MetadataClass` since its index collides with a `Display` field. Light differs for body (floor for a glow frame), ordinary contents, and glow-frame contents (full bright).

A filter written for one purpose once blanked every consumer of `EntityDraw::item` for this entity type: check who else reads a shared field and whether production ever assigns a non-default value.

### Filled maps

A packed byte is `id << 2 | brightness`; the high six bits index a base-colour table and the low two pick one of four brightness scalers whose enum order is not ascending (dimmest `LOWEST` is index 3). Id 0 is transparent (unexplored is a hole). `lodestone_render::PackedMapColour` rejects ids 62 and 63 before indexing, `map_texture_rgba` validates each byte once, and an invalid byte falls back to the transparent entry. The GPU texture is retained and rebuilt only on that map's colour revision.

Two draw sites: held (forked before the ordinary item path, or it shows the flat blank sprite) and in a frame (grouped by map id into one mesh). The picture sits a fixed sub-block clearance in front of its backing, expressed as two raster-depth steps under one shared view-projection (a second independently scaled camera matrix loses precision and reverses draw order). The integrated server has no map-data store, so singleplayer never receives contents.

`lodestone_game::maps::MapId` owns the signed-wire boundary: the store keeps only validated non-negative ids, negative values never allocate a row, and only the item-component source accepts a raw value and validates it before lookup.

### Display entities

All three share `pose = T(anchor) * orientation(billboard_mode, entity yaw/pitch, camera yaw/pitch) * Transformation(translation, left_rotation, scale, right_rotation)`, in that order (swapping scale and left rotation is invisible in a screenshot and wrong).

| mode | yaw source | pitch source |
|---|---|---|
| `Fixed` | entity | entity |
| `Horizontal` | entity | camera |
| `Vertical` | camera | entity |
| `Center` | camera | camera |

The four `Transformation` fields live on the shared `Display` record; read them unconditionally for every subtype.

`text_display`: multi-line centring measures each line's styled width (bold widens advances). The drop shadow separates from ink by doubling the polygon-offset slope term, not a geometric z-nudge (which assumes a fixed front and loses ink from behind). `FLAG_SEE_THROUGH` routes to a no-depth-test, no-depth-write pipeline drawn last. A brightness override packs sky and block nibbles and overrides sampled light. `block_display` has no shift (the falling block's `-0.5` is that entity's spawn convention). `item_display` adds a 180 degree turn plus the item's context transform; `ItemDisplayContext::None` means identity, not "draw nothing".

The text pass packs panels, shadows and glyphs into one grown-and-reused vertex buffer; clipping it after panels and shadows leaves dark shadows or no letters. Diagnose with `RUST_LOG=info,display_text=debug` (periodic demand, capacity and ink-less panel report). Change partition and upload order together in `gpu/display_text.rs`.

## How to change it

- A new projectile needs both a rig and an arm in `projectile_pitch_offset_deg`.
- A data-pack registry's wire order is alphabetical; settle tables against a captured `registry_data`.
- A field equal to its accessor default is never sent; decoders synthesise the idle default at spawn.
- Metadata index collisions recur here (firework angle bit, frame rotation, display brightness). Use the jar's entity-data-index dump and a `MetadataClass` guard that separates the claimants (a living/mob census column is the wrong axis: none are living).
- A field on a shared base record is read unconditionally by every subtype.

## Configuration

No feature flags. For the map and frame depth seam: `RUST_LOG=maps=debug` (and `pack_trace=debug`), and `LODESTONE_MAP_DISABLE_*` switches (frustum cull, back-face cull, depth test, write, bias) to bisect ordering reports; see `gpu/maps.rs`.

## Dependencies

- `lodestone-assets` (rig and mesh data, item-frame block baking).
- `lodestone-render` (`entity::projectile_model_matrix`, `painting`, `entity::item_frame_*`, `lightning_bolt`, `display`, `map_item`).
- `lodestone-server` (bolt lifecycle and snapshots).
- `lodestone-ecs` (`FireworkFlags`, `PaintingVariant`, `ItemFrameRotation`, `Display*`, folded by `ingest::apply_entity_metadata`/`apply_display_metadata`).
- `lodestone-game::maps` (`MapStore`/`MapState`).
- The 26.2 family's metadata decodes and class guards.
- `lodestone-shell` `gpu/` (`entity_passes.rs`, `world_items.rs`, `moving_blocks.rs`, `maps.rs`, `lightning_bolt.rs`, `display_text.rs`).
