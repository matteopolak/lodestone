# Mob AI

## What it is

Server-side mob AI and simulation: the per-species goal roster and priority
scheduler, brain-style target acquisition for villager-class mobs, entity and
block perception (what a goal is allowed to know about the world), the
neutral-mob anger/aggro roster, ranged attacks, vertical motion (step/jump/
fall), and the generic per-tick projectile/item drivers. All of it runs every
tick inside `MobSim` (`crates/lodestone-server/src/mobs/mod.rs`), which is
also the production wiring that gets AI-driven mobs onto the wire to a real
client.

## How it works

### Goal roster

A pure, world-free lookup from a species path (`"cow"`, `"creeper"`) to the
`Goal`s vanilla's `registerGoals()` installs, at vanilla's own priority
numbers. Code lives in `crates/lodestone-entity/src/ai/roster/`, one file per
species family (`hostile_melee`, `ranged`, `passive`, `neutral`,
`specialist`); entry point `goals_for`, called once by `MobSim::spawn_species`.

Each species resolves to a `&'static [Registration]`, one row per vanilla
`addGoal` call, tagged `goal` (we build it), `missing` (no such goal exists),
or `covered` (a sibling row already covers it — e.g. one class-agnostic
`AvoidEntityGoal` standing in for two vanilla registrations generic over
different classes). `Missing` rows stay at their real priority so an absent
capability can't be confused with an untranscribed one. Vanilla's `addGoal`
speed arguments multiply the mob's `MOVEMENT_SPEED`; each builder computes
`ctx.speed * <jar factor>` so the multiplier stays checkable.

Vanilla gives every mob two independently-numbered selectors
(`goalSelector`/`targetSelector`); `SimMob` holds one `GoalSelector`. That's
safe because a priority number only ever competes against a goal contending
for the same `Flag`, and vanilla's target goals are exactly the goals whose
flag set is `{TARGET}` — the two namespaces can't collide when flattened with
jar priorities kept verbatim. `goals_for` emits every target-selector row
ahead of every goal-selector row, reproducing vanilla's tick order (a target
acquired this tick is visible to a movement goal the same tick).
`GoalSelector::add` returns a `GoalId`; `remove(id, mob)` stops a running goal
(firing its `stop` hook) and drops it — needed for vanilla's runtime goal
swap (`AbstractSkeleton.reassessWeaponGoal()` swaps melee/bow goals when the
held item changes), which a per-`Flag` `disable` can't express.

### Target acquisition (brain-based mobs)

Two additions to `lodestone_entity::brain`, demonstrated by villager panic:
**`BrainMob::nearby_entities()`**, host-fed from a coarse pre-filter box, plus
**`NearestHostileSensor`**, which cuts that to vanilla's real 8-block range
and writes `MemoryModuleType::NEAREST_HOSTILE`; and **`Brain::add_activity_any_of`**,
a disjunctive sibling to `add_activity` — an activity becomes eligible when
*any*, rather than all, of its `(memory, status)` pairs holds. `villager_brain()`
registers `Activity::PANIC` ahead of `IDLE` this way, on
`[(HURT_BY, present), (NEAREST_HOSTILE, present)]`, because this crate's
`Behavior` trait deliberately has no seam for a behaviour to reach into its
own `Brain` the way vanilla's `VillagerPanicTrigger` does. The flee target is
a random land position rather than directed away from the threat, and no
goal on this perception seam checks line of sight.

### Perception (goal-based mobs)

`MobController` exposes eight perception methods a goal's `can_use` reads;
`NavigatingMob` is the only production implementor and `MobSim::tick` the
only feeder: `in_water`/`in_lava` derive from the `PathWorld` the mob already
borrows; `last_hurt_by`/`is_panicking` are two **independently-decaying**
records (100 vs. 40 ticks — a mob stops fleeing 60 ticks before it stops
hunting), both set from `hurt`'s single event, and damage with no living
attacker panics the mob but leaves `last_hurt_by` untouched; `no_action_time`
is the sim's per-mob counter; `avoid_threat` reads the mob census plus an
`avoided_species` table; `nearest_player`/`temptation` come from
`MobSim::set_players`, fed by `server.rs`'s `PlayerMoved` handler.

