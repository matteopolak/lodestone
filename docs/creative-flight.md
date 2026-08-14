# Creative flight

Vanilla's creative/spectator-style flight: the double-tap-space toggle, the
`Abilities` record that grants it, the physics modifications it applies, and the outbound echo
that keeps the server in agreement.

## What it is

Two things, and separating them was the whole design:

1. **Creative flight** — `Abilities.flying`. Server-granted, **collides with blocks**, runs
   vanilla's ordinary `travelInAir` arithmetic with three modifications, and it is the only
   flight the client has now.
2. **Free-fly / noclip** — `lodestone_ecs::player::Flying`. A *developer* camera: local, no
   collision, `position += dir * speed` with no velocity or drag. It predated creative flight,
   which deliberately kept it as a separate affordance — and **was later deleted as a way in.**

### The free-cam has no route from the shell any more

The user's call, and it supersedes the earlier decision to keep it: *"delete all of the nonstandard
debug things we added. we can accomplish that stuff with the real cheat commands."* Between the
real double-tap-space flight and `/gamemode creative`, the free-cam was redundant twice over, and
it was squatting on `F` — vanilla's `key.swapOffhand` — which blocked another feature.

Deleted: `InputAction::ToggleFly` / `key.lodestone.toggleFly`, its `resolve_key` arm and
`KeyOutcome::ToggleFly`, `Sim::flying`/`Sim::toggle_fly`, and the F3 overlay's `MODE FLY/WALK`
readout. **Not** deleted: `lodestone_ecs::player::Flying` itself and `fly_step`. That is a
deliberate stop, not an oversight — `lodestone-ecs` was held by another agent at the time, and
`interact.rs`'s `send_sprint_command` still reads the component. The component now has no writer,
so `fly_step` is unreachable in practice; removing it is a follow-up.

The two modes were never the same and must not be conflated. Using `Flying` to implement creative
flight would noclip where vanilla collides *and* run arithmetic the server's movement check would
correct.

## The bug this closed first

`ClientEvent::AbilitiesChanged` was a **complete island**. It was decoded correctly in
`crates/protocol/v770/src/adapter/player.rs`'s `V770Adapter::handle_play_player`, unit-tested at the protocol layer, and round-tripped
in `lodestone-model`'s own tests — and consumed **nowhere**. `grep -c AbilitiesChanged` returned
`0` in both `lodestone-ecs/src/ingest.rs` and `lodestone-shell/src/sim.rs`.

The consequence was player-visible and independent of the feature: `Flying` was a purely local
toggle with no relationship to `mayfly`, so **the client would free-cam on a server that never
granted flight**. Whether a player may fly is server authority.

`ClientAction::SetFlying` was a **second island, outbound**: encoded by four protocol adapters
(`v47`, `v340`, `v735`, `v770`) with zero producers outside their own tests.

Both are closed. The factory in both directions is the routing switch — see "How it works".

## How it works

### The consumer chain, end to end

```
ClientboundPlayerAbilitiesPacket
  → v770 adapter.rs:3301                       decode (already existed)
  → ClientEvent::AbilitiesChanged
  → session::handles_event                     ← THE NEW SWITCH ARM
  → session::apply_local_player_state          fold, whole-record assign
  → session::Abilities { flying, may_fly, flying_speed, … }
  → player::player_physics                     PlayerState::with_flight(…)
  → lodestone_physics::tick                    the three modifications
  → the player moves on screen
```

and back out:

```
player::apply_creative_flight_input            double-tap toggle, gated on may_fly
  → session::Abilities.flying
  → interact::send_abilities                   ← THE NEW PRODUCER
  → ClientAction::SetFlying
  → v770 adapter.rs:4136 → ServerboundPlayerAbilitiesPacket
```

