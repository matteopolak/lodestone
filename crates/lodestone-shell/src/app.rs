//! The windowed driver and the headless / connect runners.
//!
//! This is deliberately the *thin* layer: all simulation lives in [`crate::sim`]
//! and all GPU state in [`crate::gpu`] / [`crate::hud`]. Here we only translate
//! winit lifecycle + input events into calls on those, and own the per-frame
//! acquire → render → present dance — including treating surface loss/outdated
//! as routine (on macOS they are), reconfiguring and moving on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lodestone_render::{
    GpuContext, HeadlessTarget, RenderTarget, TargetError, fog::FogSettings, window::attach_window,
};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::chat::ChatInput;
use crate::config::{Config, Mode};
use crate::container::{
    ContainerFrame, ContainerRenderer, MenuButton, MenuContext, MenuInput, hit_test_with_scale,
};
use crate::effects::EffectsRenderer;
use crate::gpu::RenderState;
use crate::hud::{HotbarSlot, HudFrame, HudRenderer};
use crate::menu::nav::{MenuAction, MenuKey, MenuNav};
use crate::menu::render::MenuRenderer;
use crate::menu::status::StatusCache;
use crate::menu::{SessionKind, UiState};
use crate::net::NetClient;
use crate::sim::Sim;
use lodestone_assets::ResourceLocation;
use lodestone_controller::Action;
use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::menu::Menu;
use lodestone_game::recipe::RecipeBook;

/// Entry point: dispatch on the configured mode.
///
/// # Errors
/// Returns an error if GPU bring-up or the event loop fails.
pub fn run(config: Config) -> anyhow::Result<()> {
    match config.mode {
        Mode::Headless => run_headless(config),
        Mode::Connect => run_connect(config),
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

/// Map a physical key to a movement [`Action`].
fn action_for(code: KeyCode) -> Option<Action> {
    Some(match code {
        KeyCode::KeyW => Action::Forward,
        KeyCode::KeyS => Action::Back,
        KeyCode::KeyA => Action::Left,
        KeyCode::KeyD => Action::Right,
        KeyCode::Space => Action::Jump,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Action::Sneak,
        KeyCode::ControlLeft | KeyCode::ControlRight => Action::Sprint,
        _ => return None,
    })
}

/// Maps a winit mouse button to the container-click gesture it drives.
/// `None` for anything but left/right/middle (e.g. the back/forward mouse
/// buttons some mice send), which the container screen has no use for.
fn menu_button_for(button: MouseButton) -> Option<MenuButton> {
    Some(match button {
        MouseButton::Left => MenuButton::Left,
        MouseButton::Right => MenuButton::Right,
        MouseButton::Middle => MenuButton::Pick,
        _ => return None,
    })
}

/// Maps the number-row keys `1`..`9` to a hotbar slot index `0..8`. Returns
/// `None` for any other key.
fn hotbar_slot_for(code: KeyCode) -> Option<usize> {
    Some(match code {
        KeyCode::Digit1 => 0,
        KeyCode::Digit2 => 1,
        KeyCode::Digit3 => 2,
        KeyCode::Digit4 => 3,
        KeyCode::Digit5 => 4,
        KeyCode::Digit6 => 5,
        KeyCode::Digit7 => 6,
        KeyCode::Digit8 => 7,
        KeyCode::Digit9 => 8,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Frame pacing
// ---------------------------------------------------------------------------

/// Vanilla's cap on how many 20 Hz client ticks a single update may run.
///
/// Read from the decompiled 26.2 client, not guessed:
/// `.cache/mc/26.2/client-src/net/minecraft/client/Minecraft.java:262` declares
/// `private static final int MAX_TICKS_PER_UPDATE = 10;` and `:1176` applies it
/// as `for (int i = 0; i < Math.min(10, ticksToDo); i++)`. Note *where* the cap
/// lives: `DeltaTracker.Timer::advanceGameTime` returns the full uncapped tick
/// count and keeps the sub-tick residual, and `runTick` then simply **runs at
/// most ten of them and drops the rest**. Missed real time is discarded, never
/// replayed — which is the whole point.
pub(crate) const MAX_TICKS_PER_UPDATE: u32 = 10;

/// Length of one client tick in seconds (20 Hz), matching `sim`'s `TICK_DT`.
pub(crate) const TICK_SECS: f64 = 1.0 / 20.0;

/// The most real time one update may hand the simulation, in seconds.
///
/// `10 × 0.05 = 0.5`. Anything beyond this is dropped rather than replayed, so
/// alt-tabbing away for a minute costs ten ticks of catch-up, not 1200.
pub(crate) const MAX_CATCHUP_SECS: f64 = MAX_TICKS_PER_UPDATE as f64 * TICK_SECS;

/// Presentation rate while the window is visible but **unfocused**. The
/// simulation keeps running at the full 20 Hz either way; only presentation is
/// throttled.
pub(crate) const UNFOCUSED_FPS: u32 = 30;

/// [`UNFOCUSED_FPS`] as the interval between presented frames.
pub(crate) const UNFOCUSED_FRAME_INTERVAL: Duration =
    Duration::from_nanos(1_000_000_000 / UNFOCUSED_FPS as u64);

/// How long the event loop sleeps between iterations while unfocused. Kept
/// comfortably shorter than [`TICK_SECS`] so the tick loop is never the thing
/// being paced — if this ever exceeded 50 ms the sim would fall behind the
/// server even though we are "still ticking".
pub(crate) const BACKGROUND_POLL: Duration = Duration::from_millis(8);

/// Maximum gap between two left-clicks on the same container slot for the
/// second to count as a double-click gather. Winit hands us raw button
/// up/down events with no click-count of its own, so this app tracks it —
/// [`container::MenuInput::press`] still requires the *same slot* on top of
/// this timing before it arms the gather.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// What one iteration of the event loop should do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrameStep {
    /// Real seconds to advance the simulation by, already clamped to
    /// [`MAX_CATCHUP_SECS`].
    pub dt: f64,
    /// Whether to acquire a swapchain image and draw. When `false` the sim still
    /// steps — we skip *presenting*, never ticking.
    pub render: bool,
}

/// Owns the frame clock and decides, per iteration, how far to advance the sim
/// and whether to draw.
///
/// ## Why this lives here and not in `sim`
///
/// `Sim::step` already clamps its own accumulator, but the *policy* — how much
/// catch-up is acceptable, and whether an unfocused window should present —
/// belongs to the driver, alongside the winit focus/occlusion events that inform
/// it. Keeping the clock here also means the sim is advanced by an explicit,
/// injectable `dt`, so this is testable against a real `Sim` with a synthetic
/// clock and no window.
///
/// ## The bug this exists to fix
///
/// Presentation used to gate simulation: `redraw` stepped the sim and then
/// acquired a swapchain image in the same call, with the GPU-readiness guard
/// *before* the step. A backgrounded or occluded window makes `acquire()` slow
/// (macOS stops vending drawables to an occluded `CAMetalLayer` and the call
/// stalls until it times out), so the loop's iteration rate collapsed — and with
/// it the tick rate, since ticks only advanced when a frame did. Skipping
/// presentation instead of skipping the tick is what keeps keep-alives and
/// movement packets flowing while tabbed out; a client the server considers
/// stalled stops receiving chunks entirely.
///
/// ## Why the unfocused frame schedule is absolute, not "elapsed since the last
/// frame"
///
/// This was measured, not reasoned about. The obvious gate —
/// `now - last_render >= interval`, then `last_render = now` — **loses frames**,
/// because it can only fire on a loop iteration and each iteration pushes the
/// next deadline out by however far it overshot. At a 120 Hz loop with a 30 fps
/// target there are only four chances per interval, and the accumulated
/// overshoot cost 4 of every 30 frames: a one-second unfocused run presented
/// **26** frames, not 30. A 30 fps limiter that silently delivers 26 is the
/// quiet kind of wrong.
///
/// So the deadline is absolute: `next_render` advances by exactly one interval
/// from *itself*, never from `now`, and phase error cannot accumulate. The one
/// exception is a stall longer than an interval, where the schedule is re-based
/// onto `now` — otherwise coming back from a two-minute alt-tab would present a
/// burst of catch-up frames, which is the same mistake as replaying catch-up
/// ticks.
#[derive(Debug)]
pub(crate) struct FramePacer {
    last_step: Instant,
    /// The absolute time the next unfocused frame is due. Advanced by whole
    /// intervals so the presented rate does not drift below the target.
    next_render: Instant,
    focused: bool,
    occluded: bool,
}

impl FramePacer {
    /// A pacer whose clock starts at `now`, focused and visible.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_step: now,
            next_render: now + UNFOCUSED_FRAME_INTERVAL,
            focused: true,
            occluded: false,
        }
    }

    /// Record a focus change. Does **not** touch the step clock: the elapsed
    /// time since the last step is real time the sim still owes, and it is
    /// clamped on the next `begin_frame` like any other stall.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Record an occlusion change (window fully covered / minimised).
    pub(crate) fn set_occluded(&mut self, occluded: bool) {
        self.occluded = occluded;
    }

    /// Whether the window currently has focus. Test-only: the app never asks,
    /// because focus must not gate anything except presentation — which
    /// [`Self::begin_frame`] already decides.
    #[cfg(test)]
    pub(crate) fn focused(&self) -> bool {
        self.focused
    }

    /// Advance the frame clock to `now` and decide what this iteration does.
    ///
    /// The returned `dt` is the real elapsed time **clamped** to
    /// [`MAX_CATCHUP_SECS`]; the excess is dropped, exactly as vanilla drops
    /// ticks past `MAX_TICKS_PER_UPDATE`.
    pub(crate) fn begin_frame(&mut self, now: Instant) -> FrameStep {
        let dt = now.saturating_duration_since(self.last_step).as_secs_f64();
        self.last_step = now;

        let render = if self.occluded {
            // Nothing is on screen to update, and acquiring a drawable is what
            // stalls. Drop presentation entirely and keep ticking.
            false
        } else if self.focused {
            // Vsync (or the compositor) paces us; do not second-guess it.
            self.next_render = now + UNFOCUSED_FRAME_INTERVAL;
            true
        } else {
            now >= self.next_render
        };
        if render && !self.focused {
            // Advance the deadline from itself, not from `now`, so overshoot
            // does not accumulate into a lower delivered frame rate. Re-base
            // only when we are more than a whole interval late, which means a
            // real stall rather than ordinary jitter — replaying the backlog as
            // a burst of frames would be the presentation-side version of the
            // catch-up-tick bug.
            self.next_render += UNFOCUSED_FRAME_INTERVAL;
            if self.next_render <= now {
                self.next_render = now + UNFOCUSED_FRAME_INTERVAL;
            }
        }

        FrameStep {
            dt: dt.min(MAX_CATCHUP_SECS),
            render,
        }
    }

    /// How the event loop should wait after this iteration: spin while focused
    /// (vsync paces us), otherwise sleep briefly so a backgrounded window stops
    /// burning a core while still ticking well above 20 Hz.
    pub(crate) fn control_flow(&self, now: Instant) -> ControlFlow {
        if self.focused && !self.occluded {
            ControlFlow::Poll
        } else {
            ControlFlow::WaitUntil(now + BACKGROUND_POLL)
        }
    }
}

// ---------------------------------------------------------------------------
// Windowed
// ---------------------------------------------------------------------------

/// Why an integrated-server (Singleplayer) launch could not proceed.
///
/// Typed so the Error screen can eventually distinguish "the server failed to
/// start" from "the client failed to connect" — see the ergonomics note on
/// `IntegratedServer` in the session report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchError {
    /// The integrated server's transport/lifecycle exists
    /// (`lodestone_server::IntegratedServer::open_in_memory`), but no *versioned*
    /// `ServerProtocol` does — only test stand-ins. So there is nothing for the
    /// registry's real adapter to speak to in-process yet. Staged, not a runtime
    /// failure. Verified 2026-07-28; reported upstream (impl-worldgen / impl-v770).
    NoServerProtocol,
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::NoServerProtocol => write!(
                f,
                "Singleplayer is not available yet: the integrated server's \
                 transport is ready, but no versioned server protocol exists for \
                 the client to speak to in-process. Use Multiplayer for now."
            ),
        }
    }
}

