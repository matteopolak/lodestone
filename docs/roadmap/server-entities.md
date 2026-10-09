# Server-side entities, AI and gameplay mechanics — roadmap

## What it is

The capability dependencies and completion evidence for server simulation of mobs, spawning, living-entity behaviour, item and economy mechanics, and damage and health. Chunk lifecycle, persistence, blocks, redstone, world state, protocol coverage, client rendering, commands, plugins and benchmarks are separate tracks ([server simulation](./server-simulation.md)).

## How it works

`lodestone-entity` provides goal AI, A* pathfinding with malus-weighted `PathType`, a memory/sensor/behaviour architecture, attributes with a three-stage modifier fold, damage reduction, projectile integration, explosion exposure and item-entity lifecycle. `lodestone-server::mobs::MobSim` consumes the goal AI and pathfinder over real terrain (`crates/lodestone-entity/tests/live_navigation.rs` is the live evidence). The main risk is an island: green local tests with no production consumer, so every phase names its wiring gate and needs a connected client or independent oracle.

### Phase 0: connect foundations

| capability | state and completion criterion |
|---|---|
| Terrain classification | `ChunkWorld::base_path_type` reads the 32,366-state path-type census (`lodestone-data::path_types`) by per-state classification, not solid/air; `collision_top` reads real boxes. Coarse clearance sweeps still use `ChunkColumn::is_solid` (per-shape sweeps are separate work). |
| Species registry | Natural spawning, caps and despawn run from `tick::run_tick_loop`; residual: `MobSim::spawn` needs a species registry for attributes, shape and goal sets instead of one hard-coded composition. |
| Damage | Shipped through `SimMob::apply_damage` (hurt cooldown, ordered reductions). Residual: uncommon source attribution, rule-gated edge cases, loot fidelity. |
| Brain AI | Goal AI is production-ticked; compose memory/sensor/behaviour with server entities and a real roster consumer. |
| Projectiles | Shipped; impacts feed entity and block effects via `MobSim::resolve_projectile_impacts`. Residual: per-family launch, collision, on-hit fidelity. |
| Explosions | Shipped; ray-sampled exposure (`ChunkWorld` implements `explosion::RayView`, `MobSim::explode`) uses the damage pipeline. Residual: block-destruction policy, encounter-specific effects. |
| Item entities | Shipped. Residual: pickup/merge edge cases, remote-visibility coverage. |
| Client visibility | Shipped (`MobSim::snapshots()` from the tick loop). Residual: multi-client replication assertions per new mechanic. |

Evidence: `melee_attack_reduces_target_health_and_a_lethal_hit_removes_the_mob` and `two_attackers_hitting_the_same_tick_only_land_one_full_hit` (cooldown gate); explosion tests prove falloff, point-blank lethality and wall shielding against an unshielded control; the path census tests prove a lava detour the old solid/air model would cross. Spawner blocks, patrols and wandering traders run from the tick loop under their own game rules (remaining fidelity is species selection and placement/roster rules); raids remain a separate encounter-state and client-reach problem.

### Later phases

1. **Spawning:** after the species registry and terrain classification, complete `SpawnRule`/`SpawnEnvironment` fidelity, regional difficulty and species placement. Spawn eggs already go through `spawn_species`; the natural cap/despawn, spawner, patrol and trader drivers stay the only paths. Done means a live entity obeying placement, cap, despawn and visibility.
2. **Mob AI roster:** split by observable behaviour (hostile melee/ranged, passive/herd and tameable, brain-driven passives incl. villager and piglin, Nether/aquatic specialists and neutral/aggro). Each family needs species composition (and the brain integration for memory/sensor families), a real server consumer, and a test distinguishing its goal or memory transition from idle.
3. **Living-entity behaviour:** breeding, taming, leashing and aging are server-ticked and client ingest preserves tame state and leash links. Residual: ownership, holder-change, offspring and cross-client cases; golem construction (`try_construct_golem`) pattern fidelity; sheep grazing extends the world random-tick loop, never a local loop in `lodestone-entity` or `lodestone-server`.
4. **Villagers and trading:** professions/POI, gossip, trade generation/refresh, reputation and curing are server-side with wire reach; remaining gate is brain-driven integration across transitions and multi-client replication. Golem construction and professions share one POI query.
5. **Items and mechanics:** random ticks, grazing, container-click authority, hopper, furnace, dispenser and food/hunger ticking ship. Remaining: crop and bone-meal rules, composting, brewing and enchanting outcomes, station buttons, experience and fishing, potion/status integration, per-family projectile launch, armour/combat edge cases feeding `damage.rs`. Keep the raw projectile integrator distinct from per-family launch, and visual feel distinct from damage arithmetic; a mechanic is complete only when its state change reaches a connected client or persisted state.
6. **Damage and health:** damage types, `damage.rs::DamageFlags`, hurt cooldown, explosions, fall, burning, drowning, lightning, periodic effects and damage/death sounds are wired. Residual: source attribution, rule-gated edges, loot fidelity. Use independent expected values or live comparisons, not agreement of two self-authored implementations.
7. **Boss fights and structures:** dragon and wither fights tick and publish boss bars. Residual: authoritative respawn and crystal sequencing, block-write consequences, multi-client fight-state reach, remaining species/AI dependencies. Golem and boss summons share one block-pattern matcher.

```
foundation wiring ──► spawning ──► roster ──┬─► living entities
       │                                    ├─► villagers and trading
       ├─► damage and health                 ├─► items and mechanics
       └────────────────────────────────────► bosses
```

The species registry gates the roster; damage, projectiles and explosions gate hostile, ranged and boss behaviour; terrain classification and brain integration progress independently but must be consumed before dependent claims.

### Cross-cutting notes

- Sheep grazing and crop growth need the shared random-tick loop (world track). Reuse the POI query, block-pattern matcher and anger-timer state machine across consumers.
- A self-authored JVM oracle validates only the behaviour chosen for the model; prefer captured server output or live comparison.
- A newly summoned entity is not selector-visible until the next tick (poll). `Invulnerable:1b` prevents targeting; `NoAI:1b` gives a stationary lure; `tick step N` does not advance entity physics, use `tick sprint N`.

## How to change it

Add a capability to the phase owning its production consumer, name dependencies by feature, record an observable completion gate, and move durable provenance and measured constants into the subsystem doc once they are shared architecture.

## Dependencies

`lodestone-entity`, `lodestone-server`, `lodestone-data` (per-state path and collision census, no protocol dependency), the world/block-tick track, and a live server for integration checks.
