# Entity sprite renderers

## What it is

The draw path for the three entity types that had **no** draw path at all until
this landed: `dragon_fireball`, `fishing_bobber` and `ominous_item_spawner`.
`cargo xtask world-coverage` reported all three as *stranded* — named in
`SHADOW_RADII`, decoded off the wire, reaching zero pixels — and the census moved
from 148 drawn / 7 stranded to 151 drawn / 4 stranded when they were wired.

The three share a cause rather than a mechanism. None of them is a cuboid part
rig, so `EntityModelSet::resolve` skips all three and every layer of the entity
pass with it. What they need instead is three different things:

| type | vanilla renderer | what it draws |
|---|---|---|
| `dragon_fireball` | `DragonFireballRenderer` | one camera-facing quad, 2× scale, full-bright, off its own sheet |
| `fishing_bobber` | `FishingHookRenderer` | one camera-facing quad, 0.5× scale, **plus** a sagging line to the caster's hand |
| `ominous_item_spawner` | `OminousItemSpawnerRenderer` | the contained item, grown in over 50 ticks and spinning at 40°/tick |

## How it works

### The two sprites

```text
add_entity (type "dragon_fireball" | "fishing_bobber")
  → EntityDraw { type_path, feet, .. }
  → RenderState::prepare_entity_sprites          (gpu/entity_passes.rs)
      entity_sprite::entity_sprite_index_for     -> which row of ENTITY_SPRITES
      entity_sprite::entity_sprite_matrix        -> T(feet) · S(scale) · orientation
  → one instanced draw per sprite, through the base EntityPipeline
```

`lodestone_render::entity_sprite::ENTITY_SPRITES` is the whole vocabulary: one
row per type, carrying the sheet path, the pose-stack scale, the quad rect and
whether `getBlockLightLevel` is overridden to a flat 15. `EntityRenderer` bakes
one shared mesh with one `PartRange` per row at bring-up, plus one texture bind
group per row, and the row **index** selects both.

This rides the *base* entity pipeline rather than a dedicated one. Both vanilla
renderers use `RenderTypes.entityCutout`/`entityCutoutCull`, which is
`DepthStencilState.DEFAULT` plus a `0.5` alpha cutout — exactly what
`build_entity_pipeline`'s `fs_main` arm already is. The experience orb needed its
own pipeline only because `ENTITY_TRANSLUCENT` blends and cuts at `0.1`.

### The fishing line

`FishingHookRenderer` submits a second piece of geometry: sixteen segments from
the hook up to the caster's hand, through `RenderTypes.lines()` at
`Window.getAppropriateLineWidth` — a **screen-space** width, not a world one.

```text
EntityDraw { type_path: "fishing_bobber", projectile_owner: Some(id) }
  → RenderState::fishing_line_vertices           (gpu/entity_passes.rs)
      resolve the anchor (below)
      entity_sprite::fishing_line_points         -> 17 world-space points
  → DebugLineRenderer::prepare / ::draw          (gpu/debug_lines.rs)
```

The line reuses `DebugLineRenderer` — a **second instance** of it, not a second
implementation. That renderer already expands world-space segment pairs into
screen-space ribbons, which is the technique this line needs for the reason
`docs/debug-overlay.md` records: a `PrimitiveTopology::LineList` segment
rasterises at exactly one *physical* pixel and is close to invisible at a real
gameplay resolution. The only thing the two instances do not share is the width
(`MIN_LINE_WIDTH_PX` 1.5 for a diagnostic wireframe, `VANILLA_LINE_WIDTH_PX` 2.5
for anything vanilla itself draws through `lines()`), which is why `prepare`
takes it as a parameter.

The sag is `y = dy · (a² + a) · 0.5 + 0.25` with `x`/`z` plain linear
interpolations — a quadratic droop applied to the vertical term only. At the
midpoint it is `0.375` of the rise, not `0.5`. The `0.25` appears twice in
vanilla, once anchoring the offset at `entity.getPosition().add(0, 0.25, 0)` and
once inside `stringVertex`; the pose stack sits at the entity's **feet**, so the
pair is what puts the two ends exactly on the hook and the hand.

#### Resolving the anchor, which is the interesting part

