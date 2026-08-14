# Pose-dependent dimensions and the fit gate

## What it is

The player's collision box is `0.6 × 1.8` standing, `0.6 × 1.5` crouching and
`0.6 × 0.6` swimming or elytra-gliding — and *which* of those applies is decided
by a **fit-gated state machine**, not by a pose→dimensions lookup. It lives in
[`crates/lodestone-physics/src/pose.rs`](../crates/lodestone-physics/src/pose.rs)
and runs from the end of `lodestone_physics::tick`.

Before this, the engine used `EntityDimensions::PLAYER` (`0.6 × 1.8`) for every
pose. Two visible consequences: **you could not swim through a one-block gap**,
and crouching did not shrink the box, so nothing you can only reach by crouching
was reachable. Both are horizontal-position disagreements with the server, which
rubber-bands above **0.25 blocks in a single packet with no accumulator** — and a
wrong hitbox produces exactly the failure mode this repo keeps rediscovering: the
screen looks right and the server disagrees.

## How it works

### The state machine, and the three things it is not

`Player.updatePlayerPose` is
the whole rule:

```java
protected void updatePlayerPose() {
   if (this.canPlayerFitWithinBlocksAndEntitiesWhen(Pose.SWIMMING)) {
      Pose desiredPose = this.getDesiredPose();
      Pose actualPose;
      if (this.isSpectator() || this.isPassenger() || this.canPlayerFitWithinBlocksAndEntitiesWhen(desiredPose)) {
         actualPose = desiredPose;
      } else if (this.canPlayerFitWithinBlocksAndEntitiesWhen(Pose.CROUCHING)) {
         actualPose = Pose.CROUCHING;
      } else {
         actualPose = Pose.SWIMMING;
      }
      this.setPose(actualPose);
   }
}
```

1. **The desired pose is vetoed, not applied.** `getDesiredPose()` picks
   `SLEEPING > SWIMMING > FALL_FLYING > SPIN_ATTACK > CROUCHING/STANDING`, and then
   `canPlayerFitWithinBlocksAndEntitiesWhen(pose)` — `level().noCollision(this,
   getDimensions(pose).makeBoundingBox(position()).deflate(1.0E-7))`
   — can refuse it. The fallback chain is `desired → CROUCHING → SWIMMING`.
2. **The whole body sits behind an outer guard.** If not even the `0.6 × 0.6`
   swimming box fits, `setPose` is never called *at all*: the pose is **sticky**,
   keeping whatever it was rather than collapsing to the smallest box. (Encased in
   blocks, you keep the pose you arrived in — including one that manifestly does
   not fit.) A port that reads the body as "fall back to the smallest box that
   fits" gets this backwards.
3. **There is no recovery if a player's box grows into a space it does not fit.**
   `Entity.refreshDimensions` calls
   `fudgePositionAfterSizeChange` only when
   `!this.level.isClientSide() && !firstTick && !noPhysics && isSmall && (grew) &&
   !(this instanceof Player)`. Both the client clause and the `Player` clause
   exclude us, so **the fit gate is the only thing preventing a surfacing swimmer
   from being clipped into a low ceiling.** Tying the box to
   `PlayerState::swimming` and skipping the gate is not "the same thing without the
   ceremony" — it is the one change that has nothing to catch it.

`getDesiredPose`'s crouch term is `isShiftKeyDown() && !abilities.flying` — the
**raw shift key**, the same input the edge back-off reads
(`Player.isStayingOnGroundSurface`), and *not* `isCrouching()`, which is derived from the pose
and would be circular.

### What a pose actually changes

`setPose` → `onSyncedDataUpdated(DATA_POSE)` → `refreshDimensions()`, which sets
`dimensions`, sets `eyeHeight = newDim.eyeHeight()`, and calls `reapplyPosition()`
(`setPos(position)`, rebuilding the box). Because `EntityDimensions.makeBoundingBox`
anchors `minY`
at the **feet**, a pose change never moves the
player: only the top face moves. Width is `0.6` in every pose a player can hold,
so the pose decides exactly two numbers — **box height** and **eye height**.

