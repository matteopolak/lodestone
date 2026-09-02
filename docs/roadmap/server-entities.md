# Server-side entities, AI and gameplay mechanics — roadmap

Everything alive or mechanical on a Lodestone *server*: mobs and their AI, spawning,
living-entity behaviour (breeding, taming, villagers, golems, raids, bosses), the
item/economy mechanics (farming, furnaces, brewing, enchanting, XP, fishing, hunger),
and damage/health. All 57 issues below are filed as sub-issues of epic
[#5](https://github.com/matteopolak/lodestone/issues/5) (Tier 4 — the game simulation).

Not covered here (owned by other tracks): chunk generation/persistence, redstone,
block ticks and fluid flow, world state, the server tick loop and plumbing, client
rendering, client-side physics/prediction, the plugin framework, benchmarks, protocol
packet coverage, and command execution ([#48](https://github.com/matteopolak/lodestone/issues/48),
already filed).

## The one thing worth knowing before reading the phases

**This domain is not greenfield.** `lodestone-entity` already ships a real,
scheduler-faithful goal AI (`ai::GoalSelector` + a representative goal set), a
faithful A\* pathfinder (`pathfinding::{PathFinder, PathNavigator}`) with its own
malus-weighted `PathType`, a second complete AI architecture (`brain::*`, Vanilla's
memory/sensor/behaviour system), an attribute system with vanilla's three-stage
modifier fold, a damage-reduction pipeline, ballistic projectile integration, an
explosion exposure/damage model, and item-entity lifecycle rules. `lodestone-server`
has a real consumer — `mobs::MobSim` — that ticks goal AI and the pathfinder over
real terrain, proven against a live 26.2 server in
`crates/lodestone-entity/tests/live_navigation.rs`.

**Most of what is missing is *wiring*, not invention.** The dominant defect class this
repo tracks is the island: a subsystem individually built and tested that reaches zero
consumers. Phase 0 below is entirely islands — `damage.rs`, `projectile.rs`,
`explosion.rs`, the `brain` system, and the dumped `path_types.rs` census all have
**zero consumers outside their own crate**, confirmed by grep, even though the goal
scheduler and pathfinder they should plug into are already ticking in production. Filing
"implement pathfinding" or "implement the damage pipeline" as new work would have been
wrong — the actual gap is narrower and cheaper to close than a roadmap author's first
guess, and every phase after 0 depends on these wires being connected first.

Everything in Phases 1 onward (spawning data, per-species AI, villagers, farming,
processing blocks, damage types, bosses) is **genuinely absent** — no dead code to wire,
real design and implementation work.

## Phase 0 — Close the islands (do first; nothing else has a consumer without these)

| # | Issue | What it closes |
|---|---|---|
| 1 | [#204](https://github.com/matteopolak/lodestone/issues/204) | **Closed.** `ChunkWorld` used to classify every block solid/air only; it now reads the 32,366-state JVM-dumped path-type census (`lodestone-data::path_types`) for real. See "Phase 0 progress" below. |
| 2 | [#205](https://github.com/matteopolak/lodestone/issues/205) | `MobSim::spawn` hardcodes `entity_type = "minecraft:zombie"` and an empty `GoalSelector` — there is no species→(attributes, shape, goal-set) registry. Hard prerequisite for every roster issue in Phase 2. |
| 3 | [#207](https://github.com/matteopolak/lodestone/issues/207) | `damage.rs`'s ordered reduction pipeline has zero consumers; `NavigatingMob::attack` just logs the call to a `Vec` for tests. No hit anywhere reduces a health value. |
| 4 | [#209](https://github.com/matteopolak/lodestone/issues/209) | The `brain` system (Vanilla's *other* AI architecture — villager, piglin, warden, and 17 more) has no `NavigatingMob`-equivalent composition and no consumer, despite being as architecturally complete as the goal system was before that composition existed. |
| 5 | [#211](https://github.com/matteopolak/lodestone/issues/211) | `projectile.rs`'s arrow/throwable integration is never ticked by anything. Blocks every ranged goal and both boss fights. |
| 6 | [#213](https://github.com/matteopolak/lodestone/issues/213) | `explosion.rs`'s ray-sampled exposure/damage model has never been triggered. Blocks creeper, TNT's entity-facing half, ghast fireballs, the dragon fight's crystal chain reactions. |
| 7 | [#215](https://github.com/matteopolak/lodestone/issues/215) | `item_entity.rs`'s despawn/pickup-delay/merge lifecycle is unconsumed; only the fall-dynamics half reaches the client renderer. |
| 8 | [#217](https://github.com/matteopolak/lodestone/issues/217) | `MobSim` never streams positions to a connected client (the module's own doc says so). The last mile that makes every other issue in this roadmap invisible to a player. |

### Phase 0 progress (sessions closing #204/#207/#213, partial #205)

**#207 closed.** `SimMob` now carries real health, `Defenses` and a `HurtCooldown`,
seeded from `lodestone_entity::attribute::default_attributes` (real per-type data —
a spawned zombie starts at the actual `max_health` 20 / `attack_damage` 3 / `armor` 2
from vanilla's own per-species attribute registration, not an invented number). `NavigatingMob::attack`
still only records the *intent* to strike (it has no entity identity, only a `Vec3`),
so `SimMob::attack_target_id` was added alongside the existing `Vec3` target and
`MobSim::tick` resolves each tick's connected attacks into a real
`SimMob::apply_damage` call — `HurtCooldown::on_hurt` then `apply_reductions`, in
that order, exactly as `damage.rs`'s own module doc specifies. A mob whose health
reaches zero is removed from the sim. See
`crates/lodestone-server/tests/mob_sim.rs`:
`melee_attack_reduces_target_health_and_a_lethal_hit_removes_the_mob` (a staged
lethal hit removes the target; an untargeted bystander is provably untouched — the
control) and `two_attackers_hitting_the_same_tick_only_land_one_full_hit` (the
i-frame gate control: without `HurtCooldown` wired in, two hits landing the same
tick would double the damage).

**#213 closed.** `ChunkWorld` now implements `explosion::RayView` (a coarse but real
quarter-block raymarch over its own `is_solid`), and `MobSim::explode` is the first
consumer of `seen_percent`/`entity_damage` anywhere in the tree — it samples real
exposure per mob and lands the result through the same `apply_damage` pipeline #207
built. See `explosion_damages_exposed_mobs_more_up_close_and_kills_at_ground_zero`
(distance falloff, point-blank kill) and
`explosion_exposure_is_ray_sampled_a_wall_fully_shields_a_mob` (the control: two
mobs at the *same distance*, one behind a wall takes zero damage, the other takes
real damage — proving the mechanism reads real terrain, not just distance).

**#205 partially closed.** The species→attributes/shape registry this issue is
mostly about is still absent (`MobSim::spawn` still hardcodes
`minecraft:zombie`) — that needs real spawn-rule/registry data this version-free
crate cannot originate, unchanged from the original filing. What *was* closed:
`MobSim::run_spawn_cycle` gave every naturally spawned mob an empty `GoalSelector`,
which meant a mob produced by natural spawning never moved or looked around —
`MobSim::tick` on it was a provable no-op even though the goal scheduler was
"wired". Spawned mobs now get a baseline `RandomStrollGoal` + `RandomLookAroundGoal`
so they actually do something; combat goals are still the caller's job (they need a
target, which the spawn cycle has no way to name yet).

**#204 closed.** The fix this issue wants (a `PathWorld` that reads real
per-block-state collision/path-type data instead of solid/air) used to be
blocked on a real structural problem: the 32,366-state census only existed
behind `crates/protocol/v770`, and `lodestone-server` correctly refuses a
dependency on any `protocol/*` crate (`lodestone-server/Cargo.toml` documents
"no version/protocol coupling" in its own dependency comment) — so wiring the
real census into `MobSim::ChunkWorld` meant either reversing that boundary or
having `lodestone-shell` supply a richer `PathWorld`, making the server's
fidelity depend on the client.

Issue #361 extracted `path_types` (and the other eighteen game-data censuses) out
of `crates/protocol/v770` into `lodestone-data`, a crate with no protocol
dependency of its own — see `docs/lodestone-data-crate.md`. `lodestone-data`'s
`path_types::PathTypes` already implements `lodestone_model::PathTypeRegistry`,
and `PathType` in `lodestone-model`/`lodestone-entity` already mirrors it
variant-for-variant, so the boundary problem was gone: `lodestone-server` could
add a plain data dependency without reversing anything.

That local patch is what this session did. `crates/lodestone-server/Cargo.toml`
now depends on `lodestone-data`. `ChunkWorld::base_path_type` (`mobs.rs`)
resolves each cell's canonical block-state string — `ChunkColumn` (`chunk.rs`)
already stored those in full, not just a solid/air bit, so no `ChunkColumn`
data-model change was needed — to a global block-state id (a new reverse
`String -> u32` index built once over `lodestone_data::block_states`, since
nothing needed a name-to-id lookup before this) and looks it up in
`lodestone_data::path_types::path_type`, translated into
`lodestone_entity::pathfinding::PathType` by a small 1:1 match
(`census_to_pathfinding_type`). `ChunkWorld::collision_top` reads
`lodestone_data::collision_shapes` the same way, taking the max Y of the
state's real collision boxes — matching exactly what vanilla asks in its own
floor-level query for pathfinding (empty shape → 0.0, otherwise the shape's own max Y) —
instead of the old
hardcoded `1.0`. `PathWorld::collides` (the coarse jump-clearance/diagonal-reach
sweep) is unchanged and still reads `ChunkColumn::is_solid`; widening it to real
per-shape sweeps was judged a separate, larger change outside this issue's
scope. See `crates/lodestone-server/tests/chunk_world_path_census.rs`:
`real_census_forces_a_lava_detour_the_old_solid_air_model_would_walk_straight_through`
(a lava band the old solid/air mapping reads as `Open` and walks straight
through; the real census reads it as `PathType::Lava`, malus `-1`, and the
search detours around the band's end — with the old mapping re-run over the
*same* terrain as the required control, proving it really does walk straight
through) and `collision_top_reads_the_real_per_state_shape_not_a_hardcoded_full_cell`
(pins the slab/fence/water/air collision tops directly).

## Phase 1 — Spawning

Depends on Phase 0 issues #2 (species registry) and #1 (real terrain classification for
placement). | [#221](https://github.com/matteopolak/lodestone/issues/221) `SpawnRule`/
`SpawnEnvironment` has zero implementers · [#222](https://github.com/matteopolak/lodestone/issues/222)
no natural spawn cycle drives the already-proven cap/despawn engine ·
[#223](https://github.com/matteopolak/lodestone/issues/223) regional difficulty is
entirely unmodeled · [#224](https://github.com/matteopolak/lodestone/issues/224) spawn
eggs and spawner blocks never materialize an entity.

## Phase 2 — Mob AI roster

Parent [#225](https://github.com/matteopolak/lodestone/issues/225), split by observable
behaviour family rather than one issue per species (~50 goal-driven + 20 brain-driven
mobs per the census in `brain/mod.rs`): hostile melee
([#226](https://github.com/matteopolak/lodestone/issues/226)), ranged — a wholly new
goal family, none exists today
([#227](https://github.com/matteopolak/lodestone/issues/227)), passive/herd
([#228](https://github.com/matteopolak/lodestone/issues/228)), tameable companions
([#229](https://github.com/matteopolak/lodestone/issues/229)), brain-driven passives
([#230](https://github.com/matteopolak/lodestone/issues/230)), villager/piglin brain
behaviours ([#231](https://github.com/matteopolak/lodestone/issues/231)), nether/aquatic
specialists ([#232](https://github.com/matteopolak/lodestone/issues/232)), neutral/aggro
([#233](https://github.com/matteopolak/lodestone/issues/233)). Every child depends on
Phase 0 #2 and, for the brain-based children, Phase 0 #4.

## Phase 3 — Living entity behaviour

Breeding ([#234](https://github.com/matteopolak/lodestone/issues/234)), taming
([#235](https://github.com/matteopolak/lodestone/issues/235)), leashing
([#236](https://github.com/matteopolak/lodestone/issues/236)), aging
([#237](https://github.com/matteopolak/lodestone/issues/237)), sheep grazing —
**blocked on the world/block-tick track**, `randomTickSpeed` is decode-only with zero
consumers today
([#238](https://github.com/matteopolak/lodestone/issues/238)), iron/snow golem
construction ([#239](https://github.com/matteopolak/lodestone/issues/239)), wandering
trader ([#240](https://github.com/matteopolak/lodestone/issues/240)), raids and
patrols ([#241](https://github.com/matteopolak/lodestone/issues/241)).

## Phase 4 — Villagers and trading

Parent [#242](https://github.com/matteopolak/lodestone/issues/242): professions/POI
([#243](https://github.com/matteopolak/lodestone/issues/243)), gossip
([#244](https://github.com/matteopolak/lodestone/issues/244)), trade generation/refresh
([#245](https://github.com/matteopolak/lodestone/issues/245)), reputation
([#246](https://github.com/matteopolak/lodestone/issues/246)), curing zombie villagers
([#247](https://github.com/matteopolak/lodestone/issues/247)). This is the economy
*data* half; #231 above is the behaviour tree that *consumes* it — land them together.

## Phase 5 — Items and mechanics

Crop growth/bone meal — **blocked on the world/block-tick track** for the trigger
([#248](https://github.com/matteopolak/lodestone/issues/248)), composters
([#249](https://github.com/matteopolak/lodestone/issues/249)), hoppers/container
transfer ([#250](https://github.com/matteopolak/lodestone/issues/250)), furnaces/smelting
([#251](https://github.com/matteopolak/lodestone/issues/251)), brewing
([#252](https://github.com/matteopolak/lodestone/issues/252)), enchanting
([#253](https://github.com/matteopolak/lodestone/issues/253)), anvil/grindstone
([#254](https://github.com/matteopolak/lodestone/issues/254)), smithing
([#255](https://github.com/matteopolak/lodestone/issues/255)), XP orbs and levels
([#256](https://github.com/matteopolak/lodestone/issues/256)), fishing
([#257](https://github.com/matteopolak/lodestone/issues/257)), food and hunger
([#258](https://github.com/matteopolak/lodestone/issues/258)), potions/status effects
server-side, beyond `lodestone-physics::effect`'s movement-only classifier
([#259](https://github.com/matteopolak/lodestone/issues/259)), projectile launch
parameters and on-hit effects per family — distinct from Phase 0 #5's raw integrator
wiring ([#260](https://github.com/matteopolak/lodestone/issues/260)), armour/combat
maths feeding `damage.rs` — distinct from #12's client-side visual feel
([#261](https://github.com/matteopolak/lodestone/issues/261)).

## Phase 6 — Damage and health

Damage types/sources registry — the table `damage.rs`'s `DamageFlags` seam has been
waiting on since it was written
([#263](https://github.com/matteopolak/lodestone/issues/263)), fall damage
([#265](https://github.com/matteopolak/lodestone/issues/265)), drowning — server side
of the countdown client issue #60 only displays
([#267](https://github.com/matteopolak/lodestone/issues/267)), fire/burning/lightning
([#269](https://github.com/matteopolak/lodestone/issues/269)), mob loot and drop
chances ([#272](https://github.com/matteopolak/lodestone/issues/272)).

## Phase 7 — Boss fights and structures

Parent [#274](https://github.com/matteopolak/lodestone/issues/274): ender dragon
([#276](https://github.com/matteopolak/lodestone/issues/276)), wither
([#278](https://github.com/matteopolak/lodestone/issues/278)). Layered on top of every
earlier phase — mob AI/brain composition, projectiles, explosions, damage.

## Dependency graph, coarse

```
Phase 0 (islands)  ──┬─► Phase 1 (spawning) ──► Phase 2 (roster) ──┬─► Phase 3 (living entity)
  #204 path types    │                                             ├─► Phase 4 (villagers)
  #205 species reg.  │                                             │
  #207 damage        ├─► Phase 6 (damage/health, needs #263 early) │
  #209 brain wiring  │                                             │
  #211 projectiles   ├─► Phase 5 (items/mechanics, mostly parallel)│
  #213 explosions    │                                             │
  #215 item lifecycle│                                             ▼
  #217 streaming     └───────────────────────────────► Phase 7 (bosses)
```

Within Phase 0, #205 (species registry) gates every child of Phase 2, and #207/#211/#213
(damage/projectiles/explosions) gate the ranged, hostile and boss-fight issues
specifically. #204 and #209 are independent of each other and of the rest of Phase 0.

## Cross-cutting notes for whoever picks these up

- **Two things are blocked on the world/block-tick track, not on this domain**:
  sheep grazing (#238) and crop growth (#248) both need a per-chunk random-tick loop
  that does not exist anywhere yet (`randomTickSpeed` decodes with zero consumers).
  Note the dependency; do not build a random-tick loop inside `lodestone-entity` or
  `lodestone-server` to unblock these locally.
- **Several issues share a query or state machine and say so explicitly** to avoid two
  ports of the same thing: the iron-golem POI count (#239) and villager professions
  (#243) share one POI query; the wither summon (#278) and golem construction (#239)
  share one block-pattern-match approach; the enderman/zombified-piglin/bee/wolf-pack
  issues (#233) share one anger-timer state machine.
- **A self-authored JVM oracle validates only the behaviour this project chose to
  model.** Several issues above (#207, #211, #213) call this out explicitly where a
  live-server RCON comparison is the only real check, not agreement between two ports
  sharing an author.
- **Live-server hazards already measured in this repo, cited where relevant above**: a
  freshly summoned entity is not selector-visible until the next tick (poll, never
  assert immediately); `Invulnerable:1b` makes an entity un-targetable, use `NoAI:1b`
  for a stationary lure instead; `tick step N` does not advance entity physics, only
  `tick sprint N` does.

## Corrections to the briefing this roadmap started from

The initial brief characterized `path_types.rs` as "real groundwork for pathfinding,"
implying the pathfinder itself did not yet exist. On inspection, `lodestone-entity`
already ships a complete, ticking goal-AI-plus-pathfinder composition
(`ai::NavigatingMob` over `pathfinding::{PathFinder, PathNavigator}`), consumed by a
real driver (`lodestone-server::mobs::MobSim`) and proven against a live server
(`live_navigation.rs`). The actual gap — captured as #204 — is narrower: the pathfinder
runs over a solid/air-only terrain view and never reads the per-state census
`path_types.rs` generates. The brief's "158 entity dimensions" and "entity census"
claims checked out as described; the goal/brain/damage/pathfinding architecture was
undersold.
