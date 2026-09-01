# Entity picking

## What it is

The client's view-ray resolution of *which entity, if any, the crosshair is on* — vanilla's
`EntitySelector.CAN_BE_PICKED` predicate plus the ray-versus-hitbox search that consumes it. It decides
what a left-click attacks, what a right-click interacts with, and what middle-click picks, so getting the
candidate set wrong is directly visible as a wrong attack target — and, against a real server, as a
disconnect.

## How it works

`Sim::update_target` casts one ray per frame from the interpolated camera. It resolves blocks first, then
hands the same origin, direction and block-hit distance to `Sim::update_entity_target`
(`crates/lodestone-shell/src/sim/step.rs`), which walks every entity carrying `Position`, `EntityKind` and
`MinecraftEntityId` and keeps the nearest one whose hitbox the ray crosses. The winner's *server* entity id
lands in the `EntityRayTarget` resource (`crates/lodestone-shell/src/interact.rs`), which
`Sim::begin_attack_live`, `Sim::use_item_live` and `Sim::pick_block_or_entity` all read.

Four filters narrow the candidate set, in this order:

1. A cheap per-axis distance pre-filter against the search radius.
2. `interact::entity_type_can_be_picked` — vanilla's `CAN_BE_PICKED`, described below.
3. `VersionData::entity_facts`, which supplies the base hitbox. A type the census cannot size is dropped
   rather than approximated, so an unknown or plugin-namespaced type is never a candidate.
4. The exact ray-versus-AABB test, capped at `ENTITY_REACH` (3.0, vanilla's
   `DEFAULT_ENTITY_INTERACTION_RANGE`) and further capped by the block hit's own entry distance, so an
   entity behind a wall cannot be picked through it.

The local player is never a candidate: `lodestone_ecs::ingest`'s spawn and login folds never give the local
player's entity a `Position`/`EntityKind` pair, so the query structurally cannot return it — the property
vanilla gets by excluding `this` explicitly in `clip()`.

### The pick predicate, and the kick it prevents

Filter 2 exists because of a live defect: punching a mob and continuing to swing after it died got the
session disconnected with *"Attempting to attack an invalid entity"*
(`multiplayer.disconnect.invalid_entity_attacked`).

`ServerGamePacketListenerImpl.handleAttack` looks the packet's target id up in the level. A **miss is
silently ignored** — attacking an already-removed entity is a no-op, not an error. What it *disconnects*
for is a target that resolves and is an `ItemEntity`, an `ExperienceOrb`, the player themselves, or an
`AbstractArrow` whose `isAttackable()` is false. Killing a mob spawns its drops and its experience orbs
inside the hitbox the mob just vacated, so the next click landed on one of them.

Vanilla never sends that packet because its ray never picks those entities: `Entity.isPickable()` is
`false`, and neither `ItemEntity` nor `ExperienceOrb` overrides it. Lodestone's ray had no equivalent
filter at all.

`entity_type_can_be_picked` is a reduction over the ten classes that declare `isPickable()` in the 26.2
tree, derived by walking each entity type's implementation class (the `impl` column of
`crates/lodestone-data/tests/support/entity_census_jvm.txt`) up to its nearest declaration:

| declaring class | body | how the predicate answers it |
|---|---|---|
| `LivingEntity` | `!isRemoved()` | `lodestone_data::entity_census::is_living` — 90 types |
| `AbstractBoat`, `AbstractMinecart`, `FallingBlockEntity`, `PrimedTnt` | `!isRemoved()` | `NON_LIVING_PICKABLE_PATHS` |
| `BlockAttachedEntity`, `EndCrystal`, `Interaction`, `ShulkerBullet` | `true` | `NON_LIVING_PICKABLE_PATHS` |
| `Projectile` | `is(EntityTypeTags.REDIRECTABLE_PROJECTILE)` | `REDIRECTABLE_PROJECTILE_PATHS` |
| `AbstractArrow` | `super.isPickable() && !isInGround()` | always false — see below |
| `Player` | `!isSpectator() && super.isPickable()` | treated as living; see the gap below |
| `ArmorStand` | `super.isPickable() && !isMarker()` | treated as living; see the gap below |
| `EnderDragon` | `false` | an explicit exclusion, checked ahead of the living column |
| `Entity` | `false` | the default — `item`, `experience_orb`, `area_effect_cloud`, `evoker_fangs`, `eye_of_ender`, `lightning_bolt`, `marker`, `ominous_item_spawner`, the three `Display` variants |

Arrows are the non-obvious row. `AbstractArrow.isPickable()` delegates to `Projectile`'s tag test, and no
arrow type is a member of `minecraft:redirectable_projectile` (the tag's data file lists `fireball`,
`wind_charge` and `breeze_wind_charge` and nothing else) — so `arrow`, `spectral_arrow` and `trident` are
never pickable, and the `isInGround` state never gets a chance to matter. That also means the server's
`AbstractArrow && !isAttackable()` rejection is unreachable from a correct client.

The result is 131 pickable types of the census's 159, which
`the_pick_predicate_matches_the_vanilla_entity_census` asserts as a count so a version bump that moves a
type between the two buckets fails loudly.

## How to change it

* **Adding a filter** goes in `Sim::update_entity_target`, ahead of the `entity_facts` lookup so an
  excluded type never pays for a hitbox resolution.
* **A version bump** regenerates `lodestone-data`'s entity census. Re-derive the declaring class for any
  new type (walk its `impl` class's `extends` chain in `.cache/mc/<version>/client-src` to the nearest
  `isPickable()` declaration) and put it in the living column or one of the two explicit lists. The count
  assertion will name the drift.
* **Do not turn the two lists into a denylist.** Vanilla's own default is `false`, and default-deny is what
  keeps a type nobody remembered from getting the session kicked. It costs nothing today, because filter 3
  already drops any type the same census cannot size.

### Known gaps, deliberately not modelled

`Player.isPickable()` also requires `!isSpectator()` and `ArmorStand.isPickable()` also requires
`!isMarker()`. Both are per-*instance* state this client does not track for remote entities, and neither is
in the server's rejection list — attacking a spectator or a marker stand is a silent server-side no-op, not
a disconnect — so both types are reported as pickable rather than approximated. Same shape as
`EntityFacts::pushes_players` exposing a type-level maximum and leaving state gates to the consumer.

`EnderDragonPart` is pickable in vanilla and this client does not model dragon parts, so the dragon is
wholly unpickable here. That matches vanilla's behaviour for the dragon's own body entity, which also
returns `false`.

`EntityRayTarget` is a cached id recomputed once per frame, so there is a sub-frame window in which it can
name an entity a just-arrived `REMOVE_ENTITIES` has despawned. That is deliberately not guarded: per
`handleAttack` above, a target id the server cannot resolve is ignored, so the window is inert.

## Configuration

None. `ENTITY_REACH` (3.0) and `REACH` (4.5) are constants in `crates/lodestone-shell/src/sim`, matching
vanilla's `DEFAULT_ENTITY_INTERACTION_RANGE` and `DEFAULT_BLOCK_INTERACTION_RANGE`.

## Dependencies

* `lodestone-data` — `entity_types::entity_type_id_parts` for the name→id lookup and
  `entity_census::is_living` for the living column.
* `lodestone-ecs` — the `Position`/`EntityKind`/`MinecraftEntityId` components the query reads, and
  `VersionData::entity_facts` for hitbox dimensions.
* `crates/lodestone-shell/src/raycast.rs` — the block ray whose entry distance caps the entity search.
