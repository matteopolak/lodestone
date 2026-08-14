# Thrown projectiles

## What it is

The render path for the nine entity types vanilla draws with `ThrownItemRenderer`
— snowball, egg, ender pearl, experience bottle, splash potion, lingering potion,
eye of ender, fireball and small fireball. Each is drawn as its **item model**,
posed by `display.ground`, turned to face the camera, at the entity's position.

Before this landed, **no projectile of any kind rendered**: `entity_models.rs` has
no corpus entry for any of them, so `model_for_type` missed and nothing was drawn
while the entities were tracked, interpolated and counted perfectly.

Arrows, spectral arrows and tridents have since landed on their own path
— see [`projectile-renderers.md`](./projectile-renderers.md). The other
projectile shapes are **still missing**; see
[What is deliberately not here](#what-is-deliberately-not-here).

## How it works

```text
add_entity (type "snowball") + set_entity_data (ITEM_STACK)
  → EntityView { entity_type, item, .. }
  → EntitySnapshot { type_path: "snowball", item, .. }
  → EntityInterpolator → EntityDraw { id, type_path, item, feet, anim }
  → RenderState::prepare_item_geometry
      entity::thrown_item_for(type_path)        → ThrownItem { item, scale, full_bright }
      BlockModels::item                          → ItemGeometry
      entity::camera_orientation(view_matrix)    → the billboard rotation
      entity::thrown_item_mesh                   → world-space ModelMesh
  → merged into the *same* mesh drops and held items use, one draw call
```

`RenderState::merge_thrown_item` (`lodestone-shell/src/gpu.rs`) is the whole
consumer. It sits on the `type_path != "item"` branch of `prepare_item_geometry`,
*before* `merge_held_items` — a projectile carries no equipment, so the held-item
scan is skipped for it.

### The pose, from `ThrownItemRenderer.submit` (26.2)

The entire vanilla method is three lines:

```java
poseStack.scale(this.scale, this.scale, this.scale);
poseStack.mulPose(camera.orientation);
state.item.submit(poseStack, ..., state.lightCoords, ...);
```

with the entity's position already on the pose stack, and `state.item` resolved by
`extractRenderState` in **`ItemDisplayContext.GROUND`**. So:

```text
T(position) · S(scale) · camera_orientation · display_matrix(ground)
```

`GROUND` is why `ground_transform` is shared with the dropped-item path rather
than duplicated — a thrown snowball and a snowball lying on the floor use the same
`display` slot.

**No bob, no spin, no hover lift.** Those three are `ItemEntityRenderer`'s and are
the tempting thing to reuse from `dropped_item_matrix`; a bobbing, spinning
snowball in flight is the signature of having done so.

### The registration table is not uniform

`entity::thrown_item_for` is the 26.2 `EntityRenderers.java` list read directly:

| entity | item | scale | `fullBright` |
|---|---|---|---|
| `egg` | `minecraft:egg` | 1.0 | no |
| `ender_pearl` | `minecraft:ender_pearl` | 1.0 | no |
| `experience_bottle` | `minecraft:experience_bottle` | 1.0 | no |
| `eye_of_ender` | **`minecraft:ender_eye`** | 1.0 | **yes** |
| `fireball` | `minecraft:fire_charge` | **3.0** | **yes** |
| `lingering_potion` | `minecraft:lingering_potion` | 1.0 | no |
| `small_fireball` | `minecraft:fire_charge` | **0.75** | **yes** |
| `snowball` | `minecraft:snowball` | 1.0 | no |
| `splash_potion` | `minecraft:splash_potion` | 1.0 | no |

Three columns each contain a trap:

- **`eye_of_ender`'s item id is `ender_eye`, not `eye_of_ender`.** The entity type
  and the item have different names. A table derived from the entity name resolves
  no geometry for it and draws nothing — silently, since a missing item is a
  legitimate early return.
- **`fireball` is 3.0 and `small_fireball` is 0.75**, a factor of four. Treating
  the scale as uniform makes a ghast's fireball the same size as a blaze's.
- **`fullBright` is `getBlockLightLevel` overridden to `15`**, which maps onto
  `ENTITY_FULLBRIGHT`. A fireball that samples world light is invisible against a
  dark Nether ceiling, which is exactly where you meet one.

### Which item id actually gets drawn

`EntityDraw::item` first, the table second. `ThrowableItemProjectile`, `Fireball`
and `EyeOfEnder` all sync their stack through `DATA_ITEM_STACK` — the **same**
`ITEM_STACK` serializer at the same metadata index a dropped item uses — and
`lodestone-ecs`'s `apply_entity_metadata` inserts `DisplayItem` for *any* entity
type, not just `item`. So the wire value arrives with no new plumbing.

The table is the fallback for the case the wire cannot cover: vanilla only marks
the field dirty when a constructor *sets* it, so a snowball thrown by a snow golem
(the position-only constructor) arrives with the field never reported.

**Today the fallback is the *only* path taken, and that is a one-line gate, not a
missing decode.** `fold_snapshots` inserts into `ItemStacks` for **any** entity
type, so the stack is present in the ECS — but `extract_entity_draws`
(`lodestone-shell/src/entities.rs`) narrows it on the way out:

```rust
item: (kind.0 == ITEM_ENTITY_TYPE_PATH)
    .then(|| stacks.0.get(&id.0).cloned())
    .flatten(),
```

Widening that predicate to `kind.0 == ITEM_ENTITY_TYPE_PATH || thrown_item_for(&kind.0).is_some()`
(or simply dropping it — `ItemStacks` only ever holds an entry for an entity that
reported one) is what lets a splash potion of harming and one of healing differ once
data components are decoded. Nothing renders wrong without it, because every entry
in the table above happens to equal its entity's only possible stack; that is why
this is documented rather than urgent.

### The billboard rotation, and why it is derived rather than written out

`entity::camera_orientation(view_matrix)` strips the translation from the view
matrix and transposes it. That is provably the camera→world rotation — a view
matrix is `R · T` with `R` orthonormal, so `R⁻¹ = Rᵀ` — and it cannot get any
convention backwards.

Writing it out by hand was tried and was wrong three times in three different
ways, because three conventions stack:

- vanilla's own quaternion is `rotationYXZ(π - yRot, -xRot, 0)`. **Note the
  `π -`**: MC's camera space is rotated 180° from its world space, so the naive
  `Ry(yaw)·Rx(pitch)` is a semitone off by exactly a half turn;
- `glam`'s right-handed view looks down **-Z**, so "toward the viewer" is `+Z`;
- `Camera::forward` is Minecraft's convention (`yaw 0` faces `+Z`), which is not
  the same as either of the above.

A 180° yaw error here would have been **invisible on every item in the table**,
which is the reason to be careful rather than a reason to relax: every one of the
nine is a flat sprite, and `ItemModelGenerator` gives the `NORTH` face reversed
`u` UVs precisely so both faces read unmirrored from their own side. What is *not*
invisible is a wrong pitch term (an upside-down snowball) or no rotation at all (a
1/16-block slab seen edge-on — a near-invisible sliver, measured at 494 pixels
against the billboard's 3788).

## What is deliberately not here

- **Arrows, spectral arrows and tridents.** Still not *here* — but they now exist,
  on their own path: see [`projectile-renderers.md`](./projectile-renderers.md).
  `ArrowRenderer` / `ThrownTridentRenderer` draw a real 3-D mesh oriented by
  `atan2` of the entity's **velocity**, which is not a variant of this path and
  should not be bolted onto it.

  This entry used to say "`EntityDraw` carries no velocity, so this needs a data
  change as well as a mesh". True premise, wrong conclusion, and it cost the next
  agent a design: the *server* runs the same `atan2` (`AbstractArrow.tick`,
  `Projectile.shoot`) and broadcasts the result as ordinary entity rotation, so
  `EntityDraw::{yaw, pitch}` already **are** the velocity-derived angles. No data
  change was needed — only a different placement matrix. The `shakeTime` wobble is
  genuinely unreachable (see the other doc).
- **`wind_charge` and `breeze_wind_charge` are not thrown items**, though they read
  like they should be. Both use `WindChargeRenderer`, a real cuboid model, and
  `AbstractWindCharge.getItem()` returns `ItemStack.EMPTY` — there is no item
  sprite to billboard.
- `dragon_fireball`, `wither_skull`, `llama_spit`, `shulker_bullet`,
  `fishing_bobber` (which needs a line as well as a bob), `firework_rocket`,
  `end_crystal`, `falling_block`, `tnt`, item frames and paintings all have their
  own dedicated renderers and are likewise absent.

`thrown_item_for` returns `None` for every one of these, and
`the_thrown_item_table_matches_the_26_2_registrations` asserts it does — so adding
one by guessing fails a test rather than shipping a wrong billboard.

## How to change it

- **Adding a `ThrownItemRenderer` type** is one row in `thrown_item_for`'s table
  plus one row in the gate's expected list. Check the entity's `getDefaultItem()`
  in `.cache/mc/26.2/src`, not the entity name.
- **Adding a *different* renderer shape** does **not** go here.
  `prepare_item_geometry` dispatches on `thrown_item_for` returning `Some`; a
  velocity-oriented mesh needs its own corpus model and its own placement — which is
  exactly what arrows got in
  [`projectile-renderers.md`](./projectile-renderers.md), through
  `EntityModelSet::resolve_posed` rather than through this path at all.
- **The wire's stack does not reach a projectile yet** — one predicate in
  `extract_entity_draws` (`entities.rs`), described under
  [Which item id actually gets drawn](#which-item-id-actually-gets-drawn). Harmless
  today; needed before per-stack variants can show.
- **The stack's data components are dropped** at the `EntitySnapshot` boundary
  (`net::entity_snapshot` keeps only the item key), so a splash potion of harming
  and a splash potion of healing draw the same untinted bottle. That is the same
  loss `docs/dropped-items.md` records for drops, in the same place.
- **`ThrownItem::full_bright` maps to a light byte, not to a shader flag.** If the
  entity lighting model changes, this is one of the two places that hardcodes
  `ENTITY_FULLBRIGHT` for a non-GUI object.

## Configuration

None. Every number is a vanilla constant: the table above, and the `GROUND`
display transforms already documented in
[Dropped items](./dropped-items.md#configuration).

## Dependencies

- `lodestone-assets` — `BakedQuad`, `DisplayTransform`, `GuiLight`,
  `ResourceLocation`.
- `lodestone-render` — `entity::{thrown_item_for, camera_orientation,
  thrown_item_mesh, ground_transform}`, `BlockModels::item`, `ModelPipeline`.
- `lodestone-shell` — `EntityDraw` (type path + item + interpolated position),
  `EntityLightSource`, `RenderState::prepare_item_geometry`.
- The vanilla pack (`client.jar` + `blocks.json`): with no pack there is no model
  pass and no item geometry, so projectiles do not render on the demo path — the
  same degradation drops have.

## Gates

- `lodestone-render/tests/thrown_and_held_item_pixels.rs`:
  - `the_thrown_item_table_matches_the_26_2_registrations` — hermetic. The nine
    rows against the 26.2 source, **and** twelve types that must be absent.
  - `the_billboard_is_a_pure_rotation_that_stays_upright` — hermetic. At six yaws:
    `det == +1`, item-local `+Y` maps to world `+Y`, and item-local `+Z` (the
    textured `SOUTH` face) points back at the eye with `dot > 0.99`. This is the
    assertion a pixel count structurally cannot make — an upside-down snowball
    covers exactly as many pixels as an upright one.
  - `a_thrown_snowball_draws_a_silhouette_and_the_edge_on_control_does_not` —
    GPU + jar, `#[ignore]`d. Measured: **3788** lit pixels, all inside the
    projectile's own projected box, strictly fewer than the box's 8836 (a cutout,
    not a slab), top-left corner 0. The **executed** negative control is the same
    mesh through the same pass with `Mat4::IDENTITY` in place of the orientation:
    **494** pixels, a 7.7× separation.

    The control's ceiling is 20%, not the ~6% a 1/16-thick slab's face-to-edge
    ratio predicts. A 10% ceiling was tried and **failed on a working build**:
    `ItemModelGenerator` fans one edge quad per boundary texel of the alpha
    outline, and seen side-on those quads are the widest thing left, so the sliver
    is about twice as bright as the naive ratio.

- `lodestone-shell/tests/dropped_item_pixels.rs::a_thrown_snowball_reaches_pixels_through_the_real_render_call`
  — GPU + jar, `#[ignore]`d. **The island check**, and the one the render-crate gate
  structurally cannot make: it drives `RenderState::render` — the same call `app.rs`
  makes, with no extra argument — and asserts `projectiles_drawn == 1` plus a
  localised pixel cluster. Measured: **1204** differing pixels in a 39×39 box at the
  projectile's screen position, far corner 0, `projectiles_drawn` = 1 subject / 0
  empty / 0 for a `pig`.

  Its `EntityDraw::item` is deliberately `None`, because that is what
  `extract_entity_draws` produces for a non-`item` entity — so the gate exercises the
  default-item fallback, which is the path a live frame actually takes.

  **The control that had to change, and why the change is the interesting part.** The
  first version asserted that a `pig` at the same position produced a frame identical
  to the empty one. It failed at **10254** differing pixels: `pig` *does* have a
  corpus model, and the entity pass drew it correctly. "An entity of an unregistered
  type draws nothing" is simply false. The control is now the **same snowball placed
  behind the camera** — `projectiles_drawn == 0` and zero differing pixels — which is
  independent of the entity-model corpus and also proves the frustum cull is live
  rather than dead code. The `pig` case survives as a *counter*-only assertion.
