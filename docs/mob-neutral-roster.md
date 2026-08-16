# Mob neutral roster: the grudge-holding species

## What it is

The `neutral` family of the per-species goal roster — enderman, zombified piglin, bee and wolf —
plus the record of why the four mechanisms named in the tracking issue's title are **not**
implemented as goals. The tables are complete transcriptions of the
jar; the mechanisms are blocked on five named primitives that do not exist on `MobController`, and
this doc is the list of what has to be added and where.

Code: `crates/lodestone-entity/src/ai/roster/neutral.rs` (one file, no shared edits). Gates: that
module's own `#[cfg(test)]` block, plus `roster`'s cross-family invariant gates.

## How it works

Each of the four species resolves to a `&'static [Registration]` in the shape unit A3 defined — see
[`mob-goal-roster.md`](./mob-goal-roster.md) for `Registration`, `Selector`, `Coverage` and why
priorities go in verbatim from the jar. Nothing about this family departs from that shape. What is
worth reading are the four species' mechanisms, because each is a different one.

### The grudge, and what 26.2 changed

All four species implement vanilla's `NeutralMob` (`.cache/mc/26.2/src/net/minecraft/world/entity/NeutralMob.java`,
154 lines) and all four use the same duration:

```java
PERSISTENT_ANGER_TIME = TimeUtil.rangeOfSeconds(20, 39)
```

cited at `monster/EnderMan.java`, `monster/zombie/ZombifiedPiglin.java`,
`animal/bee/Bee.java`, `animal/wolf/Wolf.java`. `TimeUtil.rangeOfSeconds(min, max)` is
`UniformInt.of(min * 20, max * 20)` (`util/TimeUtil.java`) and `UniformInt::sample` is
`Mth.randomBetweenInclusive` (`util/valueproviders/UniformInt.java`), so the grudge is
**uniform on [400, 780] ticks inclusive**. Two plausible-but-wrong readings to avoid: `[20, 39]`
(seconds mistaken for ticks) and a half-open upper bound.

**26.2 stores an absolute game-time deadline, not a countdown.**
`setTimeToRemainAngry(remaining)` writes `level.getGameTime() + remaining`
(`NeutralMob.java`), `isAngry()` compares that against the clock, and
`NO_ANGER_END_TIME` is `-1L`. Porting the pre-26.2 `remainingPersistentAngerTime` countdown
would drift against a paused or stepped tick loop. The wolf additionally keeps its deadline in
**entity metadata** (`DATA_ANGER_END_TIME`, `Wolf.java`), so anything putting it on the wire must
run `crates/protocol/v770/oracle-java/EntityDataIndexOracle.java` rather than hand-counting.

### Propagation is a box, and it is not a goal

Three species chain `.setAlertOthers()` onto their `HurtByTargetGoal`. That primitive
(`ai/goal/target/HurtByTargetGoal.java`) inflates the mob's position by
`(followRange, 10.0, followRange)` — an **axis-aligned box with a flat vertical half-extent of 10**
(`ALERT_RANGE_Y = 10`) — takes only entities of the mob's **own class**, and requires
`other.getTarget() == null` and `!other.isAlliedTo(attacker)`. For a `TamableAnimal` it also requires
**`tamable.getOwner() == other.getOwner()`**.

| species | XZ half-extent | Y half-extent | source of the XZ figure |
|---|---|---|---|
| zombified piglin | 35.0 | 10.0 | `FOLLOW_RANGE`, inherited from `Zombie.java` |
| wolf | 16.0 | 10.0 | `Mob.createMobAttributes()` default, `Mob.java` |
| bee | 16.0 | 10.0 | same, via `Animal.createAnimalAttributes()` |

The zombified piglin has a **second, independent** propagation path that is easy to conflate with the
first: its own `alertOthers` (`ZombifiedPiglin.java`), same box shape, but driven from
`customServerAiStep` rather than from a goal's `start()`, throttled by
`ALERT_INTERVAL = TimeUtil.rangeOfSeconds(4, 6)` = **[80, 120] ticks**
(`PIGLIN_ALERT_INTERVAL_TICKS` in `mobs/mod.rs`), and gated on line of sight to the current target.
So "piglin group aggro" is a per-tick census, not a `Registration` — it lives in `MobSim::tick`'s
own per-mob loop, next to `feed_perception`, rather than as a roster row.

