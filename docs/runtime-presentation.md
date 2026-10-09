# Runtime presentation attach/detach

## What it is

Lets a running session switch between headless and windowed while it runs. A session can start with no window, GPU or presentation-only ECS systems and later get a window, or a windowed session can drop its window and GPU state and keep ticking. Behind the `runtime-presentation` feature on `lodestone-shell` (on by default).

## How it works

Two independent halves:

1. **ECS systems.** `Sim::client_app` adds four plugins purely to feed a renderer: `crate::entities::EntityInterpPlugin`, `crate::display_entities::DisplayEntityPlugin`, `crate::mesher::TerrainPlugin` and `crate::interact::InteractPlugin`. Every system they register is tagged `crate::sim::presentation::PresentationSet`. `Sim::detach_presentation` calls `Schedule::remove_systems_in_set(PresentationSet, ScheduleCleanupPolicy::RemoveSystemsOnly)` on `Update`, `GameTick` and `Extract`, which stops real CPU work (meshing, particles, interpolation), not just drawing, while keeping edges for surviving systems ordered against removed ones. Do not use `RemoveSetAndSystems` (the crate default): it reproducibly panics in `bevy_ecs` 0.19.1's `SystemSets::check_type_set_ambiguity` on the next rebuild of this session's real `Update` schedule (see `crate::sim::presentation::runtime::remove_from`). `RemoveSystemsOnly` leaves the empty set node, which `attach`'s `.in_set(PresentationSet)` repopulates.
2. **GPU state.** `WindowApp` (`crate::app`) holds `window`, `gpu`, `target`, `render`, `hud`, `menu` and `container` as `Option`s. `detach_presentation` sets them `None`, dropping the last strong reference to every `wgpu` handle (swapchain, depth buffer, pipelines, atlases). `attach_presentation` creates a window via `ActiveEventLoop::create_window` (available in every `ApplicationHandler` callback, including `user_event`) and reuses ordinary startup bring-up (`create_and_attach_window`/`finish_bring_up`), so there is one GPU bring-up path.

Re-attach cannot rerun `Plugin::build`: `Sim` drops its `App` at construction (`sim/build.rs`), and `add_systems` does not deduplicate, so a second build would double every system. Each plugin exposes `add_presentation_systems(&mut World)`, called by both `Plugin::build` and `crate::sim::presentation::attach`. This is safe because `detach` is exact and `Sim::presentation_attached` (seeded `true`) guards both `Sim::attach_presentation` and `detach_presentation` against redundant calls.

**Event loop.** On native, `EventLoop::run_app` owns the thread, so a headless session still runs a loop: `app::runners::run_headless_session` calls `run_app` with a `WindowApp` starting `presentation_desired: false` (no window on `resumed`) and `Sim::detach_presentation()` applied. `WindowApp::about_to_wait` (`app::lifecycle`) calls `redraw()` directly when there is no window to fire `RedrawRequested`: `redraw()` ticks the sim and reconciles menu and session state before its GPU-readiness guard, then returns with nothing to draw into, reusing the same pacing and catch-up logic as a windowed frame. The browser hands the loop away (`spawn_app` returns), so `Mode::HeadlessSession` is native-only.

**Runtime control.** `WindowApp` implements `ApplicationHandler<ShellEvent>` (`app::AppEvent` with the feature on). `AppEvent::AttachPresentation { enable_input }`, `DetachPresentation`, `ArmInput` and `Quit` arrive via winit's `user_event`, which carries a live `&ActiveEventLoop`. `run_headless_session` spawns a thread reading `attach`, `attach input`, `arm`, `disarm`, `detach` and `quit` from stdin and forwarding them through an `EventLoopProxy`. A library caller takes `EventLoop::create_proxy()` before `run_app` and calls `send_event` from anywhere.

