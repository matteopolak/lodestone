# Mob AI roster: per-species goal-sets and brains

## What it is

The implementation plan for GitHub epic [#225](https://github.com/matteopolak/lodestone/issues/225) and
its children #226–#233 — assembling real per-species goal sets and Brain behaviour sets on top of
`lodestone-entity`'s existing `GoalSelector`/`Brain` infrastructure. Its central finding is that the
roster **cannot be built first**: eight of the thirteen implemented goals are structurally incapable
of firing in production today, so this plan sequences a perception-and-driver spine ahead of every
species unit.

**Status note, 2026-08-17: C2 (#230, Brain passive roster) partially landed, not closed.** Of the
seven species #230 names, four now have a real, wired, tested behaviour beyond the generic CORE+IDLE
scaffold: goat (ram attack, landed earlier), armadillo (roll-up-on-threat, landed earlier), axolotl
(play-dead-on-low-health) and frog (tongue attack) landed this pass, and allay (item-carry-and-deliver,
note-block hearing via the warden's own vibration substrate, and a disclosed-substitute duplication
arm) landed this pass too. **Two gaps remain and #230 stays open**: camel has sitting but not the
rider-triggered dash (needs a `PlayerInput` jump-bit decode this crate currently discards, plus new
vehicle-physics wiring — out of scope this pass), and sniffer has none of its dig/seek/egg state
machine at all (a much larger unit — a block-interaction/digging mechanic, an egg-spawn producer, and
a multi-phase timer, none of which exist yet). See `brain/roster.rs`'s own module doc and
`crate::mobs::allay_carrying_tests` (`lodestone-server`) for the per-species detail. Two previously-
recorded blockers in #230's own issue thread were re-verified and found false this pass: "frog tongue
needs slime simulation" (vanilla's own eat-check for the frog is a plain entity-type/size check, no
AI dependency) and "allay
note-block duplication has nowhere for the pulse to land" (the warden's vibration substrate already
had room for a second listener; `crate::redstone_note_block::played_pulse_on_transition` re-derives
the pulse from a `RandomTickEvent`'s existing `(from, to)` pair with no new field).

**Status note, 2026-08-15: B4 (#233, neutral/aggro) landed.** All four headline mechanisms — enderman
teleport-on-stare, zombified piglin group aggro, bee sting-then-die, wolf pack aggro — are
`Coverage::Modelled` and tested against a real `NavigatingMob`. See §3's B4 entry for the summary and
`roster::neutral`'s own module doc for the full account. §4's enderman `adjustedTickDelay` literals
below are corrected inline rather than superseded separately, since the fix is small and local.

**Status note, superseded 2026-08-14 by a real re-verdict pass (not just a citation fix) — see
§1.2 and §1.4 inline for the details.** The original note below (added during a citation pass,
explicitly *not* a status audit) already found `crates/lodestone-entity/src/ai/roster/` real and
wired. The 2026-08-14 pass went further and re-checked every remaining "TRUE"/"does not exist"
claim in §1.2 and two more in §1.4 against the tree: **six of the eight islands this plan's §1
originally catalogued have since closed** — natural spawning now has a driver
(`natural_spawn.rs`, wired into the production tick loop), regional difficulty is modelled
(`regional_difficulty.rs`), spawn eggs work, ranged goals exist and fire real projectiles in
production (`ai/roster/ranged.rs` + `MobSim::tick`'s launch drain), and Brain AI is now reachable
in production because natural spawning's species table overlaps `BRAIN_SPECIES`. Two did **not**
close: `SpawnEnvironment` still has zero implementors, and spawner blocks are still unmodelled.
Original note, kept for its own history: `crates/lodestone-entity/src/ai/roster/` now exists with
`hostile_melee.rs`, `passive.rs`, `ranged.rs`, `neutral.rs`, `specialist.rs` and `probe.rs`, and
`roster::goals_for` is wired into `MobSim::spawn_species`
(`crates/lodestone-server/src/mobs/mod.rs`); `crates/lodestone-server/src/mobs.rs` itself no
longer exists (split into `crates/lodestone-server/src/mobs/`); the two `MobCategory` types were
unified; projectile hit detection exists.

---

## 1. Verified state, and what the issues get wrong

Everything in this section was re-derived from the tree on 2026-08-04 with `/usr/bin/grep`, per
CLAUDE.md rule 2. Several issue bodies are stale; each is called out so a unit author does not route
around a problem that no longer exists, or plan against a citation that was never true.

### 1.1 The fifth island — perception starvation (not filed as any issue)

This is the largest finding in this plan and it is in no issue, no doc, and no roadmap entry.

`NavigatingMob` was, at the time of writing, the only production implementor of `MobController`
(`crates/lodestone-entity/src/ai/navigating_mob.rs`). `MobController`
(`crates/lodestone-entity/src/ai/mob.rs`) declares 33 methods, 22 of them with defaults.
`NavigatingMob` overrode 24 and **left 8 perception methods at their trait defaults** at the time:

**Update, confirmed directly against the current source:** `NavigatingMob` now overrides all
8 of these (`in_water`, `in_lava`, `no_action_time`, `nearest_player`, `last_hurt_by`,
`temptation`, `avoid_threat`, `is_panicking` all have real implementations in
`navigating_mob.rs`), consistent with the status note in "What it is" — Unit A1 (the
perception seam) appears to have landed. The table below is kept as the historical record of
what was missing when this section was written:

| method | default | overridden by `NavigatingMob` at the time? |
|---|---|---|
| `in_water` | `false` | no |
| `in_lava` | `false` | no |
| `no_action_time` | `0` | no |
| `nearest_player` | `None` | no |
| `last_hurt_by` | `None` | no |
| `temptation` | `None` | no |
| `avoid_threat` | `None` | no |
| `is_panicking` | `false` | no |

At the time of writing, six goals gated `can_use` on exactly those methods, so their `can_use` was a
compile-time constant
`false` in production. **Per the update above, this table is now historical**: since
`NavigatingMob` overrides all 8 perception methods, these gates are no longer hardwired false —
re-verify each one's actual behaviour before relying on this table.

| goal | gates on | expression | verdict at the time |
|---|---|---|---|
| `FloatGoal` | `crates/lodestone-entity/src/ai/goals.rs` | `mob.in_water() \|\| mob.in_lava()` | never true |
| `LookAtPlayerGoal` | same file | `mob.nearest_player()` | never `Some` |
| `HurtByTargetGoal` | same file | `mob.last_hurt_by()` | never `Some` |
| `TemptGoal` | same file | `mob.temptation()` | never `Some` |
| `AvoidEntityGoal` | same file | `mob.avoid_threat()` | never `Some` |
| `PanicGoal` | same file | `!mob.is_panicking()` → early return | never ran |

Two more were *overridden but never fed*. `is_in_love`/`find_love_partner`/`parent_position`
(`crates/lodestone-entity/src/ai/navigating_mob.rs`) read `self.love_ticks`/`partner_candidate`/
`parent_candidate`, and `MobSim::tick` (`crates/lodestone-server/src/mobs/mod.rs`), at the time,
**never
populated any of them and never called `take_bred()`** — though per A2's own description below,
this was the planned fix for that gap, so also re-verify against the current source rather than
this note:

| goal | reads | verdict |
|---|---|---|
| `BreedGoal` | `partner_candidate` | never populated |
| `FollowParentGoal` | `parent_candidate` | never populated |

So **8 of 13 goals cannot do anything in the running game.** The 5 that work —
`RandomStrollGoal`, `RandomLookAroundGoal`, `MeleeAttackGoal`, `NearestAttackableTargetGoal`,
`SwellGoal` — are *exactly* the 5 that `MobSim::spawn_species` installs (`mobs/mod.rs`). That
is not a coincidence: the existing roster was built to the subset that happened to function.

A seventh, subtler case: `RandomStrollGoal`'s idle suppression (`impl Goal for RandomStrollGoal`,
`check_no_action && mob.no_action_time() >= 100`) is *inert in the permissive direction* — the
default `0` means the suppression never triggers, so stroll is always eligible where vanilla
suppresses it. `SimMob` does track `no_action_time` (incremented in `MobSim::tick`), but on the
*sim record*, not through the `MobController` seam, so `NavigatingMob` cannot see it. A dead-code
warning would never fire; the goal simply behaves wrong.

**Consequence for this epic.** #226's goal list needs `FloatGoal` + `HurtByTargetGoal`; #228's needs
`FloatGoal` + `PanicGoal` + `TemptGoal` + `FollowParentGoal` + `BreedGoal` + `LookAtPlayerGoal`.
Both children would compile, pass their own unit tests, and change nothing on screen. This is the
repo's dominant defect class — the island — and building the roster first manufactures a tenth
instance of it across five issues at once.

### 1.2 Confirmed still-true islands — re-verdicted 2026-08-14, and most of them no longer are

**Four of these five bullets have flipped since they were written; only "no spawn table" is
still accurate.** The chain that closed them is one commit path: `natural_spawn.rs` (new,
1,442 lines) landed and is driven from the production tick loop
(`tick.rs`'s `sim.run_spawn_cycle(&mut state, &mut natural_spawner, area.chunks())`), and
`MobSim::run_spawn_cycle` calls `self.spawn_species(candidate.entity_type, candidate.pos)`
(`crates/lodestone-server/src/mobs/mod.rs`) for every candidate — the same `spawn_species` that
installs goals via `roster::goals_for`, which (per `roster/mod.rs`'s own doc, line ~444) gives
any `BRAIN_SPECIES` match "a single `BrainGoal` and nothing else". Chase each bullet below with
that chain in mind.

- **Brain never ticked outside its crate — FLIPPED, now reachable in production.** The 2026-08-04
  update already found `impl BrainMob for NavigatingMob<'_>`. What was still missing then was a
  live spawn path that actually produces a `BRAIN_SPECIES` individual — now there is one:
  `natural_spawn.rs`'s `SPAWN_RULES` table includes `armadillo`, `axolotl`, `camel`, `frog`,
  `goat`, `hoglin` and `piglin`, all of which are also in `BRAIN_SPECIES`
  (`crates/lodestone-entity/src/brain/roster.rs`). A naturally-spawned individual of any of those
  seven now goes tick.rs → `run_spawn_cycle` → `spawn_species` → `roster::goals_for` → a real,
  ticking `BrainGoal`. Not independently re-checked this pass: whether `GoalSelector::tick`
  actually advances a `BrainGoal` the same as any other `Goal` (the doc calls the seam "real and
  generic", which implies yes, but confirm before closing C1 on this alone).
- **No natural spawn driver — FLIPPED.** `crate::natural_spawn::NaturalSpawner` (`impl
  SpawnCandidateSource for NaturalSpawner`) is now a second, real, non-test implementor of
  `SpawnCandidateSource` beyond the old `AlwaysSpawns` test mock, and `run_spawn_cycle` is called
  from `tick.rs`'s production loop, not only from `tests/mob_spawn.rs`. `run_tick_loop`'s step
  list in this bullet is now missing a step: re-read `tick.rs` for where the natural-spawn call
  sits relative to the others listed, rather than trusting the old sequence.
- **No spawn table — still TRUE, re-verified.** `SpawnEnvironment` (`crates/lodestone-entity/src/spawn.rs`)
  still has zero implementors tree-wide (`grep -rn "impl SpawnEnvironment" crates/` is empty).
  `natural_spawn.rs` did **not** adopt this trait — it built its own `SpawnRule` struct instead
  (matching the parallel finding in `docs/server-gameplay-gap-census.md`'s §9 re-verdict, same
  date: `lodestone-entity/src/spawn.rs` remains a dead second engine). Do not flip this one.
- **Regional difficulty unmodeled — FLIPPED.** `crates/lodestone-server/src/regional_difficulty.rs`
  now exists and is a real module, consumed by `lightning.rs` and `world_state.rs` (both new/landed
  since this plan was written). Re-check whether it reaches mob logic specifically
  (`DespawnCtx.difficulty_peaceful` or an equivalent) before assuming this closes any particular
  mob-AI unit — the module existing is confirmed, its reach into this plan's units is not.
- **No spawn eggs / spawners — half-flipped.** Spawn eggs landed: `crate::spawn_egg::apply_spawn_egg`
  (new module, 747 lines) is called from `apply_use_item_on` in `server.rs` and calls
  `sim.spawn_species(...)` on success — so eggs are a second production entry into the same
  `spawn_species`/`roster::goals_for` chain as natural spawning. **Spawner blocks are still
  absent, re-verified**: `server.rs`'s use-item-on handling has an explicit spawner guard whose own
  comment says "Nothing is modelled for a spawner yet". `seed_demo_mobs` is no longer the *only*
  spawn source (natural spawning and spawn eggs both landed beside it), but it is unclear from
  this pass whether it is still the *initial* one at world load — not re-checked.

### 1.3 Stale claims — do not plan against these

1. **#425 is landed and its "what is still missing" section is fully stale.** Commit `7cf02e8`
   shipped both encoders, and the first is *generic*, not another hardcoded arm:
   `encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField])`, and `encode_explode(&self, centre, radius)`
   (both in `crates/versions/26.2/src/server_protocol.rs`, packet id `play::clientbound::EXPLODE`). Gated by
   `crates/versions/26.2/tests/server_creeper_metadata_and_explode.rs`. A creeper's swell **and**
   its detonation already reach a real client. Every later unit that needs per-entity metadata on
   the wire should use `encode_set_entity_data`, not build a new encoder.
2. **#228's `randomTickSpeed` trap is stale.** It says grazing is blocked because random ticks have
   "no consumer performing per-chunk random ticks — that is the world/block-tick track's job; do not
   attempt to build a random-tick loop here." `crates/lodestone-server/src/random_tick.rs` exists,
   models grass→dirt directly, and `random_ticks.tick_chunk` **runs in the production
   tick loop** (`crates/lodestone-server/src/tick.rs`). Sheep grazing has a real consumer today and folds into the
   passive unit.
3. **#228's tempt items are wrong.** It says "wheat for cow/sheep, carrot for pig, seeds for
   chicken". In 26.2 these are **item tags**, resolved from
   `.cache/mc/26.2/src/data/minecraft/tags/item/*.json`:

   | tag | contents |
   |---|---|
   | `sheep_food` | `wheat` |
   | `cow_food` | `wheat` |
   | `pig_food` | `carrot`, `potato`, `beetroot` |
   | `chicken_food` | `wheat_seeds`, `melon_seeds`, `pumpkin_seeds`, `beetroot_seeds`, `torchflower_seeds`, `pitcher_pod` |
   | `rabbit_food` | `carrot`, `golden_carrot`, `dandelion` |

   Pig is 3 items and chicken is 6, not 1 each. These are **jar data files**, so the temptation table
   must be *generated* from them under the `LODESTONE_REGEN=1` pattern, never hand-written.
4. **#221's own citation is wrong.** It cites `SpawnRule` in `spawn.rs`. **`SpawnRule` does
   not exist as a type anywhere in the repo** — its only two hits are prose, and one is a
   *broken intra-doc link* to a type that was never written. The real seam is `SpawnConditions`
   (fields `max_block_light`/`y_range`/`needs_solid_below`, `permits`) +
   `SpawnSample` + `trait SpawnEnvironment` (all in `crates/lodestone-entity/src/spawn.rs`).
5. **`EntityDataIndexOracle.java` is not in `scripts/`.** It is at
   `crates/versions/26.2/oracle-java/EntityDataIndexOracle.java`. (`scripts/` holds only the twelve
   worldgen oracles.) The dispatch brief for this plan said `scripts/`; that was wrong.
6. **This section previously found two independent `MobCategory` types and two `check_despawn`
   functions, unnamed as a fork.** **That has since been resolved for the type, though not the
   function.** `lodestone_entity::spawn::MobCategory` (`crates/lodestone-entity/src/spawn.rs`, 8
   variants, `check_despawn` taking a `DespawnCtx`) is now the **only** `MobCategory` — its own doc
   comment records the merge ("Moved here from `lodestone_server::mob_spawn`"), and
   `lodestone_server::mob_spawn` (`crates/lodestone-server/src/mob_spawn.rs`) now does
   `pub use lodestone_entity::spawn::MobCategory;` rather than declaring its own. `MobCategory::SPAWNING`
   (the 7-entry natural-spawning subset, excluding `Misc`) moved with it. What remains split is the
   **function**: `lodestone_server::mob_spawn::check_despawn` is still a thin 5-scalar wrapper around
   the entity crate's `DespawnCtx`-taking version, by design (its own doc comment explains why),
   not an unnoticed fork. Confirmed directly against the current source rather than carried forward
   from when this was written.
7. **Breeding and aging are partially landed and their landed half is itself an island.** `7bf2873`
   ("breeding and aging fill the `MobController` no-op defaults (#225, #234, #237)") added 413 lines
   to `navigating_mob.rs` overriding 14 defaults, and a doc. It **did not touch `ai/mob.rs`** (no new
   trait methods) and its own message defers the population search and "resolve `take_bred()` into a
   spawned child" to `MobSim::tick` as a broker request that never landed. See §1.1.

### 1.4 Architectural blockers found while verifying

- **`GoalSelector` cannot remove a goal.** `crates/lodestone-entity/src/ai/goal.rs` exposes `add`,
  `disable`/`enable` per `Flag`, `len`, `is_empty`, `running_indices`,
  `is_running`, `tick` — **no `remove`, no per-goal enable**. Vanilla skeletons swap bow↔melee at
  priority 4 at runtime, through a weapon-reassessment check called from four call sites in the
  same class. Not expressible today. Unit A3 must add the capability.
- **`SimMob` holds a single `GoalSelector`** (`crates/lodestone-server/src/mobs/mod.rs`); `add_goal(priority, Box<dyn Goal>)`
  is the only dynamic-dispatch extension point in the whole `lodestone-server` crate.
  Vanilla has **two** selectors with **separate priority namespaces** — a zombie has
  `goalSelector` priorities 4/8/2/3/6/7 and `targetSelector` priorities 1/2/3/3/5, per its own
  goal registration. `MobAi` (`crates/lodestone-entity/src/ai/goal.rs`) models the pair and has **zero users tree-wide**
  (`target_selector` → 0 hits). Merging is *mostly* benign because `Flag::Target` does not contend
  with `Move`/`Look`, but the priority numbers collide. Unit A3 must either adopt `MobAi` or state an
  explicit offset convention and gate it.
- **`MobSim<'w>` borrows `&'w ChunkWorld`**, entity ids are a bare `next_id: i32` counter with a
  manual `set_next_id` (`crates/lodestone-server/src/mobs/mod.rs`, used near `seed_demo_mobs` to dodge the player's id), and **there is no
  `despawn(id)`** — only `despawn_pass(nearest_player, rng)`. All three shapes are hostile
  to a per-species roster and to the ECS migration.
- **`run_tick_loop` (`crates/lodestone-server/src/tick.rs`) is `pub(crate)`, straight-line, with no way to register per-tick
  work.** Every driver this plan adds is a hardcoded insertion until the server-ECS migration
  (`docs/server-ecs.md`, decided but unimplemented) lands.
- **No event bus, no cancellation, no hook registration server-side**, so a goal that wants to veto
  an action has nowhere to do it. Affects the neutral/aggro unit's anger propagation and the
  villager/piglin unit's schedule gating.
- **`MobSim::spawn` still hardcodes zombie.** In `crates/lodestone-server/src/mobs/mod.rs`:
  `ResourceKey::from_str("minecraft:zombie")`, and `spawn_with_type` unconditionally sets
  `MobCategory::Monster` and installs **zero** goals. `spawn_species` is the species-aware
  path; hostility is a literal 8-name string match (`is_hostile_species`, `crates/lodestone-server/src/mobs/species.rs`).
- **The species roster is a hand-written 8-arm match**, `type_spec()` in
  `crates/lodestone-entity/src/attribute.rs`: zombie/husk, skeleton/stray/wither_skeleton/
  bogged, creeper, spider, pig, cow/mooshroom, sheep, chicken. Everything else falls back to
  `combat_defaults` (`crates/lodestone-server/src/mobs/mod.rs`) plus a 0.6×1.95 body (`species_shape`, same file).
- **Nothing launches a projectile in production — FLIPPED, re-verdicted 2026-08-14.** Both halves
  named in this bullet are now closed. `crates/lodestone-entity/src/ai/roster/ranged.rs`'s own
  module doc names the exact remaining wire this bullet describes — "`MobSim::tick`'s drain on
  the fourth line above does not exist yet ... a concurrent agent holds it [`mobs.rs`]" — and that
  drain now exists: `MobSim::tick` (`crates/lodestone-server/src/mobs/mod.rs`, well before the
  file's `#[cfg(test)] mod tests` boundary, so this is production code) collects
  `m.mob.take_new_launches()` per mob into `launches`, then calls
  `self.spawn_projectile_from(key, projectile, Some(shooter))` for each one — a real, non-test
  caller of the spawn path this bullet said had none. `resolve_projectile_impacts` (hit detection)
  is likewise wired, matching the update already recorded here.
- **No ranged goal of any kind exists — FLIPPED.** `crates/lodestone-entity/src/ai/roster/ranged.rs`
  is a real module (`RangedBowAttackGoal`, a blaze fireball-burst goal, and the generic
  `ProjectileLaunch` intent) — its own doc opens by quoting this exact prior state ("`RangedAttackGoal`
  and `BowAttack` were zero hits tree-wide, so no mob in this repo could shoot anything") as the
  problem it solves. Combined with the drain above, a skeleton-shaped species installed through
  this roster can now aim, fire, and have its arrow actually spawn and hit in production.

---

## 2. Ordering decision: driver first, and perception before everything

**Decision: Phase A (perception + roster substrate) strictly before any species unit. Then roster
families in parallel. Brain driver next. Natural spawning after that. Regional difficulty floats.**

The reasoning is §1.1, and it is decisive rather than a preference. A roster unit's entire output is
a list of goal constructions handed to `SimMob::add_goal`. Eight of the thirteen goals it would
construct have a `can_use` that is constant-`false` or reads a field nothing writes. So the
observable delta from landing the passive-herd roster today is exactly zero: a cow spawned through `spawn_species`
currently gets `RandomStrollGoal` + `RandomLookAroundGoal` and wanders; add `PanicGoal`, `TemptGoal`,
`BreedGoal`, `FollowParentGoal` and `FloatGoal` at vanilla's real priorities and it **still just
wanders**. Every unit test would pass, because a goal's `can_use` is tested against a `ScriptMob`
test fake (`crates/lodestone-entity/src/ai/goals.rs`) that *does* override those methods. That is the closed-loop trap in
CLAUDE.md's rule 1, and it would fire across five issues simultaneously.

Two secondary reasons reinforce the order:

- **There is no way to create a non-zombie mob observably.** `seed_demo_mobs` (`crates/lodestone-server/src/mobs/mod.rs`) is the
  only production entry, and it is a hardcoded zombie ring. So even a *working* cow roster is
  unreachable from a client until something spawns a cow. That is why unit A4 exists and why it is
  in Phase A rather than deferred to the spawn-eggs-and-spawners unit.
- **The roster substrate does not exist.** There is no `goals_for`, no `install_*_goals`, no
  `SpeciesRegistry` — 0 hits each. Five parallel roster units all editing `spawn_species` inside the
  heavily-contended `mobs.rs` (since split into `crates/lodestone-server/src/mobs/`, with `spawn_species`
  now in `mod.rs`) would serialise on the worst choke point in the crate.
  A3 exists to give each family its own file.

**What does *not* go first:** #221/#222 (spawn tables + natural cycle) and #223 (regional
difficulty). They are real and they are on the epic's spine for *population*, but they are
orthogonal to whether a goal can fire. Sequencing them ahead would delay every visible behaviour
behind a biome spawn-table dump. They land in Phase D once the roster is worth populating a world
with, and A4 covers observability in the meantime.

---

## 3. Units

Ownership is exclusive for the unit's window. Choke points (`mobs/mod.rs`, `tick.rs`,
`lodestone-server/src/lib.rs`, `ai/mod.rs`, `server_protocol.rs`) are brokered through the
orchestrator; each unit below states the exact patch it needs there.

### Phase A — the spine (A1 → A2 → A3, A4 parallel with A2)

#### A1 — Perception seam on `NavigatingMob`
**Owns:** `crates/lodestone-entity/src/ai/navigating_mob.rs` (exclusive).
**Broker:** none.
**Do:** add settable perception inputs to `NavigatingMob` (nearest player, last-hurt-by source,
temptation position, threat position, panic flag, in-water/in-lava, no-action-time) and override the
8 trait defaults in the `impl MobController` block to read them. Pure additive; no goal
changes. Feet-block water/lava classification comes from the `PathWorld` the struct already holds.
**Gate:** in-file tests that drive each of the six structurally-dead goals to actually return
`can_use == true` and then run, using a real `NavigatingMob` over the existing `Arena` fixture (not
`ScriptMob`).
**Negative control that must fail:** the same six assertions against a `NavigatingMob` with the new
inputs left unset — must report `can_use == false` for all six. If that control passes, the test is
reading `ScriptMob` or its own setters rather than the seam.
**Vacuous if:** written against `ScriptMob` (`crates/lodestone-entity/src/ai/goals.rs`), which already overrides all eight —
that harness cannot see this bug and is exactly why the bug survived. Also vacuous if it asserts only
that the setters round-trip, rather than that a *goal* fires.

#### A2 — Perception feed and breeding resolution in `MobSim::tick`
**Owns:** `crates/lodestone-server/src/mobs/` (exclusive; the crate's hottest file — keep the
window short).
**Broker:** none beyond exclusive `mobs/`.
**Depends on:** A1.
**Do:** in `MobSim::tick`, before `m.mob.tick(&mut m.goals)`, populate each mob's new A1
inputs from the sim's own census — nearest player position, `last_hurt_by` from the existing hurt
bookkeeping, threat/temptation candidates, panic from recent damage, `no_action_time` from the sim
record it already increments. Also populate `partner_candidate`/`parent_candidate` and
**consume `take_bred()`**, applying `PARENT_AGE_AFTER_BREEDING` to both parents and spawning the
child — the step `7bf2873` explicitly deferred here. This closes the breeding/aging landed island.
**Gate:** `tests/mob_sim.rs` — two cows within breed range, both fed into love, tick until a third
mob exists with `is_baby() == true` and both parents' age reset to 6000; separately a mob damaged in
water floats and panics.
**Negative control that must fail:** the identical scenario with the feed lines removed (or with
partner range set beyond `BREED_RANGE_SQR`) must produce no child. Assert the population count
*does not* grow.
**Vacuous if:** it asserts `take_bred()` returned true rather than that an entity exists — the
existing seam already returns true without a child being created. Predict the population count, not
the flag.

#### A3 — Roster substrate + `GoalSelector` removal
**Owns:** new `crates/lodestone-entity/src/ai/roster/mod.rs`;
`crates/lodestone-entity/src/ai/goal.rs`.
**Broker:** one line `pub mod roster;` in `crates/lodestone-entity/src/ai/mod.rs`; and in
`spawn_species` (`crates/lodestone-server/src/mobs/mod.rs`) replace the four hardcoded `add_goal` calls with
`for (p, g) in roster::goals_for(species, step_per_tick) { m.add_goal(p, g); }`.
**Do:** define `pub fn goals_for(species: &str, speed: f64) -> Vec<(i32, Box<dyn Goal>)>` — pure,
world-free, unit-testable, returning an empty vec for unknown species so the fallback is explicit.
Add `GoalSelector::remove`/per-goal enable for skeleton's weapon swap (§1.4). Decide and **document**
the two-selector question: either adopt `MobAi` (`crates/lodestone-entity/src/ai/goal.rs`) or fix a priority-offset convention
mapping vanilla's `targetSelector` namespace into the single selector, and gate the chosen mapping.
**Gate:** `goals_for("creeper", …)` returns exactly vanilla's priority multiset from its own goal
registration; a removal test proves a goal stops being scheduled after `remove`.
**Negative control that must fail:** `goals_for` on an unknown species must return empty and a
zombie's set must *not* equal a creeper's — an assertion that passes for both means the table is
being read from the wrong key.
**Vacuous if:** the expected priorities are copied from our own `goals_for` rather than from the
cited vanilla source. Cite the symbol in the test, not a line number.

#### A4 — An observable way to create a species (parallel with A2)
**Owns:** `crates/lodestone-server/src/spawn.rs`.
**Broker:** whichever of `seed_demo_mobs` / a debug command path the orchestrator prefers; if
`mobs/`, must serialise behind A2.
**Do:** the minimum that lets a connected client see a named species — either widen the demo seeding
to a species list, or a creative/debug spawn path. Explicitly **not** spawn eggs or spawners: no spawn-egg item
handling, no spawner block.
**Gate:** connect the real client adapter to the integrated server and assert an `ADD_ENTITY` for a
non-zombie type id arrives.
**Negative control that must fail:** assert the *zombie-only* build produces no such packet.
**Vacuous if:** it asserts `MobSim::len()` grew rather than that a packet reached the wire — that is
the island shape this whole plan is about.

### Phase B — roster families (B1–B5 fully parallel after A3)

Each owns one file under `crates/lodestone-entity/src/ai/roster/` plus a registration line in
`roster/mod.rs` (small, brokered). Constants are cited in §4 — **cite, do not transliterate**.

- **B1 — hostile melee (#226).** Owns `roster/hostile_melee.rs`. Zombie family, skeleton melee
  variants, spider, creeper. Creeper's set is already partly proven end-to-end (§1.3 item 1) and is
  the reference shape. Needs zombie's daylight-burn and villager-over-player target priority.
- **B2 — passive herd (#228, folds in #238).** Owns `roster/passive.rs` + a **generated** tempt-item
  table from the jar's tag JSON (§1.3 item 3), following the `collision_shapes`/`hardness`
  generate-or-assert + `LODESTONE_REGEN=1` pattern. Sheep grazing is in scope now that
  `random_tick.rs` exists. **Touches metadata** (the sheep's wool-colour/sheared index) → must run the oracle, §5.
- **B3 — ranged (#227).** Owns `crates/lodestone-entity/src/ai/goals/ranged.rs` +
  `roster/ranged.rs`. The largest unit: a new goal family plus the projectile *launch* wiring that
  §1.4 shows does not exist (though hit detection itself has since landed — see the status note
  in "What it is"). Needs A3's `remove` for the skeleton's weapon-reassessment swap. Needs a brokered
  `MobSim::spawn_projectile` call site (`crates/lodestone-server/src/mobs/projectiles.rs`).
  **Recommend splitting B3 into B3a (launch + tick reaches a client as a moving arrow
  entity) and B3b (hit detection and damage).**
- **B4 — neutral/aggro (#233). Landed.** Owns `roster/neutral.rs` + a shared anger-timer state
  machine (`SimMob::anger`/`MobController::angry_target`, issue #458). All four headline mechanisms
  are real: enderman stare and teleport, zombified piglin group alert, bee sting-then-die, wolf pack
  aggro — the last three via `NearestAttackableTargetGoal::anger_gated()` (the anger-gated acquisition
  row) plus `MobSim::attack`'s `alert_species` same-species census (the propagation half, resolved as
  a direct sim-side census rather than an event bus, exactly as this bullet originally planned). Bee
  metadata (`hasStung`) was **not** needed on the wire — it stays a host-side `SimMob::stung_at` drain
  flag, never sent to a client, so no oracle run was required for it.
- **B5 — specialists (#232).** Owns `roster/specialist.rs`. Guardian beam is a *third* attack shape
  (charge-then-damage-tick, neither melee nor projectile); ghast fireball feeds `explosion.rs`, which
  now has a real caller and a real `encode_explode`. Warden is Brain-based → defer its sensor to
  Phase C. **Touches metadata** (guardian beam target) → oracle.

### Phase C — Brain (after A, parallel with B)

- **C1 — Brain driver (#209).** Owns new `crates/lodestone-entity/src/brain/navigating_brain_mob.rs`.
  Broker: a `mod` line, and the driver insertion in `MobSim::tick`/`run_tick_loop`. The
  `NavigatingMob` composition is the explicit template. **State the server-ECS-migration dependency:** without the
  ECS migration this is a hardcoded insertion into a `pub(crate)` straight-line loop; that is
  acceptable but must be written down.
  **Gate:** a Brain mob placed in a real `MobSim` world and ticked through the new composition,
  asserting the `WALK_TARGET` memory hand-off actually moves it — **not** `Brain::tick` in isolation,
  which is what already passes.
  **Negative control:** the same world with the driver insertion removed must show no movement.
- **C2 — Brain passive roster (#230).** Owns `brain/roster/passive.rs`. Depends on C1.
- **C3 — Villager and piglin (#231).** Owns `brain/roster/villager.rs`. Depends on C1. Coordinate
  with the Phase-E economy issues; needs POI claiming to gate WORK.

### Phase D — population (after B; D1 → D2, D3 parallel)

- **D1 — spawn table + `MobCategory` unification (#221).** Must resolve the duplicate-type fork
  (§1.3 item 6 — **the `MobCategory` type half is now done**, see the status note in "What it is")
  and write `SpawnRule` — a type that has never existed. Generate per-species
  conditions and biome spawn lists from the real jar.
- **D2 — natural spawn cycle (#222).** The driver. Needs D1 and a player-position feed that
  `seed_demo_mobs`'s own doc comment (`crates/lodestone-server/src/mobs/mod.rs`) says is missing. Replaces A4's demo seeding.
- **D3 — spawn eggs and spawners (#224).** Parallel with D2.

### Phase E — floats freely

- **E1 — regional difficulty (#223).** Owns a new `crates/lodestone-entity/src/difficulty.rs`. Small,
  self-contained, deterministic, no live server needed. Zero dependencies — good filler at any point,
  but it must land before D2 wants difficulty-scaled spawn composition.

### Adjacent issues: fold in, or separate

| issue | verdict |
|---|---|
| #234 breeding | **folds into A2** — half landed in `7bf2873`, the resolution is A2's job |
| #237 aging | **folds into A2** — same commit, same gap |
| #238 sheep grazing | **folds into B2** — its blocking trap is stale (§1.3 item 2) |
| #235 taming | **separate, and blocks #229** — ownership state + interaction packet, not AI |
| #229 tameable companions | stays a child, but blocked on #235; schedule after B |
| #236 leashing | **separate** — entity-attach packet, not AI |
| #239 golem construction | **separate** — block-pattern detection, belongs to the block track |
| #240 wandering trader | **separate** — spawn scheduling + trades; depends on Phase D |
| #241 raids and patrols | **separate, large** — depends on C1 *and* Phase D |

---

## 4. Vanilla constants, cited

Every value below was verified against the decompiled 26.2 source. Re-verify against that source
before citing a number in an implementing test; do not copy the numbers without re-checking them
there.

**Zombie**: follow range 35.0, movement speed 0.23, attack damage 3.0 (from its own attribute
registration); baby speed modifier 0.5, a base-scaled additive modifier (also from attribute
registration).
Goals (from its own goal registration) — `4 ZombieAttackTurtleEggGoal(1.0, 3)`, `8 LookAtPlayerGoal(Player, 8.0F)`,
`8 RandomLookAroundGoal`, `2 SpearUseGoal(1.0, 1.0, 10.0F, 2.0F)`, `3 ZombieAttackGoal(1.0, false)`,
`6 MoveThroughVillageGoal(1.0, true, 4)`, `7 WaterAvoidingRandomStrollGoal(1.0)`. Targets, same
registration — `1 HurtByTargetGoal(alert-others: ZombifiedPiglin)`, `2 NearestAttackableTargetGoal(Player, true)`,
`3 (AbstractVillager, false)`, `3 (IronGolem, true)`, `5 (Turtle, 10, …)`.

**Creeper**: swell threshold 30 ticks, explosion radius 3 (both plain fields),
fall adds `fallDistance * 1.5` capped at `swell threshold - 5`, synced swell-direction field
defaults to -1, synced ignited flag defaults to false, saved fuse (short, default 30) and
explosion-radius (byte, default 3) round-trip through its own save/load code. Goals
(from its own goal registration) — `1 FloatGoal`, `2 SwellGoal`,
`3 AvoidEntityGoal(Ocelot, 6.0F, 1.0, 1.2)`, `3 AvoidEntityGoal(Cat, 6.0F, 1.0, 1.2)`,
`4 MeleeAttackGoal(1.0, false)`, `5 WaterAvoidingRandomStrollGoal(0.8)`,
`6 LookAtPlayerGoal(Player, 8.0F)`, `6 RandomLookAroundGoal`. Targets, same registration.

**Skeleton**: movement speed 0.25 (from its own attribute registration);
`RangedBowAttackGoal<>(this, 1.0, 20, 15.0F)` (from its own goal registration) — speed 1.0, attack interval **20 ticks**,
attack radius **15.0**. Goals, same registration — `2 RestrictSunGoal`, `3 FleeSunGoal(1.0)`,
`3 AvoidEntityGoal(Wolf, 6.0F, 1.0, 1.2)`, `5 WaterAvoidingRandomStrollGoal(1.0)`,
`6 LookAtPlayerGoal(Player, 8.0F)`, `6 RandomLookAroundGoal`. A runtime weapon-reassessment check
installs bow *or* melee at priority 4.

**Spider**: max health 16.0, movement speed 0.3 (from its own attribute registration).
Goals (from its own goal registration) — `1 FloatGoal`, `2 AvoidEntityGoal(Armadillo, 6.0F, 1.0, 1.2)`,
`3 LeapAtTargetGoal(0.4F)`, `4 SpiderAttackGoal`, `5 WaterAvoidingRandomStrollGoal(0.8)`,
`6 LookAtPlayerGoal(Player, 8.0F)`, `6 RandomLookAroundGoal`.

**Blaze**: attack damage 6.0, movement speed 0.23, follow range 48.0
(from its own attribute registration). Volley timing (its own attack-tick logic): first shot `attackTime = 60`; steps 2–4 `attackTime = 6`; after
step 4 `attackTime = 100` and `attackStep = 0`; the non-volley branch uses `attackTime = 20`.

**Ghast**: explosion power 1 (a plain field), max health 10.0,
follow range 100.0 (from its own attribute registration); charge-sound timer fires at 10, explodes at **20**, resets to **-40**
(its own charge-tick logic).

**Guardian**: attack timer length 80 (a plain field), attack damage 6.0,
movement speed 0.5, max health 30.0 (from its own attribute registration); beam `attackTime` starts at **-10** and
damage lands once the attack timer reaches its configured duration.

**Enderman**: movement speed 0.3, attack damage 7.0,
follow range 64.0 (from its own attribute registration); its stare test checks look angle within
0.025 rad of dead-on, from eye height, ignoring visibility and hit-testing;
its look-for-player goal accumulates an aggro timer to a threshold of 5 (tick-delay-adjusted) and
teleports once a separate counter reaches a threshold of 30 (also tick-delay-adjusted, both timers
owned by that goal itself).
**Corrected while landing #233**: the tick-delay adjustment halves its argument (a shared
goal-scheduling helper, using ceiling-divide-by-two) and nothing in this goal's chain opts out of
per-tick updates, so the real aggro delay is **3** ticks, not the literal 5, and the
teleport-towards delay is **15**, not 30 — the landed implementation (`roster::neutral::ENDERMAN`'s own
doc comment) uses 3/15; this table previously quoted the unhalved literals.

**Zombified piglin**: movement speed 0.23 (from its own attribute registration);
anger duration is a random 20-39 seconds (a plain field); alert-others vertical range is 10 blocks (a plain field);
its alert-others hook is called from its own goal registration; `1 HurtByTargetGoal(alert-others)` (also from its own goal registration).

**Bee**: anger duration is a random 20-39 seconds (a plain field);
goals (from its own goal registration) — `0 BeeAttackGoal(1.4F, true)`, `2 BreedGoal(1.0)`,
`3 TemptGoal(1.25, BEE_FOOD)`, `5 FollowParentGoal(1.25)`, `9 FloatGoal`; a stung flag
gates death; roll condition `distanceToSqr < 4.0`.

**Wolf**: movement speed 0.3, max health 8.0, attack damage 4.0
(from its own attribute registration); tamed max health 40.0, untamed 8.0 (also from attribute
registration). Goals (from its own goal registration); targets,
same registration — `1 OwnerHurtByTargetGoal`, `2 OwnerHurtTargetGoal`,
`3 HurtByTargetGoal(alert-others)`, `6 FollowOwnerGoal(1.0, 10.0F, 2.0F)` at goal priority 6.

**Breeding**: age gained after a successful breed is 6000 ticks (a plain field, on the shared
animal-breeding base); love mode lasts 600 ticks from breed-start; love decrements with a particle
every 10 ticks; love resets on both parents at the end (all in the shared breeding/aging tick
logic every animal shares).

**Passive herd** — attributes and full goal lists (attribute values and goals both from each
species' own registration):

| species | health / speed | panic | breed | tempt | follow-parent | stroll | look |
|---|---|---|---|---|---|---|---|
| cow | 10.0 / 0.2F | `1 @2.0` | `2 @1.0` | `3 @1.25 COW_FOOD` | `4 @1.25` | `5 @1.0` | `6 @6.0F` |
| sheep | 8.0 / 0.23F | `1 @1.25` | `2 @1.0` | `3 @1.1 SHEEP_FOOD` | `4 @1.1` | `6 @1.0` | `7 @6.0F` |
| pig | 10.0 / 0.25 | `1 @1.25` | `3 @1.0` | `4 @1.2 PIG_FOOD` | `5 @1.1` | `6 @1.0` | `7 @6.0F` |
| chicken | 4.0 / 0.25 | `1 @1.4` | `2 @1.0` | `3 @1.0 CHICKEN_FOOD` | `4 @1.1` | `5 @1.0` | `6 @6.0F` |
| rabbit | 3.0 / 0.3F / atk 3.0 | `1 @2.2` | `2 @0.8` | `3 @1.0 RABBIT_FOOD` | — | `6 @0.6` | `11 @10.0F` |

Sheep's `5 eatBlockGoal` is grazing;
pig has **two** `TemptGoal`s at priority 4 — `CARROT_ON_A_STICK` and `PIG_FOOD`;
chicken additionally has `eggTime = random.nextInt(6000) + 6000` (its own egg-lay tick logic);
rabbit additionally has three `RabbitAvoidEntityGoal`s at priority 4 (Player 8.0F,
Wolf 10.0F, Monster 4.0F, all speed 2.2/2.2) and `RaidGardenGoal` at 5.

---

## 5. Metadata rule for every unit that touches it

**Never hand-count an index.** Run `crates/versions/26.2/oracle-java/EntityDataIndexOracle.java`
(*not* `scripts/`, §1.3 item 5), which dumps every synced metadata field sorted by index so collisions
land on adjacent lines. Hand counting has already shipped two bugs — the sheep's wool-colour index and
the horse's type-variant index, each off by one, both having missed a shared ageable-mob lock flag; every
sheep in the game rendered its default colour while the decoder reported a clean parse. B2 (sheep
wool) is the unit most exposed to a *recurrence* of exactly that bug — treat the previous fix as a
claim to re-verify against the oracle, not a settled fact.

Pick the guard by which classes actually collide, using
`lodestone_data::entity_census::{is_living, is_mob}` (`crates/lodestone-data/src/entity_census.rs`,
keyed by network entity-type id, `TYPE_COUNT = 158`):

- **Index 8** — the shared living-entity flags field vs. an arrow's own flags field, both a single
  byte, the arrow's crit bit `0x01` bit-identical to "using item". Living vs non-living → **`is_living`**.
- **Index 15** — the shared mob flags field (aggressive `0x04`) vs. an armour stand's own client-side
  flags field, whose `0x04` there means "show arms". An armour stand **is** a living entity, so
  `is_living` would report every decorative armour stand with arms as an aggressive mob. Living vs
  living → **`is_mob`**.
  A display entity also claims index 15 as a single byte.

Do not assume the previous collision's guard generalises. Use `encode_set_entity_data`
(`crates/versions/26.2/src/server_protocol.rs`) rather than adding another single-purpose encoder.

---

## 6. Live-gate hazards

- **`NoAI:1b` halts gravity, not just AI** — a `NoAI` subject does not fall at all. Use it for a
  stationary *target* only, never for a subject whose motion is under test; a gate that used it would
  read "no fall damage" as a code defect.
- **`Invulnerable:1b` makes an entity un-targetable**, so it silently disables the targeting goals
  most of Phase B exists to prove.
- **A freshly summoned entity is not selector-visible until the next server tick.** Poll; never
  assert immediately.
- **`tick step N` does not advance entity physics — only `tick sprint N` does**, and a
  `tick sprint 1` used for registration silently consumes a tick.
- **Offline mode derives the account UUID from the username**, and a dead player is held on the death
  screen, which sends no chunks — a total chunk blackout while join and keep-alives continue. Use
  `lodestone-testsupport`'s `unique_username`.
- `minecraft:generic` is `bypasses_armor`-tagged; use `minecraft:mob_attack` for reducible damage.

---

## 7. The three biggest risks

1. **Perception starvation is rediscovered per-unit instead of fixed once.** It is in no issue, and
   every affected goal has a green unit test against `ScriptMob`, which overrides all eight methods.
   A roster author who tests the way the existing tests are written will see green and ship an
   island. Mitigation: A1/A2 are hard gates on all of Phase B, and A1's negative control is written
   specifically to fail on the `ScriptMob` shortcut.
2. **`mobs.rs` serialises the epic.** At the time of writing it was 1,970 lines (since split into
   `crates/lodestone-server/src/mobs/`, with most of this content in `mod.rs`); it holds `MobSim`, `SimMob`, `spawn_species`,
   `run_spawn_cycle`, `despawn_pass` and `seed_demo_mobs`, and is the only place `add_goal` can be
   called. A2, A4, B3, C1, D2 and D3 all want it. Mitigation: A3's `goals_for` moves all per-species
   data out into per-family files so Phase B never touches `mobs/mod.rs`; A2 takes an exclusive short
   window; everything else states a one-line brokered patch. Residual risk is real — this is the
   choke point that the server-ECS migration exists to fix, and Phase C/D will feel it.
3. **#227 (ranged) is mis-scoped as one unit and will stall.** It needs a new goal family, a
   `GoalSelector::remove` that does not exist, a projectile *launch* path that has no production
   caller, and — at the time of writing — hit detection plus damage that were not implemented (see the
   status note in "What it is": `spawn_projectile`'s own doc comment now says this landed) — across
   `lodestone-entity`, `mobs/` and possibly `server_protocol.rs`. Mitigation: split into B3a
   (an arrow leaves a skeleton and reaches a client as a moving entity) and B3b (it hits and hurts),
   and do not let B3b block B1/B2/B4.

A fourth was named here: **the two `MobCategory` types** (§1.3 item 6). **That part of D1 is done** —
`lodestone_server::mob_spawn` now re-exports `lodestone_entity::spawn::MobCategory` rather than
declaring its own (confirmed directly against the source; see the status note in "What it is"). The
two `check_despawn` functions with different signatures remain, by design, and are not a fork to
unify.
