# Camera, view bobbing and frame pacing

## What it is

How the render camera's orientation is built (`lodestone-render`'s `Camera`), the three tick-driven effects layered onto it (walking bob, damage tilt, decaying held-item view lag), and the shell's frame clock, which decides how much simulated time a frame gets and whether it presents at all.

## How it works

### Camera basis and reversed-Z

`Camera` is a plain `Copy` struct (eye `position`, `yaw`/`pitch` in degrees, `fov_y_degrees`, `aspect`, `near`, `far`) reconciled against the reference client camera. Its basis expands the single YXZ Euler rotation `Ry(pi - yaw) * Rx(-pitch)` (no roll), not a look-at with a hardcoded `Vec3::Y` up. A look-at is degenerate at pitch `+-90` degrees: the cross product for `right` is zero in exact arithmetic, and in `f32` `cos(90)` rounds to a tiny nonzero value that still yields a finite, orthonormal, determinant-`+1` basis that rolls the image 180 degrees at the pole. Every well-formedness assertion passes on it; only a continuity sweep across the singularity, or a predicted basis at the pole, sees it. `right` has no pitch term; `up` is horizontal exactly at the poles, as in the reference.

The view determinant is `+1`, and several call sites (particles, nametags, dropped items, first-person arm) read the basis back from matrix rows and depend on it. The GUI winding invariant (`sign(det(gui_ortho * gui_item_pose))` equals `sign(det(view_projection))`) is therefore a property of the projection alone; derive it from a real camera, never assert a polarity.

Depth is `[0,1]` and reversed (near is `1`, far `0`). A forward projection spends almost all float32 mantissa near the plane, so a fixed world-space clearance's depth budget collapses as `distance^2`; reversed-Z degrades only as `1/distance`. Consequences for every depth comparison and bias: "nearer" is `GREATER_THAN_OR_EQUAL`, depth clears to `0.0`, a bias toward the eye is positive, and the far plane stays finite (frustum culling needs it).

### View bob, damage tilt and view lag

Three separate mechanisms: the walking bob (sway, dip and nod once per footfall from tick-accumulated walk distance and bob amplitude), the damage tilt (a roll toward the hit direction from a hurt-time countdown), and `ViewLag` (a smoothed lag of the held item and local third-person body behind head rotation). The tilt is unrelated to the red hurt-flash blend in the entity pipeline; they just fire together.

Tick state (`ViewBob`, with the eye-height smoother) lives in `crates/lodestone-shell/src/camera_rig.rs`. Per frame one `BobFrame` feeds two consumers: `bobbed_camera(camera, frame, tilt_strength) -> Camera` for the world, and a hand-side path for the first-person arm and held item. `Camera` has two angles but a bob matrix has three degrees of freedom, so folding bob into a `Camera` drops roll. Walk-bob roll is under a pixel, but the damage tilt is mostly roll, so the world's tilt rides an eye-space seam: it post-multiplies onto the projection (`P * tilt * V`), so `Sim::camera` (also the pick-ray origin and audio listener) never bobs. Every world-space uniform must read the one method composing this, or a pass slides against the terrain.

The hand multiplies the bob matrix directly into its own projection, so it carries every term including roll, and applying the tilt a second time independently of the world copy is correct.

Constants (from the pinned decompile): walk distance advances `0.6 x horizontal distance` per tick; bob eases toward `min(0.1, speed)` at `0.4` per tick and decays to `0` off the ground, dead or swimming; tilt is `sin(t^4 * pi) * 14 degrees * damageTiltStrength`. Easy to get backwards: the walk phase is an extrapolation (`-(walkDist + delta * partial_tick)`), not a lerp of recent samples, and the nod's `- 0.2` phase offset is radians, not a fraction of pi. Either still looks like a plausible walk.

`ViewLag` keeps current and previous yaw and pitch beside `ViewBob`, easing each toward the live view by half the remaining distance per fixed tick. A frame interpolates the pair and prefixes ten percent of the residual onto the hand pose and the synthetic local body's held-item attachment. The ordinary camera stays unlagged (picking, audio, third-person pullback). All yaw updates, interpolation and residuals use the shortest angular arc, since yaw is stored wrapped. Keep both consumers on the same sample so a rapid-turn gate sees a nonzero offset that decays on stationary ticks.

### Frame pacing

`FramePacer` (`crates/lodestone-shell/src/app/pacing.rs`) answers once per event-loop iteration how much real time to give the simulation and whether to present. It fixes the alt-tab "catch up" bug.

