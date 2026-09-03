# Runtime presentation attach/detach

## What it is

Lets a running session switch between headless and windowed **while it runs**, instead of only at
startup. A session can start headless (no window, no GPU, no presentation-only ECS
systems) and later have a window attached to it — or a windowed session can drop its window and GPU
state and keep ticking headlessly. Behind the `runtime-presentation` Cargo feature on `lodestone-shell`
(on by default).

## How it works

The mechanism has two independent halves, matching the two things a "presentation" actually is:

1. **ECS systems.** `Sim::client_app` composes four plugins on top of `lodestone_app::client_app`
   purely to feed a renderer: render-side entity interpolation (`crate::entities::EntityInterpPlugin`),
   the `Display`-family extract (`crate::display_entities::DisplayEntityPlugin`), the terrain mesher
   (`crate::mesher::TerrainPlugin`), and the pick/interaction/particle systems
   (`crate::interact::InteractPlugin`). Every system those four plugins register is tagged into
   `crate::sim::presentation::PresentationSet`, a `bevy_ecs` `SystemSet`. `Sim::detach_presentation`
   calls `Schedule::remove_systems_in_set(PresentationSet, ScheduleCleanupPolicy::RemoveSystemsOnly)` on
   each schedule the set lives in (`Update`, `GameTick`, `Extract`) — one of the two edge-repairing
   policies, so a surviving system ordered `.after`/`.before` a presentation system keeps that ordering.
   This stops real CPU work (meshing, particle simulation, interpolation), not just drawing.

   **Not `RemoveSetAndSystems`**, the crate's own default and the more obvious reading of "also remove the
   set": measured against this session's real `Update` schedule it reproducibly panics inside `bevy_ecs`
   0.19.1's own `SystemSets::check_type_set_ambiguity` the next time that schedule is rebuilt — before this
   crate's own removal logic even runs. `RemoveSystemsOnly` leaves the (now empty) set node in place, which
   `attach`'s `.in_set(PresentationSet)` calls simply re-populate, and does not trip the same path. Both
   policies are edge-repairing; this is a choice between the two, not a fallback to a lossy one — see
   `crate::sim::presentation::runtime::remove_from`'s own doc for the exact panic and the diagnosis.
