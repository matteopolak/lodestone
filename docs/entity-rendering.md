# Entity rendering: type → model → texture → pose

## What it is

The path from "the server says there is a `minecraft:drowned` at (x, y, z)" to a
posed, textured, instanced mob on screen. Three separate decisions live along it,
and each has burned us once:

1. **Which mesh** draws this entity type (`lodestone-render/src/entity.rs`).
2. **Which sheet** paints that mesh (`entity.rs`, derived from the corpus).
3. **Which `setupAnim`** poses it (`lodestone-render/src/entity_anim.rs`), fed by
   `lodestone-shell/src/entities.rs`.

## How it works

### Type path → model name

`canonical_model_name(type_path)` maps an entity type's registry path onto a
`lodestone_assets::entity_models` entry name. **The corpus is the source of
truth**: a type path that *is* a corpus entry name resolves to that entry.
Only types whose registry path differs from a model name are listed explicitly:

| type path | model    | why |
|-----------|----------|-----|
| `player`  | `player_wide` | `PlayerRenderer` picks a skin model; wide is the default |
| `bogged`  | `skeleton`    | `BoggedModel` is not ported; nearest ported mesh |

> **Gotcha, learned the hard way.** This used to be an explicit table listing
> every drawable type, written when the corpus held nine meshes. `drowned`,
> `husk`, `zombie_villager`, `stray`, `wither_skeleton`, `cave_spider` and
> `mooshroom` were all aliased onto a base mob. The tier-2/3 mesh ports landed
> their real meshes and the aliases were never revisited, so a drowned rendered
> as a fully-textured ordinary zombie — a wrong mob that looks completely
> correct. Deriving identity from the corpus means a newly ported mob is drawable
> the day its mesh lands, and any wrong-mesh substitution has to be *written
> down* rather than left behind.

### Model name → texture

`entity_texture_candidates(model_name)` returns in-jar paths in priority order.
It is **derived from each corpus entry's own `EntityTexture`**, not hand-listed —
a second table can only drift out of step with the first, and did. A
`_temperate` sheet also yields the bare legacy name as a second candidate, so one
binary works against both the pre- and post-26.2 pack layouts.

`lodestone-shell/src/resources.rs` walks `EntityModelSet` and decodes the first
candidate the jar contains; a model with no hit falls back to
`gpu.rs::synthetic_entity_texture`, a flat per-model hue. **A mob rendering in a
single flat colour means its sheet was not found; a mob rendering as the wrong
mob means resolution picked the wrong entry.** They are different bugs.

### Pose

`AnimFamily::classify` picks a `setupAnim` from a model's **part names**, not its
name — a model with `right_hind_leg`/`left_front_leg`/… is a quadruped whatever
it is called. That keeps a version-specific mob list out of a version-free crate.

The one thing it cannot express is a vanilla **subclass** override, because a
zombie's part hierarchy is identical to a player's. `HumanoidArms` carries that:
`humanoid_arms_for(model_name)` in `entity.rs` returns `Zombie` for `zombie`,
`husk`, `drowned` and `zombie_villager`, all of which call
`AnimationUtils.animateZombieArms` in vanilla. The rig is chosen in
`EntityMesh::from_named_model`, **before** the local AABB is taken, because a
zombie's resting arms stick ~0.63 blocks out in front and the culling box has to
bound the mob as drawn.

### Creeper swell (a scale, not a pose)

`Skeleton::pose_swelling(input, swell)` folds a creeper's pre-detonation growth
into the part matrices. It is deliberately **not** part of `setup_anim`: in 26.2
the effect lives in `CreeperRenderer.scale`, a `PoseStack` op wrapped around the
whole model, and `CreeperModel.setupAnim` does only head tracking and the
ordinary quadruped leg swing. `creeper_swell_scale` transcribes it:

```text
wobble = 1 + sin(swell * 100) * swell * 0.01
s      = (1 + clamp(swell,0,1)^4 * 0.4) * wobble   // x and z
hs     = (1 + clamp(swell,0,1)^4 * 0.1) / wobble   // y
```

Two things are easy to lose. The dominant term is the **quartic growth** (up to
+40% wide, +10% tall), not the `sin` — that is a ±1% shudder layered on top, and
a port that keeps only the sine produces a barely-visible jitter and no swell.
And the axes are *reciprocal* in `wobble`: the creeper squashes as it widens.

