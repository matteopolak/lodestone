# Villager WORK/MEET/REST schedule (issue #231, villager half)

## What it is

A real day/night activity schedule for villagers — `IDLE` in the morning and
late night, `WORK` at a claimed workstation from tick 2000, `MEET` at a
claimed bell from tick 9000, `REST` at a claimed bed from tick 12000 — read
straight off 26.2's own `data/minecraft/timeline/villager_schedule.json`
rather than transcribed from a pre-26.2 `Schedule` constant. A villager with
the relevant POI claimed visibly walks there once its window opens; one
without simply falls back to `IDLE`.

This is the "villager Brain package" half of #231 (piglin's own Brain work is
untouched by this change). `PANIC` — the hurt/hostile-nearby flee — already
existed (`docs/brain-target-acquisition.md`); this change adds the other four
activities.

## How it works

The chain, end to end:

```text
crate::mobs::villager::{WorkstationClaims,BedClaims,BellClaims}   (server crate; claim ledgers)
  -> SimMob::{workstation,bed,meeting_point}                       (server crate; per-mob claim positions)
  -> MobSim::feed_perception                                       (server crate; converts BlockPos -> Vec3 block centre)
  -> NavigatingMob::{set_job_site,set_home,set_meeting_point,set_day_time}  (entity crate; host injection)
  -> BrainMob::{job_site,home,meeting_point,day_time}               (entity crate; the trait seam)
  -> VillagerPoiSensor                                              (entity crate; copies into memory)
  -> MemoryModuleType::{JOB_SITE,HOME,MEETING_POINT}                (entity crate; the blackboard)
  -> Brain::update_activity_from_schedule + WalkToPoi + MoveToTargetSink  (entity crate; the switch + the walk)
  -> a real position change on a real, spawned villager
```

