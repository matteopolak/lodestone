# `maybeBackOffFromEdge` — the sneak-at-a-ledge back-off

## What it is

`Player.maybeBackOffFromEdge` is the vanilla rule that stops you walking off a drop
while shift is held. It lives in `lodestone-physics` as
`player::maybe_back_off_from_edge`, called from inside
[`entity::move_entity`](../crates/lodestone-physics/src/entity.rs) — the shared
`Entity.move` core — and selected per entity by
`MoveContext::edge_back_off: EdgeBackOff`.

**It is a desync rule, not a feel rule.** The server replays the movement we claim
through `player.move(MoverType.PLAYER, …)`, inside
`ServerGamePacketListenerImpl.handleMovePlayer`,
and `MoverType.PLAYER` is one of the two mover types this rule's own gate
(`Player.maybeBackOffFromEdge`) admits. `handleMovePlayer` then compares the
replay result against our claimed position and teleports us back when
`movedDist > 0.0625` — **0.25 blocks in a single packet, no accumulator**.

Two details sharpen that:

* The `yDist` clamp immediately before the comparison, also in `handleMovePlayer`, is
  `if (yDist > -0.5 || yDist < 0.5) { yDist = 0.0; }`. That disjunction
  is true for every finite `yDist`, so **Y is always zeroed and the check is purely
  horizontal**. This rule modifies exactly and only the horizontal components, so a
  back-off disagreement lands squarely in the measured quantity. (It reads like an
  intended `&&`; treat it as observed behaviour, not as a typo to model around.)
* The check is skipped for `isCreative()`, `isSpectator()`, `isSleeping()`,
  `isChangingDimension()` and `isInPostImpulseGraceTime()`. **A creative-mode client
  is exempt from rubber-banding entirely** — which matters for how you test this, see
  below.

## How it works

`maxDownStep = this.maxUpStep()` — the resolved **`STEP_HEIGHT` attribute**
(`LivingEntity.maxUpStep`), whose `RangedAttribute` default is `0.6`
(the `Attributes.STEP_HEIGHT` field). It is *not* a literal; the port takes it from
`EntityDimensions::step_height`, documented as the post-modifier value.

### The gate (`Player.maybeBackOffFromEdge`)

```java
!this.abilities.flying
  && !(delta.y > 0.0)
  && (moverType == MoverType.SELF || moverType == MoverType.PLAYER)
  && this.isStayingOnGroundSurface()
  && this.isAboveGround(maxDownStep)
```

