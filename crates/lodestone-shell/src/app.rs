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
//! * `recipe_panel` --- The recipe-book panel's layout, geometry and unlock toasts (issue #163).
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

use std::sync::Arc;
use std::time::Duration;

// The portable clock, not `std::time::Instant`: this module's submodules all say
// `use super::*`, so this one import is what gives `lifecycle`, `session`,
// `runners`, `weather`, `menus` and `advancements_screen` a clock that works in a
// browser as well as in a window. `std::time::Instant::now()` compiles for wasm32
// and panics when it runs — see `crate::platform`.
use crate::platform::Instant;

use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget, TargetError, fog::FogSettings};
// Native-only: it blocks on adapter/device selection. The browser arm awaits
// `attach_window_async` from `spawn_local` instead — see `app::lifecycle::resumed`,
// which is split at exactly that seam.
#[cfg(not(target_arch = "wasm32"))]
use lodestone_render::window::attach_window;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::chat::ChatInput;
use crate::config::{Config, Mode};
use crate::container::{
    ContainerFrame, ContainerRenderer, MenuButton, MenuContext, MenuHit, MenuInput,
    MenuKey as ContainerMenuKey,
};
use crate::effects::EffectsRenderer;
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
mod container_input;
mod creative_screen;
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
pub(crate) use creative_screen::CreativeSearchEdit;
#[allow(unused_imports)]
use creative_screen::creative_panel_geometry;
#[allow(unused_imports)]
pub(crate) use input::{KeyGate, KeyOutcome, drop_selected_action, offhand_swap_action, resolve_key};
#[allow(unused_imports)]
pub(crate) use launch::{LaunchError, launch_singleplayer};
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
    recipe_panel_contents, recipe_panel_geometry, recipe_panel_layout, recipe_panel_pointer_hit,
    recipe_toast_now_ms, recipe_toast_view,
};
#[allow(unused_imports)]
use runners::run_windowed;
// The two CLI-diagnostic runners are `cfg(not(wasm32))` at their definitions — see
// `run` below — so the import has to carry the identical `cfg`. A blanket `use` here
// would name items that do not exist for the browser target, which is precisely the
// `unresolved import` this crate spent the port learning to avoid.
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
use runners::{run_connect, run_headless};
#[allow(unused_imports)]
use weather::{ContinuousTimeOfDay, ShellWeatherProbe, WeatherTracker, weather_columns_for_frame};

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
        #[cfg(not(target_arch = "wasm32"))]
        Mode::Headless => run_headless(config),
        #[cfg(not(target_arch = "wasm32"))]
        Mode::Connect => run_connect(config),
        #[cfg(target_arch = "wasm32")]
        Mode::Headless | Mode::Connect => Err(anyhow::anyhow!(
            "{:?} is a native CLI diagnostic mode: it needs a filesystem to write a \
             PPM to, or a raw TCP socket and a blocking sleep. A browser session is \
             always Mode::Window.",
            config.mode
        )),
        Mode::Window => run_windowed(config),
    }
}

/// Sky distance fog sized to the shell's real render distance, so terrain
/// dissolves into the sky exactly where chunks stop loading rather than at the
/// render crate's default 8-chunk fallback. Driven once at render bring-up on
/// both the windowed and headless paths (render distance is fixed for the
/// session).
///
/// Delegates to [`crate::sim::fog_for_render_distance`] so the colour and the
/// fade band have one definition shared with the frame clear — a second copy of
/// the sky colour here is how the horizon ends up banding in a colour the sky
/// never is.
fn sky_fog(render_distance: u32) -> FogSettings {
    crate::sim::fog_for_render_distance(render_distance)
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
        .filter(|a| binds.is(*a, code))
        .find_map(InputAction::movement)
}

/// Wire button number for an off-hand `SWAP` click — vanilla's literal `40` in
/// `AbstractContainerScreen.checkHotbarKeyPressed` and the `buttonNum == 40`
/// guard in `AbstractContainerMenu.doClick`'s `SWAP` arm. Not a slot index: the
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
        .filter(|a| binds.is(*a, code))
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
        .find(|a| binds.is_mouse(*a, button))
}

