# Brain target acquisition: nearby-entity perception and OR-gated activities

## What it is

Two small additions to `lodestone_entity::brain` that a previous investigation
identified as the actual blocker for goat ram and villager panic — not a missing
jump-arc port or a missing flee behaviour, but the perception and eligibility
machinery those features need:

1. **`BrainMob::nearby_entities()`** — a host-fed list of nearby entities (id,
   position, hostile flag), and **`NearestHostileSensor`**, which reduces it to the
   nearest hostile within range and writes `MemoryModuleType::NEAREST_HOSTILE`.
2. **`Brain::add_activity_any_of`** — a disjunctive sibling to `add_activity`: an
   activity eligible when *any* of its listed `(memory, status)` pairs holds, not only
   when *all* of them do.

## How it works

### The perception half

```text
MobSim::feed_perception (brain-species mobs only, cost-gated like avoided_species)
  -> scan self.mobs within a 16x8 box
  -> NearbyBrainEntity { id, position, hostile: species::is_hostile_species(..) }
  -> NavigatingMob::set_nearby_entities
  -> BrainMob::nearby_entities()          (was: trait default, empty Vec)
  -> NearestHostileSensor::tick
       -> filter .hostile, cut to 8.0 blocks, pick nearest by squared distance
  -> MemoryModuleType::NEAREST_HOSTILE
```

The two-stage range split is deliberate: the host's 16×8 box is a coarse,
cheap pre-filter, and the *sensor* applies vanilla's real `8.0` cut
(`NearestHostileSensor.RANGE` in the jar) — the same split
`equipment-combat-stats.md`-style modules use between "cheap host filter" and "the
actual rule". Not modelled: line-of-sight (this crate's perception seam has no
ray-cast anywhere) and vanilla's `SENSOR_TAG` per-species exclusion list.

### The activity half

Vanilla's `VillagerPanicTrigger` is an imperative `Behavior<Villager>` that reaches
into its own entity's `Brain` and calls `setActiveActivityIfPossible(Activity.PANIC)`
directly from `start()`. This crate's `Behavior` trait deliberately has no seam for
that — it receives `&mut Memories` and `&mut dyn BrainMob`, never `&mut Brain`, so a
behaviour cannot reach into the scheduler that owns it (see `Behavior`'s own doc for
why: giving it that power would let any behaviour rewrite the whole activity set from
inside a tick, defeating the "coordinate only through memory" design).

The declarative equivalent, already the shape every other brain species'
`updateActivity` uses (`BrainGoal::tick`'s `set_active_activity_to_first_valid`
candidate list, evaluated fresh every tick before `brain.tick`): offer `PANIC` ahead
of `IDLE`, gated on "hurt OR a hostile is nearby". `Brain::add_activity`'s existing
requirement table is all-must-hold (`conditions.iter().all(...)`), which cannot
express an OR — hence a second table, `activity_any_requirements`, checked with
`.any(...)`, registered via `add_activity_any_of`. The two tables are mutually
exclusive per activity (whichever call registered an activity decides which table it
lands in); `Brain::activity_requirements_are_met` checks the AND table first, then the
OR table.

### The demonstrated consumer: villager panic

`villager_brain()` (in `brain/roster.rs`) is `scaffold()` plus `HurtBySensor` +
`NearestHostileSensor` + `Activity::PANIC` registered via `add_activity_any_of` on
`[(HURT_BY, ValuePresent), (NEAREST_HOSTILE, ValuePresent)]`, running the existing
`Panic` (`AnimalPanic`-style) behaviour. `brain_for("villager")` wraps it in
`BrainGoal::new(brain, vec![Activity::PANIC, Activity::IDLE])` — not
`BrainGoal::idle`, which only ever offers `IDLE` and would build `PANIC` and then
never let it become active.

**Disclosed simplification**: vanilla's `VillagerGoalPackages.getPanicPackage` uses
`SetWalkTargetAwayFrom` — a *directed* flee, away from the specific hostile/hurt
source. This reuses `Panic`'s random-land-position flee instead (no directed-away-from
behaviour exists in this crate yet). Also not ported: `VillagerCalmDown` (the OR-gate
itself handles exiting PANIC once neither condition holds, so this is not needed for
correctness, only for vanilla's exact re-entry cooldown shape),
`VillageBoundRandomStroll` (no village-bounds concept here), and
`VillagerPanicTrigger.tick`'s `spawnGolemIfNeeded` (golem-summon-on-hurt) — a
separate, unbuilt unit; nothing here calls it.

## How to change it

* **A new nearby-entity consumer** (a ram target, a golem's attacker) reads
  `BrainMob::nearby_entities()` directly or writes its own reducing sensor the way
  `NearestHostileSensor` does — the perception feed is already host-wired for every
  brain-species mob, so a new sensor needs no changes to `mobs/mod.rs`.
* **A new OR-gated activity** calls `add_activity_any_of` instead of `add_activity`.
  Remember it asserts a non-empty condition list — an activity with no way to become
  eligible belongs in neither table.
* **Widening the perception radius** past `NEAREST_HOSTILE_SCAN_RANGE`/`_Y` in
  `mobs/mod.rs` is safe for any sensor whose own range is inside it; a sensor wanting a
  *wider* radius than the host's box needs that constant raised too, since the box is
  a hard pre-filter, not merely a default.

## Disclosed gaps

* **No line-of-sight test anywhere on this seam.** A hostile can be acquired through a
  wall, the same permissive-by-default gap `crate::ai::roster::hostile_melee`'s own
  doc names for goal-system target acquisition.
* **Cost gating is per-species-family, not per-instance.** Every brain-species mob
  pays the O(n) nearby scan every tick regardless of whether its own brain actually
  registers `NearestHostileSensor`; only goal-system species are excluded.
* **Goat ram and `#230`'s remaining items are not built here.** This closes the
  perception/eligibility blocker only — `PrepareRamNearestTarget`/`RamTarget` is a
  separate, unbuilt unit. Golem-summon-on-hurt and the villager WORK/MEET/REST
  schedule, both named here as unbuilt when this doc was written, have since
  landed elsewhere: golem-summon-on-hurt in `MobSim::tick_golem_summon`
  (`mobs/mod.rs`), and the schedule in `brain::roster::villager_brain` plus
  `BrainGoal::tick`'s schedule mode — see `docs/villager-professions-and-trading.md`
  for the schedule's own account (it landed as the `WalkToPoi` behaviour, the
  `set_schedule`/`has_schedule` wiring, and `crate::mobs::villager::BellClaims`,
  the third POI-claim ledger it needed).

## Configuration

`NearestHostileSensor::RANGE` (`8.0`, `brain/sensor.rs`) is the sensor's own cut.
`NEARBY_HOSTILE_SCAN_RANGE`/`NEARBY_HOSTILE_SCAN_RANGE_Y` (`16.0`/`8.0`,
`mobs/mod.rs`) is the host's coarser pre-filter box.

## Dependencies

`lodestone_entity::brain` only for the primitive; `lodestone-server`'s
`mobs/mod.rs::feed_perception` and `species::is_hostile_species` for the production
feed. No protocol or world access.
