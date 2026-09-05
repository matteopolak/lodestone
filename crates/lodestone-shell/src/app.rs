//! The windowed driver and the headless / connect runners.
//!
//! This is deliberately the *thin* layer: all simulation lives in [`crate::sim`]
//! and all GPU state in [`crate::gpu`] / [`crate::hud`]. Here we only translate
//! winit lifecycle + input events into calls on those, and own the per-frame
//! acquire → render → present dance — including treating surface loss/outdated
//! as routine (on macOS they are), reconfiguring and moving on.
//!
//! # Module layout
//!
//! This file is the module *root*: `lib.rs` still says `pub mod app;` and every
//! public path is still `app::X`. The submodules are private and re-exported
//! here, so nothing outside this directory changed. What lives where:
//!
//! * `input` --- Key and mouse resolution: the `KeyGate` precedence chain and its outcomes.
//! * `pacing` --- Frame pacing: the tick-catch-up clamp and the unfocused/occluded schedule.
//! * `launch` --- Starting a session: the integrated server, seed resolution, and `LaunchError`.
//! * `weather` --- Weather and time-of-day plumbing for the render crate's rain/snow columns.
//! * `recipe_panel` --- The recipe-book panel's layout, geometry and unlock toasts.
//! * `session` --- `WindowApp` construction, cursor grab, and session start/teardown.
//! * `container_input` --- `WindowApp`'s container-screen gestures: clicks, swaps, drops, pick-item.
//! * `menus` --- `WindowApp`'s menu keyboard/mouse routing and the `MenuAction` match.
//! * `redraw` --- `WindowApp::redraw`: per-frame HUD assembly and render orchestration.
//! * `lifecycle` --- The winit `ApplicationHandler`: window lifecycle and raw event routing.
//! * `runners` --- The three run modes: windowed, headless PPM capture, and connect-only.
//!
//! Shared helpers that more than one submodule needs stay in this file, where
//! root-private visibility already reaches every descendant --- that is why
//! `sky_fog`, the input predicates, `DOUBLE_CLICK_WINDOW` and `WindowApp`
//! itself are here rather than in a submodule.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

// The portable clock, not `std::time::Instant`: this module's submodules all say
// `use super::*`, so this one import is what gives `lifecycle`, `session`,
// `runners`, `weather`, `menus` and `advancements_screen` a clock that works in a
// browser as well as in a window. `std::time::Instant::now()` compiles for wasm32
// and panics when it runs — see `crate::platform`.
use crate::platform::Instant;

// `HeadlessTarget`/`TargetError` moved to `crate::diagnostics` along with
// `Mode::Headless`'s own offscreen render — not needed here any more.
use lodestone_render::{GpuContext, RenderTarget, fog::FogSettings};
// Native-only: it blocks on adapter/device selection. The browser arm awaits
// `attach_window_async` from `spawn_local` instead — see `app::lifecycle::resumed`,
// which is split at exactly that seam.
#[cfg(not(target_arch = "wasm32"))]
use lodestone_render::window::attach_window;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use crate::chat::ChatInput;
use crate::config::{Config, Mode};
use crate::container::{
    ContainerFrame, ContainerRenderer, MenuButton, MenuContext, MenuHit, MenuInput,
    MenuKey as ContainerMenuKey,
};
use crate::gpu::RenderState;
use crate::hud::{HotbarSlot, HudFrame, HudRenderer};
use crate::keybinds::{Binding, InputAction, Keybinds};
use crate::menu::nav::{MenuAction, MenuKey, MenuNav};
use crate::menu::render::MenuRenderer;
use crate::menu::status::StatusCache;
use crate::menu::{SessionKind, UiState};
use crate::net::NetClient;
use crate::sim::Sim;
use lodestone_assets::ResourceLocation;
use lodestone_controller::{Action, InputState};
use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::menu::Menu;
use lodestone_game::recipe::RecipeBook;

mod advancements_screen;
mod benchmark;
mod container_input;
mod creative_screen;
mod frame_profile;
mod frame_profile_dump;
mod friends;
mod input;
mod launch;
mod lifecycle;
mod menus;
mod pacing;
mod recipe_panel;
mod redraw;
mod runners;
mod session;
mod weather;

// Re-exported so `app::X` is still the path it was before the split. The
// `allow` is because a name only the owning submodule and the `#[cfg(test)]`
// modules read still has to keep its `app::X` path, and a non-test build then
// sees the re-export as unused.
#[allow(unused_imports)]
use advancements_screen::{advancements_panel_geometry, advancements_title};
#[allow(unused_imports)]
use benchmark::{BenchmarkDriver, BenchmarkSegment};
use friends::FriendsApp;
#[allow(unused_imports)]
pub(crate) use creative_screen::CreativeSearchEdit;
#[allow(unused_imports)]
use creative_screen::creative_panel_geometry;
#[allow(unused_imports)]
pub(crate) use input::{
    KeyGate, KeyOutcome, clipboard_seam, copy_location_command, debug_enabled_feedback,
    debug_feedback, debug_shown_feedback, drop_selected_action, offhand_swap_action, resolve_key,
};
#[allow(unused_imports)]
pub(crate) use launch::{LaunchError, launch_singleplayer};
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
pub(crate) use launch::launch_open_to_lan_online;
#[allow(unused_imports)]
use frame_profile::{FramePhase, FrameProfiler, HudSubphase};
#[allow(unused_imports)]
use launch::{java_string_hash_code, parse_seed, requested_a_connection, resolve_launch_seed};
#[allow(unused_imports)]
pub(crate) use pacing::{
    BACKGROUND_POLL, FramePacer, FrameStep, MAX_CATCHUP_SECS, MAX_TICKS_PER_UPDATE, TICK_SECS,
    UNFOCUSED_FPS, UNFOCUSED_FRAME_INTERVAL,
};
#[allow(unused_imports)]
use recipe_panel::{
    RECIPE_SEARCH_MAX_LEN, RecipePanelState, auto_fill_clicks, recipe_book_type_for,
    recipe_item_identifier, recipe_panel_contents, recipe_panel_geometry, recipe_panel_layout,
    recipe_panel_pointer_hit, recipe_toast_now_ms, recipe_toast_view,
};
#[allow(unused_imports)]
use runners::run_windowed;
#[allow(unused_imports)]
use runners::run_windowed_with_app;
// Same carried-`cfg` reasoning as the import just below, but for the
// runtime-presentation runner: `run_headless_session` only exists for
// `cfg(all(not(wasm32), feature = "runtime-presentation"))`.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
#[allow(unused_imports)]
use runners::run_headless_session;
#[allow(unused_imports)]
use weather::{ContinuousTimeOfDay, ShellWeatherProbe, WeatherTracker, weather_columns_for_frame};