**The switch arm is the load-bearing line.** `SharedState::apply` forwards only events that
`ingest::handles_event` or `session::handles_event` lists; without an arm, a perfect decode and a
correct system reach zero pixels. This island has now been found four times
(`EntityDamaged`/`EntityHurtAnimation`, air supply, `EntityAnimation`, and this). A hermetic test
that calls the fold directly passes either way, which is why
`abilities_changed_is_claimed_by_this_module_and_not_by_ingest` asserts the *switch*, not the fold.

### Flight is a wrapper, not a fourth travel mode

The issue body proposed `tick_fly` alongside `tick_air`/`tick_water`/`tick_elytra`. **That shape
cannot reproduce vanilla.** `Player.travel` *wraps*
`super.travel(input)`:

```java
if (this.getAbilities().flying) {
   double originalMovementY = this.getDeltaMovement().y;
   super.travel(input);
   this.setDeltaMovement(this.getDeltaMovement().with(Direction.Axis.Y, originalMovementY * 0.6));
} else {
   super.travel(input);
}
```

So flight is three modifications to machinery that already existed, all in
`lodestone_physics::player`:

| # | what | where |
|---|---|---|
| 1 | **dispatch suppressor** — `isAffectedByFluids()` is `!flying`, so a flying player never takes the fluid branch | `travel_and_check_inside_blocks` |
| 2 | **speed substitution** — `getFrictionInfluencedSpeed` returns `getFlyingSpeed()` when airborne | `player_flying_speed` → `AirTravelContext::flying_speed` |
| 3 | **post-travel Y overwrite** — the *pre*-travel Y × `0.6` **replaces** the post-travel Y | `travel_and_check_inside_blocks` |

Three corrections to how this is usually described, each verified against the jar:

* The `0.6` is **Y-axis only**. There is no horizontal drag term, however much `0.6` looks like
  one. `there_is_no_horizontal_drag_term_in_the_flight_branch` pins it.
* It lives in `Player.travel`, not `LivingEntity.travel`.
* It multiplies the **pre**-travel Y and *overwrites* the post-travel one — gravity's contribution
  during the tick is **discarded**, not damped.

The suppressor is the strongest discriminator between the wrapper shape and the arm shape: a
player flying through water goes through `travelInAir`, keeping flight speed with no `0.8` water
slow-down, no buoyancy and no sneak-to-sink. `a_flying_player_in_water_takes_the_air_path` is that
trace.

### `getFlyingSpeed()` has four arms, and two of them are not about flight

`Player.getFlyingSpeed()`:

```java
if (this.abilities.flying && !this.isPassenger()) {
   return this.isSprinting() ? this.abilities.getFlyingSpeed() * 2.0F : this.abilities.getFlyingSpeed();
} else {
   return this.isSprinting() ? 0.025999999F : 0.02F;
}
```

**`PhysicsProfile::flying_speed = 0.02` was already the *non*-flying value** despite its name —
`getFlyingSpeed` is the airborne substitute for `getSpeed()` and predates creative flight sharing
the word. Reusing it as the flight speed would fly at 40% of vanilla's rate and merely look "a bit
slow". Creative flight's speed is `Abilities.flyingSpeed`, default `0.05F`, doubled while
sprinting, and **server-settable** — so it rides on `PlayerState::flying_speed`, not the profile.

**The non-flying sprinting arm was missing entirely**, which was a live bug with no creative
flight involved: `friction_influenced_speed_value` returned a flat `profile.flying_speed` in its
airborne branch, so **every sprint-jump accelerated at `0.02` instead of `0.025999999` — 30%
short**. See "The oracle agreed with the bug" below.

The literal is `0.025999999F`, **not** `0.026F`. They are different `f32`s
(`0.025999999046325684` vs `0.026000000536441803`), `input_vector` is linear in the value, so the
tidier constant is observable in the reported position. `the_sprint_literal_is_not_0_026` pins the
bits.

### The `!flying` conjuncts

