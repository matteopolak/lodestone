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

### Frame instance upload arena

Every visible model part needs a small `EntityInstanceRaw` vertex buffer containing
its transform, packed light, tint, and hurt overlay. Dense scenes used to call
`DeviceExt::create_buffer_init` once per part on every frame. A sampled showcase
with mobs, paintings, signs, banners, heads, mapped item frames, and particles
spent 10.5% of all stationary-frame samples in that upload helper; 78% of those
samples were creating Metal buffers rather than preparing instance data.

The first fix retained one destination buffer per ordinal batch, removing
`create_buffer_init` churn but leaving one `Queue::write_buffer` call—and one
native wgpu staging allocation—for every model part. The follow-up replaces
that pool with `RenderState::instance_arena`, one `InstanceBufferArena` shared
by all world entity and block-entity preparation passes.

`render_inner` calls `begin_frame` before the first producer. Each producer calls
`stage_instances_tinted`, which converts transforms/light/tint directly into the
arena's retained `Vec<u8>` and returns an aligned `Range<u64>`. Draw batches keep
that range rather than their own `wgpu::Buffer`. After the final block-entity
producer, `upload` grows the retained `VERTEX | COPY_DST` GPU buffer to a
power-of-two capacity when necessary and performs exactly one non-empty
`Queue::write_buffer`. Every draw then binds `shared_buffer.slice(range)`.
Queue ordering makes the write visible before the later render submission.

The one-shot `upload_instances_tinted` remains for isolated renderers that do
not own a world frame. World paths must stage into the arena. If a new
preparation pass is inserted, call it after `begin_frame` and before the single
`upload`; appending after upload or uploading twice is a lifecycle error. Keep
the returned range paired with its instance count, and retain the shared buffer
until all world draws finish.

There are no runtime flags or capacity limits. CPU capacity retains the peak
staged byte count; GPU capacity retains the peak power-of-two size and never
shrinks during the arena's lifetime. Changing retention or lifecycle belongs in
`InstanceBufferArenaState`; changing record construction belongs in
`tinted_instance`/`stage_instances_tinted`. Unit tests pin contiguous aligned
ranges, byte-for-byte conversion, CPU/GPU reuse, geometric growth, and both
invalid lifecycle transitions.

This covers the standard tinted placement record for bodies, armour, wool,
capes, elytra, paintings, orbs, sprites, spawner previews, banner layers,
block-entity parts, and the placement instance used by entity water masks. It
does not merge their geometry formats: flames, shadows, fishing lines, the
water-mask mesh itself, first-person held-item paths, uniforms, and dynamic map
textures keep their existing uploads.

### Type path → model name

`canonical_model_name(type_path)` maps an entity type's registry path onto a
`lodestone_assets::entity_models` entry name. **The corpus is the source of
truth**: a type path that *is* a corpus entry name resolves to that entry.
Only types whose registry path differs from a model name are listed explicitly:

| type path | model    | why |
|-----------|----------|-----|
| `player`  | `player_wide` | `PlayerRenderer` picks a skin model; wide is the default |
| `mannequin` | `player_wide` | `EntityRenderDispatcher.getRenderer` routes `ClientMannequin` into the *same* avatar renderer as a player, choosing the rig from the entity's skin and defaulting to `WIDE` |
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

### The "named everywhere, draws nothing" set, and where each came from

`cargo xtask world-coverage` names a class this document had no section for: a
type with a row in the client's hitbox table (`gpu/entity_passes.rs`'s
`EYE_HEIGHTS`, generated from the registry and therefore complete) and no entry
in the hand-ported rig corpus. In play that is a mob you collide with and cannot
see, which reads as a bug rather than as unported work. Ten types were in it, and
three more had the same symptom for the opposite reason — the rig existed and
nothing routed to it.

Source citations live here rather than in `entity_models.rs`, per the repo rule
that vanilla record definitions are named in `docs/` only. All paths are relative
to `.cache/mc/26.2/client-src/net/minecraft/client`, and the layer each type
bakes is registered in `model/geom/LayerDefinitions.java`.

| corpus entry | vanilla layer | renderer | notes |
|---|---|---|---|
| `giant` | `HumanoidModel.createMesh` at `MeshTransformer.scaling(6.0F)` | `GiantMobRenderer` | the constructor's `scale` argument is only a shadow radius; the 6× is in the layer |
| `leash_knot` | `model/object/leash/LeashKnotModel.createBodyLayer` | `LeashKnotRenderer` | extends `EntityRenderer`, so no 1.501 lift — routed through `non_living_vehicle_placement` |
| `sulfur_cube` | `model/monster/slime/SulfurCubeModel.createOuterBodyLayer` | `SulfurCubeRenderer` | its `scale` hook's constant part is folded into the corpus root pose; see that function's own derivation |
| `breeze` | `model/monster/breeze/BreezeModel.createBodyLayer` | `BreezeRenderer` | body layer is `retainPartsAndChildren("head", "rods")`; the wind funnel and eyes are separate layers |
| `creaking` | `model/monster/creaking/CreakingModel.createBodyLayer` | `CreakingRenderer` | keyframe-driven in vanilla; posed here by the humanoid family |
| `copper_golem` | `model/animal/golem/CopperGolemModel.createBodyLayer` | `CopperGolemRenderer` | the standing pose; running/sitting/star are separate baked layers |
| `happy_ghast` | `model/animal/ghast/HappyGhastModel.createBodyLayer(false, CubeDeformation.NONE)` at `scaling(4.0F)` | `HappyGhastRenderer` | tentacle lengths are authored, unlike `GhastModel`'s seeded ones |
| `nautilus`, `zombie_nautilus` | `model/animal/nautilus/NautilusModel.createBodyLayer` | `NautilusRenderer`, `ZombieNautilusRenderer` | one layer, two sheets; the zombie's coral crust is `ZombieNautilusCoralModel`, a separate layer |
| `camel_husk` | `AdultCamelModel.createBodyLayer` (the same layer `camel` uses) | `CamelHuskRenderer` | one rig, its own sheet |
| `elder_guardian` | `GuardianModel.createElderGuardianLayer` — the guardian layer at `ELDER_GUARDIAN_SCALE`, `scaling(2.35F)` | `ElderGuardianRenderer` | same geometry, own sheet, so a corpus entry rather than a name alias |
| `parched` | `SkeletonModel.createSingleModelDualBodyLayer` | `ParchedRenderer` | **not** the skeleton layer: every part carries a second, larger overlay box |
| `mannequin` | none of its own — `player_wide` | the player's avatar renderer, via `EntityRenderDispatcher.getRenderer`'s type switch | a name alias, listed in the table above |

**What is deliberately absent, and why it is not an oversight.** Every emissive
eyes layer (`breeze`, `creaking`, `copper_golem`), the breeze's translucent wind
funnel, the sulfur cube's inner core and carried block, the zombie nautilus's
coral, the happy ghast's harness and ropes, and every baby variant are *separate
baked layers on their own sheets*. A corpus entry carries exactly one sheet and
one mesh, so folding any of them in would paint the second layer with the first
layer's UVs. They are second-pass work of the same shape as the sheep's wool
layer, which this document already describes.

`crates/lodestone-render/tests/invisible_but_solid_rigs.rs` is the gate: it
resolves each of the thirteen from its registry path and asserts the drawn world AABB
covers the collision box the registry declares, with three controls reproducing
the specific ways it could have gone wrong. It also records the breeze's known
0.619 shortfall, which is the missing wind funnel and nothing else.

### The projectiles and effects with a cuboid rig, and the six that need a draw path

The census's other entity bucket — *absent*, nothing draws it and nothing names
it — split cleanly in two, and the split is the useful part of this section.

Six had a real cuboid rig or reused one, and are now corpus entries or aliases.
Sources under `.cache/mc/26.2/client-src/net/minecraft/client`:

| corpus entry | vanilla layer | renderer | placement |
|---|---|---|---|
| `evoker_fangs` | `model/effects/EvokerFangsModel.createBodyLayer` | `EvokerFangsRenderer` | the mob placement — it really does flip and lift — with the renderer's `Ry(90 - yRot)` reached by a `π/2` root `y_rot` |
| `shulker_bullet` | `ShulkerBulletModel.createBodyLayer` | `ShulkerBulletRenderer` | `non_living_vehicle_placement`, bob `0.15` |
| `wither_skull` | declared inline in `WitherSkullRenderer.createSkullLayer` | `WitherSkullRenderer` | `non_living_vehicle_placement`, bob `0`, extra spin `180°` |
| `llama_spit` | `LlamaSpitModel.createBodyLayer` | `LlamaSpitRenderer` | `projectile_pitch_offset_deg`, offset `0` |
| `spawner_minecart`, `command_block_minecart` | the shared `MinecartModel` frame | `MinecartRenderer` | the minecart alias, as for the other four cart types |

**The deviations, each one named rather than left to be discovered.** A fang
draws in the layer's authored pose, which is the bite at full open with the base
still buried — vanilla raises it out of the ground over the bite and shrinks it
away at the end, both from a per-entity progress value. A shulker bullet tumbles
on three axes at three rates in vanilla and gets a fixed orientation here,
tolerable only because its three slabs are symmetric under a quarter turn; its
1.5× translucent halo is a second pass and is absent. A wither skull loses the
head *pitch* (this placement has no pitch term) and always draws the harmless
sheet rather than `wither_invulnerable`. A llama spit draws `0.15` blocks low:
vanilla lifts it in world space *before* its two rotations, and neither the
projectile matrix nor a mesh offset can express that — a mesh offset would tilt
with the shot.

