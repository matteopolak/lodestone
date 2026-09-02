# The render camera: basis, view matrix, and the pitch-±90 singularity

## What it is

`lodestone-render`'s `Camera` — the eye position, yaw/pitch orientation and
projection every world-space matrix in the frame is derived from, reconciled
against vanilla's `net.minecraft.client.Camera`. Its orientation is built the way
vanilla builds it, from a **single YXZ Euler rotation**, because the obvious
alternative (a look-at with a hardcoded `Vec3::Y` up) is degenerate at pitch
`±90` and flipped the camera whenever the player looked straight up or straight
down.

## How it works

`Camera` (`crates/lodestone-render/src/camera.rs`) is a plain `Copy` struct:
`position` (the **eye**, not the feet — see `PLAYER_EYE_HEIGHT` and
`with_eye_from_feet`), `yaw`/`pitch` in degrees, `fov_y_degrees`, `aspect`,
`near`, `far`. Everything else is a method.

### The basis

`Camera::basis()` (private) returns `(right, up, forward)` in world space. It is
the direct expansion of vanilla's `Camera.setRotation`
(`.cache/mc/26.2/client-src/net/minecraft/client/Camera.java:336-344`):

```java
this.rotation.rotationYXZ((float) Math.PI - yRot * (float) (Math.PI / 180.0),
                          -xRot * (float) (Math.PI / 180.0), 0.0F);
FORWARDS.rotate(this.rotation, this.forwards);   // FORWARDS = ( 0,  0, -1)
UP.rotate(this.rotation, this.up);               // UP       = ( 0,  1,  0)
LEFT.rotate(this.rotation, this.left);           // LEFT     = (-1,  0,  0)
```

JOML's `Quaternionf.rotationYXZ(y, x, z)` is `rotationY(y).rotateX(x).rotateZ(z)`
with `rotateX`/`rotateZ` right-multiplying (local frame), so the rotation matrix
is `Ry · Rx · Rz`. With vanilla's arguments — `y = π − yaw`, `x = −pitch`, `z = 0`
(**no roll**) — that is `R = Ry(π − yaw) · Rx(−pitch)`. Using
`cos(π − yaw) = −cos yaw` and `sin(π − yaw) = sin yaw`, the `π` folds away:

```text
forward = (−sin y · cos p,  −sin p,   cos y · cos p)
up      = (−sin y · sin p,   cos p,   cos y · sin p)
left    = ( cos y,           0,       sin y       )   → right = −left
```

Three consequences worth holding on to:

* **`right` has no pitch term.** It is always horizontal, at every pitch. Rolling
  is not representable by `Camera` at all (see `docs/view-bobbing.md`).
* **`up` becomes horizontal at pitch `±90`**, which is correct and is exactly
  what vanilla does. Looking straight down while facing south puts south at the
  top of the screen (`up = (0, 0, 1)` at yaw 0, pitch `+90`).
* **`forward` is unchanged from what it always was**, bit for bit.
  `Camera::forward()` now returns `basis().2` so the direction block-targeting
  raycasts along and the direction the view matrix renders down cannot drift
  apart; the closed form is identical.

### The view matrix

`Camera::view_matrix()` assembles the basis into the standard right-handed
layout: the basis as the **rows** of the upper-left 3×3 block in the order
`right`, `up`, `-forward` (view space looks down `-Z`), with the translation
column holding the basis-projected negated eye. Its determinant is `+1`.

`particles.rs` and `nametag.rs`/`world_items.rs` read the basis back out of this
matrix (rows, i.e. one component from each glam column) rather than from an
accessor; `entity.rs`'s `camera_orientation` transposes instead of inverting for
the same reason. All three depend on `det == +1`.

## How to change it, and the gotchas

**Do not replace the basis with a look-at.** That is the bug this shape exists to
avoid, and it does not announce itself:

`look_to_mat4(position, forward(), Vec3::Y)` derives `right = normalize(forward ×
up)`. At pitch `±90` `forward` is `(0, ∓1, 0)` — parallel to the hardcoded
`Vec3::Y` — so the cross product is zero and normalising it is undefined. It
failed in **two** modes:

