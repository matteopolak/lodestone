# First-person held item

## What it is

The item in the local player's hand in first person — vanilla's
`ItemInHandRenderer.submitArmWithItem` non-empty branch. It replaces the bare arm
rather than joining it, and it is one half of a fork: **the arm or the item, never
both.**

Companion docs: [Arm swing animation](./arm-swing-animation.md) for the *other*
half of the fork and the swing clock that drives both, and
[Dropped items](./dropped-items.md) for the shared item-model pipeline.

## How it works

```text
Sim (local player's selected hotbar slot)
  → RenderState::set_main_hand_source(|| Option<ResourceLocation>)   ← app.rs, per frame
  → RenderState::prepare_first_person_hand
      MainHandSource::value()                → Some(item) or None
      BlockModels::item                      → ItemGeometry (3-D model or sprite slab)
      entity::hand_transform(display, Right, /* first_person */ true)
      entity::first_person_item_mesh         → camera-space ModelMesh
  → FirstPersonHand::Item(GpuModelMesh)
  → the "first-person hand pass": depth CLEARED, ModelPipeline,
    group 0 = hand_projection alone
```

`None` — the default, the demo path, every headless test that does not opt in —
yields `FirstPersonHand::Arm`, which is exactly the behaviour before this existed.

### The chain, from `ItemInHandRenderer` (26.2)

```text
T(i·0.56, -0.52 + h·-0.6, -0.72)                      -- applyItemArmTransform
  · T(i·xs, ys, zs)                                   -- swingArm
  · Ry(i·(45 + yr·-20)) · Rz(i·xzr·-20)
      · Rx(xzr·-80) · Ry(i·-45)                       -- applyItemArmAttackTransform
  · display_matrix_for_hand(firstperson_?hand, is_left)
```

with `i = Arm::invert()`, `h = inverseArmHeight`, and

```text
xs  = -0.4 · sin(√a·π)      ys = 0.2 · sin(√a·2π)     zs = -0.2 · sin(a·π)
yr  =  sin(a²·π)            xzr = sin(√a·π)
```

### Three traps in that chain, all of them silent

- **The translation is `0.56 / -0.52 / -0.72`, not the arm's
  `0.64000005 / -0.6 / -0.71999997`.** The two chains are 0.08 blocks apart in
  `x`, which reads as a rounding difference and is the difference between an item
  in view and one clipping the frame edge. `first_person_item_chain` and
  `first_person_arm_chain` therefore share no code, only the `attackValue` scalar.
- **`swingArm`'s coefficients are `-0.4 / 0.2 / -0.2`, the arm's are
  `-0.3 / 0.4 / -0.4`.** Same shape, different amplitudes, and `ItemSwingTerms` is
  a separate type from `ArmSwingTerms` so autocomplete cannot swap them.
- **`applyItemArmAttackTransform` is the identity at rest**, because the leading
  `Ry(i·45°)` is cancelled exactly by the trailing `Ry(i·-45°)` when both shaping
  terms vanish. Dropping either rotation leaves a **permanent 45° twist** on a
  standing player, which no swing test would notice.
  `the_first_person_item_chain_is_a_plain_translation_at_rest` pins it.

### `hand_transform(display, arm, /* first_person */ true)`

