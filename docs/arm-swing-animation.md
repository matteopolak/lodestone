# Arm swing animation

## What it is

The arm swing you see when you mine, place, or hit something. Vanilla drives
three separate things off one number — `attackAnim`, swing progress in
`0.0..=1.0`:

| consumer | vanilla | here | status |
|---|---|---|---|
| first-person arm | `ItemInHandRenderer.renderPlayerArm` | `first_person_arm_chain` | **wired** |
| your own third-person body | `HumanoidModel.setupAttackAnimation` | `Skeleton::pose`'s `attack_anim` | **wired** |
| other players and mobs | `ClientboundAnimatePacket` | — | **not wired**, see below |

The packet half — telling the *server* we swung — was already done before this
existed and is untouched: `lodestone_game::mining` emits
`ClientAction::SwingArm { hand: Main }`, and v770 encodes it as `swing`.

## How it works

Two clocks, and mixing them up is the whole hazard.

### The tick clock

`lodestone_entity::pose::EntityPose` (`crates/lodestone-entity/src/pose.rs`) is
vanilla's `LivingEntity` swing state: `swing_time`, `swinging`, `attack_anim`,
`o_attack_anim`.

- `start_swing(duration)` — `LivingEntity.swing`. Sets `swing_time = -1`, and
  **swallows a restart before the half-way point**. That swallowing is what makes
  a held mine look like continuous swinging: `interact.rs` queues a swing every
  single tick, and roughly two in three are dropped here.
- `tick()` — `LivingEntity.updateSwingTime`, once per 20 Hz tick. Advances
  `swing_time`, snapshots `o_attack_anim`, recomputes
  `attack_anim = swing_time / duration`.
- `attack_anim_lerp(partial_tick)` — `LivingEntity.getAttackAnim`, a **pure read**.

`attack_anim` is a sawtooth: it climbs to `(duration-1)/duration` then drops to
`0` in one tick. `attack_anim_lerp` is therefore *not* a plain lerp — vanilla
wraps a negative delta forward:

```rust
let mut diff = self.attack_anim - self.o_attack_anim;
if diff < 0.0 { diff += 1.0; }
self.o_attack_anim + diff * partial_tick
```

Without the wrap the arm runs **backwards through the entire arc inside one
50 ms tick** every time a swing ends or restarts. Since hold-to-mine restarts the
swing every three ticks, that is not a rare edge — it is most of the animation.
The wrap works because every swing term returns to zero at `attack_anim == 1.0`
as well as at `0.0`, so carrying the arc forward to `1.0` lands the arm at rest.
`the_arm_stays_on_screen_for_every_phase_of_the_swing` pins that (both endpoints
measure 2716 px).

### Who starts a swing

`Sim::swing_hand` (`crates/lodestone-shell/src/sim.rs`), from four places:

| site | path |
|---|---|
| `drain_action_queue` | any queued `SwingArm { Main }` — **this is the mining case** |
| `use_item_live` | right-click on a live server (bypasses the queue for wire order) |
| `break_block` | demo world |
| `place_block` | demo world |

`drain_action_queue` is the funnel that matters: it means a new producer of
swings animates for free. It matches `Hand::Main` specifically — an off-hand
swing animates the *left* arm, which neither consumer draws, so it is ignored
rather than approximated onto the right one.

All four are unconditional, not gated on a live socket: the animation is
client-side, exactly as the demo world's swings prove.

### The frame read

Two consumers, one accessor, so they cannot disagree:

- **First-person arm.** `app.rs` samples `Sim::hand_swing_progress()` once per
  frame and installs it via `RenderState::set_hand_swing_source`;
  `prepare_first_person_arm` polls it and passes it to `first_person_arm_pose`.
- **Third-person self-body.** `Sim::third_person_body_state` puts the same value
  on `AnimInput::attack_anim`, which reaches `Skeleton::pose`.

### The vanilla expressions

