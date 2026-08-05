# Mob goal roster

## What it is

The per-species goal-set seam: a pure, world-free lookup from a species path
(`"cow"`, `"creeper"`) to the list of `Goal`s vanilla's own `registerGoals()`
installs for it, at vanilla's own priority numbers. `MobSim::spawn_species`
consults it with one loop, so no per-species behaviour data lives in
`lodestone-server` any more, and five parallel roster units can each own one file
without touching a shared one.

Code: `crates/lodestone-entity/src/ai/roster/`. Entry point
`lodestone_entity::ai::roster::goals_for`. Gates:
`crates/lodestone-server/tests/mob_roster.rs` (production path) plus the module's
own `#[cfg(test)]` blocks (jar transcription).

## How it works

### The table

Each species resolves to a `&'static [Registration]`. A `Registration` is one
vanilla `addGoal` call, transcribed:

```rust
Registration::goal(2, "SwellGoal", swell)                      // we build it
Registration::missing(Selector::Goal, 3, "LeapAtTargetGoal")   // we have no such goal
Registration::covered(Selector::Goal, 3, "AvoidEntityGoal(Cat)",
                      "AvoidEntityGoal(Ocelot)")               // a sibling row covers it
```

Three properties come from that shape, and each one exists to stop a specific
kind of wrong:

- **`vanilla` is the jar's class name**, so a test can compare a whole table
  against a hand-transcribed copy of a cited `File.java:line`. The expected value
  originates outside the code under test — comparing against numbers read out of
  this crate would be a closed loop.
- **`Coverage::Missing` rows stay in the table at their real vanilla priority.**
  An absent row is indistinguishable from a vanilla registration nobody noticed;
  a present one turns "implement `LeapAtTargetGoal`" into a one-row edit the
  multiset gate already covers.
- **`Coverage::CoveredBy` is not the same as `Missing`.** Several of our goals are
  class-agnostic where vanilla's are generic over a target class: a creeper has
  two `AvoidEntityGoal` registrations (`Ocelot`, `Cat`) and our `AvoidEntityGoal`
  flees whatever the server's `avoided_species` feed reports, which is already
  both. Building two would give the creeper two goals fighting over MOVE at equal
  priority.

### Speeds are multipliers

Vanilla's `addGoal` speed arguments multiply the mob's own `MOVEMENT_SPEED`:
`PanicGoal(this, 2.0)` on a cow means twice that cow's walking speed. Our goals
take an absolute blocks-per-tick figure, so each builder is `ctx.speed *
<the jar's factor>` with the factor visible next to its citation. A flattened
absolute number could not be checked against the jar.

### One selector, two namespaces

Vanilla gives every mob **two** selectors with independent priority numbering —
`goalSelector` and `targetSelector` — and `SimMob` holds **one** `GoalSelector`.
Unit A3 resolved that in favour of keeping one, and the reasoning is load-bearing
rather than convenience:

- A priority number is only ever compared against a goal contending for the *same
  flag*. Vanilla's target goals are exactly the goals whose flag set is
  `{TARGET}`, and no other goal claims TARGET, so the two namespaces cannot
  collide when flattened. The jar's numbers are kept verbatim, with no offset
  convention to misremember.
- That is an invariant, so it is **gated**, not assumed:
  `roster::tests::target_and_goal_namespaces_cannot_contend` asserts every roster
  goal has flags either equal to `{TARGET}` or disjoint from it. The day a goal
  wants MOVE *and* TARGET, that gate fails and `MobAi` (which models the pair, and
  is kept in the tree for exactly this) becomes the answer.
- Vanilla ticks the target selector first so a target acquired this tick is
  visible to a movement goal the same tick. `goals_for` emits every
  target-selector registration ahead of every goal-selector one, and
  `GoalSelector` iterates in insertion order, which reproduces that.

### `GoalSelector::remove`

