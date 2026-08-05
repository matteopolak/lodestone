# Swimming

## What it is

The water-movement port: how the client integrates a swimming player, and —
the actual defect this work started from — why sprint-swimming didn't work
even though the client believed it was sprinting the whole time. Landed in
`13a1d3a` ("kelp is breakable again, and swimming actually swims").

Issue #59 (look-descent + camera jerk) is documented in its own subsection
below rather than folded into the narrative above, since it is a later,
unrelated pass over the same subsystem.

## How it works

### The bug: two packets, and only one was sent

Vanilla tells the server "I am sprinting" over two different packets that do
two different things. `ServerboundPlayerInputPacket` (`SetPlayerInput` in
this port) only ever gets **stored**: `ServerGamePacketListenerImpl.
handlePlayerInput` writes it to `ServerPlayer.lastClientInput` and nothing
else. The thing that actually calls `player.setSprinting(...)` is a
different packet, `handlePlayerCommand`, driven by
`ServerboundPlayerCommandPacket` (`PlayerCommand` here).

The shell was sending `SetPlayerInput` every tick and never `PlayerCommand`
— so the server never believed the client was sprinting, no matter what the
client's own HUD showed. This matters specifically for swimming because
vanilla's sprint-swim speed boost is gated server-side on `isSprinting()`;
without the `PlayerCommand` edge, a "sprinting" swim was really just a
normal-speed swim the whole time.

The fix is `send_sprint_command`
(`crates/lodestone-shell/src/interact.rs::send_sprint_command`), a `GameTick`
system. It's edge-triggered
against `last_sprinting_sent`: a rising edge sends
`ClientAction::PlayerCommand { command: PlayerCommand::StartSprinting }`, a
falling edge sends `StopSprinting`, and a tick with no change sends nothing.
`SetPlayerInput` is unaffected and still sent every tick from
`Sim::send_player_input` — both packets are needed, they just do different
jobs (`SetPlayerInput` is movement-intent bits the server stores;
`PlayerCommand` is the discrete edge that flips server-side state).
`sprint_edges_reach_the_wire_as_player_commands` (`sim.rs`) pins the
edge-triggered wire behavior — that the packet only goes out on a change,
not every tick.

### Double-tap-W

`crates/lodestone-controller/src/input.rs`:

- `SPRINT_TRIGGER_WINDOW_TICKS: u8 = 7` — vanilla's default `sprintWindow`
  (`Options.java`).
- `InputState::set` arms the window on a fresh `Forward` press while a
  window is still open, latching `sprint_latched = true`; releasing forward
  clears the latch.
- `InputState::tick()` counts the window down by one 20 Hz tick, and cancels
  a pending window on sneak or back-press.
- `movement_intent` ORs `sprint || sprint_latched`, still gated by the usual
  forward/not-sneaking rule.

**This has to tick inside the fixed-rate physics loop, not once per frame.**
`Sim::drive_interaction` calls `self.input.tick()` inside the
`while self.accumulator >= TICK_DT` loop in `sim.rs`, with an explicit
comment: ageing the window per *frame* instead of per *tick* would make the
double-tap timing frame-rate dependent, since vanilla's own
`sprintTriggerTime` is counted in ticks. A slow frame that runs several
physics ticks at once still ages the window the correct number of times;
a naive per-frame call would age it once regardless.

### Water movement

`lodestone-physics`'s `tick_water` (`crates/lodestone-physics/src/player.rs`)
runs the actual integration and documents its own gaps in a "Not modelled"
list rather than leaving them to be rediscovered:

- ~~**The swimming hitbox is not modelled.**~~ **Closed (2026-07-29).** The
  box is now pose-dependent, through vanilla's fit-gated
  `Player.updatePlayerPose` rather than a pose→dimensions lookup — see
  [`pose-dimensions.md`](./pose-dimensions.md). `Pose.SWIMMING` is the flat
  `0.6 × 0.6` box with eye `0.4`, crouching is `0.6 × 1.5` / `1.27`, and both
  are *vetoed* against the world before they are adopted, because vanilla has
  no size-growth recovery for a player (`Entity.refreshDimensions` excludes
  both clients and `Player`). A swimmer fits a one-block gap; the golden
  traces `swim_gap_tunnel` and `swim_gap_blocked_control` are the pair.
  `lodestone-physics::tick` owns the decision and writes both
  `PlayerState::pose` and `PlayerState::eye_height`; `lodestone-ecs`'s own
  `update_pose` is now redundant on the physics-walk path and is the one edit
  still owed (spelled out in `pose-dimensions.md`).