/// Whether the world's own HUD — hotbar, hearts, hunger, the XP bar — draws on
/// this screen.
///
/// **It belongs to the world, not to active play.** Vanilla extracts the entire
/// HUD whenever a level is loaded and lets whatever screen is open paint its own
/// translucent background *over* it; that background is the dim, and it is the
/// only reason an open inventory looks different from playing:
///
/// - `GameRenderer.extract` computes
///   `readyForLevelRendering = resourcesLoaded && advanceGameTime && level != null`
///   and passes it straight into the GUI (`GameRenderer.java:377,389`) — note it
///   asks about the *level*, never about `screen`.
/// - `Gui.extractRenderState` calls `hud.extractRenderState` under that flag
///   alone (`Gui.java:152-156`), then draws the open screen **afterwards**
///   (`Gui.java:171-189`), i.e. on top.
/// - `Hud.extractRenderState` itself gates only on F1 (`isHidden`) and
///   `LevelLoadingScreen` (`Hud.java:218-221`). Inside it, the hotbar, hearts,
///   hunger, the XP bar and the held-item name are gated on **game mode** only
///   (`Hud.java:534-562`) — nothing there consults `screen()`.
///
/// Exactly two HUD elements in vanilla do consult `screen()`, and neither is a
/// vital: the potion-effect icons (`Hud.java:486-488`, suppressed only when the
/// screen `showsActiveEffects()`, which is overridden `true` by `InventoryScreen`
/// and `CreativeModeInventoryScreen` because those draw their own) and the
/// subtitle overlay (`Hud.java:238-241`). The crosshair is **not** one of them
/// (`Hud.java:439-470` gates on camera type and spectator mode only) — we still
/// hide it with [`crate::menu::UiState::is_playing`], a deliberate divergence
/// while container screens have no dimmed background pass to hide behind
/// (issue #51).
///
/// [`Screen::Connecting`] is excluded because there is no world yet — since
/// issue #449 it is an `owns_frame` screen, so `draw_menu` returns early and
/// this function is never even asked about it, but the negative control in
/// `app/tests.rs` asserts the answer anyway so the set cannot silently grow.
/// The menu and error screens never get here at all: `draw_menu` returns early.
fn hud_follows_world(screen: crate::menu::Screen) -> bool {
    use crate::menu::Screen;
    // `Screen::Death` (issue #103) follows the same rule as `Paused`: vanilla's
    // `Hud.extractRenderState` gates only on F1/`LevelLoadingScreen`, never on
    // which screen is open, so the hotbar/hearts/hunger keep drawing (dimmed by
    // the death screen's own background pass) behind the death screen too.
    matches!(
        screen,
        Screen::Playing | Screen::Chat | Screen::Container | Screen::Paused | Screen::Death
    )
}

/// `MouseHandler.onScroll`'s scroll-delta transform (`MouseHandler.java:189-192`),
/// which is the boundary **both** wheel consumers read (issues #203, #444):
///
/// ```java
/// boolean discreteScroll = this.minecraft.options.discreteMouseScroll().get();
/// double scrollSensitivity = this.minecraft.options.mouseWheelSensitivity().get();
/// double scaledYOffset = (discreteScroll ? Math.signum(yoffset) : yoffset) * scrollSensitivity;
/// ```
///
/// ## Why it is one function, applied in two arms
///
/// Vanilla computes `scaledYOffset` **once** and hands the same value to
/// `screen().mouseScrolled(..)` and to `ScrollWheelHandler.onMouseScroll`. So neither
/// `discreteMouseScroll` nor `mouseWheelSensitivity` is a hotbar option or a
/// list option — they define what a wheel notch *is*, before anything decides what to
/// do with it. Putting the transform inside either consumer would have made the other
/// one silently ignore both options, which is how `discreteMouseScroll` came to be
/// #444's only row with a consumer this shell could actually reach.
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
        // `Math.signum`, which is 0.0 for 0.0 — not `1.0`. `f64::signum` returns
        // 1.0 for +0.0, so a zero delta must be handled explicitly or a stationary
        // wheel would scroll one notch per event.
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
/// vanilla's literal `600L` in
/// `OptionInstance.OptionInstanceSliderButton.applyValue`
/// (`OptionInstance.java:408`): `this.delayedApplyAt = Util.getMillis() + 600L`.
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
/// This is what vanilla's `yoffset` already has applied by the time
/// `MouseHandler.onScroll` sees it, so it belongs at *our* boundary too rather
/// than anywhere downstream.
const PRECISE_SCROLL_SCALE: f64 = 0.1;

