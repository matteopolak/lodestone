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
    GpuContext, HeadlessTarget, RenderTarget, TargetError, window::attach_window,
};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::chat::ChatInput;
use crate::config::{Config, Mode};
use crate::container::{ContainerFrame, ContainerRenderer};
use crate::effects::EffectsRenderer;
use crate::gpu::RenderState;
use crate::hud::{HotbarSlot, HudFrame, HudRenderer};
use crate::menu::UiState;
use crate::net::NetClient;
use crate::sim::Sim;
use lodestone_assets::ResourceLocation;
use lodestone_controller::Action;

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
    /// Playing ↔ paused screen state; owns cursor-grab intent and shutdown.
    ui: UiState,
    /// F3 debug overlay visibility (starts on — it's the instrument, §S4).
    show_debug: bool,
    /// Whether Tab is currently held (shows the player-list overlay).
    tab_held: bool,
    /// Editable buffer for the chat prompt; only consumed while chat is open.
    chat_input: ChatInput,
    fps_ema: f32,
    last_log: Instant,
}

impl WindowApp {
    fn new(config: Config) -> Self {
        let sim = Sim::new(config.clone());
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
            ui: UiState::new(),
            show_debug: true,
            tab_held: false,
            chat_input: ChatInput::new(),
            fps_ema: 0.0,
            last_log: Instant::now(),
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
            self.sim.input.release_all();
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
    #[allow(dead_code)]
    fn begin_singleplayer(&mut self) {
        self.ui.begin(crate::menu::SessionKind::Singleplayer);
        match launch_singleplayer() {
            Ok(net) => self.sim.attach_net(net),
            Err(e) => self.ui.session_failed(e.to_string()),
        }
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

        let frame_start = Instant::now();
        let dt = self.sim.step_realtime();

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
        let stats = render.render(device, queue, frame.view(), &camera, outline, &entity_draws);

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
            let container_frame = ContainerFrame::new(container_menu, &container_title);
            container_renderer.render(device, queue, frame.view(), &container_frame, w, h);
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
        hud.render(device, queue, frame.view(), &hud_frame, w, h);
        // Status-effect overlay, composited over the HUD in its own Load pass.
        if let Some(effects) = self.effects.as_mut() {
            effects.render(device, queue, frame.view(), self.sim.active_effects(), w, h);
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
        let mut hud = HudRenderer::new(gpu.device(), format);
        // Attach the vanilla GUI sprite atlas so the survival vitals draw from
        // real textures; on a jar-less run this is `None` and the HUD keeps its
        // procedural fallback.
        if let Some(gui) = crate::resources::load_gui_atlas() {
            hud.attach_gui(gpu.device(), gpu.queue(), format, gui);
        }
        // Attach the flat item-sprite atlas so hotbar/container slots draw real
        // item icons; jar-less runs leave this `None` and slots stay empty wells.
        if let Some(items) = crate::resources::load_item_atlas() {
            hud.attach_items(gpu.device(), gpu.queue(), format, items);
        }
        let effects = EffectsRenderer::new(gpu.device(), format);
        let container = ContainerRenderer::new(gpu.device(), format);

        // Upload whatever has already meshed; the rest streams in per frame.
        for meshed in self.sim.drain_meshes() {
            render.upload_section(gpu.device(), meshed.key, &meshed.mesh);
        }

        // Choose the session per config. Multiplayer opens a live connection and
        // shows a loading (Connecting) screen until login; otherwise we drop
        // straight into the local dev world (the worldgen stand-in — *not* the
        // integrated server, which isn't wired yet). Singleplayer is staged: see
        // `WindowApp::begin_singleplayer`.
        if self.config.connect_in_window {
            self.ui.begin(crate::menu::SessionKind::Multiplayer);
            let net = NetClient::connect(
                self.config.host.clone(),
                self.config.port,
                self.config.protocol,
            );
            self.sim.attach_net(net);
        } else {
            self.ui.enter_dev_world();
        }

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.target = Some(target);
        self.render = Some(render);
        self.hud = Some(hud);
        self.effects = Some(effects);
        self.container = Some(container);
        // Grab only if the chosen screen wants it (dev world yes; loading no).
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
                // keep grabbing the mouse of a backgrounded window.
                self.ui.pause();
                self.set_grab(false);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if self.ui.is_paused() {
                    // First click on the paused world resumes and re-grabs; it
                    // is consumed by the resume, not passed on as an attack.
                    if state == ElementState::Pressed && button == MouseButton::Left {
                        self.ui.resume();
                        self.set_grab(true);
                    }
                } else if self.grabbed {
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

                // While the chat prompt is open it captures every key.
                if self.ui.is_chat_open() {
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
                    } else if code == KeyCode::KeyQ && pressed && self.ui.is_paused() {
                        // A quit affordance from the (text-less) pause screen.
                        self.ui.request_quit();
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
                        self.sim.input.release_all();
                        let _ = self.chat_input.take();
                        if code == KeyCode::Slash {
                            self.chat_input.push_char('/');
                        }
                        self.ui.open_chat();
                        self.tab_held = false;
                        self.set_grab(false);
                    } else if code == KeyCode::KeyE && pressed && self.ui.accepts_gameplay_input() {
                        self.sim.input.release_all();
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
                        self.sim.input.set(action, pressed);
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
            self.sim.input.add_mouse(delta.0 as f32, delta.1 as f32);
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
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

    let mut sim = Sim::new(config);
    let mut render = RenderState::new(device, queue, format, w, h, sim.vanilla_atlas());

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
        let p = sim.player.position;
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
    let sky = [135i32, 181, 235];
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