`add` now returns a `GoalId`; `remove(id, mob)` stops the goal if running (firing
its `stop` hook, as vanilla's `removeAllGoals` does) and drops it, rewriting the
flag-lock table's indices. This exists for vanilla's runtime goal swap:
`AbstractSkeleton.reassessWeaponGoal()` removes both its melee and bow goals and
re-adds exactly one at priority 4 whenever the held item changes
(`monster/skeleton/AbstractSkeleton.java:132-146`). `disable` cannot express it —
it is per-`Flag` and would take out every MOVE goal the mob has.

Handles rather than indices, because removal shifts every later index down; an
index captured before a removal silently names a different goal after it. Use
`is_running_id` across a removal, not `is_running`.

## How to change it

**Adding a species: edit one file.** Pick the family module your species belongs
to, add its path to that module's `SPECIES`, and add an arm to its `lookup`. All
five family modules — `hostile_melee`, `ranged`, `passive`, `neutral`,
`specialist` — are already declared and already in `roster::FAMILIES`, so nothing
shared changes: no `mod` line, no registration list, no `mobs.rs` arm.

| module | issue | claims today |
|---|---|---|
| `hostile_melee.rs` | #226 | zombie, husk, creeper, spider, cave spider, skeleton family |
| `passive.rs` | #228 | cow, mooshroom, sheep, pig, chicken |
| `ranged.rs` | #227 | — (pre-registered, empty) |
| `neutral.rs` | #233 | — (pre-registered, empty) |
| `specialist.rs` | #232 | — (pre-registered, empty) |

`SPECIES` is not decoration: `roster`'s invariant gates iterate it, so a species
missing from it is a species nothing checks, and
`every_advertised_species_resolves_to_a_real_table` fails when the two disagree.

### Gotchas

- **Cite the jar, never memory or another table in this crate.** A priority copied
  from a neighbouring species passes every structural check. The multiset gate
  compares against a transcription of a cited `.java` line, and its whole value is
  that you must re-read the citation to change it.
- **A priority-only gate cannot see a wrong speed.** A cow's `TemptGoal` built
  with sheep's `1.1` sits at the right priority under the right name and still
  moves the cow toward the player — direction is preserved, magnitude is not.
  `passive::tests::every_speed_matches_the_jars_multiplier` predicts the value
  (`0.2 × 1.25 = 0.25`) and measures what the goal asks the mob for, via
  `roster::probe::SpeedProbe`.
- **`registerGoals` is not always one method.** A zombie's registrations are split
  across `registerGoals` (`Zombie.java:112-116`) and `addBehaviourGoals`
  (`:119-130`), and a subclass may add rows before calling `super`
  (`WitherSkeleton.java:38-41`). Grep `addGoal` in the whole file, and check
  whether the subclass overrides at all — `Husk`, `MushroomCow`, `Skeleton`,
  `Stray`, `Bogged` and `CaveSpider` do not, which is why they share a table.
- **Do not test a goal against `ScriptMob` and call it done.** That fake overrides
  all eight perception methods, which is exactly how issue #441's island survived:
  every affected goal had a green unit test while its `can_use` was a compile-time
  constant `false` in production. A roster gate must drive
  `MobSim::spawn_species`.
- **Do not add a goal in the test you are using to check the roster.** A test that
  installs `TemptGoal` and then observes tempting cannot see whether the roster
  installed anything.
- **`roster::probe::SpeedProbe` answers `Some`/`true` to every perception method
  by design.** It is for reading arguments back, never for asking whether a goal
  *should* run.

## Configuration

None. The tables are `const`, resolved at compile time, and take no feature flag.

## Dependencies

- `lodestone_entity::ai::{goal, goals}` — the scheduler and the goal
  implementations. A species can only be transcribed as far as the goals that
  exist; `Coverage::Missing` records the rest.
- `lodestone_server::mobs` — the only consumer. `MobSim::spawn_species` calls
  `goals_for`; `MobSim::tick` feeds the perception the goals read
  (`avoided_species`, `tempt_food`, nearest player, panic, breeding candidates).
- `.cache/mc/26.2/src/net/minecraft/world/entity/` — the authority for every
  priority and every speed multiplier here.

## What this deliberately does not carry

- **`MobCategory` and hostility.** Two independent types by that name exist in
  this workspace (`lodestone_entity::spawn::MobCategory`, 8 variants, and
  `lodestone_server::mob_spawn::MobCategory`, 7 variants and a different
  `check_despawn` signature); the server uses its own and unifying them is issue
  #221's call. The roster is keyed on the species path string and returns goals
  only, so it takes no side. `is_hostile_species` stays in `mobs.rs`, reduced to
  spawn category and despawn persistence.
- **Perception data.** "Which species does a spider flee", "which items tempt a
  pig" describe what a mob can *see*, are fed through `MobController` by the
  server's own census, and live next to that feed in `mobs.rs`. The roster only
  decides that a spider gets an `AvoidEntityGoal` at all.

## Known gaps

- **`MobSim::run_spawn_cycle` still installs a hardcoded stroll/look pair** rather
  than consulting the roster. It has no production caller (only
  `tests/mob_spawn.rs`), and it spawns through `MobSim::spawn`, which hardcodes
  `minecraft:zombie` — so routing it through the roster would hand every naturally
  spawned mob the zombie set. That belongs with the natural-spawn driver, issue
  #222.
- **Two of our goals' flag sets differ from vanilla's**, measured against the jar
  and left alone here because changing them alters scheduling for every species at
  once: `MeleeAttackGoal` claims `{MOVE}` where vanilla claims `{MOVE, LOOK}`
  (`ai/goal/MeleeAttackGoal.java`), so a mob can turn its head to look at a player
  while attacking; and `FollowParentGoal` claims `{MOVE}` where vanilla claims
  **nothing** (`ai/goal/FollowParentGoal.java` calls no `setFlags`), so ours
  preempts strolling instead of racing it. Ours is the more conservative choice in
  both cases — fewer concurrent writers to the navigator — but both are
  discrepancies, not decisions, and belong to whichever unit owns the goal.
- **`wither_skeleton` shares the base skeleton table** while vanilla gives it one
  extra target registration (`WitherSkeleton.java:38-41`). That row could only ever
  be `Missing` today, so it changes no behaviour, but the table is knowingly not a
  complete transcription and the multiset gate excludes it.
