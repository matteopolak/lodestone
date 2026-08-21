# Elytra rendering

## What it is

The elytra's two wings as a wearable layer over the humanoid rig: a mesh baked
from `ElytraModel.createLayer`, a per-wing transform posed off the wearer's own
`body` part matrix, and the three-way glide/crouch/rest pose that
`ElytraAnimationState` lerps toward. The geometry and pose half is landed; the
GPU draw that consumes it is not, so **an elytra reaches zero pixels today** —
see "What is missing" for the exact remaining patch.

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

## What is missing

**Nothing draws an `ElytraMesh`.** The remaining work is entirely inside
`crates/lodestone-shell/src/gpu/`, and it is a near-copy of the cape's own
pass:

1. `gpu/entities.rs` — bake `ElytraMesh::load()` beside `cape_model`, upload it
   the way `GpuEntityModel::upload_cape` uploads the cape, and load
   `ELYTRA_TEXTURE_PATH` as a bind group. Unlike the cape this has a **jar
   texture**, so it needs no remote fetch and no per-URL grouping — one sheet,
   with a player's own `skin.elytra` (then `skin.cape`) overriding it when
   present, per `WingsLayer.getPlayerElytraTexture`.
2. `gpu.rs` — an `ElytraDrawBatch` beside `CapeDrawBatch`.
3. `gpu/entity_passes.rs` — a `prepare_elytra` mirroring `prepare_capes`, with
   two differences: the gate is the **chest equipment slot** carrying
   `minecraft:elytra` (`prepare_capes` already computes exactly this predicate,
   to suppress the cape), and it pushes **two** instances per wearer, one per
   `ElytraMesh::attach` entry, each with its own `elytra_wing_transform`.
4. `gpu/stats.rs` — an `elytra_layers_drawn` counter.

Note what step 3 means: **the cape pass already suppresses itself for an elytra
wearer and nothing replaces it**, so an elytra-wearing player with a cape
currently loses the cape and gains nothing. Half of this feature has been wired
since capes landed.

**The animation state is also absent.** `elytra_target_rotations` is the pure
half; the impure half is two lerped triples (`rot*`, `rot*Old`) advanced once
per game tick by `current += (target − current) * ELYTRA_ROTATION_LERP` and
read back interpolated by partial ticks — that belongs beside
`lodestone_shell::entities::cape_sway`'s lagged cloak position. Until it
exists, passing `elytra_rest_rotations()` straight through is correct for every
wearer who is standing, walking or running, and wrong only during a glide or a
crouch. That is a legitimate first cut, not a bug, provided it is written down
as one.

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

`lodestone-assets` (the model definition and the bake), `lodestone-render` (the
mesh, the transform, the pose maths), `glam`. The missing draw depends on
`lodestone-shell`'s GPU entity passes and on `EntityDraw::equipment` already
carrying the chest slot, which it does.
