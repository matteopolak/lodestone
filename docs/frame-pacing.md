# Frame pacing

## What it is

`FramePacer` in [`crates/lodestone-shell/src/app.rs`](../crates/lodestone-shell/src/app.rs)
owns the shell's frame clock. Once per event-loop iteration it answers two
questions: **how much real time to hand the simulation**, and **whether to
present a frame at all**.

It exists to fix a reported bug: alt-tabbing away and back made the client "catch
up" by running every tick it had missed, which was visibly laggy. The fix has two
halves — a catch-up clamp, and decoupling presentation from simulation.

## How it works

### The catch-up clamp

Vanilla caps how much game time one update may run:
`Minecraft.MAX_TICKS_PER_UPDATE = 10` (`Minecraft.java:262`), applied as
`for (int i = 0; i < Math.min(10, ticksToDo); i++)` (`:1176`).

Note *where* the cap sits. `DeltaTracker.Timer::advanceGameTime` returns the
**full, uncapped** tick count and keeps the sub-tick residual; `runTick` then runs
at most ten of them and **drops the rest**. Missed real time is discarded, never
replayed. That is the whole mechanism — there is no backlog anywhere.

`FramePacer::begin_frame` mirrors it by clamping the returned `dt` to
`MAX_CATCHUP_SECS` (`10 × 0.05 = 0.5 s`).

**Measured:** a 60-second stall yields `dt = 0.5`, which drives **5** ticks, not
1200. Five rather than ten because `Sim::step` applies its own tighter
`dt.clamp(0.0, 0.25)` to the accumulator *before* the tick loop (`sim.rs`), so the
shell's effective budget is half vanilla's. That inner clamp predates the pacer
and lives in a file the pacer's change did not own, so the number is **pinned** by
`app::tests::a_long_stall_is_clamped_not_replayed` rather than corrected: if
anyone loosens or removes it, that test fails and the two caps get reconciled
deliberately.

The negative control in that test drives the same real `Sim` the proportional way
the bug describes — one tick's worth of `dt` at a time — and executes all 1200
ticks. It has been observed failing the clamped assertion, which is what makes the
5 meaningful.

### Presentation must not gate simulation

The subtler root cause. `redraw` used to step the sim and acquire a swapchain
image in the same call, with the GPU-readiness guard **before** the step. macOS
stops vending drawables to an occluded `CAMetalLayer`, so `acquire()` stalls; the
loop's iteration rate collapsed, and with it the tick rate, because ticks only
advanced when a frame did.

So the order in `redraw` is now, strictly:

1. `pacer.begin_frame(now)` — clamp `dt`, decide `render`
2. `sim.step(dt)` — **unconditional**
3. return early if `!render` — before any `acquire()`

**Never reorder these.** Keep-alives and the per-tick movement packet ride the
simulation, and a client the server considers stalled is sent no chunks at all —
a silent, total chunk blackout while join and entity movement continue perfectly.

### Unfocused and occluded

| window state | simulation | presentation | event loop |
|---|---|---|---|
| focused | 20 Hz | every iteration (vsync paces) | `ControlFlow::Poll` |
| unfocused | 20 Hz | `UNFOCUSED_FPS` = 30 | `WaitUntil(now + BACKGROUND_POLL)` |
| occluded / minimised | 20 Hz | none | `WaitUntil(now + BACKGROUND_POLL)` |

`BACKGROUND_POLL` is 8 ms — deliberately far shorter than one 50 ms tick, so the
loop is never what paces the simulation. If it ever exceeded `TICK_SECS` the sim
would fall behind the server while nominally "still ticking".

### The unfocused schedule is absolute, not elapsed-based

This was measured, and it is the one non-obvious part.

The obvious gate — `now - last_render >= interval`, then `last_render = now` —
**loses frames**. It can only fire on a loop iteration, and each firing pushes the
next deadline out by however far it overshot. At a 120 Hz loop with a 30 fps
target there are only four chances per interval, and the accumulated overshoot
cost 4 of every 30 frames:

| loop rate | naive gate | absolute schedule | target |
|---|---|---|---|
| 75 Hz | 25 | 30 | 30 |
| 77 Hz | 26 | 30 | 30 |
| 120 Hz | **26** | 30 | 30 |
| 144 Hz | 27 | 30 | 30 |
| 240 Hz | 28 | 30 | 30 |

Part of the cause is a units mismatch that is easy to reintroduce: a `Duration` is
whole nanoseconds, so an interval that lands on 33 333 333 ns is always a hair
short of `1.0 / 30.0` as an `f64`, and the very iteration that should have
presented never does. Comparing `Duration` to `Duration` removes that half; the
absolute deadline removes the rest.

`next_render` therefore advances by exactly one interval **from itself**, never
from `now`. The single exception is a stall longer than an interval, where it is
re-based onto `now` — otherwise returning from a two-minute alt-tab would present
a burst of catch-up frames, which is the presentation-side version of the very bug
this module exists to fix.

`app::tests::the_unfocused_frame_schedule_does_not_drift_below_its_target` keeps
the naive gate as a live control and requires it to be observed *failing*.

### Max Framerate, VSync and Reduce FPS When are live

Vanilla's `framerateLimit`/`enableVsync`/`inactivityFpsLimit` rows are wired now,
both onto persisted `Options` fields and onto real consumers — this section used
to describe the plumbing sitting unused; it now describes what reads it.

