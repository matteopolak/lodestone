# Server-side entities, AI and gameplay mechanics — roadmap

## What this is

This roadmap decomposes the server simulation for mobs, spawning, living-entity
behaviour, the item and economy mechanics, and damage and health. Chunk lifecycle,
world persistence, block simulation, redstone, world state, protocol coverage, client
rendering and prediction, commands, plugins, and benchmarks are separate tracks.

The tracker records ownership. This document records capability dependencies and the
evidence required before claiming a mechanic is complete.

## Existing foundations and the first rule

`lodestone-entity` already provides goal AI, A* pathfinding with malus-weighted
`PathType`, a memory/sensor/behaviour AI architecture, attributes with a three-stage
modifier fold, damage reduction, projectile integration, explosion exposure and damage,
and item-entity lifecycle rules. `lodestone-server::mobs::MobSim` consumes the goal
AI and pathfinder over real terrain; `crates/lodestone-entity/tests/live_navigation.rs`
is the live-server evidence.

The main risk is an island: a subsystem can have green local tests but no production
consumer. Every phase therefore names its wiring gate and requires a connected client
or independent oracle where applicable.

## Phase 0 — Connect foundational simulation paths

| capability | current state and completion criterion |
|---|---|
| Terrain classification | `ChunkWorld` reads the 32,366-state JVM-dumped path-type census from `lodestone-data::path_types`. A terrain path must use per-state classification, not solid/air alone. |
| Species registry | Natural spawning, caps, and despawn are driven from `tick::run_tick_loop`; their residual is roster fidelity: `MobSim::spawn` still needs a species registry for attributes, shape, and goal sets instead of a single hard-coded composition. |
| Damage | **Shipped.** Real hits resolve through `SimMob::apply_damage`, including hurt cooldown and ordered reductions. Residual: uncommon source attribution, rule-gated edge cases, and loot/drop-table fidelity. |
| Brain AI | Goal AI is production-ticked. Compose the memory/sensor/behaviour architecture with server entities and give it a real roster consumer. |
| Projectiles | **Shipped.** Tick-driven impacts feed entity and block effects. Residual: per-family launch, collision, and on-hit fidelity. |
| Explosions | **Shipped.** Ray-sampled exposure uses the common damage pipeline. Residual: block-destruction policy and encounter-specific effects. |
| Item entities | **Shipped.** Server ticks own lifecycle and client snapshots. Residual: pickup/merge edge cases and remote-visibility coverage. |
| Client visibility | **Shipped.** `MobSim::snapshots()` is published from the tick loop. Residual: multi-client replication assertions for each newly added mechanic. |

### Current integration evidence

`SimMob` carries health, `Defenses`, and `HurtCooldown` seeded from per-species
attribute data. `MobSim::tick` resolves a connected attack through
`HurtCooldown::on_hurt` and then `apply_reductions`; lethal health removes the mob.
`melee_attack_reduces_target_health_and_a_lethal_hit_removes_the_mob` proves a staged
lethal hit affects only its target, while
`two_attackers_hitting_the_same_tick_only_land_one_full_hit` proves the cooldown gate.

`ChunkWorld` implements `explosion::RayView`, and `MobSim::explode` samples exposure
per mob before using the same damage path. The explosion tests prove distance falloff,
point-blank lethality, and that a wall shields a target at the same distance as an
unshielded control.

Projectile impacts, explosions, item-entity ticking, and client snapshots are production-
wired. Projectile impacts feed block-target effects through `MobSim::resolve_projectile_impacts`;
the residual is family-specific collision, launch, and effect fidelity. Item lifecycle and
client snapshots now have a consumer; retain only pickup/merge edge cases and remote-visibility
coverage as residual gates.

`ChunkWorld::base_path_type` resolves canonical block-state strings through the global
block-state index and `lodestone_data::path_types::path_type`. `collision_top` reads
real collision boxes (empty shape is `0.0`, otherwise the maximum Y). The path census
tests prove a lava detour that the old solid/air model would cross and pin slab, fence,
water, and air collision tops. Coarse clearance sweeps still use `ChunkColumn::is_solid`;
per-shape sweeps are separate work.

Natural spawning and despawn run from the production tick loop, including cap accounting.
Spawner blocks, patrols, and wandering-trader cycles also run from that loop and obey their
separate game rules. Their remaining fidelity is species selection and full placement/roster
rules, not driver wiring. Raids remain a separate encounter-state and client-reach problem.

## Phase 1 — Spawning

After the species registry and terrain classification, complete `SpawnRule` and
`SpawnEnvironment` fidelity, regional difficulty, and species-specific placement. Spawn eggs
already materialize entities through `spawn_species`. The existing natural cap/despawn, spawner, patrol, and trader drivers
must remain the only paths; completion is a live spawned entity that obeys placement, cap,
despawn, and visibility rules.