/// Staged singleplayer launcher. Returns [`LaunchError::NoServerProtocol`] until
/// a versioned `ServerProtocol` exists for `lodestone-server` to serve the
/// registry adapter in-process. The `IntegratedServer::open_in_memory(protocol,
/// source, view_radius)` transport/lifecycle and the `Transport` seam
/// (`ClientBuilder::connect_with`) are both already there — the missing piece is
/// the server-side encoder/decoder, which lives outside this crate. Deliberately
/// does **not** fork a stand-in adapter into the shell (that would smuggle
/// version knowledge past the registry) or call a real server (a non-compiling
/// `lodestone-shell` breaks every agent's cargo); the gap is recorded here and in
/// the report, not as a build break.
pub(crate) fn launch_singleplayer() -> Result<NetClient, LaunchError> {
    Err(LaunchError::NoServerProtocol)
}

/// Whether argv asked for a connection, i.e. whether to bypass the main menu.
///
/// True for `--live`, or for any `--host`/`--port` at all.
///
/// This used to compare against `Config::default()`, which made
/// `--host 127.0.0.1 --port 25565` — spelling out the defaults — indistinguishable
/// from passing nothing, so it silently landed on the main menu. That is the
/// launch the two-worlds report came from: the user asked for a server on the
/// command line and got the title screen. [`Config::address_given`] now records
/// whether the flag was *seen*, which is the question actually being asked.
fn requested_a_connection(config: &Config) -> bool {
    config.connect_in_window || config.address_given
}

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
    /// F3 debug overlay visibility (starts on — it's the instrument, §S4).
    show_debug: bool,
    /// Whether Tab is currently held (shows the player-list overlay).
    tab_held: bool,
    /// Editable buffer for the chat prompt; only consumed while chat is open.
    chat_input: ChatInput,
    /// Press/drag/release state machine for the open container screen; see
    /// [`container::MenuInput`]. Drives every predicted click this app sends.
    menu_input: MenuInput,
    /// Whether either Shift key is currently held, tracked independently of
    /// `sim.input` (which only feeds movement `Action`s and is not read back).
    /// Container shift-clicks (`QuickMove`) need this even while the sim's own
    /// gameplay input is not being accepted.
    shift_held: bool,
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
    /// must never gate on frame rate). A genuinely empty scaffold today:
    /// `CorePlugin` installs only the schedules/sets, no game state. This is
    /// a *separate* `World` from `lodestone_client::SharedState`'s `EcsHandle`
    /// (which owns `WorldTime`) — unifying the two is a later stage; see the
    /// two-`World`s note in `docs/bevy-migration.md`'s Stage 0 report.
    ecs: lodestone_ecs::app::App,
    /// The local crafting-recipe corpus (`crate::resources::load_recipe_book`),
    /// loaded once at GPU bring-up. `None` on a jar-less run or before it has
    /// loaded. Used only for the container screen's ghost-preview draw and the
    /// debug-overlay counter — the crafting result slot itself is always the
    /// server's, never a local match (see `docs/crafting.md`).
    recipe_book: Option<RecipeBook>,
}

impl WindowApp {
    fn new(config: Config) -> Self {
        let sim = Sim::new(config.clone());
        // Matches the sky fog set at render bring-up, so the fog reconciliation's
        // first above-water frame is a no-op rather than a redundant upload.
        let applied_fog = Some(crate::sim::fog_for_render_distance(config.render_distance));
        let mut ecs = lodestone_ecs::app::App::new();
        ecs.add_plugins(lodestone_ecs::CorePlugin);
        Self {
            config,
            sim,
            window: None,
            gpu: None,
            target: None,
            render: None,
            hud: None,
            effects: None,
            container: None,
            grabbed: false,
            pacer: FramePacer::new(Instant::now()),
            ui: UiState::new(),
            nav: MenuNav::new(),
            statuses: StatusCache::new(),
            menu: None,
            favicons: crate::menu::render::FaviconCache::new(),
            cursor: (0.0, 0.0),
            show_debug: true,
            tab_held: false,
            chat_input: ChatInput::new(),
            menu_input: MenuInput::new(),
            shift_held: false,
            last_menu_click: None,
            fps_ema: 0.0,
            last_log: Instant::now(),
            applied_fog,
            ecs,
            recipe_book: None,
        }
    }

    fn set_grab(&mut self, grabbed: bool) {
        let Some(window) = &self.window else { return };
        if grabbed {
            let locked = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
            if locked.is_ok() {
                window.set_cursor_visible(false);
                self.grabbed = true;
            }
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
            self.grabbed = false;
            self.sim.input_mut().release_all();
            // Releasing the pointer also ends any held dig, so mining does not
            // continue while the player is in a menu or the window is unfocused.
            self.sim.end_attack();
        }
    }

    /// Reconcile the menu state machine with the session's real phase, then keep
    /// the cursor grab in sync with whatever screen we ended up on. Called each
    /// frame so the loading screen is never a lie: it clears the moment the
    /// server logs us in, and flips to Error the moment the session ends.
    fn drive_ui_from_session(&mut self) {
        use crate::sim::SessionPhase;
        match self.sim.session_phase() {
            // LocalOnly never drives the menu — the dev world is already Playing.
            SessionPhase::LocalOnly | SessionPhase::Connecting => {}
            SessionPhase::Connected => self.ui.session_ready(),
            SessionPhase::Ended(reason) => {
                // Only transition in once; re-setting every frame would keep
                // re-latching the same reason (harmless but wasteful).
                if self.ui.screen() != crate::menu::Screen::Error {
                    self.ui.session_failed(reason.clone());
                }
            }
        }
        // A transition may have changed grab intent (Connected → Playing grabs;
        // Ended → Error releases). Only touch the OS grab when it disagrees.
        let want = self.ui.wants_cursor_grab();
        if want != self.grabbed {
            self.set_grab(want);
        }
    }