Those two are one record in vanilla and must not be split. Measured, in
`tests/pose_dimensions.rs::a_swimming_box_with_a_standing_eye_reports_dry_eyes_while_submerged`:
a `0.6`-high box with a `1.62` eye reports `eye_in_water == false` while twenty
blocks under water, because `EntityFluidInteraction.update`'s cell sweep is
bounded by the *box* and so never visits the eye's own cell. That single boolean
gates the submerged fog, the overlay, the ambient loop **and**
`updateSwimming`'s entry condition — so the split does not merely look wrong, it
makes the swim pose unenterable.

**Not to be confused with `lodestone_entity::pose`**, which is the *animation*
pose (`EntityPose`/`RenderPose`: walk phase, head tracking, swing timer). Vanilla
drives both from the same `net.minecraft.world.entity.Pose` value, so
`HANDOFF.md`'s "humanoid swim/crouch/ride/fall-flying poses are unported" render
gap now has a real, per-tick source to read instead of re-deriving one from
`swimming`/`sneak` — which would be the ungated derivation this doc exists to warn
against, one crate over.

`step_height` is **not** pose data. Vanilla's `EntityDimensions` record carries
width/height/eyeHeight only; step height is the `STEP_HEIGHT` attribute read
through `Entity.maxUpStep()`. A crouching player still steps `0.6`.

### Where it sits in the tick

```text
lodestone_physics::tick
  travel_and_check_inside_blocks     ← vanilla `super.tick()`
    baseTick fluid summary + updateSwimming
    travel → tick_water / tick_lava / tick_elytra / tick_air
    checkInsideBlocks (stuck multiplier)
  [tick_among_entities only] apply_entity_push   ← tail of aiStep
  update_player_pose                 ← Player.tick's LAST statement
```

Two orderings are load-bearing:

* The pose is decided **after** the move, so the box a tick's movement collides
  with is the pose decided at the end of the *previous* tick, and the gate always
  probes the **post-move** position.
* `pushEntities` is the tail of `aiStep`, *inside* `super.tick()`, and therefore
  runs **before** `updatePlayerPose`. That is observable, because the push's pair
  test reads `getBoundingBox()` — which the pose sizes. This is why
  `tick_among_entities` no longer calls `tick`: it calls the shared travel half,
  then the push, then the pose.

The narrower entry points (`tick_air`, `tick_water`, `tick_lava`, `tick_elytra`)
are vanilla's `travel`, not `Player.tick`. They **read** the pose for the box and
never write it. That is not tidiness: it is what makes 19 of the 32 pre-existing
golden traces provably pose-free.

### The entity half of the gate

`canPlayerFitWithinBlocksAndEntitiesWhen` is `noCollision`, i.e. blocks **and**
entities. An earlier investigation concluded this blocked the whole job; it does
not. `getEntityCollisions` filters on `Entity.canBeCollidedWith`, which `Entity`
answers `false` and **`LivingEntity` does not override** — the only three
overrides in 26.2 are `AbstractBoat`, `Shulker` and `HappyGhast`
(see [`entity-push.md`](./entity-push.md)). So for a player with none of those
inside its pose box the entity term is *vacuously true*.

Both forms exist: `can_player_fit_within_blocks_when` (block-only, what `tick`
uses) and `can_player_fit_within_blocks_and_entities_when` (the full predicate,
what `tick_among_entities` uses with its neighbour slice). `noBorderCollision` —
the third term — remains unmodelled, as it is everywhere else in this crate.

## How to change it, and the gotchas

* **`(double)0.6F` is not `0.6`.** The pose heights are `float` literals widened
  to `double`, and only `1.5F` is exact: `(double)0.6F == 0.6000000238418579`,
  `(double)1.8F == 1.7999999523162842`. The swimming box's top at feet `y = 1.0`
  is `1.6000000238418579`, and a hand-written `1.6` is wrong in the 8th place —
  the same order of magnitude as the `1.0E-7` deflation sitting next to it.
  Build boxes from `Pose::dimensions()`, never from decimals.
  `pose.rs::the_fit_box_is_deflated_and_the_heights_are_widened_f32` pins this.
