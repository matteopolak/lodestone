# Dropped items

## What it is

The render path for `minecraft:item` entities — the stacks lying on the ground
after a block breaks or a mob dies. Item entities are *not* cuboid part rigs, so
none of the entity pipeline applies to them: a drop is an **item model** drawn in
the world, through the same `ModelPipeline` and the same stitched block atlas
that terrain and the hotbar's 3-D icons use.

Two pieces live in different crates:

- `lodestone-render/src/entity.rs` — the version-free pose math (`display.ground`
  transforms, the bob, the spin, the hover lift, and the world-space mesher).
- `lodestone-shell/src/{entities.rs,gpu.rs}` — the plumbing: which item each drop
  is carrying, and the per-frame draw.

## How it works

```text
add_entity (type "item")  +  set_entity_data (index 8, ITEM_STACK)
  → ClientHandle::entities()      → EntityView { entity_type, item, .. }
  → NetClient::entity_snapshots() → EntitySnapshot { type_path: "item", item, .. }
  → EntityInterpolator            → EntityDraw { id, type_path, item, feet, anim }
  → RenderState::prepare_item_drops
      BlockModels::item_quads  +  entity::dropped_item_mesh
  → one ModelMesh, one draw, through ModelPipeline
```

`EntityModelSet::resolve` has no corpus entry named `item` and never will, so the
instanced entity pass skips a drop entirely; `prepare_item_drops` picks them out
of the same `&[EntityDraw]` by `type_path` before the pass opens.

### The placement, from `ItemEntityRenderer.submit` (26.2)

```text
box        = the GROUND-posed model's bounding box
minOffsetY = -box.minY + 0.0625            // ITEM_MIN_HOVER_HEIGHT
bob        = sin(ageInTicks/10 + bobOffs) * 0.1 + 0.1     // always 0.0..=0.2
spin       = ageInTicks/20 + bobOffs                      // radians

T(position) · T(0, bob + minOffsetY, 0) · Ry(spin) · display_matrix(ground)
```

`bobOffs` is a per-entity random in vanilla. We cannot observe the client RNG, so
`entity::item_bob_offset` hashes the server-assigned entity id instead: same
property (two drops do not pulse in lockstep), but a pure function of data a test
can see. Re-rolling it per frame would make the item jitter rather than spin.

### The winding invariant, and the way to get it backwards

`docs/item-gui-geometry.md` states that `gui_ortho * gui_item_pose` must match
`Camera::view_projection`'s determinant **sign**, which is negative. That is a
statement about the *composed* GUI matrix, and it does not transfer here.

A dropped item's pose is a **world-space model matrix**, left-multiplied by that
same `view_projection` exactly like a terrain section's. So the pose must not
flip anything: `det(dropped_item_matrix(..)) > 0`, and the composition inherits
the camera's negative sign. Reading the GUI rule as "the pose determinant must be
negative" and coding to it ships an item you are seeing the inside of — which
spins perfectly convincingly in a screenshot.
`entity::tests::dropped_item_pose_preserves_winding` derives the reference sign
from the camera rather than hardcoding either answer.

### Lighting

`mesh_item_quads` nails every vertex to `GUI_ITEM_LIGHT` (full bright), which is
correct for an inventory slot and wrong for a drop in a cave.
`entity::dropped_item_mesh` overwrites the light byte afterwards with the world
sample from `RenderState`'s `EntityLightSource` — the same source the mobs use.

## How to change it

- **The item's identity is wired end to end.** It rides entity metadata index 8
  under the `ITEM_STACK` serializer (see
  [Entity metadata: the item field](./entity-metadata-item.md)) and is folded at
  three places: `apply_metadata` in `lodestone-client/src/state.rs`,
  `entity_snapshot` in `lodestone-shell/src/net.rs`, and
  `EntityInterpolator::update` in `lodestone-shell/src/entities.rs`.

  Each of those carries a **nested** `Option`: outer is "has the field ever been
  reported", inner is "is a stack set". Collapsing it at any layer is the bug to
  avoid, and it is not hypothetical — a drop emits index 8 exactly once, at
  spawn, and sends item-free metadata thereafter, so a layer that reads "field
  absent" as "stack empty" makes every drop revert to a placeholder one tick
  after appearing. Only `EntityInterpolator` flattens, because at the draw both
  `None`s mean "draw nothing".

  `set_item_stack` remains public: `update` calls it from the snapshot, and it
  is still the direct seam for a caller that learns an identity another way.

- **`EntitySnapshot::item` is a `ResourceLocation`, not an `ItemStack`.**
  `EntitySnapshot` deliberately depends on neither `lodestone-client` nor
  `lodestone-model` — that is what makes `entities.rs` testable with no server
  and no GPU — so `net::entity_snapshot` is the single place that knows both
  types and converts. `count` and `components` are dropped there; see the two
  bullets below for what that costs. Widening `EntitySnapshot` with a plain
  `u32` count needs no model dependency and is the intended way to restore the
  visible half.

