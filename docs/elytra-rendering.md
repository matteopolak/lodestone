# Elytra rendering

## What it is

The elytra's two wings as a wearable layer over the humanoid rig: a mesh baked
from `ElytraModel.createLayer`, a per-wing transform posed off the wearer's own
`body` part matrix, and the three-way glide/crouch/rest pose that
`ElytraAnimationState` lerps toward. Geometry, pose and the GPU draw are all
landed; the wings reach pixels. What is **not** landed is the per-tick
animation state, so every wearer is posed at the resting triple — see "The
pose is the resting one" below for what that is right and wrong for.

## What exists

| symbol | crate |
| --- | --- |
| `lodestone_assets::entity::elytra_model` | the two-wing mesh definition, 64×32 |
| `lodestone_assets::entity::ELYTRA_TEXTURE_PATH` | `textures/entity/equipment/wings/elytra.png` |
| `lodestone_render::ElytraMesh` / `ElytraWing` | the baked mesh and its two parts |
| `lodestone_render::elytra_wing_transform` | the per-wing local matrix |
| `lodestone_render::elytra_target_rotations` | `ElytraAnimationState.tick`'s branch |
| `lodestone_render::elytra_rest_rotations` | the standing triple, `(π/12, 0, −π/12)` |
| `lodestone_render::ELYTRA_ROTATION_LERP` | the per-tick approach rate, `0.3` |
| `crates/lodestone-render/tests/elytra_wings.rs` | six hermetic gates over both |
| `lodestone_shell::gpu`'s `prepare_elytra` | the per-frame instance batches |
| `EntityRenderer::elytra_model`/`elytra_gpu`/`elytra_texture` | the bake, its upload and the jar sheet |
| `RenderStats::elytra_wings_drawn` | wings, not wearers — an odd count is a defect |
| `crates/lodestone-shell/tests/elytra_wings_pixels.rs` | the pixel gate over the real draw |

Companion to [`player-capes.md`](./player-capes.md), which shares the
`"body"`-part attachment discipline and nothing else, and
[`armour-rendering.md`](./armour-rendering.md), which owns the chest slot the
elytra is worn in.

## How it works

### The mesh: two mirrored wings on a 64×32 sheet

`ElytraModel.createLayer` declares two 10×20×2 boxes inflated by 1.0, sharing
one `texOffs(22, 0)` unwrap — the right wing is the mirrored copy, which is
what lets one unwrap serve both sides. The sheet is **64×32**, not the 64×64
the player rig and the cape model both declare; a 64×64 assumption halves every
V and paints the wings from the wrong half of the strip.

### Why no rotation is baked, and why that is *not* the cape's reason

Both `player_cape_model` and `elytra_model` drop their authored `PartPose`
rotation, and they do it for opposite mechanical reasons:

| model | `setupAnim` does | consequence |
| --- | --- | --- |
| cape | **composes** a quaternion whose leading term is the pose rotation's inverse | the two cancel; baking it would double it |
| elytra | **assigns** `xRot`/`yRot`/`zRot` outright | the pose rotation is overwritten every frame |

The conclusions coincide and the reasons do not, and the difference is
load-bearing: `ElytraModel.setupAnim` also assigns `y` (3 texels when
crouching) while leaving `x` and `z` alone, so the wings' `±5` pivot X **must**
be baked and their `y` **must not** be. A port by analogy with the cape gets
the rotation right by accident and the `y` wrong.

The authored angles are not lost either — `ElytraAnimationState`'s
not-flying-not-crouching target is the same `(π/12, 0, −π/12)` triple, which is
why a standing player's wings look like the authored pose.

### The per-wing transform

`elytra_wing_transform` composes, relative to the wearer's `body` matrix:

```text
T(0, 0, 0.125) · T(pivot_x/16, y/16, 0) · Rz(z) · Ry(y) · Rx(x)
```

* The leading translate is `WingsLayer.submit`'s
  `poseStack.translate(0, 0, 0.125)`, applied to the layer as a whole and so
  **outside** the wing's own pivot. Units there are blocks — `ModelPart.render`
  is what divides texels by 16 — so 0.125 is 2 texels. Numerically the same as
  the cape's `z = 2` pivot; a different quantity with a different origin.
* Rotation order is `Rz · Ry · Rx`, matching JOML's
  `rotationZYX(zRot, yRot, xRot)` that `ModelPart.translateAndRotate` uses. It
  is *not* the `Rx · Rz · Ry` the cape ends up with, which is a composed
  quaternion chain rather than a part pose.

The right wing negates `yRot` and `zRot` and shares `xRot`. That is not
transcription: it follows from the model's mirror symmetry. Writing
`S = diag(−1, 1, 1)`, `Rx` mixes only Y and Z and so commutes with `S`, while
`Ry` and `Rz` both mix X and therefore invert — so `right = S · left · S`
exactly, which predicts the three signs vanilla uses.
`right_wing_is_the_left_wing_mirrored_through_x` asserts that identity and
carries the control showing an `xRot`-negating wing does *not* satisfy it.

### The pose branch, and two coincidences to avoid

`elytra_target_rotations` is `ElytraAnimationState.tick`'s three-way branch,
with fall-flying taking precedence over crouching (a player can be both).
Two inputs make a fixture vacuous:

* **A vertical dive returns the resting triple.** `motion = (0, −1, 0)`
  normalises to `y = −1`, so `ratio = 1 − 1^1.5 = 0` and both lerps return
  their `start`, which is exactly the not-flying branch's answer. The steepest
  possible glide and standing still are *indistinguishable*.
* **Level flight is the other endpoint**, short-circuiting `ratio` to 1.

