# Camera, view bobbing and frame pacing

## What it is

How the render camera's orientation is built and why (`lodestone-render`'s
`Camera`), the three tick-driven effects layered onto it before it reaches a
uniform — the walking bob, the damage tilt, and (not yet built) held-item view lag
— and the shell's frame clock, which decides how much simulated time a frame gets
and whether it presents at all.

## How it works

### Camera basis and the reversed-Z projection

`Camera` is a plain `Copy` struct — eye `position`, `yaw`/`pitch` in degrees,
`fov_y_degrees`, `aspect`, `near`, `far` — reconciled term-for-term against
vanilla's own client-side camera type. Its basis is a direct expansion of
vanilla's single YXZ Euler rotation (`Ry(π − yaw) · Rx(−pitch)`, no roll), **not** a
look-at built from a hardcoded `Vec3::Y` up vector. A look-at is degenerate at pitch
`±90°`: in exact arithmetic the cross product needed for `right` is zero there, and
in real `f32` arithmetic `cos(90°)` rounds to a tiny nonzero value that still
produces a finite, orthonormal, right-handed, determinant-`+1` basis — it just rolls
the image 180° at the pole. Every well-formedness assertion passes on the broken
construction; only a continuity sweep across the singularity or a predicted basis
value at the pole can see it. `right` has no pitch term at all (always horizontal);
`up` becomes horizontal exactly at the poles, which is correct and matches vanilla.

The view matrix's determinant is `+1`; several call sites (particles, nametags,
dropped items, the first-person arm) read the basis back out of the matrix rows
rather than through an accessor and depend on that sign. The GUI winding invariant
(`sign(det(gui_ortho · gui_item_pose))` must equal `sign(det(view_projection))`) is
therefore a property of the *projection* alone here, since the view's determinant is
fixed at `+1` — derive it from a real camera, never assert a polarity.

Depth is `[0,1]` DirectX-style and **reversed** (near maps to `1`, far to `0`), the
same arrangement vanilla uses. A forward `[0,1]` projection (what this renderer used
to be) spends almost the entire float32 mantissa within a few blocks of the near
plane, so a fixed world-space clearance's representable-depth budget collapses as
`distance²`; reversed-Z shrinks with distance instead and degrades only as
`1/distance`. This is not a stylistic choice on vanilla's part — it is the
arrangement that spends float exponent where the depth range actually needs it, and
it is why vanilla's own depth-bias constants are the size they are. Consequences
that follow directly and apply to every ported depth comparison and bias in the
renderer: "nearer" compares `GREATER_THAN_OR_EQUAL` in vanilla and its unflipped
reversed equivalent here; a depth attachment clears to `0.0`, not `1.0`; a bias that
pulls toward the eye is positive; the far plane stays finite (an infinite-far
reversed projection would do even better but deletes the far clip plane frustum
culling needs).

### View bobbing, the damage tilt, and the seam that used to block both

Three mechanisms a screenshot makes look like one: `bobView` (the walking sway/dip/
nod, once per footfall, driven by tick-accumulated `walkDist`/`bob`), `bobHurt` (a
roll toward the hit direction, driven by a `hurtTime` countdown), and `xBob`/`yBob`
(a smoothed lag of the held item and third-person body behind head rotation — **not
yet implemented**). `bobHurt` is unrelated to the red hurt-flash overlay, which is a
separate ~30%-red blend in the entity pipeline; the two happen to fire together.

Tick-advanced state (`ViewBob`, alongside the eye-height smoother — both are
per-tick state that cannot be a pure function of the current player pose) lives in
`crates/lodestone-shell/src/camera_rig.rs`. Once per frame a single `BobFrame` feeds
**two independent consumers**, mirroring vanilla's own two call sites:
`bobbed_camera(camera, frame, tilt_strength) -> Camera` for the world, and a
separate hand-side path for the first-person arm/held item. `Camera` only has two
angles (yaw/pitch) where a bob matrix has three degrees of freedom (it can roll), so
folding the bob into a `Camera` for the world necessarily **drops roll**: the walk
bob's own roll term is small enough to not matter (measured well under a pixel), but
the damage tilt is *mostly* roll, so the world's copy of `bobHurt` instead rides a
separate eye-space seam — vanilla's own approach, which post-multiplies the bob onto
the **projection** matrix rather than the camera itself (`P · bobHurt · V`), so
`Sim::camera` (also the block-targeting ray origin and audio listener) never bobs at
all. Every world-space uniform must read the one method that composes this, or a
pass would slide against the terrain around it while the camera tilts.

The first-person hand needs no such fold — it multiplies the bob matrix straight
into its own projection with no view-matrix decomposition step, so it carries every
term, roll included, and is what makes it correct for it to apply `bobHurt` a
*second* time independently of the world's copy, exactly as vanilla's
`renderItemInHand` does.

Constants (all read from the 26.2 decompile, not remembered): `walkDist` advances by
`0.6 × horizontal distance moved` per tick; `bob` eases toward
`min(0.1, speed)` at `0.4` per tick, decaying to `0` off the ground, dead, or
swimming; the walk translate/nod/roll and the `sin(t⁴·π) · 14° · damageTiltStrength`
hurt tilt are vanilla's own view-bob and hurt-tilt formulas; View Bobbing
defaults on and `damageTiltStrength` defaults to `1.0`. Two details that are easy to
get backwards: the walk phase (`bd`) is an *extrapolation* (`-(walkDist + delta ×
partial_tick)`), not a lerp of the two most recent samples; and the nod's phase
offset (`− 0.2` inside the cosine) is radians, not a fraction of π before the
multiply — either mistake still looks like a plausible walk animation in a
screenshot.

### Frame pacing