| conjunct | how the port satisfies it |
|---|---|
| `!abilities.flying` | by construction — this crate has no creative flight, and a flying driver does not call `tick` (same standing argument as `Player.updateSwimming`'s flying override) |
| `!(delta.y > 0.0)` | read from the candidate delta. Kept as the negated `>` so a NaN Y branches as Java does. **`delta.y == 0.0` is not upward** and does back off — that is the walking case |
| mover type | by construction — `move_entity` *is* `move(MoverType.SELF, …)`. The excluded type is `PISTON`, which this crate has no equivalent of |
| `isStayingOnGroundSurface()` | `EdgeBackOff::Player::staying_on_ground_surface`. It is exactly `isShiftKeyDown()` (`Player.isStayingOnGroundSurface`) — the **raw shift key**, not the crouch pose and not `isCrouching()` |
| `isAboveGround(maxDownStep)` | `on_ground \|\| fallDistance < maxDownStep && !canFallAtLeast(0, 0, maxDownStep - fallDistance)` |

### `canFallAtLeast` (`Player.canFallAtLeast`)

The probe box is **not** uniformly shrunk:

```text
minX + 1e-7 + deltaX     ..  maxX - 1e-7 + deltaX     // inset both sides
minY - minHeight - 1e-7  ..  minY                     // grown *downward*; top at the feet
minZ + 1e-7 + deltaZ     ..  maxZ - 1e-7 + deltaZ     // inset both sides
```

Horizontally inset (touching a neighbouring column does not count); vertically the
*bottom* is pushed `1e-7` further down — an expansion — while the top sits exactly at
the feet plane. Overlap is the strict `min < max` test, so the block you are standing
on still registers and "cannot fall" is reported.

The consequence that decides the behaviour: this is a **whole-footprint** test. For a
player at `x = 0.5` (box `[0.2, 0.8]`) with support ending at `x = 1.0`, the probe
first clears at `deltaX >= 0.8 - 1e-7` — i.e. `canFallAtLeast` becomes true *exactly*
on the tick the move would leave the supporting block, and not before. That is why
the rule appears to "stop you at the edge" despite never measuring a distance to one.

### The stepping loop (`Player.maybeBackOffFromEdge`)

Vanilla does **not** clamp once. It walks the delta toward zero in `0.05` steps,
re-probing each time, in **three** loops — X and Z independent *first*, then joint:

1. X alone, probing `canFallAtLeast(deltaX, 0.0, maxDownStep)`.
2. Z alone, probing `canFallAtLeast(0.0, deltaZ, maxDownStep)`, starting from the
   **original** `delta.z` — not from anything loop 1 produced.
3. X and Z **together**, probing `canFallAtLeast(deltaX, deltaZ, …)` and stepping both,
   starting from whatever loops 1 and 2 left behind.

Loop 3 is the outside-corner case: the floor is missing only diagonally, so neither
pure-axis probe clears the support but the joint one does. Structurally it also differs
— loops 1 and 2 `break` the instant a component is zeroed; loop 3 has no `break`, may
zero one component while still stepping the other in the same iteration, and exits only
when a `!= 0.0` guard fails.

`stepX`/`stepZ` are `Math.signum(delta) * 0.05` computed **once**, before any stepping,
so the step can never change sign mid-loop. Y passes through untouched.

Two facts about reachability, both measured:

* **At walking speed the loop always terminates on its first iteration.** A sneak-walk
  tick is ~0.03 blocks, so `|delta| <= 0.05` holds immediately and the component is
  zeroed outright. The `0.05` stepping only becomes observable above 0.05 blocks/tick.
* **Loop 3 is unreachable at walking speed.** Clearing an outside corner needs the
  inset footprint to leave its support diagonally in one tick, which for a 0.6-wide box
  means a single-tick delta near 0.35–0.8. Ice, a landing, or an external push get you
  there; walking never does.

### What it does *not* touch

Vanilla rewrites the **local candidate delta only** — it never calls
`setDeltaMovement` here. So:

* `getDeltaMovement()`, which `restituteMovementAfterCollisions` later reads, keeps its
  un-backed-off value. Velocity keeps accumulating while you are held at a ledge, and
  releasing shift launches you at full speed.
* `xCollision`/`zCollision` compare against the *backed-off* delta, inside `Entity.move`
  itself, so a fully cancelled component reads as **no collision** and
  is never zeroed by restitution.

`move_entity` mirrors this by rewriting only `move_delta` and leaving
`pre_collision_velocity` alone. A "clamp the velocity before the tick" shortcut gets
both of these wrong.

## How to change it, and the gotchas

* **Position in the tick order is load-bearing.** The rule runs *inside* the move:
  after the stuck-speed multiplier is consumed and before `collide`, both within
  `Entity.move`. So a cobweb-slowed delta is what gets probed and stepped.
  Moving it before or after the move changes results.
* **`fall_distance` is now maintained by `PlayerState`'s own tick, not just an input.**
  It reaches the rule as `PlayerState::fall_distance` →
  `EdgeBackOff::Player::fall_distance`, and is read by **one** place:
  `isAboveGround`'s airborne branch, only when `on_ground` is false. What maintains it
  now, each cited against the jar (full detail lives on
  `PlayerState::fall_distance`'s own doc comment, which is the source of truth — this
  is a summary):
  - accumulation + grounded reset, `Entity.checkFallDamage`'s `-= (float) ya`,
    called from inside `Entity.move()` itself — reproduced right after every
    `do_move`/`travel_in_air` call in `tick_air`/`tick_water`/`tick_lava`/`tick_elytra`;
  - the water reset (`Entity.updateFluidInteraction`), reached from `Entity.baseTick`,
    at the top of `tick_water`;
  - the `*= 0.5` lava halving in `Entity.baseTick`, at the top of `tick_lava`;
  - the climbable reset (`LivingEntity.handleOnClimbable`), in `tick_air` only —
    vanilla reaches it only through `travelInAir`;
  - the Slow Falling/Levitation reset and the elytra
    `Entity.checkFallDistanceAccumulation` clamp to `1.0` (called from
    `updateFallFlying`), both inside `LivingEntity.aiStep`, before the
    travel dispatch;
  - the stuck-in-block reset (`Entity.makeStuckInBlock`), riding along
    with `update_stuck_multiplier`'s existing block scan.

  **One known divergence, bounded and pinned.** `updateFluidInteraction` has
  **two** call sites, not one: `Entity.baseTick` *and*
  `LivingEntity.checkFallDamage`, the latter running
  inside `move()` against the **post-move** position under `if (!isInWater())`.
  `isInWater()` is the *cached* `wasTouchingWater` (`Entity.isInWater`) that
  `updateFluidInteraction` itself rewrites, so vanilla resets mid-`move` on the
  tick a fall first touches water and then skips that tick's accumulation,
  ending at exactly `0.0`. This crate freezes the summary per tick, so the entry
  tick still runs `tick_air` and accumulates. **It cannot move the player**: the
  accumulation runs at the end of the move, after the back-off gate has read the
  old value, and the next tick's `tick_water` resets before any gate reads it —
  so the divergence is a one-tick transient visible only to an external reader
  (a future fall-damage predictor). Closing it costs a second
  `compute_fluid_state` on every air tick. Both halves of that claim — that it
  *is* positive on the entry tick, and that it is `0.0` again one tick later —
  are pinned by `fall_distance.rs`'s
  `water_entry_tick_is_the_one_known_divergence_and_it_lasts_exactly_one_tick`.

  **Still not modelled**, matching pre-existing gaps elsewhere in this crate: the
  `movementLength >= 1.0` clip-through reset inside `Entity.move` (needs a world
  raycast against `FALLDAMAGE_RESETTING` this crate's `CollisionView` has no
  equivalent of), creative flight's reset (`Player.aiStep` — this crate has no
  creative flight at all), riding/vehicles (this crate has no riding state), and
  bubble columns. **Teleport is the driver's responsibility**: this crate has no
  teleport primitive of its own, so a caller that snaps `PlayerState::position`
  directly (a server correction, a respawn, an ender pearl) must call
  `PlayerState::reset_fall_distance()` itself.

  **Know the direction of the *old* default's error**, for context on why this
  mattered: the airborne branch probes `maxDownStep - fallDistance`, so the old
  permanent `0.0` probed the *full* step height — a strictly weaker `canFallAtLeast`
  — and the gate opened **more** often than vanilla's, never less. Grounded, the
  value was unread and the server resets it to `0.0` on every grounded tick anyway,
  so the bridging/sneak-placing case was exact even before this — the divergence was
  specifically an **airborne** sneak (a mid-air ledge probe, e.g. sneaking during a
  fall) opening the gate when vanilla's would have stayed shut.