Thirteen sites, each applied where vanilla applies it. Twelve were tabulated in the original
research comment; **the first one below was not, and it is the one that matters most at ground level.**

| behaviour while flying | source | where |
|---|---|---|
| **no `jumpFromGround` at all** — the whole aiStep jump block is gated on `isAffectedByFluids()` | `LivingEntity.aiStep` | `tick_air` |
| fluid travel branch suppressed | `Player.isAffectedByFluids` | `travel_and_check_inside_blocks` |
| `resetFallDistance()` every tick, **pre**-travel | `Player.aiStep` | `travel_and_check_inside_blocks` |
| `getBlockSpeedFactor()` → `1.0F` (also while gliding) | `Player.getBlockSpeedFactor` | `MoveContext::suppress_block_speed_factor` |
| `onClimbable()` → `false` | `Player.onClimbable` | `on_climbable`, `AirTravelContext::suppress_climbable` |
| `updateSwimming()` → `setSwimming(false)` | `Player.updateSwimming` | `travel_and_check_inside_blocks` |
| no crouch pose (shift is *descend*) | `Player.getDesiredPose` | `pose::desired_pose` |
| `maybeBackOffFromEdge` → identity | `Player.maybeBackOffFromEdge` | `AirTravelContext::edge_back_off` |
| `makeStuckInBlock` skipped | `Player.makeStuckInBlock` | `travel_and_check_inside_blocks` |
| bubble-column impulse skipped | `Player.onAboveBubbleColumn` / `Player.onInsideBubbleColumn` | `travel_and_check_inside_blocks` |
| `isPushedByFluid()` → `false` | `Player.isPushedByFluid` | covered by the suppressor (see gotchas) |
| `canGlide()` → `false` | `Player.canGlide` | driver: not modelled, see divergences |
| landing cancels flight | `LocalPlayer.aiStep` | `cancel_flight_on_landing` |

The edge back-off one is worth noting: `EdgeBackOff`'s doc used to record `!abilities.flying` as
"satisfied by construction — this crate does not model creative flight at all". **That argument
has expired**, and the gate is now real. It is a good example of why a "vacuously true" note needs
re-checking when the premise changes rather than being trusted.

### The client half

`LocalPlayer.aiStep` splits around `super.aiStep()`, and the order is observable, so it is two
systems chained around `player_physics`:

```
apply_creative_flight_input   →  player_physics  →  cancel_flight_on_landing
   (toggle, ±flyingSpeed*3)      (the travel)       (onGround && flying)
```

* **The toggle** is `jumpTriggerTime`: the first jump *press* while `mayfly` sets it to `7`; a
  second press inside that seven-tick window flips `flying`. Gated on `mayfly` — on a survival
  server this system does nothing and space just jumps.
* **The vertical impulse** is `inputYa * abilities.getFlyingSpeed() * 3.0F` using the **raw**
  ability speed, *not* the sprint-doubled `Player.getFlyingSpeed()`. Sprinting doubles horizontal
  flight and leaves the climb rate alone.
* **Landing cancels flight** *after* the move, because `on_ground` is written by that move.
  Reading it before would cancel one tick early — visible as flight cutting out just before
  touchdown.

## How to change it, and the gotchas

* **Adding a `!flying` gate: put it where vanilla puts it.** The three modifications are in
  `travel_and_check_inside_blocks`; per-site conjuncts belong at their own site. Do not add a
  `tick_fly`.
* **The Y-overwrite capture point reads through `snap_small_velocity` non-destructively.** Vanilla
  reads the Y inside `travel()`, which is *after* aiStep's snap-to-zero prologue — and that
  prologue lives inside our per-path `tick_air`/`tick_elytra`, not in the caller. Deriving the
  capture by applying the snap without mutating is provably vanilla's value **because the only
  other writer between the snap and `travel()` is the jump block, which flight suppresses**. If
  that ever stops being true, this breaks quietly: capturing pre-snap leaves a residual
  `1.8e-3 × 0.6^n` that decays but never reaches zero, is invisible in position, and then appears
  as a ~0.001 offset the next time the player climbs.
  `a_hovering_flier_settles_to_exactly_zero_vertical_velocity` is the guard.