Ordering inside `MobSim::tick` matches vanilla's `Mob.serverAiStep()`:
`no_action_time` ages for every mob **before** `feed_perception` runs (a
read-then-apply two-pass, since one mob's threat/partner/parent decision
depends on every other mob), before goals tick. `tempt_food` (which item
tempts which species) is transcribed per-species from the jar's own item
tags, not folklore lists, which are wrong for several species in 26.2.

**Block perception.** `PathWorld::block_cues(x, y, z) -> BlockCues` classifies
one block into a struct of boolean predicates (not an enum or block id, since
vanilla's own tests are simultaneous tag checks); `MobController::ate(EatenBlock)`
carries the resulting mutation intent back out, drained by the host like
`attack`. This is a *query*, not a per-tick feed — `EatBlockGoal` is the only
reader and consults a block roughly once every 500 ticks, so pre-feeding it
would cost far more than it saves. Most goal rows that look
block-perception-gated actually need a larger mechanism: a host-computed
*candidate block position* (the `MoveToBlockGoal` shape) for a skeleton's
sun-flee spot, a zombie's turtle-egg attack, and a rabbit's garden raid, plus
block-state properties and destroy intents on top. Only sheep grazing and
half of the rabbit's powder-snow climb are closed by `block_cues` alone.
Every jar tick constant on `EatBlockGoal` is **halved in practice**
(`Goal.adjustedTickDelay`, for a goal that doesn't override
`requiresUpdateEveryTick`): the jar's `1000`/`50`/`40`/`4` are 500/25/20/2
ticks.

### Neutral roster (anger/aggro)

Enderman, zombified piglin, bee and wolf implement vanilla's `NeutralMob`,
sharing an anger duration uniform on **[400, 780] ticks inclusive**, stored
as an **absolute game-time deadline** rather than a countdown
(`SimMob::anger` holds `{ end_time, target }`, so a paused or stepped tick
loop can't desync it) — started alongside `note_hurt` in `MobSim::attack`,
cleared by `feed_perception` once the deadline passes.

Three species chain `.setAlertOthers()` onto `HurtByTargetGoal`, alerting
same-species mobs with no current target inside a box (flat vertical
half-extent **10** for all three; XZ half-extent 35 for zombified piglin via
`FOLLOW_RANGE`, 16 for wolf/bee). The zombified piglin also has a **second,
independent** propagation path — a per-tick census throttled to
**[80, 120] ticks**, gated on line of sight — modelled as a per-mob countdown
in `MobSim::tick`, not a roster row.

The enderman stare test is `look.dot(dir) > 1.0 - coneSize / dist`
(`coneSize = 0.025`), so the required precision **increases with distance**
(the cone narrows, not widens); `EndermanFreezeWhenLookedAt` consumes it
directly, but the teleport-on-stare goal is not built. A stung bee
**survives the sting** — anger clears immediately, and death comes later from
a per-tick probabilistic roll whose clamp guarantees the bee is dead within
1200 ticks and alive one tick after stinging.

Five primitives back all of this on `MobController`: an anger deadline/
target, a gaze test (fed from a real per-player view vector), instant
teleport, self-damage (drained through the normal `apply_damage` path,
i-frames included), and an ownership relation (fed from `PlayerIdentity`).
Each is landed on both the goal-system and host sides; what a given species
does with the primitive is tracked per-species in the roster module.

### Ranged attacks

A goal never spawns an entity — it computes vanilla's aiming maths and calls
`MobController::launch_projectile`; `NavigatingMob` accumulates launches and
a host drains them once per tick, resolving each into a real
`ProjectileRegistry` entry via `MobSim::spawn_projectile`. Three goal shapes:
a 20-tick-draw bow attack, a no-draw interval-lerp generic ranged attack, and
the blaze's burst-of-three fireball (6 ticks apart, 60-tick wind-up, 100-tick
pause, melees under 2 blocks). Launch velocity follows vanilla's
`Projectile.getMovementToShoot`: arrows, tridents, snowballs and potions
launch at power **1.6** plus an arc lift; small fireballs launch at power
**0.1** with **no** arc lift and accelerate in flight instead.

Skeleton-family species register through `hostile_melee`'s shared table, not
here, because the weapon choice is decided by equipment — a bow is handed
out unconditionally, making the melee fallback unreachable for every subtype
except `WitherSkeleton`, which overrides the equipment instead. The
drowned's trident row is deliberately left `Missing`: only a minority of
drowned hold a trident and this repo has no inventory model to key off of.

### Vertical motion

`NavigatingMob::step_vertical` decides a path-following mob's height each
tick, keyed on `dy = waypoint_y - pos_y` and whether a jump/fall is already
in progress (`fall_speed != 0.0`). A rise within the mob's step-height
attribute resolves instantly (auto-step). A larger rise seeds
`fall_speed = -JUMP_POWER` (fixed **0.42** blocks/tick) and integrates one
tick of real projectile motion — displacement uses the *pre-update* speed,
then gravity/drag update the stored speed for next tick, landing the peak
near real vanilla jump height (≈1.252 blocks after 6 ticks). Folding gravity
into the same tick's displacement instead measurably undershoots the peak
(~0.85 blocks), which is why jump and fall integrate in different orders.
Descent is gravity-accelerated and lands exactly on `waypoint_y`, resetting
`fall_speed`; a jump's ascent hands off into this same branch once its speed
crosses back to non-negative — there is no separate jump state machine, only
the sign of `fall_speed`. A jump only ever starts from rest, matching
vanilla's `MoveControl` staying in its `JUMPING` operation until grounded.

### Tick drivers

`ProjectileRegistry` (ballistic motion) and `ItemEntityRegistry`
(dropped-item age/pickup-delay/merge) are per-tick driver types in
`lodestone-entity` that own a collection of otherwise-correct,
previously-unconsumed entity mechanics and advance all of them through one
`tick()` call. Both are deliberately world/wire-free — hit detection, pickup
overlap and merge-adjacency stay the caller's job, mirroring the split
`SimMob` already makes for mobs. `ItemEntityRegistry::merge` follows
vanilla's overload: only the surviving side is updated, `pickup_delay`
becomes `max(to, from)` and `age` becomes `min(to, from)`. `MobSim` owns one
instance of each as fields (plus small metadata maps for uuid and
entity-type key, since the registries stay version/wire-free) and calls
`self.projectiles.tick()` / `self.items.tick()` every server tick alongside
mob AI; `MobSim::snapshots()` folds mobs, projectiles and items into one
list for the wire.

### Live wiring: reaching a real client

`IntegratedServer::open_in_memory_with_mobs` spawns the existing connection
task (diffing `LiveMobSource::snapshots()` against what was last sent)
alongside `tick::run_tick_loop`, which builds a second, independent
`ChunkWorld` snapshot of the same deterministic terrain, seeds a small fixed
population once via `seed_demo_mobs` (cycling `DEMO_SPECIES`, each spawned
through the real roster with `MobSim::spawn_species`), then loops at 20 Hz:
tick the sim and block entities, publish snapshots. `LiveMobSource` is an
`Arc<Mutex<Vec<EntitySnapshot>>>` behind `EntitySource`.
`crates/lodestone-shell/src/net.rs` calls this for singleplayer with a small
fixed chunk radius around join spawn, independent of the client's own
streamed view radius.

`DEMO_SPECIES`'s first six entries cover one species per roster family plus
a creeper, since production seeds exactly six; `zombie` stays at index 0
because mob ids are assigned in spawn order starting at **1000**
(`MobSim::set_next_id(1000)`), chosen to avoid colliding with the local
player's own entity id (`1`), which silently ate the first mob a fresh
`MobSim` ever spawned before this was found — `MobSim::new`'s own default id
start (`1`) is unchanged for hermetic tests.

## How to change it

- **Goal roster**: adding a species touches one file — add its path to the owning family's species list
  and an arm to its lookup. Cite the jar directly, never a neighbouring species' table; a priority or
  speed multiplier copied from a similar-looking species is a common, hard-to-notice mistake.
  `registerGoals` is not always one method in vanilla — a subclass may split it or add rows before
  calling its parent, so check every registration site in the source, not just the obvious one. Test
  goals against the real controller, not a perception stub that overrides every method — a stub can make
  a goal that never actually fires in production look green. Use goal handles rather than captured
  indices across a removal, since removal shifts later indices down.
- **Target acquisition**: a new nearby-entity consumer should read the existing perception feed rather
  than writing a new one — it's already wired for every brain-species mob. Widening a sensor's radius
  needs the host's own coarser pre-filter box raised too, since that's a hard cut, not just a default.
- **Perception**: adding a method needs three steps — declare it with a default, implement it, and feed
  it from the tick loop. Skipping the feed step produces no compile error and no warning, only a
  perception method that always returns its default. A range that's an attribute on the mob and a range
  that's an argument to the goal are two different things; applying both silently takes the minimum.
  Don't widen the block-cue query for a candidate-position search (checking many positions per tick);
  that class of check belongs on the world-census side instead, not the per-block query.
- **Neutral roster / ranged attacks / vertical motion**: never register a neutral mob's plain,
  non-anger-gated target goal — only the anger-gated form, or it attacks on sight. A subclass that skips
  its parent goal registration inherits some rows and silently drops others; check per species. A wrong
  registry name in a projectile-kind-to-entity-type mapping streams a different, real entity type
  silently — gate it against the generated entity-type registry. Do not unify the jump/fall integration
  order — front-loading displacement before gravity in both directions measurably shortchanges a jump's
  peak height.
- **Tick drivers**: keep wire identity (uuid, entity-type key) in the owning simulation's own metadata
  maps, never on the driver registries themselves — that's what keeps the registries version/wire-free.
  A despawned entry must be dropped from both the registry and its metadata map together, or the map
  leaks silently. Test the driver's `tick()` itself, not only the pure per-entity function it calls,
  which proves nothing about whether anything drives it in production.
- **Live wiring**: the simulation and everything it holds must stay `Send`, since it's ticked from a
  spawned async task. There is no natural spawning in production yet — a fixed demo population is seeded
  once so AI motion has something to move (see [`docs/mob-spawning.md`](./mob-spawning.md) for the
  candidate-source seam this will eventually plug into) — and no despawn pass, since the tick task has no
  way to learn a player's position. A version target with no async timer support (e.g. wasm32) falls
  back to a mob-free path entirely.

## Configuration

- Roster tables (`ai/roster/`) are `const`, compile-time, no feature flag.
- `NearestHostileSensor::RANGE` (`8.0`) and the host's coarser pre-filter
  (`16.0`/`8.0` XZ/Y, `mobs/mod.rs`).
- Perception timers: `LAST_HURT_BY_TICKS`, `PANIC_DAMAGE_TICKS`
  (`ai/navigating_mob.rs`); `TEMPT_RANGE`, `AVOID_RANGE`, `AVOID_RANGE_Y`,
  `BREED_RANGE`, `BREED_DISTANCE_SQR`, `FOLLOW_PARENT_RANGE`/`_Y`
  (`mobs/mod.rs`).
- Anger duration bounds `[400, 780]` ticks; piglin alert interval
  `[80, 120]` ticks (`mobs/mod.rs`).
- Vertical motion: `JUMP_POWER` (`0.42`), `FALL_GRAVITY_PER_TICK` (`0.08`),
  `FALL_VERTICAL_AIR_DRAG` (`0.98`), all `pub const` in `navigating_mob.rs`.
- Live wiring: demo mob count (`6`), spawn center matching the server's
  hardcoded join spawn, mob-area radius (clamped `1..=3`) in `net.rs`;
  `seed_demo_mobs`'s ring radius (`6.0` blocks) and the mob tick interval
  (`50ms`, one vanilla tick) in `mobs/mod.rs`.
- No env vars anywhere in this subsystem.

## Dependencies

- `lodestone_entity::ai::{goal, goals, roster}` — scheduler, goal
  implementations, roster tables.
- `lodestone_entity::brain` — brain-activity primitives for villager-class
  target acquisition.
- `lodestone_entity::pathfinding::world::PathWorld` — block/fluid
  classification (`block_cues`, water/lava).
- `lodestone_entity::projectile` / `item_entity` — `ProjectileRegistry`,
  `ItemEntityRegistry`.
- `lodestone_server::mobs` — `MobSim`/`SimMob`, the only production consumer:
  spawns species, feeds perception, resolves attacks/launches/anger/
  ownership, ticks the registries, and owns the live-wiring loop.
- `crates/protocol/v770` — the entity encoders, and the entity-type registry
  ranged attacks validate projectile kinds against.
- `.cache/mc/26.2/src/net/minecraft/world/entity/` — the authority for every
  priority, speed multiplier, range and timing cited here.
- Related: [`docs/autonomous-navigation.md`](./autonomous-navigation.md)
  (pathfinding), [`docs/combat.md`](./combat.md) (attack resolution and
  knockback), [`docs/mob-spawning.md`](./mob-spawning.md) (natural spawning),
  [`docs/villagers.md`](./villagers.md) (the work/rest schedule built on
  brain target acquisition).