The discriminating input is a shallow dive: `motion = (0.5, −0.5, 0)` gives
`ratio = 1 − (1/√2)^1.5 = 0.4052…`, a value no other branch produces.
`a_shallow_dive_lands_strictly_between_the_two_glide_endpoints` asserts that
property rather than assuming it.

## The draw

`RenderState::prepare_elytra` (`gpu/entity_passes.rs`) is the cape pass's
sibling and runs beside it every frame. Three things differ from the cape:

* **The gate is the chest slot**, not a skin field — the same
  "the chest item's path is literally `elytra`" predicate `prepare_cape` uses
  to *suppress* the cape. The two must stay one predicate: if they ever
  disagree a wearer can lose the cape and gain no wings, which is exactly the
  state this feature shipped in between capes landing and this pass existing.
  `WingsLayer.submit`'s real gate is an `Equippable` with a non-empty
  `assetId` whose asset declares a `WINGS` layer, which for every vanilla item
  means the elytra and nothing else.
* **Two instances per wearer**, one per `ElytraMesh::attach` entry, each
  carrying its own `elytra_wing_transform` composed onto the wearer's `body`
  matrix. `RenderStats::elytra_wings_drawn` therefore counts wings: an odd
  number means the bake produced one wing, which is the "half the elytra is
  missing" symptom `ElytraMesh::load`'s own `!quads.is_empty()` filter can
  produce.
* **The batch key is `(texture, wing)`.** The two wings are different geometry
  and cannot share an instanced draw; the texture is the jar sheet for almost
  everyone and the wearer's own **cape** sheet when they have one.
  `WingsLayer.getPlayerElytraTexture` prefers `skin.elytra()` first, and that
  preference is **not** wired: `lodestone_shell::remote_skins::RemoteSkin`
  carries no `elytra` field, so `ProfileTextures::elytra` is dropped at the
  decode. Adding it is a `RemoteSkin` change, not a render one.

Drawn through the **base** entity pipeline, immediately after the cape, for
the same reason wool and the cape are: no second layer at the same inflation
to correct z-fighting for.

### The pose is the resting one, always — a deliberate first cut

`elytra_target_rotations` is the pure half of `ElytraAnimationState`; the
impure half is two lerped triples (`rot*`, `rot*Old`) advanced once per game
tick by `current += (target − current) * ELYTRA_ROTATION_LERP` and read back
interpolated by partial ticks. That belongs beside
`lodestone_shell::entities::cape_sway`'s lagged cloak position, and it does
not exist yet. Until it does, `prepare_elytra` passes
`elytra_rest_rotations()` and `crouching: false` straight through.

**What that is right and wrong for:** correct for every wearer who is
standing, walking or running — the rest triple *is* the
not-flying-not-crouching branch's target — and wrong during a **glide** or a
**crouch**, where the wings stay spread instead of folding back. The check a
reader can run without trusting this paragraph: `EntityDraw` carries no
fall-flying flag and no crouch flag, so there is no input reaching
`prepare_elytra` that could select either of the other two branches. Closing
this means adding that state, not editing the pass's arithmetic.

### What the pixel gate covers, and what it does not

`crates/lodestone-shell/tests/elytra_wings_pixels.rs` drives the real
`RenderState::render` and measures the pixels that change between an
elytra-wearing zombie and the same zombie with an empty chest slot, bracketed
against the two wings' analytic projected silhouette (the summed shoelace area
of their front-facing quads, cross-checked against the back-facing sum) and
localised to the wings' own projected rect.

Measured: **4710** changed pixels against a **7044** px analytic silhouette
(67%), and **0** with the draw-loop arm disabled — the neuter was observed
failing, not described. Note `elytra_wings_drawn` still read **2** under that
neuter, because it is incremented in `prepare_elytra`, one layer above the
draw: the counter is corroboration and the pixels are the evidence.

The gate installs its own `EntityDraw`, so it verifies the draw half only. It
says nothing about whether the wire's equipment packet actually lands
`(EquipmentSlot::Chest, minecraft:elytra)` in that field.

Its fixture orientation is load-bearing and was measured rather than assumed:
at `yaw: 180.0` (facing the camera) the torso occludes the wings and only 974
pixels change, all of them new sky rather than repaints.

## How to change it

* **The wings attach by the wearer's `body` part and gate on
  `wearer_carries_armour`** — the animation family, not part names. A pig has a
  `body`; attaching by name alone straps an elytra to a farm animal. The gate
  lives inside `ElytraMesh::attach` so a caller cannot forget it.
* **Baby wearers need a second bake.** Vanilla has `ModelLayers.ELYTRA_BABY`
  via `ElytraModel.BABY_TRANSFORMER = MeshTransformer.scaling(0.5F)`. Not
  ported — the same gap `armour-rendering.md` records for baby armour meshes.
* **The `wings` layer type is deliberately not an `ArmourLayerType` variant.**
  It is a real `EquipmentClientInfo.LayerType`, but that enum keys `armour_layers`
  and the trim sprite id, and an elytra has neither. One texture, one constant.
* Enchantment glint on an elytra is not ported, exactly as it is not for armour.

## Configuration

None. No feature gate, no env var. The texture is a jar asset, so a resource
pack replaces it through the ordinary `ResourceManager` stack.

## Dependencies

`lodestone-assets` (the model definition, the bake and the jar sheet),
`lodestone-render` (the mesh, the transform, the pose maths), `glam`. The draw
lives in `lodestone-shell`'s GPU entity passes and reads `EntityDraw::equipment`
for the chest slot and `EntityDraw::player_skin`'s cape URL for the texture
override.