## Phase 2 — Mob AI roster

Split roster work by observable behaviour rather than by one task per species:

- Hostile melee and ranged combat.
- Passive/herd and tameable companions.
- Brain-driven passives, villager and piglin behaviours.
- Nether/aquatic specialists and neutral/aggro state machines.

Every family depends on species composition; memory/sensor/behaviour families also need
the Phase-0 brain integration. A completed family needs a real server consumer and a
test that distinguishes its intended goal or memory transition from idle behaviour.

## Phase 3 — Living entity behaviour

Breeding, taming, leashing, and aging are server-ticked, while client ingest and extraction
preserve tame state and leash links, including mobs which enter view already leashed. Residual
work is the uncommon ownership, holder-change, offspring, and cross-client replication cases.
Golem construction is production-wired through `try_construct_golem`; residual work is
pattern fidelity and connected-client replication. Raids need encounter state and client
reach. Sheep grazing is production-ticked through the world random-tick loop; extend
its behaviour there, not with a local loop in `lodestone-entity` or `lodestone-server`.

## Phase 4 — Villagers and trading

Villager professions/POI, gossip, trade generation/refresh, reputation, and curing are
server-side capabilities with wire reach. The remaining gate is complete brain-driven
integration across those transitions and multi-client trade/state replication. Golem
construction and professions share the same POI query rather than maintaining separate counts.

## Phase 5 — Items and mechanics

The tick loop already ships random ticks and grazing, container-click authority and sync,
hopper transfer, furnace ticking, dispenser spawn/placement paths, food/hunger ticking, and
the relevant menu primitives. Remaining work is concrete fidelity/reach: crop and bone-meal
rules, composting, brewing and enchanting outcomes, station button paths, experience and
fishing loops, potion/status-effect integration, per-family projectile launch/effects, and
armour/combat edge cases feeding `damage.rs`.

Keep the raw projectile integrator distinct from per-family launch semantics, and keep
client visual feel distinct from server damage arithmetic. A mechanic is complete only
when its state change reaches a connected client or a persisted server state, not merely
when it decodes or has a local helper.

## Phase 6 — Damage and health

Damage types and `damage.rs::DamageFlags`, hurt cooldown, explosions, fall, burning,
drowning, lightning, periodic effects, and damage/death sound reach are production-wired.
Residual work is uncommon source attribution, rule-gated edge cases, and loot/drop-table
fidelity rather than another generic damage path.
Use independent expected values or live-server comparisons; agreement between two
self-authored implementations is not evidence of parity.

## Phase 7 — Boss fights and structures

Dragon and wither fights are ticked and publish boss bars; their modules cover phase/combat
arithmetic, projectiles, effects, and summon logic. Residual work is encounter-level fidelity:
authoritative respawn and crystal sequencing, block-write consequences where applicable,
multi-client fight-state reach, and the remaining species/AI dependencies. Golem and boss
summons should share one block-pattern matcher.

## Dependency graph

```
foundation wiring ──► spawning ──► roster ──┬─► living entities
       │                                    ├─► villagers and trading
       ├─► damage and health                 ├─► items and mechanics
       └────────────────────────────────────► bosses
```

The species registry gates the roster. Damage, projectiles, and explosions gate hostile,
ranged, and boss behaviour. Terrain classification and brain integration can progress
independently, but both must be consumed before their dependent gameplay claims are made.

## Cross-cutting validation notes

- Sheep grazing and crop growth require the shared random-tick loop. Keep that dependency
  in the world track.
- Reuse the POI query, block-pattern matcher, and anger-timer state machine across their
  respective consumers rather than creating behaviour-specific copies.
- A self-authored JVM oracle validates only the behaviour chosen for the model. Prefer
  captured server output or a live-server comparison where available.
- A newly summoned entity is not selector-visible until the next tick; poll rather than
  assert immediately. `Invulnerable:1b` prevents targeting, while `NoAI:1b` supplies a
  stationary lure. `tick step N` does not advance entity physics; use `tick sprint N`.

## How to change this roadmap

Add a capability to the phase that owns its production consumer, name its dependencies
by feature, and record an observable completion gate. Move durable data provenance and
measured constants to the relevant subsystem documentation when they become shared
architecture rather than roadmap sequencing.

## Configuration and dependencies

This track relies on `lodestone-entity`, `lodestone-server`, `lodestone-data`, the
world/block-tick track, and a live server for integration checks. The data layer supplies
the per-state path and collision census without introducing a protocol dependency into
the server simulation.