/// One winit scroll event as the **notch count** vanilla's `yoffset` carries.
///
/// The two delta kinds are different units and were being read as the same one:
/// `LineDelta` is already notches, but `PixelDelta` is raw points, and a trackpad
/// event carrying `p.y == 12.0` was arriving as *twelve* notches. Downstream that
/// is `12 * scrollRate()` — 144 px of a settings list in one event, which is the
/// owner's "scrolling is a fixed jump" report, and twelve hotbar slots for the
/// same flick.
///
/// [`PRECISE_SCROLL_SCALE`] is the conversion, taken from GLFW rather than tuned:
/// it is by definition the number vanilla receives for the same physical gesture.
///
/// Free function for [`scale_scroll`]'s reason — and applied *before* it, because
/// `discreteMouseScroll`'s `signum` must see a notch count. Applied after, a
/// trackpad event would be `signum(12.0) == 1.0` either way and the option would
/// mask the bug rather than the bug showing.
fn wheel_notches(delta: winit::event::MouseScrollDelta) -> f64 {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y),
        winit::event::MouseScrollDelta::PixelDelta(p) => p.y * PRECISE_SCROLL_SCALE,
    }
}

/// Vanilla's `ScrollWheelHandler.onMouseScroll` (issue #203): folds a
/// sensitivity-scaled, possibly-fractional scroll offset into a whole number
/// of hotbar slots, carrying the remainder in `accum` across calls so a
/// `mouseWheelSensitivity` below 1.0 does not silently drop sub-notch scroll.
///
/// `accum` resets to zero on a direction reversal
/// (`Math.signum(scaledYOffset) != Math.signum(this.accumulatedScrollY)`,
/// `ScrollWheelHandler.java:14-16`) rather than fighting the new direction with
/// old carry — one hard flick back should not need to "pay off" the previous
/// direction's fractional debt first.
fn accumulate_scroll(accum: &mut f64, scaled: f64) -> i32 {
    if *accum != 0.0 && scaled.signum() != accum.signum() {
        *accum = 0.0;
    }
    *accum += scaled;
    let whole = accum.trunc();
    *accum -= whole;
    whole as i32
}

