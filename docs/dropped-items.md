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
  → RenderState::prepare_item_geometry
      BlockModels::item_quads  +  entity::dropped_item_mesh
  → one ModelMesh, one draw, through ModelPipeline
```

`EntityModelSet::resolve` has no corpus entry named `item` and never will, so the
instanced entity pass skips a drop entirely; `prepare_item_geometry` picks them out
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

- **`display.ground` *is* reachable now — this section said the opposite for a
  while, and that stale note was cited as fact.** `ItemGeometry::display` carries
  every one of the nine `display` slots, and `ground_transform(&display, gui_light)`
  reads the item's own declared `ground`. `BLOCK_ITEM_GROUND` and
  `GENERATED_ITEM_GROUND` survive as the **fallback** for a model chain that
  declares no `ground` at all (an undeclared slot would otherwise pose a full-size
  1×1×1 block lying in the grass), and `ground_transform_for` is only that
  fallback's `gui_light` keying. Both constants are verbatim from 26.2's
  `block/block.json` and `item/generated.json`.

  Posing a drop with the *gui* transform instead is still the tempting shortcut,
  and is visibly wrong in two ways at once: 30°/225° of tilt, and 2.5× the size.

- **Flat sprite items are no longer a hole, and the note that said they were
  outlived the fix by long enough to cause real damage.** `9980a96` added
  `extruded_sprite_geometry` — vanilla's `ItemModelGenerator` transcribed, a
  1/16-block slab with a `SOUTH` face, a u-reversed `NORTH` face and one edge quad
  per boundary texel of the sprite's alpha outline — and `BlockModels::build`
  inserts the result into **the same `items` map** the 3-D models go into, under
  the same key. So `BlockModels::item` answers a diamond exactly as it answers a
  stone, and the drop pass cannot tell which baking path produced the geometry.

  The stale version of this bullet ("`collect_item_model_parts` keeps only
  `IconPart::Model`, so an `item/generated` icon never enters
  `BlockModels::items()`") was propagated verbatim into **four** GitHub issues
  (#33, #50, #54, #56) as their shared root cause. Three of the four had entirely
  different causes; see [Thrown projectiles](./thrown-projectiles.md) and
  [First-person held item](./first-person-held-item.md). The cost of a stale note
  is not that it is wrong — it is that it is *specific and plausible*, so nobody
  re-checks it.

  Two pieces of vanilla's flat-item handling are genuinely still missing: the
  multi-copy fan along `z` for a large stack (`FLAT_ITEM_DEPTH_THRESHOLD`, which is
  defined and unused), and the stack count that would drive it — see the next
  bullet.

- **Stack count used to be dropped at the `EntitySnapshot` boundary; it no
  longer is, though the *draw* still renders exactly one copy.** The count is
  decoded — it reaches `EntityView::item` as a real `ItemStack` — and
  `net::entity_snapshot` now reads `stack.count` into `EntitySnapshot::count:
  u32` (defaulting to `1` whenever there is no reported stack, never `0`, so a
  consumer that multiplies by count never draws zero copies of nothing).
  `entities.rs` carries it the rest of the way: `ItemStacks`'s map value
  widened from a bare `ResourceLocation` to a `TrackedStack { id, count }`,
  `fold_snapshots` records the real count instead of implicitly always `1`,
  and `extract_entity_draws` copies it onto `EntityDraw::count`. Hermetic tests
  (`item_count_reaches_the_draw`, `set_item_stack_with_count_is_recorded_and_reachable`
  in `entities.rs`; `entity_snapshot_carries_item_count_through` in `net.rs`)
  pin the chain, including the "no stack at all" control reading `1`.

  **The draw itself is still one copy regardless of count** — that half is
  outside `entities.rs`'s files. `gpu.rs::prepare_item_geometry` is what would
  turn `EntityDraw::count` into the extra `dropped_item_mesh` calls, and it is
  held; read from `.cache/mc/26.2/client-src/net/minecraft/client/renderer/entity/{ItemEntityRenderer,state/ItemClusterRenderState}.java`,
  not summarised:

  ```java
  // ItemClusterRenderState.java
  public static int getRenderedAmount(final int stackCount) {
     if (stackCount <= 1) return 1;
     else if (stackCount <= 16) return 2;
     else if (stackCount <= 32) return 3;
     else return stackCount <= 48 ? 4 : 5;
  }

  // ItemEntityRenderer.submitMultipleFromCount, amount = getRenderedAmount(count)
  if (modelDepth > 0.0625F) {           // FLAT_ITEM_DEPTH_THRESHOLD
     submit(pose);                      // the first copy, unperturbed
     for (i in 1..amount) {
        jitter = random_in(-0.15, 0.15) on each of x, y, z;
        submit(pose translated by jitter);
     }
  } else {                              // a flat sprite: fan along Z instead
     offsetZ = modelDepth * 1.5;
     translate(0, 0, -offsetZ * (amount - 1) / 2); submit(pose);
     for (i in 1..amount) {
        translate(0, 0, offsetZ);
        jitter = random_in(-0.075, 0.075) on x, y only;
        submit(pose translated by jitter);
     }
  }
  ```

  Two things worth knowing before landing it: vanilla branches on the
  posed model's own Z-depth against `FLAT_ITEM_DEPTH_THRESHOLD` (defined,
  unused, in `lodestone-render/src/entity.rs`) — a 3-D model jitters in X/Y/Z,
  a flat sprite instead fans along Z, evenly spaced, with a smaller jitter —
  and vanilla seeds its per-copy jitter from `RandomSource` keyed on
  `Item.getId(item) + damageValue`, which we cannot observe or reproduce
  bit-for-bit any more than `item_bob_offset` can observe the spawn-time RNG
  for bob phase. The precedent that function set — hash something we *can*
  see (there, the entity id) for the same *property* (two drops do not pulse
  in lockstep) rather than the exact bytes — is the right template here too;
  do not spend effort trying to match vanilla's `RandomSource` output exactly.
  Data components (dye colour, trim, custom model data) are still discarded at
  the `net::entity_snapshot` boundary; unlike the count they change how an item
  looks rather than how many of it there are, and nothing in the item pipeline
  reads them.

  **What landing it needs, concretely:**
  1. `lodestone-render/src/entity.rs` — a `posed_item_z_extent(quads, ground) ->
     (f32, f32)` mirroring `posed_item_y_extent` (same file, ~line 1274), so
     `prepare_item_geometry` can read the posed model's Z-size and pick the
     branch above.
  2. `lodestone-render/src/entity.rs` — a jitter function in
     [`item_bob_offset`]'s idiom, e.g. `item_cluster_jitter(id: i32, copy: u32)
     -> Vec3`, hashing `(id, copy)` rather than trying to reproduce
     `RandomSource`.
  3. `lodestone-shell/src/gpu.rs::prepare_item_geometry` — where it currently
     calls `dropped_item_mesh` once per drop, call `rendered_amount(draw.count)`
     (vanilla's `getRenderedAmount`, transcribed above) and loop that many
     times, merging one `dropped_item_mesh`-equivalent call per copy at
     `draw.feet + jitter` (copy `0` unperturbed, matching vanilla's own
     unperturbed first `submit`). This needs either a new `dropped_item_mesh`
     overload taking an extra world-space offset, or computing the offset
     `Vec3` here and adding it to `draw.feet` before the existing call — the
     latter needs no render-crate signature change at all.

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
  with `minecraft:diamond` and asserts **`item_drops_drawn == 1`** — the assertion
  that used to read `== 0` and was the visible marker of the sprite gap. It carries
  no pixel check, on purpose: the camera is aimed at the *block* item's summon
  position and the two items are summoned at different coordinates, so a
  "differing pixels > 0" assertion there would be a *world*-species vacuous test —
  pointed at a scene that structurally cannot contain its subject.

- `lodestone-render/tests/sprite_drop_pixels.rs` — the pixel evidence for the
  extruded slab specifically: a silhouette inside the item's own projected box,
  strictly smaller than that box (it is a cutout, not a slab), correlated against
  the sprite's **own alpha row profile read out of the atlas**, with the
  vertically-reversed profile required to score worse.

  The negative control for the decode is the fold arm itself: deleting the
  `metadata.item` arm in `apply_metadata` makes the same summon arrive as
  `drop.item = None`, and this test fail. That was run, not assumed.
