# Entity rendering

## What it is

The path from "the server says there is an entity at (x, y, z)" to a posed, textured, lit mob, sprite or nametag on screen, plus two systems on the same entity data: picking (what the crosshair targets) and pose-dependent collision dimensions.

## How it works

The CPU renderer sits behind the stable `lodestone_render::entity` re-export, split by responsibility: `entity_catalog.rs` (type, model and texture lookup), `entity_model.rs` (baked part meshes, placement), `entity_batch.rs` (instances, culling, draw grouping), `entity_layers.rs` (armour, wool, cape, elytra), `entity_item.rs` (dropped, framed, thrown item poses), `entity_orb.rs`, `entity_first_person.rs`. Baked meshes are one per model/layer; frame state is per instance. Keep new code in the narrowest module and preserve the root re-export.

### Type path, model, texture

- The jar-derived dimensions table takes a validated `lodestone_data::entity_type::EntityType`, never a raw registry integer. The render fold narrows once per ingest snapshot and stores `Option<EntityType>` beside the path in `RenderKind`. Closed dispatch (dropped-item physics, projectiles, TNT fuse, orbs, armour-stand pose, sheep wool) matches the generated enum; `None` (custom or data-pack type) keeps the path for resource-pack model lookup and declines all built-in behaviour. Never add another path-string match.
- Dropped items keep consecutive 20 Hz physics positions and render between them with the frame-clock residual; corrections preserve velocity and ground state without restarting visual age.
- `ClientHandle::entity_by_network_id` takes a server-owned `EntityNetworkId`; the raw `entity(i32)` classifies first, so an unknown id is a normal miss and a negative local id cannot alias a server entity.
- Display billboard metadata crosses the seam as `lodestone_model::BillboardMode`; the adapter alone converts ordinals, mapping unknown to `Fixed`.
- `canonical_model_name(type_path)` maps to a `lodestone_assets::entity_models` corpus entry; the corpus is the source of truth and only a few types alias (`player`/`mannequin` to `player_wide`, `bogged` to `skeleton`). Record an alias as pending the moment the real mesh lands or it survives as a wrong-but-plausible mob.
- `entity_texture_candidates(model_name)` derives paths from each entry's own `EntityTexture`. A flat-hue fallback means the sheet was not found; the wrong mob means the wrong entry resolved.
- Variants (wolf breed, pig/cow/chicken climate, horse coat from the packed variant int's low byte) use `EntityTexture::ByVariant` + `resolve(variant)`. Llama, cat, parrot and mooshroom have entries but no variant axis. A resolver can be tested and reachable yet have zero production callers if every site asks for `default_path()`.

### Pose

- Arm swings decode to `lodestone_model::Hand` at each protocol boundary and stay typed until the final animation byte; pre-off-hand protocols produce `Hand::Main`.
- `AnimFamily::classify` picks a pose setup from part names (a quadruped has `right_hind_leg`/`left_front_leg`), keeping version-specific lists out of a version-free crate. Subclass overrides on an identical skeleton (zombie arms on a player rig) go in the `HumanoidArms` table keyed on model name, never in the classifier.
- Creeper swell is a scale about the root part, applied before the ground-lift translate, so conjugate it as `T(+1.501) * S * T(-1.501)` or the creeper sinks:

```text
wobble = 1 + sin(swell * 100) * swell * 0.01
s      = (1 + clamp(swell,0,1)^4 * 0.4) * wobble   // x and z
hs     = (1 + clamp(swell,0,1)^4 * 0.1) / wobble   // y
```

- A render field whose correct rest value is `0.0` is easy to leave unwired because the identity default hides it. Gate the caller, not the formula.
- The walk cycle samples the drawn (interpolated) position once per 20 Hz tick; sampling fresh network snapshots opens an `INTERP_STEPS` (3x) gap and over-swings legs.

### Shading

Final pixel is `texel * diffuse * light_term`, faded toward fog.

| what | rule | gotcha |
|---|---|---|
| diffuse | two lights, `min(1, (max(dot(n,L0),0)+max(dot(n,L1),0))*0.6+0.4)`, `L0=(0.2,1,-0.7)`, `L1=(-0.2,1,0.7)` normalised | a single `abs()`-folded light lights backfaces like forward faces |
| normal | derivatives of model-local position, negated | world-space varyings quantise far from the origin; a sign error is invisible on axis-aligned faces, so gate shading by location |
| world light | per instance (`EntityInstanceRaw::light`) | sampled once per entity; cannot live on the shared vertex buffer |
| light probe | the entity's eye, not its feet | a tall mob is lit by its head's cell |
| fire | forces only the block half of light to 15 | forcing the whole byte gives a burning mob in a cave a daytime sky |
| night darkening | client-side only; scales the sky half (1.0 noon, 0.24 midnight) | scaling all of `light_term` blackens torch-lit interiors |
| eye height | per registered type (102 of 158 override `height*0.85`) | most overrides floor into the default's cell; test one that crosses a boundary (`elder_guardian`, `ghast`) |
| colour space | tint and shade multiply in gamma space | linear multiply washes out |
| texture format | sheet must be `_srgb` | plain `Rgba8Unorm` plus an sRGB swapchain double-encodes |
| fog | shared camera uniform, byte-compatible with the block shader | keeps the pass inside the 4-bind-group floor |

Pose eye height and baby dimensions are not in the eye-height table. Entities draw after opaque terrain and before the translucent water pass (the fluid pipeline writes no depth, so drawing after water paints opaque colour over it).

### Render layers

A layer is a second independently baked mesh posed off the wearer's own animated part matrices, matched by part name, never a second skeleton. Sheep wool: `WoolMesh::attach` gates on the resolved model name, not `AnimFamily` (every quadruped shares sheep's part names); it is skipped when sheared, at the draw site.