**The six that are left need a draw path, not a rig**, which is why none of them
is a corpus entry away and why they were not attempted here:

| subject | what vanilla draws |
|---|---|
| `painting` | a quad textured from the painting variant, sized by the variant |
| `lightning_bolt` | procedural geometry rebuilt per frame from a seeded random walk |
| `fishing_bobber` | a camera-facing billboard plus a line back to the caster's hand |
| `dragon_fireball` | one camera-facing quad assembled vertex by vertex |
| `firework_rocket` | an item model, billboarded — see below |
| `ominous_item_spawner` | the carried item, spun and scaled in over 50 ticks |

The last two are worth separating from the first four, because "not a
`ThrownItemRenderer` registration" and "not drawn as an item" are two different
claims and `thrown_item_for`'s doc used to run them together. A firework rocket
*is* drawn exactly the way that table's members are drawn. What keeps it out is
its inputs: its stack comes from the entity rather than from a default, and a
crossbow-fired rocket is spun onto its flight axis by a metadata bit the draw
record does not carry. Adding it to that table would change what the table means
and take its parity gate's premise with it.

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

### Diagnosing a hitbox with no body

F3+B obtains hitboxes from entity state independently of GPU model draws. When
an entity type has no ordinary baked rig, `gpu/entity_passes.rs` skips that body
so specialised passes can render items, displays, paintings, sprites and moving
blocks. The `entity=debug` diagnostic filters those known dedicated-renderer
types (including thrown-item billboards and invisible marker/interaction
entities), then emits one record per remaining missing type with both the
registry type and resolved model path. Enable it with `RUST_LOG=entity=debug`
while reproducing a multiplayer scene; any record now identifies a bounded
mapping/corpus gap rather than an expected dispatch split.

The overlay follows the same generic visibility and pose rules as vanilla's
`EntityHitboxDebugRenderer`: an entity with the shared invisible flag has no
F3+B box, and player boxes/eye rays come from `Avatar`'s pose table rather than
a fraction of its standing height. The live F3+B path reads `Pose` and the
resolved `minecraft:scale` attribute into a small debug-only side table; normal
render `EntityDraw`s stay free of overlay-only fields. This matters for server
NPC helpers: an invisible helper player is neither a second hitbox nor a
second nametag, while an invisible named armour stand remains a valid vanilla
hologram because its renderer deliberately retains custom-name-visible text.

### Variant → texture (a wolf's breed, a pig's climate)

`entity_texture_candidates` answers "which sheet does this *model* have", which is
one sheet per model. Several mobs have more: nine wolf breeds and three climate
skins share one mesh apiece, and `lodestone-assets` has modelled all of them —
`EntityTexture::ByVariant`, and `EntityTexture::resolve(variant)` to select one —
since the corpus was written.

**`resolve` had no production caller.** Every consumer asked for `default_path()`,
so every wolf drew pale and every pig drew temperate. That is the *dual* of this
repo's usual island: not "nothing calls this subsystem" but "nothing **reads** this
function", and `cargo xtask connectedness` structurally cannot see it — the packet
decodes, the fold lands `Variant` on a component, and the gap is that nothing
downstream asks. The query that finds this class is *"what reads this?"*, not "is
every assignment the same constant".

The chain, end to end:

| step | where |
|---|---|
| wire → `EntityVariant::Keyed("minecraft:ashen")` | `protocol/v770`'s metadata decode |
| → `lodestone_ecs::entity::Variant` on the ingest entity | `lodestone_ecs::ingest::apply_entity_metadata` |
| → `EntityDraw::variant_sheet` (a corpus reference) | `entities.rs::extract_entity_draws`, via `lodestone_render::entity_variant_sheet` |
| → the draw-grouping key, so one batch is one sheet | `gpu/entity_passes.rs` |
| → a bind group | `gpu/frame.rs`, against `EntityRenderer::variant_textures` |

Three things about it worth knowing before changing it:

* **Only the axes that actually arrive are lifted.** `entity_variant_sheet` handles
  the wolf's breed and the pig/cow/chicken climate, both of which come over as
  `Keyed` holder ids. Horse colour, llama, cat, parrot and mooshroom have corpus
  entries and their own axes and are deliberately *absent* rather than half-lifted:
  each needs its own answer to "does this key or ordinal reach us", and guessing one
  wrong ships a confidently wrong skin rather than a missing one.
* **The variant joins the batch key.** Texture identity is not the model, so without
  it nine breeds collapse into one bind group and all nine draw whichever sheet won.
  Same shape, and the same reason, as `EntityDrawBatch::skin` for player skins.
* **The shell loads variant sheets by *listing*, not by enumerating the variant
  enums** (`resources.rs::load_entity_variant_textures`, walking
  `entity_variant_sheet_dirs`). An enumeration would be a second table beside the
  corpus's own `select` functions, free to drift the moment a breed is added; listing
  costs a few dozen extra decodes at startup and needs no change for a new variant.
  A reference the pack does not ship falls back to the model sheet.