**Landed.** `SimMob::piglin_alert_ticks` carries the countdown (`-1` sentinel = no active timer,
rolled fresh via `piglin_alert_interval` the first tick a piglin holds a target, no immediate fire —
a disclosed simplification, since the seam has no "did I have a target last tick" signal to detect
vanilla's true acquisition edge from). While the countdown holds a target it decrements every tick;
at zero it checks `RayView::is_clear` between the piglin's own position and
`MobController::attack_target()` (the disclosed stand-in for a live `getTarget()` reference this seam
does not carry) and, if clear, queues an alert applied to the rest of `self.mobs` after the per-mob
loop ends — reusing this section's own box. `mobs::anger_tests::a_piglin_holding_a_target_alerts_a_neighbour_that_did_not_exist_for_the_one_shot_alert`
isolates this from the one-shot `alertOthers` path above by spawning the neighbour *after* the first
piglin's grudge already started, so the one-shot census (which walks `self.mobs` at the instant of
the hit) could not have reached it — only the ongoing per-tick mechanism can.

### The enderman stare has a real geometry

`isBeingStaredBy(player)` (`monster/EnderMan.java`) is two conditions:

1. `LivingEntity.PLAYER_NOT_WEARING_DISGUISE_ITEM` (`LivingEntity.java`) — false when the
   player's helmet is in `ItemTags.GAZE_DISGUISE_EQUIPMENT`. This is the carved pumpkin.
2. `isLookingAtMe(player, 0.025, true, false, getEyeY())` (`LivingEntity.java`): with
   `look` the player's normalised view vector and `dir` the normalised offset from the player's eyes
   to the enderman, a stare is

   ```
   look.dot(dir) > 1.0 - coneSize / dist
   ```

   plus line of sight. `coneSize` is `0.025` and `adjustForDistance` is `true`, so **the tolerance is
   divided by distance, and the required precision *increases* with range** — the cone narrows, not
   widens. Hand-derived from the formula: at 2 blocks the threshold is `0.5` (up to ~60° off is still
   a stare); at 5 blocks it is `0.8` (only ~37°). The same angular offset that reads as a stare up
   close reads as a near-miss at range — the opposite of the fixed-angle cone an approximation reaches
   for, which would answer identically at both distances.

Two goals consume it. `EndermanFreezeWhenLookedAt` (`monster/EnderMan.java`, flags `{JUMP, MOVE}`) stops the
navigation while a staring player is within `distanceToSqr <= 256.0` (16 blocks) — **now built**
(`crates/lodestone-entity/src/ai/goals.rs`, `EndermanFreezeWhenLookedAt`), a real
`Coverage::Modelled` row in the `ENDERMAN` table below. Its
`can_use` reads `MobController::attack_target` for vanilla's `getTarget() instanceof Player` (every
target this seam ever sets is a player position, so there is nothing else to type-check) and enforces
the 16-block range itself, exactly where vanilla does — the range is **not** folded into
`is_being_stared_at`, which would silently take the minimum of two ranges.
`EndermanLookForPlayerGoal` does the teleporting: when stared at from
`distanceToSqr < 16.0` (4 blocks) it calls `teleport()`; when the target is beyond
`distanceToSqr > 256.0` it calls `teleportTowards` after a delay. It stays `Coverage::Missing` — it
needs its own aggro/teleport state machine layered on the gaze test, not just the boolean.

**`adjustedTickDelay` halves every literal.** `Goal.adjustedTickDelay` is
`requiresUpdateEveryTick() ? ticks : reducedTickDelay(ticks)` (`ai/goal/Goal.java`) and
`reducedTickDelay` is `Mth.positiveCeilDiv(ticks, 2)`. `requiresUpdateEveryTick` defaults
`false` and is overridden by neither `NearestAttackableTargetGoal`, `TargetGoal`, nor
`EndermanLookForPlayerGoal`. So the real values are:

| written in the jar | actual |
|---|---|
| `aggroTime = adjustedTickDelay(5)` | **3** ticks |
| `teleportTime++ >= adjustedTickDelay(30)` | **15** ticks |

`docs/plans/mob-ai-roster.md` §4 quotes the literals `5` and `30`, correctly as literals; anything
transcribing them as tick counts is off by a factor of two.

### Bee sting-then-die is not "die on stinging"

