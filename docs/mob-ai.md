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