The scale is composed as `T(+1.501) ∘ S ∘ T(-1.501)` above the root part, because
vanilla applies it **before** the `translate(0, -1.501, 0)` ground lift, so the
lift scales too and the creeper grows *upward out of the floor*. Scaling about
the model origin instead — the obvious implementation — sinks the feet ~0.16
blocks at full swell. `swollen_creeper_keeps_its_feet_on_the_ground` pins it.

**Not yet wired, and nothing sets `swell` above zero.** The chain stops in the
protocol layer: `Creeper.DATA_SWELL_DIR` is metadata index 16 (a `VarInt`, `-1`
or `1`), `v770`'s `read_entity_metadata` decodes that serializer correctly but
drops the value at its "decoded for alignment, not surfaced" arm because
`EntityMetadataUpdate` has no field for it. Reaching a live creeper needs, in
order: a field on `EntityMetadataUpdate`; a class-guarded arm in
`packets/metadata.rs` (index 16 collides with `IDX_BABY`, so it needs a
`MetadataClass::Creeper` guard, and index 17's powered flag collides likewise);
`apply_metadata` in `state.rs`; a per-entity `swell` counter on `entities.rs`'s
`Track`, since `getSwelling` is a *client-side integral* of the synced direction
(`swell += swellDir` each tick, divided by 28) and not a synced value; and a
`swell` field on `AnimInput` to carry it the last hop.

One known gap once it is wired: `EntityMesh::local_min`/`local_max` come from
`rest_pose()`, so a swelling creeper is drawn up to 41% wider than its own
culling box and will clip at the frame edge. `MAX_SWELL_SCALE` is exported for
whoever widens the creeper's local bounds.

### Walk cycle

`entities.rs` samples the **drawn** (interpolated) position once per 20 Hz tick
and feeds the distance to `walk_target_speed` = `min(distance * 4, 1)`, matching
vanilla's `updateWalkAnimation`. See that module's docs for why the tempting
alternative — the gap a fresh snapshot opens up — is wrong by exactly
`INTERP_STEPS` (3×) and makes every mob's legs swing too far and too fast.

### Shading: light, colour space, fog

A mob's final pixel is `texel x diffuse x light_term`, faded toward the fog
colour by view distance. Three things about that were wrong at once and each is
independent of the others, so it is worth naming them separately.

**World light is per instance.** Vanilla samples the lightmap once per entity at
its block position (`LivingEntityRenderer` -> `Level::getLightColor`), so a mob
is uniformly lit by the block it stands in. That is why light rides the
*instance* buffer (`EntityInstanceRaw::light`, shader location 8) and not the
vertex buffer: the vertex buffer is shared by every instance of a model type and
could only ever say one thing for all of them. The shader turns the packed byte
into terrain's own `light_term = 0.2 + 0.8 * max(sky, block)`, floor included.

Before this existed every mob rendered full-bright. Measured against terrain
through both real pipelines with the same mid-grey texel (byte 128):

| surface | sunlit (sky 15) | dark (light 0) |
| --- | --- | --- |
| block top (`face_shade` 1.0) | 128 | 25 |
| block N/S (0.8) | 102 | 21 |
| block E/W (0.6) | 77 | 15 |
| block bottom (0.5) | 64 | 13 |
| mob, before | 159 | 159 |
| mob, after | 88 | 18 |

The terrain column is exactly `128 x face_shade x light_term`, i.e. vanilla. The
blocks were never the problem; the mob was up to 10x too bright beside them.

**The multiply happens in gamma space,** through the same
`srgb_to_linear(linear_to_srgb(rgb) * shade)` round-trip the model shader uses.
Vanilla is not colour-managed. Doing it in linear light and re-encoding pulls
every factor toward 1.0 (a shade of 0.6 reads as 0.79) — the washed-out look
`4e8f058` removed from terrain, which entities still carried afterwards.

**The sheet must be an `_srgb` texture format.** A vanilla PNG holds
gamma-encoded bytes. Binding it as plain `Rgba8Unorm` hands the shader `0.50`
where the linear value is `0.21`, and the sRGB swapchain then encodes it a second
time: a measured **+48%** on every mob pixel, enough on its own to make a mob
brighter than the brightest sunlit block face. This is a property of the *upload*
(`entity_texture_from_image` in `lodestone-shell/src/gpu.rs`), not of the shader,
and it is invisible to any test that renders to a non-sRGB target.

**Fog rides the camera uniform.** `EntityCameraUniform` is `CameraUniform`
followed by `FogUniform`, byte-compatible with `ModelCameraUniform`, so a mob and
the block behind it cannot be fogged by different math. Folding it in rather than
adding a bind group also keeps the pass inside the portable four-group floor.
Without it, mobs stayed at full contrast at the render-distance edge and under
water while terrain faded.

### Draw order

Entities are drawn **after opaque terrain and before the translucent water
pass**, matching vanilla's `SOLID`/`CUTOUT` -> entities -> destroy progress ->
`TRANSLUCENT`.

This is a separate defect from the fog term and neither substitutes for the
other. The fluid pipeline runs with `depth_write_enabled: false`, so water leaves
nothing in the depth buffer; a mob drawn afterwards passes the depth test against
the sea floor and writes **opaque** colour straight over the surface. The result
is a submerged mob painted on top of the water at any depth. Fog tints a mob by
distance — it cannot put a water surface in front of it.

### Render layers: sheep wool (issue #53)

Vanilla draws wool as a `RenderLayer` (`SheepWoolLayer`) over a sheep's own
body model, following the exact same shape as the humanoid armour layer
documented in [`armour-rendering.md`](./armour-rendering.md): a second,
independently-baked mesh, posed off the wearer's — here the sheep's — own
already-animated part matrices, never a second skeleton. This pass ports the
mesh and the dye maths and confirms the SHEARED bit is already decoded, but
does not land pixels: the mechanism that poses a layer off another rig's
`part_transforms` (`ArmourMesh`/`attach`) lives in
`lodestone-render/src/entity.rs`, which this pass does not own, and the same is
true of the shell plumbing that would call it. What follows is landed, tested,
and precisely specified for whoever lands the rest.

**Landed, in `lodestone-assets/src/entity_models.rs`:**

* `sheep_wool_model()` — `SheepFurModel.createFurLayer`'s mesh: `head` at
  `+0.6` inflation, `body` at `+1.75`, all four legs at `+0.5`, sheet 64×32.
  Its six parts share `sheep_model`'s part *names and pivots exactly*
  (pinned by `sheep_wool_model_shares_sheep_body_part_names_and_pivots` in
  `lodestone-assets/tests/entity_models.rs`), which is the whole precondition
  for posing wool off the sheep body's `part_transforms` by name — the
  `ArmourMesh::attach` discipline. Three details read from
  `SheepFurModel.java` rather than guessed, all called out in the function's
  doc comment: the head's wool box is one texel *shallower* in Z than the
  body's (wool never reaches the snout — vanilla, not a bug), the legs are a
  genuinely *shorter* box (6 texels tall, not a scaled 12 — "socks", not a
  wrong deformation), and the fur legs are **not mirrored** the way the body's
  right legs are.
* `sheep_wool_tint(ordinal: u8) -> [u8; 3]` — `ColorLerper.Type.SHEEP`'s
  16-entry dye table at vanilla's fixed `brightness = 0.75`, with `DyeColor.WHITE`
  special-cased to vanilla's own literal `(230, 230, 230)` rather than the
  formulaic `0.75 * 255`. `ordinal` is the same `0..=15` value the protocol
  layer already decodes (see below). Out-of-range fails open to white, matching
  `armour_layer_tint`'s rule for an unrecognised colour.
* Confirmed against the real jar (`sheep_wool_texture_decodes_from_the_real_jar`,
  `lodestone-assets/tests/real_jar.rs`, `--ignored`): `sheep_wool.png` is 64×32
  and **exactly** greyscale — 888/888 opaque texels have R==G==B — which is the
  precondition for painting it by a flat gamma-space tint multiply rather than
  needing a per-colour texture the way horse coats do.

**Was decoded, not an island at the protocol layer, but dropped one hop
later — now fixed as far as the data can go without the held render files.**
Grepped for the producer across the whole tree, not just a consumer in one file
(per `CLAUDE.md`'s rule on stale absence claims):

* `crates/protocol/v770/src/packets/metadata.rs` decodes the sheep wool
  metadata byte (index 17) into `EntityVariant::Dyed { color, sheared }`
  (`color` low nibble, `sheared` bit `0x10`), guarded on `MetadataClass::Sheep`
  so it cannot collide with another mob's byte-valued index 17.
  `lodestone-model/src/event.rs` carries both fields on the shared
  `EntityVariant::Dyed` arm.
* `lodestone-client/src/state.rs`'s `entity_view` reads a `Variant` ECS
  component straight into `EntityView::variant: Option<EntityVariant>` — fully
  wired, nothing missing here.
* **It used to stop at `lodestone-shell/src/net.rs::entity_snapshot`, and now
  does not.** `EntitySnapshot` (`lodestone-shell/src/entities.rs`) gained a
  `variant: Option<EntityVariant>` field, and `entity_snapshot` now reads
  `view.variant` into it — the exact same shape of fix already landed for
  velocity and equipment. `entities.rs` also gained a `sheep_wool` helper that
  narrows a snapshot's variant to a `SheepWool { color, sheared }` payload
  **gated on the resolved `type_path` being exactly `"sheep"`**, never on
  `AnimFamily::Quadruped` (the pig/cow trap below), a `RenderWool` component
  that carries it alongside `RenderEquipment` (same "replace wholesale, no
  movement gate" treatment — shearing is a metadata update, not a movement),
  and `EntityDraw::wool: Option<SheepWool>`, populated by
  `extract_entity_draws`. Hermetic tests
  (`sheep_wool_narrows_only_the_dyed_variant_on_a_sheep`,
  `sheep_wool_reaches_the_draw_only_for_a_sheep`,
  `shearing_updates_wool_on_a_sheep_that_has_not_moved` in `entities.rs`;
  `entity_snapshot_carries_variant_through` in `net.rs`) pin the whole chain,
  including the pig/cow trap as an executed negative control.
* **What is deliberately *not* filtered at this layer:** a sheared sheep still
  yields `Some(SheepWool { sheared: true, .. })`, not `None` — the data stays
  honest about what was reported, and vanilla's "sheared sheep grow no wool
  mesh" gate belongs at the point that draws the mesh (`prepare_wool`, below),
  the same discipline `EntityDraw::equipment` already uses for slots it cannot
  yet draw.
* **What is still not wired:** the `EntityView`-to-pixels half. `EntityDraw`
  carries the payload now, but nothing meshes, poses or draws it — that is the
  `WoolMesh`/`prepare_wool` work in the two held render files, unchanged from
  the spec below.

**The pig/cow trap applies here too, worse.** `AnimFamily::Quadruped` is
`sheep`'s, `pig`'s, `cow`'s *and* `wolf`'s family — gating a wool attach on
family alone would draw a fleece on a pig exactly the way an ungated armour
attach would draw a breastplate on one (`armour-rendering.md`'s "a pig has both
`head` and `body`" trap). A correct gate has to be keyed on the **resolved
model name being `"sheep"`**, not on the animation family, since the family is
shared by mobs that must never grow wool.

**Pixel evidence, without touching the held files.** Every piece needed to
pose a second mesh off a wearer's part matrix — `EntityMesh::from_model`,
`EntityModelSet::resolve`/`get`, `plan_entities`, `EntityBatch::parts`,
`Skeleton::index_of`, `GpuEntityModel::upload`, `upload_instances_tinted` — is
already public, so
`lodestone-render/tests/sheep_wool_pixels.rs` reimplements the
`ArmourMesh::attach` idea locally (bake the wool mesh independently, look up
each of its six parts' matrices by name against the sheep body's own
`part_transforms`) entirely against that public API, with no edit to
`lodestone-render/src/entity.rs`. `#[ignore]`d (needs a GPU adapter); measured
results from one run:

```text
determinism control (sheared x2) : 0 px differ (must be 0)
sheared vs woolly (white tint)    : 10151 px differ / 65536 total
white-tint vs red-tint, in wool region : 10151/10151 px differ
average per-channel byte delta         : 88.2
```

The three assertions: a sheared/woolly pair (the briefing's own suggested
control) must differ by a real, non-trivial pixel count; two sheared renders
must be pixel-identical (rules out a non-deterministic renderer); and,
restricted to exactly the pixels the wool layer newly covers, a red-tinted
render must differ substantially from a white-tinted one at the *same* pose —
proving `sheep_wool_tint`'s bytes reach the shader's per-instance tint, not
just that the CPU table has the right numbers.

**Wiring still needed (outside this change's files), fully specified:**

The `EntitySnapshot`/`EntityDraw` half (originally items 2 and 3 here) is
**landed** — see the fold above. What is left is entirely inside the two files
`lodestone-shell`'s render layer holds:

1. **`lodestone-render/src/entity.rs`** — a `WoolMesh`/`SheepWoolModelSet` type
   mirroring `ArmourMesh`/`ArmourModelSet` (same file, ~line 917–1071) field
   for field: `vertices`, `indices`, `parts: Vec<(&'static str, PartRange)>`,
   built from `sheep_wool_model()` via `bake_entity_parts` exactly as
   `ArmourMesh::for_slot` builds from `humanoid_armour_model`. Its `attach`
   must gate on the wearer's **resolved model name being `"sheep"`**, not
   `wearer.family()` — see the pig/cow trap above; `wearer_carries_armour`'s
   `AnimFamily::Humanoid` check is not the right template to copy verbatim
   here for exactly that reason.
2. **`lodestone-render/src/entity_pipeline.rs`** — a `GpuEntityModel::upload_wool`
   mirroring `upload_armour` (same file, ~line 245), taking `&WoolMesh`.
3. **`lodestone-shell/src/gpu.rs`** — a `prepare_wool` mirroring `prepare_armour`:
   skip sheep whose `EntityDraw::wool.sheared` is true (vanilla's own gate;
   the field itself is not pre-filtered — see above), else attach the one
   wool mesh, tint via `sheep_wool_tint(color)`, batch by texture
   (`entity/sheep/sheep_wool`), and draw. **Use the base entity pipeline
   (`Less`), not `armour_pipeline` (`LessEqual`).** Armour needs `LessEqual`
   because leather's two layers are coplanar at the same inflation; wool has no
   second layer at the same inflation as itself, so there is no z-fighting risk
   to correct for, and copying `armour_pipeline` here would be picking a
   pipeline for the wrong reason (see `CLAUDE.md`'s note that the base and
   armour pipelines already disagree on this compare function and neither
   should be copied without checking why). `EntityRenderer` (the struct
   holding `armour_pipeline`/`armour_models`/`armour_textures`) needs the
   equivalent `wool_model: Option<GpuEntityModel>` (there is only one mesh, no
   per-material variant) and `wool_texture: Option<wgpu::BindGroup>`, loaded
   from `entity/sheep/sheep_wool` the same way `load_humanoid_armour_textures`
   loads armour's sheets, and the draw call wired into the render pass right
   after the `armour_batches` block, before the dropped-item pass.
4. **Five existing `EntityDraw { .. }` struct literals in `gpu.rs`** (its
   `into_draw`, one hermetic armour test, two pig-culling-gate literals, one
   zombie-hue-gate literal) and **one `EntitySnapshot { .. }` literal in
   `sim.rs`'s own test module** now need `wool: None`/`variant: None` (plus the
   pre-existing `count: 1` from the drop-count widening below) added — the
   mechanical consequence of widening a struct these two held files construct
   by full literal. None of them need any *behavioural* change beyond that.

**Deliberately out of scope for this pass**, same as armour's equivalent list:

* **Baby sheep.** `BabySheepModel`/`textures/entity/sheep/sheep_wool_baby.png`
  is a separate, smaller mesh; not built.
* **The `jeb_` rainbow name easter egg.** `SheepRenderState.getWoolColor`
  lerps through every dye colour once named `jeb_`; `sheep_wool_tint` only
  implements the plain per-dye table.
* **`sheep_wool_undercoat.png` / `SheepWoolUndercoatLayer`.** A second overlay
  that only draws for a jeb_ sheep or a non-white one; not built, since it
  depends on the same unwired dye plumbing as the primary layer.

### Other render layers (surveyed, not landed)

The same mechanism — a second mesh posed off the wearer's part matrix — is
missing for a family of vanilla layers, and every one of them hits the same
architectural blocker sheep wool does: the attach type lives in
`lodestone-render/src/entity.rs` and the draw call lives in
`lodestone-shell/src/gpu.rs`, neither owned by this pass. Listed here rather
than half-landed, per `CLAUDE.md`'s "one working seam plus a clear list beats
twelve half-done layers": wolf collar (dyed) and wet/angry variants, charged
creeper aura, iron golem vines and cracked overlays, llama decor, horse
markings and armour, mooshroom mushrooms, snow golem pumpkin, shulker head,
villager and zombie-villager profession/type overlays, and glowing-eye layers
(enderman, spider, blaze). Sheep wool is the one seam proven end-to-end at the
mesh/tint/pixel level; the others have not been investigated further than this
list.

## How to change it

* **New mob ported.** Add the `EntityModelEntry` to
  `lodestone-assets/src/entity_models.rs`. Nothing in the render crate needs
  touching: identity resolution and the texture path both follow. If vanilla
  renders it with another mob's model *class*, add an alias to
  `canonical_model_name`; if it overrides `HumanoidModel`'s arms, add it to
  `humanoid_arms_for`.
* **Porting `bogged`.** Delete its alias in the same change.
* **New `setupAnim` override.** If it is structural (a new limb layout), extend
  `AnimFamily::classify`. If it is a subclass override on an identical skeleton,
  extend `HumanoidArms` — do not add a mob-name branch to the classifier.
* **`set_*_rot` adds, vanilla assigns.** Identical wherever the driven limb is
  authored at zero rotation.
  `entity_anim.rs::models_that_author_a_driven_limb_rotation` pins the 14 models
  where it is not; check that list before relying on additive behaviour.
  `animate_zombie_arms` is the exception that **assigns**, faithfully.

* **A mob looks too bright or too dark.** Check the four factors in that order,
  because they are independent: the sheet's texture *format* (`_srgb`?), the
  shader's `light_term`, **the sky-darken factor** (below), and where the multiply
  sits relative to the transfer curve. Measure on an **sRGB** target — a
  `Rgba8Unorm` target hides the colour-space half entirely, and a brightness
  threshold calibrated on one is meaningless on the other.
* **Wiring real world light.** `RenderState::set_entity_light_source` takes
  `Fn(Vec3) -> Option<u8>` returning packed `sky << 4 | block` at a mob's feet.
  Until something installs one, every mob is `ENTITY_FULLBRIGHT`. The equivalent
  world lookup already exists for particles in `Sim::extract_particles`.

### Sky light does not change at night — a fix that shipped and did nothing

`53850ce` made entities sample world light and `52f109f` installed the sampler.
Both were correct. **The player still reported full-bright mobs at night**, and
every candidate cause that involved sampling or shader plumbing was wrong.

**A server's sky-light array is time-invariant.** It records how much sky *reaches*
a block, not how bright the sky currently is. Measured live against a vanilla 26.2
oracle at one sky-lit position, with the server's own clock as the control
(`crates/lodestone-shell/tests/live_entity_light_time_of_day.rs`):

```text
noon     clock= 6000  packed=0xF0  sky=15 block=0  light_term=1.000
midnight clock=18000  packed=0xF0  sky=15 block=0  light_term=1.000
```

Identical byte, identical `light_term`. No sampling fix could ever have worked.
Vanilla darkens **client-side only**, in `LightTexture.updateLightTexture`, by
scaling the *sky* half of the lightmap by `Level.getSkyDarken(partialTick)`.
`lodestone_render::entity::sky_darken_for_time_of_day` is that curve
(`1.0` at noon, `0.24` at midnight, including `LightTexture`'s `* 0.95 + 0.05`
lift), and the entity shader applies it as
`light_term = 0.2 + 0.8 * max(sky * sky_darken, block)`.

Three gotchas, each of which produces a plausible-looking wrong build:

* **Only the sky half is scaled.** Scaling the whole `light_term` passes a
  naive day/night ratio gate and turns every torch-lit interior black at sunset.
  `entity_night_pixels::a_torch_lit_mob_is_identical_at_midnight_and_noon` pins it.
* **`0.0` means "not wired", not "pitch dark".** The factor rides the group-0
  uniform's one spare lane (`FogUniform::end_enabled.z`) so `EntityCameraUniform`
  stays byte-identical to `ModelCameraUniform` — the model shader is at wgpu's
  4-bind-group floor, so growing the uniform is not free. Every existing caller
  builds its fog from `FogUniform::new`/`disabled`, which zero that lane, so a
  literal reading would pin every mob everywhere at the `0.2` floor. Vanilla's
  range is `[0.24, 1.0]`, so `0.0` is safe as the sentinel and reads as noon.
* **The WGSL lives in a Rust `r"…"` raw string.** A double quote anywhere in a
  shader comment terminates it, and the resulting errors point at the comment
  text as if it were code. Use backticks in shader comments.

**Terrain has no sky-darken term.** `model_pipeline.rs` and the fluid shader still
render at permanent noon, so at night mobs are now correctly darker than the
blocks around them. The same factor needs plumbing there; the uniform lane already
carries it and the model shader simply does not read `.z` yet.

* **Wiring the world clock.** `RenderState::set_sky_darken_source` takes
  `Fn() -> Option<f32>`, polled once per frame, and is installed once at connect
  time next to `set_entity_light_source`. Note `NetClient` has **no** `world_time`
  method — the clock is reached through `net.shared_handle()`, whose
  `ClientHandle::world_time()` returns `(world_age, time_of_day)`. Until something
  installs a source, mobs render at permanent noon.

## Configuration

* `LODESTONE_ASSETS`, or `.cache/mc/<version>/` discovered upward from the cwd —
  the pack root that must contain `client.jar`. Absent, every mob falls back to
  its synthetic flat colour.
* `AnimInput::aggressive` exists but is always `false`: `Mob.isAggressive` rides
  a shared-flags bit nothing decodes yet.

## Gates

Nothing here is done until it is on screen; the crate's own tests cannot see that
nothing consumes them.

* `tests/entity_gate.rs` — a pig reaches pixels at all.
* `tests/entity_anim_pixels.rs` — changing `AnimInput` changes the leg band, with
  a rest-vs-rest control at exactly 0.
* `tests/creeper_swell_pixels.rs` — a fully-primed creeper covers 1.70x the
  pixels of a calm one (band 1.25–2.00; the unfixed build reads exactly 1.00),
  with a same-input control at swell 0 differing by 0 px. Measures silhouette
  **area**, not "pixels differ", because a scale anchored at the wrong origin
  also moves pixels; a companion assertion holds the soles within 8 px (the
  wrong anchor moves them 15).
* `tests/entity_variant_pixels.rs` — a drowned renders as a drowned (resolved
  through `EntityModelSet::resolve`, so the alias table is inside the gate), and
  a zombie's arms are out in front. Each uses the **pre-fix build as its own
  negative control**.

* `tests/entity_light_pixels.rs` — terrain's four `(direction, light)` clusters
  and a mob's silhouette measured in the same units, plus two gates: a mob at
  light 0 must render at ~0.2 of its sunlit brightness (the full-bright bug
  reads exactly 1.0), and must not out-shine the brightest terrain face at the
  same light level.
* `tests/entity_fog_pixels.rs` — the same mob at two depths must differ by >= 25
  bytes toward the fog colour, with an equal-depths control at exactly 0 and a
  fog-disabled control that collapses the depth response. Also
  `water_surface_covers_a_mob_behind_it`, which runs **both draw orders** and
  requires the wrong one to reproduce the no-water render bit for bit.
* `tests/sheep_wool_pixels.rs` (`lodestone-render`, `#[ignore]`d) — a sheared
  sheep and a woolly one differ by a real pixel count, with a sheared×2
  determinism control at exactly 0, and a red-tinted wool render differs from a
  white-tinted one **only within the pixels the wool layer itself newly
  covers** — see the sheep wool section above for the measured numbers.
  `lodestone-assets/tests/entity_models.rs` carries the hermetic half: the wool
  mesh's part names/pivots match the sheep body's exactly (the attach
  precondition), its per-part inflation matches vanilla's baked geometry, and
  `sheep_wool_tint`'s 16-entry table matches hand-derived `DyeColor` values.
  `lodestone-assets/tests/real_jar.rs::sheep_wool_texture_decodes_from_the_real_jar`
  (`#[ignore]`d) is the external-authority check that `sheep_wool.png` is
  64×32 and genuinely greyscale.

## Dependencies

`lodestone-assets` (`entity_models` corpus, `bake_entity_parts`, `ZipSource`,
`Image`), `lodestone-entity::pose` (`WalkAnimation`, `walk_target_speed`),
`glam`, `wgpu` via `entity_pipeline`.