* **A 1.5-block gap is a flush fit, and that is vanilla.** `(double)1.5F` is
  exact, so a crouching box under a top slab has its top *precisely* on the slab's
  underside. Both the strict `min < max` overlap and `collide`'s `1.0E-7`
  perpendicular epsilon admit it. This is the real "crouch under a top slab" case
  players use constantly, and `crouch_low_corridor` is its golden trace.
* **Adding a pose is mechanical, but check `getDesiredPose`'s order.**
  `Pose`'s doc table carries vanilla's dimensions for the three unmodelled poses
  (`SLEEPING` `fixed(0.2,0.2)` eye `0.2`, `SPIN_ATTACK` `scalable(0.6,0.6)` eye
  `0.4`, `DYING` `fixed(0.2,0.2)` eye `1.62`). `SLEEPING` is tested **first** in
  `getDesiredPose`, so a driver that adds sleep must add the pose in the same
  change or the machine will crouch a sleeping player.
* **The `isSpectator() || isPassenger()` bypass is deliberately absent.** Vanilla
  lets those two adopt an unfittable desired pose; this crate has neither, and its
  absence is conservative (it can only refuse a pose vanilla would have granted).
  A driver adding vehicles must apply the bypass itself.
* **`PlayerState::eye_height` is now an output mirror.** Nothing inside `tick`
  reads it — both consumers (`compute_fluid_state`'s eye sweep and
  `getFluidJumpThreshold`) call `Pose::eye_height()` directly, exactly as vanilla
  has one pose-derived `eyeHeight` field that cannot disagree with the box. So an
  out-of-band write to the field can mislead the camera and can never move the
  player; `tick_ignores_an_out_of_band_eye_height_write` is that gate. `with_pose`
  sets both; prefer it to `with_eye_height`.
* **The sneak bit must reach the server, and it already does.** The pose reads
  `MovementInput::sneak`, the same bit `lodestone-controller`'s `movement_intent`
  sends as `ServerboundPlayerInputPacket::shift`
  (`send_player_input`, `crates/lodestone-controller/src/ecs.rs`), which is what makes the server
  call `setShiftKeyDown(true)` and run *its* `updatePlayerPose` the same way. This
  is [`edge-back-off.md`](./edge-back-off.md)'s "inverse trap" with a second
  consequence: a headless/bot path that crouches locally without sending the bit
  now disagrees about the **hitbox** as well as the edge back-off, and a
  1.8-vs-1.5 box disagreement is far more visible than a back-off one. Set both or
  neither.
* **The navigator still plans for a standing player.** `lodestone-nav`'s
  `BODY_HEIGHT` is `EntityDimensions::PLAYER.height` (`1.8`), so it will not route
  through a one-block gap even though a swimmer now fits one. That is a
  deliberate non-goal here, not an oversight — a pose-aware planner needs a
  pose-aware *edge* model (you can only cross a swim-height gap while sprint-
  swimming in water), which is a search-space change rather than a constant.

### The `lodestone-ecs` change this is still owed

`lodestone_ecs::player::player_physics` calls `lodestone_physics::tick` and then
its own `update_pose` (`crates/lodestone-ecs/src/player.rs`), which writes
`eye_height` from `swimming`/`intent.sneak` alone — **ungated**. Since physics now
owns the decision, that write is redundant in every case except the one where it
is wrong: with the desired pose vetoed (crouch forced under a ≤1.8 ceiling while
shift is *not* held) it writes `1.62` over the pose's `1.27`. The box is
unaffected — it comes from `pose` — so the residue is confined to the camera and
the fog. Two edits close it, neither of which this change was allowed to make:

1. **Move `update_pose` under the `PlayerCollision::Pending` arm only**, and give
   it the ungated body it already has, with a comment saying *why* it is ungated
   there: with no `CollisionView` there is nothing to gate against, and inventing a
   world to probe would be worse than an explicit approximation. On the
   `PlayerCollision::View` path, drop the call entirely — `tick` has already set
   both `pose` and `eye_height`. This keeps
   `pending_updates_the_pose_while_no_world_does_not` passing as written.
   Better still, have the `Pending` body set the pose too:
   `*player = player.with_pose(lodestone_physics::desired_pose(player, intent))`,
   so the two fields stay coupled even on the degraded path.