The reference caps one update at 10 ticks and discards the rest, so there is no backlog. `FramePacer::begin_frame` clamps `dt` to `10 x 0.05 = 0.5 s`, and the simulation's tick loop applies a tighter `dt.clamp(0.0, 0.25)`, so a long stall runs 5 ticks (narrower on purpose, pinned by a test).

The second half of the bug: `redraw` used to step the simulation and acquire a swapchain image together, readiness check first. An occluded window (macOS stops vending drawables to a hidden `CAMetalLayer`) stalls `acquire()` and with it tick rate. The strict order in `redraw` is: clamp `dt` and decide whether to render, step the simulation unconditionally, then return before any `acquire()` if not rendering. Never reorder: the movement packet and keep-alives ride the step, and a client the server thinks stalled gets no chunks (a silent blackout on a healthy-looking connection).

An unfocused or occluded window still ticks at 20 Hz but presents at `UNFOCUSED_FPS` or not at all, polling the event loop faster than a tick interval. The unfocused schedule advances against an absolute deadline (`next_render += one interval` from itself, never from `now`); a naive "enough elapsed since last render" gate loses frames because each firing pushes the next deadline out by the overshoot, and whole-nanosecond `Duration` makes `1/30 s` a hair short. A stall longer than one interval rebases to `now` instead of bursting.

Three options compose on one schedule: VSync (present mode changes only on the frame it flips), Max Framerate (`10..=260`, `260` unlimited) and Reduce FPS When (a lower cap after input inactivity). Native and browser key events reset the pacer clock; mouse movement alone does not. A cap on a focused window uses `ControlFlow::WaitUntil`, not busy polling. Tick rate is never touched.

On macOS, VSync off also switches `SurfaceTarget` to mailbox presentation (`SurfaceTarget::set_mailbox`): a windowed Metal layer recycles its three drawables only as the compositor consumes them, so every acquire blocked until the next refresh (on 120 Hz, 1.8 ms of CPU work spent 6.5 ms in acquire). In mailbox mode frames render into a ring of three offscreen textures; a `lodestone-presenter` thread owns the swapchain and copies the newest finished frame into each freed drawable; intermediate frames are never shown. The renderer never takes the last-published slot or the one being copied, and waits when two published frames are unfinished on the GPU, bounding memory and latency.

The presenter copies on its own Metal command queue: a copy into a drawable waits until the compositor releases it, and on wgpu's queue every later frame waited too (measured 1,158 frames/s with presentation disabled vs about 245 with the copy on wgpu's queue). Separate queues are unordered, so the presenter CPU-waits for the published frame's submission before copying and for its copy before releasing the slot. Presentation counters count frames handed to the presenter; its shown count is `mailbox_presented` in each benchmark segment-transition log line.

## How to change it

- `FramePacer` and `ViewBob` are pure with injected clock and pose; test with a synthetic clock and a real `Sim`, no window or GPU.
- Do not fix a pole recurrence by clamping pitch tighter than +-90 degrees: the flip happens at the bound, clamping hides one symptom and leaves the NaN/roll reachable by other callers.
- Do not pause the world on focus loss; pausing UI and throttling presentation are fine, stopping `sim.step` on `Focused(false)` is the bug.
- Bob goes on `render_camera`, never `Sim::camera`.
- A bob fixture that never accumulates walk distance measures nothing; flatten a path the player can walk.

## Configuration

- `Options::view_bobbing` (default on; Accessibility screen beside Notification Time) and `Options::damage_tilt_strength` (`0.0..=1.0`, default `1.0`; the hurt tilt only, not the unscaled death roll). A malformed persisted value reads as on.
- `Options::framerate_limit` (`10..=260`, default `120`), `enable_vsync` (default `true`), `inactivity_fps_limit` (`Minimized`/`Afk`, default `Afk`) in `config.rs`, read by `app::pacing::effective_target_fps`.
- Constants `MAX_TICKS_PER_UPDATE`, `TICK_SECS`, `UNFOCUSED_FPS`, `BACKGROUND_POLL` in `app/pacing.rs`.

## Dependencies

`glam` (projection assembled from `Mat4::from_cols`), `winit` (`ControlFlow` and focus, occlusion, keyboard and mouse events), `lodestone_physics::PlayerState` (bob amplitude and phase), `crate::sim::Sim` (`step(dt)`, `tick_count()`), `lodestone_render::SurfaceTarget` (swapchain configuration).