    /// Staged Singleplayer entry point. Vanilla's singleplayer starts an
    /// integrated server in-process and connects to it over a local transport;
    /// that server (`impl-worldgen`'s `lodestone-server`, via a future
    /// `IntegratedServer::start`) is not wired yet. Rather than fork a second
    /// launch path or silently do nothing, this drives the honest failure path:
    /// the menu shows an Error explaining the feature is staged. Kept here so the
    /// wiring is a one-call swap once the seam lands.
    fn begin_singleplayer(&mut self) {
        self.ui.begin(crate::menu::SessionKind::Singleplayer);
        match launch_singleplayer() {
            Ok(net) => self.sim.attach_net(net),
            Err(e) => self.ui.session_failed(e.to_string()),
        }
    }

    /// Open a live connection to `host:port` and show the loading screen.
    ///
    /// Factored out of `resumed` because the menu's Join button needs the exact
    /// same sequence, including the entity light sampler — which must be
    /// installed at connect time, not after login (see the long note at the
    /// `resumed` call site for why).
    fn connect_to(&mut self, host: String, port: u16) {
        let net = NetClient::connect(host, port, self.config.protocol);
        if let Some(render) = self.render.as_mut() {
            let handle = net.shared_handle();
            // Terrain and mobs must read the same clock: `RenderState` folds this
            // factor into the fog lane both the model and entity passes sample.
            // Installing it for one and not the other makes mobs darker than the
            // blocks they stand on at midnight.
            let clock = net.shared_handle();
            render.set_sky_darken_source(move || {
                clock
                    .get()
                    .map(|h| lodestone_render::entity::sky_darken_for_time_of_day(h.world_time().1))
            });
            render.set_entity_light_source(move |feet| {
                crate::net::entity_light_at(
                    &handle,
                    feet.x.floor() as i32,
                    feet.y.floor() as i32,
                    feet.z.floor() as i32,
                )
            });
        }
        self.sim.attach_net(net);
    }

    /// The menu currently drawn as the container screen — the open non-player
    /// menu if the server has one open, else the player inventory while `E`
    /// has it up — or `None` when no container UI is showing.
    ///
    /// Mirrors the `container_menu` selection `redraw` makes for drawing, so
    /// hit-testing and drawing never disagree about which menu is on screen
    /// (see the layout module's own warning about that class of bug).
    fn active_container_menu(&self) -> Option<Menu> {
        if let Some(open) = self.sim.open_menu() {
            Some(open.menu)
        } else if self.ui.is_container_open() {
            Some(self.sim.player_menu())
        } else {
            None
        }
    }

    /// Predicts a container click against the live client state and submits
    /// it to the server.
    ///
    /// This goes straight to [`lodestone_client::ClientHandle::menu_click`]
    /// rather than through `Sim`/`NetClient`'s `send_action` queue, and
    /// deliberately so: the prediction has to run inside the read-model the
    /// live `Menus` session lives in (see the doc comment on
    /// `ClientHandle::menu_click`), and `NetClient::send_action` only ever
    /// forwards an *already-built* [`lodestone_model::ClientAction`] — it has
    /// no menu to predict a click against. `NetClient::shared_handle()` is
    /// the existing, already-public seam onto that same live handle (used
    /// today for the sky-darken and entity-light samplers), so this needs no
    /// change to `net.rs` or `sim.rs`.
    ///
    /// Silently drops the click if there is no live connection yet (matches
    /// every other best-effort send in this app, e.g. `NetClient::send_action`
    /// itself).
    fn send_menu_click(&self, click: Click) {
        let Some(net) = self.sim.net() else { return };
        // Named separately from its `.get()` below rather than chained: the
        // `Arc<OnceLock<_>>` `shared_handle()` returns is an owned value, and
        // `.get()` borrows from it — keeping it in a binding of its own avoids
        // relying on let-else's temporary-scope-extension rules to keep that
        // borrow valid.
        let shared = net.shared_handle();
        let Some(handle) = shared.get() else { return };
        // `Sim` has no game-mode accessor to source a real `PlayerCtx` from
        // (see the report on this change) — hardcoded survival, matching the
        // only existing production-shaped precedent
        // (`container.rs`'s own click-driving tests use `PlayerCtx::survival()`
        // /`::creative()` explicitly rather than reading one off anything).
        let _ = handle.menu_click(click, PlayerCtx::survival());
    }

