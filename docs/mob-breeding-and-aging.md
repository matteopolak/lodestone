# Mob breeding and aging (issues #234, #237)

## What it is

The entity-side half of vanilla animal breeding (`Animal`/`AgeableMob`) and
baby growth, filling four `MobController` trait-default no-ops that were
previously always `false`/`None`/`{}`
(`crates/lodestone-entity/src/ai/mob.rs:138-166`): `is_in_love`,
`find_love_partner`, `love_partner_position`, `breed`, plus `is_baby` and
`parent_position`. Until this, `BreedGoal` and `FollowParentGoal`
(`crates/lodestone-entity/src/ai/goals.rs:643-716`, `566-641`) were
scheduler-correct and unit-tested but could never actually run against the
crate's one production `MobController` implementor,
[`NavigatingMob`](../crates/lodestone-entity/src/ai/navigating_mob.rs) — the
same "goal exists, effect doesn't" shape #225's triage sweep found for all
four defaults.

## How it works

`NavigatingMob` now owns the timing state vanilla keeps on `Animal`/
`AgeableMob` directly:

- `love_ticks: i32` — vanilla `Animal.inLove`. `set_in_love()` sets it to
  [`LOVE_TICKS`] (600, vanilla `Animal::setInLove`,
  `.cache/mc/26.2/src/net/minecraft/world/entity/animal/Animal.java:174`).
  Decremented by one every [`advance`](../crates/lodestone-entity/src/ai/navigating_mob.rs)
  call, unconditionally — mirroring `Animal::aiStep` ageing it regardless of
  whether any goal ran. `is_in_love()` is `love_ticks > 0`.
- `age: i32` — vanilla `AgeableMob.age`
  (`.cache/mc/26.2/src/net/minecraft/world/entity/AgeableMob.java:37`):
  negative while a baby (ticks up toward `0`), positive as the post-breeding
  parent cooldown (ticks down toward `0`). `is_baby()` is `age < 0`.
  [`BABY_START_AGE`] is `-24_000`, [`PARENT_AGE_AFTER_BREEDING`] is `6_000`
  (`AgeableMob.java:31`, `Animal.java:44`). `age_locked: bool` freezes it
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
  immediately (vanilla `Animal::resetLove`,
  `Animal.java:227-228`), but does **not** spawn a child or apply the
  parent-age cooldown — this seam has no notion of the partner's identity or
  of creating a new entity. The host resolves `take_bred()` into an actual
  spawn.

## What now drives it in production

**Not yet fully wired** — the population-wide search and the "resolve a
`take_bred()` into a child" step belong in `MobSim::tick`
(`crates/lodestone-server/src/mobs.rs`), which is another agent's file. A
full patch is included in this issue's broker request; see the PR/commit
that lands it for the exact `crates/lodestone-server/src/mobs.rs` anchor.
Until that patch lands, `MobSim::tick` never calls `set_love_partner_candidate`/
`set_parent_candidate`, so `is_in_love()`/`is_baby()` stay reachable but
unfed in the one place that runs in a real game.

**What is proven today**: a driver-level test,
`crates/lodestone-entity/src/ai/navigating_mob.rs`'s
`breed_goal_drives_two_navigating_mobs_to_a_predicted_tick`, runs two real
`NavigatingMob`s through two real `GoalSelector`s holding the production
`BreedGoal`, with only the partner-candidate refresh played by the test
(exactly the population search the server patch will perform). It predicts
—not merely asserts the sign of—the tick breeding completes: vanilla's own
`BreedGoal.java:57` timer (`loveTime >= adjustedTickDelay(60)`) matches
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
  1–7 XP (`Animal.java:231`, `this.getRandom().nextInt(7) + 1`) and triggers
  `Stats.ANIMALS_BRED`/`CriteriaTriggers.BRED_ANIMALS`
  (`Animal.java:222-223`). `MobSim` has no experience-orb entity type and no
  stats/advancements plumbing at all — an honest scope cut, not a silent
  gap.
- **No food-item feeding.** `set_in_love()`/age-speedup-via-feeding
  (`Animal.mobInteract`, `Animal.java:140-164`) needs per-species food-item
  detection and a player-interaction path into `MobSim`; nothing here
  triggers it — a caller (or a test) must call `set_in_love()` directly for
  now.
- **Taming (#235) and leashing (#236) are untouched.** Neither
  `MobController` nor `goals.rs` has any surface for them yet — see the
  parent issue's per-family table for the honest breakdown.

## Configuration

No feature flags or env vars. The vanilla constants above are `pub const`s
on `lodestone_entity::ai`: [`LOVE_TICKS`], [`BABY_START_AGE`],
[`PARENT_AGE_AFTER_BREEDING`] (re-exported from `navigating_mob.rs` via
`ai/mod.rs`).

## Dependencies

- `lodestone-entity`'s `ai::goals::{BreedGoal, FollowParentGoal}` — the
  goals this wiring finally makes reachable.
- `crates/lodestone-server/src/mobs.rs`'s `MobSim`/`SimMob` — the intended
  production host; see `docs/live-mob-sim.md` for the tick loop this state
  needs to be fed from.

[`LOVE_TICKS`]: ../crates/lodestone-entity/src/ai/navigating_mob.rs
[`BABY_START_AGE`]: ../crates/lodestone-entity/src/ai/navigating_mob.rs
[`PARENT_AGE_AFTER_BREEDING`]: ../crates/lodestone-entity/src/ai/navigating_mob.rs