/// What one raw physical key means while a Controls-menu bind button is
/// mid-capture (issue #15's last hop) — extracted as a pure function so the
/// decision is unit-testable without a window, the same reason
/// [`resolve_key`] and [`WindowApp::menu_key_for`] are split out. Deliberately
/// **not** `menu_key_for`: that function drops any physical key with no
/// printable `text` (F-keys, modifiers, arrows other than Up/Down), which is
/// exactly the common rebind target a Controls menu exists to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureKey {
    /// Escape: cancel the capture without changing the binding. Vanilla sets
    /// `InputConstants.UNKNOWN` on Escape unconditionally
    /// (`KeyBindsScreen.java:73-74`); this client deliberately does not — see
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
    sim: Sim,
    window: Option<Arc<Window>>,
    gpu: Option<GpuContext>,
    target: Option<lodestone_render::SurfaceTarget<'static>>,
    render: Option<RenderState>,
    hud: Option<HudRenderer>,
    /// Self-contained overlay for active status effects (owns its own pipeline
    /// so it composites over the HUD without touching the HUD renderer).
    effects: Option<EffectsRenderer>,
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
    /// Whether the debug modifier (F3) is currently held — issue #197's chords.
    /// Fed into [`app::input::KeyGate::debug_held`]; see there for why this is a
    /// gate flag rather than a second bindable action.
    debug_held: bool,
    /// Whether a chord (F3+B/F3+G) fired while the modifier was held. Vanilla's
    /// `didDebugAction`: on release, the overlay toggles only if this is `false`,
    /// so F3+B does not also open the overlay.
    debug_chord_used: bool,
    /// F3+B — draw entity hitboxes. `Arc<AtomicBool>` because the only consumer
    /// is the `Send + Sync + 'static` closure `install_debug_lines_source`
    /// installs, which cannot borrow `self`.
    debug_hitboxes: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// F3+G — draw the borders of the chunk the player is in. Same `Arc` reason
    /// as [`Self::debug_hitboxes`].
    debug_chunk_borders: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The menu row whose slider the mouse is currently dragging, if any —
    /// vanilla's `AbstractSliderButton.dragging`.
    ///
    /// Holds the **row**, not just a flag, because a drag continues while the
    /// cursor leaves the row: once it has begun, the row is fixed and only the
    /// x matters. Cleared on mouse-up.
    menu_slider_drag: Option<usize>,
    /// The Render Distance value last seen on [`Self::nav`], for edge detection.
    ///
    /// Seeded from the launch config, so the first frame arms nothing.
    render_distance_seen: u32,
    /// When the pending Render Distance change becomes live — vanilla's
    /// `OptionInstance.OptionInstanceSliderButton.delayedApplyAt`.
    ///
    /// # Why deferred rather than per-frame
    ///
    /// `renderDistance` is the one `IntRange` vanilla builds with
    /// `applyValueImmediately == false` (`Options.java:1470-1477`), and
    /// `applyValue` is `this.delayedApplyAt = Util.getMillis() + 600L`
    /// (`OptionInstance.java:404-409`), committed from the render extract once
    /// the deadline passes (`:429-435`). Re-armed by *every* change, so the
    /// commit lands 600 ms after the drag stops, not 600 ms after it starts.
    ///
    /// Applying per frame instead would reload chunks on every pixel of the
    /// drag — which is exactly the cost the delay exists to avoid, and why the
    /// value being *stored* eagerly (it always was, see
    /// `config::Options::render_distance`) is not the same thing as it being
    /// *applied*.
    render_distance_apply_at: Option<Instant>,
    /// Whether the player-list binding is currently held (shows the overlay).
    tab_held: bool,
    /// A `key.screenshot` press waiting to be serviced (issue #16).
    ///
    /// **The capture cannot happen at key time**, which is the whole reason
    /// this field exists rather than the effects arm doing the work: a
    /// swapchain image has no defined content until a pass has rendered into
    /// it, so reading the texture when the key arrives copies out either the
    /// *previous* frame or undefined memory. `redraw()` drains this flag
    /// immediately before `AcquiredFrame::present`, once world, HUD and every
    /// menu overlay have already written into `frame.view()`.
    pending_screenshot: bool,
    /// The rebindable action → input table (`docs/keybindings.md`), loaded from
    /// the persisted [`crate::config::Options`] at construction.
    ///
    /// Held here rather than reached for through [`MenuNav`] because this is the
    /// *consumer*: every input event reads it, and `Keybinds` is `Copy` so the
    /// read is a field access rather than a borrow that would fight the
    /// `&mut self` effect calls in `window_event`.
    ///
    /// **A Controls menu will need a writer**, and this field is the reason that
    /// is a small addition rather than a rewrite: `MenuNav` already owns the
    /// loaded `Options` and the path to persist them to, so the menu's rebind
    /// call belongs there (a `nav.rebind(action, binding)` that sets the field
    /// and calls the existing `persist_options`), and this field then becomes
    /// `*self.nav.keybinds()`, re-read once per frame or on change. Deliberately
    /// not done yet: `nav.rs` is a shared file and an accessor with no caller is
    /// the island pattern `CLAUDE.md` §1 warns about. See `docs/keybindings.md`
    /// for the exact patch.
    keybinds: Keybinds,
    /// Editable buffer for the chat prompt; only consumed while chat is open.
    chat_input: ChatInput,
    /// Wrapped chat rows, persisted across frames — see
    /// [`crate::hud::ChatWrapCache`]. Lives here rather than in `hud` because
    /// the HUD `Frame` is rebuilt from scratch every frame and so can hold no
    /// state of its own; issue #527 (a).
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
    /// distinguish `key.drop`'s drop-one from drop-stack
    /// (`Screen.hasControlDown()`/`Minecraft.hasControlDown()`), which is a
    /// modifier read at drop time, not a `KeyMapping` of its own.
    ctrl_held: bool,
    /// Fractional carry for the hotbar mouse-wheel scroll (issue #203), so a
    /// `mouseWheelSensitivity` below 1.0 does not lose sub-notch scroll and
    /// above 1.0 can cross more than one slot per notch. Mirrors vanilla's
    /// `ScrollWheelHandler.accumulatedScrollY`: each event adds
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
    fps_ema: f32,
    last_log: Instant,
    /// The fog settings last uploaded to the renderer, so submerged fog is
    /// re-uploaded only when it actually changes (the player crossing a
    /// water/lava surface) rather than every frame. Seeded to the sky fog set at
    /// render bring-up so the first frame above water is a no-op.
    applied_fog: Option<FogSettings>,
    /// This driver's own `bevy_ecs` `App` (`docs/bevy-migration.md` Stage 0),
    /// stepped once per frame in [`WindowApp::redraw`] via `Runner::Winit`
    /// (`app.update()` called directly, no internal timer — packet ingest
    /// must never gate on frame rate). Only `CorePlugin` is added, and nothing
    /// in the shell reads from it — `self.ecs.update()` is the only other
    /// reference to this field. It is a *separate* `World` from `Sim`'s own
    /// `EcsHandle`, which §4.1(c) made the one that `lodestone_client::SharedState`
    /// adopts; this field predates that unification and was not folded into
    /// it. See the two-`World`s note in `docs/bevy-migration.md`'s Stage 0
    /// report for how this scaffold started, and §4.1(c)'s report for which
    /// `World` actually won.
    ecs: lodestone_ecs::app::App,
    /// The local crafting-recipe corpus (`crate::resources::load_recipe_book`),
    /// loaded once at GPU bring-up. `None` on a jar-less run or before it has
    /// loaded. Used only for the container screen's ghost-preview draw and the
    /// debug-overlay counter — the crafting result slot itself is always the
    /// server's, never a local match (see `docs/crafting.md`).
    ///
    /// Since issue #148 this is a **cache** of `lodestone_ecs::RecipeRegistry`'s
    /// book, not the authority: plugins register recipes into that resource, and
    /// `Self::sync_recipe_book` re-clones this field when
    /// [`recipe_book_revision`](Self::recipe_book_revision) falls behind. Reads
    /// stay on a plain field rather than a guard because the four read sites are
    /// all inside the render pass, and `lodestone_ecs::EcsHandle`'s discipline
    /// forbids holding a guard across one.
    recipe_book: Option<RecipeBook>,
    /// The `RecipeRegistry::revision` [`recipe_book`](Self::recipe_book) was
    /// cloned at, so a plugin registering mid-session refreshes the cache exactly
    /// once rather than on every frame. Issue #148.
    recipe_book_revision: u64,
    /// Persisted recipe-book **panel** state (issue #163): whether the panel is
    /// open, and the search/tab/page the user last left it on.
    ///
    /// Persisted across frames *and* across container open/close, deliberately:
    /// vanilla's `RecipeBookComponent` state lives on the client's own
    /// `RecipeBook`, not on the screen, so reopening a crafting table keeps the
    /// book open with the same tab. Rebuilding it per frame would reset the
    /// search box on every mouse move.
    recipe_panel: RecipePanelState,
    /// The recipe-unlock toast queue (issue #163) —
    /// [`lodestone_game::recipe::RecipeToastQueue`], drained into
    /// [`crate::hud::HudFrame::recipe_toast`] each frame.
    ///
    /// **Has no live producer yet.** The only thing that can fill it is the
    /// `recipe_book_add` decode, which does not exist in
    /// `crates/protocol/v770` (tracked on #436), so on a real server this stays
    /// empty and no toast draws. That is the honest degradation, not a bug: the
    /// render path is wired and gated so the toast appears the moment decode
    /// lands. Deliberately **no** fake producer was added to light it up early.
    recipe_toasts: lodestone_game::recipe::RecipeToastQueue,
    /// The world's weather, resolved from the net thread's cell once per frame.
    /// `None` before a session exists; installed alongside the other render
    /// sources by [`WindowApp::install_session_render_sources`] and cleared with
    /// the session, so a fresh connect never inherits the last one's storm.
    /// The rain **ambience** is deliberately absent from this struct. Its cadence
    /// lives in [`lodestone_render::RainAmbience`] (vanilla's `rainSoundTime`,
    /// unit-tested), but it has **no producer**, because the only `ShellAudio` in
    /// the process is a private field on `Sim` with no public play method. Adding
    /// one `pub fn play_local_sound(&mut self, name: &str, category, pos, volume,
    /// pitch)` to `crate::sim::Sim` — forwarding to `self.audio` exactly as the
    /// `NetUpdate::Sound` arm at `sim.rs:4722` already does — is the whole
    /// remaining wiring. Recorded here rather than left as two dead fields, per
    /// `CLAUDE.md`'s island rule: an unused field reads as an oversight, a named
    /// blocker does not.
    weather: Option<Arc<WeatherTracker>>,
    /// The creative-inventory screen's own UI state (issue #158): selected tab,
    /// scroll offset, search text.
    ///
    /// Persisted across open/close for the same reason
    /// [`recipe_panel`](Self::recipe_panel) is — vanilla's `selectedTab` is a
    /// `static` field on `CreativeModeInventoryScreen`, so reopening the screen
    /// returns to the tab you left it on.
    ///
    /// Whether the screen is *showing* is not stored here: it is
    /// [`WindowApp::creative_screen_open`], derived from the container flag plus
    /// the player's own abilities, so the two can never disagree.
    creative: crate::container::CreativeState,
    /// The Advancements screen's in-flight viewport drag (issue #167): the cursor
    /// position the last pan was measured from, in physical pixels.
    ///
    /// The *screen* state (tab, per-tab scroll) lives on [`MenuNav`] beside every
    /// other menu screen's; only the drag is here, because it is a property of the
    /// mouse rather than of the screen — the same split
    /// [`menu_slider_drag`](Self::menu_slider_drag) already makes.
    advancements_drag: Option<(f32, f32)>,
    /// The live advancement progress snapshot plus the completion-toast queue
    /// (issue #167) — see [`AdvancementsFeed`](advancements_screen::AdvancementsFeed).
    advancement_feed: advancements_screen::AdvancementsFeed,
    /// The world this process is *hosting*, if any (issue #535).
    ///
    /// Set by [`WindowApp::begin_singleplayer`] and cleared on quit-to-title, and
    /// read by exactly one thing: the pause menu's Open to LAN, which republishes
    /// the same launch on a TCP port. `None` for a multiplayer session, which is
    /// what makes that button honest there — there is no world of ours to publish.
    hosted_world: Option<crate::menu::nav::SingleplayerLaunch>,
    /// The merchant screen's own UI state (issue #245's UI half) — vanilla's
    /// `MerchantScreen.shopItem`, which trade row is selected: whose
    /// out-of-stock overlay shows, and the index the next `SELECT_TRADE` send
    /// carries.
    ///
    /// Not reset on close, matching [`creative`](Self::creative)/
    /// [`recipe_panel`](Self::recipe_panel)'s own "persisted across open/close"
    /// precedent — vanilla's own field is `screen`-scoped state that a fresh
    /// `MerchantScreen` starts at `0` every time, but this client rebuilds no
    /// screen object per open, so restarting at `0` here would need an extra
    /// reset call at the one place a merchant menu opens, for a difference no
    /// player can see (`0` is what a stale value most often already is). A
    /// stale value past the real offer list is harmless either way: every
    /// reader (`container::geometry`'s `draw_merchant_trades`,
    /// `container::merchant::hit_test_local`) indexes with `.get()`/a bounded
    /// loop, never a bare index.
    merchant_selected: usize,
}

#[cfg(test)]
mod tests;

/// Gates for the recipe-book panel wiring (issue #163).
///
/// The recipe-book UI landed fully built and unit-tested in `container.rs` and
/// `lodestone-game`, and reached **zero pixels** because the three call sites
/// that drive it live in `app.rs`/`sim.rs`/`hud.rs`. These gates measure the
/// wiring itself, not the geometry — `container.rs`'s own 75 tests already prove
/// the geometry is right, and every one of them passed while nothing drew.
#[cfg(test)]
mod recipe_book_wiring;
