# Mob AI roster: species behavior and production wiring

## What it is

How species behavior is assembled from `lodestone-entity` goals and brains and made observable through `lodestone-server`. A roster entry counts only if it is selected at spawn, ticked by production simulation, and its visible state reaches a client.

## How it works

```text
spawn source -> MobSim::spawn_species -> roster selection -> goal or brain tick
    -> MobController action -> simulation state -> entity update or clientbound packet
```

- `lodestone_entity::ai::roster::goals_for` returns a priority-ordered goal list per species; an unknown species returns an empty list.
- Brain-driven species are selected by `MobSim::spawn_species` and ticked through `BrainGoal`.
- `MobController` is the seam between behavior and simulation. Production must feed every perception value: water, lava, idle time, nearest player, recent attacker, temptation, avoidance threat, panic, and breeding/parent candidates. A goal gated on an unfed value is inert even when its unit tests pass against a scripted controller.
- Natural spawning, spawn eggs and spawner blocks all go through `spawn_species`. A zombie-only or roster-less constructor is not a substitute.
- Goal and target selection have independent priority namespaces. Runtime equipment or state changes need a real remove/replace operation, not a disabled flag.

### Current production ledger

| surface | state |
|---|---|
| Goal rosters, brain driver | selected from `spawn_species` and ticked in production |
| Natural spawning | uses the same species path; placement rules stay in `natural_spawn::SpawnRule` |
| Spawner blocks | tick via `spawn_species`; not modelled: species placement, custom spawn data and equipment, fixed positions, entity collision, passengers |
| Camel | rider jump reaches dash state and metadata; ridden physics is client-authoritative, so no server dash impulse or full saddle/on-ground gate |
| Sniffer | seeks, walks, digs, rises and drops loot; omitted: cosmetic scent/happy states, path-aware targets, particles, sounds, drop offset, full mid-dig cancellation |

### Species families

| family | seam |
|---|---|
| hostile melee | goal roster and combat action |
| passive herd | perception feeds and generated food data |
| ranged | goal selector and projectile path |
| neutral | shared anger state and entity updates |
| specialists | dedicated state machine and packet metadata |
| brain-driven | brain driver and population source |

## How to change it

- Add a primitive only if more than one roster uses it. Otherwise put species policy in its own `ai/roster/` or `brain/roster/` module and register it through `goals_for`. Keep `MobSim` patches to perception feeds, action drains and the spawn path.
- Test across the full chain with a real simulated mob and a negative control that removes the perception feed or registration. For population effects, assert the spawned child or entity count.
- Search for the production assignment of a capability, not just its trait method. Breeding needs a partner feed and a child-creating consumer; ranged attacks need projectile creation, collision, damage and wire visibility.
- Take constants and species data from the oracle or generated data (`LODESTONE_REGEN=1` regenerates). Never copy a sibling roster or round-trip our own encoder.
- For metadata changes run `EntityDataIndexOracle.java` under the version family's `oracle-java/`; never hand-count indices.
- In live gates, give a new entity one server tick before testing selector visibility, and advance physics with the operation that actually runs it.

## Dependencies

`lodestone-entity` (goals, brains, spawn definitions), `lodestone-server` (`MobSim`, tick loop, projectiles, entity packets), `lodestone-data` entity classification, and [`server-ecs-migration.md`](server-ecs-migration.md).
