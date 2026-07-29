# Swimming

## What it is

The water-movement port: how the client integrates a swimming player, and —
the actual defect this work started from — why sprint-swimming didn't work
even though the client believed it was sprinting the whole time. Landed in
`13a1d3a` ("kelp is breakable again, and swimming actually swims").

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

The fix is `Sim::send_is_sprinting_if_needed`
(`crates/lodestone-shell/src/sim.rs:1858`), called from
`Sim::drive_interaction` once per live physics tick. It's edge-triggered
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

- **The swimming hitbox is not modelled.** Vanilla's `Pose.SWIMMING` is a
  flat `0.6 × 0.6` box (down from the standing `0.6 × 1.8`) with eye height
  `0.4`. This port keeps the standing `EntityDimensions::PLAYER` for every
  pose, so a swimmer cannot squeeze through a one-block gap the way vanilla
  can. Only the **eye height** is modelled as a pose input —
  `SWIMMING_EYE_HEIGHT: f32 = 0.4` (`sim.rs`), applied by
  `Sim::update_pose` to `PlayerState::eye_height` — the collision box stays
  standing-sized regardless of pose.
- **`WATER_MOVEMENT_EFFICIENCY` has no reachable value.** The field defaults
  to `0.0` and the formula that consumes it (halved when airborne, lerps
  between the water slowdown and normal speed) is fully wired at the point
  of use in `player.rs`. Practically: Depth Strider currently changes
  nothing, because there is no path from "the enchantment is on the boots"
  to this field.

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

  3. **Still open, and it is one line: nothing consumes the value.**
     `lodestone_ecs::player::player_physics` computes `movement_speed` from
     `profile.base_movement_speed` and the sprint modifier — see the
     "`movement_speed` is not attribute-driven" note under *How to change it*
     below. `water_movement_efficiency` needs the same treatment in the same
     function.
- **Bubble columns are not implemented.** `docs/fluid-classification.md`
  already documents that `bubble_column` is one of the five classes
  hardcoded to read as water for classification purposes
  (`UNCONDITIONAL_WATER_BLOCKS`) — so a bubble column correctly makes you
  swim, and correctly fogs/sounds like water. But `BubbleColumnBlock`'s
  up/down impulse is nowhere in `lodestone-physics`: standing in one moves
  you like plain water, not like a lift or a drain.

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
  Measured while closing the `EntityIndex` hole above.
  `lodestone_ecs::player::player_physics` does call the physics seam —
  `*player = player.with_movement_speed(attr)` — but `attr` is
  `profile.base_movement_speed` (a hardcoded `0.1`, `lodestone-physics`'s
  `profile.rs:142`) times `(1 + profile.sprint_speed_modifier)` when
  sprinting. **No server-reported attribute reaches it.** So:

  - a `/attribute @s minecraft:movement_speed base set 0.5` changes nothing
    client-side;
  - **Speed and Slowness do not change the local player's walk speed.**
    `lodestone_physics::movement_speed_modifier` exists, is unit-tested, and
    has no production caller — `player_physics` never folds
    `PhysicsState.effects` into the value. `effective_speed`'s own doc comment
    says "sprint + Speed/Slowness already folded in by the entity layer",
    which is true of the *seam's contract* and false of every caller.

  This is the same hole as the attribute one, one layer further on: the value
  had nowhere to come from, so the caller invented one. The read side is now
  wired (`ClientHandle::local_player_attributes`), so both fixes are the same
  shape of change in the same place.

- **Wiring `WATER_MOVEMENT_EFFICIENCY` (and `movement_speed`) for real**: the
  fold and the wire-to-fold conversion exist (`lodestone_entity::attribute` —
  `AttributeInstance::value`, `instance_from_snapshot`, `attribute_value`,
  `water_movement_efficiency_key`), and so does the read —
  `ClientHandle::local_player_attributes`, which is where the fold now lands.
  What's left:
  0. A `NetClient::local_player_attributes` passthrough in
     `crates/lodestone-shell/src/net.rs` (`self.handle.get().map_or_else(
     Vec::new, |h| h.local_player_attributes())`). Deliberately **not** added
     ahead of a consumer: an accessor with no caller is the island `CLAUDE.md`
     rule 1 is about, and it is three lines to write beside step 1.
  1. Each physics tick, fold the snapshot list through
     `attribute::attribute_value` and write the results into per-player
     components (mirroring how `sim.rs` already writes `Flying`/`MovementIntent`
     straight into the ECS `World`). Do this in `sim.rs`'s per-tick path, **not**
     inside a system that would have to reach `ClientHandle` under the `World`
     write guard — `local_player_attributes` is an ECS-backed read and rule 1 in
     [`world-unification.md`](./world-unification.md) applies.
  2. Have `lodestone-ecs`'s `player_physics`
     (`crates/lodestone-ecs/src/player.rs`) read those components instead of
     deriving from `profile`: `with_water_movement_efficiency(value)` beside the
     existing `with_movement_speed` call, and `with_movement_speed` fed from the
     attribute rather than `profile.base_movement_speed`. Keep the profile value
     as the fallback for when the server has reported nothing — that is what the
     offline fixture world needs.
  Don't hardcode a Depth Strider constant as a shortcut — that reintroduces
  exactly the "guessed number instead of real data" pattern `CLAUDE.md` warns
  against elsewhere in this repo.
- **Bubble columns**: would need a `BubbleColumnBlock`-equivalent impulse in
  `tick_water`, gated on the block's `drag_direction`/`drag` blockstate
  property (not yet decoded anywhere in this tree, as far as this doc's
  research went — check before assuming it's already available).

## Configuration

None of its own. `SPRINT_TRIGGER_WINDOW_TICKS` is a compile-time constant
matching vanilla's default, not a runtime option.

## Dependencies

- `lodestone-physics::player` — `tick_water`, `PlayerState::eye_height`,
  `WATER_MOVEMENT_EFFICIENCY`.
- `lodestone-entity::attribute` — `AttributeInstance::value` (the vanilla
  three-stage fold), `instance_from_snapshot`/`attribute_value` (the
  wire-shaped `EntityAttributeSnapshot` → fold conversion),
  `water_movement_efficiency_key`. This is what makes Depth Strider's
  arithmetic computable at all; see the "still missing" list above for what
  is left to actually reach it from a live connection.
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