* **`fall_distance` is not zero at the end of a flying tick, and that is correct.** The reset is
  pre-travel (`Player.aiStep`, before `super.aiStep()`), and `Entity.move`'s own
  `checkFallDamage` then accumulates *this* tick's descent. What the reset buys is that it cannot
  accumulate **across** ticks. Asserting `== 0.0` is wrong about vanilla in a way that looks more
  correct — it cost a test iteration here.
* **`PhysicsProfile::flying_speed` is `0.02` and is not the flight speed.** See above. The two are
  2.5× apart and vanilla calls both "flying speed".
* **New `AirTravelContext` / `MoveContext` fields must be inert at `Default`.** Both new context
  fields were added as `Option<f32>`/`bool` defaulting to "behave exactly as before", because
  `travel_in_air` is a public seam shared with mobs and its doc invites `..default()`. The proof
  that this worked is in "the control" below: reverting one oracle line reproduced all 31,620
  checked-in golden values bit-for-bit, so nothing else in the change set moved anything.
* **`isPushedByFluid()` is covered only incidentally.** In vanilla the fluid push is in
  `baseTick`'s `updateFluidInteraction`, *outside* `travel`; here `apply_fluid_push` is called at
  the top of `tick_water`, which the suppressor makes unreachable while flying. Same answer, but
  for a different structural reason — if the push ever moves out of `tick_water`, it needs its own
  `!flying` gate.
* **A driver that adds vehicles must handle `!isPassenger()` itself.** Vanilla falls to the
  *non*-flying arm of `getFlyingSpeed()` for a passenger; it does not merely skip the doubling.

## Divergences, deliberate

Each is pinned by a test, so it fails loudly if the assumption changes.

* **Spectator mode is deferred, not half-modelled.** `Player.tick` sets
  `this.noPhysics = this.isSpectator()`, which makes `Entity.move` skip
  collision resolution entirely; spectators also cannot interact with blocks and are not pickable.
  **None of that is modelled.** The only spectator-aware line in the whole change is the
  `!isSpectator()` conjunct in `cancel_flight_on_landing`, so a spectator keeps flying instead of
  being dropped on contact with the ground.
  `spectator_noclip_is_not_modelled_and_this_pins_that` asserts the gap explicitly.
  Closing it needs (a) a `no_physics` path through `move_entity`/`collide`, and (b) block-interaction
  suppression in the shell. Stating the gap beats a partial model that looks finished — the ruling
  `docs/edge-back-off.md` records for its own sibling case.
* **The one-shot hop on engaging flight while standing is not modelled.** Vanilla does
  `if (abilities.flying && this.onGround()) this.jumpFromGround();` on the toggle edge, in
  `LocalPlayer.aiStep`. `jump_from_ground` is private to `lodestone-physics` and needs a
  `CollisionView` for `getBlockJumpFactor`, which `apply_creative_flight_input` does not hold. Cost:
  a slightly less snappy takeoff (a one-tick `+0.42` Y). Flight itself is unaffected.
* **`canGlide()`'s `!flying` conjunct is not modelled**, because nothing in this repo starts an
  elytra glide from the client yet (`tryToStartFallFlying` has no port). `fall_flying` arrives as
  server entity data, and the travel dispatch honours it exactly as vanilla's does if both bits are
  somehow set.
* **`getMovementEmission()`'s `flying` arm** (`Player.getMovementEmission`) is sound, not physics.
* **`isControlledCamera()`** is vacuously true — this engine has no camera possession.

## The oracle agreed with the bug