* **Note what this makes newly relevant.** `move_entity`'s doc says it models "the
  parts of `Entity.move` that affect an entity's reported position", and fall distance
  used to be outside that — it drove fall *damage*, which the server owns. This rule
  moves it inside. Any future "position-only" pruning must not re-drop it.
* **`Math.signum` is not `f64::signum`.** Java returns the argument for `±0.0`; Rust
  returns `±1.0`. `mth::java_signum` exists for this. It is currently harmless here (the
  step is only read inside a `while delta != 0.0` loop) but the discrepancy survives
  review.
* **A mob must not acquire this.** `EdgeBackOff::Entity` is `Default`, so
  `MoveContext::default()` — what `lodestone-shell`'s dropped-item mover and any mob
  loop pass — selects vanilla's identity base implementation. Keep it an enum rather
  than a bool for that reason.
* **Cost is zero unless sneaking.** The gate short-circuits on
  `staying_on_ground_surface` before any `canFallAtLeast` probe, so a pathfinder
  calling physics thousands of times pays nothing on non-sneaking candidates. A
  sneaking candidate pays one `no_collision` for `isAboveGround` plus one per loop
  iteration.
* **`no_collision` is the block half only, and `canFallAtLeast` is one of the two
  callers that notices.** Vanilla's `noCollision(entity, box)` is
  `noBlockCollision && noEntityCollision && noBorderCollision`
  (`CollisionGetter.noCollision`), and `Player.canFallAtLeast` calls the full form.
  **Update:** the entity term now exists as
  `lodestone_physics::push::no_entity_collision`, with the conjunction as
  `no_collision_among_entities` — see [`entity-push.md`](./entity-push.md). The back-off
  still calls the block-only form, and the gap that leaves is narrower than it reads:
  `getEntityCollisions` filters on `Entity.canBeCollidedWith`, which no player and no
  mob overrides, so the only entities that could ever change the answer are a **boat,
  a shulker or a happy ghast** — i.e. sneaking at the edge of a boat's deck. The world
  border remains wholly unmodelled and is now the only unmodelled term of the three.
* **Regenerating golden traces needs rustfmt afterwards.** `gen_golden.py` emits one
  line per tick; the checked-in `support/golden_traces.rs` is rustfmt'd to four. Run
  `python3 crates/lodestone-physics/tests/gen_golden.py` from the repo root, then
  `rustfmt --edition 2024 crates/lodestone-physics/tests/support/golden_traces.rs`, or
  the diff is 13,680 spurious deletions.