    /// Translate one winit key event into a [`MenuKey`], or `None` if the menu
    /// has no use for it.
    fn menu_key_for(event: &winit::event::KeyEvent) -> Option<MenuKey> {
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::ArrowUp => return Some(MenuKey::Up),
                KeyCode::ArrowDown => return Some(MenuKey::Down),
                KeyCode::Enter | KeyCode::NumpadEnter => return Some(MenuKey::Enter),
                KeyCode::Escape => return Some(MenuKey::Escape),
                KeyCode::Tab => return Some(MenuKey::Tab),
                KeyCode::Backspace => return Some(MenuKey::Backspace),
                KeyCode::Delete => return Some(MenuKey::Delete),
                _ => {}
            }
        }
        // Anything else is text. `KeyEvent::text` is already the composed
        // character, so this is the path that makes non-US layouts type
        // correctly into the address field.
        event
            .text
            .as_ref()
            .and_then(|t| t.chars().next())
            .filter(|c| !c.is_control())
            .map(MenuKey::Char)
    }

    /// Feed one menu key through the navigator and act on what it asks for.
    fn handle_menu_key(&mut self, key: MenuKey) {
        let action = self.nav.key(&mut self.ui, key);
        self.apply_menu_action(action);
    }

    /// Perform the one side effect a [`MenuAction`] names. Exhaustive on purpose:
    /// a new variant must fail to compile here rather than silently do nothing.
    fn apply_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::None => {}
            MenuAction::Singleplayer => {
                // The honest staged failure, not the old offline demo world.
                // `Sim::new` no longer builds one (see its docs): a client holds
                // the server's world or none at all, and a demo world left
                // resident under a later multiplayer join is the two-worlds
                // defect this button used to be the entry point for.
                self.begin_singleplayer();
            }
            MenuAction::Connect(entry) => {
                self.connect_to(entry.host.clone(), entry.effective_port());
            }
            MenuAction::Quit => {}
            MenuAction::Reprobe(Some(entry)) => self.statuses.refresh_one(&entry),
            MenuAction::Reprobe(None) => {
                self.statuses.refresh(self.nav.list().entries());
            }
            MenuAction::Forget(entry) => {
                self.statuses.forget(&entry);
                // A delete or re-address changes the row set; probe whatever is
                // now in the list (idempotent, so this costs nothing per frame).
                self.statuses.refresh(self.nav.list().entries());
            }
            MenuAction::QuitToTitle => {
                // `UiState` has already moved to `MainMenu` — `nav.rs`'s
                // `key_paused` calls `ui.quit_to_title()` before returning this
                // action. What is left is tearing down whatever live session is
                // attached to `Sim` so a fresh connect afterward starts clean;
                // see `Sim::end_session` for exactly what resets vs. persists.
                self.sim.end_session();
                // The pause screen already released the pointer on entry, so
                // this is normally a no-op; cheap insurance against a future
                // caller reaching `QuitToTitle` some other way.
                self.set_grab(false);
            }
        }
    }

    /// Route a mouse position (physical pixels) to a menu row, if it is over one.
    ///
    /// `Screen::Paused` gets its frame from [`crate::menu::render::pause_frame`]
    /// directly rather than [`crate::menu::render::frame_for`], which returns
    /// `None` for it by design (see that function's doc on why the pause
    /// overlay is not an `owns_frame` screen).
    ///
    /// Both branches convert the physical framebuffer size and cursor down to
    /// the same logical canvas [`MenuRenderer::render`]/`render_overlay`
    /// actually draw into (via [`crate::menu::render::logical_canvas`]) before
    /// calling [`crate::menu::render::row_rect`] — mirroring
    /// `container::hit_test_with_scale`'s own `x / scale` pattern. Skipping
    /// this (as this function used to) is exactly the "clicks land one slot
    /// off, invisible in any screenshot" bug that module warns about: it is
    /// only invisible at `gui_scale == 1`, which is why it went unnoticed.
    fn menu_row_at(&mut self, x: f32, y: f32) -> Option<usize> {
        let frame = if self.ui.is_paused() {
            crate::menu::render::pause_frame(&self.nav)
        } else {
            crate::menu::render::frame_for(
                &self.ui,
                &self.nav,
                &self.statuses,
                &mut self.favicons,
            )?
        };
        let (fb_w, fb_h) = self.target.as_ref().map(RenderTarget::size)?;
        let (w, h) = crate::menu::render::logical_canvas(frame.gui_scale, fb_w, fb_h);
        let scale = crate::config::calculate_gui_scale(frame.gui_scale, fb_w, fb_h).max(1) as f32;
        let (lx, ly) = (x / scale, y / scale);
        (0..frame.rows.len()).find(|&i| {
            crate::menu::render::row_rect(&frame.rows, i, w, h)
                .is_some_and(|(rx, ry, rw, rh)| {
                    lx >= rx && lx <= rx + rw && ly >= ry && ly <= ry + rh
                })
        })
    }

    /// Draw one menu screen. Returns `false` when the current screen is not a
    /// menu, so the caller falls through to the world path.
    fn draw_menu(&mut self) -> bool {
        // Land any finished status pings before building the frame, or a row
        // shows "PINGING" for one frame longer than it needs to.
        self.statuses.pump();
        // `frame_for` is the authority on which screens this renderer owns — it
        // covers the three menu screens *and* the error screen. Asking it,
        // rather than re-deriving the set here, is what keeps the two from
        // drifting apart into a screen that is drawn twice or not at all.
        let Some(frame) = crate::menu::render::frame_for(
            &self.ui,
            &self.nav,
            &self.statuses,
            &mut self.favicons,
        ) else {
            return false;
        };
        let (Some(gpu), Some(target), Some(menu)) = (
            self.gpu.as_ref(),
            self.target.as_mut(),
            self.menu.as_mut(),
        ) else {
            // GPU not up yet; still report the screen as handled so the world
            // path does not run for a menu.
            return true;
        };
        let (w, h) = target.size();
        let device = gpu.device();
        let queue = gpu.queue();
        let surface = match target.acquire() {
            Ok(f) => f,
            Err(e) => {
                if e.needs_reconfigure() {
                    target.reconfigure(device);
                }
                return true;
            }
        };
        menu.render(device, queue, surface.view(), &frame, w, h);
        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        surface.present(queue);
        true
    }

    /// Route one key press to the open chat prompt. Enter sends the line through
    /// the client's chat/command seam, Escape cancels, Backspace edits, and any
    /// printable text is appended (control chars and `§` are filtered by
    /// [`ChatInput`]). Both Enter and Escape close the prompt and re-grab.
    fn handle_chat_key(&mut self, event: &winit::event::KeyEvent) {
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::Escape => {
                    let _ = self.chat_input.take();
                    self.ui.close_chat();
                    self.set_grab(self.ui.wants_cursor_grab());
                    return;
                }
                KeyCode::Enter | KeyCode::NumpadEnter => {
                    let line = self.chat_input.take();
                    self.sim.send_chat(&line);
                    self.ui.close_chat();
                    self.set_grab(self.ui.wants_cursor_grab());
                    return;
                }
                KeyCode::Backspace => {
                    self.chat_input.backspace();
                    return;
                }
                _ => {}
            }
        }
        if let Some(text) = &event.text {
            self.chat_input.push_str(text.as_str());
        }
    }

    fn redraw(&mut self) {
        // Reconcile the menu with the live session before we borrow GPU state.
        self.drive_ui_from_session();
        if self.sim.open_menu().is_some() && self.ui.is_playing() {
            self.ui.open_container();
            self.set_grab(false);
        }

        // Pace the frame and tick **before** the GPU-readiness guard. Simulation
        // must never be conditional on a swapchain image: keep-alives and the
        // per-tick movement packet ride this loop, and a client the server
        // considers stalled is sent no chunks at all. `step.dt` is already
        // clamped to vanilla's ten-tick catch-up budget, so a long stall is
        // dropped rather than replayed in a burst.
        let frame_start = Instant::now();
        let step = self.pacer.begin_frame(frame_start);
        let dt = step.dt;
        // `Runner::Winit`: the host event loop drives this driver's `App`
        // itself, once per `RedrawRequested`, by calling `update()` directly
        // — no internal timer, so packet ingest is never gated on frame rate.
        self.ecs.update();
        self.sim.step(dt);
        if !step.render {
            // Unfocused (throttled to ~30 fps) or occluded: skip presenting
            // only. `acquire()` is the call that stalls on a backgrounded
            // window, so it is precisely what must not run here.
            return;
        }

        // A menu screen owns the whole frame — its pass clears, so there is no
        // world render behind it and none of the HUD state below is built.
        if self.draw_menu() {
            return;
        }

        let (Some(gpu), Some(target), Some(render), Some(hud), Some(container_renderer)) = (
            self.gpu.as_ref(),
            self.target.as_mut(),
            self.render.as_mut(),
            self.hud.as_mut(),
            self.container.as_mut(),
        ) else {
            return;
        };
        let device = gpu.device();
        let queue = gpu.queue();

        // Upload any freshly-meshed sections, and drop sections emptied by edits.
        for meshed in self.sim.drain_meshes() {
            render.upload_section(device, meshed.key, &meshed.mesh);
        }
        for key in self.sim.drain_removals() {
            render.remove_section(&key);
        }

        let (w, h) = target.size();
        let frame = match target.acquire() {
            Ok(frame) => frame,
            Err(e) => {
                if e.needs_reconfigure() {
                    target.reconfigure(device);
                    render.resize(device, w, h);
                }
                // Transient (timeout/occluded/validation): just skip this frame.
                return;
            }
        };

        let aspect = w as f32 / h as f32;
        // Recompute the targeted block from the interpolated camera each frame.
        self.sim.update_target(aspect);
        let camera = self.sim.camera(aspect);
        // Reconcile fog with the player's bit-exact fluid state each frame,
        // re-uploading only when it changes (crossing a water/lava surface) so a
        // submerged eye dissolves terrain into short water/lava fog and the
        // surface restores the render-distance sky fog.
        let desired_fog = self.sim.fog_settings();
        if self.applied_fog != Some(desired_fog) {
            render.set_fog(desired_fog);
            self.applied_fog = Some(desired_fog);
        }
        // Drive the audio listener from the exact camera we render, so what the
        // player hears is spatialised to match what they see. No-op when audio
        // is disabled.
        self.sim.set_audio_listener(&camera);
        let outline = self.sim.target.map(|hit| hit.block);
        let entity_draws = self.sim.entity_draws();
        // Extraction lives in `Sim` because resolving each particle's light
        // needs the world; doing it here would hand out two borrows of `Sim`.
        let particle_frame = self.sim.extract_particles(&camera);
        render.prepare_particles(device, queue, self.sim.particle_instances(), &camera);
        render.update_animation(queue, self.sim.tick_count());
        // Route the progressive-mining crack overlay when a dig is in flight,
        // otherwise take the plain path (avoids building the crack buffer while
        // idle). `crack_target()` reads the live mining state, so it is `None`
        // off a server and on the demo path.
        let stats = match self.sim.crack_target() {
            Some(crack) => render.render_with_crack(
                device,
                queue,
                frame.view(),
                &camera,
                outline,
                &entity_draws,
                crack,
            ),
            None => render.render(device, queue, frame.view(), &camera, outline, &entity_draws),
        };

        // Fold GPU counters + timing into the debug overlay.
        let frame_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        let inst_fps = if dt > 0.0 { (1.0 / dt) as f32 } else { 0.0 };
        self.fps_ema = if self.fps_ema == 0.0 {
            inst_fps
        } else {
            self.fps_ema * 0.9 + inst_fps * 0.1
        };
        self.sim.stats.section_count = stats.sections_drawn;
        self.sim.stats.quads = stats.total_quads;
        self.sim.stats.vram_bytes = stats.vram_bytes;
        self.sim.stats.entities_drawn = stats.entities_drawn;
        self.sim.stats.particles_alive = particle_frame.alive;
        self.sim.stats.particles_drawn = stats.particles_drawn;
        self.sim.stats.particles_unresolved = particle_frame.unresolved;
        self.sim.stats.frame_ms = frame_ms;
        self.sim.stats.fps = self.fps_ema;

        let open_menu = self.sim.open_menu();
        let player_menu;
        let (container_menu, container_title) = if let Some(open) = open_menu.as_ref() {
            (Some(&open.menu), open.title.to_plain_string())
        } else if self.ui.is_container_open() {
            player_menu = self.sim.player_menu();
            (Some(&player_menu), "Inventory".to_string())
        } else {
            (None, String::new())
        };
        if container_menu.is_some() {
            // The carried stack follows the pointer, so the frame needs the cursor
            // in physical pixels — the same space `hit_test` and the menu layout
            // use (see the `cursor` field). Without this the stack is built but
            // never positioned, and nothing draws.
            let container_frame = ContainerFrame::new(container_menu, &container_title)
                .with_cursor(Some([self.cursor.0, self.cursor.1]))
                .with_recipe_book(self.recipe_book.as_ref());
            container_renderer.render_scaled(
                device,
                queue,
                frame.view(),
                &container_frame,
                self.nav.gui_scale(),
                w,
                h,
            );
        }

        // Assemble the HUD frame: debug overlay, chat log + prompt, tab list,
        // and the survival gauges. Locals are collected up-front so their
        // borrows outlive the frame struct.
        let chat_open = self.ui.is_chat_open();
        // Pull enough history for the HUD to fade/scroll; it caps and ages them.
        // The feed hands back owned legacy strings (flattened from the canonical
        // `ChatFeed`'s `Text` at read time); borrow them into the `&str` slice
        // the HUD frame takes, keeping both locals alive for the frame's scope.
        let chat_owned: Vec<(String, f32)> = self.sim.recent_chat(if chat_open { 20 } else { 10 });
        let chat_lines: Vec<(&str, f32)> = chat_owned
            .iter()
            .map(|(line, age)| (line.as_str(), *age))
            .collect();
        let player_rows: Vec<String> = if self.tab_held {
            self.sim.player_rows()
        } else {
            Vec::new()
        };
        let health = self.sim.health();
        let food = self.sim.food();
        let sidebar = self.sim.sidebar();
        let boss_bars = self.sim.boss_bars();
        let crosshair = self.ui.is_playing();

        // Snapshot the player's nine hotbar slots into owned draw records. The
        // owned `Menu` is dropped once the icons are built; `hotbar_records`
        // outlives `hud.render` below because the frame borrows it.
        let player_menu = self.sim.player_menu();
        let hotbar_records: Vec<Option<HotbarSlot>> = (0..9)
            .map(|i| {
                player_menu.player_native(i).and_then(|st| {
                    let item = ResourceLocation::parse(&st.item().to_string()).ok()?;
                    let damage = st
                        .components()
                        .get_int(lodestone_game::item::DAMAGE_COMPONENT)
                        .and_then(|v| u32::try_from(v).ok());
                    let max_damage = st
                        .components()
                        .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
                        .and_then(|v| u32::try_from(v).ok());
                    Some(HotbarSlot {
                        item,
                        count: st.count().max(0) as u32,
                        damage,
                        max_damage,
                        enchanted: false,
                    })
                })
            })
            .collect();
        drop(player_menu);

        let mut hud_frame = HudFrame::new(&self.sim.stats);
        hud_frame.show_debug = self.show_debug;
        hud_frame.crosshair = crosshair;
        hud_frame.chat = &chat_lines;
        hud_frame.chat_input = chat_open.then(|| self.chat_input.as_str());
        hud_frame.players = self.tab_held.then_some(player_rows.as_slice());
        hud_frame.sidebar = sidebar.as_ref();
        hud_frame.boss_bars = &boss_bars;
        hud_frame.health = health;
        hud_frame.food = food;
        hud_frame.hotbar = crosshair.then(|| self.sim.selected_slot());
        hud_frame.hotbar_items = crosshair.then_some(hotbar_records.as_slice());
        hud_frame.xp = self.sim.xp();
        hud_frame.title = self.sim.title_overlay();
        hud_frame.action_bar = self.sim.action_bar_overlay();
        hud_frame.recipe_stats = self
            .recipe_book
            .as_ref()
            .map(|book| (book.len(), book.tags().len()));
        // The 3-D block-item icons need the baked model set (for geometry) and a
        // depth attachment (so the near faces of the mini-block win over the far
        // ones). Both are `None` on the demo path, which degrades to flat sprites.
        let item_models = self.sim.vanilla_atlas().and_then(|a| a.models());
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            Some(render.depth_view()),
            &hud_frame,
            item_models,
            self.nav.gui_scale(),
            w,
            h,
        );
        // Status-effect overlay, composited over the HUD in its own Load pass.
        if let Some(effects) = self.effects.as_mut() {
            effects.render(device, queue, frame.view(), self.sim.active_effects(), w, h);
        }

        // The pause overlay draws *over* the world/HUD/container passes above
        // rather than replacing them — see `Screen::Paused`'s doc comment and
        // `menu::render::owns_frame`'s, which is deliberately why `Paused` is
        // not in that set: adding it there would route this screen through
        // `draw_menu`'s `Clear` pass instead and stop the world rendering
        // behind it for as long as the game is paused.
        if self.ui.is_paused()
            && let Some(menu) = self.menu.as_mut()
        {
            let pause_frame = crate::menu::render::pause_frame(&self.nav);
            menu.render_overlay(device, queue, frame.view(), &pause_frame, w, h);
        }

        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        frame.present(queue);

        if self.last_log.elapsed() >= Duration::from_secs(1) {
            self.last_log = Instant::now();
            println!("{}", self.sim.stats.one_line());
        }
    }
}

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Lodestone")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let (gpu, target) = match attach_window(window.clone()) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("failed to attach GPU to window: {e}");
                event_loop.exit();
                return;
            }
        };

        let (w, h) = target.size();
        let format = target.format();
        let mut render = RenderState::new(
            gpu.device(),
            gpu.queue(),
            format,
            w,
            h,
            self.sim.vanilla_atlas(),
        );
        // Size the sky fog to our real render distance so terrain fades into the
        // sky where chunks actually stop, not at the render crate's 8-chunk default.
        render.set_fog(sky_fog(self.config.render_distance));
        let mut hud = HudRenderer::new(gpu.device(), format);
        // Attach the vanilla GUI sprite atlas so the survival vitals draw from
        // real textures; on a jar-less run this is `None` and the HUD keeps its
        // procedural fallback.
        if let Some(gui) = crate::resources::load_gui_atlas() {
            hud.attach_gui(gpu.device(), gpu.queue(), format, gui);
        }
        // Load the real crafting-recipe corpus from `client.jar`, once. Feeds
        // the container screen's ghost-preview draw and the debug-overlay
        // counter; a jar-less run leaves this `None` and neither draws.
        self.recipe_book = crate::resources::load_recipe_book();
        // Attach the flat item-sprite atlas so hotbar/container slots draw real
        // item icons; jar-less runs leave this `None` and slots stay empty wells.
        // Loaded once and shared: the container screen needs the same atlas, and
        // `ItemAtlas` is behind an `Arc` precisely so the second consumer is a
        // refcount bump rather than a second stitch of the whole item corpus.
        let item_atlas = crate::resources::load_item_atlas();
        if let Some(items) = item_atlas.clone() {
            hud.attach_items(gpu.device(), gpu.queue(), format, items);
        }
        // Attach the 3-D block-item pass, which borrows the world renderer's own
        // block atlas, tint palette and animation slots rather than uploading a
        // second copy of any of them. Present only on the live vanilla path (the
        // demo world bakes no models), where block items would otherwise draw an
        // empty well.
        if let (Some(atlas_view), Some(atlas_sampler), Some(palette), Some(anim)) = (
            render.model_atlas_view(),
            render.model_atlas_sampler(),
            render.model_palette_buffer(),
            render.model_anim_buffer(),
        ) {
            hud.attach_item_models(
                gpu.device(),
                format,
                atlas_view,
                atlas_sampler,
                palette,
                anim,
            );
        }
        let effects = EffectsRenderer::new(gpu.device(), format);

        // The container screen draws real item icons through the *same* shared
        // pass the hotbar uses (`hud::item_icon`), so both must be attached or
        // slots fall back to hash-derived colour swatches. Without this the
        // capability is complete, gated and reaches zero pixels — the island
        // pattern this project has hit eleven times.
        let mut container = ContainerRenderer::new(gpu.device(), format);
        if let Some(items) = item_atlas {
            container.attach_items(gpu.device(), gpu.queue(), format, items);
        }
        if let (Some(atlas_view), Some(atlas_sampler), Some(palette), Some(anim)) = (
            render.model_atlas_view(),
            render.model_atlas_sampler(),
            render.model_palette_buffer(),
            render.model_anim_buffer(),
        ) {
            container.attach_item_models(
                gpu.device(),
                format,
                atlas_view,
                atlas_sampler,
                palette,
                anim,
            );
        }

        // Upload whatever has already meshed; the rest streams in per frame.
        for meshed in self.sim.drain_meshes() {
            render.upload_section(gpu.device(), meshed.key, &meshed.mesh);
        }

        let menu = MenuRenderer::new(gpu.device(), format);

        // Choose the session per config. A connection target on the command line
        // dials it immediately (and shows a loading screen until login);
        // otherwise the window opens on the **main menu**, which is now the GUI
        // entry point. Singleplayer from the menu enters the local worldgen world
        // — *not* the integrated server, which isn't wired yet (see
        // `WindowApp::begin_singleplayer`).
        if requested_a_connection(&self.config) {
            self.ui.begin(SessionKind::Multiplayer);
            let net = NetClient::connect(
                self.config.host.clone(),
                self.config.port,
                self.config.protocol,
            );
            // Install the entity light sampler now, at connect time, not after
            // login: `set_entity_light_source` wants a `'static` closure
            // installed *once*, and the shared handle it needs is available
            // immediately (it is an `Arc<OnceLock<_>>` the net thread resolves
            // later — see `net::SharedHandle`). Waiting for `LoggedIn` would
            // just delay the install for no benefit, since the closure already
            // tolerates an unresolved handle (`entity_light_at` reads `None`
            // and the sampler falls back to full-bright, exactly matching the
            // "no world yet" state during connect). This has to happen before
            // `attach_net` moves `net` into `self.sim` — `NetClient` itself
            // isn't `Clone` and doesn't outlive this function, only the shared
            // handle inside it does.
            let entity_light_handle = net.shared_handle();
            // See `connect_to`: same clock for terrain and mobs, installed here
            // too because this is the second, independent connect path.
            let clock = net.shared_handle();
            render.set_sky_darken_source(move || {
                clock
                    .get()
                    .map(|h| lodestone_render::entity::sky_darken_for_time_of_day(h.world_time().1))
            });
            render.set_entity_light_source(move |feet| {
                crate::net::entity_light_at(
                    &entity_light_handle,
                    feet.x.floor() as i32,
                    feet.y.floor() as i32,
                    feet.z.floor() as i32,
                )
            });
            self.sim.attach_net(net);
        }
        // No target requested: stay on `Screen::MainMenu`, which `UiState::new`
        // already put us on. Nothing else to do.

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.target = Some(target);
        self.render = Some(render);
        self.hud = Some(hud);
        self.effects = Some(effects);
        self.container = Some(container);
        self.menu = Some(menu);
        // Grab only if the chosen screen wants it (menus and loading: no).
        self.set_grab(self.ui.wants_cursor_grab());
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let (Some(gpu), Some(target), Some(render)) = (
                    self.gpu.as_ref(),
                    self.target.as_mut(),
                    self.render.as_mut(),
                ) {
                    target.resize(gpu.device(), size.width, size.height);
                    render.resize(gpu.device(), size.width, size.height);
                }
            }
            WindowEvent::Focused(false) => {
                // Losing focus pauses (and releases the pointer) so we don't
                // keep grabbing the mouse of a backgrounded window. The *world*
                // is not paused by this: `Screen::Paused` is local UI state and
                // the sim keeps ticking (see `FramePacer`), which is what keeps
                // keep-alives and movement flowing to the server.
                self.ui.pause();
                self.set_grab(false);
                self.pacer.set_focused(false);
            }
            WindowEvent::Focused(true) => {
                // Presentation resumes at full rate. The pointer is *not*
                // re-grabbed here — the player clicks to resume, as before.
                self.pacer.set_focused(true);
            }
            WindowEvent::Occluded(occluded) => {
                // Fully covered or minimised: there is nothing on screen to
                // update and acquiring a drawable is what stalls, so drop
                // presentation entirely while continuing to tick.
                self.pacer.set_occluded(occluded);
            }
            // Hovering a menu row highlights it, so the mouse and the keyboard
            // drive one selection rather than two. `Screen::Paused` shares this
            // arm too even though it is not `owns_frame` — see `menu_row_at`'s
            // doc — because it has its own row navigation to hover just like
            // every screen this renderer owns.
            WindowEvent::CursorMoved { position, .. }
                if crate::menu::render::owns_frame(self.ui.screen()) || self.ui.is_paused() =>
            {
                self.cursor = (position.x as f32, position.y as f32);
                if let Some(row) = self.menu_row_at(self.cursor.0, self.cursor.1) {
                    self.nav.hover(&self.ui, row);
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if crate::menu::render::owns_frame(self.ui.screen()) || self.ui.is_paused() =>
            {
                if state == ElementState::Pressed && button == MouseButton::Left {
                    // Only a click *on a row* activates: clicking the backdrop
                    // must not confirm whatever happens to be highlighted.
                    if let Some(row) = self.menu_row_at(self.cursor.0, self.cursor.1) {
                        self.nav.hover(&self.ui, row);
                        self.handle_menu_key(MenuKey::Enter);
                    }
                }
                // Every `owns_frame` action handles its own grab (each of them
                // either stays on a menu screen, which never grabs, or moves to
                // Playing through a path that already calls `set_grab`).
                // `PauseButton::BackToGame` does not — `handle_menu_key` only
                // calls `MenuNav::key`, which flips `UiState` to `Playing` and
                // returns, with nothing here to notice. Without this a click on
                // Back to Game resumes play with the pointer still released:
                // visible but unusable.
                let want = self.ui.wants_cursor_grab();
                if want != self.grabbed {
                    self.set_grab(want);
                }
            }
            // Track the cursor and, mid-drag, the slots it paints while a
            // container screen is up. This is a separate arm from the menu one
            // above because `Screen::Container` is not `owns_frame` — the
            // container overlay draws over the world, it does not replace it.
            WindowEvent::CursorMoved { position, .. } if self.ui.is_container_open() => {
                self.cursor = (position.x as f32, position.y as f32);
                if self.menu_input.is_dragging() {
                    if let (Some(menu), Some((w, h))) = (
                        self.active_container_menu(),
                        self.target.as_ref().map(RenderTarget::size),
                    ) {
                        let hit = hit_test_with_scale(
                            &menu,
                            self.nav.gui_scale(),
                            w,
                            h,
                            self.cursor.0,
                            self.cursor.1,
                        );
                        self.menu_input.dragged(hit);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } if self.ui.is_container_open() => {
                if let Some(menu_button) = menu_button_for(button)
                    && let (Some(menu), Some((w, h))) = (
                        self.active_container_menu(),
                        self.target.as_ref().map(RenderTarget::size),
                    )
                {
                    let hit = hit_test_with_scale(
                        &menu,
                        self.nav.gui_scale(),
                        w,
                        h,
                        self.cursor.0,
                        self.cursor.1,
                    );
                    let ctx = MenuContext {
                        cursor_loaded: menu.carried().is_some(),
                        // No game-mode plumbing exists on `Sim` to source this
                        // from yet — see the report on this change.
                        creative: false,
                    };
                    let clicks = match state {
                        ElementState::Pressed => {
                            let now = Instant::now();
                            let is_repeat = menu_button == MenuButton::Left
                                && self
                                    .last_menu_click
                                    .is_some_and(|t| now.duration_since(t) < DOUBLE_CLICK_WINDOW);
                            self.last_menu_click = Some(now);
                            self.menu_input
                                .press(hit, menu_button, self.shift_held, ctx, is_repeat, &menu)
                        }
                        ElementState::Released => {
                            self.menu_input
                                .release(hit, menu_button, self.shift_held, ctx, &menu)
                        }
                    };
                    for click in clicks {
                        self.send_menu_click(click);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // `Screen::Paused` no longer reaches this catch-all at all — the
                // `owns_frame(...) || self.ui.is_paused()` arm above now handles
                // every click while paused (hover + activate the highlighted
                // pause-menu row, including Back to Game via `MenuKey::Enter`).
                if self.grabbed {
                    // Left mines (hold-to-mine on live; one-shot break on demo),
                    // right uses/places against the targeted face.
                    match (button, state) {
                        (MouseButton::Left, ElementState::Pressed) => {
                            self.sim.begin_attack();
                        }
                        (MouseButton::Left, ElementState::Released) => {
                            self.sim.end_attack();
                        }
                        (MouseButton::Right, ElementState::Pressed) => {
                            self.sim.use_item();
                        }
                        _ => {}
                    }
                }
            }
            // Scroll cycles the hotbar (down = right, like vanilla) only
            // during active play; menus and the chat prompt ignore it.
            WindowEvent::MouseWheel { delta, .. } if self.ui.accepts_gameplay_input() => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                if dy > 0.0 {
                    self.sim.cycle_slot(-1);
                } else if dy < 0.0 {
                    self.sim.cycle_slot(1);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;

                // Tracked unconditionally (not gated on `accepts_gameplay_input`
                // like `action_for`'s Sneak binding below): a container
                // shift-click is a `QuickMove`, not movement, and must still
                // work while gameplay input is not being accepted.
                if let PhysicalKey::Code(code) = event.physical_key
                    && matches!(code, KeyCode::ShiftLeft | KeyCode::ShiftRight)
                {
                    self.shift_held = pressed;
                }

                // A menu screen captures every key: the edit form needs the
                // whole keyboard, and no gameplay binding may fire behind it.
                // `Screen::Paused` shares this arm too — it is not `owns_frame`
                // (see that function's doc), but it has its own keyboard
                // navigation (`MenuNav::key_paused`) that needs exactly the same
                // routing, including the grab re-sync just below for Back to
                // Game.
                if crate::menu::render::owns_frame(self.ui.screen()) || self.ui.is_paused() {
                    if pressed {
                        if let Some(key) = Self::menu_key_for(&event) {
                            self.handle_menu_key(key);
                            // Entering the world grabs; leaving it releases.
                            let want = self.ui.wants_cursor_grab();
                            if want != self.grabbed {
                                self.set_grab(want);
                            }
                        }
                    }
                } else if self.ui.is_chat_open() {
                    if pressed {
                        self.handle_chat_key(&event);
                    }
                } else if let PhysicalKey::Code(code) = event.physical_key {
                    if code == KeyCode::Escape && pressed {
                        // Context-sensitive: Playing↔Paused, Error→menu, etc.
                        if self.ui.is_container_open() {
                            self.sim.close_open_menu();
                        }
                        self.ui.on_escape();
                        self.set_grab(self.ui.wants_cursor_grab());
                    } else if self.ui.is_container_open() && pressed {
                        match code {
                            KeyCode::Escape | KeyCode::KeyE => {
                                self.sim.close_open_menu();
                                self.ui.close_container();
                                self.set_grab(self.ui.wants_cursor_grab());
                            }
                            _ => {}
                        }
                    } else if code == KeyCode::F3 && pressed {
                        // Toggle the debug instrument (§S4).
                        self.show_debug = !self.show_debug;
                    } else if code == KeyCode::Tab && self.ui.accepts_gameplay_input() {
                        // Hold-to-show player list; released on key-up.
                        self.tab_held = pressed;
                    } else if (code == KeyCode::KeyT || code == KeyCode::Slash)
                        && pressed
                        && self.ui.accepts_gameplay_input()
                    {
                        // Open the chat prompt; `/` pre-fills the command prefix.
                        // Release held movement so we don't walk while typing.
                        self.sim.input_mut().release_all();
                        let _ = self.chat_input.take();
                        if code == KeyCode::Slash {
                            self.chat_input.push_char('/');
                        }
                        self.ui.open_chat();
                        self.tab_held = false;
                        self.set_grab(false);
                    } else if code == KeyCode::KeyE && pressed && self.ui.accepts_gameplay_input() {
                        self.sim.input_mut().release_all();
                        self.ui.open_container();
                        self.tab_held = false;
                        self.set_grab(false);
                    } else if code == KeyCode::KeyF && pressed && self.ui.accepts_gameplay_input() {
                        self.sim.toggle_fly();
                    } else if let Some(slot) = hotbar_slot_for(code)
                        && pressed
                        && self.ui.accepts_gameplay_input()
                    {
                        // Number keys 1..9 select the hotbar slot directly.
                        self.sim.select_slot(slot);
                    } else if let Some(action) = action_for(code)
                        && self.ui.accepts_gameplay_input()
                    {
                        self.sim.input_mut().set(action, pressed);
                    }
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }

        // Clean shutdown path: any handler may latch a quit request.
        if self.ui.quit_requested() {
            event_loop.exit();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event
            && self.ui.is_playing()
            && self.grabbed
        {
            self.sim.input_mut().add_mouse(delta.0 as f32, delta.1 as f32);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        // Spin while focused (vsync paces the loop); otherwise sleep in short
        // `BACKGROUND_POLL` slices so a backgrounded window stops burning a core
        // yet still wakes far more often than the 20 Hz tick needs.
        event_loop.set_control_flow(self.pacer.control_flow(Instant::now()));
    }
}

fn run_windowed(config: Config) -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = WindowApp::new(config);
    event_loop.run_app(&mut app)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Headless: render one frame offscreen, save a PPM, print stats.
// ---------------------------------------------------------------------------

fn run_headless(config: Config) -> anyhow::Result<()> {
    let ctx = GpuContext::new_headless_blocking()
        .map_err(|e| anyhow::anyhow!("headless GPU bring-up failed: {e}"))?;
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (1280u32, 720u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let render_distance = config.render_distance;
    // The offline evidence path, and the one place the demo world still exists:
    // this renders a single frame with no server and *fails* below 5% terrain
    // coverage, so it needs a world that does not come from a connection. The
    // interactive client has none — see `Sim::new`. (`Sim::new` would delegate
    // here anyway on `Mode::Headless`; spelled out so the dependency is visible
    // at the call site rather than hidden in a mode check.)
    let mut sim = Sim::with_demo_world(config);
    let mut render = RenderState::new(device, queue, format, w, h, sim.vanilla_atlas());
    render.set_fog(sky_fog(render_distance));

    // Mesh everything and upload.
    let meshes = sim.drain_all_meshes();
    let mut meshed_quads = 0usize;
    for m in &meshes {
        meshed_quads += m.mesh.quad_count();
        render.upload_section(device, m.key, &m.mesh);
    }

    // Let the player settle onto the ground so the camera sits at a sane height.
    for _ in 0..40 {
        sim.step(1.0 / 20.0);
    }

    let camera = sim.camera(w as f32 / h as f32);
    // Outline the block directly under the settled player, as a visible probe.
    let outline = {
        let p = sim.player().position;
        Some([
            p.x.floor() as i32,
            p.y.floor() as i32 - 1,
            p.z.floor() as i32,
        ])
    };
    let start = Instant::now();
    let frame = target
        .acquire()
        .map_err(|e: TargetError| anyhow::anyhow!("headless acquire failed: {e}"))?;
    let entity_draws = sim.entity_draws();
    let _ = sim.extract_particles(&camera);
    render.prepare_particles(device, queue, sim.particle_instances(), &camera);
    render.update_animation(queue, sim.tick_count());
    let stats = render.render(device, queue, frame.view(), &camera, outline, &entity_draws);
    let pixels = target.read_texels(device, queue);
    let frame_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Coverage: fraction of pixels that clearly aren't the sky clear colour.
    //
    // This target is *not* an sRGB-format texture, so these bytes are the
    // shader's linear output scaled straight to 0..255 with no gamma encode
    // (unlike the swapchain, which is sRGB and would encode them). That's
    // `SKY_COLOR * 255` rounded, not the on-screen sky colour — read
    // `gpu::SKY_COLOR`'s doc comment before touching this to keep the two in
    // sync.
    let sky = [62i32, 118, 211];
    let mut terrain_px = 0usize;
    for px in pixels.chunks_exact(4) {
        let d = (i32::from(px[0]) - sky[0]).abs()
            + (i32::from(px[1]) - sky[1]).abs()
            + (i32::from(px[2]) - sky[2]).abs();
        if d > 60 {
            terrain_px += 1;
        }
    }
    let coverage = terrain_px as f64 / (w * h) as f64 * 100.0;

    let out = "lodestone-frame.ppm";
    write_ppm(out, w, h, &pixels)?;

    sim.stats.section_count = stats.sections_drawn;
    sim.stats.quads = stats.total_quads;
    sim.stats.vram_bytes = stats.vram_bytes;
    sim.stats.frame_ms = frame_ms as f32;

    println!("=== lodestone headless render ===");
    println!("world chunks      = {}", sim.world.len());
    println!("sections meshed   = {}", meshes.len());
    println!("sections drawn    = {}", stats.sections_drawn);
    println!("quads (meshed)    = {meshed_quads}");
    println!("quads (drawn)     = {}", stats.total_quads);
    println!("draw calls        = {}", stats.draw_calls);
    println!("mesh VRAM (bytes) = {}", stats.vram_bytes);
    println!("terrain coverage  = {coverage:.1}%");
    println!("frame time (ms)   = {frame_ms:.3}");
    println!("saved frame       = {out}");
    println!("{}", sim.stats.one_line());

    if coverage < 5.0 {
        anyhow::bail!("rendered frame shows <5% terrain ({coverage:.1}%) — nothing visible");
    }
    Ok(())
}

/// Write a binary (P6) PPM — no image-crate dependency needed for evidence.
fn write_ppm(path: &str, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut buf = Vec::with_capacity((w * h * 3 + 32) as usize);
    buf.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for px in rgba.chunks_exact(4) {
        buf.extend_from_slice(&px[..3]);
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)
}

// ---------------------------------------------------------------------------
// Connect: stream live events for a bounded time, no GPU.
// ---------------------------------------------------------------------------

fn run_connect(config: Config) -> anyhow::Result<()> {
    println!(
        "connecting to {}:{} (protocol {}) for {}s…",
        config.host,
        config.port,
        config.protocol,
        config.connect_for.as_secs()
    );
    let net = NetClient::connect(config.host.clone(), config.port, config.protocol);
    let deadline = Instant::now() + config.connect_for;
    let mut seen = 0usize;

    while Instant::now() < deadline {
        for update in net.poll() {
            seen += 1;
            println!("[net] {update:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("streamed {seen} update(s); exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cheap sim: headless mode with the smallest render distance that still
    /// generates real terrain, so physics ticks do real collision work.
    fn pacing_sim() -> Sim {
        // Explicitly the demo-world fixture: this needs real terrain so the
        // physics ticks do collision work, and the client `Sim::new` has none.
        Sim::with_demo_world(Config {
            mode: Mode::Headless,
            render_distance: 2,
            ..Config::default()
        })
    }

    /// Ticks a real `Sim` executes when advanced by `dt` in one call.
    fn ticks_for(sim: &mut Sim, dt: f64) -> u64 {
        let before = sim.tick_count();
        sim.step(dt);
        sim.tick_count() - before
    }

    #[test]
    fn vanillas_cap_is_ten_ticks_of_real_time() {
        // Guards the constant against a silent edit. 10 ticks × 50 ms = 500 ms;
        // read from Minecraft.java:262 / :1176 (see `MAX_TICKS_PER_UPDATE`).
        assert_eq!(MAX_TICKS_PER_UPDATE, 10);
        assert!((MAX_CATCHUP_SECS - 0.5).abs() < 1e-12, "{MAX_CATCHUP_SECS}");
    }

    #[test]
    fn a_long_stall_is_clamped_not_replayed() {
        // The reported bug: tab out for a minute, tab back in, and the client
        // tries to run every tick it missed. Sixty seconds is 1200 ticks.
        let stall = Duration::from_secs(60);
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        let step = pacer.begin_frame(t0 + stall);

        assert!(
            (step.dt - MAX_CATCHUP_SECS).abs() < 1e-12,
            "a {stall:?} stall must be clamped to {MAX_CATCHUP_SECS}s, got {}",
            step.dt
        );

        // Drive a *real* sim with it and count the ticks that actually run.
        let mut sim = pacing_sim();
        let clamped = ticks_for(&mut sim, step.dt);
        assert!(
            clamped <= u64::from(MAX_TICKS_PER_UPDATE),
            "catch-up must never exceed vanilla's cap, got {clamped}"
        );

        // Measured: **5**, not 10. `Sim::step` applies its own, tighter
        // `dt.clamp(0.0, 0.25)` (sim.rs:938) to the accumulator *before* the
        // tick loop, so the shell's effective catch-up budget is half vanilla's
        // 500 ms. That inner clamp predates this pacer (it is in the initial
        // commit) and lives in a file this change does not own, so the number is
        // pinned here rather than corrected: if anyone loosens or removes it,
        // this fails and the two caps get reconciled deliberately.
        assert_eq!(
            clamped, 5,
            "sim.rs's inner 0.25 s clamp is expected to bind before app.rs's \
             {MAX_CATCHUP_SECS} s one; if this changed, reconcile the two caps"
        );

        // -- negative control ------------------------------------------------
        // Prove the detector fires: the same real `Sim`, driven the
        // *proportional* way the bug describes (one tick's worth of dt at a
        // time until the stall is consumed), executes the full 1200 ticks. If
        // `tick_count` could not observe a burst, this would not move either.
        let mut control = pacing_sim();
        let mut unclamped = 0u64;
        for _ in 0..(stall.as_secs_f64() / TICK_SECS) as u32 {
            unclamped += ticks_for(&mut control, TICK_SECS);
        }
        assert_eq!(unclamped, 1200, "control must replay every missed tick");
        assert!(
            unclamped > clamped * 100,
            "clamp must be a large reduction: {clamped} vs {unclamped}"
        );
    }

    #[test]
    fn a_normal_frame_is_untouched_by_the_clamp() {
        // The clamp must be invisible at playable frame rates, or it would be
        // silently dropping game time during ordinary play (which is exactly
        // what a too-tight cap does: at 4 fps a 0.25 s cap discards 75% of it).
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        let frame = Duration::from_micros(16_667); // 60 fps
        let step = pacer.begin_frame(t0 + frame);
        assert!(
            (step.dt - frame.as_secs_f64()).abs() < 1e-9,
            "60 fps frame was altered: {}",
            step.dt
        );

        // And a 4 fps frame — the rate an occluded window degrades to — must
        // still deliver all 250 ms, i.e. five whole ticks, not be truncated.
        let mut pacer = FramePacer::new(t0);
        let step = pacer.begin_frame(t0 + Duration::from_millis(250));
        let mut sim = pacing_sim();
        assert_eq!(ticks_for(&mut sim, step.dt), 5);
    }

    #[test]
    fn an_unfocused_window_keeps_ticking_and_presents_at_thirty_fps() {
        // The whole point: presentation throttles, simulation does not.
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_focused(false);

        let mut sim = pacing_sim();
        let mut rendered = 0u32;
        let mut ticks = 0u64;
        // One simulated second at a 120 Hz loop rate.
        for i in 1..=120u32 {
            let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0));
            if step.render {
                rendered += 1;
            }
            ticks += ticks_for(&mut sim, step.dt);
        }

        // 19 or 20: one simulated second at 20 Hz, modulo where the fixed-step
        // residual happens to land (1/120 is not exact in binary, so the last
        // tick can fall just past the second boundary).
        assert!(
            (19..=20).contains(&ticks),
            "unfocused must still tick at ~20 Hz, got {ticks}"
        );
        assert!(
            (30..=31).contains(&rendered),
            "unfocused presentation should be ~30 fps, got {rendered}"
        );
        assert!(
            u64::from(rendered) > ticks,
            "sanity: 30 fps presentation must still outpace 20 Hz ticking"
        );
    }

    /// Counts frames a naive "elapsed since the last presented frame" gate would
    /// deliver over `iters` iterations of a `loop_hz` loop. This is verbatim the
    /// implementation [`FramePacer`] used to have — including the `as_secs_f64()`
    /// comparison against a `1.0 / 30.0` target, which is part of why it drifted:
    /// a `Duration` is whole nanoseconds, so an interval that lands on
    /// 33 333 333 ns is *always* a hair short of 1/30 s and the very iteration
    /// that should have presented never does.
    fn naive_gate_frames(loop_hz: u32, iters: u32) -> u32 {
        let target_secs = 1.0 / f64::from(UNFOCUSED_FPS);
        let t0 = Instant::now();
        let mut last_render = t0;
        let mut n = 0;
        for i in 1..=iters {
            let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
            if now.saturating_duration_since(last_render).as_secs_f64() >= target_secs {
                last_render = now;
                n += 1;
            }
        }
        n
    }

    /// Same span, driven through the real pacer while unfocused.
    fn paced_frames(loop_hz: u32, iters: u32) -> u32 {
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_focused(false);
        let mut n = 0;
        for i in 1..=iters {
            let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
            if pacer.begin_frame(now).render {
                n += 1;
            }
        }
        n
    }

    #[test]
    fn the_unfocused_frame_schedule_does_not_drift_below_its_target() {
        // The bug, and the negative control for the fix. A 30 fps limiter that
        // quietly delivers 26 fps is the whole reason the deadline is absolute:
        // the naive gate can only fire on a loop iteration, and each firing
        // pushes the next deadline out by however far it overshot.
        //
        // Measured, one simulated second each:
        //   loop     naive   paced   target
        //   120 Hz     26      30      30
        //    75 Hz     25      30      30
        //    77 Hz     26      30      30
        for loop_hz in [120u32, 75, 77, 144, 240] {
            let naive = naive_gate_frames(loop_hz, loop_hz);
            let paced = paced_frames(loop_hz, loop_hz);
            assert!(
                (UNFOCUSED_FPS..=UNFOCUSED_FPS + 1).contains(&paced),
                "at {loop_hz} Hz the absolute schedule delivered {paced}, \
                 wanted {UNFOCUSED_FPS}"
            );
            // The control must be observed *failing* the same assertion, or this
            // test proves only that some number came out of some function.
            assert!(
                naive < UNFOCUSED_FPS,
                "control did not fire at {loop_hz} Hz: the naive gate delivered \
                 {naive}, so this test is not measuring the drift it exists for"
            );
        }
        // Exact pre-fix number at the loop rate the sibling test uses, pinned so
        // a future refactor that reintroduces drift is unambiguous.
        assert_eq!(naive_gate_frames(120, 120), 26);
    }

    #[test]
    fn coming_back_from_a_stall_resumes_the_rate_rather_than_replaying_a_backlog() {
        // The presentation-side twin of the catch-up-tick bug: a schedule that
        // advanced by whole intervals *unconditionally* would owe 3600 frames
        // after a two-minute stall and present them as fast as the loop spins.
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_focused(false);
        // Two minutes with no iterations at all, then a tight 120 Hz loop for
        // half a second.
        let resume = t0 + Duration::from_secs(120);
        assert!(pacer.begin_frame(resume).render, "the first frame back draws");

        let mut after = 0;
        for i in 1..=60u32 {
            if pacer
                .begin_frame(resume + Duration::from_secs_f64(f64::from(i) / 120.0))
                .render
            {
                after += 1;
            }
        }
        // Half a second at 30 fps is 15 frames. The backlog would be ~3600.
        assert!(
            (14..=16).contains(&after),
            "expected the steady ~30 fps rate after resuming, got {after} frames \
             in 0.5 s — a replayed backlog looks like ~60 (loop-rate-bound)"
        );
    }

    #[test]
    fn an_occluded_window_skips_presenting_entirely_but_still_ticks() {
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_occluded(true);

        let mut sim = pacing_sim();
        let mut ticks = 0u64;
        for i in 1..=120u32 {
            let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0));
            assert!(!step.render, "occluded windows must not acquire a drawable");
            ticks += ticks_for(&mut sim, step.dt);
        }
        assert!(
            (19..=20).contains(&ticks),
            "occluded must still tick at ~20 Hz, got {ticks}"
        );

        // Control: the identical loop with occlusion cleared *does* render, so
        // the assertion above is testing occlusion and not a dead pacer.
        pacer.set_occluded(false);
        let step = pacer.begin_frame(t0 + Duration::from_secs(2));
        assert!(step.render, "clearing occlusion must restore presentation");
    }

    #[test]
    fn focus_selects_the_control_flow_without_ever_stopping_the_loop() {
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        assert!(matches!(pacer.control_flow(t0), ControlFlow::Poll));
        assert!(pacer.focused());

        pacer.set_focused(false);
        match pacer.control_flow(t0) {
            ControlFlow::WaitUntil(at) => {
                let slice = at.saturating_duration_since(t0);
                assert!(
                    slice < Duration::from_secs_f64(TICK_SECS),
                    "background poll {slice:?} must wake faster than one 50 ms tick, \
                     or the sim falls behind the server while merely unfocused"
                );
            }
            other => panic!("unfocused must sleep, not spin or wait forever: {other:?}"),
        }
        assert!(!pacer.focused());
    }

    #[test]
    fn staged_singleplayer_launch_fails_loudly_with_a_fix_hint() {
        // The integrated server isn't wired, so the launcher must report an
        // honest, actionable failure rather than silently doing nothing.
        let err = launch_singleplayer().expect_err("singleplayer must be staged, not silent");
        assert_eq!(err, LaunchError::NoServerProtocol);
        let msg = err.to_string();
        assert!(
            msg.contains("integrated server"),
            "unhelpful message: {msg}"
        );
        assert!(
            msg.contains("Multiplayer"),
            "should point at the working path: {msg}"
        );
    }
}