- Baby humanoid armour is the exception: its own mesh, pivots, extra parts and pose. `lodestone_assets::equipment::baby_armour_model` builds it per `BabyArmourKind`; `BabyArmourMesh` bakes under the wearer's rig name and `ArmourModelSet::baby` feeds `prepare_armour` for `BABY_ARMOUR_WEARERS`, from the `humanoid_baby` sheets without trims. Gate: `crates/lodestone-render/tests/entities/baby_armour_oracle.rs` against `oracle-java/BabyArmourOracle.java` (`just oracle-baby-armour`, 72 scenarios, with adult-mesh and wearer-skeleton controls); `armour_pixels.rs` measures the helmet's top row.
- Fields at their default are not sent, so an unsheared white sheep's wool byte never appears on the wire. Synthesize the idle default once at spawn, never in the raw decoder, or an already-dyed sheep resets on every later packet.
- Opaque entity pipelines write depth pulled toward the camera by `CAMERA_DEPTH_BIAS`. A decal pass without depth writes (banner and shield patterns, armour trim) must carry the same bias (`build_entity_pipeline`'s `matches_opaque_bias`) or it is rejected.
- Landed overlays use `EntityDraw::overlay_sheet` (an `EntityOverlay { sheet, tint }`) emitting a second instance into `PreparedEntityBatches::overlays`, drawn by `gpu/frame.rs` through the translucent pipeline: horse markings (packed variant's second byte; gate `horse_markings_pixels.rs`) and the wolf collar for tamed wolves (`lodestone_ecs::entity::CollarColor`, index 21, default red when absent; gates `mob_variant_wire.rs`, `mob_variant_pixels.rs`; baby collars not selected).
- Fox coat and axolotl colour are index-18 ordinals decoded under `MetadataClass::Fox`/`Axolotl` (the class guard is required: the index is shared with the sheep wool byte and creeper ignited bit) into `EntityVariant`, resolved by `entity_variant_sheet_for`.
- Not landed: charged-creeper aura, iron golem cracks, llama decor, horse armour, mooshroom mushrooms, glowing eyes.

### Sprite entities

These types have no cuboid rig and must stay out of the model corpus: `dragon_fireball` (camera-facing quad, 2x, full-bright), `fishing_bobber` (quad 0.5x plus a sagging line to the caster's hand), `ominous_item_spawner` (carried item grown in over 50 ticks, spinning 40 degrees/tick). Both quads share one baked mesh. Never recover a sprite row's index by pointer identity (the table is a `const`); index by value.

The fishing line reuses the debug-line renderer (a screen-space ribbon) with a quadratic sag (midpoint at 0.375 of the rise). Anchor: owner by wire id gives third person; not found but a synthetic local-player draw exists gives our body; neither gives first person with the camera as anchor.

### Nametags

One `NameTag { text, see_through }` from two rules:

- A player's tag is its tab-list display name: UUID-keyed rows by entity UUID; protocol 5 keeps the profile name beside the UUID in `named_entity_spawn` as `PlayerProfileName` and matches it exactly against the name-keyed row. If the server decorates the list name the match fails and no tag is drawn rather than guessing.
- Any other entity's tag is `CUSTOM_NAME` gated on `CUSTOM_NAME_VISIBLE`, with no type-name fallback.

`TabList` keeps session-local identity history apart from active rows, so a remove clears the overlay while a spawned entity still resolves its name; a resolved name is also kept in the render skin cache for a missing-row frame. Both sources obey the team's `name_tag_visibility` and the invisibility flag (armour stands excepted so an invisible named stand is a hologram). `see_through` is the crouching pass and suppresses the depth-testless pass.

Style walks a real `Text`/`TextSpan` tree (hex colour survives). Bold redraws the glyph offset and widens the advance; obfuscated glyphs resample from a same-advance pool per draw; no drop shadow. Fonts come from `resources::open_vanilla_pack_stack`, the same stack as the HUD; keep font discovery there.

`wgpu` cannot say "this pipeline ignores the pass's depth attachment", so the see-through pass uses `Always` with no depth write. The plate is black at 0.25 opacity in gamma space with asymmetric left/top one-pixel padding and no z-offset; world-text passes draw into a raw non-sRGB view to avoid linear blending that reads too weak. The two passes submit different colour/background/plate combinations, so read both.

Cutoff is 64 blocks squared from camera to the entity's feet. The anchor is the bounding-box top plus 0.5: dimensions census height, `EntityDraw::scale` for baby/small stands, the 1.5-block crouching box, zero for a marker stand. Per-type overrides (sitting cat, sleeping villager) are not ported.

### Named cosmetics

Raw custom-name text is compared exactly and case-sensitively: `Dinnerbone` and `Grumm` flip model-backed living entities upside down (carried on `EntityDraw` so body and layers agree; the nameplate keeps its anchor); `jeb_` animates sheep wool through the 16 colours every 25 age ticks using interpolated age. Sprite, vehicle and non-living entities do not get them; the sheep undercoat texture is unsupported.

### GPU bring-up

The first frame needs only pipelines, camera, player rigs and synthetic fallback sheets. Later redraws install bounded batches of model uploads and decoded sheets; the trim atlas decodes independently and installs in the same batches. On the browser, trim decoding handles at most four palette permutations before yielding. Timings use the `startup_profile` trace target.

### Picking

One ray per frame from the interpolated camera: blocks first, then entities capped by the block-hit distance and `ENTITY_REACH` (3.0), through a distance pre-filter, `CAN_BE_PICKED`, a hitbox lookup (drops types the census cannot size) and the exact ray-vs-AABB test. The local player is never a candidate.

`CAN_BE_PICKED` exists because attacking a just-died mob lands the next click on its item or orb, and the server kicks for "Attempting to attack an invalid entity". It is default-deny by category: living entities pickable unless removed; boats, minecarts, falling blocks, TNT, hanging entities, end crystals, interaction entities and shulker bullets always; projectiles only if tagged `redirectable_projectile` (fireball, wind charge, no arrows); players and ordinary armour stands pickable; marker stands excluded once their flags arrive; the dragon and everything else never.

### Pose dimensions

The player box (`0.6x1.8` standing, `0.6x1.5` crouching, `0.6x0.6` swimming/gliding) is a fit-gated state machine. Desired pose priority is `SLEEPING > SWIMMING > FALL_FLYING > SPIN_ATTACK > CROUCHING/STANDING` (from the raw shift key); it is vetoed if it does not fit, falling back to crouching then swimming. If even swimming does not fit the pose is sticky (no write). There is no recovery if a box later grows into a space it no longer fits, so the fit gate alone stops a surfacing swimmer clipping a low ceiling.

A pose changes two coupled numbers, box height and eye height, anchored at the feet (a standing eye on a swimming box reports "not submerged" underwater). The pose is decided after the tick's movement; entity push runs before it. The entity-collision half of the fit test is vacuously true except for boats, shulkers and the happy ghast.

Gotchas: pose heights are widened `f32` literals (`0.6f32 as f64 != 0.6`), so build boxes from the pose table. A 1.5-block gap is a flush fit, the real crouch-under-a-slab case. Test sleeping before crouching. `eye_height` elsewhere is an output mirror, never an input.

## How to change it

- New mob: add the `EntityModelEntry` to the corpus. Alias only for another mob's model class.
- Too bright or dark: check texture format (`_srgb`), `light_term`, the sky-darken factor, and which side of the gamma curve the multiply sits on; indistinguishable on a non-sRGB target, so measure on a real one.
- World light and sky darkening ride a source function installed at connect time on every connect path; until then mobs render full-bright at permanent noon. Terrain does not yet read the sky-darken lane.
- New picking filter: ahead of the hitbox lookup, keep default-deny.
- New pose: extend the pose table and check priority order before wiring input.
- Metadata indices are reused across unrelated classes: run the metadata index oracle, use a class guard, and keep a literal byte fixture plus an `EntityDraw` state control.

## Configuration

`LODESTONE_ASSETS` (or a discovered `.cache/mc/<version>/`) is the pack root; absent, mobs fall back to flat colour and sprite, nametag and shadow passes draw nothing. `ENTITY_REACH` (3.0) and `REACH` (4.5) match the default interaction ranges. The `entityShadows` video option gates the shadow pass.

## Dependencies

`lodestone-assets` (corpus, fonts), `lodestone-data` (dimension, eye-height, collision census), `lodestone-physics` (pose dimensions and quantized sin/cos; never `f32::sin`/`cos`, which diverge at cardinal angles), `lodestone-ecs` (metadata components), `lodestone-render` (`entity_pipeline.rs`), `lodestone-shell` (`gpu/entity_passes.rs`, `gpu/nametag.rs`, `sim/`). See [camera-and-view](./camera-and-view.md) for the reversed-Z projection every depth-biased pass assumes.
