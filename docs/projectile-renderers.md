# Projectile renderers (arrow, spectral arrow, trident)

## What it is

The three entity types vanilla draws with `ArrowRenderer` and `ThrownTridentRenderer`:
`minecraft:arrow`, `minecraft:spectral_arrow` and `minecraft:trident`. Each is a
code-built cuboid rig aligned to the projectile's **velocity**, not a billboard and
not a mob.

Issue #380. Before this, `crates/lodestone-entity/src/projectile.rs` modelled arrow
motion in detail — gravity `0.05`, air inertia `0.99`, the different in-water step
order per `AbstractArrow.tick` — and arrows drew **zero pixels**, because nothing on
the render side knew the types existed. A textbook island: the physics crate's tests
were green throughout.

Not covered here: the nine `ThrownItemRenderer` billboards (snowball, egg, pearl,
potions, fireballs, eye of ender) — see [`thrown-projectiles.md`](./thrown-projectiles.md),
which explains why arrows are deliberately *not* in that table.

## How it works

Three pieces, in the order data flows.

### 1. The rigs — `lodestone-assets/src/entity_models.rs`

`arrow_model()` and `trident_model()`, registered in `entity_models()` as three
entries (`arrow` and `spectral_arrow` share `arrow_model`; only the sheet differs,
which is exactly what vanilla does by having two renderer classes over one
`ModelLayers.ARROW` bake).

No new geometry primitive was needed: `CubeDef`/`PartDef`/`PartPose` in
`lodestone-assets/src/entity.rs` already had per-part rotation, per-part scale and
per-cube `tex_scale`, which is the complete set `ArrowModel` uses.

| | arrow | trident |
|---|---|---|
| sheet | 32×32 | 32×32 |
| boxes | 3 (2 of them zero-extent planes) | 5, all solid |
| real quads | 6 of 18 (12 degenerate) | 30 of 30 |
| long axis | **`+X`** (shaft, tip at high X) | **`−Y`** (spikes below the pole) |
| mesh scale | `0.9×` on the **root** pose | none |

Three things about the arrow rig that are easy to lose:

* **Zero-extent boxes.** `back` is `addBox(0, -2.5, -2.5, 0, 5, 5)` (zero *width*)
  and `cross` is `addBox(-12, -2, 0, 16, 4, 0, …)` (zero *depth*). `bake_entity`
  emits all six faces of every box regardless, so twelve of the eighteen quads have
  zero area. That is vanilla's own behaviour (`ModelPart.Cube` does the same) and it
  is harmless — a zero-area quad rasterises nothing — but the corpus tests that walk
  UVs must skip them, via `quad_is_degenerate`.
* **`texScale`.** `cross` passes `(xTexScale, yTexScale) = (1.0, 0.8)`, so its `v`
  divisor is `32 × 0.8 = 25.6` rather than `32`. That is why the shaft strip in
  `arrow.png` is 5 pixels tall for a 4-texel-tall box. **Nothing else in the corpus
  uses `texScale`**, so a wrong value here is caught by exactly one test
  (`arrow_cross_north_uv_matches_hand_derived_vanilla_unwrap`); a value of `1.0`
  gives `v ∈ [0, 0.125]` instead of `[0, 0.15625]`, which looks plausible and crops
  the shaft.
* **The `0.9×` is a root scale, not a per-part one.**
  `LayerDefinition.create(mesh.transformed(pose -> pose.scaled(0.9F)), 32, 32)`
  reads as "scale every part", but `PartDefinition.transformed` applies the function
  to *its own* pose and copies its children untouched
  (`PartDefinition.java:95-99`). Writing it as three per-part `0.9`s would look
  identical for `cross_1`/`cross_2` and leave the fletching's pivot at `x = -11/16`
  instead of `-11/16 × 0.9`, i.e. 1.1 texels past the end of the shaft.
  `the_arrow_mesh_scale_moves_the_fletching_pivot_too` pins it.

### 2. The placement — `lodestone-render/src/entity.rs`

```rust
projectile_model_matrix(pos, yaw_deg, pitch_deg, scale)
  = T(pos) · Ry(yaw − 90°) · Rz(pitch) · S(scale)
```

`projectile_pitch_offset_deg(model_name)` is the **switch**: `Some(0.0)` for
`arrow`/`spectral_arrow`, `Some(90.0)` for `trident`, `None` for everything else.
`EntityModelSet::resolve_posed` reads it and calls either
`EntityInstance::new_projectile` (this matrix) or `EntityInstance::new` (the mob
one).

