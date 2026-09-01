# The inventory player avatar

## What it is

The player standing in the inventory panel with their head and eyes tracking the
mouse — vanilla's `InventoryScreen.extractEntityInInventoryFollowsMouse`. Before
this, the recess at `(leftPos + 26, topPos + 8)` was the *hole in vanilla's own
`inventory.png`* with nothing rendered into it, so the screen showed a black box
where the player belongs. It is the first thing in this workspace to draw a full
3-D entity rig inside a 2-D GUI panel, and the first to use a GPU scissor.

Two halves, in two crates:

| half | lives in | owns |
|---|---|---|
| **pose** | `lodestone-render/src/gui_entity.rs` | the record definition, the matrices, the look angles |
| **draw** | `lodestone-shell/src/container/player_preview.rs` | one `EntityPipeline`, the rig upload, the skin sheet, the scissored pass |

The split is the same one [`item-gui-geometry.md`](item-gui-geometry.md) and
[`gui-item-icons.md`](gui-item-icons.md) already have for block items, for the
same reason: the fidelity questions ("does the winding flip?", "does the head
turn the right way?") are answered by unit tests over pure matrix construction,
not by squinting at a screenshot.

## How it works

### The record definition

`InventoryScreen.java:104-140` (26.2). The full transcription, with citations, is
`gui_entity.rs`'s module doc — read that, not this summary. The two things most
likely to be got wrong:

**`bodyRot` and `yRot` are not the same kind of number.** `bodyRot = 180 + a` and
`yRot = a` look like two absolute yaws a constant 180° apart. They are not:
`LivingEntityRenderer.java:246` defines `state.yRot = wrapDegrees(headRot - bodyRot)`,
so `yRot` is the head yaw **relative to the body** (vanilla's `netHeadYaw`) while
`bodyRot` is absolute. Absolutely, the body sits at `180 + a` and the head at
`180 + 2a` — **the head really does track twice as far as the body**, and that
over-rotation plus the head pitch (`xRot = -yAngle * 20`, zeroed only for a
`FALL_FLYING` pose) is what produces the "the eyes follow you" read. Coding it as
two absolute yaws draws a player permanently looking over their own shoulder.

**Vanilla mixes units on purpose.** `xAngle`/`yAngle` are the *radian* output of
`Math.atan`, multiplied by `20.0F` and then read as **degrees**. Reproduced
verbatim; "correcting" it changes the swivel by `180/π`.

### Where the matrices come from

Vanilla renders this through `PictureInPictureRenderer`: an offscreen
`(x1-x0) * guiScale × (y1-y0) * guiScale` colour+depth pair, an ortho over that
texture, then a premultiplied-alpha blit into the rect. Its model-view is
`translate(w/2, h/2, 0) · scale(s, s, -s)` with `s = guiScale * size`.

Every term is proportional to `guiScale`, so the whole thing collapses to one
matrix in **logical** GUI pixels with `s = size` — the space this workspace's GUI
path already works in (`gui_ortho`). The offscreen target then buys exactly one
thing: clipping, which a `set_scissor_rect` over the same rect provides for an
opaque/cutout pass. That is the deliberate divergence; the matrices are vanilla's.

The composed `mesh → GUI pixel` matrix is `gui_entity_pose`:

```text
T(rect centre) · S(size, size, -size) · T(0, bbHeight/2 + offsetY, 0) · Rz(π) · Rx(camera pitch)
  · entity_model_matrix(ZERO, bodyRot, 1.0)
```

The second factor is **the same function every mob in the world is placed by**,
composed rather than restated — so the `MODEL_FEET_OFFSET` lift and the rig flip
cannot drift between the inventory and the world.

### Pipeline flow

```text
ContainerGeometry::build_inner
  -> player_avatar: Some(PlayerAvatar { rect, mouse })      // only for MenuKind::Player
ContainerRenderer::render_geometry_scaled
  -> PlayerPreview::draw
       -> avatar_part_matrices  (gui_ortho * view * EntityInstance::part_transforms)
       -> upload_instances, one per rig part
       -> pass: colour LOAD, depth CLEAR, set_scissor_rect, draw_indexed per part
```

The pass is recorded **after the panel-art pass and before the slot item passes**,
which is where vanilla calls it — from `InventoryScreen.extractBackground`, right
after its own `INVENTORY_LOCATION` blit. Ordering against the slots is free (no
slot lives in the recess); ordering against the *panel* is not — drawn first it
would be painted over.

## How to change it, and the gotchas

**The `z` scale is `-size`, not `size`, and this is the one that bites.** The rig's
front is mesh `-Z` (derived: `entity_model_matrix` at yaw `0` maps mesh `-Z` to
world `+Z`, and Minecraft's yaw `0` faces `+Z`). `S(size, size, -size)` maps that
onto a *larger* GUI `z`, which `gui_ortho` makes *nearer*. Write the obvious
`Mat4::from_scale(Vec3::splat(size))` and the face loses the depth test to the
back of the skull — you see the inside of the far side of the head, which reads as
odd shading rather than as an obviously broken draw. Note that
`EntityPipeline` has `cull_mode: None`, so the symptom is **depth order**, not a
vanished silhouette; the determinant sign detects it either way.
`the_pose_winds_like_the_world_camera`,
`dropping_the_z_flip_reverses_the_winding` and
`the_face_is_nearer_than_the_back_of_the_head_in_both_arms` hold this down, the
last two by computing the wrong-pipeline hypothesis and requiring it to disagree.

**`Rz(π)` and `entity_model_matrix`'s `scale(-1, -1, 1)` cancel exactly.** That is
not redundancy to optimise away — it is *why* vanilla rotates by π at all.
`LivingEntityRenderer` flips the rig so a `+Y`-up world can draw a `Y`-**down**
mesh, and GUI space is already `y`-down, so the flip has to be undone. Remove
either alone and the avatar is upside down and mirrored;
`the_pi_roll_cancels_the_rig_flip` pins the composed form.

**Its own camera buffer, always.** `queue.write_buffer` is ordered against the
**submit**, not the encoder, so two passes sharing one uniform buffer in a single
submit both read the *last* value written, and nothing fails loudly. This pass's
projection is `gui_ortho`; the world entity pass's is a perspective camera. Do not
"share" the world renderer's buffer to save 128 bytes.

**Group 0 is the identity, not `gui_ortho`.** The clip matrix is baked into every
per-part *instance* transform, because the entity shader computes
`view_proj · instance · vertex` and the instance is the only per-part slot.
Writing `gui_ortho` into the uniform as well applies it twice.

**Its own `EntityPipeline`, not the world's.** `ContainerRenderer` is constructed
independently of `gpu::RenderState` and receives only a depth view and a
`BlockModels` borrow, so reaching the world's `EntityRenderer` would mean threading
it through `app.rs`'s whole redraw path. The cost is one pipeline object, one mesh
upload and one 64×64 texture — the same trade
`ContainerRenderer::attach_items` already documents for the item atlas. It is
**not** a fifth bind group: the entity shader still spends exactly two (camera,
texture), so nothing here goes near wgpu's 4-group floor.

**The cursor arrives in physical pixels and must be divided down.**
`ContainerFrame::cursor` is viewport pixels, the same space `hit_test_with_scale`
takes; `build_inner` divides by `calculate_gui_scale` before handing it over. Skip
that and at `gui_scale = 2` the head aims twice as far out as the pointer is.
`the_avatars_cursor_is_divided_down_to_the_logical_canvas` computes both
hypotheses and requires them to be separable.

**The rect hangs off the *drawn* panel origin.** It is measured from `build_inner`'s
already-shifted `x`/`y`, so an open recipe book moves the avatar with the panel for
free. Restating `panel_origin_with_scale` inside `player_preview.rs` would agree
with the draw by coincidence and diverge the moment the book opened —
`an_open_recipe_book_moves_the_avatar_with_the_panel` is the gate, and it carries a
premise-false control asserting the book really does shift the panel at that
canvas size.

**The scissor is clamped, and `None` means "do not draw".** wgpu validates a
scissor against the attachment and rejects an overrun.
`panel_origin_with_scale` floors the origin at `8`, so a window narrower than the
panel really does push the recess off the right edge. Far edges round *outwards*
(`ceil`), or a scaled avatar loses its rightmost column.

**The depth clear is full-attachment.** A `LoadOp::Clear` ignores the scissor. That
is both safe and necessary: the world's depth buffer is still resident and would
swallow a rig at GUI clip depth, and the container's own item-model pass clears it
again immediately afterwards, so nothing downstream inherits this pass's depth.

**Not yet handled**, each deliberately:

* ~~**Real skins.**~~ **Landed** — see [`player-skins.md`](player-skins.md).
  The preview resolves the same UUID-scoped `RemoteSkin` as the world body and
  first-person arm. Its binding identity is `(profile UUID, source)`, so account
  switches and resource-pack renderer rebuilds both rebind correctly. A cached
  `skin.png` is accepted only with a matching `skin.uuid`; it can no longer make
  another account from the switcher appear in this preview.
* ~~**Live pose — now fed, partially.**~~ **Fed whole.**
  `ContainerFrame::with_avatar_pose` → `PlayerAvatar::pose` → `gui_entity_anim`'s
  `base`, produced in `app/redraw.rs` from **`Sim::local_body_anim()`** — the walk
  cycle, the arm swing, the head pitch and the crouch, the same `AnimInput` the
  third-person body draws with.

  **This entry used to say `limb_swing`/`limb_swing_amount` were unreachable
  because of a crate boundary. They were not, and the diagnosis of *which*
  obstacle mattered is the useful part.** The walk state does live on
  `Sim::body_pose`, a private field whose only public reader was
  `third_person_body_state` — but `sim/camera.rs` is *inside* `sim`, so access was
  never the problem. The obstacle was that reader's
  `camera_type.is_first_person()` **early return**, which is a *drawing* decision
  (do not draw a body the camera is inside) wrongly gating a *pose* that is
  camera-independent. `local_body_anim` is the same construction without it, and
  `third_person_body_state` now calls the shared `body_anim` so the two cannot
  diverge.

  The consumer needs the pose precisely when the gate said no: the inventory
  screen is only ever opened in first person.

  One trap if you touch this: **`attack_anim` is a phase, and `1.0` is a no-op.**
  `HumanoidModel.setupAttackAnimation` drives it through sines, so phase `1.0` is
  the rest pose again. A gate written with `1.0` measures a delta of `1.7e-8` and
  reads as "the pose never arrives" when the pose is arriving perfectly.
  `the_live_pose_reaches_the_draw_and_moves_the_right_arm` uses `0.5` and asserts
  the endpoint identity so the property is recorded rather than commented.

  `hand_swing_progress()`/`tick_count()` are deliberately **no longer** read at
  the call site: `local_body_anim` takes `attack_anim` and `age_ticks` off the
  *same* `body_pose.render(partial_tick)` call as the limb swing, so the swing and
  the walk cannot drift by a frame.
* **Armour, held items and the elytra.** The world path draws all three off the
  wearer's own part matrices (`prepare_armour`, `merge_held_items`); the avatar
  draws the body only. The seam is `avatar_part_matrices`' output — the same
  `part_transforms` those passes consume.
* **`boundingBoxHeight` is the constant `1.8`.** `PLAYER_BB_HEIGHT`'s doc explains
  why looking it up through `lodestone_data::entity_dimensions` is not available to
  a module that never sees a packet, and why a crouching box cannot reach this
  screen.

## Configuration

Nothing env-driven. Everything comes from the vanilla pack:
`ContainerRenderer::attach_player_preview` returns `false` when no `client.jar`
skin sheet resolves, and the recess then stays empty. There is **no synthetic
fallback** — the same deliberate asymmetry `attach_background` and
`gpu/entities.rs`'s armour sheets take, for the same reason: a flat-magenta
humanoid in the inventory reads as a rendering bug, not as "no pack found".
`player_preview_attached()` is the assertable signal, so a gate cannot pass on a
silently degraded run.

The vanilla constants — `+26`/`+8`, `49×70`, `size = 30`, `offsetY = 0.0625`,
the `40` divisor and the `20` scale — are all published from
`lodestone_render::gui_entity` rather than restated per call site.

## Dependencies

* `lodestone-render` — `gui_entity` (this feature's own module), `gui_ortho`,
  `entity_model_matrix`, `EntityInstance`/`EntityMesh`/`EntityModelSet`,
  `Skeleton::pose`, `EntityPipeline`, `GpuEntityModel`, `upload_instances`.
* `lodestone-assets` — the baked `player_wide` rig and the PNG decode.
* `crate::gpu::entities::entity_texture_from_image` — shared, not copied: the
  `Rgba8UnormSrgb` choice is worth +48% brightness on every pixel if a second copy
  gets it wrong. Widening it from `pub(super)` to `pub(crate)` is the only change
  this feature made outside `container/`.
* `crate::resources::vanilla_manager` — pack discovery.
* `crate::menu::render::logical_canvas` / `crate::config::calculate_gui_scale` —
  the one definition of "what is a logical GUI pixel", shared with `hit_test`.

## Related

* [`item-gui-geometry.md`](item-gui-geometry.md) — the item counterpart of the pose
  half, and the origin of the GUI winding invariant.
* [`gui-item-icons.md`](gui-item-icons.md) — the item counterpart of the draw half.
* [`container-screen.md`](container-screen.md) — the panel this sits in.
* [`entity-rendering.md`](entity-rendering.md) — the world path this shares its
  placement function and its pipeline shape with.