- ~~**`WATER_MOVEMENT_EFFICIENCY` has no reachable value.**~~ **Closed
  (2026-07-30).** The field defaults to `0.0` and the formula that consumes it
  (halved when airborne, lerps between the water slowdown and normal speed) is
  fully wired at the point of use in `player.rs`; Depth Strider's boot
  enchantment now reaches it every tick. See the worked history below for how
  the remaining pieces closed, and "Re-verifying the `EntityIndex` fix" for the
  one belief in this history that was checked again before being trusted.

  **Correction (2026-07-29):** an earlier version of this doc said the
  three-stage vanilla `calculateValue()` fold "isn't implemented." That was
  wrong even when written — `lodestone_entity::attribute::AttributeInstance
  ::value()` has done exactly this fold (`AddValue` → `AddMultipliedBase` →
  `AddMultipliedTotal`, matching `AttributeInstance.calculateValue`,
  `.cache/mc/26.2/src/.../AttributeInstance.java:148-166`) since the initial
  commit. `minecraft:water_movement_efficiency` also already has a real
  registry default/range (`0.0, 0.0, 1.0`, matching `Attributes.java:108-109`
  exactly) in `attribute::default_def`. Neither piece was ever missing.

  What *was* missing, and is now closed on the `lodestone-entity` side:
  `attribute::instance_from_snapshot`/`attribute::attribute_value` convert
  the **wire-shaped** `EntityAttributeSnapshot` (`base` + `modifiers`, no
  min/max — the shape `read_update_attributes`,
  `crates/protocol/v770/src/packets/metadata.rs:485`, decodes to) into a
  foldable `AttributeInstance` and read it back through the same `value()`.
  `attribute::water_movement_efficiency_key()` names the attribute id so a
  per-tick caller doesn't hand-parse the literal. See
  `crates/lodestone-entity/src/attribute.rs`'s tests for a Depth-Strider-
  shaped worked example (base `AddValue` + two `AddMultipliedBase` +
  one `AddMultipliedTotal`, chosen so the two multiplicative stages can't
  coincide if swapped).

  **Update (2026-07-29, the ingest-seam change): the two blockers below are
  closed. One step is left, and it is the physics call site.**

  1. ~~No accessor surfaces the local player's own attributes.~~
     `ClientHandle::local_player_attributes()` does
     (`crates/lodestone-client/src/handle.rs`), reading the `Attributes`
     component off the session entity. Deliberately *not* routed through
     `ClientHandle::entity(id)`: the local player carries no
     `EntityKind`/`Position`/`Rotation`/`HeadYaw` — those would duplicate
     `PhysicsState` — so `entity_view` cannot build a view of it and must
     not be taught to.
  2. ~~`EntityIndex` never gets an entry for the local player's own id.~~
     `lodestone_ecs::ingest::apply_local_player_login` inserts it from
     `ClientEvent::Login`, together with `MinecraftEntityId` and an empty
     `Attributes`, so `apply_entity_attributes` resolves our own id and folds
     the server's `update_attributes` onto a real component.
     `login_indexes_the_local_player_so_its_own_attributes_fold` is the gate
     and `without_the_login_the_local_players_attributes_are_dropped_on_the_floor`
     is its control — the pre-fix behaviour verbatim.

     The routing hazard the old text warned about was real and was handled by
     moving the *whole* `PlayerSnapshot` fold into components rather than by
     making `SharedState::apply` non-exclusive; see
     [`world-unification.md`](./world-unification.md#the-vitals-collapse-and-the-second-blocker-c-hid).

     Two guards came with it: `apply_entity_spawn` and `apply_entity_removal`
     now skip an id held by a `LocalPlayer`, because indexing our own id put
     the local player inside reach of two systems that `despawn` by index.

  3. ~~Still open, and it is one line: nothing consumes the value.~~ **Closed
     (2026-07-30).** `lodestone_ecs::player::player_physics`
     (`crates/lodestone-ecs/src/player.rs`) now folds the local player's
     `Attributes` component through `attribute::attribute_value` +
     `water_movement_efficiency_key()` every tick and writes the result via
     `PlayerState::with_water_movement_efficiency`, the same seam that already
     injects `MOVEMENT_SPEED`. `movement_speed` itself is a **separate,
     still-open** gap — see the "`movement_speed` is not attribute-driven"
     note under *How to change it* below, which this did not touch.

     **Re-verifying the `EntityIndex` fix, rather than trusting this
     document's own "closed" note.** This step was blocked on exactly one
     thing — the local player having an `Attributes` component that ingest
     actually writes to — which step 2 above already claimed was closed. Before
     building on that claim, it was checked again against the code, not just
     against this file: `lodestone_ecs::ingest::apply_local_player_login`
     (`crates/lodestone-ecs/src/ingest.rs:166`) still inserts
     `(MinecraftEntityId, Attributes::default())` on `ClientEvent::Login`, and
     `apply_entity_attributes` (`ingest.rs:442`) still resolves through
     `EntityIndex` with no special-case that would exclude the local player's
     own id. Both were true. `CLAUDE.md`'s point about stale notes is
     specifically that a claim can be correct and *still* be worth re-checking,
     because the check is what makes it safe to build on — not because it was
     expected to be wrong here.

     **The plan below (steps 0 and 1) turned out to be unnecessary — a shorter
     route existed.** It assumed `player_physics` would need `Attributes` handed
     to it from outside the ECS, through a `ClientHandle`/`NetClient` read and a
     per-tick write into components from `sim.rs`. That assumption was never
     re-checked before, and it doesn't hold: `Sim` builds *one* `bevy_ecs`
     `World` and adds `IngestPlugin` (which owns `apply_entity_attributes`,
     writing `Attributes` directly onto the `LocalPlayer` entity once
     `apply_local_player_login` has indexed it) and `LocalPlayerPlugin` (which
     owns `player_physics`) to that **same** `World` — see `sim.rs`'s
     `app.add_plugins((..., LocalPlayerPlugin, ..., IngestPlugin, ...))` and its
     own comment citing `§4.1(c)` ("one `World`, one `GameTick`, one
     accumulator"). `player_physics` can therefore just add `Option<&Attributes>`
     to its query and read the component directly — no `NetClient` passthrough,
     no extra per-tick fold in `sim.rs`, because ingest already wrote the
     component the physics system sits next to. `Option` rather than a bare
     reference because `spawn_local_player` does not insert `Attributes` eagerly
     (it is server-reported, added only on login), so the offline demo world and
     the pre-login title-screen player have no such component at all;
     `attribute_value` already reads "no snapshot for this key" as the registry
     default (`0.0`), so `None` folds to the same inert value an empty snapshot
     list would. Pinned by
     `depth_strider_attribute_reaches_the_physics_state_each_tick` and its
     control, `no_attributes_component_folds_to_the_registry_default`
     (`crates/lodestone-ecs/src/player.rs`).

     `ClientHandle::local_player_attributes()` (step 1's original target
     reader) is not deleted — it is still the right accessor for a caller
     *outside* the ECS `GameTick` schedule (a plugin, a debug overlay), it is
     just not on the path `player_physics` itself needed.
- ~~**Bubble columns are not implemented.**~~ **Closed** (issue #199) — see
  [`bubble-columns.md`](./bubble-columns.md). The impulse turned out not to
  belong in `tick_water` at all: `BubbleColumnBlock.entityInside` is reached
  from `applyEffectsFromBlocks`, which `LivingEntity.aiStep` calls *after*
  `travel()`, so it lives one level up beside `update_stuck_multiplier`. The
  `drag` property that blocked it was already on the wire and in the generated
  state table the whole time.
- ~~**The swim look-descent is not modelled.**~~ **Closed** (issue #59) — see
  below.

### Lava movement (issue #214)

`tick_lava` (`crates/lodestone-physics/src/player.rs`) is `travelInLava`
(`LivingEntity.java:2539-2555`), and it is a *different* branch from water,
not a retuned copy of it: flat `0.02F` input speed regardless of depth, and an
extra `-baseGravity/4` term on top of whatever the depth branch below does.

**The predicate.** `isInShallowFluid(LAVA)` = `getFluidHeight(LAVA) <=
getFluidJumpThreshold()`. The threshold is `eyeHeight() < 0.4 ? 0.0 : 0.4`
(`fluid_jump_threshold`, `Entity.java:3692-3694`) — `0.4` for every real
player pose, since even the swimming pose's eye height is exactly `0.4` and
`0.4 < 0.4` is false. This is the *same* predicate and the *same*
`FluidState::lava_height` input `apply_fluid_jump` already used for the jump
decision (`LivingEntity.aiStep`'s jump block); the movement branch below was
the one piece of that pair still unported, closing issue #214.

**Both arms**, `<= 0.4` (shallow) vs. `> 0.4` (deep):

- shallow: `multiply(0.5, 0.8, 0.5)`, then
  `getFluidFallingAdjustedMovement` — the same buoyant slow-descent water
  always gets (normally `y - baseGravity/16`, clamped to `-0.003` near
  terminal fall speed).
- deep: a flat `scale(0.5)` on all three axes. **No** falling-adjustment at
  all — this is not water with different numbers, it is a structurally
  simpler arm.

`fall_distance` participates in **neither** the predicate nor either arm.
This was worth checking explicitly rather than assuming: reading
`travelInFluid`/`travelInLava`/`getFluidFallingAdjustedMovement` directly
shows `isFalling` there is `getDeltaMovement().y <= 0.0`, not a
`fallDistance` comparison. So, unlike the accumulation work in
[`edge-back-off.md`](./edge-back-off.md) (where `fall_distance` being
permanently `0.0` *did* silently gate two other systems), this branch's
absence was not fall-distance-shaped — it was simply never ported, on both
sides of the predicate. The pre-existing `lava_sink` golden scenario only
ever exercised the deep arm (a coarse `is_lava` presence cell reads as a full
`1.0` height through `fluid_state::cell_height`'s fallback, which is always
`> 0.4`); it stays golden byte-for-bit unchanged as the regression control
that this port is additive. `lava_shallow` (new) uses a fine `fluid_at` cell
with a low `amount` (`3` ⇒ height `0.333`) — coarse one-block-deep presence
cannot reach the shallow arm at all, regardless of how many blocks "deep" it
looks, because presence-only reads as a full cell. Both are replayed
bit-for-bit against `gen_golden.py`, an independent Python oracle, in
`tests/golden.rs`. `tests/lava_depth.rs` adds the primitive-level evidence a
smooth trace can't show on its own: a pure control that runs identical
input through `tick_lava` twice, varying only `lava_height`, and a check that
the predicate's `<=` (not `<`) is inclusive at the exact threshold.

Out of scope for this port, named rather than silently drifted into: fire
damage, lava's rendering, and the fluid-flow simulation (issue #309).

### Look-descent and the camera jerk (issue #59)

Two reported bugs, one real cause each — they turned out to be unrelated to
each other, which is itself the finding.

**Looking down did not make you descend.** `Player.travel`
(`Player.java:1401-1415`) has an override `LivingEntity.travel` never gets:

```java
if (this.isSwimming()) {
   double lookAngleY = this.getLookAngle().y;
   double multiplier = lookAngleY < -0.2 ? 0.085 : 0.06;
   if (lookAngleY <= 0.0
      || this.jumping
      || !this.level().getFluidState(BlockPos.containing(this.getX(), this.getY() + 1.0 - 0.1, this.getZ())).isEmpty()) {
      Vec3 movement = this.getDeltaMovement();
      this.setDeltaMovement(movement.add(0.0, (lookAngleY - movement.y) * multiplier, 0.0));
   }
}
```

This was simply never ported — `tick_water` went straight from the jump
decision into `travelInWater`'s own physics, so a swimmer's vertical velocity
was governed entirely by buoyancy and the jump impulse, never by where they
were looking.

Both constants are exactly `0.085`/`0.06`, read directly from
`Player.java:1408` in the 26.2 decompile — a caveat in the original issue body
said these could not be re-verified in 26.2 (a grep of `0.085` over
`world/entity/` without the `player/` subtree turns up only
`DropChances.DEFAULT_EQUIPMENT_DROP_CHANCE`), but grepping `Player.java`
itself finds the literal block verbatim. The recollection was right; the
verification method that failed to confirm it was too narrow.

The fix lands in `tick_water`
(`crates/lodestone-physics/src/player.rs`), between `apply_fluid_jump` (the
aiStep jump decision) and the `isFalling`/`oldY` capture that opens
`travelInWater` proper — because `Player.travel` modifies `deltaMovement.y`
*before* calling `super.travel()`, which is everything after that point in
the function. `calculate_view_vector` (already present for the elytra path;
`getLookAngle()` **is** `calculateViewVector(getXRot(), getYRot())`, so it is
the same function, not a lookalike) supplies `lookAngleY`; the head-submerged
probe reads `CollisionView::fluid_at` at
`floor(x), floor(y + 1.0 - 0.1), floor(z)`, matching
`BlockPos.containing(x, y + 0.9, z)` and accepting **any** fluid, not just
water, exactly as `!getFluidState(pos).isEmpty()` does.

Three new golden scenarios pin this (`gen_golden.py` / `tests/golden.rs`):
`swim_look_down_dives` (paired against the pre-existing `swim_sprint`, whose
pitch-0 trace turns out to be the blend's own fixed point — `lookAngleY == vy
== 0`, so a swimmer looking straight ahead while sprint-swimming never drifted
vertically at all, before or after this fix, which is why regenerating
`golden_traces.rs` changed **no existing scenario**, only added three), and
the `swim_surface_look_up_no_pulldown` / `swim_surface_look_down_control` pair,
which seed the swim pose directly at the surface (same technique
`elytra_gap_glide` uses to seed `FallFlying`) so that `headSubmerged` is
false and the *gate*, not just the multiplier, is on trial: looking up
produces an exact flat line (`vy == 0.0` every tick, since sprint-swimming
also suppresses buoyancy via `getFluidFallingAdjustedMovement`'s own
`!sprinting` guard), looking down at the same spot descends steadily toward a
`vy` fixed point around `-0.22`.

**The zero-deletion control was run first, and it did not pass as a byte
diff.** Regenerating the checked-in `golden_traces.rs` with the *unmodified*
generator produced a 4:1 line-count mismatch (4,927 vs. 19,206 lines) against
what was already checked in. That was not drift: the checked-in file has been
through `cargo fmt`, which reflows each single-line generator-emitted
`GoldenTick { .. }` literal onto four lines; the generator itself has always
emitted one line each. Extracting every `0x…` hex literal from both files, in
order, and diffing *that* (28,560 values each) showed zero differences —
the true control. Run `cargo fmt` on the regenerated file before diffing it
by eye, or this false alarm reproduces for the next person too.

**`swimAmount` was investigated as a candidate camera fix and ruled out.**
`LivingEntity.swimAmount`/`swimAmountO` (`LivingEntity.java:174,275-276,
3478-3483`) is a `0..1` ramp, `±0.09`/tick, clamped — now modelled as
[`PlayerState::swim_amount`]/`swim_amount_o`
(`crates/lodestone-physics/src/player.rs`), advanced in
`travel_and_check_inside_blocks` right after `update_swimming`, matching
vanilla's exact call order (`LivingEntity.tick()` calls `updateSwimAmount()`
immediately after `super.tick()`, which is where `updateSwimming` lives, and
*before* `aiStep()`/`travel`). But grepping every `.cache/mc/26.2/client-src`
hit for `swimAmount` turns up only `HumanoidModel`, `HumanoidMobRenderer`,
`DrownedRenderer`/`DrownedModel` and the humanoid render state — **never**
`Camera` or `GameRenderer`. It blends the swimming model's body-pitch
animation. Nothing in this repo's render path consumes it yet (rendering the
humanoid swim pose is `entity-rendering.md`'s territory, out of this issue's
scope); it exists on `PlayerState` as exact per-tick output for whenever that
lands, the same way `eye_in_water`/`swimming` already do.

**The actual camera jerk is `Camera`'s own, separate smoothing, which this
engine never had at all.** `Camera.java` keeps a *second*, camera-local
`eyeHeight`/`eyeHeightOld` pair, entirely independent of the entity's:

```java
// Camera.tick(), :80-88
this.eyeHeightOld = this.eyeHeight;
this.eyeHeight = this.eyeHeight + (this.entity.getEyeHeight() - this.eyeHeight) * 0.5F;
```

read back with the same current/previous + partial-tick shape as position
interpolation (`Camera.alignWithEntity`, `:246-264`):
`Mth.lerp(partialTicks, eyeHeightOld, eyeHeight)`. A pose change
(`PlayerState::eye_height`, e.g. `1.62 → 0.4` entering swim) is an atomic snap,
by design — `crate::pose::update_player_pose` sets it once. `camera_rig.rs`'s
`build_camera` was, and still is, handed that snapped value directly every
frame; the jerk is exactly that snap, reaching the screen unfiltered.

[`EyeHeightSmoother`](../crates/lodestone-shell/src/camera_rig.rs) is the fix:
`tick(target)` eases half the remaining distance per **physics** tick, and
`lerp(alpha)` reads the frame's actual value between the last two ticks'
smoothed values, exactly mirroring `Camera.tick()`/`alignWithEntity`. It is
unit-tested (`camera_rig.rs`'s own tests: a fresh smoother reports the seed
before any tick, one tick covers exactly half the remaining distance, repeated
ticks converge without ever snapping, and reversing direction mid-ramp still
eases rather than jumping), **and it is wired**.

`Sim` holds a persistent `eye_height_smoother` field (`sim.rs:565`), seeded from
the spawn pose (`:797`) so the first frame does not ease up from zero, ticked once
per physics tick beside `body_pose` (`:2256`), and read interpolated by
`Sim::camera` (`:3489`) in place of the raw `interp.eye_height`.

Note *why* interpolating the entity's own eye height would not have worked: the
value being interpolated between two ticks is itself already the post-snap one.

The gate is `sprinting_underwater_enters_the_swim_pose_and_drops_the_camera`, which
asserts the camera is **mid-ease** one tick after the pose flips — that is the
assertion proving `Sim::camera` reads the smoother rather than the pose — and then
that it converges. One subtlety it encodes: `lerp(0.0)` returns the *previous*
tick's smoothed value (that is what the `O` twin is for), so reading at alpha 0.0
straight after a flip shows the pre-flip height. Correct semantics, and not what a
mid-ease assertion wants.

**Pose oscillation was checked and ruled out as a second jerk source.** The
pose fit gate (`ab9351c`) is a pure, deterministic function of position each
tick — no hysteresis, no per-tick randomness — and `EntityDimensions::
bounding_box` anchors `min_y` at the feet, so a pose change only ever moves
the box's *top* face, never the player. See
[`pose-dimensions.md`](./pose-dimensions.md) for the fuller argument; nothing
specific to issue #59 turned up here beyond re-confirming both properties
still hold.

### Why this was also a kelp-targeting bug

The pick ray's origin is the camera, and the camera is offset by
`player.eye_height` (`Sim::camera`). Before `eye_height` followed pose (this
same commit), a swimmer's eye sat a full metre above where it should have —
so a swimmer looking down at a block a metre in front of them was actually
targeting the block a metre *beyond* it. This compounded with the separate
`is_water`/pick-ray conflation documented in
[`fluid-classification.md`](./fluid-classification.md) (fixed by
`LiveCollision::is_pickable` in `crates/lodestone-shell/src/collision.rs`) —
between the two, a swimmer trying to break kelp was both aiming at the wrong
cell and unable to target the right one even by luck.

Note the pick-ray consequence compounds the other way now, too: with the box
*and* the eye following the pose, `PlayerState::eye_height` is an output mirror
rather than an input — nothing inside `lodestone_physics::tick` reads it, so a
driver that writes it cannot move the player, only the camera.

There's also a sneak-vetoes-sprint exception specific to swimming-while-
descending: `Sim::swim_adjusted_intent` (`sim.rs`), which ports
`LocalPlayer.canStartSprinting`/`shouldStopSwimSprinting` — sneaking to
descend while swimming should not itself cancel a sprint the way sneaking
does on land.

## How to change it

- **Sprint edge detection and the `PlayerCommand`/`SetPlayerInput` split** —
  `Sim::send_is_sprinting_if_needed` and `Sim::send_player_input` in
  `crates/lodestone-shell/src/sim.rs`. If you add another discrete
  server-side toggle (e.g. a future elytra start/stop), it almost certainly
  needs the same edge-triggered `PlayerCommand` treatment, not a per-tick
  resend through `SetPlayerInput` — that's the exact bug this doc exists to
  record.
- **Double-tap timing** — `crates/lodestone-controller/src/input.rs`'s
  `InputState`. Keep `tick()` inside the fixed 20 Hz loop; moving it to a
  per-frame call reintroduces frame-rate-dependent timing.
- **Water integration** — `lodestone-physics::player::tick_water`. Its own
  "Not modelled" doc list is the authoritative gap inventory; update it
  alongside any fix so the list doesn't go stale the way `docs/in-flight.md`
  did.
- **`movement_speed` is not attribute-driven either, and it looks like it is.**
  Issue #193 / Tier 1 epic #1. `lodestone_ecs::player::player_physics` does
  call the physics seam — `*player = player.with_movement_speed(attr)` — but
  `attr` is `profile.base_movement_speed` (a hardcoded `0.1`,
  `lodestone-physics`'s `profile.rs:142`) times
  `(1 + profile.sprint_speed_modifier)` when sprinting. **No server-reported
  attribute reaches it.** So a `/attribute @s minecraft:movement_speed base
  set 0.5` changes nothing client-side, and Speed/Slowness/Soul Speed/boot
  enchantments do not change the local player's walk speed.

  **Re-verified against the jar (2026-07-31) rather than trusting the
  original write-up's plan verbatim** — `CLAUDE.md`'s rule 2. Two things
  changed the picture:

  1. **Speed/Slowness never need a client-side attribute-modifier fold of
     their own.** `LivingEntity.onEffectAdded`/`onEffectUpdated`
     (`.cache/mc/26.2/src/.../LivingEntity.java:1075-1103`) call
     `addAttributeModifiers`/`removeAttributeModifiers` on the entity's
     `AttributeMap` only `if (!this.level().isClientSide())` — i.e.
     **server-side only**. The client's own `handleUpdateMobEffect`
     (`ClientPacketListener.java:1794-1813`) calls `forceAddEffect`, which
     does *not* touch attributes on that path. The server instead marks the
     attribute dirty and syncs the resulting base+modifiers over the wire via
     `ClientboundUpdateAttributesPacket` (`ServerEntity.sendChanges`,
     `ServerEntity.java:289,352`), which is the packet
     `lodestone_ecs::ingest::apply_entity_attributes` already folds into
     `Attributes`. So folding `movement_speed` through the same
     `Attributes`-component seam Depth Strider uses already covers Speed,
     Slowness, Soul Speed and any equipment modifier — no second,
     client-side effect→attribute translation needs to exist.
     `lodestone_physics::effect::movement_speed_modifier`/`classify`'s
     Speed/Slowness branches remain unit-tested and production-caller-free
     after this finding, same as before it — they were never the missing
     piece.
  2. **The sprint bonus is itself the same shape of modifier, server-side.**
     `LivingEntity.setSprinting` adds/removes a transient
     `AttributeModifier(SPRINTING_MODIFIER_ID, 0.3F, ADD_MULTIPLIED_TOTAL)`
     (`LivingEntity.java:154-157,2311-2317`) on the same `MOVEMENT_SPEED`
     instance — not a separate multiply in `travel`. `Player.aiStep` then
     does `this.setSpeed((float)this.getAttributeValue(MOVEMENT_SPEED))`
     (`Player.java:456`) with **no further sprint arithmetic** — vanilla's
     `getSpeed()` *is* the folded attribute value. The client-local sprint
     multiply `player_physics` already does on top of `profile.
     base_movement_speed` exists only because our sprint intent is
     client-predicted (double-tap-W, the local key) and reaches the server's
     own `SPRINTING_MODIFIER_ID` modifier one or more ticks later over the
     `PlayerCommand` round trip (see "Sprint edge detection" above) — so it
     stays, layered on top of the attribute-derived base rather than
     replacing vanilla's single-attribute-read model, exactly as the original
     scoping said.

  **Closed on the `lodestone-physics`/`lodestone-entity` side (2026-07-31),
  open on the `lodestone-ecs` side — the wiring call site itself is out of
  this pass's file ownership.**
  `lodestone_entity::attribute::movement_speed_key()` now exists
  (`crates/lodestone-entity/src/attribute.rs`), mirroring
  `water_movement_efficiency_key()` exactly, with its own worked fold test
  (`movement_speed_folds_a_speed_ii_modifier_onto_the_player_base` — a
  Speed-II-shaped `ADD_MULTIPLIED_TOTAL` modifier on the player's real `0.1`
  base folds to `0.14`, matching `AttributeInstance::value`'s existing
  three-stage fold, which was already correct for all three operations
  before this issue — proven by `water_movement_efficiency_folds_through_the_
  wire_snapshot`'s deliberately-order-sensitive worked example). On the
  physics side, `PlayerState::movement_speed`/`with_movement_speed`/
  `effective_speed` were **already fully wired and tested** —
  `injected_attribute_speed_replaces_not_stacks_with_sprint` already pinned
  that a `Some(v)` override is used verbatim, `(float)`-cast, never
  re-multiplied by physics itself. What did **not** exist: any golden
  scenario or ECS system that ever set `movement_speed` away from its `None`
  default, so the seam was reachable but never actually reached end to end.
  `tests/golden.rs`'s `walk_speed_ii_matches_golden` (backed by a new
  `gen_golden.py` scenario) and `tests/movement_speed.rs`'s pure control
  close that gap on the physics side — see their own doc comments for what
  each proves and why the golden regeneration was a zero-diff across all 42
  pre-existing scenarios before the new one was added.

  **The remaining patch, spelled out exactly** (blocked on this pass's file
  ownership, not on any unresolved question — `crates/lodestone-ecs/**` is
  out of scope here): in `crates/lodestone-ecs/src/player.rs`'s
  `player_physics`, change

  ```rust
  let base = f64::from(profile.base_movement_speed);
  let attr = if intent.sprint {
      base * (1.0 + f64::from(profile.sprint_speed_modifier))
  } else {
      base
  };
  *player = player.with_movement_speed(attr);
  ```

  to

  ```rust
  let base = attributes.map_or(f64::from(profile.base_movement_speed), |attrs| {
      attribute_value(&attrs.0, &movement_speed_key())
  });
  let attr = if intent.sprint {
      base * (1.0 + f64::from(profile.sprint_speed_modifier))
  } else {
      base
  };
  *player = player.with_movement_speed(attr);
  ```

  (adding `movement_speed_key` to the existing
  `use lodestone_entity::attribute::{attribute_value, water_movement_efficiency_key};`
  import) — the same `Attributes`-component read `player_physics` already
  holds for Depth Strider, `profile.base_movement_speed` kept as the fallback
  for the offline demo world exactly as before. The proof shape to pin it,
  mirroring `depth_strider_attribute_reaches_the_physics_state_each_tick` /
  `no_attributes_component_folds_to_the_registry_default`: a
  `movement_speed` snapshot (base `0.1` + a Speed-II-shaped
  `ADD_MULTIPLIED_TOTAL` modifier) on the local player's `Attributes`
  component reaching `PlayerState::movement_speed` through a real `GameTick`
  run, plus the no-`Attributes` control folding to `profile.
  base_movement_speed` rather than a stale value. A live-server gate
  (`/effect give @s speed 30 1`, watch reported delta-position magnitude
  change) remains the `needs-live-verify` half neither the hermetic ECS test
  nor this pass's physics-only work can stand in for.

- ~~**Wiring `WATER_MOVEMENT_EFFICIENCY` (and `movement_speed`) for real**~~ —
  **`WATER_MOVEMENT_EFFICIENCY` half closed 2026-07-30; `movement_speed` is
  still open, see below.** The fold and the wire-to-fold conversion exist
  (`lodestone_entity::attribute` — `AttributeInstance::value`,
  `instance_from_snapshot`, `attribute_value`, `water_movement_efficiency_key`).
  This bullet used to plan a three-step route through `ClientHandle`/`net.rs`
  and a `sim.rs` per-tick fold into components (steps 0–2, preserved below only
  because the corrected version is shorter and worth contrasting with):

  > 0. A `NetClient::local_player_attributes` passthrough in `net.rs`.
  > 1. Each physics tick, fold the snapshot list in `sim.rs`'s per-tick path and
  >    write per-player components from it.
  > 2. Have `player_physics` read those components instead of deriving from
  >    `profile`.

  **That plan assumed `player_physics` had no cheaper way to reach
  `Attributes` than a `ClientHandle` round trip, and that assumption was never
  checked before being written down.** It doesn't hold: `Sim` adds
  `IngestPlugin` (which owns `apply_entity_attributes`, writing `Attributes`
  onto the `LocalPlayer` entity once `apply_local_player_login` has indexed
  it) and `LocalPlayerPlugin` (which owns `player_physics`) to the **same**
  `bevy_ecs::World` — see `sim.rs`'s own `§4.1(c)` comment on its
  `add_plugins` call. `player_physics` therefore just queries
  `Option<&Attributes>` on the same entity directly and folds it in-place; no
  passthrough and no extra per-tick write ever needed to exist. Implemented in
  `crates/lodestone-ecs/src/player.rs`, pinned by
  `depth_strider_attribute_reaches_the_physics_state_each_tick` and
  `no_attributes_component_folds_to_the_registry_default`.

  **`movement_speed` is the same shape of gap, and the route is now fully
  known rather than merely scoped** — see the bullet above ("`movement_speed`
  is not attribute-driven either…") for the exact patch, the jar citations
  that replaced the original plan's assumptions, and what closed on the
  `lodestone-physics`/`lodestone-entity` side on 2026-07-31.
  `movement_speed_key()` now exists next to `water_movement_efficiency_key()`
  in `lodestone_entity::attribute`, so that part of this bullet's original
  "does not exist yet" is done. Do **not** route this through
  `ClientHandle`/`net.rs`, and don't hardcode a Speed/Depth-Strider constant
  as a shortcut — both reintroduce exactly the "guessed number instead of
  real data" pattern `CLAUDE.md` warns against elsewhere in this repo.
- **Bubble columns**: would need a `BubbleColumnBlock`-equivalent impulse in
  `tick_water`, gated on the block's `drag_direction`/`drag` blockstate
  property (not yet decoded anywhere in this tree, as far as this doc's
  research went — check before assuming it's already available).

## Configuration

None of its own. `SPRINT_TRIGGER_WINDOW_TICKS` is a compile-time constant
matching vanilla's default, not a runtime option.

## Dependencies

- `lodestone-physics::player` — `tick_water`, `PlayerState::eye_height`,
  `PlayerState::swim_amount`/`swim_amount_o`, `WATER_MOVEMENT_EFFICIENCY`.
- `lodestone-shell::camera_rig` — `EyeHeightSmoother`, the issue #59 camera-jerk
  fix (wired into `Sim`; see above).
- `lodestone-physics::pose` — the pose the swim flag feeds
  ([`pose-dimensions.md`](./pose-dimensions.md)): `state.swimming` is
  `getDesiredPose`'s top-priority input, and the fit gate decides whether it is
  granted.
- `lodestone-entity::attribute` — `AttributeInstance::value` (the vanilla
  three-stage fold), `instance_from_snapshot`/`attribute_value` (the
  wire-shaped `EntityAttributeSnapshot` → fold conversion),
  `water_movement_efficiency_key`, `movement_speed_key` (issue #193,
  2026-07-31). `crates/lodestone-ecs/src/player.rs`'s `player_physics` is a
  direct dependent of this module for `water_movement_efficiency_key` (added
  2026-07-30, closing Depth Strider) — see the "still open" note above for
  the exact patch that makes it a dependent of `movement_speed_key` too,
  which as of this doc's last update it is not yet.
- `lodestone-controller::input` — `InputState`, double-tap detection.
- `crates/lodestone-shell/src/sim.rs` — `Sim::drive_interaction`,
  `send_is_sprinting_if_needed`, `send_player_input`, `update_pose`,
  `swim_adjusted_intent`, `camera`.
- [`fluid-classification.md`](./fluid-classification.md) — the `is_water`
  seam that both gates the swim path and, once unified with the mesher,
  needed the separate pick-ray fix this doc also touches on.

## Tests

Hermetic, in `crates/lodestone-shell/src/sim.rs`:
`sprint_edges_reach_the_wire_as_player_commands` (the edge-triggered
`PlayerCommand` behavior). Physics-side water movement tests live in
`lodestone-physics`'s own test module alongside `tick_water`.

Depth Strider's attribute fold, hermetic, in `crates/lodestone-ecs/src/player.rs`:
`depth_strider_attribute_reaches_the_physics_state_each_tick` (a
`water_movement_efficiency` snapshot on the local player's `Attributes`
component reaches `PlayerState` through a real `GameTick` run) and its
control, `no_attributes_component_folds_to_the_registry_default` (no
`Attributes` component at all — the offline/pre-login state — must fold to
the registry default, not a stale or hard-coded value).

Issue #59: `tests/golden.rs`'s `swim_look_down_dives_matches_golden`,
`swim_surface_look_up_no_pulldown_matches_golden`,
`swim_surface_look_down_control_matches_golden` (bit-exact vs. the Python
oracle in `gen_golden.py`, zero tolerance). `camera_rig.rs`'s own test module
covers `EyeHeightSmoother` in isolation (seed/tick/lerp, convergence, reversal
mid-ramp) — hermetic, since it is not wired into `Sim` yet.

Issue #193 (`movement_speed`), physics/entity side, 2026-07-31:
`lodestone-entity::attribute`'s `movement_speed_key_matches_the_registry_id`
and `movement_speed_folds_a_speed_ii_modifier_onto_the_player_base` (the
wire-shaped fold, mirroring the Depth Strider worked example).
`lodestone-physics::tests::golden::walk_speed_ii_matches_golden` (a new
`gen_golden.py` scenario — regenerating the golden file with only the
oracle's `player_speed`-override refactor first produced a byte-for-byte
**zero diff** across all 42 pre-existing scenarios, confirming none of them
ever exercised `movement_speed` away from its `None` default; this is the one
that does, bit-exact against the Python oracle). `tests/movement_speed.rs`'s
`speed_ii_and_sprint_are_the_only_difference` is the pure control, in the
style of `tests/lava_depth.rs`'s `shallow_vs_deep_is_the_only_difference`:
the same one-tick walk through three different injected `movement_speed`
values (base, Speed II, sprinting), each expected position hand-derived from
`Entity.getInputVector`/`modifyInputSpeedForSquareMovement`'s jar formula
rather than by calling the crate's own helpers. The `lodestone-ecs` wiring
gate this table's Depth Strider row already has
(`depth_strider_attribute_reaches_the_physics_state_each_tick` /
`no_attributes_component_folds_to_the_registry_default`) does **not** yet
have a `movement_speed` counterpart — that is the remaining patch spelled out
above, out of this pass's file ownership.