**Input is inert on attach by default.** `WindowApp::input_armed` starts `true` for ordinary startup and `false` when `AttachPresentation` creates a window on a headless session (unless `enable_input: true`). While false, `window_event`/`device_event` swallow keyboard, mouse button, wheel, cursor and mouse-motion events; resize, focus, close and redraw still work. A scripted headless client must not receive an operator's keystrokes just because someone attached a window to watch.

### The winit-free headless build

`winit` is absent from the resolved graph with `--no-default-features`: `cargo tree -p lodestone-shell --no-default-features -i winit` fails to match. `cargo xtask check-no-winit-headless` (`xtask/src/no_winit_headless.rs`, part of `just check-seam`) runs that command and fails if `winit` is reachable; it has been watched to fail against a deliberately reintroduced dependency.

- `winit` is optional, pulled only by `window = ["dep:winit", "lodestone-render/window"]`; `lodestone-render` no longer hardcodes its own `window` feature from the shell.
- The `app` module (`WindowApp`, winit events, windowed and `--headless-session` runners) is `#[cfg(feature = "window")]` in `lib.rs`, gated whole because it is one cohesive windowed driver.
- `Mode::Headless` (one-shot offscreen render) and `Mode::Connect` (event-stream diagnostic) live in the always-compiled, winit-free `crate::diagnostics`. `crate::run` dispatches to `app::run` with `window` on, or to diagnostics (refusing `Mode::Window` with a named error) with it off. `main.rs` calls `crate::run`; the browser build (`web/`) calls `app::run` directly.
- `crate::keybinds::Binding` (persisted in `options.json`, read by `config` and `menu::nav`) now uses its own `Key` and `MouseButton` enums mirroring winit 0.30's variants. `From<winit::keyboard::KeyCode> for Key` and `From<winit::event::MouseButton> for MouseButton` exist only behind `window`, and call sites convert with `.into()` inside `app`. `config.rs`, `hud.rs` and `menu/nav.rs` needed no change (their winit references were test-only).

A `--no-default-features` build keeps `Mode::Headless` and `Mode::Connect`; `Mode::Window` (and `Mode::HeadlessSession`, since `runtime-presentation` implies `window`) refuse with a named error.

## How to change it

- New presentation-only system: `.in_set(PresentationSet)` in the owning plugin's `add_presentation_systems` (or a new plugin the same way); `detach`/`attach` iterate the set.
- A system that must not be presentation-only: move its registration into a plain `Plugin::build` that is never removed.
- Another swept schedule: add a line to both `crate::sim::presentation::runtime::{detach, attach}`.
- Another runtime trigger: build an `EventLoopProxy<AppEvent>` before `run_app` and `send_event` (signal handler, thread, host event loop); `WindowApp::user_event` is the one funnel.
- Gotchas: attach is not idempotent by itself (never call `crate::sim::presentation::attach` outside `Sim::attach_presentation`); the renderer is not systems (`WindowApp::redraw` is an imperative method, so only the `Option` fields detach).

## Configuration

- `runtime-presentation` on `lodestone-shell` (default, implies `window`). Off, `Mode` is fixed at startup and `Sim::attach_presentation`/`detach_presentation`, `WindowApp`'s versions, `AppEvent` and `Mode::HeadlessSession` do not compile (checked by `just check-seam`). `PresentationSet` stays unconditional (a zero-cost marker).
- `--headless-session` (native, behind the feature): see `app::runners::run_headless_session` for the stdin commands.
- `window` on `lodestone-shell` (default): gates the whole `app` module and pulls `winit` and `lodestone-render`'s `window`.

## Dependencies

- `bevy_ecs` 0.19 `Schedule::remove_systems_in_set` and `ScheduleCleanupPolicy`.
- `winit` 0.30 `ActiveEventLoop::create_window`, `EventLoop::<T>::with_user_event()` and `EventLoopProxy<T>`.
- `wgpu` 30 `Instance::generate_report()`, used only by the ignored GPU gate `crate::gpu::pixel_gates::detach_presentation_releases_wgpu_resources`.