The `true` matters and is the silent failure mode. `false` reads
`thirdperson_righthand`, which for `item/handheld` is a *different* rotation and
scale — the item still draws, still in the hand, just at a visibly wrong angle. The
transform plumbing itself (`DisplaySlot::FirstPersonRightHand`,
`Arm::display_slot`, `DisplayTransforms::get`'s left-hand fallback) has been
present since the `display` map crossed the asset boundary.

### The pass

Its own render pass, at the end of the frame, with **depth cleared** — vanilla does
`clearDepthTexture(..., 0.0)` immediately before `renderItemInHand`, and our `[0,1]`
DirectX depth makes the equivalent clear `1.0`. Without it the item is occluded by
any block within ~0.75 blocks of the eye, i.e. exactly when you are mining. A
missing clear shows up as *items occluded by world geometry*, not as a winding
fault.

Group 0 is **`hand_projection(aspect)` alone, with no view matrix**, because
`GameRenderer.renderItemInHand` multiplies the pose stack by
`modelViewMatrix.invert()` while pushing `modelViewStack.mul(modelViewMatrix)` and
the shader evaluates `Proj · ModelViewStack · PoseStack`: the view rotation cancels
exactly, leaving a camera-space pose. Feeding `Camera::view_projection` here parks
the item at the world origin, visible only when the player stands on it.

`hand_projection`'s FOV is a hard-coded **70°** (`Camera.calculateHudFov`), *not*
the player's FOV, so the item keeps a constant apparent size while sprinting or
while the FOV slider moves.

The two hand passes need **two camera buffers for one value** — the entity pipeline
(bare arm) and the model pipeline (held item) declare different group-0 layouts.
`RenderState::write_hand_camera` writes both together so they cannot drift.

### The item is geometry, not a texture

A held item goes through `ModelPipeline` with the *same* stitched block atlas, tint
palette and animation slots the terrain and the hotbar icons use. That is four bind
groups, which is exactly `wgpu`'s portable `max_bind_groups` floor — see
`CLAUDE.md`. Nothing here introduces a fifth.

## The wiring: landed

`RenderState::set_main_hand_source` is installed in `app.rs`, re-sampled every
frame alongside `set_hand_swing_source` from `self.sim.selected_slot()` against
the hotbar records the HUD frame already builds — the shape this section used
to specify as outstanding. `stats.first_person_item_drawn` is the check that
it landed; it and `first_person_arm_drawn` are mutually exclusive, and
**both** `false` in first person means the `player_wide` rig failed to load —
a real defect, not a quiet frame.

## World light (issue #74): "the hand is lit as if it were noon"

Vanilla lights the hand from the player's own light level, not full-bright —
`ItemInHandRenderer` submits through the normal entity path, so it picks up
`LightTexture` like any other entity. Reported in play as the arm/held item
staying lit while walking into darkness.

**What was already working, and had been for a while.** `RenderState::hand_light`
samples real per-position world light
(`self.entity_light.sample(camera.position)`, at the eye rather than the feet)
for **both** branches, and `write_hand_camera` already folded
`self.sky_darken.value()` into the **arm**'s `EntityCameraUniform` via
`EntityCameraUniform::with_sky_darken`. Both the light sampler and the
sky-darken source are installed in `app.rs` on both connect paths (the
`5832ecb` fullbright fix `docs/entity-rendering.md` records) — there was
nothing unwired at the source end.

**What was actually still broken: one hop later, and only for the item.**
`write_hand_camera`'s model-pipeline branch wrote the held item's
`model.hand_cam_buffer` with a bare `FogUniform::disabled()`. That leaves the
shared sky-darken lane (`fog.end_enabled[2]`, the same spare lane
`docs/time-of-day-lighting.md` documents) at its `0.0`/"unwired" sentinel, and
`model_pipeline.rs`'s `sky_darken()` reads `<= 0.0` as `1.0` — permanent noon.
So the arm (entity pipeline, lane correctly set) already dimmed at night while
the held item (model pipeline, lane left at the sentinel) stayed lit as if it
were noon, right next to it on the same screen — exactly the reported
symptom, and worse outdoors at night than in a literal unlit cave, where both
branches read block light 0 either way.

The fix: `write_hand_camera` now builds one `hand_fog` (`FogUniform::disabled()`
with `end_enabled[2]` set to `self.sky_darken.value()`, mirroring
`RenderState::fog_with_clock`'s own pattern for terrain and world mobs) and
passes the *same* value into both the entity uniform and
`update_model_shared_camera_buffer`, so the two branches cannot drift apart
again.

**The trap this correction avoids**: darkening the whole `light_term` rather
than only the sky half, which blacks out a torchlit hand at night — the exact
mistake `docs/time-of-day-lighting.md`'s
`a_torch_lit_mob_is_identical_at_midnight_and_noon` gate exists to catch for
world mobs. Nothing needed changing here: both shaders already compute the same
light term (now vanilla's `lightmap.fsh` curve, see
[light-ramp.md](./light-ramp.md)), scaling only the
sky half, and gamma-space multiply
(`srgb_to_linear(linear_to_srgb(rgb) * shade)`) was likewise already correct
in both. The bug was purely in which `FogUniform` reached the buffer, not in
any shader math.

## How to change it

- **The three swing animation types.** 26.2 branches on
  `itemStack.getSwingAnimation().type()`: `WHACK` runs `swingArm`, `STAB` runs
  `SpearAnimations.firstPersonAttack`, `NONE` runs nothing. Only `WHACK` is
  modelled. At `attack_anim == 0` **all three are the identity**, so a resting hand
  is correct for every item whatever its type; mid-swing a spear gets `WHACK`'s
  motion. Fixing it needs the item's `SwingAnimation` component, which the item
  pipeline does not decode.
- **`inverseArmHeight` is hardcoded `0.0`**, so the item never dips and rises on a
  hotbar change. It is `swapAnimationScale(item) · (1 - lerp(oHeight, height))` and
  the shell tracks neither height — the same gap the bare arm has.
- **The special-cased poses are all absent here**: bow and crossbow while drawing,
  shield, spyglass, map (one- and two-handed), trident, and the eating/drinking and
  brush animations. Each is its own branch in `submitArmWithItem`. An item in one of
  those categories draws through the generic chain, which is the resting pose —
  wrong while in use, correct while merely held.

  **"needs use-item state the shell does not track" is no longer true** — that was
  the state before issue #57. The using-item bit is decoded and folded all the way to
  a `lodestone_ecs::entity::ItemUse` component, and third-person mobs and remote
  players draw the bow/crossbow arm pose from it
  ([`item-use-arm-poses.md`](./item-use-arm-poses.md)). What is still missing *for
  this pass* is two specific things, and neither is the state:

  1. **The local player cannot reach `ItemUse` through the generic path.** It has no
     `EntityKind`/`Position`/`Rotation`/`HeadYaw` (deliberately — that absence keeps
     a self-model off `ClientHandle::entities()`), so `entity_view()`'s early `?`
     returns before the flags are read. It needs a session-scoped fold of the same
     shape as `ingest::apply_local_player_on_fire` (`7822a60`) plus a
     `PlayerSnapshot` field.
  2. **`ItemInHandRenderer`'s own transforms**, which are *not* the humanoid arm
     poses and are separate work: `case BOW` is a translate/rotate/scale on the item
     pose stack driven by `power = (t² + 2t)/3` clamped at 1, with a shake term
     above `power > 0.1`, and the crossbow case has its own distinct constants.
- **The off hand is not drawn at all.** `Arm::Left` is fully supported by every
  function here (`invert`, the left-hand `display` mirror), but
  `prepare_first_person_hand` only ever asks for `Arm::Right`. Adding the off hand
  needs a second source and a second mesh, not new math.
- **An item with no baked geometry falls back to the arm**, not to nothing —
  `IconPart::Special` items (chests, shulkers, shields, banners) have no geometry,
  and vanilla would draw their special renderer, which a bare arm is closer to than
  an empty screen.

## Configuration

None. `FIRST_PERSON_ITEM_OFFSET`, `FIRST_PERSON_ITEM_EQUIP_DIP`,
`HAND_FOV_Y_DEGREES`, `HAND_NEAR` and `HAND_FAR` are vanilla constants in
`lodestone-render/src/entity.rs`.

## Dependencies

- `lodestone-assets` — `BakedQuad`, `DisplaySlot`, `DisplayTransform`,
  `DisplayTransforms`, `GuiLight`, `ResourceLocation`.
- `lodestone-render` — `entity::{first_person_item_chain,
  first_person_item_attack_chain, first_person_item_matrix, first_person_item_mesh,
  hand_transform, hand_projection}`, `BlockModels::item`, `ModelPipeline`.
- `lodestone-shell` — `RenderState::{set_main_hand_source,
  prepare_first_person_hand, write_hand_camera}`, `HandSwingSource` for the swing
  scalar, `EntityLightSource` for the light byte.
- The vanilla pack: no pack, no model pass, no item geometry — the bare arm draws
  instead, since the `player_wide` rig is code-authored in `lodestone-assets`.

## Gates

In `lodestone-render/tests/thrown_and_held_item_pixels.rs`:

- `the_first_person_item_chain_is_a_plain_translation_at_rest` — hermetic. The rest
  chain must reduce to `applyItemArmTransform`'s translation alone (max element
  error `< 1e-5`), which is the `Ry(45)/Ry(-45)` cancellation, plus the left-hand
  mirror on `x` only.
- `the_first_person_item_pose_takes_the_world_winding_rule` — hermetic. The
  reference sign is **derived from a real camera** (`Camera::view_projection`'s
  determinant), never asserted: `hand_projection` must share it, the pose must be
  `det > 0` for both arms at five swing phases, and the composition must match the
  camera's sign. Coding to the GUI rule (negative) instead ships an inside-out item
  that still looks plausible in a screenshot.
- `the_first_person_item_lands_in_the_bottom_right_of_frame` — GPU + jar,
  `#[ignore]`d. Measured for a `diamond_pickaxe`: **7124** lit pixels, **all** of
  them in the bottom-right quadrant, none in the other three. The **executed**
  negative control mirrors the posed mesh in `x` about the eye — an `Arm::invert`
  sign error — and lands **0** bottom-right / **7123** bottom-left, i.e. it *fails
  the subject's own assertion*, which is what makes the quadrant floor real rather
  than satisfiable by any item anywhere.

  **A square viewport draws zero item pixels, and that is not a bug.**
  `applyItemArmTransform` puts the item 0.56 blocks right of the eye and 0.72
  forward, and `hand_projection`'s 70° FOV is *vertical*, so the horizontal
  half-angle grows with aspect. Measured on the same working build: 256×256 → **0**
  pixels, aspect 1.5 → **2722**, 16:9 → **4191**. The gate therefore renders 448×256.
  A gate on a square target would have read as "the held item does not render" and
  sent the next reader hunting a chain bug that does not exist.

In `crates/lodestone-shell/tests/first_person_hand_light_pixels.rs` (GPU +
jar, `#[ignore]`d, 448×256 for the same reason above) — issue #74's gate,
driving the real `RenderState::render` path for **both** branches
separately, since they draw through different pipelines and a fix to one
does not imply the other:

- `the_first_person_arm_dims_with_the_world_at_night` and
  `the_first_person_held_item_dims_with_the_world_at_night` each render at
  noon (`sky_darken = 1.0`) and at midnight (`sky_darken = 0.24`, with
  `entity_light` pinned to `sky=15, block=0` so the sky term is what actually
  moves) and assert the hand's mean channel value drops by a real margin, with
  a same-`sky_darken`-twice determinism control that must be pixel-identical.
  Measured on the real jar: arm noon **32.20** → midnight **4.97** (5131 px);
  item noon **24.15** → midnight **3.67** (7124 px — the same figure
  `thrown_and_held_item_pixels.rs` measures for the same `diamond_pickaxe`).
- **The executed negative control**: reverting `write_hand_camera`'s item
  branch to the pre-fix `FogUniform::disabled()` (no sky-darken lane) makes
  `the_first_person_held_item_dims_with_the_world_at_night` fail with noon and
  midnight reading the **identical** `24.15` — proving the gate is sensitive
  to the actual bug, not just to *a* difference between the two renders. The
  arm test still passes unmodified in that same run, since the arm's own
  uniform write was never the broken half.