`tests/gen_golden.py` is this crate's Python oracle. Its `tick_air` function read
`speed = P.flying_speed` — the **same** defect as the Rust, with no sprint term. So the two ports
agreed with each other and both disagreed with the jar, and `GOLDEN_SPRINT_JUMP` encoded the wrong
airborne acceleration for as long as it had existed.

This is exactly why *"a self-authored oracle validates the behaviour you chose to model"*, and why
every expected value in `tests/creative_flight.rs` is hand-derived from a Java literal rather than
produced by calling the crate's own helper.

### The controls, and watching them fail

1. **The fix breaks the goldens, as it must.** Before regenerating, the suite went red on exactly
   three traces — `sprint_jump`, `swim_sprint`, `swim_look_down_dives` — and they are precisely the
   three where a **sprinting** player is airborne. 44 of 47 traces were untouched.
2. **The isolation control.** Reverting *only* the one oracle line and regenerating reproduced the
   checked-in file's numbers with **0 of 31,620 values differing**. That is the proof that every
   other edit in this change set — two new `AirTravelContext` fields, a new `MoveContext` field, a
   changed `friction_influenced_speed_value` signature — is inert for existing behaviour.
3. **A near-miss worth recording.** The first attempt at control 2 parsed
   `golden_traces.rs` line-by-line for `GoldenTick {` and reported *all 47 consts changed,
   including `FREE_FALL`* — which has no sprint and no input, so it could not possibly have moved.
   The cause: the checked-in file is `cargo fmt`-expanded (21,282 lines) while the generator emits
   one line per tick (5,473), so the "semantic" comparison was comparing **formatting**. Re-parsing
   format-independently (every `0x…` token per const, in order) gave the real answer: 3 consts,
   626 values. The lesson is the one `CLAUDE.md` already records about `diff | grep -c '^<'` —
   *a control whose premise is false fails in the safe-looking direction.* Ask what else could
   produce the signal.
4. Regenerating therefore also required `rustfmt` on that **one** file to restore its committed
   shape, which turns a 20,251-line reformat into a **626-added / 626-deleted** diff that matches
   the semantic count exactly.

## Configuration

Nothing tunable. Everything is server-supplied or a vanilla constant.

| value | default | source |
|---|---|---|
| `Abilities.flyingSpeed` | `0.05F` | server, per-player (`player_abilities`) |
| `Abilities.mayfly` | `false` | server — **the gate** |
| `PhysicsProfile::flying_speed` | `0.02F` | non-flying airborne, not flight |
| `PhysicsProfile::airborne_sprint_speed` | `0.025999999F` | non-flying airborne, sprinting |
| the Y overwrite | `0.6` | `Player.travel` |
| `jumpTriggerTime` window | `7` ticks | `LocalPlayer.aiStep` |
| vertical impulse | `flyingSpeed * 3.0F` | `LocalPlayer.aiStep` |

The developer free-fly camera's speed is `lodestone_ecs::player::FLY_SPEED`, unrelated.

## Dependencies

* `lodestone-physics` — `PlayerState::{flying, flying_speed}`, `player_flying_speed`, the three
  modifications, the conjuncts.
* `lodestone-ecs` — `session::Abilities` (fold + switch arm), `player::{apply_creative_flight_input,
  cancel_flight_on_landing, JumpTriggerTime, WasJumping, LastFlyingSent}`.
* `lodestone-shell` — `interact::send_abilities` (the outbound echo).
* `lodestone-model` — `ClientEvent::AbilitiesChanged`, `ClientAction::SetFlying`.
* `protocol/v770` — both packets. Decode and encode both pre-existed; only the producers/consumers
  were missing.

## Related

* `docs/edge-back-off.md` — the `!flying` conjunct that stopped being vacuous, and the
  partial-model ruling this doc follows for spectator.
* `docs/swimming.md` — the fluid branch the suppressor bypasses.
* `docs/session-components.md` — where `Abilities` sits.
* `docs/block-physics-constants.md` — `getBlockSpeedFactor`.
