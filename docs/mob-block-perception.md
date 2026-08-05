# Mob block perception

## What it is

The seam that lets a mob AI goal ask what block it is standing on. `MobController`
declared 33 methods and not one read a block, so every vanilla goal whose
predicate consults the world was inexpressible — a sheep that eats grass could
not ask whether there was grass (issue #456). Goals now pull `BlockCues` through
the controller, which the production host answers from the `PathWorld` it already
borrows for pathfinding.

## How it works

Three pieces, in the order data flows:

1. **`PathWorld::block_cues(x, y, z) -> BlockCues`**
   (`crates/lodestone-entity/src/pathfinding/world.rs`). The host adapter
   classifies one block into the small set of *predicates* goals branch on.
   Defaults to `BlockCues::NONE`, so the 13 existing `PathWorld` implementors
   compile unchanged.
2. **`MobController::block_cues_at_feet` / `block_cues_below`**
   (`crates/lodestone-entity/src/ai/mob.rs`). The two positions
   `EatBlockGoal` distinguishes, and the only two any of the goals in question
   read. `NavigatingMob` overrides both by asking `self.world`.
3. **`MobController::ate(EatenBlock)`** — the *intent* for the resulting world
   mutation, drained by the host with `NavigatingMob::take_new_eaten`, exactly
   like `attack` and `launch_projectile`. This crate can write neither a block
   nor entity metadata.

`BlockCues` is a struct of booleans, not an enum and not a block id, because
vanilla's own tests are predicates over **tags** that can hold simultaneously:
`state.is(BlockTags.EDIBLE_FOR_SHEEP)` beside `state.is(Blocks.GRASS_BLOCK)`
(`ai/goal/EatBlockGoal.java:16,34`). A block id would drag a registry into
`lodestone-entity`, which the `PathWorld` seam exists to avoid.

### Why a query and not a per-tick feed

Every other perception method on `MobController` is a value the server's census
pushes in once per tick. A pre-fed block snapshot would have matched that shape
and cost about three orders of magnitude more work than the goals need:
`EatBlockGoal` is the only reader and its `can_use` consults a block on roughly
**one tick in 500** (`random.nextInt(adjustedTickDelay(1000))`). Pushing two
block lookups per mob per tick to serve that multiplies by the whole mob
population; pulling them costs nothing on the 499 ticks nobody asks.

The usual objection to a query — that a world handle on the trait changes its
object-safety story and puts a lifetime through everything — does not apply,
because **the handle is not on the trait**. `NavigatingMob` already borrows
`&'w dyn PathWorld`, so the override needs no new parameter and no new lifetime,
and a test double still answers from a plain field. This is neither of the two
options issue #456 posed, and cheaper than both.

## The seven `Missing` rows: what the seam actually closes

Issue #456's headline is that *seven* unmodelled roster rows across two families
are one missing capability. Measured against the jar, that is **not right**, and
the correction is the useful part. Only one row is closed by block perception
alone; one needs no block access at all; the remaining five each need a second,
larger mechanism, and three of those five share it.

| row | what it really needs | closed by this seam? |
|---|---|---|
| sheep `EatBlockGoal` | two local cues (`ai/goal/EatBlockGoal.java:33-34`) | **yes** |
| skeleton `RestrictSunGoal` | **no block read at all** — `level.isBrightOutside()` plus an empty HEAD slot (`RestrictSunGoal.java:17`). Its *effect* is `GroundPathNavigation.setAvoidSun(true)`, i.e. a sky-light penalty in the path evaluator | no — a pathfinder feature |
| skeleton `FleeSunGoal` | `isBrightOutside`, `mob.isOnFire()`, `canSeeSky(pos)` (a column query, not a neighbour cue), and 10 random probes for a shaded position (`FleeSunGoal.java:64-73`) | no — needs a host-computed candidate position |
| zombie `ZombieAttackTurtleEggGoal` | `RemoveBlockGoal` over a 24-block spiral with vertical range 3, plus block-break progress and a destroy intent (`Zombie.java:113`, `RemoveBlockGoal.java`) | no — candidate position + destroy intent |
| rabbit `RaidGardenGoal` | `MoveToBlockGoal(0.7, 16)` spiral, the `#supports_crops` tag, and `CarrotBlock.AGE` — a block-state **property**, not a boolean cue (`Rabbit.java:593-665`) | no — candidate position + state properties |
| rabbit `ClimbOnTopOfPowderSnowGoal` | one local cue (block above is powder snow or has empty collision) **and** `mob.isInPowderSnow`/`wasInPowderSnow`, which no physics here sets (`ClimbOnTopOfPowderSnowGoal.java:24-29`) | cue yes, goal no — blocked on powder-snow physics |
| zombie `MoveThroughVillageGoal` | village/POI and door structure lookup | no — nothing here resembles it |

The regrouping that matters: **three rows (`FleeSunGoal`'s hide position,
`ZombieAttackTurtleEggGoal`, `RaidGardenGoal`) want the same second mechanism** —
a host-computed *candidate block position*, the `MoveToBlockGoal` shape, in the
`partner_candidate`/`parent_candidate` style already on `NavigatingMob`. That
belongs on the census side, because the search is over a population of blocks and
this seam hands goals answers rather than queries. Do **not** try to serve it by
widening `block_cues`: `RemoveBlockGoal`'s search box alone is 49×49 columns ×
7 y-levels, up to ~17k positions per attempt, and pre-feeding anything like it
would put a per-tick cost on every mob for a goal two species register.

## How to change it

* **Adding a cue.** Add a field to `BlockCues`, answer it in the host's
  `PathWorld` impl, and cite the jar predicate in the doc comment. Do not add one
  speculatively — a cue nothing reads is a cost the host pays for nothing.
* **Adding a mutation.** Add a variant to `EatenBlock` (or a sibling intent
  enum), state the vanilla mutation on the variant, and drain it in the host.
  Note vanilla calls `mob.ate()` even when the `mobGriefing` gamerule suppresses
  the block change (`EatBlockGoal.java:64-68`), so the gamerule check belongs on
  the host side, not in the goal.
* **The gotcha that cost the most time here.** Every tick constant in
  `EatBlockGoal` is **halved**: `Goal.adjustedTickDelay` is
  `Mth.positiveCeilDiv(t, 2)` for any goal that does not override
  `requiresUpdateEveryTick`, and this one does not (`ai/goal/Goal.java:53-55`).
  The jar's `1000`, `50`, `40`, `4` are 500, 25, 20 and 2 ticks in practice.
  Transcribing the unhalved numbers makes a sheep graze half as often and hold
  the animation twice as long, which no test asserting "it eventually ate" would
  catch — `the_grazing_interval_is_the_halved_delay_and_not_the_jar_literal`
  distinguishes the two hypotheses by count.
* **The other gotcha: grazing depletes its own supply.** A sheep that eats turns
  its column to dirt and then cannot graze again until it wanders to fresh grass,
  so an observed *rate* in a mutating world is bounded by how fast a 0.23
  blocks/tick animal finds grass, not by the goal's interval. Measure intervals
  with mutations disabled.

## What is not reached yet

Two links are in files owned elsewhere and ship as separate patches. Until both
land, **a sheep in a running game does not graze**:

1. `crates/lodestone-server/src/mobs.rs` — `ChunkWorld` must implement
   `block_cues`, and `MobSim::tick` must drain `take_new_eaten` and perform the
   mutation plus the species' `ate()` effects.
2. `crates/lodestone-entity/src/ai/roster/passive.rs` — the sheep's
   `EatBlockGoal` row must flip from `Coverage::Missing` to `Modelled`.

Issue #238 (sheep grazing) additionally needs `Sheep.ate()`'s wool regrowth —
`setSheared(false)` plus `ageUp(60)` — which is entity metadata on the wire.

## Configuration

None. No feature flag, no env var. `BlockCues::NONE` is the default answer at
both the world and the controller seam, which means a host that classifies
nothing leaves every cue-reading goal inert **and nothing fails** — the failure
mode this doc exists to make visible.

## Dependencies

* `crates/lodestone-entity/src/pathfinding/world.rs` — `PathWorld`, `BlockCues`.
* `crates/lodestone-entity/src/ai/mob.rs` — `MobController`, `EatenBlock`.
* `crates/lodestone-entity/src/ai/navigating_mob.rs` — the production override
  and the `take_new_eaten` drain.
* `crates/lodestone-entity/src/ai/goals.rs` — `EatBlockGoal`, the only reader.
* `crates/lodestone-entity/tests/block_perception.rs` — the gate, including the
  stone control and the inert-world control.