| forward | result |
|---|---|
| exactly `(0, ∓1, 0)` | `forward × Vec3::Y == 0`, normalise → the whole matrix is `NaN`, a blank frame |
| what f32 pitch `90.0` actually gives | **finite and orthonormal, but rolled 180°** |

The second mode is the one that shipped. `cos(90°)` in f32 rounds to
`-4.371139e-8`, not `0`, so the cross product is tiny-but-non-zero and normalises
to unit length — pointing the *opposite* way from a hair earlier. Measured at
yaw 0 across the single `0.05°` step from pitch `89.95` to `90.0`:

| vector | pitch 89.95 | pitch 90.0 |
|---|---|---|
| `right` | `(-1, 0, 0)` | `(+1, 0, 0)` |
| `up` | `(0, 0.00087, 0.99999964)` | `(0, 4.4e-8, -1)` |
| `forward` | `(0, -0.99999964, 0.00087)` | `(0, -1, -4.4e-8)` |

`right` and `up` **both** flip while `forward` stays put, so it is a 180° roll
about the view axis — the image turns upside down. Crucially that keeps the basis
finite, unit length, orthogonal, right-handed and determinant `+1`, so *every*
well-formedness assertion passes on the broken code. Only a **continuity sweep**
across the singularity or a **predicted basis value** can see it, and a gate
sampling pitch `0`/`±45` cannot see it at all.

**Do not "fix" a recurrence by clamping pitch tighter than `±90`.** A clamp
already exists (`crates/lodestone-ecs/src/player.rs`,
`pitch.clamp(-90.0, 90.0)`), and the flip happens *at* the bound, not past it.
Clamping to `±89.9` hides one symptom, diverges from vanilla (which renders
looking perfectly straight down correctly), and leaves the `NaN` reachable by
every other caller of the construction.

**The GUI winding invariant is a sign relationship to this matrix.** `CLAUDE.md`:
`sign(det(gui_ortho * gui_item_pose))` must **equal**
`sign(det(Camera::view_projection()))`, and that sign is negative because glam's
DirectX RH perspective determinant is itself negative. Because `det(view) == +1`,
the projection alone decides it — so any change here must keep `det(view) == +1`
or held items and GUI blocks render inside-out while still looking plausibly
isometric in a screenshot. Derive the sign from a real camera; never assert a
polarity.

**Roll is still not representable.** `bobbed_camera`
(`crates/lodestone-shell/src/camera_rig.rs`) folds a bob matrix into a `Camera`
by inverting `B · V` and decomposing yaw/pitch back out, which structurally drops
roll — that divergence is documented in `docs/view-bobbing.md` and is unchanged
by this work. One related note: `yaw_pitch_from_forward` is gimbal-locked at
pitch `±90` (`atan2` of two zeros), so a *bobbing* camera looking exactly
straight down loses its yaw. It is unreachable while standing still, because
`bobbed_camera` returns the camera bit-identically for an inert frame.

## Configuration

No env vars or feature flags. The numeric conventions, all reconciled against
the jar and pinned by tests:

| constant | value | source |
|---|---|---|
| `PLAYER_EYE_HEIGHT` | `1.62` | `Avatar.DEFAULT_EYE_HEIGHT` |
| default `fov_y_degrees` | `70.0` (vertical) | `options.fov` → JOML `perspective` |
| default `near` | `0.05` | `Camera.PROJECTION_Z_NEAR` |
| `far` | `max(rd_chunks · 16 · 4, cloud_chunks · 16)` | `Camera::far_for_render_distance` |
| depth range | `[0, 1]` DirectX/Metal, **reversed** (near → `1`, far → `0`) | vanilla's own arrangement; every ported depth comparison and polygon offset transcribes with **no** sign flip |

### The depth range is reversed, and that is load-bearing everywhere

`Camera::projection_matrix` is glam's `camera::rh::proj::directx::perspective`
with `near` and `far` exchanged in the `z` row — written out elementwise rather
than called with swapped arguments, so the exchange is visible.