2. **GPU state.** `WindowApp` (`crate::app`) holds `window`, `gpu`, `target`, `render`, `hud`, `menu` and
   `container` as `Option`s. `WindowApp::detach_presentation` sets all of them to `None`, which drops the
   last strong reference to every `wgpu` handle they hold — the swapchain, the depth buffer, every
   pipeline, and the block/particle/GUI atlas textures. `WindowApp::attach_presentation` creates a window
   through `ActiveEventLoop::create_window` (available on every `ApplicationHandler` callback, including
   the `user_event` callback this issue's runtime toggle uses) and reuses the same bring-up
   (`create_and_attach_window`/`finish_bring_up`) ordinary startup uses, so there is exactly one GPU
   bring-up path rather than a second copy that can drift.

Re-attach cannot go through `Plugin::build`/`App::add_plugins` a second time: `Sim` takes the `World` out
of its `App` and drops the `App` at construction (`sim/build.rs`), so there is no `App` left, and
`add_systems` does not deduplicate — a second `build()` call on a schedule that still held the old
systems would double every one of them. Each of the four plugins instead exposes its own
`add_presentation_systems(&mut World)`, called both by `Plugin::build` (construction) and by
`crate::sim::presentation::attach` (a runtime re-attach) — one place the registrations are spelled out,
reachable with or without an `App`. This is safe only because `detach` is *exact*: `Sim::presentation_attached`
(a plain bool, seeded `true`) guards both `Sim::attach_presentation`/`detach_presentation` against a
redundant call.

**Keeping the event loop alive.** On native, `EventLoop::run_app` owns the calling thread until the loop
exits, so a headless session still needs a running loop — `app::runners::run_headless_session` calls
`run_app` exactly as `run_windowed` does, just with a `WindowApp` that starts with
`presentation_desired: false` (so `resumed` does not create a window) and `Sim::detach_presentation()`
already applied. `WindowApp::about_to_wait` (`app::lifecycle`) falls back to calling `redraw()` directly
whenever there is no window to fire `RedrawRequested`: `redraw()` already ticks the sim and reconciles
menu/session state *before* its GPU-readiness guard ("Simulation must never be conditional on a swapchain
image" — its own comment), and returns as soon as it reaches that guard with nothing to draw into, so this
reuses the exact same pacing/catch-up logic a windowed frame uses rather than a second, divergent tick
loop. The browser target differs structurally (`spawn_app` hands the loop to the browser and returns
immediately) and is out of scope here — `Mode::HeadlessSession` is native-only, like `Mode::Headless`/
`Mode::Connect`.

**Runtime control.** `WindowApp` implements `ApplicationHandler<ShellEvent>`, where `ShellEvent` is
`app::AppEvent` with the feature on. `AppEvent::AttachPresentation { enable_input }` /
`DetachPresentation` / `ArmInput` / `Quit` are delivered through winit's `user_event` callback, which
(like every `ApplicationHandler` method) carries a live `&ActiveEventLoop` — the mechanism the owner's own
steer on the issue named: attach/detach the ECS systems at runtime, driven from outside the loop.
`app::runners::run_headless_session` is one concrete driver: it spawns a background thread that reads
`attach` / `attach input` / `arm` / `disarm` / `detach` / `quit` from stdin and forwards them through an
`EventLoopProxy`. A library caller wants the same thing without a binary: call `EventLoop::create_proxy()`
before `run_app` and `EventLoopProxy::send_event` from wherever the decision is made — `user_event` is the
reusable mechanism, the stdin loop is just how the shipped binary drives it.

**Input is inert on attach by default.** `WindowApp::input_armed` starts `true` for ordinary startup
(unchanged behaviour) and `false` whenever `AppEvent::AttachPresentation` creates a window on a
previously headless session, unless the caller explicitly asked for `enable_input: true`. While
`input_armed` is `false`, `window_event`/`device_event` (`app::lifecycle`) swallow every
`KeyboardInput`/`MouseInput`/`MouseWheel`/`CursorMoved`/mouse-motion event before the rest of the match
runs; window management (resize, focus, close, redraw) is unaffected. This is the issue's one resolved
open question: a script driving the client headlessly must not start receiving an operator's keystrokes
just because someone attached a window to watch it.

## How to change it

- Adding a fifth presentation-only system: tag its `add_systems` call with
  `.in_set(crate::sim::presentation::PresentationSet)` in whichever of the four plugins' own
  `add_presentation_systems` functions it belongs to (or a new plugin, added the same way). Nothing else
  needs to change — `detach`/`attach` iterate the set, not a system list.
- A system that must **not** be presentation-only but currently lives in one of the four plugins: move its
  registration out of `add_presentation_systems` into a plain `Plugin::build` call (or a new plugin) that
  is never removed.
- Changing which schedules are swept: `crate::sim::presentation::runtime::{detach, attach}` name `Update`,
  `GameTick` and `Extract` explicitly — a new schedule needs a line in both.
- A runtime trigger other than stdin: build an `EventLoopProxy<AppEvent>` (`event_loop.create_proxy()`,
  before `run_app`) and call `send_event` from wherever the decision is made (a signal handler, another
  thread, an embedding host's own event loop). `WindowApp::user_event` is the one thing every trigger goes
  through.

### Gotchas

- **Attach is not idempotent by itself.** `WindowApp::attach_presentation`/`Sim::attach_presentation` both
  check "already attached" before doing anything, specifically because `add_systems` silently duplicates
  a system rather than erroring. Don't call `crate::sim::presentation::attach` (the raw function) directly
  outside `Sim::attach_presentation` — that guard is what makes it safe.
- **`ScheduleCleanupPolicy::RemoveSetAndSystems` panics on this crate's real `Update` schedule** (see
  above) — use `RemoveSystemsOnly`, not the crate's own default, if you touch this code.
- **The renderer is not systems.** `WindowApp::redraw` is an imperative method holding `&mut` field
  borrows; there is nothing to detach on the render side except the `Option` fields themselves.

## Configuration

- Cargo feature `runtime-presentation` on `lodestone-shell`, in `default`. Off: `Mode` is fixed at startup
  exactly as before this issue, and none of `Sim::attach_presentation`/`detach_presentation`,
  `WindowApp::attach_presentation`/`detach_presentation`, `AppEvent`, or `Mode::HeadlessSession` compile in
  — checked by `cargo check -p lodestone-shell --no-default-features` (part of `just check-seam`) staying
  green with none of those symbols in the binary. `crate::sim::presentation::PresentationSet` itself stays
  unconditional (a zero-cost marker), so the four plugins need no `cfg` of their own.
- `--headless-session` CLI flag (native only, behind the feature): starts `Mode::HeadlessSession` — see
  `app::runners::run_headless_session`'s own doc for the stdin command set.

### A known limitation, stated plainly

`winit` remains an **unconditional** dependency of `lodestone-shell` — `cargo tree -p lodestone-shell` (any
feature combination) still lists it, because `WindowApp`/`Mode::Window` needed it before this issue and
still do. Making a genuinely winit-free build of this crate possible would mean gating the pre-existing
windowed implementation itself, which reaches beyond `app/`/`sim/` into `keybinds.rs`, `config.rs`,
`hud.rs` and `menu/nav.rs` (the last of which is outside this issue's assigned scope). That refactor is
real and larger than this issue; it was not attempted here. What `runtime-presentation` off does prove,
checkably, is that none of the *new* attach/detach machinery this issue adds is present in the binary.

## Dependencies

- `bevy_ecs` 0.19's `Schedule::remove_systems_in_set`/`ScheduleCleanupPolicy` — the removal mechanism.
- `winit` 0.30's `ActiveEventLoop::create_window` (available from every `ApplicationHandler` callback) and
  `EventLoop::<T>::with_user_event()`/`EventLoopProxy<T>` — window creation mid-process and the runtime
  control channel.
- `wgpu` 30's `Instance::generate_report()` — the measurement in
  `crate::gpu::pixel_gates::detach_presentation_releases_wgpu_resources`, a `#[ignore]`d GPU gate. Not
  used anywhere in production code, only in the test that proves detach releases real resources.