**A tamed wolf now draws its tame sheet — the chain is closed end to
end.** `EntityMetadataUpdate::tamed`/`sitting` decode off the wire
(`v770`'s `read_entity_metadata`, `MetadataClass::Tamable`), and
`SimMob::snapshot` sends them for wolf/cat/parrot/ocelot.
`lodestone_ecs::ingest::apply_entity_leash`'s sibling arm,
`apply_entity_metadata`, now folds `metadata.tamed` into
`lodestone_ecs::entity::Tamed` — per-entity state, so `ingest` and not
`session`, per this repo's router table; mechanically confirmed by
`lodestone_model::event::route`, which answers `ClientEvent::EntityMetadataUpdated`
with `INGEST` alone (`session: false`), so no session query could ever see
this event even by mistake. `crates/lodestone-shell/src/entities.rs`'s
`extract_entity_draws` bridges `Tamed` off the ingest entity the same way it
bridges `Variant`, and the draw call site now calls
`entity_variant_sheet_for(&kind.0, &variant.0, tamed)` instead of the plain
`entity_variant_sheet`.

`entity_variant_sheet_for(model_name, variant, tamed: bool)` actually
selects `WolfState::Tame`/`Wild` from that last argument, tested by
`entity_variant_sheet_for_resolves_the_tame_sheet_when_told_the_wolf_is_tamed`,
and `lodestone-render`'s `lib.rs` now re-exports it — it existed but was never
re-exported, so no external crate could actually call it, an island in its
own right found while wiring this. The plain `entity_variant_sheet` is left
alone — still always `Wild` — for every remaining caller with no tame bit to
supply (fixtures, and any model with no tame axis);
`a_tamed_wolf_still_resolves_to_its_wild_sheet_through_the_plain_entry_point`
pins that as a deliberate, not a missed, choice. 26.2 also ships a `_baby`
axis (`WolfVariants.register` builds six identifiers, not three) which
`WolfState` does not model at all — unrelated to this fix and still open.

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

**Now decoded and integrated as far as the protocol/render seam goes; still an
island one hop short of production, in a crate this agent does not own.**
Live player report: "the creeper has a hiss but no explosion sound, and it
doesnt expand/turn white or blink or whatever." Three fixes, landed
incrementally:

* **The protocol decode.** `v770`'s `packets/metadata.rs` now has
  `MetadataClass::Creeper` and three class-guarded arms:
  `Creeper.DATA_SWELL_DIR` (index 16, `INT`), `DATA_IS_POWERED` (17,
  `BOOLEAN`), `DATA_IS_IGNITED` (18, `BOOLEAN`) — all three collide with
  *other* mobs' unrelated fields at the same index (a warden's anger level is
  also an `INT` at 16; see that module's `every_metadata_index_constant_
  matches_the_jar_dump` and its accompanying collision test), so the class
  guard is load-bearing, not decoration, exactly like the sheep/horse variant
  guards above it. `EntityMetadataUpdate` carries the three as
  `creeper_swell_dir`/`creeper_powered`/`creeper_ignited`. `handle_add_entity`
  synthesizes vanilla's own idle defaults (`-1`/`false`/`false`) at spawn for
  the same reason the sheep-wool fix does: `SynchedEntityData` never puts a
  field on the wire that is already at its accessor default, so an ordinary
  unlit creeper's spawn packet never mentions any of the three.
* **The per-tick integration.** `lodestone-shell`'s `entities.rs` has a
  `CreeperFuse` component (`swell_dir`/`old_swell`/`swell`), present only on
  entities whose `RenderKind` is `"creeper"`, and a `tick_creeper_fuse` system
  in `GameTick`/`TickSet::Animate` that does exactly `Creeper.java`'s
  `this.swell += swellDir`, clamped `0..=30`. `extract_entity_draws` lerps
  `old_swell`/`swell` by the frame's partial tick and divides by 28 (not 30 —
  see `MAX_SWELL`'s doc) into `EntityDraw::creeper_swelling`, which feeds
  *both* the scale above and the white-flash overlay below.
* **The white-flash overlay.** `entity_anim::creeper_white_overlay_progress`
  transcribes `CreeperRenderer.getWhiteOverlayProgress` — a **blink**, not a
  fade: `swelling` buckets into steps of `0.1`, odd-numbered buckets are "on"
  at a strength clamped to `0.5..=1.0`, even-numbered buckets are fully off.
  It reuses the hurt overlay's mechanism as predicted rather than building a
  parallel path, but needed its own **channel**: `EntityInstanceRaw` gained a
  `white_overlay: u32` attribute (location 10) alongside the existing
  `tint`/hurt-overlay word, because vanilla's red and white overlays are
  different rows of one `OverlayTexture` lookup and the tint word's spare byte
  was already fully claimed by the boolean red gate.
  `entity_pipeline::creeper_overlay_alpha_from_progress` transcribes
  `OverlayTexture`'s alpha derivation (`u = floor(progress*15)`, `alpha = (1 -
  u/15*0.75)*255`), and `entity.wgsl`'s `fs_main` blends white only when the
  red (hurt) overlay is **absent** — vanilla's `v == 3` row is flat red
  regardless of `u`, so a creeper that is somehow both hurt and swelling shows
  red, never a mix of the two.
  `creeper_white_overlay_pixels.rs` is a **magnitude** gate, not a direction
  one — CLAUDE.md's own retrospective on the hurt overlay (a
  direction-only check passed 3440/3440 while the shader rendered 70% red
  where vanilla renders 30%) is exactly the trap this was written not to
  repeat. It renders a flat-**black**-textured mob (so `shaded` is a hard
  zero regardless of lighting) with fog disabled, predicts the *exact* output
  byte from `OverlayTexture`'s formula and the shader's own documented
  `srgb_to_linear`, and separately predicts the swapped-argument hypothesis
  (that same exact bug shape, reproduced) — the measurement must land on
  the correct prediction and clearly miss the swapped one. Measured: byte 133
  vs. predicted-correct 133, predicted-swapped 13.

**Closed.** `lodestone-ecs/src/entity.rs` has a `CreeperSwellDir(pub i32)`
component, folded from `EntityMetadataUpdate::creeper_swell_dir` by a new arm
in `ingest.rs::apply_entity_metadata`; `lodestone-client`'s `EntityView` grew
the matching `creeper_swell_dir: Option<i32>` field and `entity_view()`
mapping. Only the direction is a component — `creeper_powered`/`creeper_ignited`
decode at the protocol layer but nothing downstream reads either one yet, so
per CLAUDE.md's island rule they stay protocol-only until a render path
consumes them.

The last hop — `net.rs::entity_snapshot`'s `creeper_swell_dir: None,` becoming
`creeper_swell_dir: view.creeper_swell_dir,` — is one line in a brokered file;
see the commit that lands this doc update for the exact patch. Once that
lands, a creeper's fuse direction reaches `EntitySnapshot` → `CreeperFuse` →
`tick_creeper_fuse` → `EntityDraw::creeper_swelling`, the chain that was
already fully wired and tested waiting on exactly this value.

**The chain was not "already fully wired", and the last hop was in this crate.**
The paragraph above was written in good faith and was wrong about its own
subject, which is worth keeping because the reason is general.
`EntityDraw::creeper_swelling` really did receive a correct, interpolated,
non-zero swell — and then **nothing read it**. The shell's
`RenderState::prepare_entities` resolved every entity through
`EntityModelSet::resolve_posed`, whose swell is a hard `0.0`, so
`Skeleton::pose_swelling`, `creeper_swell_scale`,
`creeper_white_overlay_progress` and `creeper_overlay_alpha_from_progress` had
**zero production callers between them** — every one of them was reached only
from its own tests. This field's own doc comment asserted "two consumers
downstream, both in `gpu.rs`", and a grep for either function in the whole
`gpu` module returned nothing.

Two lessons, both mechanical:

* **`swell = 0.0` is a documented exact identity**, which is what made the gap
  invisible: no frame looked wrong, no counter moved, and every unit test of
  every formula passed. An identity default is the perfect camouflage for a
  missing call, so a formula whose zero case is the identity needs a gate on
  its **caller**, not on itself.
* **`cargo xtask connectedness` structurally cannot see this.** It answers "is
  this clientbound packet reaching anything", and the packet was reaching
  `EntityDraw` fine. The detector that *would* have found it is different: a
  field on a render/instance struct whose every assignment site in the tree is
  the same literal constant. Every `creeper_swelling:` in the repo was `0.0`.

Fixed by `EntityModelSet::resolve_animated` (and
`EntityInstance::new_animated`), which take the swell and the death time and
which `prepare_entities` now calls; `resolve_posed` remains as the zero-extras
delegate for the five call sites that have neither. The white-flash byte joins
`(hurt, skin)` in that function's batch-grouping key, because the tint is one
repeated value per batch.

The culling-box gap is closed in the same pass. `EntityMesh::local_min`/
`local_max` came from `rest_pose()`, so a swelling creeper was drawn up to 41%
wider than its own box and would cull at the frame edge; `from_named_model` now
pads the creeper's local bounds by `MAX_SWELL_SCALE`, conjugated about
`MODEL_FEET_OFFSET` on the y axis exactly as `swell_root_affine` is (a plain
scale about the model origin would let the padded box sink below the feet
plane). It is padded once at bake time rather than recomputed per frame: one
constant box that always contains the drawn model costs a slightly conservative
cull and cannot drift from the pose, where a per-frame exact box would be a
second derivation of the same geometry.

**A separate, server-side gap, now also closed.** Everything
above is the decode/render side — it works against *any* server that sends
`SET_ENTITY_DATA`/`EXPLODE`, including a real vanilla one, which is what
every gate above was validated against. Our own integrated server
(`crates/lodestone-server`, `crates/protocol/v770/src/server_protocol.rs`)
is a *different* producer, and it used to never send either packet at
all when hosting: `MobSim::tick` already called `MobSim::explode` the tick
a creeper's fuse completed (the exposure/damage maths, `SwellGoal`
landed in `1feed17`/`16a5b9f`/`614acb8`), but nothing encoded `DATA_SWELL_DIR`
for the wire, and no `EXPLODE` encoder existed anywhere in this crate — so a
client connected to *our* server saw a creeper vanish with real blast damage
and no swelling animation, no particle, and no sound, even though the
render-side chain documented above was already complete.

Closed by a general `ServerProtocol::encode_set_entity_data(entity_id,
fields: &[MetadataField])` (replacing the single hardcoded
`encode_air_supply_update` local-player arm as the only per-entity metadata
mechanism) plus `ServerProtocol::encode_explode(centre, radius)`, both fed
from `SimMob::snapshot`/`MobSim::take_detonations` through the same
`EntityStreamer::sync` diff loop position/rotation already use. See
`crates/protocol/v770/tests/server_creeper_metadata_and_explode.rs` for the
gate: it drives `MobSim` through the same production tick path
`run_tick_loop` uses, encodes with our own server, and decodes with the
same `V770Adapter` `tests/live_creeper_explosion.rs` validated against real
captured vanilla bytes — asserting `creeper_swell_dir == Some(1)` on tick 1
and a detonation at exactly tick 30 that decodes to a `Particles` directive
then a `Sound` naming `minecraft:entity.generic.explode`.

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

**The diffuse term is vanilla's two lights, not one.** `ModelVertex` carries no
normal, so the fragment shader reconstructs one from screen-space derivatives of
the interpolated world position and applies
`assets/minecraft/shaders/include/light.glsl`'s `minecraft_mix_light`:

```wgsl
let n = -normalize(cross(dpdx(in.world), dpdy(in.world)));
let light_0 = normalize(vec3<f32>(0.2, 1.0, -0.7));   // Lighting.DIFFUSE_LIGHT_0
let light_1 = normalize(vec3<f32>(-0.2, 1.0, 0.7));   // Lighting.DIFFUSE_LIGHT_1
let diffuse = min(1.0, (max(dot(n, light_0), 0.0) + max(dot(n, light_1), 0.0)) * 0.6 + 0.4);
```

`0.6` is `MINECRAFT_LIGHT_POWER` and `0.4` is `MINECRAFT_AMBIENT_LIGHT`; the two
vectors are `com.mojang.blaze3d.platform.Lighting.DIFFUSE_LIGHT_0/1`, normalised,
which `Lighting.updateLevel(DEFAULT)` installs for the world. The **first-person
hand runs under the same entry**: `renderItemInHand` is called from inside
`renderLevel`, and `GameRenderer`'s only `setupFor(ITEMS_3D)` comes afterwards,
for the GUI.

Until a fix landed this was **one** light and an `abs()`
(`0.4 + 0.6 * abs(dot(n, normalize(0.3, 1.0, 0.55)))`), which is wrong in two
independent ways:

| surface normal | vanilla | one `abs()` light |
| --- | --- | --- |
| `+Y` up | 1.0000 | 0.9085 |
| `-Y` down | 0.4000 | 0.9085 |
| `±Z` north/south | 0.7396 | 0.6797 |
| `±X` east/west | 0.4970 | 0.5525 |
| `(0, 0.466, -0.847)` | 0.9138 | **0.4000** |

* `abs()` lights a face pointing *away* from the light exactly as brightly as one
  pointing into it, so undersides were 2.3x too bright.
* One direction has a whole **great circle** of normals perpendicular to it, all
  pinned at the `0.4` floor. Two near-opposing lights have no such band —
  `L1 = (-L0.x, L0.y, -L0.z)`, so for any normal exactly one of the two dots is
  positive and their sum is `0.8085*n.y + |0.1617*n.x - 0.5659*n.z|`; the only
  dark region is the underside.

Axis-aligned box faces never land on that band, which is why standing mobs looked
passable and the defect was reported against the *arm*: `first_person_arm_pose`
rotates it, and 2253 of its 2314 pixels sat at diffuse `0.497` where vanilla puts
them at `0.877` — measured, byte 64 against byte 112 on a mid-grey sheet.

**The reconstructed normal is negated, and that sign is derived rather than
asserted.** With a right-handed view matrix NDC y points up while framebuffer y
points down, so for a plane facing the camera `dpdx` runs along view `+x` and
`dpdy` along view `-y`; `cross(+x, -y) = -z` points *away* from the eye. Negating
gives the side being looked at, which for a closed mesh is the outward face — the
same answer vanilla reaches by computing both signs and letting `gl_FrontFacing`
choose (`entity.vsh`'s `PER_FACE_LIGHTING` pair). It is also what keeps
`cull_mode: None` safe: a lone quad is lit from whichever side is visible.

Getting that sign backwards is nearly invisible. `±X` and `±Z` box faces are
*equal* under a flip (the two lights are mirror images), so a flipped normal only
shows up as an inverted up/down — and, measured here, **a gate that asserts the
frame's set of shades matches vanilla's set passes the flip**, because a flip
permutes that set without changing it. `entity_diffuse_two_lights_pixels.rs`
therefore binds value to *location*: the topmost band of a box seen from above
must read vanilla's `1.0`, the bottommost band seen from below must read `0.4`.
Three controls were watched failing — the shipped one-light shader (12034/12034
and 2314/2314 pixels on the rival prediction), the sign flip, and dropping the
second light while keeping `max` (up face 113 where vanilla is 128).

**World light is per instance.** Vanilla samples the lightmap once per entity —
`EntityRenderer.getPackedLightCoords`, whose result becomes
`EntityRenderState.lightCoords` — so a mob is uniformly lit by *one* cell, and
every layer of it (body, armour, wool, held item) draws with that same byte.
That is why light rides the
*instance* buffer (`EntityInstanceRaw::light`, shader location 8) and not the
vertex buffer: the vertex buffer is shared by every instance of a model type and
could only ever say one thing for all of them. The shader turns the packed byte
into terrain's own light term — vanilla's `lightmap.fsh` curve, see
[light-ramp.md](./light-ramp.md). (This used to read "floor included"; the
retired ramp's `0.2` floor is gone, so an unlit mob is now black.)

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

### Render layers: sheep wool

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
  metadata byte (index 18 — corrected from an initial hand-count of 17 that
  missed `AgeableMob.AGE_LOCKED`; see `IDX_SHEEP_WOOL`'s own doc comment) into
  `EntityVariant::Dyed { color, sheared }`
  (`color` low nibble, `sheared` bit `0x10`), guarded on `MetadataClass::Sheep`
  so it cannot collide with another mob's byte-valued index 18.
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

**Landed.** The `EntitySnapshot`/`EntityDraw` half (originally items 2 and 3
here) landed first — see the fold above. The remaining mesh/pipeline/draw work
specified below is now also landed, unchanged from the spec in every
particular that mattered:

1. **`lodestone-render/src/entity.rs`** — `WoolMesh`/`SheepWoolModelSet`,
   mirroring `ArmourMesh`/`ArmourModelSet` field for field: `vertices`,
   `indices`, `parts: Vec<(&'static str, PartRange)>`, built from
   `sheep_wool_model()` via `bake_entity_parts` exactly as
   `ArmourMesh::for_slot` builds from `humanoid_armour_model`.
   `WoolMesh::attach` takes the wearer's **resolved model name** as an
   explicit second argument (`instance.model`/`EntityBatch::model`, not
   `Skeleton::family()`), because `Skeleton` itself carries no model name —
   unlike `wearer_carries_armour`, which can read `wearer.family()` straight
   off the skeleton it is handed. Two hermetic tests pin the pig/cow trap
   specifically for wool (`a_sheep_attaches_every_wool_part_to_its_own_body`,
   `a_pig_and_a_cow_attach_no_wool_despite_sharing_every_part_name` — the
   second is an **executed** negative control: gating on `wearer.family()`
   instead, as tried, makes it fail with `left: 6, right: 0` for both a pig
   and a cow, since every quadruped shares the exact part names wool looks
   up).
2. **`lodestone-render/src/entity_pipeline.rs`** — `GpuEntityModel::upload_wool`,
   mirroring `upload_armour` exactly.
3. **`lodestone-shell/src/gpu.rs`** — `RenderState::prepare_wool`, mirroring
   `prepare_armour`: skips sheep whose `EntityDraw::wool.sheared` is true
   (vanilla's own gate, applied at the point that draws the mesh — the field
   itself stays unfiltered upstream, as specified), else attaches the one
   wool mesh, tints via `sheep_wool_tint(color)`, and accumulates per-part
   instance buffers (`WoolPartAccum`, mirroring `ArmourPartAccum` minus the
   texture grouping armour needs and wool does not — one mesh, one sheet).
   Drawn through the **base** entity pipeline (`self.entities.pipeline.pipeline`),
   not `armour_pipeline`, for exactly the reason specified: wool has no second
   layer at the same inflation to correct z-fighting for. (Both pipelines are now
   `LessEqual`, so this is a choice of pass, not of depth
   compare — see `docs/armour-rendering.md`'s depth section.) `EntityRenderer` gained `wool_models: SheepWoolModelSet`,
   `wool_gpu: Option<GpuEntityModel>` and `wool_texture: Option<wgpu::BindGroup>`
   (no per-material table — there is only one mesh), loaded from
   `entity/sheep/sheep_wool.png` by a `load_sheep_wool_texture` that
   duplicates pack discovery for the same reason
   `load_humanoid_armour_textures` does (`resources::vanilla_manager` is
   `#[cfg(test)]`-only). The draw call sits in the render pass right after the
   `armour_batches` block, before the dropped-item pass, exactly as specified.
4. **The mechanical struct-literal widening** (`wool: None` on every
   `EntityDraw { .. }` literal in `gpu.rs`, `variant: None` on `sim.rs`'s test
   literal) had already landed by the time this pass started — no longer
   outstanding.

**Pixel evidence through the real shell path**, not just the reimplemented
plumbing above: `crates/lodestone-shell/tests/sheep_wool_pixels.rs`
(`#[ignore]`d) drives the actual `RenderState::render` call `app.rs` makes,
with a woolly sheep against the briefing's own suggested negative control — a
**sheared** sheep, identical in every other respect. Measured on the real
`26.2` jar:

```text
subject (woolly) non-sky px  = 8378
control (sheared) non-sky px = 7386
delta                        = 992
body-only ring estimate      = 2042.0 px (lower bound; head/legs not counted)
subject wool_layers_drawn    = 1
control wool_layers_drawn    = 0
```

The `lodestone-render` gate above (`sheep_wool_pixels.rs`, reimplementing
`ArmourMesh::attach`'s discipline against public API only) still passes
unmodified and reports the same `10151`/`0` figures it always did — it is now
corroborating evidence for the shipped `WoolMesh`/`prepare_wool` path rather
than the only proof anything works at all.

**Deliberately out of scope for this pass**, same as armour's equivalent list:

* **Baby sheep.** `BabySheepModel`/`textures/entity/sheep/sheep_wool_baby.png`
  is a separate, smaller mesh; not built.
* **The `jeb_` rainbow name easter egg.** `SheepRenderState.getWoolColor`
  lerps through every dye colour once named `jeb_`; `sheep_wool_tint` only
  implements the plain per-dye table.
* **`sheep_wool_undercoat.png` / `SheepWoolUndercoatLayer`.** A second overlay
  that only draws for a jeb_ sheep or a non-white one; not built, since it
  depends on the same unwired dye plumbing as the primary layer.

**A second, independent bug in this subsystem: a naturally white sheep
rendered with no wool at all** (player report: "White sheep render with no
wool. A brown sheep had wool properly."). Everything above — the mesh, the
tint table, the fold through `EntitySnapshot`, the pixel gates — was correct
and landed; the bug was one layer further down, in what the wire ever tells
the decoder in the first place.

**Root cause, confirmed against the real 26.2 jar, not assumed.** Vanilla's
`SynchedEntityData` only ever puts a field on the wire when it differs from
the accessor's own default:

```java
// SynchedEntityData.DataItem — .cache/mc/26.2/src/net/minecraft/network/syncher/SynchedEntityData.java:207-209
public boolean isSetToDefault() {
   return this.initialValue.equals(this.value);
}
```

```java
// SynchedEntityData.getNonDefaultValues — same file, :92-106
public @Nullable List<SynchedEntityData.DataValue<?>> getNonDefaultValues() {
   ...
   for (SynchedEntityData.DataItem<?> dataItem : this.itemsById) {
      if (!dataItem.isSetToDefault()) { ... }
   }
   ...
}
```

and `getNonDefaultValues()` — not `packDirty()` — is what a spawn's *initial*
`ClientboundSetEntityDataPacket` is built from, and what every later resend
(`addPairing`) uses too:

```java
// ServerEntity — .cache/mc/26.2/src/net/minecraft/server/level/ServerEntity.java:87 and :282-283
this.trackedDataValues = entity.getEntityData().getNonDefaultValues();
...
if (this.trackedDataValues != null) {
   broadcast.accept(new ClientboundSetEntityDataPacket(this.entity.getId(), this.trackedDataValues));
}
```

`Sheep` defines its wool accessor with default byte `0`:

```java
// Sheep.java:63, :114
private static final EntityDataAccessor<Byte> DATA_WOOL_ID = SynchedEntityData.defineId(Sheep.class, EntityDataSerializers.BYTE);
...
entityData.define(DATA_WOOL_ID, (byte)0);
```

and byte `0` decodes to `DyeColor.byId(0 & 15)` = **white**, sheared bit
(`0x10`) unset — `Sheep.java`. So a naturally white, unsheared sheep
never puts metadata index 18 on the wire, at spawn or ever. This is not a
decode bug: `read_entity_metadata` in
`crates/protocol/v770/src/packets/metadata.rs` was decoding correctly all
along — there was simply nothing in the byte stream to decode. `variant`
stayed `None`, `entities::sheep_wool` (which matches only
`Some(EntityVariant::Dyed { .. })`) returned `None`, and the wool layer never
drew. A dyed or sheared sheep worked throughout, because that state is
non-default and therefore always on the wire — exactly matching both
sightings in the report ("a brown sheep had wool properly").

**Live confirmation attempted, not obtained — recorded rather than silently
dropped.** The briefing stated a creative oracle was running on `:25570`/
`:25571`; at the time this fix was verified, the game port was closed, no
matching Java process was running, and the Docker daemon backing
`scripts/live-oracles/creative.sh` was not up on this machine. Bringing it up
was judged not worth the session budget for a claim the jar already proves
outside our own code (`decode(encode(x)) == x` is explicitly not good enough
per `CLAUDE.md`, but *vanilla source* is; see "Data sources, in order"). If
this needs re-confirming on the wire, `creative.sh` plus
`rcon-op.py … "summon minecraft:sheep …"` against a fresh entity, inspected
before it can be dyed, is the way — remember a freshly summoned entity is not
selector-visible until the next server tick.

**The fix, and where it lives.** `crates/lodestone-shell/src/entities.rs` was
off-limits (another agent's in-flight change), and the model layer's own
[`EntityMetadataUpdate::variant`] doc comment already specifies the intended
contract — "`None` means the packet did not carry a variant field; a consumer
treats that as the type's vanilla default, not 'unknown'" — so the fix
belongs at the point that first learns an entity is a sheep, not the point
that consumes the fold. `crates/protocol/v770/src/adapter/entity.rs`'s
`handle_add_entity` already computes `TrackedEntity { class:
metadata_class(name), .. }` at spawn — the exact "what the server said
becomes what the entity is" seam — so for a `MetadataClass::Sheep` spawn it
now also emits a synthetic `ClientEvent::EntityMetadataUpdated` carrying
`EntityVariant::Dyed { color: 0, sheared: false }`, through the **same**
channel a real `set_entity_data` uses. A later real `set_entity_data` naming
index 18 (dye, shear) decodes afterward in packet order and overwrites this
default exactly as it would overwrite any other value — no downstream
consumer (the ECS fold, the shell snapshot) needs a special case for
"unreported".

**A plausible-looking alternative that would have been wrong, recorded rather
than tried and reverted.** The tempting simpler fix is inside
`read_entity_metadata` itself: "if `class == Sheep` and index 18 never
appeared in this packet's list, default it." That is wrong, and the wrongness
is invisible from reading the decoder alone — `EntityMetadataUpdate` is
documented as *cumulative*: `None` means "this packet did not mention it",
consumed by folding only non-`None` fields into persisted state. The decoder
has no notion of "this is the entity's first packet" versus "the fortieth
unrelated update" (health, pose, air supply, …); defaulting inside it would
silently reset an already-dyed sheep back to white on every later packet that
happens not to mention wool. Defaulting must happen exactly once, at spawn.
`crates/protocol/v770/tests/sheep_wool_default.rs`'s
`the_raw_decoder_never_invents_a_default_only_spawn_does` pins this boundary
directly: an empty metadata list decoded for a `Sheep`-classed entity must
report `variant: None`, not a default — proof the raw decoder stays pure and
the synthesis lives only in `handle_add_entity`.

**Verification.** `crates/protocol/v770/tests/sheep_wool_default.rs`:
`sheep_spawn_with_no_metadata_packet_still_reports_default_wool` decodes only
an `add_entity` packet (no `set_entity_data` at all — the fixture is a
structurally genuine absence, not an explicit byte `0`, to avoid the "world"
species of vacuous test) and asserts the synthesized `Dyed { color: 0,
sheared: false }` is present; `non_sheep_spawn_synthesizes_no_wool_variant` is
the gating control (a pig spawn synthesizes nothing).
**Negative control, run and observed to fail:** the gate on
`tracked.class == Some(MetadataClass::Sheep)` in `handle_add_entity` was
temporarily replaced with `false && …`, reproducing the reported bug exactly:

```text
thread 'sheep_spawn_with_no_metadata_packet_still_reports_default_wool' panicked:
assertion `left == right` failed: ...
  left: None
 right: Some(Dyed { color: 0, sheared: false })
```

— restored immediately after, and `cargo test -p lodestone-v770
--no-fail-fast` reconfirmed all 63 test binaries green.
`crates/lodestone-render/tests/sheep_wool_pixels.rs` gained
`the_synthesized_default_wool_colour_renders_visibly` (`#[ignore]`d, GPU),
proving the render-side half: the exact colour the seam now picks
(`sheep_wool_tint(0)`) is a real, visible wool colour, not a sentinel —
measured `10151`/`65536` px differing from a bare sheep on this machine,
matching the existing gate's own figures for the same colour.

**Census: the same hole exists for every other `EntityVariant` arm, but
nothing else reaches a pixel today, so nothing else was fixed.** Grepped for
every producer and consumer of `lodestone_model::EntityVariant` across the
whole tree (not a named file):

| arm | vanilla accessor & default (jar-confirmed) | decoded today? | reaches a pixel today? |
|---|---|---|---|
| `Dyed` (sheep) | `Sheep.DATA_WOOL_ID`, byte `0` = white/unsheared | yes | **yes, now** (this fix) |
| `Horse { color, markings }` | `Horse.DATA_ID_TYPE_VARIANT`, int `0` = `Variant.WHITE`/no markings (`Horse.java`, `equine/Variant.java`) | yes (`IDX_HORSE_VARIANT`) | no — `EntityVariant::Horse` has no consumer outside `metadata.rs`'s own tests; `lodestone-assets`' `EntityVariant::HorseColor` is a *different*, unrelated enum in a different crate |
| `Villager { kind, profession, level }` | `Villager.DATA_VILLAGER_DATA`, `VillagerData(PLAINS, NONE, 1)` (`Villager.java`, `VillagerData.java`) | yes | no — no consumer outside `metadata.rs`'s own tests |
| `Keyed(id)` (registry-holder: pig/cow/chicken temperature, cat coat, wolf coat, frog, …) | one registry default per mob, e.g. `PigVariants.DEFAULT` (`Pig.java`), `CatVariants.BLACK` (`Cat.java`), `WolfVariants.DEFAULT` (`Wolf.java`) — same "never on the wire at default" shape, per mob | yes, by serializer (self-identifying, no class needed) | no — `lodestone-assets`' `EntityVariant::Temperature`/`Cat`/`Wolf`/… are again a separate enum; nothing bridges the two today |

Recommendation: **do not fix the other three arms yet.** Doing so now would
mean guessing at a default that has no pixel consumer to prove against —
exactly the risk `CLAUDE.md`'s evidence standards warn about ("an expected
value must originate outside the code under test"), and the sheep case had
the live report *and* the jar to cross-check against. The moment any of
`Horse`/`Villager`/`Keyed` grows a real renderer (the same
`WoolMesh`/`prepare_wool`-shaped work `entity.rs`/`gpu.rs` need for horses,
villagers or biome-variant animals), whoever lands it should add the matching
`handle_add_entity` synthesis at the same time, following this fix as the
template — and reuse `the_raw_decoder_never_invents_a_default_only_spawn_does`'s
shape as the boundary pin for whichever class they add.

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

### Mob fire

Player report: "mobs dont show flames yet." Landed as two separate halves —
see the two commits' own messages — because the bit extraction alone reaches
nothing and the render pass alone has no input.

**The bit.** `EntityDraw::on_fire` reads bit `0x01` of the shared-flags byte
via `lodestone_ecs::entity::EntityFlags`, bridged through `EntityIndex` in
`extract_entity_draws` (`entities.rs`) exactly the way `EntityDraw::hurt` and
`EntityDraw::item_use` already are — **not** through `EntitySnapshot`. This is
a deliberate correction to how the work was originally briefed: the briefing
proposed decoding the bit in `net.rs`'s `entity_snapshot` and threading it
through `EntitySnapshot`, but `EntityFlags` already exists, is already
populated for every remote entity by `lodestone-ecs::ingest::
apply_entity_metadata`, and is already read this same way by `MobState`/
`HurtTime`/`ItemUse` — so no `net.rs` change, and no new `EntitySnapshot`
field, was needed at all.

**The geometry**, derived from vanilla's `FlameFeatureRenderer.prepare`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/feature/
FlameFeatureRenderer.java`) — see `flame_quads`'s doc in
`lodestone-render/src/entity_pipeline.rs` for the full line-by-line derivation
and `lodestone-render/tests` (in-module `entity_pipeline::tests`) for the
predicted-vs-measured geometry (a zombie: 6 quads, first-quad world
half-width 0.42 blocks, stack top ~3.07 blocks above the feet):

* One camera-**yaw-only** billboarded column of quads (not axis-aligned, and
  not a full look-at billboard — the rotation is the *camera's own* yaw,
  identical for every flame drawn that frame, not a per-entity vector toward
  the camera), stacked from the entity's feet upward, shrinking (`×0.9` per
  quad) and receding in depth (`-0.03` per quad) as it rises.
* Scaled by the entity's **own hitbox** (`lodestone_data::entity_dimensions`
  times its age scale — vanilla's `EntityRenderState.boundingBoxWidth`, i.e.
  `getDimensions().scale(getAgeScale())`), not this crate's own baked mesh
  AABB — the two differ for several mobs (e.g. a zombie's model geometry
  includes its outstretched arms; its hitbox does not).

#### The transform: two bugs and the reasoning that hid each

Both were reported by the player as one symptom pair ("the fire is tied to one
side of the mob" and "a baby zombie's fire should be smaller"). Both live in the
*caller*, not in `flame_quads`, which was faithful throughout.
`flame_instance_matrix` is now the single place either is decided.

**1. The billboard rotation is `Ry(PI - yaw)`, not `Ry(yaw)`.** Vanilla's
`Mth.rotationAroundAxis(Mth.Y_AXIS, camera.orientation, …)` is a swing/twist
decomposition about Y, and `Camera.setRotation` builds
`rotationYXZ(PI - yaw, -pitch, 0)`, so the projection is exactly `Ry(PI - yaw)`
— the pitch term drops out, which is why this takes a yaw and not a camera.

The wrong version was defended in a code comment: entity draws are double-sided
(`cull_mode: None`), so *a flat billboard* reads identically face-on for either
sign. **The flame is not a flat billboard.** It is a stack that steps forward in
`z` (`-0.03` per quad, on top of a `0.3` pose-level push) *and* insets laterally
(`×0.9`), and a stack with depth and lateral asymmetry is not sign-symmetric.
With the sign wrong the flame counter-rotates as you orbit instead of following:
correct from one azimuth, displaced from the opposite one. That is a depth-order
symptom, which is precisely what `cull_mode: None` converts a bad transform into
— the general rule `CLAUDE.md` already states, applied to a case where the
comment argued the other way.

The derivable invariant, and what the gate asserts: **the local `+Z` the
billboard maps must point toward the camera**, i.e. against `Camera::forward`.
Derived from a real `Camera` at eight azimuths, with both wrong hypotheses
computed in the same run and required to fail. The off-axis azimuths are
load-bearing — a **pure sign flip** (`Ry(-yaw)`) is invisible at yaw `0` and
`180`, so a gate checking only the two obvious opposed sides passes with it.
Measured: the shipped `Ry(yaw)` reddens at yaw `0`; `Ry(PI + yaw)` survives yaw
`0` and reddens first at yaw `45`.

**2. `pose.scale(s, s, s)` was missing entirely, and the mesh was baked per
model type.** `s = boundingBoxWidth * 1.4` was computed inside `flame_quads` (to
derive `h`) and never applied to anything, so every flame was `1/s` times too
large — worst on a wide mob, where `s` is furthest from `1`. `flame_quads`' own
doc claimed it "multiplies through by `s` at the end"; it never did, and the
existing geometry tests all multiplied by `s` by hand to get world coordinates,
which is what made the omission invisible.

The scale is now per **instance** and the mesh stays per **type**, and the reason
is a piece of arithmetic worth keeping:

> The layer count comes from `h = height / s = height / (width × 1.4)`, which is
> **invariant** under a uniform hitbox scale. An age scale is uniform, so a baby
> and an adult of one type have the *same* layer count and differ only in `s`.

So a baby zombie's flame is exactly half an adult's, with six quads in both —
which is what vanilla itself draws. **A "babies get fewer layers" rule would be a
second, wrong change**: the layer count varies with **aspect ratio**, not age (a
spider gets 2 quads to a zombie's 6, and both keep their count as babies). The
gate predicts both numbers from vanilla's constants — `s = 0.84` adult, `0.42`
baby — and asserts the counts are *equal*, with the spider arm as the control
that the count is not simply constant.

`EntityDraw::scale` is the age scale (`0.5` for a `Baby`, `1.0` otherwise), so
the per-entity box needed no new plumbing; `flame_hitbox_width` in
`gpu/entity_passes.rs` is the one multiply.
* Alternates between two textures (`fire_0`/`fire_1`) every quad, combined
  into one side-by-side texture by
  `lodestone_assets::entity_flame::load_combined_flame_texture` so the flame
  pass binds exactly one extra texture, not two.
* Animated by a 32-frame vertical strip per texture, **not** a per-vertex UV
  scroll — see `load_combined_flame_texture`'s doc for `fire_0.png`'s
  `fire_0.png.mcmeta`-specified frame permutation, which `fire_1` does not
  share and which had to be corrected on the CPU side so both textures index
  by a plain `tick % 32`.
* Rendered **cutout, not translucent**: vanilla's own render type
  (`RenderTypes.entityCutoutCull`) is `ALPHA_CUTOUT` at `0.1` with backface
  culling, full-bright forced block light, and no per-face diffuse shading —
  `fs_main_flame`/`vs_main_flame` in `entity.wgsl` skip the mob shader's
  two-light diffuse entirely rather than applying it to a self-lit sprite.

**The pipeline** reuses `EntityPipeline`'s existing two bind-group layouts
(camera, texture) — a fourth pipeline variant alongside the mob/armour/banner
ones, over the 4-bind-group floor for exactly the same reason those three
are. Its own instance format, `FlameInstanceRaw` (a model matrix plus the
current animation frame — no light/tint/overlay word, since vanilla's flame
never varies any of those), is why it needs its own vertex entry point
(`vs_main_flame`) rather than sharing `vs_main`'s.

### Entity shadows

Owner report: "entity shadows are missing" (also filed as the video option
appearing unimplemented). There was no shadow machinery anywhere on this
tree before this landed — no `EntityShadow` type, no quad/decal geometry, no
shader — confirmed by a search across `lodestone-shell`/`lodestone-render`
before writing any of it, per this repo's "check for existing machinery
first" rule.

**The algorithm**, transcribed from `EntityRenderer.extractShadow`/
`extractShadowPiece` (`.cache/mc/26.2/client-src`): for each shadow-casting
entity, scan the block column(s) under it out to its shadow radius, and for
every candidate Y layer from `floor(feet.y - depth)` up to `floor(feet.y)`
(`depth` shrinking with distance-to-camera and light, capped at the radius),
ask whether the block *below* that layer is solid ground. If so, emit a flat
quad at that layer's floor, sized to the block, textured with
`textures/misc/shadow.png` (a radial gradient) at a UV computed from the
piece's offset from the entity and the entity's own radius — so a wide
shadow radius spreads one gradient sprite across several quads rather than
tiling it. Alpha comes from `powerAtDepth * 0.5 * Lightmap.getBrightness(..)`,
clamped to `[0, 1]`, and a piece is skipped outright if the local raw
brightness is `<= 3`. `RenderState::prepare_shadows`
(`gpu/entity_passes.rs`) is the whole implementation; `push_shadow_quad`
builds the two triangles.

**One disclosed simplification left**, plus one that is now closed:

* **Per-species shadow radius and strength: landed.** `SHADOW_RADII` and
  `SHADOW_STRENGTHS` in `gpu/entity_passes.rs` carry all 157 registered
  entity types, generated by `scripts/dump-entity-shadows.py` from the
  decompiled client. Owner report: shadows "look good! maybe a bit too big
  though" — they were, and by more than the old note admitted.

  The flat `0.5` was disclosed as harmless ("a chicken casts a slightly
  oversized shadow and a cow a slightly undersized one, never a missing or a
  wildly wrong one"). Both halves were false.
  `EntityRenderer.shadowRadius`'s own field default is **`0.0F`** and **35 of
  the 157 types take it** — every arrow and thrown item, item frames,
  paintings, armour stands, shulkers, `interaction`/`marker` — so a flat
  `0.5` drew a player-sized disc under things vanilla gives no shadow at all.
  Among the types that do cast one the spread is **21×**, `0.14` (tadpole) to
  `3.0` (giant), with `0.15` for a dropped item and an experience orb, the two
  you see most. `0.5` is the modal value only because the humanoids cluster
  there (34 of 157) against `0.8` for the boats (21) and `0.7` for the
  quadrupeds and minecarts (25).

  The predecessor note said the tractable route was a JVM oracle in
  `EntityDataIndexOracle.java`'s shape. **That route does not exist**, and
  knowing why saves the next reader the attempt: that oracle works because
  `EntityDataAccessor`s are `static` fields a bare `Bootstrap.bootStrap()` can
  read, whereas `shadowRadius` is an **instance** field assigned in a
  constructor that needs a live `EntityRendererProvider.Context` — a
  `Minecraft`, a font, a baked model set. There is no headless way in. The
  decompiled source read mechanically is the outside source instead, and the
  script's header carries the two traps that made its first two generations
  silently wrong (a generic `extends` bound read as a superclass, which put 51
  types at a plausible-looking `0.0`; and a literal passed at the
  *registration* site rather than in the renderer, as `giant` does).

  Still not right: a `Display` entity's shadow radius is genuinely **synced
  per-entity** rather than a renderer constant, so the three `*_display` rows
  carry the accessor default (`0.0`) and `EntityDraw` would have to carry the
  reported value before they can be.
* **Ground detection is "does the collision shape fill the whole cell",
  not the block's real sub-shape.** Vanilla paints a shadow shaped like the
  slab or stair it sits on; this pass gates on
  `lodestone_data::collision_shapes::collision_boxes` covering the full unit
  cube and, for a slab/stair/fence/carpet/etc., draws **nothing at that
  layer** — the per-column scan then keeps going downward through the rest
  of its Y range, so the entity's shadow either lands one cell lower on the
  next full block underneath, or is absent if none is within `depth`. An
  edge (the entity standing half over open air) is unaffected: each column
  is independent, so the columns with ground still draw and the columns
  without still do not — only an individual non-full piece's *shape* is
  approximated away, not the coverage pattern.

**The ground and light queries** reuse existing seams rather than adding new
world-crossing plumbing beyond one: `RenderState::set_entity_light_source`
(already installed for mob lighting) is reused as-is to sample brightness at
each candidate cell, and a new, matching `ShadowGroundSource`
(`gpu/sources.rs`) — `Fn([i32; 3]) -> Option<u32>`, a raw block-state id —
is installed the same way, at connect time, on **both** independent connect
paths (`app/session.rs::install_shadow_ground_source`, called from
`connect_to`, and `app/lifecycle.rs`'s own `--connect` path, which already
duplicates `set_entity_light_source`/`set_sky_darken_source` for the same
structural reason). The closure is just `NetClient::block_at` through a
cloned `SharedHandle`, the same "hand out a cheap `'static` handle" shape
`entity_light_at` already uses.

**The pipeline** does *not* go through `build_entity_pipeline` the way the
mob/armour/banner/flame/orb/water-mask pipelines do: every one of those
shares a static `ModelVertex` mesh plus a per-instance transform buffer, and
a shadow piece has no shared mesh to instance — each one is unique,
positioned and sized fresh every frame. `EntityPipeline::shadow_pipeline`
instead takes a single plain (non-instanced) `ShadowVertex` buffer (position,
UV, per-piece alpha), rebuilt and re-uploaded whole each frame — the same
shape a debug-line or block-outline draw already uses elsewhere in this
engine. It still reuses the entity pipeline's own camera/texture bind-group
layouts, so the pass spends the same two groups every other entity pass
does. State is vanilla's own `RenderPipelines.ENTITY_SHADOW`
(`ColorTargetState(TRANSLUCENT)`, `DepthStencilState(GREATER_THAN_OR_EQUAL,
writeDepth = false)`), translated through this engine's `[0,1]` depth the
same way every other entity pipeline here does (`GREATER_THAN_OR_EQUAL` →
`LessEqual`). Fog is **not** applied (vanilla's shadow render type inherits
it) — a shadow piece sits within a few blocks of its casting entity and the
radius is capped at 32, so the visible cost is a shadow that stays a hair
too dark at the extreme edge of render distance, never a wrong one up close.

**One deliberate divergence: a polygon offset vanilla does not carry.** Owner
report: "the entity ground-shadow decal z-fights with the ground". A shadow
piece is placed *exactly* coplanar with the ground — `ShadowFeatureRenderer.
prepare` emits it at `piece.relativeY() + shapeBelow().bounds().minY`, and the
only blocks reaching it are the ones `isCollisionShapeFullBlock` accepted,
whose bounds are the unit cube. Zero separation, and vanilla's `ENTITY_SHADOW`
uses the two-argument `DepthStencilState`, so it carries no bias either.
Vanilla can afford that because it is reversed-Z. This renderer's forward
`[0,1]` depth cannot. Measured (`near = 0.05`, `far =
far_for_render_distance(12)`, `Depth32Float`), one ULP of the depth buffer is
worth, in blocks of world separation:

| distance | forward `[0,1]` (here) | reversed-Z (vanilla) |
|---|---|---|
| 2 blocks | `2.44e-06` | `5.96e-08` |
| 8 blocks | `3.84e-05` | `2.38e-07` |
| 16 blocks | `1.55e-04` | `4.77e-07` |

16 blocks is the whole reach of the feature — `pow = (1 - distSq / 256) *
strength` must be positive — so that is the full range, not a slice of it.
The *shape* of the left column is the finding: a ULP's worth grows as the
**square** of the distance, a 64x swing across the feature's own reach, so no
fixed world-space lift can work. Anything large enough to resolve at 16 blocks
visibly floats the decal at 2; anything discreet at 2 is unresolvable at 16.

`EntityPipeline::SHADOW_DEPTH_BIAS` is a polygon offset instead, which does not
have that shape: for a floating-point depth format the offset unit is one ULP
of the *primitive's own* depth, so `constant: -10` is ten ULPs wherever the
piece happens to be — `4.5e-05` blocks of pull at 2 blocks, `2.96e-03` at 16,
and never more, because the feature's own distance cutoff bounds it. Negative
because `[0,1]` depth puts *nearer* at *lower*; this is the same translation
`crack_pipeline` documents at length for the block-breaking decal, which is
this repo's other coplanar-decal pass. Measured effect at fixed camera and
fixture: the decal's footprint grows `348 → 366` px at 7.8 blocks, `146 → 160`
at 10.3 and `78 → 88` at 13.0, and is unchanged at 5.4 — pixels that were
losing the depth comparison to the coplanar ground, in a fraction that grows
with distance exactly as the table predicts.

The same depth-buffer policy now applies to **depth-writing entity geometry**. A
living model is positioned with its feet on the entity's reported `y`, and a chicken
toe can therefore be exactly coplanar with the terrain top face. Moving the chicken
model up by an epsilon would be a species-specific approximation and would fail at
different camera distances. `build_entity_pipeline` instead gives every
depth-writing entity variant the shared `CAMERA_DEPTH_BIAS`; depth-read-only passes
remain unbiased, and translucent terrain remains on its separate no-depth-write
pipeline. The bias is only a few ULPs, so it stabilizes physical contact without
making an entity behind a real intervening surface visible.

Note what was *not* observed, because it changes what a future reader should
expect: across twelve headless configurations (distance, world-coordinate
magnitude, far plane, grazing angle, sub-block feet offsets) the unbiased
decal never **speckled**. Coplanar surfaces in this renderer flip wholesale or
lose a fringe, not per pixel — `ground_plate_z_fight_pixels.rs` measured the
same for ground plates. So a report of "shadow z-fighting" here means a decal
that is partly or wholly missing, not one that shimmers.

The `entityShadows` video option (`menu/options.rs`'s `LiveOption::
EntityShadows`) gates the whole pass — `RenderState::
set_entity_shadows_enabled`, polled every frame in `app/redraw.rs` beside
`Sim::set_cutout_leaves` — and, like every other pipeline here, `shadow_texture`
is `None` without a vanilla pack, in which case the pass draws nothing rather
than a synthetic placeholder.

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
  `Fn(Vec3) -> Option<u8>` returning packed `sky << 4 | block` at **an arbitrary
  world position** — the source is position-agnostic and the caller decides where
  to probe (see "Which cell an entity samples" below). Until something installs
  one, every mob is `ENTITY_FULLBRIGHT`. The equivalent world lookup already
  exists for particles in `Sim::extract_particles`.

### The swim body-pitch rotation (issue #573), and why it is player-only

`lodestone_physics::player::PlayerState::swim_amount`/`swim_amount_o` (the local
player's own `0..1` ramp toward the swim pose) and `crate::entities::SwimRamp` /
`tick_swim_ramp` (the same ramp, integrated client-side for every *other* tracked
entity off the ingest `Pose` component — only the pose is ever synced, never the ramp,
exactly like `CreeperFuse`) both existed with nothing reading them: the model pose stood
upright regardless. `EntityDraw::swim_amount` is the last hop, and
`gpu/entity_passes.rs`'s `apply_swim_rotation` is the consumer.

**Gated on `type_path == "player"`, not on `swim_amount > 0.0` alone.** Vanilla's base
`LivingEntityRenderer.setupRotations` has **no** swim branch — only `AvatarRenderer`
(the player) and `DrownedRenderer` override it, with two different formulas (a plain
rotation for the player, a `rotateAround` the vertical centre for a drowned zombie).
`SwimRamp` is populated for every entity kind because the *pose* concept is universal;
the rotation this build applies is not, so the species gate lives at the render call
site rather than upstream in the producer.

**Composed by conjugation.** `resolve_animated`'s output already equals
`A · flip_scale · lift` for `A = T(feet) · Ry(180 − yaw) · Rz(fall_over)` — the same
decomposition `dying_entity_model_matrix`'s own doc names. Vanilla inserts the swim
rotation exactly between the yaw/fall-over term and the Y-down flip, so
`apply_swim_rotation` rebuilds `A` from the same `feet`/`yaw`/`death_time` the resolver
was called with and left-multiplies every already-baked matrix
(`transform`/`part_transforms`/`hand_transforms`) by `A · Rx(xAngle) · A⁻¹` — reproducing
`A · Rx(xAngle) · flip_scale · lift` without decomposing the baked matrices back into
their factors. The entity's AABB is conservatively re-inflated to a sphere around `feet`
sized by the old maximum corner distance, since the true rotated box is more expensive to
recompute and a larger box only costs an occasional missed cull, never a wrong one.

**Two vanilla pieces are not ported**, both because the input is not available at this
call site: the `isInWater` branch of `targetXRot` (this always takes the water-submerged
reading — `PlayerState::swimming` requires real fluid contact to ever ramp up, so this is
wrong only for the tail of the ramp decaying back to `0.0` after leaving the water), and
`isVisuallySwimming`'s extra `translate(0, -1, 0.3)` crawl nudge (no fluid/on-ground
state reaches `prepare_entities` today).

**Remote players are covered; the local player's own third-person body is not.**
`gpu/sources.rs`'s `ThirdPersonBodyState` (what `F5` third person draws for your own
avatar) has no `swim_amount` field yet — `sim/camera.rs::third_person_body_state` builds
it from `Sim`'s own `PlayerState`, but nothing plumbs the value through, so `into_draw`
sets `swim_amount: 0.0` unconditionally. **Crawling is not covered either**: it shares
vanilla's `Pose.SWIMMING` mechanism (crawling *is* the swimming pose, entered under a
one-block gap) and therefore shares this same rotation once `Pose` reports it, but the
`isVisuallySwimming` translate above is exactly the crawling-specific piece this build
does not port — so a crawling entity gets the body-pitch rotation but not the forward
nudge vanilla adds on top of it.

### Which cell an entity samples, and what fire does to it

`gpu/entity_passes.rs`'s `entity_light` is the single place any entity pass gets
its light. Both of its rules were missing until a player-visible lighting pass
went looking, and both are one line of vanilla:

* **The probe is the entity's eye, not its feet.**
  `EntityRenderer.getPackedLightCoords` is
  `BlockPos.containing(entity.getLightProbePosition(t))`, and
  `Entity.getLightProbePosition` returns `getEyePosition`. So a tall mob standing
  in a dark cell with its head in a lit one is lit *by its head*. Every call site
  passed `feet` before, and `EntityLightSource`'s own doc claimed that was
  vanilla — it never was. `FirstPersonHand::hand_light` was always right, because
  it samples at `camera.position`, which already is the eye.
* **Fire forces the block half to 15, and only the block half.**
  `EntityRenderer.getBlockLightLevel` is
  `entity.isOnFire() ? 15 : level.getBrightness(BLOCK, pos)`;
  `getSkyLightLevel` has no such branch. Forcing the whole byte would give a
  burning mob in a pitch-dark cave a daytime sky as well. Here the block half is
  the low nibble, so it is `| 0x0F` — vanilla spells it
  `LightCoordsUtil.withBlock`.

**Eye height is per type and is not `height * 0.85`.** That formula is only
`EntityDimensions.defaultEyeHeight`, which 56 of 26.2's 158 registered types
take; the other 102 name an explicit `EntityType.Builder.eyeHeight`, and
`EYE_HEIGHTS` in `entity_passes.rs` carries those. The trap is that most
overrides are small enough to floor into the *same* block cell as the default for
an entity standing on integer `y`, so a wrong table looks right in a screenshot —
`elder_guardian`, `ghast` and `happy_ghast` are the three that do not, and any
entity at a non-integer `y` moves the boundary under all the rest. That is also
why the gate in `entity_passes.rs` uses a ghast: it separates eye-probing from
feet-probing *and* from the 0.85 formula, three different bytes on one input.

Two things `EYE_HEIGHTS` deliberately cannot express, because `EntityDraw` has no
input for them: **pose** (a crouching player's eye is `1.27`, a swimming one's
`0.4`) and **a baby's own `BABY_DIMENSIONS`** (a baby zombie's eye is `0.775`,
not the adult's `1.74` halved). The age-scale approximation lands in the right
block cell for every baby checked, so the probe is right and the number is not.
To fix either properly, `EntityDraw` needs to carry the pose.

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
`max(get_brightness(sky) * sky_darken, get_brightness(block))`, then mixes
`notGamma` in — see [light-ramp.md](./light-ramp.md). Note the order: the curve
is applied to the raw level and `sky_darken` scales the *result*.

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
* `crates/lodestone-shell/tests/sheep_wool_pixels.rs` (`#[ignore]`d) — the
  island check: drives the real `RenderState::render` call, not reimplemented
  plumbing, with the sheared-vs-woolly pair as its negative control and
  `RenderStats::wool_layers_drawn` (1 vs. 0) as an exact corroboration. Also
  `crates/lodestone-render/src/entity.rs`'s
  `a_pig_and_a_cow_attach_no_wool_despite_sharing_every_part_name`, a hermetic
  **executed** negative control for the pig/cow trap: gating `WoolMesh::attach`
  on `wearer.family()` instead of the resolved model name makes it fail with
  `left: 6, right: 0` for both animals, since every quadruped shares the exact
  part names wool looks up.

## The reconstructed normal is model-local, and why that matters

`entity.wgsl`'s `shade_entity` has no per-vertex normal to work from, so it
rebuilds one from the screen-space derivatives of a position varying. That
varying is `VsOut::local` — the vertex's offset from its own instance's
model-matrix translation — and **not** `VsOut::world`, which exists only for
the distance-fog term.

A derivative is translation-invariant, so both give mathematically the same
normal. What differs is the precision the interpolator has to spend. Fed
absolute world coordinates the varying quantises to the `f32` ULP at the
player's distance from the origin — 0.00195 blocks at 30,000 — against a
per-pixel step across a half-block skull of about 0.005. The interpolated value
then climbs in a staircase, and the derivative of a staircase is either zero or
one whole step, chosen per 2×2 quad. The normal is noise, and the two-light
diffuse term paints that noise as dense per-pixel speckle over every entity and
block entity: mobs, players, banners, skulls, armour.

The signature is what identifies it, and it is what an owner report described:
an **axis-aligned** face holds two of its three world components exactly
constant, so those derivatives are exactly zero and nothing can cancel, while
an off-axis face varies in all three. "Static only when the head is not at
0/90/180/270." Measured on the real skull rig with a uniform texture, so any
spatial variation at all is geometry or shading and never texel data
(speckle / roughness, `crates/lodestone-render/tests/block_entity_rotation_noise_pixels.rs`):

| world origin | 0°/90°/180°/270° | 22.5° | 45° | 67.5° |
|---|---|---|---|---|
| 0 … 8,000 | 0 / 0 | 0 / 0 | 0 / 0–1 | 0 / 0 |
| 30,000 | 0 / 0 | 8 / 24 | 4 / 8 | 0 / 38 |
| 100,000 | 0 | 8 | 3 | 175 |

All zero after the change; putting the one line back to `dpdx(in.world)` fails
that gate and only that gate, which is the control.

**The coordinate at which it bites depends on the render resolution**, because
the race is the per-pixel step against the ULP and doubling the resolution
halves the step. On the neutered shader, speckled pixels at 67.5° with the rig
1.2 blocks from the eye: the first non-zero arm is at world 30,000 in a 256²
frame and at **4,096** in a 1024² one (320 px, against 0 at every axis-aligned
rotation at both). A real window is larger still, so a clean headless frame is
evidence of the same inequality with more margin, never of a clean frame on
someone's screen. The gate runs at 512² and sweeps out to 100,000.

**Two things this does not fix.** The subtraction has to happen in the vertex
stage — differencing two already-quantised large numbers in the fragment
recovers nothing, which is why `fog_amount`'s own `in.world - camera.fog_eye.xyz`
is not a precedent to copy. And `world = model * position` / `clip = view_proj *
world` are still absolute, so the *vertex positions* still quantise: at 100,000
one ULP is 0.0078 blocks against a banner flag box only 0.0625 blocks deep, and
its 1-texel side faces collapse toward degenerate (3 interior pixels, measured).
Removing that needs the instance matrices built camera-relative on the CPU, in
`entity_pipeline.rs` and its callers, not a shader change.

## Mip depth, per texture — the census

Asked while chasing the same report, because "an allocated-but-unwritten mip
level" is exactly 50/50 noise and would have explained it. It is not what is
happening here, and the answer is worth keeping so nobody re-derives it:

| texture | levels | every level written? |
|---|---|---|
| every stitched atlas (block, model, GUI, item, particle, container, crack) — `GpuAtlas::upload_mips` | `mips.len()`, from `atlas_mip_levels` | yes, by construction |
| entity + block-entity sheets, player/remote skins, banner and shield masks — `entity_texture_from_image` | 1 | yes |
| glint sheet, map textures, panorama, menu blur, HUD icon sheets, sky, weather, screen effects, GPU-timing scratch, render target, depth buffer | 1 | yes |

`GpuAtlas::upload_mips` sets `mip_level_count = mips.len()` and then writes
exactly `mips.len()` levels, so the two cannot disagree; `Atlas::mip_count` is
`1 + mips.len()` and `Atlas::mip` covers every index in that range, so
`atlas_mip_levels`' `filter_map` never silently drops one. **Nothing in the tree
allocates a mip level it does not write.**

Two corollaries. `model.wgsl`'s `sample_rgss` — the supersampling that fixed
minified terrain cutouts — is in the *terrain* shader only; `entity.wgsl` takes
a single plain `textureSample` and its sheets carry one level, so narrowing
RGSS would change nothing about entities. And `dpdx`/`dpdy` appear in exactly
three shaders: `block.wgsl` and `model.wgsl` differentiate *UVs*, which are
small and well conditioned, and `entity.wgsl` differentiates a *position*,
which is the case above.

## Dependencies

`lodestone-assets` (`entity_models` corpus, `bake_entity_parts`, `ZipSource`,
`Image`), `lodestone-entity::pose` (`WalkAnimation`, `walk_target_speed`),
`glam`, `wgpu` via `entity_pipeline`.