/// The winit user-event type this app's event loop carries.
///
/// `()` — winit's own default — with `runtime-presentation` off, so a build
/// without the feature gets exactly the `EventLoop::new()` shape it had
/// before this issue, no `AppEvent` machinery compiled in, and `user_event`
/// falls back to `ApplicationHandler`'s no-op default (nothing ever sends a
/// `()`). `AppEvent` with the feature on, so a caller outside the event loop
/// can reach a running session — see [`AppEvent`] and
/// `app::runners::run_headless_session`.
#[cfg(feature = "runtime-presentation")]
pub(crate) type ShellEvent = AppEvent;
#[cfg(not(feature = "runtime-presentation"))]
pub(crate) type ShellEvent = ();

/// Custom winit events for driving presentation attach/detach from outside
/// the event loop — the concrete shape of a runtime attach/detach: attach
/// the presentation-only ECS systems and the GPU state at runtime, as long as
/// both can also be removed again. Delivered through
/// [`ApplicationHandler::user_event`](winit::application::ApplicationHandler::user_event),
/// which — like every `ApplicationHandler` callback — carries a live
/// `&ActiveEventLoop`, the thing `docs/runtime-presentation.md` cites as
/// closing the "can a window be created mid-process" question with no spike
/// needed.
#[cfg(feature = "runtime-presentation")]
#[derive(Debug, Clone, Copy)]
pub(crate) enum AppEvent {
    /// Create a window, GPU surface and `RenderState` for a session that is
    /// currently headless, and re-attach [`crate::sim::Sim`]'s
    /// presentation-only ECS systems. `enable_input` seeds
    /// [`WindowApp::input_armed`] — see that field's own doc for why a
    /// runtime attach defaults it to `false`.
    AttachPresentation {
        enable_input: bool,
    },
    /// Arm or disarm live keyboard/mouse input on an already-attached
    /// window, independent of attach/detach.
    ArmInput(bool),
    /// Drop the window, GPU surface and `RenderState`, and detach `Sim`'s
    /// presentation-only ECS systems.
    DetachPresentation,
    /// Exit the event loop cleanly.
    Quit,
}

/// Entry point: dispatch on the configured mode.
///
/// # Errors
/// Returns an error if GPU bring-up or the event loop fails.
pub fn run(config: Config) -> anyhow::Result<()> {
    match config.mode {
        // `Headless` writes a PPM and `Connect` paces itself with
        // `std::thread::sleep`; both are CLI diagnostics, and both are native-only.
        // A browser reaches this function with `Mode::Window` — there is no command
        // line to select anything else — so the arms are refused rather than gated
        // into silence, which keeps "how did we get here?" answerable if a future
        // caller does set one.
        // **These two are behind the ownership gate as well**, and they are the
        // one pair the menu's own gate structurally cannot cover: neither ever
        // builds a `MenuAction`, so neither passes the `Entitlement` the play
        // verbs carry. `--connect` establishes a real session against a real
        // server and `--headless` renders a world; a compliance gate a command
        // line walks around is not a gate.
        //
        // The proof is a parameter on each runner rather than a check here, so
        // the requirement travels with the function: a third diagnostic mode
        // added later has to obtain one too, or it does not compile.
        // `require_owned_account`/`run_headless`/`run_connect` live in
        // `crate::diagnostics` now — unconditional, winit-free, and shared
        // with `crate::run`'s own `window`-off dispatch, so there is exactly
        // one copy of each rather than one per caller.
        #[cfg(not(target_arch = "wasm32"))]
        Mode::Headless => crate::diagnostics::run_headless(crate::diagnostics::require_owned_account()?, config),
        #[cfg(not(target_arch = "wasm32"))]
        Mode::Connect => crate::diagnostics::run_connect(crate::diagnostics::require_owned_account()?, config),
        #[cfg(target_arch = "wasm32")]
        Mode::Headless | Mode::Connect => Err(anyhow::anyhow!(
            "{:?} is a native CLI diagnostic mode: it needs a filesystem to write a \
             PPM to, or a raw TCP socket and a blocking sleep. A browser session is \
             always Mode::Window.",
            config.mode
        )),
        // Same ownership-gate reasoning as `Headless`/`Connect`
        // just above: this establishes a real, persistent session outside the
        // menu's own `Entitlement` check, so it needs the identical proof.
        #[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
        Mode::HeadlessSession => run_headless_session(crate::diagnostics::require_owned_account()?, config),
        #[cfg(all(target_arch = "wasm32", feature = "runtime-presentation"))]
        Mode::HeadlessSession => Err(anyhow::anyhow!(
            "HeadlessSession is a native CLI diagnostic mode: it needs a real OS \
             event loop and stdin for its attach/detach control thread, neither of \
             which a browser session has. A browser session is always Mode::Window."
        )),
        Mode::Window => run_windowed(config),
    }
}

/// [`run`], around a caller-composed [`lodestone_app::App`] instead of
/// [`Sim::client_app`]'s own — the missing half of the plugin-registration seam:
/// `Sim` could already adopt a pre-built `App` (`Sim::from_app`), but every real
/// entry point into the *windowed* client (`run`, `run_windowed`, `WindowApp::new`)
/// built its own with no way for a caller to add a plugin, so a third-party plugin
/// could reach a headless consumer (`ClientBuilder::ecs`) but never the shipped,
/// on-screen game. This is that entry point: a downstream crate calls
/// `Sim::client_app()`, adds its plugin, and hands the result here instead of to
/// `Sim::from_app` directly, to get the real winit-driven, GPU-rendered client
/// rather than a bare ticked `Sim`.
///
/// Only [`Mode::Window`] has a use for the extra `App` — `Headless`/`Connect`
/// are CLI diagnostics gated behind an ownership check with their own
/// unconditional composition (`crate::diagnostics`), and a headless *session*
/// or bot consumer already has `ClientBuilder::ecs` — so every other mode is
/// refused rather than silently dropping the caller's plugins.
///
/// # Errors
/// Returns an error if GPU bring-up or the event loop fails, or if `config.mode`
/// is not [`Mode::Window`].
pub fn run_with_app(app: lodestone_app::App, config: Config) -> anyhow::Result<()> {
    match config.mode {
        Mode::Window => run_windowed_with_app(app, config),
        other => Err(anyhow::anyhow!(
            "run_with_app only supports Mode::Window: {other:?} has its own \
             composition route (Sim::from_app directly for a diagnostic, or \
             ClientBuilder::ecs for a headless session/bot) that does not need \
             a windowed WindowApp at all."
        )),
    }
}

/// Sky distance fog sized to the shell's real render distance, so terrain
/// dissolves into the sky exactly where chunks stop loading rather than at the
/// render crate's default 8-chunk fallback. Driven once at render bring-up
/// (render distance is fixed for the session).
///
/// Delegates to [`crate::sim::fog_for_render_distance`] so the colour and the
/// fade band have one definition shared with the frame clear — a second copy of
/// the sky colour here is how the horizon ends up banding in a colour the sky
/// never is. `crate::diagnostics::run_headless` (the headless render path,
/// unconditional and outside this `window`-gated module) calls
/// `fog_for_render_distance` the same way rather than through this thin
/// wrapper, so the two never drift.
fn sky_fog(render_distance: u32) -> FogSettings {
    crate::sim::fog_for_render_distance(render_distance)
}

