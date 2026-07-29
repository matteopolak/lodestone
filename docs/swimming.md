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
  of use in `player.rs`. What's missing is upstream: nothing in the shell
  can currently reach the local player's attribute set to compute a
  non-default value (`EntitySnapshot` drops `attributes`; there is no
  `NetClient` accessor for the local player's own attributes), and the
  three-stage vanilla `calculateValue()` attribute fold (base → add →
  multiply) isn't implemented. Practically: Depth Strider currently changes
  nothing, because there is no path from "the enchantment is on the boots"
  to this field. The doc comment on the field itself frames this as "closer
  than it looks... a missing accessor, not missing data" — the arithmetic
  side is done, only the plumbing to a real value is not.
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
- **Wiring `WATER_MOVEMENT_EFFICIENCY` for real**: needs (1) the local
  player's attribute set reachable from the shell (widen `EntitySnapshot` or
  add a `NetClient` accessor) and (2) the vanilla three-stage attribute fold.
  Neither exists yet; don't hardcode a Depth Strider constant as a
  shortcut — that reintroduces exactly the "guessed number instead of real
  data" pattern `CLAUDE.md` warns against elsewhere in this repo.
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