`doHurtTarget` (`animal/bee/Bee.java`) deals `(int)ATTACK_DAMAGE` = 2, applies poison for
0/10/18 seconds by difficulty, then sets `hasStung` and calls
`stopBeingAngry()`. **The bee survives the sting.** Death happens later, in
`customServerAiStep`:

```java
if (hasStung) {
   this.timeSinceSting++;
   if (this.timeSinceSting % 5 == 0
       && this.random.nextInt(Mth.clamp(1200 - this.timeSinceSting, 1, 1200)) == 0) {
      this.hurtServer(level, this.damageSources().generic(), this.getHealth());
   }
}
```

The clamp is what bounds it: at `timeSinceSting == 1200` the divisor is 1, `nextInt(1) == 0` is
always true, and 1200 is a multiple of 5. So **a stung bee is certainly dead within 1200 ticks of
stinging, and certainly alive one tick after it.** Those are the two values a gate should predict;
"the bee dies when it stings" is the wrong hypothesis and it is separated by the tick-after-sting
assertion.

The shape is a drain-flag, not a goal. The precedent in this repo is the creeper fuse: `SwellGoal`
sets a direction, `NavigatingMob::advance` integrates it, `take_detonated()` drains it, and
`MobSim::tick` resolves it into an explosion and a removal. A bee needs the same three pieces plus
access to its own health, which lives on `SimMob`.

## How to change it

Add a species path to `SPECIES` and an arm to `lookup`, then transcribe its `addGoal` block into a
new `const` table and add the species to the module's own multiset gate. Nothing shared changes.

### The five primitives the headline mechanisms wait on

This is the actionable part. Each row is a gap in the AI seam, not in this file:

| # | primitive | landed as | unblocks |
|---|---|---|---|
| 1 | ~~anger deadline + anger target~~ — **landed**, see below | `mobs.rs` (host state + resolution), seam already had the rest | all four species' anger-gated targeting |
| 2 | ~~a gaze test~~ — **landed and fed**, see below | `MobController::is_being_stared_at` + free `is_in_view_cone`; `PlayerPerception::view_direction` now carries a real view vector, fed from `server.rs`'s `PlayerMoved` arm | enderman freeze + stare |
| 3 | ~~instant relocation of a mob~~ — **landed**, see below | `MobController::teleport_to` + `SimMob::teleport_to` | enderman teleport |
| 4 | ~~a mob damaging **itself**~~ — **landed**, see below | `MobController::damage_self`, drained by a `MobSim::tick` arm | bee sting-then-die |
| 5 | ~~an ownership relation~~ — **landed**, see below | `MobController::owner_position` + `SimMob::owner_id`/`owner_uuid`, fed from `PlayerIdentity` at the perception seam | wolf tame half, the owner filter inside pack alert, and the wolf's `OwnerHurtByTargetGoal`/`OwnerHurtTargetGoal` |

Plus a sixth that is not a trait method: **same-species propagation**, which belongs in
`MobSim::feed_perception`'s census rather than in a goal, because the seam deliberately hands a goal
`Option<Vec3>` answers and never a population.

#### Primitive 1, as landed

The row above predicted this would change `ai/mob.rs` and `ai/navigating_mob.rs`. **It did not** —
both already carried `MobController::angry_target` and `NavigatingMob::set_angry_target`, and the
seam had already made the deliberate choice that the *deadline* is the host's: `angry_target` is a
pre-computed **answer**, not a query, because the seam has no shared game clock to compare an
absolute deadline against. The only missing half was the host, so the whole change is in
`lodestone-server/src/mobs/mod.rs`.

- **State.** `SimMob::anger` holds `{ end_time, target }`, where `end_time` is an absolute
  `MobSim::tick_count`. Not a countdown — 26.2 compares against an absolute game time
  (`NeutralMob.java`), and a decrementing counter drifts against a stepped tick loop.
- **Start.** In `MobSim::attack`, beside the existing `note_hurt` — vanilla starts the grudge and
  the retaliation record from the same event.
- **Expiry.** `feed_perception` clears the grudge outright once `now >= end_time` and feeds
  `set_angry_target`, mirroring `stopBeingAngry`.
- **Duration.** `UniformInt.of(400, 780)` **inclusive**, drawn `lo + nextInt(hi - lo + 1)`.
- **No species list.** Anger starts for every mob, reusing the same structural route: only a species
  whose roster registers an anger-gated row can *read* it, so a zombie's unread grudge is inert,
  whereas a name list here would be another `is_hostile_species` waiting to go stale.

