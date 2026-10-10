# Mob AI

## What it is

Server-side mob AI: the per-species goal roster and priority scheduler, brain-based target acquisition for villager-class mobs, perception (what a goal may know about the world), neutral-mob anger, ranged attacks, vertical motion, and the per-tick projectile and item drivers. All of it runs every tick inside `MobSim` (`crates/lodestone-server/src/mobs/mod.rs`), which also gets AI-driven mobs onto the wire.

## How it works

### Goal roster

A pure, world-free lookup from a species path (`"cow"`) to its `Goal`s at the reference's own priority numbers. Code is in `crates/lodestone-entity/src/ai/roster/`, one file per family (`hostile_melee`, `ranged`, `passive`, `neutral`, `specialist`); `goals_for` is called once by `MobSim::spawn_species`.

Each species resolves to a `&'static [Registration]` tagged `goal` (built), `missing` (no such goal exists) or `covered` (a sibling row already covers it). `Missing` rows keep their real priority so an absent capability is not mistaken for an untranscribed one. Speed arguments multiply the mob's movement speed; builders compute `ctx.speed * <jar factor>` so the multiplier stays checkable.

The reference has two independently numbered selectors (goals, targets); `SimMob` holds one `GoalSelector`. That is safe because priorities only compete within a contended `Flag`, and target goals are exactly those with flag set `{TARGET}`. `goals_for` emits every target row before every goal row, preserving tick order (a target acquired this tick is visible to movement the same tick). `GoalSelector::add` returns a `GoalId`; `remove(id, mob)` stops and drops a running goal, needed for runtime swaps (a skeleton's melee and bow goals) that a per-`Flag` `disable` cannot express.

### Navigation motion

`PathNavigator` keeps the effective speed of its active request and `NavigatingMob` uses it per step; species spawning supplies the attribute-to-ground conversion, so a roster multiplier changes motion without rebuilding the path. Brain walk-target speeds are modifiers multiplied by the body's ground rate. Rates stay `f64`. A downward waypoint stays active until the body clears the supporting cell horizontally, then gravity moves it; upward waypoints keep the auto-step and jump transitions.

### Brain-based target acquisition

`lodestone_entity::brain` adds `BrainMob::nearby_entities()` (host-fed from a coarse pre-filter box), `NearestHostileSensor` (cuts that to the true 8-block range and writes `NEAREST_HOSTILE`) and `Brain::add_activity_any_of` (an activity eligible when any of its `(memory, status)` pairs holds). `villager_brain()` registers `Activity::PANIC` ahead of `IDLE` on `[(HURT_BY, present), (NEAREST_HOSTILE, present)]`, because `Behavior` has no seam to reach into its own `Brain`. The flee target is a random land position, not directed away from the threat, and nothing on this seam checks line of sight.

### Perception (goal-based mobs)

`MobController` exposes eight perception methods that goals' `can_use` read; `NavigatingMob` is the only implementor and `MobSim::tick` the only feeder.

- `in_water`/`in_lava` derive from the borrowed `PathWorld`.
- `last_hurt_by` (100 ticks) and `is_panicking` (40) are independently decaying records set from `hurt`'s single event; damage with no living attacker panics but leaves `last_hurt_by` untouched.
- `no_action_time` is the sim's per-mob counter; `avoid_threat` reads the census plus `avoided_species`; `nearest_player`/`temptation` come from `MobSim::set_players`, fed by the `PlayerMoved` handler in `server/play_dispatch.rs`.

`MobSim::tick` ages `no_action_time` for every mob before `feed_perception` (a read-then-apply two pass, since one mob's decision depends on all others), before goals tick. Tempt foods are transcribed per species from the jar's item tags, not folklore lists.

**Block perception.** `PathWorld::block_cues(x, y, z) -> BlockCues` classifies one block into boolean predicates (the reference tests are simultaneous tag checks); `MobController::ate(EatenBlock)` carries the mutation intent back for the host to drain. It is a query, not a per-tick feed, because the sheep grazing goal consults a block about once per 500 ticks. Goals that walk to a target block (skeleton sun-flee, zombie turtle-egg attack, rabbit garden raid) need a host-computed candidate position plus destroy intents, which `block_cues` alone does not close. Every jar tick constant on grazing is halved in practice (a shared default throttles goals that do not override their update cadence): 1000/50/40/4 are 500/25/20/2 ticks.

### Neutral roster (anger)

Enderman, zombified piglin, bee and wolf share an anger duration uniform on [400, 780] ticks inclusive, stored as an absolute game-time deadline (`SimMob::anger` holds `{ end_time, target }`, so a paused or stepped loop cannot desync it). It starts with `note_hurt` in `MobSim::attack` and is cleared by `feed_perception` after the deadline.

Three species alert same-species mobs with no target inside a box (vertical half-extent 10; horizontal 35 for the piglin via follow range, 16 for wolf and bee). The piglin has a second, independent propagation: a census throttled to [80, 120] ticks, gated on line of sight, as a per-mob countdown in `MobSim::tick`.

The enderman stare test is `look.dot(dir) > 1.0 - 0.025 / dist`, so required precision rises with distance; the freeze-when-looked-at goal consumes it, the teleport-on-stare goal is not built. A stung bee survives the sting: anger clears at once and a per-tick probabilistic roll guarantees death within 1200 ticks.

Five primitives back this on `MobController`: anger deadline and target, gaze test (per-player view vector), instant teleport, self-damage (through `apply_damage`, i-frames included) and ownership (from `PlayerIdentity`).

### Ranged attacks

A goal never spawns an entity: it computes the aiming maths and calls `MobController::launch_projectile`; `NavigatingMob` accumulates launches and the host drains them once per tick into `ProjectileRegistry` via `MobSim::spawn_projectile`. Shapes: a 20-tick-draw bow attack, a no-draw interval ranged attack, and the blaze burst (three fireballs 6 ticks apart, 60-tick wind-up, 100-tick pause, melee under 2 blocks). Arrows, tridents, snowballs and potions launch at power 1.6 plus arc lift; small fireballs at 0.1 with no lift and accelerate in flight.

Skeleton-family species register through `hostile_melee`'s shared table because weapon choice is by equipment; a bow is handed out unconditionally, so the melee fallback is reachable only for the wither skeleton. The drowned trident row is `Missing` (no inventory model to key off).

### Vertical motion

`NavigatingMob::step_vertical` decides height from `dy = waypoint_y - pos_y` and whether a jump or fall is in progress (`fall_speed != 0.0`). A rise within the step-height attribute resolves instantly. A larger rise seeds `fall_speed = -JUMP_POWER` (0.42) and integrates one tick of projectile motion: displacement uses the pre-update speed, then gravity and drag update the stored speed, giving a peak near 1.252 blocks after 6 ticks (folding gravity into the same tick undershoots to about 0.85). Descent is gravity-accelerated and lands exactly on `waypoint_y`; there is no jump state machine, only the sign of `fall_speed`, and a jump only starts from rest.

### Tick drivers

`ProjectileRegistry` (ballistic motion) and `ItemEntityRegistry` (age, pickup delay, merge) in `lodestone-entity` advance through one `tick()` each and stay world- and wire-free; hit detection, pickup overlap and merge adjacency are the caller's job. A merge updates only the surviving side: `pickup_delay = max`, `age = min`. `MobSim` owns one of each plus uuid and entity-type maps, ticks them with mob AI, and `MobSim::snapshots()` folds mobs, projectiles and items into one wire list.

### Live wiring

`IntegratedServer::open_in_memory_with_mobs` runs a connection task beside `tick::run_tick_loop` (20 Hz: sim, natural spawning, block entities), publishing entity snapshots through `LiveMobSource`. Browser singleplayer uses the same cache via `IntegratedServer::serve_with_transport`; connection timer passes deliver publications even when the client sends nothing. The [dimension runtime](dimension-runtime.md) keeps each dimension's population, publication and action targets together; perception reads the whole same-dimension player registry, and travel clears publication cursors and entity diff maps because revisions and ids repeat across dimensions.

`LiveMobSource` holds the entity list and a monotonic revision under one mutex; every publication advances it, even an empty list, and the producer never compares snapshots. `EntitySource::snapshots_if_changed` reads both together. Each connection remembers its own last revision (new connections start with none), so repeated passes skip the clone and diff. Sources without a counter use the unconditional default; sources with a `PlayerRegistry` always merge and diff players; boss bars are diffed every pass. `net.rs` uses a small fixed chunk radius around join spawn for singleplayer.

### Goal cadence

A mob's goals start, stop and tick only on its "full" parity ticks (every second tick, offset by the mob's id, and the first two ticks). On the off ticks only goals that declare `requires_update_every_tick` run (swim-float, look-around, swell, melee, the ranged attacks). The swell goal also drops its fuse when the target is out of sight. Raw tick delays are halved with `reduced_tick_delay` (`ceil(n/2)`) because they count goal ticks. Two consequences for tests: draw `n` of a lone goal's RNG lands near tick `2n`, and a state change driven by a goal needs two sim ticks to be sure of including a goal tick.

### Pathing parameters

- **Fall budget**: `max_fall_distance` is 3 with no target; with a target it adds the health above a third of max health, less four per difficulty step below normal. The sim refreshes it each tick (`SimMob::set_difficulty` feeds the difficulty).
- **Stroll target**: ten candidates within 10 horizontal and 7 vertical, rejecting unstable footing, water and any non-zero malus; the first survivor wins and the result is the cell's bottom centre. A ground navigator snaps an air target down to the first block below and lifts a solid target out of it.
- **Path length and reach**: maximum length is `max(follow range, 16)`; an entity target reaches 0, a position reaches 1.
- **Melee re-path**: `MeleeAttackGoal` waits `4 + rand(7)` ticks between paths (+5 beyond 16 blocks, +10 beyond 32, +15 on failure) and only re-paths with sight and either a target that moved a block or a 5% roll.
- **Waypoint timeout**: a node is dropped when the time spent exceeds three times `distance / speed * 20`; the timer is not reset per node, so a long path must be re-created.
- **Malus**: per-species path-type overrides live in `crates/lodestone-entity/data/path_malus.json`, keyed by `PathType` variant name and generated from the reference by `just regen-path-malus` (`scripts/extract-path-malus.py`). `species_malus_overrides` reads it.

### Swimming

`MobShape::swimmer` selects `NavMode::Swim`: the search expands the six faces plus horizontal diagonals over cells whose whole body extent is water, and `swim_step` moves with an eased speed, a vertical push proportional to the vertical share of the heading, a 0.9 drag and gravity once out of the water. A swimmer's stroll destination comes from the same ten-offset search, kept only if the cell is open water.

Cod, salmon, tropical fish and pufferfish are swimmers (`species_swims` in `mobs/mod.rs`) with the `roster/aquatic.rs` goals: panic, flee players within 8 blocks (the perception feed is `flees_players`), swim stroll. Flock following and the pufferfish puff are `Missing` rows. Amphibious navigation is not implemented.

### Drifting and climbing

`NavMode::Drift` (squid, glow squid) has no path: `DriftGoal` chooses a velocity vector and `drift_step` applies it in pulses (full vector while the pulse phase is in the last quarter of its first half-turn, 0.9 glide after, only falling out of water); `DriftFleeGoal` aims the vector away from whoever hurt it, scaled by distance. Both are goals with no flags, so they run beside each other.

A mob with `MobShape::can_climb` (spiders) keeps the target of its last move and, when its ground path ends, heads straight at it. Pressing into a wall (reported by the live collision sweep) raises it 0.2 blocks per tick with gravity suspended; the attack goal stopping navigation ends the climb.

### Daylight and floating

`mobs/sunlight.rs` ignites sun-sensitive undead (zombie, zombie villager, skeletons, phantom) in bright daylight: each tick, with probability `2 * (brightness - 0.4) / 30` where brightness is `v / (4 - 3v)` for sky light `v = (15 - darkening) / 15` (darkening from `natural_spawn::sky_darkening_for`), and only with open sky over the eye (`PathWorld::sees_sky`, every cell above has zero light dampening), no helmet and not in water. Rain and block light are not modelled. The skeleton's `FleeSunGoal` heads for the first of ten random spots the sky does not light; the avoid-sun path restriction is missing.

`NavMode::Fly` (ghast) has no path or gravity: `FloatAroundGoal` picks a wanted spot within 16 blocks per axis, and every two to six ticks the body adds `flying_speed * 5/3` along the direction to it, or drops the wish if the swept body would hit a block; air keeps 0.91 of the velocity per tick. The reference's height-map clamp on the wanted spot is not modelled.

`NavMode::Air` (bee) searches the same six-face volume as a swimmer over cells the mob's path types allow (`search.rs` `volume_type`), and `air_step` follows the waypoint: the heading turns up to 90 degrees a tick, then 0.02 is added along it scaled by the requested speed (the `flying_speed` attribute times the goal modifier), with an equal vertical share whenever the waypoint is not level; the displacement settles at `0.012 / 0.09` a tick for a modifier of 1. Gravity applies only until the first move. `BeeWanderGoal` is a stroll at interval 10 whose destination (`air_wander_target`) is up to 8 blocks out within a quarter turn of the heading, 7 vertical, lifted 1 to 3 above any solid. Hive, flower and pollination goals stay `Missing`.

`NavMode::Flutter` (bat) has no goals (`roster/aerial.rs`): `flutter_step` hangs the bat under a full-cube block until the block goes or a player is within 4 blocks, then eases its velocity a tenth of the way toward half a block sideways and 0.7 vertically per tick (toward a random block within 6 sideways, -2 to +3 vertically), with 0.01 forward thrust, gravity 0.08, drags 0.91 and 0.98, and vertical speed cut to 0.6 each tick; a flying bat hangs again with a 1 in 100 roll under a full cube. Hanging reaches the client as `MetadataField::BatResting`. The roost test is any single full-cube collision shape, so blocks the reference excludes (glass, leaves, ice) count; the hanging bat's muted ambient sound is not modelled.

`NavMode::Swoop` (phantom) has no path: `roster/aerial.rs` goals write a `SwoopState` (move target, anchor, swooping) and `swoop_step` is the move control, turning 4 degrees a tick and easing the velocity a fifth of the way toward a speed that climbs while the heading holds and falls while it turns. A target scan every 60 ticks (64 blocks, line of sight) starts a 10-tick circle, then a dive at the target's body that ends on contact (a hit, damage 6), a wall or being hurt; the anchor then returns to 10 to 29 blocks over the ground. Cats frightening a phantom, choosing the highest player and the scan's horizontal limit are not modelled, and the sea-level clamp is fixed at 63.

### Fleeing

`MobController::flee_target` picks the destination of an avoid goal: up to 16 blocks out within a quarter turn of the direction away from the threat, rejected if nearer the threat than the mob already is. A controller that cannot aim falls back to the plain stroll search.

### Terrain and sight

Mobs tick against the live terrain (`tick_in`): an absent column is blocked for pathing, collision and sight, so a mob never enters unloaded ground. Target acquisition needs a ray between the two eyes clear of collision shapes (`PathWorld::has_line_of_sight`); a held target out of sight is dropped after 60 consecutive ticks.

## How to change it

- **Roster**: adding a species touches one family file (species list plus lookup arm). Cite the jar for that species, not a neighbour; copied priorities are the common silent error. A subclass may split or extend registration, so check every site. Test goals against the real controller, not a stub overriding every method. Use `GoalId` handles, not indices, across removals.
- **Brain**: new nearby-entity consumers read the existing feed. Widening a sensor radius also needs the host pre-filter box raised.
- **Perception**: declare with a default, implement, and feed from the tick loop; skipping the feed compiles and silently returns the default. Do not widen `block_cues` for many-position searches; those belong on the world-census side.
- **Neutral/ranged/vertical**: register only anger-gated target goals for neutral mobs or they attack on sight. Gate projectile-kind to entity-type mappings against the generated registry. Do not unify jump and fall integration order.
- **Tick drivers**: keep wire identity in the owner's maps, drop registry entry and map entry together, and test `tick()` itself.
- **Live wiring**: the sim must stay `Send`. Preserve the atomic revision/list read in `LiveMobSource`, keep revisions connection-local, and note that forwarding only an inner source's revision misses independent player changes.

## Configuration

- Roster tables are `const`, no feature flag.
- `NearestHostileSensor::RANGE` (8.0), host pre-filter (16.0 XZ, 8.0 Y) in `mobs/mod.rs`.
- Timers: `LAST_HURT_BY_TICKS`, `PANIC_DAMAGE_TICKS` (`ai/navigating_mob.rs`); `TEMPT_RANGE`, `AVOID_RANGE`, `AVOID_RANGE_Y`, `BREED_RANGE`, `BREED_DISTANCE_SQR`, `FOLLOW_PARENT_RANGE` (`mobs/mod.rs`); anger [400, 780] and piglin alert [80, 120] ticks.
- `JUMP_POWER` 0.42, `FALL_GRAVITY_PER_TICK` 0.08, `FALL_VERTICAL_AIR_DRAG` 0.98 in `navigating_mob.rs`.
- Mob tick interval 50 ms (`mobs/mod.rs`); singleplayer mob-area radius clamped 1..=3 (`net.rs`).

## Dependencies

- `lodestone_entity::{ai, brain, pathfinding::world::PathWorld, projectile, item_entity}`.
- `lodestone_server::mobs` (`MobSim`/`SimMob`), the only production consumer.
- The version crate's entity encoders and entity-type registry.
- The pinned `.cache/mc` decompile for every priority, multiplier, range and timing.
- Related: [autonomous navigation](autonomous-navigation.md), [combat](combat.md), [mob spawning](mob-spawning.md), [villagers](villagers.md).