**The mob matrix cannot be reused, and this is the substance of the issue.**
`ArrowRenderer extends EntityRenderer`, not `LivingEntityRenderer`
(`ArrowRenderer.java:14`). `EntityRenderer.java` contains no `scale(` call at all;
the `scale(-1, -1, 1)` and the `translate(0, -1.501, 0)` that `entity_model_matrix`
carries are `LivingEntityRenderer.java:85` and `:87`. So a projectile gets
**neither** — and therefore these two rigs are authored `+Y` **up**, unlike every
mob rig in the same file, which is Y-down.

Two details of the wrong answer, both measured:

* The mob matrix puts the model origin at **`feet + 1.501`**, not `feet − 1.501`:
  the lift is applied *before* the Y negation, so `-1.501` comes back out positive.
  #380's own investigation note said "1.5 blocks low" and the first draft of the
  test asserted that and failed at `65.501`. It is 1.5 blocks **high**.
* The mob matrix sends the arrow's tip (model `+X`) to `(cos yaw, 0, sin yaw)`; the
  projectile matrix sends it to `(sin yaw, 0, cos yaw)`. Those are reflections of
  each other across the `x = z` diagonal, so they *agree* at `yaw = 45°` and are
  exactly opposed at `135°`. A control that asserts "the two point opposite ways" at
  an arbitrary yaw has a false premise a quarter of the time.

**Pitch is about `Z`, not `X`.** The shaft runs along `+X`, so a rotation about `X`
would spin the arrow on its own axis and leave the silhouette almost unchanged while
every arrow flew level. `ArrowRenderer.java:25` is `Axis.ZP`.

**The trident's `+90°` is what unifies the two rigs.** `TridentModel`'s pole lies
along `Y` with its tip at negative `Y`; `Axis.ZP.rotationDegrees(xRot + 90)`
(`ThrownTridentRenderer.java:31`) rotates that axis onto the arrow's `+X`. One
matrix serves both, and the whole difference between the two renderers is that
number.

### 3. Where yaw, pitch and between-packet motion come from

Vanilla derives a projectile's `yRot`/`xRot` from `atan2` on its own velocity:

```java
// AbstractArrow.tick, every tick; Projectile.shoot, once at launch
yRot = atan2(movement.x, movement.z) * 180/PI;   // atan2(-x, -z) when !physicsEnabled
xRot = atan2(movement.y, movement.horizontalDistance()) * 180/PI;
setXRot(lerpRotation(getXRot(), xRot));          // Mth.lerp(0.2, …), not assigned
setYRot(lerpRotation(getYRot(), yRot));
```

The server broadcasts velocity-derived rotation in ordinary spawn/move packets, and those reports
remain the reconciliation authority. They are not frequent enough to animate flight by themselves,
however. `entities.rs` therefore attaches `ProjectilePhysics` to arrow-family entities, initialises
the existing `lodestone_entity::projectile::Projectile::arrow` simulation from the wire position and
velocity, and advances it in the client `GameTick` schedule. Each tick updates the same
`TransformFrom`/`TransformTo` interpolation pair the renderer already consumes, including locally
derived yaw and pitch. A later server report replaces the local state, so this is prediction between
authoritative snapshots rather than a second server simulation.

To extend the animated family, update `is_ballistic_projectile` and choose the matching vanilla
`Projectile` profile; do not treat thrown-item billboards as arrows merely because both have a
velocity. Their drag, gravity, collision and renderer orientation differ.

**Note the convention clash, because it is real.** A projectile's `yRot` is *not* a
mob's body yaw: `Projectile.shoot` sets `yRot = atan2(mx, mz)`, so an arrow fired by
a player looking at yaw `Y` carries `yRot = −Y`. `Ry(yRot − 90°)` maps model `+X` to
`(sin yRot, 0, cos yRot)`, which is the motion direction — the two halves only agree
because both were taken from vanilla together. Do not "fix" one without the other.

### Measured against the live server

Both sign conventions are the *opposite* of a player's, which is exactly the kind of
claim this repo has got backwards from reading source. So they were checked against
Mojang's own 26.2 dedicated server over RCON: summon a `NoGravity` arrow with a known
`Motion` and `Rotation:[0f,0f]`, force ticks, and read back `Rotation`.