**The gotcha, and it cost a vacuous test.** The first version of the gate read its expected bounds
from `ANGER_TICKS`, the constant under test. Setting that constant to `(20, 39)` — the
seconds-as-ticks misreading these tests exist to exclude — left **every assertion passing**, because
the expectation moved with the subject. The bounds are now jar literals stated independently in the
test module, and the control fails as it must. If you touch these tests, keep that separation.

**The anger-gated rows flipped to `Coverage::Modelled` once `NavigatingMob::find_nearest_target`
stopped looping back through `self.attack_target` and started actually searching** — the old
`no_anger_gated_target_row_is_modelled` guard that held them `Missing` for exactly that reason is
gone from this file's tests, replaced by
`tests::anger_gated_target_rows_use_the_anger_gated_search_not_the_open_one`, which proves both the
positive claim and the behavioural property the old guard cared about (a live nearby player with no
grudge is never acquired). The zombified-piglin, wolf and bee anger-gated target rows are real now;
see `roster::neutral`'s own module doc for the citation.

#### Primitives 2-5, as landed

The remaining four landed as `MobController` methods plus `NavigatingMob` overrides and a `MobSim`
host half. Each primitive was necessary but not sufficient at the time it landed — the enderman's
goals, the bee's sting hook, the wolf's tame half and `alertOthers` all needed more, named below (most
of which have since landed too; read each bullet for its current state rather than assuming
`Missing` from this paragraph).