/// Canonical native framebuffer size for an opt-in live benchmark. Ordinary
/// play returns `None` and keeps winit's existing logical-size policy.
fn window_physical_size(config: &Config) -> Option<(u32, u32)> {
    config
        .benchmark
        .as_ref()
        .map(|_| crate::config::BenchmarkConfig::PHYSICAL_SIZE)
}

/// Select the one monitor carrying the platform's hardware built-in flag.
/// Names and primary-monitor status are deliberately absent: macOS can expose
/// generic names, and either external monitor may be the desktop primary.
fn select_builtin_monitor<T>(monitors: impl IntoIterator<Item = (T, bool)>) -> Option<T> {
    monitors
        .into_iter()
        .find_map(|(monitor, built_in)| built_in.then_some(monitor))
}

#[cfg(target_os = "macos")]
fn benchmark_builtin_monitor(event_loop: &ActiveEventLoop) -> Option<MonitorHandle> {
    use winit::platform::macos::MonitorHandleExtMacOS;

    select_builtin_monitor(event_loop.available_monitors().map(|monitor| {
        let built_in = objc2_core_graphics::CGDisplayIsBuiltin(monitor.native_id());
        (monitor, built_in)
    }))
}

#[cfg(not(target_os = "macos"))]
fn benchmark_builtin_monitor(event_loop: &ActiveEventLoop) -> Option<MonitorHandle> {
    // Other platforms do not expose CoreGraphics' laptop-panel flag. Keep the
    // benchmark usable there while macOS remains fail-closed and exact.
    event_loop.primary_monitor()
}

#[cfg(target_os = "macos")]
fn monitor_native_id(monitor: &MonitorHandle) -> Option<u32> {
    use winit::platform::macos::MonitorHandleExtMacOS;

    Some(monitor.native_id())
}

#[cfg(not(target_os = "macos"))]
fn monitor_native_id(_monitor: &MonitorHandle) -> Option<u32> {
    None
}

/// Benchmark sessions must measure the client rather than a persisted frame
/// limiter. Ordinary play returns its already-resolved target unchanged.
fn benchmark_target_fps(config: &Config, ordinary: Option<u32>) -> Option<u32> {
    if config.benchmark.is_some() {
        None
    } else {
        ordinary
    }
}

/// Benchmark sessions request wgpu's portable unthrottled present mode.
/// Ordinary play remains driven by the persisted VSync option.
fn benchmark_present_mode(
    config: &Config,
    ordinary: wgpu::PresentMode,
) -> wgpu::PresentMode {
    if config.benchmark.is_some() {
        wgpu::PresentMode::AutoNoVsync
    } else {
        ordinary
    }
}

/// Whether focus loss may put the pacer into its background schedule.
fn should_background_pace(config: &Config) -> bool {
    config.benchmark.is_none()
}

/// Maps a winit mouse button to the container-click gesture it drives.
/// `None` for anything but left/right/middle (e.g. the back/forward mouse
/// buttons some mice send), which the container screen has no use for.
///
/// **Deliberately not routed through [`crate::keybinds`]**, and vanilla agrees:
/// `AbstractContainerScreen` tests raw button indices 0/1/2 rather than
/// consulting a `KeyMapping`. Slot-click gestures are container-UI chrome, not
/// gameplay bindings — the same boundary that keeps the arrow keys out of the
/// keybind table (see that module's docs).
fn menu_button_for(button: MouseButton) -> Option<MenuButton> {
    Some(match button {
        MouseButton::Left => MenuButton::Left,
        MouseButton::Right => MenuButton::Right,
        MouseButton::Middle => MenuButton::Pick,
        _ => return None,
    })
}

/// The movement [`Action`], if any, that `code` drives under `binds`.
///
/// Replaces the old hardcoded `action_for`. Two behavioural notes:
///
/// * The old table bound Sneak to **either** shift and Sprint to **either**
///   control. A [`Binding`] names one physical key, matching vanilla (whose
///   defaults are `LEFT_SHIFT` / `LEFT_CONTROL` specifically), so the
///   right-hand modifiers no longer walk. This is the one intentional
///   behaviour change in the refactor; the right-hand keys are now rebindable
///   to whatever the player wants instead of being a silent alias.
/// * The scan is over [`InputAction::ALL`] in declaration order, so the
///   movement actions are checked before anything else and a key bound to two
///   movement actions resolves to the earlier one — deterministic rather than
///   map-iteration-order dependent.
fn movement_action_for(binds: &Keybinds, code: KeyCode) -> Option<Action> {
    InputAction::ALL
        .into_iter()
        .filter(|a| binds.is(*a, code.into()))
        .find_map(InputAction::movement)
}

/// Wire button number for an off-hand `SWAP` click — vanilla's literal `40`,
/// used both by the hotbar-key intercept that emits it and by the container
/// menu's own `SWAP` click arm that matches it. Not a slot index: the
/// off-hand's *native* index happens to be 40 too, which is why this is named
/// after the button rather than the slot.
const OFFHAND_SWAP_BUTTON: i32 = 40;

/// The hotbar slot index `0..=8` that `code` selects under `binds`, or `None`.
///
/// Replaces the old hardcoded number-row table. The `Hotbar1 → 0` off-by-one
/// lives in [`InputAction::hotbar_slot`], not here, so there is one place to get
/// it wrong.
fn hotbar_slot_for(binds: &Keybinds, code: KeyCode) -> Option<usize> {
    InputAction::ALL
        .into_iter()
        .filter(|a| binds.is(*a, code.into()))
        .find_map(InputAction::hotbar_slot)
}

/// The gameplay action, if any, that this **mouse button** invokes under
/// `binds` — the mouse-side twin of [`movement_action_for`].
///
/// Only attack and use are mouse-bindable in practice, but this scans the whole
/// table so a player who binds, say, jump to the middle button gets what they
/// asked for rather than nothing.
fn mouse_action_for(binds: &Keybinds, button: MouseButton) -> Option<InputAction> {
    InputAction::ALL
        .into_iter()
        .find(|a| binds.is_mouse(*a, button.into()))
}