2. **`fly_step` must reset the pose, not just the eye height**
   (`player.rs`). It already clears `swimming` and writes
   `DEFAULT_EYE_HEIGHT`, for exactly the reason given in its comment: free-fly
   never calls `tick`, so nothing else would. Add
   `player.pose = lodestone_physics::Pose::Standing;` beside it — vanilla's
   `getDesiredPose` returns `STANDING` while `abilities.flying` and
   `updatePlayerPose` runs on every `Player.tick`. Without it, a swimmer who takes
   off keeps a `0.6`-high box for the whole flight, and the first physics-walk tick
   after landing collides with it before the machine can correct.

Everything else in the shell needs no change: `player_physics` already calls
`lodestone_physics::tick`, so the machine is consumed by the real per-tick path
from the moment it lands — it is not an island.

## Configuration

None. No feature flag, no env var, no tunable.

## Dependencies

* `lodestone-physics::collision::no_collision` — the block half of the gate.
* `lodestone-physics::push::no_collision_among_entities` — the full predicate,
  used by `tick_among_entities`.
* `lodestone-physics::entity::EntityDimensions` — `Pose::Standing.dimensions()`
  **is** `EntityDimensions::PLAYER` (asserted, so the pose path and every
  non-pose caller cannot drift).
* Consumers: `lodestone_physics::tick` / `tick_among_entities`, and through them
  `lodestone_ecs::player::player_physics`.

## Gates

| gate | where | what it pins |
|---|---|---|
| the machine | `src/pose.rs::tests` (8 tests) | the fallback chain, the outer guard's stickiness, `getDesiredPose`'s priority order, the widened `f32` heights and the `1e-7` deflation, step height being pose-independent, `Standing == EntityDimensions::PLAYER`, and the entity term with a shulker |
| bit-exact trajectories vs the Python oracle | `tests/golden.rs`: `swim_gap_tunnel`, `swim_gap_blocked_control`, `crouch_low_corridor`, `stand_low_corridor_control`, `crouch_release_stays_crouched`, `elytra_gap_glide` | the pose-sized box through the whole tick, zero tolerance |
| the seams | `tests/pose_dimensions.rs` (6 tests) | box/eye coupling and the cost of splitting it, `tick`'s immunity to an out-of-band `eye_height` write, `travel` not touching the pose, the push pair test reading the pose box, and "an unfittable seeded pose shrinks and never displaces the player" |

Every one carries a control that must fail the same assertion: the two taller
poses refused in the same one-block gap at the same position; the ceiling removed
so the machine *does* revert to standing; the same corridor walked with shift
released, jamming flush on `x = 1.0`; the standing eye height on a swimming box
reading dry; and `tick_air` leaving the pose alone where `tick` crouches.

### Which traces exercise the gate, and which provably cannot

Counted rather than reasoned about — the generator was instrumented to record
every pose `update_player_pose` committed, per scenario. Of the 32 pre-existing
traces:

* **19 never run the machine at all.** They replay through `tick_air` /
  `tick_water` / `tick_elytra` directly, which are `travel`, not `Player.tick`.
  (Among them all three `sneak_edge_*` traces and `ladder_sneak_hold` — the ones
  that *look* like they should have started crouching.)
* **11 run it and only ever hold `STANDING`**: `lava_sink`, `levitation`,
  `slow_falling_water`, `soul_sand_walk`, `jump_boost`, `honey_jump`,
  `slime_bounce`, `water_current_push` and the three `entity_push_*`. Desired
  pose `STANDING`, granted, box unchanged.