- **Gaze.** `MobController::is_being_stared_at` (a fed boolean) plus the free function
  `is_in_view_cone`, which is vanilla's exact `dot > 1.0 - coneSize / (adjustForDistance ? dist : 1.0)`
  (`LivingEntity.java`) with the line-of-sight half left to the host (the same disclosed gap
  `find_nearest_target` names). `NavigatingMob::set_stared_at` is the feed, and
  `EndermanFreezeWhenLookedAt` (`ai/goals.rs`) is now a real consumer, registered in the `ENDERMAN`
  table as `Coverage::Modelled` and driven through the real roster in
  `roster::neutral::tests::a_stared_at_enderman_freezes_while_an_unwatched_one_at_the_same_spot_closes_in`.
  **The host feed now lands too.** `PlayerPerception::view_direction` (an
  `Entity.calculateViewVector`-shaped unit vector) is fed from
  `lodestone-server/src/server.rs`'s `PlayerMoved` arm — the same `-yaw.sin()*pitch.cos(),
  -pitch.sin(), yaw.cos()*pitch.cos()` formula `block_placement.rs`'s `nearest_look` already uses —
  and `MobSim::feed_perception` (`mobs/mod.rs`) computes the stare per mob per tick from it and the
  connected players' eye positions, calling `NavigatingMob::set_stared_at`. An enderman really does
  freeze when looked at in the running game now; see `docs/lightning.md`'s sibling landing and
  `mobs::primitives_tests::the_gaze_feed_reaches_is_being_stared_at_and_a_look_away_does_not` for the
  server-side gate (a pair at the identical position, differing only in `view_direction`). What is
  *not* modelled at the feed: the carved-pumpkin disguise check (`PlayerPerception` has no armour-slot
  data) and a real line-of-sight raycast (the same disclosed gap `find_nearest_target` already
  carries) — both err permissive, matching every other perception feed here.
- **Teleport.** `MobController::teleport_to` rewrites position instantly and abandons the active path;
  `SimMob::teleport_to` is the host command. No goal calls it yet (the enderman's `teleport()` /
  `teleportTowards` are `Missing`), so it is API, not behaviour.
- **Self-damage.** `MobController::damage_self` records an intent drained by `MobSim::tick` and
  applied through `SimMob::apply_damage` (i-frames and reductions included, matching vanilla's
  `hurtServer`). The bee's `hasStung` flag, `timeSinceSting` counter and `customServerAiStep` roll are
  **not** implemented — the primitive is the pipeline, not the bee.
- **Ownership.** `MobController::owner_position` (a fed position) plus `SimMob::owner_id`/`owner_uuid`
  and `set_owner_id`/`set_owner`; `feed_perception` resolves the id to a position each tick. The
  **player** half landed too: `PlayerIdentity` at the perception seam gives `MobOwner::Player(Uuid)`
  a real identity to carry, which is what the wolf-pack same-owner filter inside
  `HurtByTargetGoal.alertOthers` compares (`MobSim::attack`'s pack-alert census) and what the wolf's
  `OwnerHurtByTargetGoal`/`OwnerHurtTargetGoal` target-selector rows read — see
  `roster::neutral::WOLF`'s own table doc for how those two resolve to real producers
  (`MobSim::attack_from_player` for "the owner just hit this", and the hostile-melee-against-player
  resolution inside `MobSim::tick` for "this just hit the owner").

### Gotchas

- **The family is not uniform, and the exception is the interesting one.** `ZombifiedPiglin`
  declares no `registerGoals`; it overrides `addBehaviourGoals`, the hook
  `Zombie.registerGoals` calls at `Zombie.java`. Its table is Zombie's three own rows
  **plus** its six, and because the override does not call `super`, Zombie's
  `MoveThroughVillageGoal` is *dropped* and `SpearUseGoal`/`ZombieAttackGoal` renumber from 2/3 to
  1/2. `the_piglin_inherits_three_rows_and_drops_one_from_its_parent` pins all of that.
- **Never register the plain `NearestAttackableTargetGoal` constructor for one of these three
  species — only the `anger_gated` form.** A jar signature ending in `isAngryAt` is the entire
  difference between a neutral mob and a hostile one, and the plain constructor takes no predicate,
  so a `Modelled` row built from it makes zombified piglins, wolves and bees attack on sight.
  `NavigatingMob::find_nearest_target` now really does search (it no longer loops back through
  `self.attack_target`), so this is a live hazard, not a latent one.
  `tests::anger_gated_target_rows_use_the_anger_gated_search_not_the_open_one` is the guard.
- **`attribute.rs`'s `type_spec` now has arms for all four**, so they spawn at the
  jar's 0.3/0.23/0.3/0.3 rather than the registry's 0.7. Every rostered species now resolves, pinned
  structurally by `every_rostered_species_has_a_type_spec_arm`. The paragraph below describes the
  prior state and is kept for the reasoning, not the status: it used to mean four arms in
  `attribute.rs`.
- **These species have no production spawn path.** `seed_demo_mobs` hardcodes `minecraft:zombie`, and
  it is the only production caller of `spawn_species`. So a neutral mob can be spawned and ticked in
  a test but reaches **zero pixels** today. That is unit A4's territory, not this file's, but it
  means "the roster is right" and "a player can see a wolf" are different claims.
- **Do not claim `llama`** (nor `panda`/`polar_bear` without checking first). They are neutral mobs
  and belong here eventually, but `llama` is the *rosterless* control in several existing gates,
  every one of which asserts `is_fallback(registrations_for("llama"))`. Claiming it turns those
  controls red.
- **The behavioural gates in this module detect which rows a species has, not their priority
  numbers.** This was measured, not assumed: transcribing the wolf's panic row at 6 instead of 1
  (below melee's 5) leaves them green, because `PanicGoal::is_interruptable()` is `false` and panic
  precedes melee in table order, so once it holds MOVE no priority dislodges it. A second mutation —
  the piglin's melee at 9, below its stroll at 7 — is likewise invisible, because
  `RandomStrollGoal`'s interval roll lets melee re-take MOVE the next tick. **The priority guard is
  the multiset gate against the jar**, which caught both mutations immediately and named the row.

## Configuration

None. The tables are `const` and take no feature flag. Every constant is a cited vanilla value.

## Dependencies

- `lodestone_entity::ai::{goal, goals}` — the scheduler and the goal implementations. A species can
  only be transcribed as far as the goals that exist; `Coverage::Missing` records the rest.
- `lodestone_entity::ai::roster` — `Registration`, `Selector`, `Coverage`, `SpeciesContext` and the
  shared builders, all from unit A3.
- `lodestone_server::mobs` — the only consumer, via `MobSim::spawn_species`, and the place the
  propagation census and the bee's self-damage resolution would have to go.
- `.cache/mc/26.2/src/net/minecraft/world/entity/` — the authority for every priority, multiplier and
  timing here.

## Related

- [`mob-goal-roster.md`](./mob-goal-roster.md) — the roster seam this family plugs into.
- [`mob-perception.md`](./mob-perception.md) — what a goal is allowed to know, and why
  `last_hurt_by` being fed is what makes retaliation work here.
- [`plans/mob-ai-roster.md`](./plans/mob-ai-roster.md) — the epic plan; this is unit B4.