/// Whether the world's own HUD — hotbar, hearts, hunger, the XP bar — draws on
/// this screen.
///
/// **It belongs to the world, not to active play.** Vanilla extracts the entire
/// HUD whenever a level is loaded and lets whatever screen is open paint its own
/// translucent background *over* it; that background is the dim, and it is the
/// only reason an open inventory looks different from playing:
///
/// - Vanilla's own render-extract step computes a "ready for level rendering"
///   flag from resources-loaded, advance-game-time and a non-null level, and
///   passes it straight into the GUI extract — note it asks about the
///   *level*, never about the open screen.
/// - The GUI extract step runs the HUD's own extract under that flag alone,
///   then draws the open screen **afterwards**, i.e. on top.
/// - The HUD's own extract gates only on F1 ("hidden") and the loading
///   screen. Inside it, the hotbar, hearts, hunger, the XP bar and the
///   held-item name are gated on **game mode** only — nothing there consults
///   the open screen.
///
/// Exactly two HUD elements in vanilla do consult the open screen, and
/// neither is a vital: the potion-effect icons (suppressed only when the
/// screen reports it draws its own, which the inventory and creative
/// inventory screens both do) and the subtitle overlay. The crosshair is
/// **not** one of them (it gates on camera type and spectator mode only) —
/// we still hide it with [`crate::menu::UiState::is_playing`], a deliberate
/// divergence while container screens have no dimmed background pass to hide
/// behind.
///
/// [`Screen::Connecting`] is excluded because there is no world yet — it is
/// an `owns_frame` screen, so `draw_menu` returns early and
/// this function is never even asked about it, but the negative control in
/// `app/tests.rs` asserts the answer anyway so the set cannot silently grow.
/// The menu and error screens never get here at all: `draw_menu` returns early.
fn hud_follows_world(screen: crate::menu::Screen) -> bool {
    use crate::menu::Screen;
    // `Screen::Death` follows the same rule as `Paused`: vanilla's own HUD
    // extract gates only on F1/the loading screen, never on which screen is
    // open, so the hotbar/hearts/hunger keep drawing (dimmed by the death
    // screen's own background pass) behind the death screen too.
    matches!(
        screen,
        Screen::Playing | Screen::Chat | Screen::Container | Screen::Paused | Screen::Death
    )
}

/// Vanilla's own scroll-delta transform, which is the boundary **both**
/// wheel consumers read: with discrete scroll on, the delta collapses to its
/// sign; either way the result is then multiplied by the wheel-sensitivity
/// option.
///
/// ## Why it is one function, applied in two arms
///
/// Vanilla computes the scaled offset **once** and hands the same value to
/// both the open screen's own scroll handler and to the hotbar/list scroll
/// handler. So neither the discrete-scroll option nor the sensitivity option
/// is a hotbar option or a list option — they define what a wheel notch *is*,
/// before anything decides what to do with it. Putting the transform inside
/// either consumer would have made the other one silently ignore both
/// options, which is how discrete-scroll support came to have only one
/// consumer this shell could actually reach.
///
/// **The order matters and is vanilla's:** `signum` first, *then* the sensitivity
/// multiply. Reversed, a sensitivity of 2.0 with discrete scrolling on would yield
/// `signum(2 * dy) == 1.0` and the option would silently cap wheel speed at one notch
/// — the sensitivity row would stop working whenever this row was on.
///
/// Free function rather than a `WindowApp` method so it is testable without a window,
/// exactly as [`accumulate_scroll`] and [`resolve_key`] are.
fn scale_scroll(dy: f64, discrete: bool, sensitivity: f32) -> f64 {
    let d = if discrete {
        // Vanilla's own sign function returns 0.0 for 0.0 — not 1.0.
        // `f64::signum` returns 1.0 for +0.0, so a zero delta must be handled
        // explicitly or a stationary wheel would scroll one notch per event.
        if dy == 0.0 { 0.0 } else { dy.signum() }
    } else {
        dy
    };
    d * f64::from(sensitivity)
}

/// The F3 overlay's fixed adapter block — see
/// [`crate::hud::DebugStats::adapter`] for why these three lines and not
/// vanilla's JVM-shaped ones.
///
/// `max_bind_groups` is here because it has already caused a crash class in this
/// repo: it reads `4` on a plain adapter and `8` on this Mac, which is why the
/// model shader is pinned at four groups (`CLAUDE.md`'s rendering constraints).
/// Having it on screen means the next person hitting it can read it rather than
/// deduce it.
fn adapter_lines(gpu: &GpuContext) -> Vec<String> {
    let info = gpu.adapter().get_info();
    let limits = gpu.device().limits();
    vec![
        format!("GPU {}", info.name),
        format!("{:?} / {:?}", info.backend, info.device_type),
        format!(
            "BIND GROUPS {} TEX2D {}",
            limits.max_bind_groups, limits.max_texture_dimension_2d
        ),
    ]
}

/// How long after the last Render Distance change the new value goes live —
/// vanilla's literal `600` (milliseconds) applied by its own delayed-slider
/// commit logic.
pub(crate) const RENDER_DISTANCE_APPLY_DELAY: Duration = Duration::from_millis(600);

/// GLFW's own scale for a **precise** scrolling delta — a trackpad or a Magic
/// Mouse, as opposed to a notched wheel.
///
/// `glfw/src/cocoa_window.m`'s `scrollWheel:` handler:
///
/// ```objc
/// double deltaX = [event scrollingDeltaX];
/// double deltaY = [event scrollingDeltaY];
/// if ([event hasPreciseScrollingDeltas]) {
///     deltaX *= 0.1;
///     deltaY *= 0.1;
/// }
/// _glfwInputScroll(window, deltaX, deltaY);
/// ```
///
/// This is what vanilla's own scroll offset already has applied by the time
/// its mouse handler sees it, so it belongs at *our* boundary too rather
/// than anywhere downstream.
const PRECISE_SCROLL_SCALE: f64 = 0.1;

/// One winit scroll event as the **notch count** vanilla's own scroll-callback
/// parameter carries.
///
/// The two delta kinds are different units and were being read as the same one:
/// `LineDelta` is already notches, but `PixelDelta` is raw points, and a trackpad
/// event carrying `p.y == 12.0` was arriving as *twelve* notches. Downstream that
/// is twelve times the scroll-rate option — 144 px of a settings list in one event, which is the
/// owner's "scrolling is a fixed jump" report, and twelve hotbar slots for the
/// same flick.
///
/// [`PRECISE_SCROLL_SCALE`] is the conversion, taken from GLFW rather than tuned:
/// it is by definition the number vanilla receives for the same physical gesture.
///
/// Free function for [`scale_scroll`]'s reason — and applied *before* it, because
/// the discrete-scroll option's sign-collapse must see a notch count. Applied
/// after, a trackpad event would be `signum(12.0) == 1.0` either way and the
/// option would mask the bug rather than the bug showing.
fn wheel_notches(delta: winit::event::MouseScrollDelta) -> f64 {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y),
        winit::event::MouseScrollDelta::PixelDelta(p) => p.y * PRECISE_SCROLL_SCALE,
    }
}

