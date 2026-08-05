# Mob neutral roster: the grudge-holding species

## What it is

The `neutral` family of the per-species goal roster — enderman, zombified piglin, bee and wolf —
plus the record of why the four mechanisms issue [#233](https://github.com/matteopolak/lodestone/issues/233)
names in its title are **not** implemented as goals. The tables are complete transcriptions of the
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

cited at `monster/EnderMan.java:83`, `monster/zombie/ZombifiedPiglin.java:58`,
`animal/bee/Bee.java:129`, `animal/wolf/Wolf.java:117`. `TimeUtil.rangeOfSeconds(min, max)` is
`UniformInt.of(min * 20, max * 20)` (`util/TimeUtil.java:13-15`) and `UniformInt::sample` is
`Mth.randomBetweenInclusive` (`util/valueproviders/UniformInt.java:28-30`), so the grudge is
**uniform on [400, 780] ticks inclusive**. Two plausible-but-wrong readings to avoid: `[20, 39]`
(seconds mistaken for ticks) and a half-open upper bound.

**26.2 stores an absolute game-time deadline, not a countdown.**
`setTimeToRemainAngry(remaining)` writes `level.getGameTime() + remaining`
(`NeutralMob.java:20-22`), `isAngry()` compares that against the clock (`:112-120`), and
`NO_ANGER_END_TIME` is `-1L` (`:16`). Porting the pre-26.2 `remainingPersistentAngerTime` countdown
would drift against a paused or stepped tick loop. The wolf additionally keeps its deadline in
**entity metadata** (`DATA_ANGER_END_TIME`, `Wolf.java:560`), so anything putting it on the wire must
run `crates/protocol/v770/oracle-java/EntityDataIndexOracle.java` rather than hand-counting.

### Propagation is a box, and it is not a goal

Three species chain `.setAlertOthers()` onto their `HurtByTargetGoal`. That primitive
(`ai/goal/target/HurtByTargetGoal.java:72-111`) inflates the mob's position by
`(followRange, 10.0, followRange)` — an **axis-aligned box with a flat vertical half-extent of 10**
(`ALERT_RANGE_Y = 10`, `:20`) — takes only entities of the mob's **own class**, and requires
`other.getTarget() == null` and `!other.isAlliedTo(attacker)`. For a `TamableAnimal` it also requires
**`tamable.getOwner() == other.getOwner()`** (`:88`).

| species | XZ half-extent | Y half-extent | source of the XZ figure |
|---|---|---|---|
| zombified piglin | 35.0 | 10.0 | `FOLLOW_RANGE`, inherited from `Zombie.java:133` |
| wolf | 16.0 | 10.0 | `Mob.createMobAttributes()` default, `Mob.java:167` |
| bee | 16.0 | 10.0 | same, via `Animal.createAnimalAttributes()` |

The zombified piglin has a **second, independent** propagation path that is easy to conflate with the
first: its own `alertOthers` (`ZombifiedPiglin.java:139-149`), same box shape, but driven from
`customServerAiStep` (`:112`) rather than from a goal's `start()`, throttled by
`ALERT_INTERVAL = TimeUtil.rangeOfSeconds(4, 6)` = **[80, 120] ticks** (`:62`, sampled `:135`), and
gated on line of sight to the current target (`:131`). So "piglin group aggro" is a per-tick census,
not a `Registration` — modelling it means an arm in `MobSim::tick`, next to `feed_perception`.

### The enderman stare has a real geometry

`isBeingStaredBy(player)` (`monster/EnderMan.java:209-211`) is two conditions:

1. `LivingEntity.PLAYER_NOT_WEARING_DISGUISE_ITEM` (`LivingEntity.java:212-215`) — false when the
   player's helmet is in `ItemTags.GAZE_DISGUISE_EQUIPMENT`. This is the carved pumpkin.
2. `isLookingAtMe(player, 0.025, true, false, getEyeY())` (`LivingEntity.java:1756-1775`): with
   `look` the player's normalised view vector and `dir` the normalised offset from the player's eyes
   to the enderman, a stare is

   ```
   look.dot(dir) > 1.0 - coneSize / dist
   ```

   plus line of sight. `coneSize` is `0.025` and `adjustForDistance` is `true`, so **the tolerance is
   divided by distance and the acceptance cone widens with range** — the opposite of the fixed-angle
   cone an approximation reaches for.

Two goals consume it. `EndermanFreezeWhenLookedAt` (`:401-430`, flags `{JUMP, MOVE}`) stops the
navigation while a staring player is within `distanceToSqr <= 256.0` (16 blocks).
`EndermanLookForPlayerGoal` (`:484-574`) does the teleporting: when stared at from
`distanceToSqr < 16.0` (4 blocks) it calls `teleport()`; when the target is beyond
`distanceToSqr > 256.0` it calls `teleportTowards` after a delay.

**`adjustedTickDelay` halves every literal.** `Goal.adjustedTickDelay` is
`requiresUpdateEveryTick() ? ticks : reducedTickDelay(ticks)` (`ai/goal/Goal.java:49-51`) and
`reducedTickDelay` is `Mth.positiveCeilDiv(ticks, 2)` (`:53-55`). `requiresUpdateEveryTick` defaults
`false` (`:28-30`) and is overridden by neither `NearestAttackableTargetGoal`, `TargetGoal`, nor
`EndermanLookForPlayerGoal`. So the real values are:

| written in the jar | actual |
|---|---|
| `aggroTime = adjustedTickDelay(5)` (`:509`) | **3** ticks |
| `teleportTime++ >= adjustedTickDelay(30)` (`:565`) | **15** ticks |

`docs/plans/mob-ai-roster.md` §4 quotes the literals `5` and `30`, correctly as literals; anything
transcribing them as tick counts is off by a factor of two.

### Bee sting-then-die is not "die on stinging"

`doHurtTarget` (`animal/bee/Bee.java:224-249`) deals `(int)ATTACK_DAMAGE` = 2, applies poison for
0/10/18 seconds by difficulty (`:231-240`), then sets `hasStung` (`:243`) and calls
`stopBeingAngry()` (`:244`). **The bee survives the sting.** Death happens later, in
`customServerAiStep` (`:374-379`):

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

| # | primitive | file that must change | unblocks |
|---|---|---|---|
| 1 | anger deadline + anger target, with `advance()` integrating it | `ai/mob.rs` (trait method) + `ai/navigating_mob.rs` (state) | all four species' anger-gated targeting |
| 2 | a gaze test — "is that player looking at me", needing the player's **view vector**, which `PlayerPerception` does not carry | `ai/mob.rs` + `mobs.rs` (feed) | enderman freeze + stare |
| 3 | instant relocation of a mob | `ai/mob.rs` + `ai/navigating_mob.rs` (must also invalidate the active path) | enderman teleport |
| 4 | a mob damaging **itself** (drain-flag shape) | `ai/navigating_mob.rs` + a `MobSim::tick` arm | bee sting-then-die |
| 5 | an ownership relation, which needs a **player identity** the seam has none of | `ai/mob.rs`, `mobs.rs`, `PlayerPerception` | wolf tame half, and the owner filter inside pack alert |

Plus a sixth that is not a trait method: **same-species propagation**, which belongs in
`MobSim::feed_perception`'s census rather than in a goal, because the seam deliberately hands a goal
`Option<Vec3>` answers and never a population.

### Gotchas

- **The family is not uniform, and the exception is the interesting one.** `ZombifiedPiglin`
  declares no `registerGoals`; it overrides `addBehaviourGoals` (`:71-78`), the hook
  `Zombie.registerGoals` calls at `Zombie.java:116`. Its table is Zombie's three own rows
  (`:113-115`) **plus** its six, and because the override does not call `super`, Zombie's
  `MoveThroughVillageGoal` is *dropped* and `SpearUseGoal`/`ZombieAttackGoal` renumber from 2/3 to
  1/2. `the_piglin_inherits_three_rows_and_drops_one_from_its_parent` pins all of that.
- **Do not model a target row whose jar signature ends in `isAngryAt`.** That predicate is the entire
  difference between a neutral mob and a hostile one. Our `NearestAttackableTargetGoal` takes no
  predicate, so a `Modelled` row there makes zombified piglins, wolves and bees attack on sight.
  It is currently *latent* rather than active only because `NavigatingMob::find_nearest_target`
  returns `self.attack_target` instead of searching (`ai/navigating_mob.rs:904-906`) — the day that
  self-loop is fixed, a `Modelled` row here turns three neutral species hostile.
  `no_anger_gated_target_row_is_modelled` is the guard.
- **`attribute.rs`'s `type_spec` has no arms for these four species**, so they currently spawn with
  the generic registry `movement_speed` default rather than the jar's 0.3/0.23/0.3/0.3. Every speed
  multiplier in the tables is therefore correct but scaled against the wrong base in production. The
  speed gate feeds the jar's value explicitly and says so; fixing it means four arms in
  `attribute.rs`.
- **These species have no production spawn path.** `seed_demo_mobs` hardcodes `minecraft:zombie`, and
  it is the only production caller of `spawn_species`. So a neutral mob can be spawned and ticked in
  a test but reaches **zero pixels** today. That is unit A4/#224's territory, not this file's, but it
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