## Configuration

None. No feature flag, no env var. The rule is on for every `PlayerState` moved through
`tick`/`tick_air`/`tick_water`/`tick_lava`/`tick_elytra`, gated only by the sneak input.

## Wiring — already complete, nothing to do in the shell

`player_physics` (`crates/lodestone-ecs/src/player.rs`) calls
`lodestone_physics::tick(player, intent, view, profile)` with `intent =
MovementIntent.0`, whose `sneak` bit is written by
`lodestone-controller`'s `movement_intent` system. `tick_air` builds
`AirTravelContext::edge_back_off = EdgeBackOff::Player { staying_on_ground_surface:
input.sneak, .. }`, so the rule reaches real movement with **no shell change**.

The other half of the wire was already correct too:
`send_player_input` (`crates/lodestone-controller/src/ecs.rs`) sends `shift: intent.sneak` in
`ServerboundPlayerInputPacket`, which is precisely what makes the server call
`setShiftKeyDown(true)` inside `ServerGamePacketListenerImpl.handlePlayerInput` and therefore apply
the back-off in its own replay.

**That is the confirmation the open question wanted.** It also means the divergence
was live before this change: the shell told the server it was sneaking, the server
backed the movement off, and the client did not.

**The inverse trap.** The two must stay consistent. A driver that sneaks *locally*
without sending the input packet — or that sends shift without modelling the rule —
manufactures the same disagreement, just in the other direction. If a headless/bot
path ever bypasses `movement_intent`, it must set both or neither.

## Testing

* `crates/lodestone-physics/tests/golden.rs` — `sneak_edge_stop`,
  `sneak_edge_walk_off` (the world control) and `sneak_edge_diagonal` (the only
  scenario entering loop 3), replayed bit-for-bit from the independent Python oracle
  in `tests/gen_golden.py`.
* `crates/lodestone-physics/tests/edge_back_off.rs` — the pure control (one delta,
  one world, `EdgeBackOff::Player` vs `EdgeBackOff::Entity`, nothing else varying) plus
  hand-derived expectations for the step loop, the X/Z independence, the
  `fall_distance` gate (at the `move_entity` primitive level, `fall_distance`
  hand-set), and the velocity-survives-the-cancel property.
* `crates/lodestone-physics/tests/fall_distance.rs` — whether `PlayerState`
  actually *maintains* `fall_distance`: every accumulation/reset site
  driven by real ticks through the public `tick`/`tick_air`/`tick_water`/
  `tick_lava`/`tick_elytra` entry points, plus a flagship test that drives a real
  fall past `maxDownStep` and shows it changes the committed position at this gate
  versus a zero control — the end-to-end version of what the hand-set test above
  checks in isolation.

### The live gate that is still owed

`docs/baritone-port.md` §10 names two verifications. The first — read the
`maybeBackOffFromEdge` call site in `Entity.move` and confirm the server replay reaches
it — is **done and confirmed** (`ServerGamePacketListenerImpl.handleMovePlayer`, mover type
`PLAYER`, admitted by `Player.maybeBackOffFromEdge`). The second — sneak off a ledge on the oracle
and watch the correction counter — has **not been run**.

Whoever runs it should know three things that are easy to get wrong:

1. **It must be the survival oracle** (`./scripts/live-oracles/survival.sh`, game
   `:25565`, RCON `:25566`). The rubber-band check is skipped for `!isCreative()`,
   also inside `handleMovePlayer`, so running it against
   `creative.sh` — the default reflex — gives a **guaranteed vacuous pass**: zero
   corrections whether or not the rule is modelled.
2. **The counter to read is `Sim::teleport_count`** (`crates/lodestone-shell/src/sim.rs`),
   incremented on every adopted `TeleportPlayer` inside `Sim::poll_net`
   (`crates/lodestone-shell/src/sim/net_apply.rs`).
3. **It needs a control proving the counter can move inside the test's lifetime** —
   an RCON `tp` provocation that must increment it. Otherwise "no corrections" is the
   *duration* species of vacuous test: a counter that was never going to move.

## Dependencies

* `lodestone-physics::collision::no_collision` — the block-only overlap probe.
* `lodestone-physics::entity::{EntityDimensions, MoveContext, move_entity}`.
* `lodestone-physics::mth::java_signum`.
* Consumers: `lodestone-ecs`'s `player_physics`, via `lodestone_physics::tick`.