```
                       server           atan2 (as modelled)
+X level        yRot=  90.00 xRot=  0.00   yRot=  90.00 xRot=  0.00
-X level        yRot= -90.00 xRot=  0.00   yRot= -90.00 xRot=  0.00
+Z level        yRot=   0.00 xRot=  0.00   yRot=   0.00 xRot=  0.00
-Z level        yRot= 180.00 xRot=  0.00   yRot= 180.00 xRot=  0.00
rising  +X      yRot=  90.00 xRot= 18.43   yRot=  90.00 xRot= 18.43
falling +X      yRot=  90.00 xRot=-18.43   yRot=  90.00 xRot=-18.43
straight up     yRot=   0.00 xRot= 90.00   yRot=   0.00 xRot= 90.00
diag +X+Z       yRot=  45.00 xRot=  0.00   yRot=  45.00 xRot=  0.00
diag -X-Z       yRot=-135.00 xRot=  0.00   yRot=-135.00 xRot=  0.00
```

Nine for nine. The two rows that matter most: **`+X` motion gives `yRot = +90`**,
where a *player* facing `−X` has yaw `+90`; and **rising motion gives a positive
`xRot`**, where a player looking *up* has a negative pitch.

**Live-oracle hazard, new and expensive.** The oracle world's
`server.properties` has **`pause-when-empty-seconds=60`** (a 1.21.2+ default). With
no players connected, the server **stops ticking entirely** — and an RCON entity
probe then reads a frozen world while `list`, `time query`, `summon` and `data get`
all keep working perfectly. The first run of the sweep above reported `yRot = 0.00`
for all nine cases and looked like a total convention mismatch; the arrow's `Pos`
never changed from its summon position. Two further prerequisites, both of which
produced the same symptom:

* the chunk must be **force-loaded** (`forceload add`), or `@e` finds nothing —
  `summon` still reports "Summoned new Arrow";
* only **`tick sprint N`** advances entity physics (`tick step` does not), and it is
  **asynchronous** — it replies "The game is sprinting" and returns before the ticks
  have run, so a read immediately afterwards sees the old value.

An earlier one-off probe that happened to run a stray `tick sprint 1` read
`yRot = 80.34` after ten ticks, which is `90 × (1 − 0.8¹⁰) = 80.35` — an incidental
but exact confirmation of `lerpRotation`'s `Mth.lerp(0.2, …)`.

Two divergences from vanilla that follow from using the server's rotation:

* Rotation is quantised to `360/256 ≈ 1.4°` on the wire. Between reports the local projectile tick
  recomputes its angles from velocity, as vanilla's client does; the next server report reconciles it.
  `ProjectilePhysics` keeps the last authoritative position, velocity, and grounded state separately
  from the locally integrated values. This distinction is load-bearing: the ingest entity retains its
  last wire rotation between packets, and treating that stale rotation as a new correction rewinds the
  projectile to the old server position every frame.
* `lerpRotation`'s 20 %-per-tick easing happens **on the server**. Lodestone derives the predicted
  angle directly from its predicted velocity, then uses the normal render interpolation between
  client ticks.

## How to change it

| you want to… | touch |
|---|---|
| add another projectile type | an `EntityModelEntry` in `entity_models.rs`, **and** an arm in `projectile_pitch_offset_deg`. Forgetting the second is silent: the rig bakes, uploads and draws — 1.5 blocks high and mirrored. `exactly_the_projectile_models_take_the_projectile_placement` sweeps the corpus against the switch and will fail. |
| change the placement maths | `projectile_model_matrix`. Its unit tests derive the expected tip direction from `Projectile.shoot`'s `atan2`, not from the matrix. |
| draw a *tipped* arrow | `TippableArrowRenderer` picks `arrow_tipped.png` on `state.isTipped` (`Arrow.getColor() > 0`), and vanilla also tints it by the potion colour. Neither the bit nor the colour is decoded; a `ByVariant` texture would carry the sheet but not the tint. |
| animate the stuck-arrow wobble | `ArrowModel.setupAnim` adds a `zRot` wobble from `state.shake` for seven ticks after an arrow lands. `shakeTime` is *not* on the wire — vanilla sets it client-side from the `IN_GROUND` metadata **transition**, so reaching it needs a metadata-change hook, not a decode. |
| draw the trident's enchantment glint | needs a whole render type (scrolling additive layer) that this engine does not have, plus `isFoil`, which is not decoded. |
| add the crit particle trail | `AbstractArrow.ID_FLAGS` is metadata index 8, `0x01` — **bit-identical to `LivingEntity`'s "using item"** flag, the collision issue #57 had to guard `is_living` against. Surfacing it needs a third census-style guard decision, not just a decode. Separate work. |
| turn on back-face culling in `EntityPipeline` | **read the Y-flip section below first.** It is the one change that would make the arrow rig's flip observable again. |