/// Vanilla's own scroll-wheel handler: folds a sensitivity-scaled,
/// possibly-fractional scroll offset into a whole number of hotbar slots,
/// carrying the remainder in `accum` across calls so a sensitivity below 1.0
/// does not silently drop sub-notch scroll.
///
/// `accum` resets to zero on a direction reversal (the scaled offset's sign
/// differing from the accumulator's own) rather than fighting the new
/// direction with old carry — one hard flick back should not need to "pay
/// off" the previous direction's fractional debt first.
fn accumulate_scroll(accum: &mut f64, scaled: f64) -> i32 {
    if *accum != 0.0 && scaled.signum() != accum.signum() {
        *accum = 0.0;
    }
    *accum += scaled;
    let whole = accum.trunc();
    *accum -= whole;
    whole as i32
}

/// Vanilla's own hotbar-selection step: collapses the whole-notch count
/// [`accumulate_scroll`] returns to its **sign** before it becomes a
/// hotbar-slot step — the selection index is decremented by that sign,
/// clamped/wrapped to the hotbar's length.
///
/// [`accumulate_scroll`] can legitimately return a magnitude greater than one:
/// a high sensitivity setting, or — the common case on a trackpad — a
/// single pixel-delta event large enough that its scaled offset crosses
/// several whole notches at once. Vanilla never turns that magnitude into
/// several slots; the hotbar always advances by exactly one slot per scroll
/// *event*, discarding the rest of that event's whole-notch count rather than
/// queuing it for a later one. Passing the raw magnitude straight to
/// [`crate::sim::Sim::cycle_slot`] instead is the owner's "scroll a bit,
/// nothing happens; scroll more, it jumps like six slots" report: small
/// deltas sit in [`accumulate_scroll`]'s fractional carry exactly as vanilla's
/// dead zone does (correct), and then a brisk flick's single large event
/// jumps several slots at once (the bug this function closes).
///
/// Free function for [`scale_scroll`]'s reason: testable without a window,
/// and its own thing to get right independent of the accumulator or the
/// scale.
fn hotbar_scroll_step(whole: i32) -> i32 {
    whole.signum()
}

/// What one raw physical key means while a Controls-menu bind button is
/// mid-capture — extracted as a pure function so the
/// decision is unit-testable without a window, the same reason
/// [`resolve_key`] and [`WindowApp::menu_key_for`] are split out. Deliberately
/// **not** `menu_key_for`: that function drops any physical key with no
/// printable `text` (F-keys, modifiers, arrows other than Up/Down), which is
/// exactly the common rebind target a Controls menu exists to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureKey {
    /// Escape: cancel the capture without changing the binding. Vanilla sets
    /// its own "unbound" sentinel on Escape unconditionally; this client
    /// deliberately does not — see
    /// [`crate::menu::nav::MenuNav::capture_binding`]'s own doc on why
    /// unconditional-Unbound is the `Pause` hazard.
    Cancel,
    /// Any other identified key: finish the capture with it.
    Bind(KeyCode),
}

fn capture_key_for(physical_key: PhysicalKey) -> Option<CaptureKey> {
    match physical_key {
        PhysicalKey::Code(KeyCode::Escape) => Some(CaptureKey::Cancel),
        PhysicalKey::Code(code) => Some(CaptureKey::Bind(code)),
        // No `KeyCode` to persist — nothing to do, the same as `menu_key_for`
        // falling through to its own `_ => {}`.
        PhysicalKey::Unidentified(_) => None,
    }
}

/// Maximum gap between two left-clicks on the same container slot for the
/// second to count as a double-click gather. Winit hands us raw button
/// up/down events with no click-count of its own, so this app tracks it —
/// [`container::MenuInput::press`] still requires the *same slot* on top of
/// this timing before it arms the gather.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