Every hop above is exercised by a real, spawned villager through
`MobSim::tick` in
`crates/lodestone-server/src/mobs/mod.rs::villager_schedule_tests` — not a
hermetic `Brain`/`BrainMob` double — because a chain this long is exactly the
shape that reaches five hops working and fails silently on the sixth (see
`CLAUDE.md`'s own account of that failure mode). The two headline tests spawn
a villager some distance from a composter (`WORK`) or a bell (`MEET`), let it
claim the POI while the clock sits in `IDLE`'s window, then move the clock
into the target window and assert the villager's distance to the POI both
*shrinks by a real margin* and *lands near `WalkToPoi`'s own close-enough
radius* — a magnitude prediction, not merely "the position changed".

### The entity-crate half (`crates/lodestone-entity/src/brain/`)

- `memory.rs` — three new `MemoryModuleType` positions: `JOB_SITE`, `HOME`,
  `MEETING_POINT` (vanilla's own memory names).
- `mob.rs` — `BrainMob` grows `day_time() -> i32` (defaults to `0`) and
  `job_site`/`home`/`meeting_point() -> Option<Vec3>` (default `None`).
  **`day_time` is deliberately not `game_time`**: `game_time` is
  `NavigatingMob`'s own per-mob monotonic tick counter (see its doc for why),
  which has no relationship to the real world clock a schedule needs.
- `sensor.rs` — `VillagerPoiSensor` copies the three `BrainMob` answers into
  their memories each tick, the same shape `HurtBySensor`/`NearestHostileSensor`
  already are for their own host-fed inputs.
- `behaviors.rs` — `WalkToPoi::new(source_memory, speed, close_enough)`: a
  simplified `SetWalkTargetFromBlockMemory` — when farther than
  `close_enough` blocks from `source_memory`'s position it writes
  `WALK_TARGET`; `MoveToTargetSink` (already in every brain's `CORE`) does the
  actual walking. **Two disclosed cuts** against the jar original: no
  intermediate-point walk when the target is very far, and no
  `CANT_REACH_WALK_TARGET_SINCE`-driven claim abandonment — see the type's own
  doc comment.
- `mod.rs` — `Brain::has_schedule()`, so a caller can tell whether
  `update_activity_from_schedule` is safe to call at all (see below).
- `driver.rs` — `BrainGoal::tick` now calls
  `Brain::update_activity_from_schedule(mob.day_time(), mob.game_time())`
  after its existing candidate-list check, but **only when the brain carries a
  schedule and `PANIC` did not just become active**. Both guards are
  load-bearing:
  - Without the `has_schedule()` guard, every *other* brain species (goat,
    warden, …) would have `IDLE` force-picked over their own activity
    (`RAM`, …) every 20 ticks, because `update_activity_from_schedule`'s own
    fallback for "no schedule attached" is `IDLE`.
  - Without the `PANIC` guard, a hurt villager's `WORK`/`MEET`/`REST` would be
    re-picked over `PANIC` on the very same tick `PANIC` started, because
    `update_activity_from_schedule` only checks `is_active(scheduled)`, which
    knows nothing about `PANIC`'s urgency. Vanilla avoids this by simply never
    registering its own schedule-check behaviour inside `getPanicPackage` —
    this port's equivalent is skipping the call outright while `PANIC` is
    active.
- `roster.rs` — `villager_brain()` adds `WORK`/`MEET`/`REST` (each one
  `WalkToPoi` behaviour, at vanilla's own close-enough distances: `9`/`6`/`1`)
  and `brain.set_schedule([(10, IDLE), (2000, WORK), (9000, MEET), (11000,
  IDLE), (12000, REST)])` — `villager_schedule.json`'s own keyframes, not the
  baby track (this crate has no separate baby-villager brain).
  `brain_for("villager")`'s candidate list is now `[PANIC]` alone, **not**
  `[PANIC, IDLE]` — see that function's own doc comment for why leaving `IDLE`
  in would have fought the schedule every tick that is not itself the
  (throttled) schedule check.
- `ai/navigating_mob.rs` — `NavigatingMob` gains `day_time`/`job_site`/`home`/
  `meeting_point` fields, their `set_*` host-injection setters, and the
  `BrainMob` overrides that read them back.

### The server-crate half (`crates/lodestone-server/src/`)

- `mobs/villager/mod.rs` — `BellClaims`/`find_and_claim_bell`/`is_bell_block`,
  `WorkstationClaims`/`BedClaims`'s third sibling, for `PoiTypes.MEETING`
  (`minecraft:bell`, 32 tickets — a crowd, not a queue of one, unlike a
  workstation or a bed's single ticket). Nothing about the raid trigger needs
  it (`occupied_homes_in_range` already covers that with beds alone); `MEET`
  is the only reason this ledger exists.
- `mobs/mod.rs` — `SimMob::meeting_point`/`bell_search_cooldown` (mirrors
  `bed`/`bed_search_cooldown`), `MobSim::tick_villager_bells` (mirrors
  `tick_villager_beds`, called right after it), `MobSim::day_time` +
  `set_day_time` (host-injected once per tick), and
  `feed_perception`'s new per-mob feed: `m.workstation`/`m.bed`/
  `m.meeting_point` (all `Option<BlockPos>`) convert to block-centre `Vec3`
  and reach `NavigatingMob::set_job_site`/`set_home`/`set_meeting_point`
  unconditionally for every mob (every non-villager's claim fields simply stay
  `None` for their whole life, the same "harmless default" shape
  `set_nearby_entities` already is for a goal-driven mob).
- `tick.rs` — `crate::tick::run_tick_loop` reads
  `world_state.time().day_time.rem_euclid(24_000)` (a **read**, not
  `tick_time()`'s advancing call — that would double-advance the clock once
  per tick) and calls `sim.set_day_time(...)` immediately before
  `sim.tick_with_terrain(...)`.

## How to change it

- **A new WORK/MEET/REST behaviour** (the profession-specific work animation,
  the trade UI, the sleep pose, socialising at the bell) is a new priority
  slot inside the relevant `brain.add_activity(...)` call in
  `roster::villager_brain`, exactly like adding a new goal to a
  `GoalSelector` species row.
- **A new schedule-driven species** (nothing today besides the villager)
  calls `Brain::set_schedule` in its own `roster::*_brain` constructor and
  makes sure `BrainMob::day_time` is fed for it — `has_schedule()`'s guard in
  `BrainGoal::tick` means nothing else needs to change.
- **Widening `WalkToPoi`'s fidelity** (the far-target intermediate-point walk,
  the unreachable-claim abandonment) is a change to that one type in
  `brain/behaviors.rs`; every activity that uses it (`WORK`/`MEET`/`REST`)
  picks the improvement up for free.
- **The 100-tick job/bed/bell search interval** is a scope choice
  (`JOB_SEARCH_INTERVAL_TICKS`/`BED_SEARCH_INTERVAL_TICKS`/
  `BELL_SEARCH_INTERVAL_TICKS` in `mobs/mod.rs`), not a transcribed vanilla
  constant — see `villager::SEARCH_RADIUS`'s own doc for the same disclosure
  about the 16-block bounded scan versus vanilla's real ~48-block indexed
  search.

## Disclosed gaps

- **The `WORK`/`MEET`/`REST` activities are commute-only.** A working
  villager walks to its workstation and stops; it does not run
  `WorkAtPoi`/`WorkAtComposter` (harvest/restock animation and particles),
  `ShowTradesToPlayer`/`SetLookAndInteract` (villager-initiated trade UI),
  `SleepInBed` (the sleeping pose and bed-occupied flag), or
  `SocializeAtBell`/`StrollAroundPoi` (wandering near the claim rather than
  beelining to its centre). The day/night switch and the walk are real; what
  a villager visibly *does* once arrived is not.
- **No baby-villager schedule.** `villager_schedule.json`'s
  `baby_villager_activity` track (`PLAY` instead of `WORK`/`MEET`) is not
  read — every villager, baby or adult, uses the adult track.
- **`WalkToPoi` never gives up.** A villager whose claimed POI sits behind
  unnavigable terrain keeps retrying every tick rather than eventually
  abandoning the claim and searching elsewhere — see that type's own doc.
- **Piglin's own Brain package is untouched.** This change is the villager
  half of #231 only.

## Configuration

`VILLAGER_SPEED_MODIFIER` (`0.5`, `brain/roster.rs`) is vanilla's own
`Villager.SPEED_MODIFIER`. `VILLAGER_SCHEDULE` (same file) is the keyframe
table. The three close-enough radii (`9`/`6`/`1` for job site/bell/bed) are
inline in each `WalkToPoi::new(...)` call in `villager_brain`.
`JOB_SEARCH_INTERVAL_TICKS`/`BED_SEARCH_INTERVAL_TICKS`/
`BELL_SEARCH_INTERVAL_TICKS` and `villager::SEARCH_RADIUS` are named above.

## Dependencies

`lodestone_entity::brain` for the schedule/activity/behaviour machinery;
`crate::poi_storage::PoiRecord` for `BellClaims`'s ticket accounting (native
only — see `villager/mod.rs`'s own doc for the `wasm32` narrowing this
inherits); `crate::world_state::WorldStateHandle` for the real day-time feed
in `tick.rs`. No protocol changes — a claimed profession already reached the
wire before this change (`docs/villager-professions-and-trading.md`); this
change is entirely server-side movement and has nothing new to encode.