- **`display.ground` is not reachable** from a baked `ItemGeometry`.
  `lodestone-assets`' `icon.rs` keeps only the `gui` slot
  (`resolved.display.get("gui")`), discarding the rest of `ResolvedModel::display`,
  so `entity.rs` names vanilla's two ground transforms as constants
  (`BLOCK_ITEM_GROUND`, `GENERATED_ITEM_GROUND`) and picks between them on
  `gui_light`. Both are verbatim from 26.2's `block/block.json` and
  `item/generated.json`. If `IconPart::Model` ever carries the whole `display`
  map, replace `ground_transform_for` with a lookup and delete the constants.
  Posing a drop with the *gui* transform instead is the tempting shortcut and is
  visibly wrong in two ways at once: 30°/225° of tilt, and 2.5× the size.

- **Flat sprite items are the remaining hole, and it is a big one.**
  `collect_item_model_parts` keeps only `IconPart::Model` parts, so an
  `item/generated` icon (`gui_light: front`) never enters `BlockModels::items()`
  and a dropped stick, diamond or apple draws **nothing even though its identity
  decodes correctly**. That is the majority of items. Vanilla extrudes the sprite
  into a thin slab and fans a stack of them along `z`
  (`FLAT_ITEM_DEPTH_THRESHOLD`); that extrusion is not baked anywhere yet.
  `live_dropped_item.rs` asserts this explicitly (`minecraft:diamond` →
  `item == Some(minecraft:diamond)`, `item_drops_drawn == 0`) so it cannot be
  mistaken for the metadata chain failing.

- **Stack count is dropped at the `EntitySnapshot` boundary**, so a drop always
  renders one copy where vanilla renders up to five with a seeded jitter
  (`ItemEntityRenderer.getRenderedAmount`: 1 copy at count ≤ 1, then 2, 3, 4, 5
  as the count passes 1, 16, 32 and 48). The count *is* decoded — it reaches
  `EntityView::item` as a real `ItemStack` — and is discarded in
  `net::entity_snapshot`. Data components (dye colour, trim, custom model data)
  are discarded at the same point; unlike the count they change how an item looks
  rather than how many of it there are, and nothing in the item pipeline reads
  them.

- **Pickup animation.** `TakeItemEntity` *does* decode — into
  `ClientEvent::ItemPickup`, folded by `lodestone-game`'s `PickupFeed` — but
  nothing in the shell consumes that feed, so a collected item vanishes instead
  of arcing to the collector.

## Configuration

None. Every number is a vanilla constant in `lodestone-render/src/entity.rs`:
`ITEM_MIN_HOVER_HEIGHT`, `ITEM_BOB_AMPLITUDE`, `ITEM_BOB_TICKS_PER_RADIAN`,
`ITEM_SPIN_TICKS_PER_RADIAN`, `FLAT_ITEM_DEPTH_THRESHOLD`, and the two
`display.ground` transforms.

## Dependencies

- `lodestone-assets` — `BakedQuad`, `DisplayTransform`, `GuiLight`.
- `lodestone-render` — `BlockModels::items()` for the geometry snapshot,
  `item_render::display_matrix`, `models::mesh_item_quads`, `ModelPipeline`.
- The vanilla pack (`client.jar` + `blocks.json`): with no pack there is no model
  pass and no item geometry, so drops do not render on the offline demo path.

## Gates

- `lodestone-render`, `entity::tests` — bob range and period, spin period, phase
  distinctness and stability, hover lift, spin-about-the-position, the winding
  invariant, the light override.
- `lodestone-shell/tests/dropped_item_pixels.rs` — hermetic pixel gate: causes a
  drop, asserts a localised cluster of the right size, an opposite corner of 0,
  and two executed negative controls (no entity; entity with no stack). Bobbing
  is checked both ways: two phases must move the centroid, the same phase twice
  must differ by exactly 0 pixels.
- `lodestone-shell/tests/live_dropped_item.rs` — `/summon item` on the live
  oracle, nothing faked. Asserts the entity arrives as a tracked `EntityDraw`
  with type path `item` **and** `item == Some(minecraft:diamond_block)` decoded
  off the wire, renders it against a control built from the same entity with its
  identity removed (2383 lit px vs 0, opposite corner 0), then repeats the summon
  with `minecraft:diamond` to pin the sprite-item gap above.

  The negative control for the decode is the fold arm itself: deleting the
  `metadata.item` arm in `apply_metadata` makes the same summon arrive as
  `drop.item = None`, and this test fail. That was run, not assumed.