* **exactly two hold a smaller box, and both are byte-identical anyway.**
  `slime_bounce_sneak` crouches (`1.8 → 1.5`) on a flat slime floor with nothing
  above head height, so the shorter top face intersects the same — empty — set of
  cells. `swim_sprint` enters `SWIMMING` (`1.8 → 0.6`) in a 5×5×21 open water
  shaft with **no solid blocks at all**, so `gather` returns nothing for any box
  and its `is_in_water` scan lands on water at every `y` the shorter box spans.

The six new traces hold `CROUCHING`, `SWIMMING` and `FALL_FLYING` against real
geometry, and their two world controls hold `STANDING` throughout — which is what
makes them controls.

That is the argument; the regeneration is the measurement. The **control was run
first, before any change**: regenerating the checked-in
`support/golden_traces.rs` with the unmodified generator produced an empty diff,
proving the file was not already drifted (had it been, "byte-identical" would have
meant nothing). Regenerating after adding the machine *and* the six scenarios
produced **2664 insertions and 0 deletions**.

## The live gate that is owed

Nothing here has been run against a real server. The recipe follows
[`edge-back-off.md`](./edge-back-off.md)'s shape, and its traps apply unchanged:

1. **It must be the survival oracle** (`./scripts/live-oracles/survival.sh`). The
   rubber-band check is skipped for `isCreative()`
   (`ServerGamePacketListenerImpl.handleMovePlayer`), so `creative.sh` gives a
   guaranteed vacuous pass.
2. **Read `Sim::teleport_count`** (`crates/lodestone-shell/src/sim.rs`).
3. **Run the *unpatched* build first and confirm the counter does increment**, or
   "no corrections" is the duration species of vacuous test. This is a case where
   the control should fire loudly: with the old always-`1.8` box, a client trying
   to swim into a one-block gap claims positions the server's own replay refuses
   outright, so the disagreement is not a slow drift — it grows by a full tick of
   swim speed (~0.2 blocks) every tick until it passes `0.25` in a single packet.

Setup for the swim case, all RCON:

```
/setblock <x> <y> <z> water …            # build a 1-high flooded tunnel off a pool
/effect give @s minecraft:water_breathing 300
```

and sprint-swim in. The crouch case is simpler and needs no fixture beyond a top
slab at head height: stand under it, release shift, and walk. Both should report
zero corrections; the unpatched build should report many.

Note that the *server* runs the same `updatePlayerPose` on its `ServerPlayer`
(it is on `Player`, not `LocalPlayer`), driven by the `shift` bit in
`ServerboundPlayerInputPacket` and by its own `updateSwimming` — which is why
`docs/swimming.md`'s `PlayerCommand` sprint edge is a precondition for the swim
half of this gate. Send only `SetPlayerInput` and the server's pose never becomes
`SWIMMING`, so it will refuse a movement the client thinks is legal.

## Re-checked for camera jerk, and cleared

An earlier report described a camera jerk entering/leaving swim mode and asked whether
pose oscillation — this gate flipping pose on consecutive ticks — could be a
second cause distinct from the eye-height snap `docs/swimming.md` documents as
the real one. It is not, and the two properties that rule it out were
re-verified rather than assumed:

* **The gate is a pure function of position, run once per tick, with no
  hysteresis.** `update_player_pose` reads `state.position` and the current
  world only; nothing here remembers "how long has it held this pose" or
  damps a rapid flip. If a caller's own inputs (position, `sneak`, `swimming`)
  genuinely oscillate tick to tick — hovering exactly at a submersion boundary,
  say — the pose *would* legitimately follow, but that is the caller feeding
  it oscillating state, not a bug in the gate reacting to stable state.
* **`min_y` is anchored at the feet, so only the top face ever moves.**
  Re-read directly off `EntityDimensions::bounding_box`
  (`crates/lodestone-physics/src/entity.rs`): `feet.y` is the box's
  `min_y` unconditionally, and `feet.y + height` is `max_y`. A pose change
  cannot be "the origin is wrong" here — there is no code path that anchors
  the box anywhere but the feet.

Neither check found anything specific to that report; both simply re-confirm
what this doc already documents above. The eye-height snap
(`docs/swimming.md`'s `EyeHeightSmoother` section) remains the one identified
cause of the camera jerk.