Vanilla forks on `getCameraType().isFirstPerson() && owner == Minecraft.getInstance().player`:
our own rod seen from our own eyes gets a near-plane projection, everything else
gets an offset from the owner entity's body. This client reproduces that fork
**without knowing its own entity id**, because two existing facts encode it:

* `entities::extract_entity_draws` deliberately excludes the local player, so a
  lookup by the wire's owner id missing means "the owner is us";
* `ThirdPersonBodyState::into_draw` pushes a synthetic draw under
  `LOCAL_PLAYER_DRAW_ID` (`-1`) **iff** the camera is detached.

So: found by real id → the third-person branch on that entity; not found but the
synthetic body present → the third-person branch on our own body; neither →
first person, and the camera is the anchor. Each of the three is exactly the
branch vanilla would take.

A predecessor filed this as blocked — *"the local player has no `EntityDraw` in
first person while the third-person synthetic one uses id `-1`, which never
matches the wire's owner id"*. The premise is true and the conclusion is not: the
mismatch **is** the signal, and vanilla does not read the local player's entity
position in first person either.

### The ominous item spawner

```text
set_entity_data (ITEM_STACK serializer)  →  EntityDraw::item
  → RenderState::merge_ominous_spawner_item      (gpu/world_items.rs)
      entity_sprite::ominous_spawner_item_scale  -> min(age, 50) / 50
      entity_sprite::ominous_spawner_spin_degrees-> wrapDegrees(age · 40)
      entity_sprite::ominous_spawner_item_mesh
  → merged into the same ModelMesh the dropped items use
```

It needs **no** protocol work: the metadata fold routes an `ITEM_STACK` field by
its *serializer* rather than by its index or its entity type, so
`EntityDraw::item` was already populated for it. The whole feature was a missing
draw arm.

The pose is `T(feet) · S(item_scale) · Ry(spin) · T(cluster offset) · display_matrix(ground)`
— **not** the dropped item's. `ItemEntityRenderer.submit` adds a bob, a hover
lift and `ItemEntity.getSpin`'s own rate; `OminousItemSpawnerRenderer` calls none
of them. Reusing `dropped_item_matrix` here draws an item that bobs when it
should hang still and spins at the wrong speed, which reads as "close enough" in
a screenshot and is wrong in motion.

Light is `LightTexture.FULL_BRIGHT`, passed literally by the vanilla renderer,
which never samples the world.

## How to change it

**Adding a sprite** is a row in `ENTITY_SPRITES` plus nothing else — the bake
loop, the texture load and the pass all iterate the table. Two numbers matter per
row and they are not interchangeable: the fireball's quad is `y ∈ [-0.25, 0.75]`
(three-quarters above its own origin) and the hook's is `y ∈ [-0.5, 0.5]`
(centred). Read them off the renderer's own `vertex` calls, never from the
entity's `EntityDimensions`, which is a hitbox and a different number.

**Never recover a row's index by pointer identity.** `ENTITY_SPRITES` is a
`const`, so it is inlined at every use site and may occupy as many addresses as
it has uses. This shipped once: a `std::ptr::eq` search against a returned
`&'static EntitySprite` matched nothing, and both sprites drew **zero pixels**
while the table, the mesh and the matrix were all correct. Use
`entity_sprite_index_for` and `entity_sprite_at`, which pass an index and compare
no addresses;
`every_sprite_index_resolves_back_to_its_own_row` is the gate.

**The owner id is the one wire field here**, and it travels on the spawn packet's
Object Data — the same trailing VarInt a falling block reads a block state from,
under a different type's interpretation. `FishingHook.getAddEntityPacket` writes
`owner == null ? this.getId() : owner.getId()`, so it is never the `0` a bare
`Projectile` would write. Nothing else carries it:
`FishingHook.defineSynchedData` registers only `DATA_HOOKED_ENTITY` and
`DATA_BITING`, so an adapter that discards the field leaves the client unable to
learn where the line is anchored, with nothing logged.

Widening `ClientEvent::ProjectileOwner` to every `Projectile` subclass is a
*decode* change, not a new event — the adapter emits it only for the one type
whose reading it has established and whose consumer exists.

### Gotchas