`FramePacer` (`crates/lodestone-shell/src/app/pacing.rs`) answers two questions once
per event-loop iteration: how much real time to hand the simulation, and whether to
present a frame at all. It exists to fix a real bug — alt-tabbing away and back used
to make the client visibly "catch up" by running every missed tick.

Vanilla caps how much game time one update may run at 10 ticks
(`MAX_TICKS_PER_UPDATE`); missed real time beyond that is discarded, never replayed
— there is no backlog anywhere. `FramePacer::begin_frame` mirrors this by clamping
the returned `dt` to `10 × 0.05 = 0.5s`; the simulation's own tick loop applies a
tighter `dt.clamp(0.0, 0.25)` to its accumulator before that, so in practice a long
stall drives 5 ticks here, not 10 — a deliberately narrower budget than vanilla's,
pinned by its own test so a future change to either clamp is a conscious decision.

The subtler half of the same bug: `redraw` used to step the simulation and acquire a
swapchain image in one call, with the GPU-readiness check *before* the step. An
occluded window (macOS stops vending drawables to a hidden `CAMetalLayer`) stalls
`acquire()`, and with it the whole loop's iteration rate — and with *that*, tick
rate, since ticks only used to advance when a frame did. The fix is a strict order in
`redraw`: clamp `dt` and decide whether to render, step the simulation
**unconditionally**, then return early (before any `acquire()`) if not rendering.
Never reorder this — the per-tick movement packet and keep-alives ride the
simulation step, and a client the server considers stalled receives no chunks at all,
a silent total blackout while the connection otherwise looks healthy.

An unfocused or occluded window still ticks at the full 20 Hz but presents at a
capped rate (`UNFOCUSED_FPS`) or not at all, polling the event loop far more often
than one tick interval so the loop itself is never what paces simulation. That
unfocused schedule advances against an **absolute** deadline (`next_render += one
interval`, from itself, never from `now`), not a naive "has enough elapsed since
last render" gate — the naive form loses frames under a fast event loop, because
every firing pushes the next deadline out by however far the previous one
overshot, and a `Duration`'s whole-nanosecond precision makes a target interval like
`1/30s` a hair short of the float equivalent on every comparison. The one exception
is a stall longer than one interval, which re-bases onto `now` rather than bursting
out every missed frame at once.

Three real player-facing options now drive the same schedule, composing with each
other rather than gating one another: VSync (present-mode switch, changed only on
the frame it actually flips, to avoid rebuilding the swapchain every frame),
Max Framerate (a raw cap, `10..=260` with `260` meaning unlimited), and Reduce FPS
When (drops to a lower cap after periods with no keyboard/mouse input, matching
vanilla's own AFK clock — deliberately not reset by mouse movement alone). Any
active cap on a **focused** window schedules `ControlFlow::WaitUntil` rather than
busy-polling; the tick rate itself is never touched by any of these, only
presentation.

## How to change it, and the gotchas

- **`FramePacer` and `ViewBob` are pure and take an injected clock/pose**, so their
  behaviour is fully testable with a synthetic clock and a real `Sim`, no window or
  GPU required. Prefer adding a test there over a full integration gate.
- **Do not "fix" a recurrence by clamping pitch tighter than ±90°.** The clamp
  already exists; the basis flip happens exactly at the bound, and clamping past it
  only hides one symptom while diverging from vanilla (which renders straight down
  correctly) and leaving the underlying `NaN`/roll bug reachable by any other caller.
- **Do not "pause the world" on focus loss.** Pausing UI state and throttling
  presentation are both fine; stopping `sim.step` on `WindowEvent::Focused(false)` is
  exactly the bug this module exists to prevent.
- **The bob goes on `render_camera`, never on `Sim::camera`** — the latter is also
  the pick-ray origin and the audio listener, and vanilla bobs neither.
- **To land held-item view lag (`xBob`/`yBob`)**: add the same current/previous pair
  to `ViewBob`, prefixed onto the hand pose — it is a smoothed *lag*, not a bob, and
  is unrelated to the arm's own `bobHurt` wiring (the two were historically
  conflated in this doc).
- **A bob-fixture test that never actually accumulates `walkDist` measures
  nothing** — a hermetic gate needs a flattened path the player can really walk down
  before asserting on the resulting bob.

## Configuration

- `Options::view_bobbing` (default on), on the Accessibility settings screen
  alongside Notification Time — not on Video. `Options::damage_tilt_strength`
  (`0.0..=1.0`, default `1.0`) is the separate accessibility control for the hurt
  tilt alone; it does not affect the (unscaled) death roll. A malformed persisted
  value must read as **on** — degrading a shipped option to off would be a silent
  feature loss.
- `Options::framerate_limit` (`10..=260`, default `120`), `Options::enable_vsync`
  (default `true`), `Options::inactivity_fps_limit` (`Minimized`/`Afk`, default
  `Afk`) — all in `config.rs`, all read by `app::pacing::effective_target_fps`.
- Compile-time pacing constants: `MAX_TICKS_PER_UPDATE`, `TICK_SECS`,
  `UNFOCUSED_FPS`, `BACKGROUND_POLL` in `app/pacing.rs`.

## Dependencies

- `glam` — `Mat4`/`Vec3`/`Vec4`; the projection is assembled directly from
  `Mat4::from_cols` (DirectX `[0,1]` convention, range reversed) rather than a
  library helper.
- `winit` — `ControlFlow` and the `Focused`/`Occluded`/keyboard/mouse events the
  pacer reads.
- `lodestone_physics::PlayerState` — grounded state, velocity and pose feed the
  bob's amplitude and phase.
- `crate::sim::Sim` — `step(dt)`/`tick_count()`; the pacer only ever supplies `dt`.
- `lodestone_render::SurfaceTarget` — owns the swapchain configuration VSync writes
  through.