Gotchas, in rough order of how quietly they bite:

1. **The switch, not the corpus, is what makes a projectile a projectile.** A new
   entry with no `projectile_pitch_offset_deg` arm draws in the wrong place and every
   mesh test stays green.
2. **`resolve` vs `resolve_posed`.** `resolve` is `resolve_posed` with `pitch = 0`,
   which is correct for every mob (a mob's pitch is head tracking and travels
   through `AnimInput`) and *flat* for an arrow. Five call sites deliberately still
   use `resolve`; only `prepare_entities` needs the pitch.
3. **Degenerate quads.** Any new whole-corpus test that walks positions or UVs must
   skip them or it will compare collapsed-unwrap noise. This is not hypothetical: it
   is how the first draft of `a_y_flip_of_the_arrow_rig_moves_no_geometry` produced a
   false negative.

## The Y flip, and why it needs no gate

#380 specified a two-direction long-axis pixel test and warned, correctly, that such
a test cannot catch a wrong `scale(1, -1, 1)`, because `ArrowModel` is symmetric
under `y → −y`. The conclusion drawn from that warning — "so resolving the flip
needs a texel comparison against a captured vanilla frame, or a live oracle" — does
not follow.

**On this rig a Y flip changes no pixel at all.** So there is nothing for any pixel
gate to catch, and no oracle would settle it either: a vanilla frame and a
Y-flipped frame are the same frame. Three facts, each with its own control:

1. **No vertex moves.** `cross_1` (`xRot = π/4`) and `cross_2` (`3π/4`) each span a
   plane through the shaft axis; `y → −y` maps each onto the other's plane, and the
   cube's `y ∈ [-2, +2]` extent is symmetric about the pivot, so the swap is exact.
   `back` is a 45°-rotated square, symmetric under `y → −y` outright. The silhouette
   is therefore identical *from every angle* — which is the fact the long-axis gate
   could not have caught, now proved rather than assumed.
2. **The shaft planes' UVs are identical too**, because both are built from the
   *same* `CubeListBuilder`: the two parts exchange places vertex-for-vertex **and**
   texel-for-texel. This is the load-bearing half, because the shaft box is the one
   that samples the arrowhead — the only genuinely Y-asymmetric region of
   `arrow.png` (rows 1 and 3 differ at `x = 13..15`: greys 193/226 against 158/158).
3. **The fletching keeps its four UV corners but reassigns them**, by a reflection
   across a diagonal of its 5×5 patch. That residual is texture-dependent, so it is
   settled against Mojang's PNG rather than argued: the patch is a **plus sign**,
   invariant under the whole dihedral group, so every reassignment samples the same
   texel.

Where each is asserted:

| fact | test | its control |
|---|---|---|
| 1, 2 | `lodestone-assets/tests/entity_models.rs::a_y_flip_of_the_arrow_rig_moves_no_geometry` | the **trident** rig, which is asymmetric in Y and must show a difference |
| 3 | `lodestone-assets/tests/real_jar.rs::arrow_fletching_patch_is_fully_symmetric` (`--ignored`) | the **arrowhead** patch of the same PNG, which must *fail* the symmetry check |

The one thing the flip does change is triangle **winding**. That is invisible only
because `EntityPipeline` sets `cull_mode: None` and its shader takes
`abs(dot(n, light_dir))`. Enabling back-face culling would make the flip observable
again, and would need a real oracle at that point.

The same symmetry argument covers **sub-90° roll about the shaft**: the fletching
planes sit at 45° and 135°, a set that maps to itself under a 90° roll.

## Proof that it reaches pixels

`crates/lodestone-render/tests/arrow_pixels.rs`, `#[ignore]`d — run with
`cargo test -p lodestone-render --test arrow_pixels -- --ignored --nocapture`.
Needs a GPU adapter and `.cache/mc/26.2/client.jar`; a missing either is a
**failure**, never a skip.

1. `an_arrow_reaches_pixels_inside_its_own_projected_rect` — arrow, spectral arrow
   and trident, each with its **real vanilla sheet** (resolved through
   `entity_texture_candidates`, so a wrong texture reference fails here). The rect is
   derived from the instance's own world AABB through the same `view_projection` the
   draw uses. Control: the identical measurement over an instance-free frame must
   report zero.