* **A remote owner outside tracking range takes the first-person branch** and
  anchors the line at our own hand. Vanilla draws nothing at all there
  (`shouldRender` requires a non-null player owner). A bobber is always within a
  few blocks of its caster, so a visible bobber whose owner is untracked is close
  to unreachable — but it is a real difference, not a rounding one.
* **The local player's own rod in first person is drawn right-handed, always.**
  There is no `EntityDraw` for the local player, and `Player.getMainArm()` is a
  synced client option this build does not decode for *anyone* — the same gap
  `ThirdPersonBodyState::into_draw`'s `main_arm_left` states for our own body —
  so vanilla's own default stands in. The **swing** is not a gap: it comes from
  `HandSwingSource`, the same `Sim::hand_swing_progress` the first-person arm
  pass polls, which is exactly the `getAttackAnim(partialTicks)` vanilla passes
  here.
* **The first-person anchor reads the camera's *effective* FOV**, where vanilla
  reads `options.fov()` for the plane height and the `960 / fov` factor while
  taking `zNear` and the aspect from the live projection. A dynamic FOV modifier
  (sprinting, a spyglass) therefore moves the anchor a few centimetres where
  vanilla's would not.
* **The spawner's grow-in ramp counts from when this client first *tracked* the
  entity**, not from the server-side spawn, so a spawner that walks into view
  grows in again. The same approximation the dropped item's bob phase already
  accepts.
* **`Mth.sin`/`Mth.cos`, not `f32::sin`/`f32::cos`.** The hand anchor's
  trigonometry goes through `lodestone_physics::mth`; that is why
  `lodestone-render` now depends on `lodestone-physics`, which has no
  dependencies of its own and cannot cycle.

## Configuration

None. All three draw whenever the entity is in view and the vanilla pack carries
the sheet; a missing sheet draws nothing rather than a stand-in, the same
asymmetry the flame, wool and experience-orb passes document.

## Dependencies

* `lodestone-render/src/entity_sprite.rs` — the table, the quad mesh, the
  placement matrices, the line curve and both hand anchors. Version-free, no GPU.
* `lodestone-render/src/camera.rs` — `camera_orientation` for the billboard
  rotation and, through the same matrix, the camera basis the first-person anchor
  projects onto.
* `lodestone-physics/src/mth.rs` — vanilla's quantized sine table.
* `lodestone-shell/src/gpu/entities.rs` — bakes the shared mesh and the per-row
  texture bind groups.
* `lodestone-shell/src/gpu/entity_passes.rs` — `prepare_entity_sprites` and
  `fishing_line_vertices`.
* `lodestone-shell/src/gpu/world_items.rs` — `merge_ominous_spawner_item`.
* `lodestone-shell/src/gpu/debug_lines.rs` — the screen-space ribbon expansion
  the line reuses.
* `crates/protocol/v770/src/adapter/entity.rs` → `lodestone_model::ClientEvent::ProjectileOwner`
  → `lodestone_ecs::ingest::apply_projectile_owner` → `lodestone_ecs::entity::ProjectileOwner`
  → `EntityDraw::projectile_owner`.

## Gates

| gate | half it covers |
|---|---|
| `lodestone-render` `entity_sprite::tests` | the geometry: the table, the UV pairing, the line's quadratic sag, the four-row holding-arm truth table, both hand anchors, the spawner's ramp and spin |
| `crates/lodestone-shell/tests/entity_sprite_pixels.rs` | the **draw**, rasterised — and nothing past the edge of its own fixture, because it installs its own `EntityDraw` |
| `crates/lodestone-shell/tests/stranded_entity_producers_wire.rs` | the **producer**: `ClientEvent` → ingest → `extract_entity_draws` → the fields the draw site reads |
| `crates/protocol/v770/tests/entity_spawn.rs` | the wire: the Object Data field becomes an owner id, and only for a bobber |
| `lodestone-ecs` `ingest::tests` | the fold, including a spawn and its owner arriving in one batch |

Measured, with each neuter observed rather than described: `dragon_fireball`
**8,402** px, `fishing_bobber` **273** px plus **305** px of line over **16**
segments, and the spawner **0 / 90 / 373** px at ages 0 / 25 / 200 — a half-grown
to grown area ratio of **0.241** against the quarter-area prediction.