From `ItemInHandRenderer.renderPlayerArm` in `.cache/mc/26.2/client-src`
(26.2 ships de-obfuscated), transcribed term by term into
`ArmSwingTerms::new` / `first_person_arm_chain`
(`crates/lodestone-render/src/entity.rs`):

```text
s  = sqrt(a)
xs = -0.3 · sin(s·π)      ys = 0.4 · sin(s·2π)      zs = -0.4 · sin(a·π)
yr =        sin(s·π)      zr =       sin(a²·π)

T(i·(xs + 0.64000005), ys - 0.6, zs - 0.71999997)
  · Ry(i·45°) · Ry(i·yr·70°) · Rz(i·zr·-20°)
  · T(i·-1, 3.6, 3.5) · Rz(i·120°) · Rx(200°) · Ry(i·-135°) · T(i·5.6, 0, 0)
```

`i` is `Arm::invert()`. Everything from `T(i·-1, 3.6, 3.5)` rightward is the
pre-existing rest chain, unchanged — the swing is purely additive, and
`arm_chain_at_rest_matches_the_static_chain` asserts `attack_anim == 0.0`
reproduces the old matrix exactly.

**The `sqrt` is the shape, not a detail.** Three of five terms use `sqrt(a)` and
one uses `a²`; only `zs` is linear. `sin(sqrt(a)·π)` rises fast and decays
slowly — the arm snaps out and eases back. A linear ramp gives a symmetric
pendulum that is visibly not Minecraft. Measured coverage across a swing shows
the signature plainly:

```text
a = 0.000 -> 2716 px    a = 0.0625 -> 10555 px   a = 0.250 -> 4723 px
a = 0.500 -> 1182 px    a = 0.5625 ->  1067 px   a = 1.000 -> 2716 px
```

Note `ys` uses `2π`, not `π`: over one swing the vertical offset goes up, back
through zero, and down again, rather than making a single hump like `x` and `z`.

## How to change it, and the gotchas

- **Drive the clock off the tick, never the frame.** `entities.rs:56-66` records
  this exact bug being found in the limb-swing code: driven per frame the phase
  advanced up to 3x too fast and swing speed became frame-rate dependent.
  `swing_progress_advances_per_tick_not_per_render_read` (pose.rs) and
  `swing_progress_is_tick_driven_not_frame_driven` (sim.rs) are the gates.
- **The first-person arm and the third-person body must never share a pose
  function.** First person swings the *camera-space chain* that a **rested** arm
  part hangs off; third person swings the *arm part* inside a rested body. They
  share the scalar and nothing else. See
  [`third-person-player-body.md`](./third-person-player-body.md).
- **A missing `set_hand_swing_source` looks exactly like a working rested arm.**
  The pass still runs and `RenderStats::first_person_arm_drawn` is still `true`;
  the source defaults to `0.0`. Suspect this first if a swing does not appear.
- **`attack_anim` is clamped, not extrapolated.** The shaping functions are
  periodic, so a value past `1.0` silently animates some *other* part of the
  curve rather than failing. `HandSwingSource::value` clamps and maps NaN to rest.
- **Do not fold in `applyItemArmAttackTransform`.** That is a different chain, for
  the case where the main hand is *not* empty and vanilla draws the item instead
  of the arm. The shell always renders the empty-hand case (it has no local held
  stack at the render layer), so that chain has no call site here.
- **There is a one-tick (50 ms) offset from vanilla** on the animation *starting*.
  Vanilla's `handleKeybinds` runs before `updateSwingTime` in the same tick; here
  `Sim::step` ticks `body_pose` before draining the action queue. Left alone
  deliberately — reordering that loop would move wire-ordering-sensitive code for
  an invisible gain. Documented on `Sim::swing_hand`.

### Remote players and mobs are not wired

