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

### Walk cycle

`entities.rs` samples the **drawn** (interpolated) position once per 20 Hz tick
and feeds the distance to `walk_target_speed` = `min(distance * 4, 1)`, matching
vanilla's `updateWalkAnimation`. See that module's docs for why the tempting
alternative — the gap a fresh snapshot opens up — is wrong by exactly
`INTERP_STEPS` (3×) and makes every mob's legs swing too far and too fast.

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
* `tests/entity_variant_pixels.rs` — a drowned renders as a drowned (resolved
  through `EntityModelSet::resolve`, so the alias table is inside the gate), and
  a zombie's arms are out in front. Each uses the **pre-fix build as its own
  negative control**.

## Dependencies

`lodestone-assets` (`entity_models` corpus, `bake_entity_parts`, `ZipSource`,
`Image`), `lodestone-entity::pose` (`WalkAnimation`, `walk_target_speed`),
`glam`, `wgpu` via `entity_pipeline`.