2. `the_shaft_follows_yaw_and_pitch` — three poses (`+X`, `+Y`, `+Z`) whose
   silhouettes must be a horizontal bar, a vertical bar, and an isotropic blob.
3. `the_mob_placement_would_draw_the_arrow_above_its_own_rect` — the same mesh
   rendered through `EntityInstance::new`, i.e. the pre-fix answer, as its own
   negative control: it must land entirely outside the projectile rect, and *above*
   it in the world.

Measured numbers, at `WIDE_FRAMING` in a 512×512 frame:

```
arrow           rect x234..319 y240..272   drawn x236..316 y248..263   416 px in-rect, 0 out
spectral_arrow  rect x234..319 y240..272   drawn x236..316 y248..263   356 px
trident         rect x232..408 y241..271   drawn x234..406 y242..269  1424 px
```

**Assertion 1 alone is not sufficient, and that is measured, not assumed.** With
`projectile_pitch_offset_deg("arrow")` neutered to `None` — arrows back on the mob
placement — assertion 1 **still passed**:

```
arrow           rect x238..274 y84..144    drawn x246..265 y94..130    320 px in-rect, 0 out
spectral_arrow  rect x238..274 y84..144    drawn x246..265 y94..130    302 px in-rect, 0 out
```

The reason it passed is the reason the rect is derived rather than hardcoded, turned
against itself: `projected_rect` comes from the *instance's own* AABB, so it moved
1.5 blocks up along with the wrongly-placed arrow — `y84..144` where the correct
build has `y240..272` — and the arrow filled it perfectly. A wrongly-placed arrow is
still an arrow inside its own AABB. Assertions 2 and 3 both failed.

(The `320` there and the `416` above are **not** a coverage difference: that
experiment predates assertion 1 moving to `WIDE_FRAMING` for the trident's sake, so
the two numbers are at different camera distances. The comparable pair is
`320 px in / 0 out` versus `416 px in / 0 out` — both clean passes.)

Two more controls, both watched failing:

* `is_arrow` forced to `true` (a detector that cannot tell the clear colour from
  geometry): all three tests fail.
* the `arrow` corpus entry renamed (the actual pre-#380 state): all three tests fail
  at resolve, plus two `lodestone-render` unit tests.

Every reading in the gate is reported as a **bounding box**, never a percentage: an
arrow is a few hundred pixels in a 512×512 frame, so any frame-average measure is
swamped by the sky. `assert_not_clipped` guards every silhouette — the first framing
(1.1 blocks) ran the broadside arrow off the right edge, and because
`projected_rect` clamps to the viewport too, the in-rect count still looked healthy
while every extent reading was really a statement about the frame size.

## Configuration

None. No env vars, no features. The rigs are compile-time data in the corpus; the
sheets come from the normal `ResourceManager` pack path
(`assets/minecraft/textures/entity/projectiles/arrow.png`,
`…/arrow_spectral.png`, `assets/minecraft/textures/entity/trident/trident.png`), and
a missing sheet falls back to `synthetic_entity_texture` like any other mob.

## Dependencies

* `lodestone-assets` — `entity::{CubeDef, PartDef, PartPose, bake_entity}` for the
  geometry primitive, `entity_models` for the rig data and texture references.
* `lodestone-render` — `entity::{projectile_model_matrix, projectile_pitch_offset_deg,
  EntityInstance::new_projectile, EntityModelSet::resolve_posed}`; drawn by the
  ordinary `entity_pipeline` instanced pass with no new pipeline, bind group or
  shader.
* `lodestone-shell` — `gpu.rs`'s `prepare_entities` (the one production call site)
  and `entities.rs`'s `EntityDraw::{yaw, pitch}`.
* `lodestone-entity` — `projectile.rs`, which models the motion these now draw.
  Nothing in the render path calls it; the server is the authority for position and
  rotation.
* Reference (behaviour only, never transliterated): `.cache/mc/26.2/client-src`'s
  `ArrowModel`, `ArrowRenderer`, `TippableArrowRenderer`, `SpectralArrowRenderer`,
  `TridentModel`, `ThrownTridentRenderer`, `EntityRenderer`, `LivingEntityRenderer`;
  and `.cache/mc/26.2/src`'s `AbstractArrow`, `Projectile`.