A **forward** `[0, 1]` projection puts every window depth just under `1.0`, where
`f32` spacing is a flat `2^-24`, so a fixed world clearance `c` at distance `d`
buys `2^23 · near · c / d^2` representable values and collapses as `d^2`.
Reversed, depth is `near · (far - d) / ((far - near) · d)`, which shrinks with
distance and rides down through the float exponent: the relative separation is
`c · far / (d · (far - d))` and a float carries a `2^23`-to-`2^24` window of ULPs
per binade, so the separation degrades only as `1 / d`. Measured on a framed
map's `1.01 / 128` clearance, head-on
(`crates/lodestone-render/tests/coplanar_overlay_depth_survey.rs`):

| distance | forward `[0,1]` | reversed |
|---|---|---|
| 2 | 1,661 ULP | 53,166 ULP |
| 12 | 47 | 5,888 |
| 32 | 6 | 3,311 |
| 64 | 1 | 1,655 |

What follows from it, and what a reader porting a constant needs:

* **Nearer is greater.** Vanilla's `GREATER_THAN_OR_EQUAL` is
  `lodestone_render::DEPTH_COMPARE_NEARER_OR_EQUAL`, its strict sibling is
  `DEPTH_COMPARE_NEARER`, and neither flips.
* **A depth attachment clears to `lodestone_render::DEPTH_CLEAR` = `0.0`.** A
  clear to `1.0` under a "nearer wins" comparison rejects every fragment and
  draws nothing, silently.
* **A polygon offset that pulls toward the eye is positive.**
* **`gui_ortho` carries the same direction**, because a GUI item draws through
  the same pipeline into a depth buffer cleared the same way.
* **The projection matrix's 4x4 determinant is positive**, where the forward
  one's was negative — mirroring the clip `z` axis flips a 4x4 determinant.
  Nothing about *screen* winding changes: the rasterizer decides facing from
  projected `x`/`y` alone. A gate asserting that sign as a polarity is asserting
  a property of the depth convention, not measuring winding.

The far plane stays **finite**. An infinite-far reversed projection is the usual
companion and would make the separation exactly `c / d`, but it deletes the far
clip plane that `Frustum` extracts and section culling uses; the finite form
already carries the numbers above.

## Dependencies

`glam` (`Mat4`/`Vec3`/`Vec4`). The projection is assembled directly from
`Mat4::from_cols` — the `[0,1]` DirectX convention rather than the OpenGL
`[-1,1]` one, with the range reversed. Nothing else.
Consumers: `lodestone-render`'s `weather_pipeline`, `entity`, `item_render`,
`section`; `lodestone-shell`'s `camera_rig`, `sim/camera`, `particles`, `gpu/*`.

## Gates

`crates/lodestone-render/tests/camera_pitch_singularity.rs` — 14 tests, of which
three are controls that keep the detector honest:

* **subject:** basis health (finite, unit, orthogonal, right-handed, `det +1`) at
  pitch `±90` across eight yaws; predicted basis *values* at `±90`; a `0.05°`
  continuity sweep across `±90` requiring `dot(adjacent) > 0.9999`; agreement
  with `Ry(π − yaw) · Rx(−pitch)` rebuilt from glam's own `Mat3` primitives (a
  different construction path for the same vanilla expression); element-for-
  element agreement with the old `look_to_mat4` away from the singularity;
  `forward()` bit-identity; `det(view_projection)` sign against a pitch-0
  reference at every pitch.
* **controls:** `legacy_look_to_is_nan_for_an_exactly_vertical_forward`;
  `legacy_basis_at_the_singularity_is_rolled_180_degrees` (asserts the legacy
  basis is *healthy* — the reason a health check cannot find this);
  `legacy_basis_flips_through_the_pitch_singularity`, a `#[should_panic]` sweep
  over the old construction, so a green run proves the continuity detector fires
  rather than describing that it would.

The observed control failure on the unfixed tree was
`view_matrix: right is discontinuous between pitch 89.95 and 90 at yaw 0:
Vec3(-1.0, 0.0, 0.0) → Vec3(1.0, -0.0, 0.0), dot = -1`.

The camera half of the winding invariant is also pinned from the GUI side by
`item_render.rs`'s `winding_matches_the_world_camera`, `entity.rs`'s
`dropped_item_pose_preserves_winding` / `first_person_arm_pose_preserves_winding`
and `tests/sprite_drop_pixels.rs`.
