# Mob breeding and aging (issues #234, #237)

## What it is

The entity-side half of vanilla animal breeding (`Animal`/`AgeableMob`) and
baby growth, filling four `MobController` trait-default no-ops that were
previously always `false`/`None`/`{}`
(`crates/lodestone-entity/src/ai/mob.rs`'s `MobController` trait): `is_in_love`,
`find_love_partner`, `love_partner_position`, `breed`, plus `is_baby` and
`parent_position`. Until this, `BreedGoal` and `FollowParentGoal`
(`crates/lodestone-entity/src/ai/goals.rs`) were
scheduler-correct and unit-tested but could never actually run against the
crate's one production `MobController` implementor,
[`NavigatingMob`](../crates/lodestone-entity/src/ai/navigating_mob.rs) — the
same "goal exists, effect doesn't" shape a prior triage sweep found for all
four defaults.

## How it works

`NavigatingMob` now owns the timing state vanilla keeps on `Animal`/
`AgeableMob` directly:

- `love_ticks: i32` — vanilla `Animal.inLove`. `set_in_love()` sets it to
  [`LOVE_TICKS`] (600, vanilla `Animal::setInLove`,
  `.cache/mc/26.2/src/net/minecraft/world/entity/animal/Animal.java`).
  Decremented by one every [`advance`](../crates/lodestone-entity/src/ai/navigating_mob.rs)
  call, unconditionally — mirroring `Animal::aiStep` ageing it regardless of
  whether any goal ran. `is_in_love()` is `love_ticks > 0`.
- `age: i32` — vanilla `AgeableMob.age`
  (`.cache/mc/26.2/src/net/minecraft/world/entity/AgeableMob.java`):
  negative while a baby (ticks up toward `0`), positive as the post-breeding
  parent cooldown (ticks down toward `0`). `is_baby()` is `age < 0`.
  [`BABY_START_AGE`] is `-24_000`, [`PARENT_AGE_AFTER_BREEDING`] is `6_000`
  (`AgeableMob.BABY_START_AGE`, `Animal.PARENT_AGE_AFTER_BREEDING`). `age_locked: bool` freezes it
  (vanilla `AgeableMob.AGE_LOCKED`); the golden-dandelion interaction that
  sets that flag in vanilla is not implemented, but the freeze itself is
  honoured.
- `partner_candidate: Option<Vec3>` / `parent_candidate: Option<Vec3>` — host
  injection points. `lodestone-entity` has no notion of a *population* of
  mobs (see the crate's own `MobController::find_love_partner` doc comment:
  "the host performs the version/type-specific `canMate` filter and holds
  the chosen partner"), so `NavigatingMob` cannot search siblings itself.
  The host refreshes these once per tick, before calling
  `tick`/`advance`, with the result of its own population-wide search;
  `find_love_partner`/`love_partner_position`/`parent_position` just read
  them back.
- `bred: bool`, drained by `take_bred()` — set when a `BreedGoal` calls
  `MobController::breed()`. `breed()` also resets `love_ticks` to `0`
  immediately (vanilla `Animal::resetLove`, called from
  `Animal.finalizeSpawnChildFromBreeding`), but does **not** spawn a child or apply the
  parent-age cooldown — this seam has no notion of the partner's identity or
  of creating a new entity. The host resolves `take_bred()` into an actual
  spawn.

## What now drives it in production

**Now fully wired.** The population-wide search and the "resolve a
`take_bred()` into a child" step landed in `MobSim::tick`/`MobSim::resolve_breeding`
(`crates/lodestone-server/src/mobs/mod.rs`) in a prior session; `MobSim::feed_perception`
performs the partner search and `resolve_breeding` performs the child spawn,
the parent-age cooldown and the experience orb. See `docs/taming-and-breeding.md`
for the interaction-arm side (what puts an animal in love in the first place).

## The age-scaled hitbox and movement speed

Until this pass, `species_shape` and `combat_defaults` took only
`entity_type`/static `attrs` — no `is_baby` parameter existed anywhere, so a
spawned or bred baby kept its species' **adult** collision box and adult
movement step forever. `pose.rs`'s `BABY_LIMB_SCALE` is a walk-cycle
*animation* constant and never touched the hitbox or speed.

**Fixed by threading `is_baby` through the shape fold, and re-deriving it on
every baby/adult boundary crossing:**

- [`species_shape`] now takes `is_baby: bool`. For a species with an entry in
  [`baby_dimensions`] it uses that literal (mirroring vanilla's own
  `BABY_DIMENSIONS` overrides — a baby zombie is `0.49×0.98`, **not**
  `0.6×1.95` halved); for anything else it falls back to
  [`DEFAULT_BABY_AGE_SCALE`] (`0.5`), vanilla `LivingEntity.getAgeScale()`'s
  own default. The `SCALE` attribute is still applied once, uniformly, after
  either selection — matching vanilla's separate
  `getDefaultDimensions().scale(getScale())` fold.
- [`SimMob::set_age`] detects a baby/adult boundary crossing (not every call —
  a baby's 24,000-tick countdown would otherwise re-resolve the shape for no
  observable change) and calls the new `NavigatingMob::set_shape`/
  `set_step_per_tick` to update the live mob's hitbox and speed. Both the
  spawn path (`spawn_species(...).set_age(BABY_START_AGE)`) and the breeding
  path (`resolve_breeding`'s `child.set_age(BABY_START_AGE)`) go through this
  one method, so a bred child gets the correct shape with no separate wiring.
- [`baby_speed_multiplier`] carries the zombie family's `SPEED_MODIFIER_BABY`
  (`ADD_MULTIPLIED_BASE` `0.5`, i.e. `base * 1.5`) — the only baby-only speed
  modifier among the species this sim spawns babies for. Every breedable
  `Animal` here (cow, sheep, pig, chicken, rabbit, wolf) has **no** baby speed
  modifier in vanilla; only its hitbox shrinks.
- **`combat_defaults` deliberately did not gain an `is_baby` parameter.**
  Checked against `Zombie.createAttributes()` and every breedable species'
  attribute builder: `max_health`/`attack_damage`/`armor` do not vary with
  age for any species this sim models. Threading a parameter through that
  changes nothing would be the "vacuous species" this repo's evidence
  section warns about — the function's own doc comment now says so, so a
  future reader does not "fix" a gap that was checked and closed as absent.
- **Not covered**: the path navigator keeps the width it was constructed
  with across a shape change (rebuilding it would drop an in-flight path —
  disclosed in `NavigatingMob::set_shape`'s own doc), and no species outside
  the table above has vanilla's `BABY_DIMENSIONS` ported — the generic `0.5`
  fallback is real vanilla behaviour for those, not a placeholder, but a
  species with its own literal (cat, ocelot, the horse family, chicken's
  `#chicken_food` cousins not yet tameable here) will read slightly wrong
  until someone adds a row.
- **The server authoritatively uses the corrected shape and speed, and the
  wire now tells a client to *render* a baby small too.**
  `crate::protocol::MetadataField::Baby` and its `v770` `encode_set_entity_data`
  arm existed for a time with **zero producers** — declared, encoded, decoded,
  documented, and never once constructed — so every baby mob still reached the
  client as an adult despite the server-side hitbox/speed fix above. Closed by
  a species switch in `SimMob::snapshot`, following the same pattern
  `TamableFlags`/`HorseFlags` already use for their own shared metadata index:
  index 16 collides with `Creeper.DATA_SWELL_DIR` (an `INT`), so the guard has
  to live in the producer, not the encoder.
  `MetadataField::Baby` is pushed unconditionally (not only while `is_baby()`
  is true) for exactly the species this doc already scopes ageing to — the
  breedable-animal family (cow, mooshroom, sheep, pig, chicken, rabbit, wolf)
  and the zombie family (zombie, husk, zombie_villager, drowned,
  zombified_piglin) — confirmed against `.cache/mc/26.2/src/` to descend from
  `AgeableMob` or `Zombie`, the two vanilla classes whose own `DATA_BABY_ID`
  resolves to that index. Unconditional matters because a baby **grows up**:
  an encoder that only sent `Baby(true)` on arrival would leave a matured mob
  stuck at baby size on every already-connected client, the same "absence
  cannot mean false" argument `CreeperSwellDir` already makes for its own
  retreat-to-default case.
  `crates/lodestone-server/src/mobs/mod.rs`'s `baby_metadata_tests` module
  covers both directions: every eligible species emits `Baby(false)` as an
  adult, `creeper`/`ghast`/`phantom` (index 16's other real claimants) emit no
  `Baby` field at all, and a baby zombie that grows up reports `Baby(false)`
  rather than dropping the field. `crates/protocol/v770/src/server_protocol.rs`'s
  `index_sixteen_tests` module checks the wire encoding and the index/serializer
  claims against the committed `EntityDataIndexOracle` dump.

`crates/lodestone-server/src/mobs/mod.rs`'s `baby_shape_tests` module predicts
the exact baby zombie box against the halved-adult wrong hypothesis, the
exact baby cow box, the generic fallback for a species with no table entry
(a control against skeleton, which never breeds but exercises the same
`is_baby()` boundary), the exact `0.23 * 1.5 = 0.345` baby zombie speed
against a cow's unchanged speed, growing back up restoring the adult shape,
and that a bred child's shape is already correct with no extra host wiring.

**What is proven today**: a driver-level test,
`crates/lodestone-entity/src/ai/navigating_mob.rs`'s
`breed_goal_drives_two_navigating_mobs_to_a_predicted_tick`, runs two real
`NavigatingMob`s through two real `GoalSelector`s holding the production
`BreedGoal`, with only the partner-candidate refresh played by the test
(exactly the population search the server patch will perform). It predicts
—not merely asserts the sign of—the tick breeding completes: vanilla's own
`BreedGoal.tick`'s timer (`loveTime >= adjustedTickDelay(60)`) matches
`BreedGoal::BREED_TIME` (60) in `goals.rs`, and with both mobs already
in range the goal starts on tick 1, so `take_bred()` must be `false` through
tick 59 and `true` at exactly tick 60 — verified for both mobs. A negative
control, `breed_goal_never_fires_without_a_partner_candidate`, runs the
identical setup for 200 ticks with the candidate refresh removed and asserts
it never breeds — proving the assertion above is actually exercising the
candidate wiring, not some coincidence of the goal's own gating.
`follow_parent_goal_drives_a_baby_navigating_mob_toward_its_parent` is the
same shape for `FollowParentGoal`: a real A* search carries a baby toward an
injected parent candidate.

## How to change it, and the gotchas

- **The population search cannot live in `lodestone-entity`.** Any
  temptation to make `NavigatingMob` "just find its own partner" runs into
  the same version-free-crate boundary as pathfinding's `PathWorld` seam:
  the crate has no way to enumerate siblings, and should not gain one — the
  host (here, `MobSim`) is exactly the layer that already holds
  `Vec<SimMob>`.
- **Partner selection is currently a fresh nearest-candidate search every
  tick, not a persisted "held partner" like vanilla's `BreedGoal.partner`.**
  With two or three animals this is behaviourally identical to vanilla; with
  several same-species in-love animals equally close together it can thrash
  between candidates tick to tick instead of committing to one. Fixing this
  needs a persisted `love_partner_id: Option<i32>` on `SimMob` — noted as
  follow-up in the broker patch, not implemented, to keep that patch small.
- **No XP orb, stat, or advancement trigger on breeding.** Vanilla awards
  1–7 XP (`Animal.finalizeSpawnChildFromBreeding`, `this.getRandom().nextInt(7) + 1`) and triggers
  `Stats.ANIMALS_BRED`/`CriteriaTriggers.BRED_ANIMALS`
  (same method). `MobSim` has no experience-orb entity type and no
  stats/advancements plumbing at all — an honest scope cut, not a silent
  gap.
- **No food-item feeding.** `set_in_love()`/age-speedup-via-feeding
  (`Animal.mobInteract`) needs per-species food-item
  detection and a player-interaction path into `MobSim`; nothing here
  triggers it — a caller (or a test) must call `set_in_love()` directly for
  now.
- **Taming landed** (see `docs/taming-and-breeding.md`) — `MobController` and
  `goals.rs` both gained real surface for it. Leashing has not; see the
  parent issue's per-family table for the honest breakdown.

## Configuration

No feature flags or env vars. The vanilla constants above are `pub const`s
on `lodestone_entity::ai`: [`LOVE_TICKS`], [`BABY_START_AGE`],
[`PARENT_AGE_AFTER_BREEDING`] (re-exported from `navigating_mob.rs` via
`ai/mod.rs`).

## Dependencies

- `lodestone-entity`'s `ai::goals::{BreedGoal, FollowParentGoal}` — the
  goals this wiring finally makes reachable.
- `crates/lodestone-server/src/mobs/mod.rs`'s `MobSim`/`SimMob` — the intended
  production host; see `docs/live-mob-sim.md` for the tick loop this state
  needs to be fed from.

[`LOVE_TICKS`]: ../crates/lodestone-entity/src/ai/navigating_mob.rs
[`BABY_START_AGE`]: ../crates/lodestone-entity/src/ai/navigating_mob.rs
[`PARENT_AGE_AFTER_BREEDING`]: ../crates/lodestone-entity/src/ai/navigating_mob.rs