struct WindowApp {
    config: Config,
    /// Opt-in deterministic benchmark choreography. `None` is the ordinary
    /// player-controlled path and pays no per-frame state-machine work.
    benchmark: Option<BenchmarkDriver>,
    /// Last segment announced to the benchmark log, for transition-only
    /// markers rather than one line per frame.
    benchmark_segment: Option<BenchmarkSegment>,
    sim: Sim,
    window: Option<Arc<Window>>,
    gpu: Option<GpuContext>,
    target: Option<lodestone_render::SurfaceTarget<'static>>,
    render: Option<RenderState>,
    hud: Option<HudRenderer>,
    container: Option<ContainerRenderer>,
    grabbed: bool,
    /// Frame clock: clamps catch-up and throttles presentation when the window
    /// is unfocused or occluded. The sim ticks regardless.
    pacer: FramePacer,
    /// Playing ↔ paused screen state; owns cursor-grab intent and shutdown.
    ui: UiState,
    /// Menu selection, the add/edit form, and the saved server list.
    nav: MenuNav,
    /// Per-server status pings for the multiplayer list. Probes run on their own
    /// threads; `pump()` moves results into slots once per frame.
    statuses: StatusCache,
    /// Credential-free Friends view backed by the private service worker.
    friends: FriendsApp,
    /// Self-contained menu pipeline (own shader, own buffer, clears the frame).
    /// `None` until GPU bring-up.
    menu: Option<MenuRenderer>,
    /// Decoded favicon mosaics, so a server's PNG is inflated once rather than
    /// once per frame.
    favicons: crate::menu::render::FaviconCache,
    /// Last cursor position in physical pixels, for menu hit-testing. Physical
    /// because that is the space `SurfaceTarget::size` and the menu layout use;
    /// mixing in logical coordinates puts the hit rects at half scale on a
    /// Retina display.
    cursor: (f32, f32),
    /// F3 debug overlay visibility. **Starts off, as vanilla does** — press F3
    /// to bring it up.
    ///
    /// It used to start on, because §S4 treats it as the instrument rather than
    /// a feature. That reasoning still holds for the *content*, but it does not
    /// require the thing to be on screen by default, and a permanently-visible
    /// overlay is simply not what the game looks like. The stdout status line
    /// (`pos=… fps=… chunks=…`) is unaffected and remains the instrument that
    /// does not depend on anyone having pressed a key.
    show_debug: bool,
    /// Whether the debug modifier (F3) is currently held, for its chords.
    /// Fed into [`app::input::KeyGate::debug_held`]; see there for why this is a
    /// gate flag rather than a second bindable action.
    debug_held: bool,
    /// Whether a chord (F3+B/F3+G) fired while the modifier was held. Vanilla's
    /// own "did a debug action" flag: on release, the overlay toggles only if
    /// this is `false`, so F3+B does not also open the overlay.
    debug_chord_used: bool,
    /// F3+B — draw entity hitboxes. `Arc<AtomicBool>` because the only consumer
    /// is the `Send + Sync + 'static` closure `install_debug_lines_source`
    /// installs, which cannot borrow `self`.
    debug_hitboxes: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// F3+G — draw the borders of the chunk the player is in. Same `Arc` reason
    /// as [`Self::debug_hitboxes`].
    debug_chunk_borders: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Shift+F3 — the profiler pie chart's own visibility, independent of
    /// [`Self::show_debug`] (vanilla's F3 text overlay can be up with the
    /// chart off, and — unlike vanilla — this instrument's chart needs no
    /// live world to be meaningful, but it is only ever drawn while
    /// `show_debug` is also true; see `app::redraw`'s gate). A plain `bool`,
    /// not an `Arc<AtomicBool>` like [`Self::debug_hitboxes`]: nothing but
    /// `redraw` (same struct, same thread) ever reads this.
    show_profiler_chart: bool,
    /// The profiler pie chart's drilled-in wedge, or `None` at the root —
    /// vanilla's number-key/`0` navigation, as the F3 chord
    /// [`app::input::KeyOutcome::ProfilerChartSelect`] sets it. Persists
    /// across frames like [`Self::show_profiler_chart`] and for the same
    /// reason: `hud::ProfilerChart::selected` is read fresh from here every
    /// `redraw`, never accumulated in the HUD layer itself.
    profiler_chart_selected: Option<usize>,
    /// The menu row whose slider the mouse is currently dragging, if any —
    /// vanilla's own slider-drag flag.
    ///
    /// Holds the **row**, not just a flag, because a drag continues while the
    /// cursor leaves the row: once it has begun, the row is fixed and only the
    /// x matters. Cleared on mouse-up.
    menu_slider_drag: Option<usize>,
    /// The Render Distance value last seen on [`Self::nav`], for edge detection.
    ///
    /// Seeded from the launch config, so the first frame arms nothing.
    render_distance_seen: u32,
    /// When the pending Render Distance change becomes live — vanilla's own
    /// delayed-apply deadline for the slider.
    ///
    /// # Why deferred rather than per-frame
    ///
    /// Render distance is the one slider option vanilla builds with immediate
    /// apply turned off, and its own apply routine sets the deadline to 600 ms
    /// in the future, committed from the render extract once the deadline
    /// passes. Re-armed by *every* change, so the commit lands 600 ms after
    /// the drag stops, not 600 ms after it starts.
    ///
    /// Applying per frame instead would reload chunks on every pixel of the
    /// drag — which is exactly the cost the delay exists to avoid, and why the
    /// value being *stored* eagerly (it always was, see
    /// `config::Options::render_distance`) is not the same thing as it being
    /// *applied*.
    render_distance_apply_at: Option<Instant>,
    /// Whether the player-list binding is currently held (shows the overlay).
    tab_held: bool,
    /// A `key.screenshot` press waiting to be serviced.
    ///
    /// **The capture cannot happen at key time**, which is the whole reason
    /// this field exists rather than the effects arm doing the work: a
    /// swapchain image has no defined content until a pass has rendered into
    /// it, so reading the texture when the key arrives copies out either the
    /// *previous* frame or undefined memory. `redraw()` drains this flag
    /// immediately before `AcquiredFrame::present`, once world, HUD and every
    /// menu overlay have already written into `frame.view()`.
    pending_screenshot: bool,
    /// Resource-pack generation the **icon** surfaces were last rebuilt for —
    /// the flat item atlas and its glint sheet, the HUD and menu GUI atlases,
    /// and the special-renderer icon sheets.
    ///
    /// Deliberately a second latch rather than a reuse of `Sim`'s own
    /// `last_pack_generation`. That one is consumed by
    /// `Sim::reload_resource_pack_atlas` *before* its remaining guards, which
    /// then return `None` for the demo world, a jar-less run, or a pack whose
    /// own load falls back to the demo palette — so a caller that keyed icon
    /// refreshes off that method's `Some` lost the generation on any of those
    /// frames and, since the counter only moves forward, stranded every icon
    /// surface on the previous pack for the rest of the process. None of these
    /// surfaces reads the block atlas, so they were never the block reload's
    /// to gate. See `docs/resource-packs.md`.
    last_icon_pack_generation: u64,
    /// Editable buffer for the chat prompt; only consumed while chat is open.
    chat_input: ChatInput,
    /// Wrapped chat rows, persisted across frames — see
    /// [`crate::hud::ChatWrapCache`]. Lives here rather than in `hud` because
    /// the HUD `Frame` is rebuilt from scratch every frame and so can hold no
    /// state of its own.
    chat_wrap: crate::hud::ChatWrapCache,
    /// Press/drag/release state machine for the open container screen; see
    /// [`container::MenuInput`]. Drives every predicted click this app sends.
    menu_input: MenuInput,
    /// Whether either Shift key is currently held, tracked independently of
    /// `sim.input` (which only feeds movement `Action`s and is not read back).
    /// Container shift-clicks (`QuickMove`) need this even while the sim's own
    /// gameplay input is not being accepted.
    shift_held: bool,
    /// Whether either Control key is currently held, tracked the same way as
    /// [`Self::shift_held`] and for the same reason `resolve_key` needs it: to
    /// distinguish `key.drop`'s drop-one from drop-stack, which vanilla reads
    /// as a live Control-held check, not a bindable key of its own.
    ctrl_held: bool,
    /// The live winit modifier state — Shift/Control/Alt/Super — updated from
    /// `WindowEvent::ModifiersChanged` (`app::lifecycle`'s `window_event`).
    ///
    /// **This field's prior absence is what made Cmd+A/Cmd+V type letters
    /// instead of acting.** Every real `winit::event::KeyEvent` carries no
    /// modifier state of its own outside
    /// this; without tracking it, `app::menus::menu_key_for` had no way to
    /// distinguish Cmd+A from `a`, so a shortcut both failed to act *and*
    /// typed the letter it was chording with. `shift_held`/`ctrl_held` above
    /// are a narrower, older mechanism kept for their own two call sites
    /// (container shift-click, `key.drop`'s hold-Ctrl-for-stack) — this field
    /// is the general one `menu_key_for` and `handle_chat_key` consult for the
    /// macOS-aware `EDIT_SHORTCUT_MODIFIER` shortcuts (select-all/copy/cut/
    /// paste, `crate::menu::focus::EDIT_SHORTCUT_MODIFIER`).
    modifiers: ModifiersState,
    /// Fractional carry for the hotbar mouse-wheel scroll, so a sensitivity
    /// below 1.0 does not lose sub-notch scroll and above 1.0 can cross more
    /// than one slot per notch. Mirrors vanilla's own scroll accumulator:
    /// each event adds
    /// `dy * sensitivity`, [`accumulate_scroll`] truncates off the whole
    /// slots and keeps the remainder, and a direction reversal drops
    /// whatever was carried in the old direction rather than fighting it.
    /// Not persisted — vanilla's own accumulator does not survive a restart
    /// either, being a field on a `MouseHandler` that is rebuilt with the
    /// window.
    scroll_accum: f64,
    /// When the left button last pressed on the container screen, for
    /// [`DOUBLE_CLICK_WINDOW`]-based double-click detection.
    last_menu_click: Option<Instant>,
    /// Per-phase CPU frame timing — see `app::frame_profile`'s module doc.
    frame_profile: FrameProfiler,
    /// The fog settings last uploaded to the renderer, so submerged fog is
    /// re-uploaded only when it actually changes (the player crossing a
    /// water/lava surface) rather than every frame. Seeded to the sky fog set at
    /// render bring-up so the first frame above water is a no-op.
    applied_fog: Option<FogSettings>,
    /// The local crafting-recipe corpus (`crate::resources::load_recipe_book`),
    /// loaded once at GPU bring-up. `None` on a jar-less run or before it has
    /// loaded. Used only for the container screen's ghost-preview draw and the
    /// debug-overlay counter — the crafting result slot itself is always the
    /// server's, never a local match (see `docs/crafting.md`).
    ///
    /// This is a **cache** of `lodestone_ecs::RecipeRegistry`'s
    /// book, not the authority: plugins register recipes into that resource, and
    /// `Self::sync_recipe_book` re-clones this field when
    /// [`recipe_book_revision`](Self::recipe_book_revision) falls behind. Reads
    /// stay on a plain field rather than a guard because the four read sites are
    /// all inside the render pass, and `lodestone_ecs::EcsHandle`'s discipline
    /// forbids holding a guard across one.
    recipe_book: Option<RecipeBook>,
    /// The `RecipeRegistry::revision` [`recipe_book`](Self::recipe_book) was
    /// cloned at, so a plugin registering mid-session refreshes the cache exactly
    /// once rather than on every frame.
    recipe_book_revision: u64,
    /// Persisted recipe-book **panel** state: whether the panel is
    /// open, and the search/tab/page the user last left it on.
    ///
    /// Persisted across frames *and* across container open/close, deliberately:
    /// vanilla's own recipe-book-panel state lives on the client's own recipe
    /// book, not on the screen, so reopening a crafting table keeps the
    /// book open with the same tab. Rebuilding it per frame would reset the
    /// search box on every mouse move.
    recipe_panel: RecipePanelState,
    /// The recipe-unlock toast queue —
    /// [`lodestone_game::recipe::RecipeToastQueue`], drained into
    /// [`crate::hud::HudFrame::recipe_toast`] each frame.
    ///
    /// Pushed to by [`WindowApp::sync_recipe_toasts`], which diffs
    /// `Sim::known_recipes()` against [`Self::recipe_toast_seen`] every frame.
    /// The stale claim this doc comment used to carry — "has no live producer
    /// yet, the decode does not exist" — was true when written and is not any
    /// more: `decode_recipe_book_add` exists, folds into `SessionRecipeBook`,
    /// and now has a reader.
    recipe_toasts: lodestone_game::recipe::RecipeToastQueue,
    /// `RecipeDisplayId`s [`WindowApp::sync_recipe_toasts`] has already toasted
    /// (or, for the very first sync, seeded without toasting — see that
    /// method's doc for why the first `RecipeBookAdded` after connecting must
    /// not fire N toasts for a fresh join's whole unlock history).
    recipe_toast_seen: HashSet<i32>,
    /// Whether [`WindowApp::sync_recipe_toasts`] has performed its one-time
    /// seed of [`Self::recipe_toast_seen`] yet this session. Distinct from
    /// `recipe_toast_seen.is_empty()`: a fresh join with zero unlocked recipes
    /// (a brand-new player) must still latch this, or every frame would try to
    /// re-seed and any later real unlock would be silently treated as "already
    /// seen".
    recipe_toast_synced: bool,
    /// `RecipeDisplayId`s [`WindowApp::sync_recipe_book_seen`] has already
    /// reported to the server this session (vanilla's own "recipe book seen"
    /// packet), so a recipe whose button stays on screen for many frames is
    /// reported exactly once rather than every frame the panel stays open on
    /// that page.
    recipe_book_seen: HashSet<i32>,
    /// The bundle slot currently tracking a scroll-driven selection highlight
    /// (`BUNDLE_ITEM_SELECTED` / `SelectBundleItem`), or `None` when no bundle
    /// is being scrolled — see
    /// `crate::container::bundle`'s module doc for why this lives beside the
    /// menu rather than mutated inside it the way vanilla's own client does.
    bundle_selection: Option<crate::container::bundle::BundleSelection>,
    /// The world's weather, resolved from the net thread's cell once per frame.
    /// `None` before a session exists; installed alongside the other render
    /// sources by [`WindowApp::install_session_render_sources`] and cleared with
    /// the session, so a fresh connect never inherits the last one's storm.
    /// The rain **ambience** is deliberately absent from this struct. Its cadence
    /// lives in [`lodestone_render::RainAmbience`] (vanilla's own rain-sound
    /// timer, unit-tested), but it has **no producer**, because the only `ShellAudio` in
    /// the process is a private field on `Sim` with no public play method. Adding
    /// one `pub fn play_local_sound(&mut self, name: &str, category, pos, volume,
    /// pitch)` to `crate::sim::Sim` — forwarding to `self.audio` exactly as the
    /// `NetUpdate::Sound` arm in `Sim::poll_net` (`crate::sim::net_apply`)
    /// already does — is the whole remaining wiring. Recorded here rather
    /// than left as two dead fields, per
    /// `CLAUDE.md`'s island rule: an unused field reads as an oversight, a named
    /// blocker does not.
    weather: Option<Arc<WeatherTracker>>,
    /// The creative-inventory screen's own UI state: selected tab,
    /// scroll offset, search text.
    ///
    /// Persisted across open/close for the same reason
    /// [`recipe_panel`](Self::recipe_panel) is — vanilla's own selected-tab
    /// field is `static` on its creative screen class, so reopening the
    /// screen returns to the tab you left it on.
    ///
    /// Whether the screen is *showing* is not stored here: it is
    /// [`WindowApp::creative_screen_open`], derived from the container flag plus
    /// the player's own abilities, so the two can never disagree.
    creative: crate::container::CreativeState,
    /// The Advancements screen's in-flight viewport drag: the cursor
    /// position the last pan was measured from, in physical pixels.
    ///
    /// The *screen* state (tab, per-tab scroll) lives on [`MenuNav`] beside every
    /// other menu screen's; only the drag is here, because it is a property of the
    /// mouse rather than of the screen — the same split
    /// [`menu_slider_drag`](Self::menu_slider_drag) already makes.
    advancements_drag: Option<(f32, f32)>,
    /// The live advancement progress snapshot plus the completion-toast queue
    /// — see [`AdvancementsFeed`](advancements_screen::AdvancementsFeed).
    advancement_feed: advancements_screen::AdvancementsFeed,
    /// The world this process is *hosting*, if any.
    ///
    /// Set by [`WindowApp::begin_singleplayer`] and cleared on quit-to-title, and
    /// read by exactly one thing: the pause menu's Open to LAN, which republishes
    /// the same launch on a TCP port. `None` for a multiplayer session, which is
    /// what makes that button honest there — there is no world of ours to publish.
    hosted_world: Option<crate::menu::nav::SingleplayerLaunch>,
    /// The merchant screen's own UI state — vanilla's own "selected trade"
    /// field: which trade row is selected, whose out-of-stock overlay shows,
    /// and the index the next `SELECT_TRADE` send carries.
    ///
    /// Not reset on close, matching [`creative`](Self::creative)/
    /// [`recipe_panel`](Self::recipe_panel)'s own "persisted across open/close"
    /// precedent — vanilla's own field is scoped to its merchant screen
    /// object, which starts at `0` every time it is freshly constructed, but
    /// this client rebuilds no screen object per open, so restarting at `0` here would need an extra
    /// reset call at the one place a merchant menu opens, for a difference no
    /// player can see (`0` is what a stale value most often already is). A
    /// stale value past the real offer list is harmless either way: every
    /// reader (`container::geometry`'s `draw_merchant_trades`,
    /// `container::merchant::hit_test_local`) indexes with `.get()`/a bounded
    /// loop, never a bare index.
    merchant_selected: usize,
    /// The anvil rename box's persistent editable-text state — see
    /// [`crate::container::AnvilRenameState`]'s own module doc for the whole
    /// chain this closes. Synced from the input slot once per
    /// frame in `redraw`'s anvil-name computation, edited per keystroke by
    /// `KeyOutcome::AnvilRename`, and read back by `ContainerFrame::with_anvil_name`.
    ///
    /// Not reset on close, matching [`creative`](Self::creative)'s own
    /// precedent: [`crate::container::AnvilRenameState::sync`] re-seeds it
    /// from whatever is in slot 0 the next time an anvil is open, so a stale
    /// value between sessions is never visible.
    anvil_rename: crate::container::AnvilRenameState,
    /// The beacon screen's pending primary/secondary power selection
    /// (`SetBeaconEffects` remainder) — see
    /// [`crate::container::beacon::BeaconSelection`]'s own module doc.
    /// Synced from `container_data` once per frame in `redraw`, the same
    /// place [`Self::anvil_rename`] is, and edited by
    /// [`Self::handle_beacon_click`](crate::app::container_input).
    ///
    /// Not reset on close, matching [`anvil_rename`](Self::anvil_rename)'s
    /// own precedent: [`crate::container::beacon::BeaconSelection::sync`]
    /// re-seeds it from the menu's own `container_data` the next time a
    /// beacon is open, so a stale value between sessions is never visible.
    beacon_selection: crate::container::beacon::BeaconSelection,
    /// The stonecutter recipe grid's persisted scroll offset (`0.0..=1.0`,
    /// vanilla's own scroll-offset field) — advanced by
    /// [`WindowApp::scroll_stonecutter`], read by
    /// [`WindowApp::handle_stonecutter_click`] through
    /// [`crate::container::stonecutter::start_index_for_scroll`].
    ///
    /// Not reset on close or on an input-slot change, matching
    /// [`merchant_selected`](Self::merchant_selected)'s own precedent: a
    /// stale offset past the real match count is harmless, since
    /// [`crate::container::stonecutter::start_index_for_scroll`]'s own
    /// `offscreen_rows` clamp bounds it against whatever the *current*
    /// match count is, every time it is read.
    stonecutter_scroll: f32,
    /// The loom pattern grid's persisted scroll offset — the same shape as
    /// [`stonecutter_scroll`](Self::stonecutter_scroll), advanced by
    /// [`WindowApp::scroll_loom`] and read by
    /// [`WindowApp::handle_loom_click`] through
    /// [`crate::container::loom::start_row_for_scroll`].
    loom_scroll: f32,
    /// Game-rule overrides collected on Create New World's More tab, queued
    /// for exactly one send once the fresh singleplayer session
    /// reaches `SessionPhase::Connected` — see
    /// [`Self::drive_ui_from_session`]'s own arm and
    /// [`crate::sim::Sim::send_set_game_rules`]. `Option` rather than a bare
    /// (possibly empty) `Vec` so "already sent" and "nothing to send" are two
    /// different states: `take()` clears it the moment it is used, so a
    /// `Connected` phase read on every later frame (see that method's own
    /// doc on why it is not a one-shot transition) cannot resend it.
    pending_game_rules: Option<Vec<(lodestone_model::ResourceKey, String)>>,
    /// When the F3 debug overlay's own network-chart figures were last
    /// refreshed via a `ClientAction::PingRequest` send (`PingRequest`
    /// remainder) — `None` means "never, this session". Real
    /// vanilla re-sends this every client tick while its own network-chart
    /// sub-panel shows; this client has no such
    /// sub-panel, so the closest honest equivalent is "while F3 is open, at
    /// most once per second" — real, F3-gated traffic rather than either an
    /// unconditional per-tick spam or a send with no UI behind it at all.
    /// See [`Self::redraw`]'s own call site and
    /// [`crate::sim::Sim::send_ping_request`].
    last_ping_request: Option<Instant>,
    /// Whether this app wants a window as soon as the event loop
    /// resumes. `true` for ordinary startup ([`WindowApp::new`]) — unchanged
    /// from before this field existed, `resumed` always created a window
    /// unconditionally. `false` for
    /// [`WindowApp::new_headless_session`]: `resumed` then does nothing, and
    /// window/GPU/render bring-up only happens once an
    /// [`AppEvent::AttachPresentation`] arrives (`app::lifecycle`'s
    /// `resumed`/`user_event`). Also cleared by
    /// [`WindowApp::detach_presentation`], so a re-resume after a mid-session
    /// detach does not immediately recreate the window it just dropped.
    #[cfg(feature = "runtime-presentation")]
    presentation_desired: bool,
    /// The resolved open question on input while a window is attached at
    /// runtime: whether a real window's
    /// keyboard/mouse input reaches gameplay. `true` for ordinary startup —
    /// unchanged behaviour, so play is unaffected by this field existing.
    /// Set to whatever [`AppEvent::AttachPresentation`] asked for
    /// (`enable_input`) when a window is created on a previously headless
    /// session, and **defaults to `false` there**: a script driving the
    /// client headlessly must not start receiving an operator's keystrokes
    /// just because someone attached a window to watch it. Flipped
    /// explicitly with [`AppEvent::ArmInput`]. Consulted at the top of
    /// `window_event`/`device_event` (`app::lifecycle`), which swallow every
    /// keyboard/mouse/motion event while this is `false` and let window
    /// management (resize, focus, close, redraw) through unaffected.
    #[cfg(feature = "runtime-presentation")]
    input_armed: bool,
}

#[cfg(test)]
mod tests;

/// Gates for the recipe-book panel wiring.
///
/// The recipe-book UI landed fully built and unit-tested in `container.rs` and
/// `lodestone-game`, and reached **zero pixels** because the three call sites
/// that drive it live in `app.rs`/`sim.rs`/`hud.rs`. These gates measure the
/// wiring itself, not the geometry — `container.rs`'s own 75 tests already prove
/// the geometry is right, and every one of them passed while nothing drew.
#[cfg(test)]
mod recipe_book_wiring;
