# Mob vertical motion: auto-step, jump, and fall

## What it is

`NavigatingMob::step_vertical` (`crates/lodestone-entity/src/ai/navigating_mob.rs`) —
the one function that decides a path-following mob's height each tick: an instant
auto-step over a small ledge, a real gravity-accelerated jump arc over a taller one, or a
gravity-accelerated fall down a drop. It is the vertical half of
[`NavigatingMob::advance`](../crates/lodestone-entity/src/ai/navigating_mob.rs), whose
horizontal half moves the mob toward its current waypoint by up to `step_per_tick` blocks
(see `docs/mob-species-spawning.md` for how that per-tick rate itself is derived).

## How it works

Every call is one of three cases, keyed on `dy = waypoint_y - pos_y` and whether a jump or
fall is already in progress (`fall_speed != 0.0`, a positive-down signed velocity carried
between calls):

- **Auto-step** (`fall_speed == 0.0 && 0.0 <= dy <= max_up_step`): resolves instantly,
  `pos.y = waypoint_y`. This matches the real vanilla mechanic it ports — a step within the
  mob's own step-height attribute (`species_shape`'s `step_height` fold, per species) is
  absorbed by the same tick's collision response, with no visible pause and no jump at all.
- **Jump** (`fall_speed == 0.0 && dy > max_up_step`): the real vanilla mechanic this used to
  skip. A rise taller than auto-step range is not resolved by climbing at the step rate —
  that reads as sliding up a ramp, not jumping, which is the "unnatural" symptom this fixes.
  Instead the follower seeds `fall_speed = -JUMP_POWER` (a fixed `0.42` blocks/tick upward
  speed — the default block-jump-factor, no-Jump-Boost case; `docs/autonomous-navigation.md`
  independently derives the same `0.42` constant for its own jump-apex simulation, which is
  worth treating as cross-validation of the number) and integrates one tick of real
  projectile motion: this tick's displacement is the *current* stored speed (used before
  gravity reduces it, matching the real order the port comes from — velocity moves the body
  first, then gravity/drag update the stored speed for the *next* tick), then gravity adds
  and drag decays the speed for storage. That order is why the peak the mob actually reaches
  is close to the well-known real jump height (measured here at **≈1.252 blocks** after 6
  ticks of ascent from a dead stop, comfortably clearing a full block) — front-loading
  gravity into the same tick's displacement (the fall case below) measurably undershoots the
  peak instead, which is why the two cases are integrated differently.
- **Fall / jump-descent** (`fall_speed != 0.0`, or `fall_speed == 0.0 && dy < 0.0`): a
  gravity-accelerated descent, landing exactly on `waypoint_y` and resetting `fall_speed` to
  `0.0` the tick it does. A jump's ascent hands off into this same branch once its stored
  speed crosses back to non-negative (the arc's peak), so one mechanism carries a jump
  through its whole rise-then-fall — there is no separate "jump" state machine, only the sign
  of `fall_speed`.

## How to change it, and the gotchas

- **The jump and fall branches integrate gravity/drag in *different orders*, deliberately.**
  The jump branch moves by the speed stored *before* this tick's gravity/drag update (so the
  full `0.42` launch speed applies to the very first tick's displacement); the fall branch
  folds gravity into the *same* tick's displacement before moving. Unifying them onto one
  order was tried and rejected: doing the fall branch's front-loaded order for the ascent
  measurably shortchanges the peak (~0.85 blocks reached instead of ~1.25 for the same launch
  speed), which would make mobs fail to clear a plain 1-block ledge — an actual functional
  regression, not just a smoothness difference. Do not "simplify" this into one formula
  without re-checking the peak height it produces.
- **The auto-step/jump split is keyed on `max_up_step`, which is per-species**
  (`species_shape`'s `step_height` fold — see `docs/mob-species-spawning.md`). A species with
  an unusually tall step height jumps less often, exactly as vanilla's own `mob.maxUpStep()`
  gate does.
- **A jump only ever *starts* from `fall_speed == 0.0`** (grounded, no motion in progress).
  There is no re-triggering mid-arc, matching vanilla's own `MoveControl`, which stays in its
  `JUMPING` operation (ignoring new jump requests) until the mob is back on the ground.
- **The landing clamp (`new_pos <= waypoint_y`, reset `fall_speed` to `0.0`) only fires while
  descending** (`displacement > 0.0`, this crate's positive-down convention). Clamping on the
  *ascending* half by mistake would cut a jump's rise short the instant `pos_y` first crosses
  `waypoint_y` on the way up, which is usually well below the arc's real peak.

## Configuration

`JUMP_POWER` (`0.42`), `FALL_GRAVITY_PER_TICK` (`0.08`), and `FALL_VERTICAL_AIR_DRAG`
(`0.98`) are the three tunables, all `pub const` in `navigating_mob.rs`. They are transcribed
vanilla defaults (default jump strength, base gravity, base vertical air drag), not tuned
values — changing them changes jump height and fall speed for every mob using this follower.

## Dependencies

Pure arithmetic — `step_vertical` takes plain `f64`s and touches no world state. The caller,
`NavigatingMob::advance`, supplies `max_up_step` from `MobShape` (`crates/lodestone-entity/src/pathfinding/world.rs`)
and folds the result into the mob's reported `velocity` for yaw-facing and any consumer that
reads it.

## Evidence

| claim | where |
|---|---|
| a rise within `max_up_step` resolves in one call, unchanged from before this doc | `navigating_mob.rs`, `a_rise_within_max_up_step_resolves_in_one_call_and_a_larger_one_does_not` |
| a rise beyond `max_up_step` follows a real jump arc, not an instant glide, with the bound proven load-bearing by an `INFINITY`-step control | `navigating_mob.rs`, `removing_the_step_bound_reproduces_the_glide_bug` |
| the jump peak lands close to vanilla's real jump height and clears a full block | `navigating_mob.rs`, `a_jump_over_a_full_block_reaches_the_real_vanilla_peak_height` |
| a jump that starts mid-arc is not re-triggered by a second call | `navigating_mob.rs`, `a_jump_already_in_progress_is_not_re_triggered` |
| the descent after a jump peak lands exactly on the waypoint and resets the stored speed | `navigating_mob.rs`, `a_jump_lands_exactly_on_the_waypoint_after_its_arc` |
| an ordinary fall (no jump) is unaffected by the jump branch | `navigating_mob.rs`, `a_fall_accelerates_under_gravity_instead_of_snapping_to_the_landing_height`, `a_fall_lands_exactly_on_the_surface_and_resets_afterwards` |