**VSync** (`Options::enable_vsync`) drives `SurfaceTarget`'s present mode, exactly
the way the deleted `unlock_framerate` debug knob (issue #382) did:
`WindowApp::sync_vsync_present_mode` polls the option once per presented frame
(`app/redraw.rs`, right before the View Bobbing push) and hands
`SurfaceTarget::set_present_mode` either `default_present_mode()` (on — the
adapter's own remembered `Fifo`, not `wgpu::PresentMode::AutoVsync`, which
resolves to `FifoRelaxed` wherever that exists and would quietly change default
presentation everywhere that mode is advertised) or `AutoNoVsync` (off — never a
concrete `Immediate`/`Mailbox`, which is a validation error on an adapter that
does not advertise it; `AutoNoVsync` simply degrades to `Fifo`). The equality
guard inside `set_present_mode` is what makes the per-frame poll safe:
`surface.configure` recreates the swapchain, so only the frame the option
actually flips pays for a rebuild.

**Max Framerate** (`Options::framerate_limit`, raw fps `10..=260`, `260` = the
`UNLIMITED_FRAMERATE_CUTOFF` sentinel) and **Reduce FPS When**
(`Options::inactivity_fps_limit`, `Minimized`/`Afk`) both feed
`app::pacing::effective_target_fps`, vanilla's `FramerateLimitTracker` minus the
`WINDOW_ICONIFIED`/`OUT_OF_LEVEL_MENU` branches — this pacer already throttles an
occluded/unfocused window unconditionally (the table above), so those two vanilla
branches are subsumed by a mechanism that predates the option. What is new is the
AFK clock: `Afk` (vanilla's default) additionally drops to `min(limit, 30)` fps
after 60 s with no real key/mouse input and to a flat `10` after 600 s
(`FramePacer::record_input`, hooked from `app/lifecycle.rs`'s `window_event` on
`KeyboardInput`/`MouseInput`/`MouseWheel` — deliberately not `CursorMoved`,
mirroring vanilla's own `onInputReceived` call sites). `Minimized` never reduces
for idle input, matching vanilla's own gate on the AFK clock.

**Composes with VSync, is not gated by it**: vanilla applies
`FramerateLimiter.limitDisplayFPS` whenever `framerateLimit < 260`
unconditionally, vsync on or off, and this client reproduces that rather than
inventing a precedence between the two options.

`FramePacer::begin_frame(now, target_fps)` schedules against an **absolute**
deadline exactly like the unfocused table below, generalised: `target_fps` of
`None` means "no schedule, vsync/the compositor paces a focused window"
(unchanged from before either option existed); any `Some(fps)` — a real cap while
focused, or the always-at-least-`UNFOCUSED_FPS` unfocused case — paces against
`next_render`. **This is not a busy-wait**: `FramePacer::control_flow` reports
`ControlFlow::WaitUntil(next_render)` whenever a cap applies to a focused window,
not `ControlFlow::Poll` — without it, a `framerateLimit` below the refresh rate
would still spin the event loop at 100% of a core calling `begin_frame` every
iteration only to find `render == false` most of the time.

Note a cap only removes *our* pacing on a **focused** window's presentation rate;
the tick rate is never touched (`step.dt` is computed before either option is
read), and an unfocused/occluded window's own throttle (below) still applies,
tightened further by an explicit lower cap rather than overridden by it.

## How to change it

- The constants (`MAX_TICKS_PER_UPDATE`, `TICK_SECS`, `UNFOCUSED_FPS`,
  `BACKGROUND_POLL`) and `FramePacer` itself are all in
  `crates/lodestone-shell/src/app/pacing.rs` — the "Frame pacing" section of the
  old monolithic `app.rs`, moved verbatim when that file became a directory.
  They are still re-exported as `app::MAX_TICKS_PER_UPDATE` and friends, so no
  caller's path changed.
- `FramePacer` is pure and takes an injected `Instant`, so every behaviour above
  is testable with a synthetic clock and a real `Sim`, with no window and no GPU.
  Add tests there, not to an integration gate.
- **Do not "pause the world" on focus loss.** `Screen::Paused` is local UI state
  only; `WindowEvent::Focused(false)` releases the pointer and throttles
  presentation, and must never stop `sim.step`.
- If you change `UNFOCUSED_FPS`, the drift test's expected counts move with it —
  it derives the target from the constant, but the *control* numbers in the table
  above were measured at 30.

## Configuration

The catch-up/schedule shape is compile-time constants — `MAX_TICKS_PER_UPDATE`,
`TICK_SECS`, `UNFOCUSED_FPS`, `BACKGROUND_POLL` — but three real runtime
settings now feed the schedule itself, all in
[`Options`](../crates/lodestone-shell/src/config.rs) and all on the Video
settings page: `framerate_limit` (raw fps `10..=260`, default `120`,
`UNLIMITED_FRAMERATE_CUTOFF = 260` = "Unlimited"), `enable_vsync` (default
`true`), and `inactivity_fps_limit` (`Minimized`/`Afk`, default `Afk`). See
`app::pacing::effective_target_fps` and `WindowApp::sync_vsync_present_mode` for
where each is read.

An install that toggled the old, deleted `unlock_framerate` debug knob (#382)
still has `"unlock_framerate": true` sitting in its `options.json`. That is
harmless — `Options::from_json` reads the keys it knows and ignores the rest,
which
`config::tests::an_unknown_key_in_the_file_is_ignored_rather_than_failing_the_load`
pins, so a stale key can never cost `gui_scale` or the keybinds table.

## Dependencies

- `winit` — `ControlFlow`, and the `Focused` / `Occluded` / `KeyboardInput` /
  `MouseInput` / `MouseWheel` window events that feed the pacer (the last three
  reset the AFK clock).
- `crate::sim::Sim` — `step(dt)` and `tick_count()`; the pacer only supplies `dt`.
- `lodestone_render::SurfaceTarget` — owns the swapchain configuration, so the
  present mode is set through it rather than from `app.rs` directly.
- `crate::config::{Options, InactivityFpsLimit}` — the three persisted fields
  above.