`ClientboundAnimatePacket` **is** decoded — `crates/protocol/v770/src/adapter.rs`
handles `play::clientbound::ANIMATE` and emits
`ClientEvent::EntityAnimation { entity_id, action }` with
`AnimationAction::SwingMainHand`, covered by
`crates/protocol/v770/tests/entity_events.rs`. **Nothing consumes that event
anywhere.** So other players' and mobs' swings are invisible, and this is the one
consumer of the three still at zero pixels.

Closing it needs, in order:

1. `lodestone-ecs/src/ingest.rs` — add `EntityAnimation` to `handles_event` and an
   `apply_entity_animation` system resolving `entity_id` through `EntityIndex`.
   (The ingest fold already receives raw `ClientEvent`s, so **no** `net.rs`
   `NetUpdate` variant is needed.)
2. `lodestone-shell/src/entities.rs` — the swing state alongside `WalkAnim` (a new
   component, or an `EntityPose` swing field on the existing one), advanced in
   `tick_walk_animation`'s `TickSet::Animate` slot so it is tick-driven.
3. `render_anim` — replace the hardcoded `attack_anim: 0.0` with the interpolated
   read. That line is the island marker; everything downstream of it
   (`Skeleton::pose`'s `attack_anim`) already works and is unit-tested.

## Configuration

None. No env vars, flags or constants to set. `DEFAULT_SWING_DURATION` is 6
ticks, which in 26.2 is the *component* default (`SwingAnimation.DEFAULT` is
`(WHACK, 6)`) rather than a hard-coded constant — an item shipping its own
`swing_animation` component swings for a different number of ticks, and nothing
here decodes that component yet.

`swing_duration(base, haste_amplifier, mining_fatigue)` models
`LivingEntity.getCurrentSwingDuration` (Haste shortens, Mining Fatigue lengthens,
Haste wins outright when both are present) but **both arguments are `None` at
every call site**, because no local mob-effect state is reachable:
`update_mob_effect` is decoded and forwarded as `NetUpdate::EffectApplied`, but
nothing folds it into a per-entity effect set. `lodestone_game::mining::BreakInputs`
has the identical hole for the identical reason. Closing it is a change of
arguments at one call site.

## Dependencies

- `lodestone-entity` — `pose::EntityPose`, the tick clock.
- `lodestone-render` — `entity::first_person_arm_chain` / `first_person_arm_pose`
  (first person), `entity_anim::Skeleton::pose` (third person).
- `lodestone-shell` — `sim.rs` (clock owner and both accessors), `gpu.rs`
  (`HandSwingSource`, the arm pass), `app.rs` (installs the source per frame).
- `lodestone-game` — `mining`, the main producer of swings.

## Gates

| gate | what it catches |
|---|---|
| `pose.rs::attack_anim_wraps_forward_across_the_sawtooth_drop` | the backwards-rewind bug, with a control that a *rising* delta is not wrapped |
| `pose.rs::swing_progress_advances_per_tick_not_per_render_read` | a frame-driven clock |
| `pose.rs::swing_duration_models_haste_and_mining_fatigue` | the effect model, including Haste-wins |
| `entity.rs::arm_swing_terms_match_hand_evaluated_vanilla` | the `sqrt`/`a²` shaping, against closed-form values at `a = 0.25` where `sqrt` is exact |
| `entity.rs::arm_chain_at_rest_matches_the_static_chain` | the swing is additive, with a control that it moves at all |
| `sim.rs::a_queued_main_hand_swing_reaches_the_arm_pose` | **the island** — producer to consumer, with an idle control |
| `sim.rs::an_off_hand_swing_does_not_drive_the_main_arm` | the `Hand::Main` match |
| `first_person_arm_swing_pixels.rs` | pixels actually move, with a zero-difference negative control, plus a coverage floor so "swung out of frame" cannot read as success |

The pixel gates are `#[ignore]`d; run them with
`cargo test -p lodestone-render --test first_person_arm_swing_pixels -- --ignored --nocapture`.
