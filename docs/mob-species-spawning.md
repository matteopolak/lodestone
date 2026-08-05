# Species-aware mob spawning (issue #205)

## What it is

`MobSim::spawn_species` (`crates/lodestone-server/src/mobs.rs`), a spawn entry
point that resolves a mob's body, combat stats, and baseline goal set from its
real vanilla species instead of the universal `minecraft:zombie` placeholder
`MobSim::spawn` used to hand every caller. Before this, `SimMob::entity_type`
defaulted to `minecraft:zombie` unconditionally and a freshly spawned mob's
`GoalSelector` started empty, so two different species were behaviourally and
nominally identical — a `minecraft:pig` and a `minecraft:zombie` were, on the
wire and in the sim, the same mob.

## How it works

```text
MobSim::spawn_species(entity_type, pos) -> &mut SimMob
  attrs   = default_attributes(entity_type)              // lodestone_entity::attribute
  shape   = species_shape(entity_type, attrs)             // dimension census + SCALE/STEP_HEIGHT fold
  speed   = attrs["movement_speed"]                        // blocks/tick, direct read
  budget  = floor(attrs["follow_range"] * 16)              // A* open-set budget
  hostile = is_hostile_species(entity_type)                // coarse Monster/Animal split

  spawn_with_type(pos, shape, speed, budget, entity_type)  // shared with `spawn`
  -> set_category(Monster | Creature), set_persistent(!hostile)
  -> add_goal(RandomStrollGoal) + add_goal(RandomLookAroundGoal)
  -> if hostile: add_goal(MeleeAttackGoal)
```

`species_shape` folds the real 26.2 dimension census
(`lodestone_data::entity_dimensions::base_dimensions`, keyed by
`lodestone_data::entity_types::entity_type_id_parts`) with the type's
`SCALE`/`STEP_HEIGHT` attributes — the same maths
`crate::resolve_mob_shape` uses for a version-aware caller, duplicated here
(not called) because `MobSim` already reads `lodestone_data` directly for its
path/collision census and has no `VersionAdapter` to thread through. Falls
back to `MobShape::land(0.6, 1.95)` for a species the census does not know by
name.

`combat_defaults` (already species-aware, pre-existing) supplies
`max_health`/`attack_damage`/`armor` from `default_attributes`'s hand-verified
`type_spec` table — zombie's `20`/`3.0`/`2.0` vs. pig's `10`/generic/`0`, for
example.

`is_hostile_species` is a coarse classifier deciding only the spawn
`MobCategory` and despawn persistence — goal sets have belonged to
`lodestone_entity::ai::roster` since the roster landed.

**Updated by #457.** It listed eight species for a long time after the roster
grew to twenty-seven, so `drowned`, `cave_spider`, `zombie_villager`,
`parched`, `guardian`, `elder_guardian`, `ghast`, `blaze`, `enderman` and
`zombified_piglin` all got the wrong category. It now lists seventeen, each
read from that species' own `EntityType.Builder.of(X::new, MobCategory.…)`
registration in `EntityTypes.java` — **not** inferred from the `type_spec`
attribute template, which is a different question with a different answer:

- a **ghast** is `MobCategory.MONSTER` while its attribute builder is a bare
  `Mob.createMobAttributes()` with no `attack_damage` at all;
- a **snow golem** is `MobCategory.MISC`, which this boolean cannot represent
  — it lands as `Creature`, the safe direction, and that gap is #221's.

It is still a name list, and a name list still ages. What keeps it honest is
`every_rostered_species_has_a_decided_category`, which drives
`roster::*::SPECIES` through a jar-cited table so a new species with no decided
category fails rather than silently defaulting.

`MobSim::spawn` (the pre-existing, zombie-hardcoded entry point) is unchanged
for its own callers — both now share a private `spawn_with_type` helper that
takes the entity type as a parameter, so nothing about `spawn`'s existing
behaviour moved.

## How to change it, and the gotchas

- **This is the registry infrastructure, not a full roster.** Per-species
  goal sets beyond "hostile gets melee, passive doesn't" (ranged attacks,
  breeding preferences, avoid-water, …) are separate, larger issues that this
  is a hard prerequisite for. Extend `is_hostile_species` or add a proper
  per-species goal table when picking one of those up — don't hand-roll
  another zombie-shaped default.
- **`NearestAttackableTargetGoal` is deliberately not added here.** Its
  `find_nearest_target` (`NavigatingMob`'s `MobController` impl) is a stub
  that just returns whatever `attack_target` is already set — there is no
  population search behind it (mirroring the breeding/parent candidate seam
  `MobSim::tick`'s own doc discusses). Adding the goal without a real search
  driving it would be cosmetic. A hostile mob's `MeleeAttackGoal` still fires
  correctly once *something* — a caller, or a future population search —
  calls `SimMob::set_attack_target`/`set_attack_target_id`.
- **`movement_speed` is read directly as blocks/tick**, matching the
  convention `run_spawn_cycle`'s candidates and the old hardcoded `0.23`
  demo-seeding literal already used — not vanilla's real UI-speed-to-motion
  conversion. Revisit if a species issue needs exact vanilla speed parity.
- **Unknown species are not an error.** `default_attributes` returning `None`
  falls back to `AttributeMap::new()` (generic defaults: 20 health, 2 attack,
  no armor) and `species_shape` falls back to the zombie-sized box — the same
  "explicit fallback, never a silent guess" contract `resolve_mob_shape` uses.

## Configuration

No feature flags or constants beyond the classification list in
`is_hostile_species` and the melee reach (`2.0` blocks, matching this file's
existing hand-written tests) hardcoded in `spawn_species`.

## Dependencies

- `lodestone_entity::attribute::default_attributes` (combat stats, `type_spec`).
- `lodestone_data::{entity_dimensions, entity_types}` (26.2 dimension census).
- `lodestone_entity::ai::goals::{RandomStrollGoal, RandomLookAroundGoal, MeleeAttackGoal}`.
- See [`docs/live-mob-sim.md`](./live-mob-sim.md) for the tick loop that
  ultimately drives whatever `spawn_species` produces once wired into a real
  spawn source.
