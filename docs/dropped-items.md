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

- **Stack count reaches the draw, and the draw makes vanilla's 1-5 copies.**
  The count is decoded into `EntityView::item` as a real `ItemStack`, narrowed
  onto `EntityFacts::count` (defaulting to `1` whenever there is no reported
  stack, never `0`), carried by `ItemStacks`'s `TrackedStack { id, count, foil }`
  and copied onto `EntityDraw::count` by `extract_entity_draws`.

  `prepare_item_geometry` (`gpu/world_items.rs`) then draws
  `lodestone_render::entity::rendered_amount(count)` copies — 1, then 2 above 1,
  3 above 16, 4 above 32, 5 above 48, transcribed from
  `ItemClusterRenderState.getRenderedAmount`. Copy `0` is unperturbed, matching
  vanilla's own first unperturbed `submit`; the rest are offset by
  `item_cluster_jitter(entity_id, copy, extent)`.

  Two things about the branch:

  - **It is the *posed* model's z-depth that picks it**, via
    `posed_item_z_extent` against `FLAT_ITEM_DEPTH_THRESHOLD` (`0.0625`). A solid
    model jitters `±0.15` on all three axes; a flat sprite instead **fans** its
    copies evenly along z at `depth * 1.5`, centred on the entity, and jitters
    only `±0.075` on x and y. Getting the branch backwards makes a stack of
    blocks look like a flat fan and a stack of sticks like a cloud.
  - **The jitter is a hash, not vanilla's RNG.** Vanilla seeds it from a
    `RandomSource` keyed on `Item.getId(item) + damageValue`, which we cannot
    observe — the same situation `item_bob_offset` is in for the bob phase. So
    `item_cluster_jitter` hashes `(entity_id, copy)` for the same *property* (no
    two drops and no two copies scatter in lockstep) rather than chasing bytes.
    Do not spend effort trying to match vanilla's output exactly.

  Data components other than dye and foil (trim, custom model data) are still
  discarded before the draw; unlike the count they change how an item looks
  rather than how many of it there are.

- **Enchanted drops glint.** `EntityFacts::foil` narrows
  `lodestone_render::glint::has_foil` off the reported stack's components and
  rides the same `TrackedStack` -> `EntityDraw` path as the count.
  `prepare_item_geometry` returns a **second** mesh holding only the enchanted
  items' quads, and `frame.rs` re-rasterises it through the glint pipeline in the
  *same* render pass, immediately after the base item draw. Both properties are
  load-bearing: the glint pipeline compares depth `EQUAL`, so it can only shimmer
  where the base draw has just written depth, and the two meshes are merged from
  one `dropped_item_mesh` call per copy so their vertices cannot diverge.

  The glint's group-0 uniform is its **own** buffer
  (`GlintPass::world_uniform_buffer`), not the hand's. `queue.write_buffer` is
  ordered against the submit rather than against the encoder, and the world items
  and the first-person hand draw in different passes of one submit — so a single
  buffer written twice would hand both passes the last value and the shimmer
  would land nowhere.

- **Pickup animation — landed** (issue #365), see
  [item-pickup-animation.md](./item-pickup-animation.md). The flight reuses this
  page's draw path exactly: it emits an ordinary `EntityDraw` with
  `type_path == "item"`, so `prepare_item_geometry` needed no change. This bullet
  used to read "nothing in the shell consumes that feed, so a collected item
  vanishes"; the missing hop was one arm in `net.rs`'s `forward`.

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
