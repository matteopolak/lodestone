//! The windowless, GPU-less **simulation**: the generated world, the player
//! driven by the real physics engine, the off-thread mesh scheduler, and the
//! optional live connection. Keeping this free of winit and wgpu is what lets
//! the interesting logic — stepping, meshing, camera derivation — be unit tested
//! headlessly, with the windowed layer in [`crate::app`] staying a thin driver.

use std::time::Instant;

use lodestone_client::{ClientAction, OpenMenuSnapshot};
use lodestone_controller::{InputState, apply_look, move_action, movement_intent};
use lodestone_game::menu::Menu;
use lodestone_physics::{MovementInput, PhysicsProfile, PlayerState, Vec3d, tick};
use lodestone_render::Camera;
use lodestone_world::{ChunkPos, World};

use crate::audio::ShellAudio;
use crate::blocks::id;
use crate::camera_rig::build_camera;
use crate::chat::{ChatLog, compose_chat_action};
use crate::collision::WorldCollision;
use crate::config::Config;
use crate::entities::{EntityDraw, EntityInterpolator};
use crate::hud::{DebugStats, process_rss_bytes};
use crate::mesher::{MeshScheduler, Meshed, SectionKey, snapshot_section};
use crate::net::{NetClient, NetUpdate};
use crate::overlay::{BossBarView, Sidebar};
use crate::raycast::{REACH, RayHit, raycast};
use crate::worldgen;

/// Fixed physics timestep: 20 ticks per second, like vanilla.
const TICK_DT: f64 = 1.0 / 20.0;
/// Cap how far worldgen spans regardless of render distance, so start-up meshing
/// stays snappy for the demo.
const MAX_WORLD_RADIUS: i32 = 6;
/// Horizontal free-fly speed in blocks per tick (sprint doubles it). The physics
/// engine models no creative/spectator flight, so fly is a shell-side free-cam.
const FLY_SPEED: f64 = 0.45;
/// Block placed by right-click interaction (the demo palette has no inventory).
const PLACE_BLOCK: u32 = id::STONE;
/// Number of hotbar slots (vanilla is a fixed 9).
const HOTBAR_SLOTS: usize = 9;

/// The whole non-graphical game state.
#[derive(Debug)]
pub struct Sim {
    /// Parsed configuration.
    pub config: Config,
    /// The world being rendered (locally generated for now).
    pub world: World,
    /// The player, advanced by the bit-exact physics engine.
    pub player: PlayerState,
    /// Held keys + accumulated mouse motion.
    pub input: InputState,
    /// Latest debug stats (the app fills in FPS/frame-time/GPU counters).
    pub stats: DebugStats,
    /// The block the view ray is currently pointing at (for outline + edits).
    pub target: Option<RayHit>,
    profile: PhysicsProfile,
    scheduler: MeshScheduler,
    net: Option<NetClient>,
    accumulator: f64,
    last_step: Instant,
    status: String,
    /// Player feet position at the start of the most recent physics tick, used
    /// to interpolate the camera between fixed ticks.
    prev_position: Vec3d,
    /// Fractional progress `[0,1)` from the last tick toward the next.
    interp_alpha: f32,
    /// Total physics ticks run since start.
    tick_count: u64,
    /// Total frames (calls to [`Sim::step`]) since start.
    frame_count: u64,
    /// Whether free-fly (noclip) is active instead of physics-walk.
    fly: bool,
    /// Sections whose geometry vanished (all-air after an edit) and must be
    /// dropped from the GPU. Drained by the app each frame.
    pending_removals: Vec<SectionKey>,
    /// Coarse lifecycle of the live connection, driven by [`NetUpdate`]s. The
    /// app maps this onto the menu state machine (Connecting → ready on
    /// [`SessionPhase::Connected`], → failed on [`SessionPhase::Ended`]).
    phase: SessionPhase,
    /// Received chat/system lines (bounded scrollback), rendered by the HUD.
    chat_log: ChatLog,
    /// Folded server tab-list state; rendered while Tab is held.
    tab_list: lodestone_game::tablist::TabList,
    /// Folded server scoreboard state; rendered as the right-edge sidebar.
    scoreboard: lodestone_game::scoreboard::Scoreboard,
    /// The local player's active status effects, folded from `update_mob_effect`
    /// / `remove_mob_effect` and ticked down at 20 Hz; drawn by [`crate::effects`]
    /// as the top-right HUD stack. Distinct from [`PlayerState::effects`], which
    /// is the *physics* view (only motion-relevant effects); this is the full,
    /// display-oriented set with durations and levels.
    hud_effects: lodestone_game::effect::ActiveEffects,
    /// Title/subtitle overlay, folded through the canonical
    /// [`lodestone_game::player_state::TitleState`] and ticked at 20 Hz for the
    /// vanilla fade. Empty (drawing nothing) until the server sends a title.
    title: lodestone_game::player_state::TitleState,
    /// Action-bar overlay (GameInfo messages), folded through
    /// [`lodestone_game::player_state::ActionBar`]; self-clears after 60 ticks.
    action_bar: lodestone_game::player_state::ActionBar,
    /// Monotonic wall-clock seconds since the sim started, accumulated from the
    /// real per-frame `dt` in [`Sim::step`]. Stamps chat arrivals so the HUD can
    /// age lines for the vanilla fade-out without reaching for a clock itself.
    clock_secs: f64,
    /// Latest server-reported health in `0..=20`, `None` until the server sends
    /// one (i.e. on the local dev world it stays `None` and no bar is drawn).
    health: Option<f32>,
    /// Latest server-reported food level in `0..=20`, `None` until reported.
    food: Option<i32>,
    /// Latest server-reported experience (progress toward next level, level,
    /// total points), `None` until `set_experience` arrives. The HUD must not
    /// substitute a locally-derived guess for this — there is no vanilla
    /// leveling curve the shell could invert from partial data that would be
    /// guaranteed to match the (possibly modded) server's own numbers.
    experience: Option<(f32, i32, i32)>,
    /// Per-entity interpolation, smoothing the 20 Hz snapshot stream into the
    /// render-rate transforms the entity pass draws. Empty off a live server.
    entity_interp: EntityInterpolator,
    /// Selected hotbar slot in `0..9`. Owned locally: the selected slot is an
    /// input the player drives (number keys / scroll), echoed to the server via
    /// [`ClientAction::SetCarriedItem`]. Defaults to slot 0, matching vanilla.
    selected_slot: usize,
    /// Live audio, or `None` when disabled (no asset root, no device — see
    /// [`ShellAudio::from_env`]). The whole audio path is `if let Some`, so a
    /// disabled engine is simply silent, never a crash.
    audio: Option<ShellAudio>,
    /// The server-assigned entity id for the local player, set on
    /// [`NetUpdate::LoggedIn`]. `None` off a live server (or before login
    /// completes), in which case entity-scoped `NetUpdate`s that need to
    /// distinguish "this is us" (e.g. mob effects) are not the local player's
    /// and are ignored rather than misattributed.
    local_entity_id: Option<i32>,
}

/// The coarse phase of the shell's session, distilled from [`NetUpdate`]s so the
/// app can drive the [`crate::menu`] state machine without re-reading net wire
/// details. Purely a read-model: it never affects physics or rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPhase {
    /// No live connection — the local dev world (worldgen stand-in).
    LocalOnly,
    /// A live connection is attached and still handshaking / logging in.
    Connecting,
    /// Logged in to the server.
    Connected,
    /// The session ended; carries the human-readable reason (disconnect,
    /// net error, or death). Terminal until a new connection is attached.
    Ended(String),
}

impl Sim {
    /// Build the simulation: generate the world, place the player, and schedule
    /// every non-empty section for meshing.
    #[must_use]
    pub fn new(config: Config) -> Self {
        let radius = (config.render_distance as i32).clamp(1, MAX_WORLD_RADIUS);
        let world = worldgen::generate(radius);

        let feet = worldgen::spawn_feet();
        let mut player = PlayerState::at(Vec3d::new(feet[0], feet[1], feet[2]), 180.0);
        player.pitch = 10.0;

        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(2);
        let mut scheduler = MeshScheduler::new(workers);

        // Schedule every section that holds geometry.
        for (pos, chunk) in world_sections(&world) {
            for si in 0..chunk {
                let key = SectionKey {
                    cx: pos.0,
                    cz: pos.1,
                    si,
                    min_y: worldgen::MIN_Y,
                };
                if let Some(snap) = snapshot_section(&world, key) {
                    scheduler.submit(snap);
                }
            }
        }

        let mut stats = DebugStats {
            status: "local world".into(),
            ..Default::default()
        };
        stats.chunk_count = world.len();

        Self {
            config,
            world,
            player,
            input: InputState::default(),
            stats,
            target: None,
            profile: PhysicsProfile::mc_1_21(),
            scheduler,
            net: None,
            accumulator: 0.0,
            last_step: Instant::now(),
            status: "local world".into(),
            prev_position: player.position,
            interp_alpha: 0.0,
            tick_count: 0,
            frame_count: 0,
            fly: false,
            pending_removals: Vec::new(),
            phase: SessionPhase::LocalOnly,
            chat_log: ChatLog::new(),
            tab_list: lodestone_game::tablist::TabList::new(),
            scoreboard: lodestone_game::scoreboard::Scoreboard::new(),
            hud_effects: lodestone_game::effect::ActiveEffects::new(),
            title: lodestone_game::player_state::TitleState::new(),
            action_bar: lodestone_game::player_state::ActionBar::new(),
            clock_secs: 0.0,
            health: None,
            food: None,
            experience: None,
            entity_interp: EntityInterpolator::new(),
            selected_slot: 0,
            audio: ShellAudio::from_env(),
            local_entity_id: None,
        }
    }

    /// Attach a live connection whose updates are polled each frame.
    pub fn attach_net(&mut self, net: NetClient) {
        self.net = Some(net);
        self.status = "connecting…".into();
        self.phase = SessionPhase::Connecting;
    }

    /// The coarse session phase, for the menu state machine.
    #[must_use]
    pub fn session_phase(&self) -> &SessionPhase {
        &self.phase
    }

    /// The most recent chat/system lines (oldest-first) for the HUD to draw,
    /// each paired with its **age in seconds** (now − arrival) so the HUD can
    /// apply the vanilla fade-out. Lines carry legacy `§` colour codes.
    #[must_use]
    pub fn recent_chat(&self, n: usize) -> Vec<(String, f32)> {
        let now = self.clock_secs;
        self.chat_log
            .recent(n)
            .into_iter()
            .map(|(line, at)| (line, (now - at).max(0.0) as f32))
            .collect()
    }

    /// Server-reported health in `0..=20`, or `None` off a live survival server.
    #[must_use]
    pub fn health(&self) -> Option<f32> {
        self.health
    }

    /// Server-reported food level in `0..=20`, or `None` off a live server.
    #[must_use]
    pub fn food(&self) -> Option<i32> {
        self.food
    }

    /// Server-reported experience as `(progress, level, total)`, or `None`
    /// before `set_experience` has arrived (e.g. the local dev world, or a
    /// live server before the first packet). `progress` is `0.0..1.0` toward
    /// the next level.
    #[must_use]
    pub fn experience(&self) -> Option<(f32, i32, i32)> {
        self.experience
    }

    /// The current tab-list, formatted as `NAME  <latency>ms` rows sorted by
    /// vanilla display order. Empty until the server sends player-list data.
    #[must_use]
    pub fn player_rows(&self) -> Vec<String> {
        crate::tablist::player_rows(&self.tab_list)
    }

    /// The scoreboard sidebar to draw, or `None` when none is displayed (or off
    /// a live server). Folded through [`lodestone_game::scoreboard::Scoreboard`].
    #[must_use]
    pub fn sidebar(&self) -> Option<Sidebar> {
        crate::scoreboard::sidebar_from(&self.scoreboard)
    }

    /// The active boss bars to draw, in render order. Empty off a live server.
    #[must_use]
    pub fn boss_bars(&self) -> Vec<BossBarView> {
        self.net
            .as_ref()
            .map_or_else(Vec::new, NetClient::boss_bars)
    }

    /// The XP bar to draw as `(level, progress 0..=1)`, `Some` only once the
    /// server has sent an experience update. Reads the already-folded
    /// [`Sim::experience`]; off a live server it stays `None` and no bar draws.
    #[must_use]
    pub fn xp(&self) -> Option<(i32, f32)> {
        self.experience
            .map(|(progress, level, _total)| (level, progress))
    }

    /// The title/subtitle overlay as `(title, subtitle, alpha)`, `Some` while a
    /// server-sent title is visible. `Text` is flattened to a legacy `§` string
    /// at read time, matching the chat path, so colour survives once decoded.
    #[must_use]
    pub fn title_overlay(&self) -> Option<(String, Option<String>, f32)> {
        let title = self.title.title()?;
        Some((
            title.to_legacy_string(),
            self.title.subtitle().map(|s| s.to_legacy_string()),
            self.title.alpha(),
        ))
    }

    /// The action-bar message as `(text, alpha)`, `Some` while a GameInfo
    /// message is visible (fades over its final ticks).
    #[must_use]
    pub fn action_bar_overlay(&self) -> Option<(String, f32)> {
        let text = self.action_bar.text()?;
        Some((text.to_legacy_string(), self.action_bar.alpha()))
    }

    /// The local player's active status effects, for the top-right HUD overlay.
    /// Empty until a server applies one; ticked down in [`Sim::step`].
    #[must_use]
    pub fn active_effects(&self) -> &lodestone_game::effect::ActiveEffects {
        &self.hud_effects
    }

    /// The folded player inventory menu. Off a live connection this returns an
    /// empty player menu so the local inventory screen can still render.
    #[must_use]
    pub fn player_menu(&self) -> Menu {
        self.net
            .as_ref()
            .and_then(NetClient::player_menu)
            .unwrap_or_else(Menu::player)
    }

    /// The currently open server menu, if any.
    #[must_use]
    pub fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        self.net.as_ref().and_then(NetClient::open_menu)
    }

    /// Best-effort close request for the open server menu.
    pub fn close_open_menu(&self) {
        let Some(open) = self.open_menu() else { return };
        if let Some(net) = &self.net {
            net.send_action(ClientAction::ContainerClose {
                window_id: open.window_id,
            });
        }
    }

    /// Compose a typed chat line onto the outbound [`ClientAction`] seam and hand
    /// it to the live client (a leading `/` is a command, else a chat message).
    /// A blank line sends nothing. No-op without a live connection. Returns
    /// whether anything was sent, so the caller can echo command feedback.
    pub fn send_chat(&self, line: &str) -> bool {
        let Some(action) = compose_chat_action(line) else {
            return false;
        };
        if let Some(net) = &self.net {
            net.send_action(action);
            true
        } else {
            false
        }
    }

    /// The currently selected hotbar slot, `0..9`.
    #[must_use]
    pub fn selected_slot(&self) -> usize {
        self.selected_slot
    }

    /// Select hotbar slot `slot` (`0..9`); out-of-range values are ignored. When
    /// the selection actually changes, echoes it to the server via
    /// [`ClientAction::SetCarriedItem`] so the held item stays in sync. No-op
    /// off a live connection beyond updating the local selection the HUD draws.
    pub fn select_slot(&mut self, slot: usize) {
        if slot >= HOTBAR_SLOTS || slot == self.selected_slot {
            return;
        }
        self.selected_slot = slot;
        self.send_selected_slot();
    }

    /// Advance the hotbar selection by `delta` slots, wrapping at both ends
    /// (mouse-wheel behaviour). A positive `delta` moves right, matching vanilla
    /// scroll-down.
    pub fn cycle_slot(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let n = HOTBAR_SLOTS as i32;
        let next = (self.selected_slot as i32 + delta).rem_euclid(n) as usize;
        self.select_slot(next);
    }

    /// Push the current selection to the server. Best-effort: no-op without a
    /// live connection, and a closed session just drops it.
    fn send_selected_slot(&self) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetCarriedItem {
                slot: self.selected_slot as i32,
            });
        }
    }

    /// Number of meshing jobs still outstanding.
    #[must_use]
    pub fn pending_meshes(&self) -> usize {
        self.scheduler.pending()
    }

    /// Collect finished meshes for the caller to upload to the GPU.
    pub fn drain_meshes(&mut self) -> Vec<Meshed> {
        self.scheduler.drain()
    }

    /// Block until every scheduled mesh is ready (used by headless runs/tests).
    pub fn drain_all_meshes(&mut self) -> Vec<Meshed> {
        let n = self.scheduler.pending();
        self.scheduler.drain_blocking(n)
    }

    /// Sections that became empty (drained by the app to remove GPU meshes).
    pub fn drain_removals(&mut self) -> Vec<SectionKey> {
        std::mem::take(&mut self.pending_removals)
    }

    /// Whether free-fly mode is active.
    #[must_use]
    pub fn flying(&self) -> bool {
        self.fly
    }

    /// Toggle free-fly (noclip) mode. Entering fly zeroes velocity so the player
    /// doesn't keep any fall momentum.
    pub fn toggle_fly(&mut self) {
        self.fly = !self.fly;
        self.player.velocity = Vec3d::ZERO;
        self.player.on_ground = false;
    }

    /// Frames rendered per physics tick since start (fixed-timestep health).
    #[must_use]
    pub fn frames_per_tick(&self) -> f32 {
        if self.tick_count == 0 {
            0.0
        } else {
            self.frame_count as f32 / self.tick_count as f32
        }
    }

    /// Apply accumulated mouse motion to the view angles.
    pub fn apply_mouse(&mut self) {
        let (dx, dy) = self.input.take_mouse();
        if dx != 0.0 || dy != 0.0 {
            let (yaw, pitch) = apply_look(
                self.player.yaw,
                self.player.pitch,
                dx,
                dy,
                self.config.sensitivity,
            );
            self.player.yaw = yaw;
            self.player.pitch = pitch;
        }
    }

    /// Advance the simulation by real elapsed time, running fixed 20 Hz physics
    /// ticks through the real engine against the world's collision. Rendering
    /// interpolates between ticks via [`Sim::interp_alpha`].
    pub fn step(&mut self, dt: f64) {
        self.apply_mouse();
        self.clock_secs += dt.max(0.0);
        self.accumulator += dt.clamp(0.0, 0.25);

        let intent = movement_intent(&self.input);
        while self.accumulator >= TICK_DT {
            self.prev_position = self.player.position;
            if self.fly {
                self.fly_tick(intent);
            } else {
                self.physics_tick(intent);
            }
            self.tick_count += 1;
            self.accumulator -= TICK_DT;
            // Age the HUD status effects at the same fixed 20 Hz the server ticks
            // them, so displayed timers count down in step with the world.
            self.hud_effects.tick(1);
            // Age the title/subtitle and action-bar overlays at the same 20 Hz
            // so their vanilla fades run in step with the world.
            self.title.tick(1);
            self.action_bar.tick(1);
            // Vanilla emits a movement packet every tick (20 Hz); mirror that so
            // the server sees our authoritative position/rotation and never has
            // to correct us. Only once we're actually in the world — before the
            // server places us the adapter (correctly) has no Play-state packet
            // for a Move, so sending earlier just produces dropped-action noise.
            // Best-effort — a closed session just drops it.
            if self.phase == SessionPhase::Connected
                && let Some(net) = &self.net
            {
                net.send_action(move_action(&self.player));
            }
        }
        self.interp_alpha = (self.accumulator / TICK_DT) as f32;
        self.frame_count += 1;

        self.poll_net();
        self.update_entities(dt as f32);
        self.refresh_stats();
    }

    /// Fold this frame's entity snapshots into the interpolator so
    /// [`entity_draws`](Self::entity_draws) yields smooth per-frame transforms.
    /// No live connection means no entities.
    fn update_entities(&mut self, dt: f32) {
        let snapshots = self
            .net
            .as_ref()
            .map_or_else(Vec::new, NetClient::entity_snapshots);
        self.entity_interp.update(&snapshots, dt);
    }

    /// The interpolated entities to draw this frame, resolved by the renderer
    /// into instanced draws. Empty off a live server.
    #[must_use]
    pub fn entity_draws(&self) -> Vec<EntityDraw> {
        self.entity_interp.draws()
    }

    /// One fixed physics tick through the real engine.
    ///
    /// The `MOVEMENT_SPEED` attribute is injected each tick via
    /// [`PlayerState::with_movement_speed`] — exercising the attribute seam the
    /// physics crate exposes from a *real* caller, not a test. When sprinting we
    /// hand in `base·(1 + sprint_modifier)`; the engine then ignores its own
    /// sprint speed maths (no double-count) while the sprint flag still drives
    /// the sprint jump boost.
    fn physics_tick(&mut self, intent: MovementInput) {
        let base = f64::from(self.profile.base_movement_speed);
        let attr = if intent.sprint {
            base * (1.0 + f64::from(self.profile.sprint_speed_modifier))
        } else {
            base
        };
        self.player = self.player.with_movement_speed(attr);
        let view = WorldCollision::new(&self.world);
        tick(&mut self.player, intent, &view, &self.profile);
    }

    /// One free-fly tick: move horizontally relative to yaw, vertically with
    /// jump/sneak, ignoring gravity and collision. This is a shell-side camera,
    /// not a physics model — the engine has no flight (see the report).
    fn fly_tick(&mut self, intent: MovementInput) {
        let speed = if self.input.sprint_held() {
            FLY_SPEED * 2.0
        } else {
            FLY_SPEED
        };
        let yaw = f64::from(self.player.yaw).to_radians();
        let (sy, cy) = yaw.sin_cos();
        let f = f64::from(intent.forward);
        let s = f64::from(intent.strafe);
        // vanilla getInputVector with pitch ignored: horizontal move only.
        let mut dx = s * cy - f * sy;
        let mut dz = f * cy + s * sy;
        let len = (dx * dx + dz * dz).sqrt();
        if len > 1.0 {
            dx /= len;
            dz /= len;
        }
        self.player.position.x += dx * speed;
        self.player.position.z += dz * speed;
        if intent.jump {
            self.player.position.y += speed;
        }
        if intent.sneak {
            self.player.position.y -= speed;
        }
        self.player.velocity = Vec3d::ZERO;
        self.player.on_ground = false;
    }

    /// Convenience wrapper using the wall clock since the last call.
    pub fn step_realtime(&mut self) -> f64 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f64();
        self.last_step = now;
        self.step(dt);
        dt
    }

    /// Recompute the targeted block by casting the view ray from the (already
    /// interpolated) camera. Call once per frame before rendering the outline.
    pub fn update_target(&mut self, aspect: f32) {
        let cam = self.camera(aspect);
        let origin = [
            f64::from(cam.position.x),
            f64::from(cam.position.y),
            f64::from(cam.position.z),
        ];
        let fwd = cam.forward();
        let dir = [f64::from(fwd.x), f64::from(fwd.y), f64::from(fwd.z)];
        let view = WorldCollision::new(&self.world);
        self.target = raycast(origin, dir, REACH, |x, y, z| view.is_solid(x, y, z));
    }

    /// Break the currently targeted block (set it to air) and remesh. Returns
    /// whether a block was broken.
    pub fn break_block(&mut self) -> bool {
        let Some(hit) = self.target else { return false };
        if self.set_block_world(hit.block, id::AIR) {
            self.remesh_around(hit.block);
            self.target = None;
            true
        } else {
            false
        }
    }

    /// Place [`PLACE_BLOCK`] against the targeted face, if the cell is empty and
    /// doesn't intersect the player. Returns whether a block was placed.
    pub fn place_block(&mut self) -> bool {
        let Some(hit) = self.target else { return false };
        let pos = hit.place_position();
        let cell_empty = {
            let view = WorldCollision::new(&self.world);
            view.block_at(pos[0], pos[1], pos[2]) == id::AIR
        };
        if !cell_empty || self.block_intersects_player(pos) {
            return false;
        }
        if self.set_block_world(pos, PLACE_BLOCK) {
            self.remesh_around(pos);
            true
        } else {
            false
        }
    }

    fn block_intersects_player(&self, block: [i32; 3]) -> bool {
        let bb = self.player.bounding_box(&self.profile);
        let (x0, y0, z0) = (
            f64::from(block[0]),
            f64::from(block[1]),
            f64::from(block[2]),
        );
        bb.max_x > x0
            && bb.min_x < x0 + 1.0
            && bb.max_y > y0
            && bb.min_y < y0 + 1.0
            && bb.max_z > z0
            && bb.min_z < z0 + 1.0
    }

    fn set_block_world(&mut self, block: [i32; 3], value: u32) -> bool {
        let pos = ChunkPos {
            x: block[0].div_euclid(16),
            z: block[2].div_euclid(16),
        };
        let Some(chunk) = self.world.get_mut(pos) else {
            return false;
        };
        let col = &mut chunk.column;
        if block[1] < col.min_y() || block[1] >= col.max_y() {
            return false;
        }
        col.set_block(
            block[0].rem_euclid(16) as usize,
            block[1],
            block[2].rem_euclid(16) as usize,
            value,
        );
        true
    }

    /// Re-snapshot and re-schedule the section holding `block`, plus any
    /// neighbour section that shares the boundary the block sits on (a face on a
    /// section edge changes the neighbour's mesh via culling/AO). Sections that
    /// became all-air are queued for GPU removal instead.
    fn remesh_around(&mut self, block: [i32; 3]) {
        let cx = block[0].div_euclid(16);
        let cz = block[2].div_euclid(16);
        let lx = block[0].rem_euclid(16);
        let lz = block[2].rem_euclid(16);
        let si = (block[1] - worldgen::MIN_Y).div_euclid(16);
        let ly = (block[1] - worldgen::MIN_Y).rem_euclid(16);

        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx == -1 && lx != 0) || (dx == 1 && lx != 15) {
                        continue;
                    }
                    if (dy == -1 && ly != 0) || (dy == 1 && ly != 15) {
                        continue;
                    }
                    if (dz == -1 && lz != 0) || (dz == 1 && lz != 15) {
                        continue;
                    }
                    let nsi = si + dy;
                    if nsi < 0 {
                        continue;
                    }
                    let key = SectionKey {
                        cx: cx + dx,
                        cz: cz + dz,
                        si: nsi as usize,
                        min_y: worldgen::MIN_Y,
                    };
                    match snapshot_section(&self.world, key) {
                        Some(snap) => self.scheduler.submit(snap),
                        None => self.pending_removals.push(key),
                    }
                }
            }
        }
    }

    /// Handle a `ChunkLoaded` / [`NetUpdate::Chunk`] dirty-region signal: the
    /// world at column `(cx, cz)` changed, so re-mesh every section it holds.
    /// Reads the column's own `min_y`/`section_count`, so it is correct for the
    /// locally generated world.
    ///
    /// It does **not** yet re-mesh from the *live* client world. The section
    /// source now exists ([`crate::net::NetClient::sections_at`]), but placing
    /// those sections needs column geometry (`min_y`/`section_count`) the client
    /// handle does not expose, and lighting them needs a bulk light read it also
    /// does not expose — both reported upstream. The multi-section meshing loop
    /// that consumes `sections_at` is `impl-render`'s to own; this remains a
    /// no-op for live columns until that lands, rather than duplicating it here.
    fn mark_column_dirty(&mut self, cx: i32, cz: i32) {
        let Some(chunk) = self.world.get(ChunkPos { x: cx, z: cz }) else {
            return;
        };
        let min_y = chunk.column.min_y();
        let count = chunk.column.section_count();
        for si in 0..count {
            let key = SectionKey { cx, cz, si, min_y };
            match snapshot_section(&self.world, key) {
                Some(snap) => self.scheduler.submit(snap),
                None => self.pending_removals.push(key),
            }
        }
    }

    fn poll_net(&mut self) {
        // Collect owned updates first so the immutable borrow of `self.net`
        // ends before the loop — the sound arms need `&mut self.audio` and (for
        // entity sounds) a fresh read of `self.net` for positions, neither of
        // which can coexist with a borrow held across the loop.
        let updates = match &self.net {
            Some(net) => net.poll(),
            None => return,
        };
        for update in updates {
            match update {
                NetUpdate::Connecting => {
                    self.status = "connecting…".into();
                    self.phase = SessionPhase::Connecting;
                }
                NetUpdate::LoggedIn { entity_id } => {
                    self.status = format!("connected (entity {entity_id})");
                    self.phase = SessionPhase::Connected;
                    self.local_entity_id = Some(entity_id);
                }
                NetUpdate::Chunk { x, z } => {
                    // §12.24 dirty-region signal: no block data travels on the
                    // event — the client applies decoded chunks to its own
                    // `World`, which we now read via `NetClient::sections_at`.
                    // Re-meshing the *live* column additionally needs the column
                    // geometry + light seams (see `mark_column_dirty`); until
                    // those land and `impl-render`'s multi-section loop consumes
                    // them, this only re-meshes locally generated columns.
                    self.mark_column_dirty(x, z);
                }
                NetUpdate::Chat { text, player } => {
                    tracing::info!(target: "chat", "{}", text.to_legacy_string());
                    if player {
                        self.chat_log.push_player(
                            text,
                            lodestone_game::chat::MessageTrust::NotSecure,
                            self.clock_secs,
                        );
                    } else {
                        self.chat_log.push_system(text, self.clock_secs);
                    }
                }
                NetUpdate::Health { health, food } => {
                    self.health = Some(health);
                    self.food = Some(food);
                    if health <= 0.0 {
                        self.status = "server: player dead (no chunks)".into();
                    }
                }
                NetUpdate::Experience {
                    progress,
                    level,
                    total,
                } => {
                    self.experience = Some((progress, level, total));
                }
                NetUpdate::Death => {
                    self.status = "server: died".into();
                    self.phase = SessionPhase::Ended("player died".into());
                }
                NetUpdate::Sound {
                    name,
                    category,
                    pos,
                    volume,
                    pitch,
                    seed,
                } => {
                    if let Some(audio) = &mut self.audio {
                        let pos = glam::Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                        audio.play_sound(&name, category, pos, volume, pitch, seed);
                    }
                }
                NetUpdate::EntitySound {
                    name,
                    category,
                    entity_id,
                    volume,
                    pitch,
                    seed,
                } => {
                    // Resolve the entity's live position *before* borrowing the
                    // audio engine mutably (disjoint, sequential borrows).
                    let pos = self.entity_sound_position(entity_id);
                    if let Some(audio) = &mut self.audio {
                        audio.play_entity_sound(&name, category, pos, volume, pitch, seed);
                    }
                }
                // Only the local player's effects are folded: they feed both the
                // physics view ([`PlayerState::effects`]) and the display view
                // ([`Sim::hud_effects`]). Entity-scoped effects are filtered here
                // rather than in `net::forward`, keeping the wire event
                // entity-agnostic.
                NetUpdate::EffectApplied {
                    entity_id,
                    effect,
                    amplifier,
                    duration_ticks,
                    ambient,
                    show_icon,
                } => {
                    if self.local_entity_id == Some(entity_id) {
                        self.player.effects.apply(&effect, amplifier);
                        if let Ok(id) =
                            lodestone_model::Identifier::new("minecraft", effect.as_str())
                        {
                            self.hud_effects
                                .apply(lodestone_game::effect::StatusEffect {
                                    id,
                                    amplifier: u8::try_from(amplifier).unwrap_or(u8::MAX),
                                    duration_ticks,
                                    ambient,
                                    show_particles: true,
                                    show_icon,
                                });
                        }
                    }
                }
                NetUpdate::EffectRemoved { entity_id, effect } => {
                    if self.local_entity_id == Some(entity_id) {
                        self.player.effects.remove(&effect);
                        if let Ok(id) =
                            lodestone_model::Identifier::new("minecraft", effect.as_str())
                        {
                            self.hud_effects.remove(&id);
                        }
                    }
                }
                NetUpdate::TabListEvent(event) => {
                    let _ = self.tab_list.apply(&event);
                }
                NetUpdate::ScoreboardEvent(event) => {
                    let _ = self.scoreboard.apply(&event);
                }
                NetUpdate::TitleEvent(event) => {
                    let _ = self.title.apply(&event);
                }
                NetUpdate::ActionBar(text) => {
                    self.action_bar.set(text);
                }
                NetUpdate::Disconnected(reason) => {
                    self.status = format!("disconnected: {reason}");
                    self.phase = SessionPhase::Ended(format!("disconnected: {reason}"));
                }
                NetUpdate::Error(e) => {
                    self.status = format!("net error: {e}");
                    self.phase = SessionPhase::Ended(format!("net error: {e}"));
                }
            }
        }
    }

    /// World-space origin for an entity-attached sound: the entity's live feet
    /// position raised half a block so the source sits at body centre. Falls
    /// back to the player's current position if the entity is unknown (so the
    /// sound is still heard rather than dropped) — the same "audible, not
    /// silent" preference the live gate encodes.
    fn entity_sound_position(&self, entity_id: i32) -> glam::Vec3 {
        if let Some(net) = &self.net
            && let Some(snap) = net
                .entity_snapshots()
                .into_iter()
                .find(|s| s.id == entity_id)
        {
            return snap.feet + glam::Vec3::new(0.0, 0.5, 0.0);
        }
        let p = self.player.position;
        glam::Vec3::new(p.x as f32, p.y as f32, p.z as f32)
    }

    /// Push the listener transform to the audio engine from the render camera.
    /// Called once per frame by [`crate::app`] with the exact interpolated
    /// camera it renders, so what the player hears matches what they see.
    pub fn set_audio_listener(&self, camera: &Camera) {
        if let Some(audio) = &self.audio {
            audio.set_listener(camera);
        }
    }

    fn refresh_stats(&mut self) {
        self.stats.position = [
            self.player.position.x,
            self.player.position.y,
            self.player.position.z,
        ];
        self.stats.yaw = self.player.yaw;
        self.stats.pitch = self.player.pitch;
        self.stats.chunk_count = self.world.len();
        self.stats.live_columns = self.net.as_ref().map_or(0, |n| n.loaded_chunks().len());
        self.stats.world_bytes = self.world.heap_bytes();
        self.stats.rss_bytes = process_rss_bytes();
        self.stats.frames_per_tick = self.frames_per_tick();
        self.stats.flying = self.fly;
        self.stats.target = self.target.map(|h| h.block);
        self.stats.status = self.status.clone();
    }

    /// Build the render camera for the given viewport aspect ratio, with the
    /// feet position interpolated between the last two physics ticks so motion
    /// stays smooth even though physics runs at a fixed 20 Hz. View angles are
    /// current (mouse-look is per-frame, matching vanilla).
    #[must_use]
    pub fn camera(&self, aspect: f32) -> Camera {
        let a = f64::from(self.interp_alpha);
        let mut interp = self.player;
        interp.position = Vec3d::new(
            self.prev_position.x + (self.player.position.x - self.prev_position.x) * a,
            self.prev_position.y + (self.player.position.y - self.prev_position.y) * a,
            self.prev_position.z + (self.player.position.z - self.prev_position.z) * a,
        );
        build_camera(&interp, aspect, self.config.render_distance)
    }
}

/// Enumerate `(chunk-x, chunk-z)` and section count for every loaded column.
fn world_sections(world: &World) -> Vec<((i32, i32), usize)> {
    let radius = MAX_WORLD_RADIUS;
    let mut out = Vec::new();
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            if let Some(chunk) = world.get(lodestone_world::ChunkPos { x: cx, z: cz }) {
                out.push(((cx, cz), chunk.column.section_count()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Mode};

    fn test_config() -> Config {
        Config {
            mode: Mode::Headless,
            render_distance: 2,
            ..Config::default()
        }
    }

    #[test]
    fn new_generates_world_and_schedules_meshes() {
        let sim = Sim::new(test_config());
        assert!(!sim.world.is_empty(), "world should have chunks");
        assert!(sim.pending_meshes() > 0, "sections should be scheduled");
    }

    #[test]
    fn all_scheduled_sections_mesh() {
        let mut sim = Sim::new(test_config());
        let meshes = sim.drain_all_meshes();
        assert!(!meshes.is_empty());
        assert!(meshes.iter().any(|m| m.mesh.quad_count() > 0));
    }

    #[test]
    fn stepping_settles_the_player_on_the_ground() {
        let mut sim = Sim::new(test_config());
        for _ in 0..60 {
            sim.step(1.0 / 20.0);
        }
        assert!(sim.player.on_ground, "player should be standing on terrain");
        assert_eq!(sim.stats.position[1], sim.player.position.y);
    }

    #[test]
    fn mouse_look_updates_view_and_clears_delta() {
        let mut sim = Sim::new(test_config());
        let yaw0 = sim.player.yaw;
        sim.input.add_mouse(50.0, 0.0);
        sim.apply_mouse();
        assert_ne!(sim.player.yaw, yaw0);
        assert_eq!(sim.input.mouse_dx, 0.0);
    }

    #[test]
    fn connected_sim_emits_one_move_per_physics_tick() {
        use crate::net::NetUpdate;
        let (net, actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // Before login the adapter has no Play-state Move packet, so the shell
        // must not spew movement yet: drive to Connected first.
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net(); // → Connected
        assert_eq!(*sim.session_phase(), SessionPhase::Connected);
        sim.step(5.0 / 20.0); // ~5 ticks, all now in-world.
        let sent = std::iter::from_fn(|| actions.try_recv().ok()).count();
        assert!(sent > 0, "a connected sim should send movement packets");
        assert_eq!(
            sent as u64, sim.tick_count,
            "exactly one outbound Move per physics tick"
        );
    }

    #[test]
    fn move_is_withheld_until_connected() {
        // A sim that is merely Connecting (attached, not yet logged in) must send
        // nothing — otherwise every pre-Play tick is a dropped-action on the wire.
        let (net, actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        assert_eq!(*sim.session_phase(), SessionPhase::Connecting);
        sim.step(5.0 / 20.0);
        assert!(sim.tick_count > 0, "ticks must still run while connecting");
        let sent = std::iter::from_fn(|| actions.try_recv().ok()).count();
        assert_eq!(sent, 0, "no movement should be sent before login");
    }

    #[test]
    fn disconnected_sim_sends_nothing() {
        // Without a net attached, stepping must not attempt to send.
        let mut sim = Sim::new(test_config());
        sim.step(5.0 / 20.0);
        assert!(sim.net.is_none());
    }

    #[test]
    fn mob_effect_applied_for_local_player_reaches_status_effects() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
        sim.poll_net();
        assert!(sim.player.effects.levitation.is_none());

        feed.send(NetUpdate::EffectApplied {
            entity_id: 7,
            effect: "levitation".into(),
            amplifier: 2,
            duration_ticks: 200,
            ambient: false,
            show_icon: true,
        })
        .unwrap();
        sim.poll_net();
        assert_eq!(
            sim.player.effects.levitation,
            Some(2),
            "the wire→StatusEffects seam must fold an effect for the local entity id"
        );
        // The same event must also reach the display model with its full data.
        let chips = crate::effects::chips_from(sim.active_effects());
        assert_eq!(chips.len(), 1, "the HUD effect model must fold it too");
        assert_eq!(chips[0].label, "levitation III"); // amplifier 2 → level III
        assert_eq!(chips[0].time, "0:10"); // 200 ticks → 10 s

        feed.send(NetUpdate::EffectRemoved {
            entity_id: 7,
            effect: "levitation".into(),
        })
        .unwrap();
        sim.poll_net();
        assert!(sim.player.effects.levitation.is_none());
        assert!(
            sim.active_effects().is_empty(),
            "removal must clear the HUD effect model as well"
        );
    }

    #[test]
    fn mob_effect_for_a_different_entity_is_not_applied_to_the_local_player() {
        use crate::net::NetUpdate;
        // `update_mob_effect` is entity-agnostic on the wire; only the entity id
        // that matches the local player's should ever mutate `sim.player`.
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
        sim.poll_net();

        feed.send(NetUpdate::EffectApplied {
            entity_id: 1234, // some other (mob) entity, not the local player
            effect: "levitation".into(),
            amplifier: 0,
            duration_ticks: 200,
            ambient: false,
            show_icon: true,
        })
        .unwrap();
        sim.poll_net();
        assert!(
            sim.player.effects.levitation.is_none(),
            "a remote entity's effect must not leak into the local player's StatusEffects"
        );
        assert!(
            sim.active_effects().is_empty(),
            "a remote entity's effect must not reach the local HUD overlay either"
        );
    }

    #[test]
    fn session_phase_tracks_net_updates() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        // Before any connection: purely local.
        assert_eq!(*sim.session_phase(), SessionPhase::LocalOnly);

        // Attaching a live connection moves us to Connecting immediately, so the
        // menu shows a loading screen rather than a lie.
        sim.attach_net(net);
        assert_eq!(*sim.session_phase(), SessionPhase::Connecting);

        // LoggedIn ⇒ Connected (the menu's "session_ready").
        feed.send(NetUpdate::LoggedIn { entity_id: 42 }).unwrap();
        sim.poll_net();
        assert_eq!(*sim.session_phase(), SessionPhase::Connected);

        // A mid-game disconnect ⇒ Ended with the reason preserved, which is what
        // drives the menu's Error screen. Assert the reason survives, so a
        // blank/again-Connected mapping can't pass.
        feed.send(NetUpdate::Disconnected("Server closed".into()))
            .unwrap();
        sim.poll_net();
        match sim.session_phase() {
            SessionPhase::Ended(reason) => {
                assert!(reason.contains("Server closed"), "reason lost: {reason}");
            }
            other => panic!("expected Ended, got {other:?}"),
        }
    }

    #[test]
    fn session_phase_reports_net_error_as_ended() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::Error("connection refused".into()))
            .unwrap();
        sim.poll_net();
        match sim.session_phase() {
            SessionPhase::Ended(reason) => {
                assert!(reason.contains("connection refused"), "got {reason}");
            }
            other => panic!("expected Ended, got {other:?}"),
        }
    }

    #[test]
    fn inbound_chat_is_logged_and_typed_lines_route_to_the_action_seam() {
        use crate::net::NetUpdate;
        use lodestone_client::ClientAction;
        let (net, actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        // Inbound server chat must surface in the HUD log (not merely logged).
        feed.send(NetUpdate::Chat {
            text: lodestone_model::Text::literal("hello world"),
            player: false,
        })
        .unwrap();
        sim.poll_net();
        let lines: Vec<String> = sim.recent_chat(10).into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            lines,
            vec!["hello world".to_string()],
            "inbound chat must reach the display log"
        );

        // Typed lines route through the one outbound action seam: a leading '/'
        // is a command (slash stripped), otherwise a chat message.
        assert!(sim.send_chat("/say hi"), "a command line must send");
        assert!(sim.send_chat("plain message"), "a chat line must send");
        // Anti-vacuity: a blank line must send *nothing*, so "everything sends"
        // can't pass — and neither can "nothing sends", guarded by the two above.
        assert!(!sim.send_chat("   "), "blank input must not send");

        let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert_eq!(
            sent,
            vec![
                ClientAction::SendCommand {
                    command: "say hi".into()
                },
                ClientAction::SendChat {
                    text: "plain message".into()
                },
            ],
            "exactly the two non-blank lines route, with the command slash stripped"
        );
    }

    #[test]
    fn chat_lines_age_as_the_clock_advances() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        feed.send(NetUpdate::Chat {
            text: lodestone_model::Text::literal("aged line"),
            player: false,
        })
        .unwrap();
        sim.poll_net();
        // Freshly received: age is ~0.
        assert!(
            sim.recent_chat(1)[0].1 < 0.001,
            "a just-received line is young"
        );

        // Advancing the sim clock ages the line by real elapsed time.
        sim.step(2.5);
        let age = sim.recent_chat(1)[0].1;
        assert!(
            (2.4..=2.6).contains(&age),
            "line age must track the sim clock, got {age}"
        );
    }

    #[test]
    fn server_health_and_food_reach_the_hud_accessors() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // Off a live server there is no survival state, so the HUD draws no bars.
        assert_eq!(sim.health(), None);
        assert_eq!(sim.food(), None);

        feed.send(NetUpdate::Health {
            health: 14.0,
            food: 17,
        })
        .unwrap();
        sim.poll_net();
        // Both fields must land — a one-sided store would leave the other None.
        assert_eq!(sim.health(), Some(14.0));
        assert_eq!(sim.food(), Some(17));
    }

    #[test]
    fn server_experience_reaches_the_hud_accessor() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // Off a live server (or before the first packet) there is no real XP
        // value, so the HUD must not draw a faked bar.
        assert_eq!(sim.experience(), None);

        feed.send(NetUpdate::Experience {
            progress: 0.6,
            level: 30,
            total: 1395,
        })
        .unwrap();
        sim.poll_net();
        assert_eq!(sim.experience(), Some((0.6, 30, 1395)));
    }

    #[test]
    fn title_events_fold_into_the_title_overlay() {
        use crate::net::NetUpdate;
        use lodestone_model::{ClientEvent, Text};

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // No title yet → nothing to draw.
        assert!(sim.title_overlay().is_none());

        feed.send(NetUpdate::TitleEvent(ClientEvent::TitleText {
            text: Text::literal("Welcome"),
        }))
        .unwrap();
        feed.send(NetUpdate::TitleEvent(ClientEvent::SubtitleText {
            text: Text::literal("to the server"),
        }))
        .unwrap();
        sim.poll_net();

        let (title, subtitle, _alpha) = sim
            .title_overlay()
            .expect("a server-sent title must reach the HUD accessor");
        assert_eq!(title, "Welcome");
        assert_eq!(subtitle.as_deref(), Some("to the server"));

        // A clear packet must empty the overlay again.
        feed.send(NetUpdate::TitleEvent(ClientEvent::TitlesCleared {
            reset_times: false,
        }))
        .unwrap();
        sim.poll_net();
        assert!(sim.title_overlay().is_none());
    }

    #[test]
    fn game_info_chat_folds_into_the_action_bar_not_the_feed() {
        use crate::net::NetUpdate;
        use lodestone_model::Text;

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        assert!(sim.action_bar_overlay().is_none());

        feed.send(NetUpdate::ActionBar(Text::literal("Boss incoming")))
            .unwrap();
        sim.poll_net();

        let (text, alpha) = sim
            .action_bar_overlay()
            .expect("a GameInfo message must reach the action-bar accessor");
        assert_eq!(text, "Boss incoming");
        assert!(alpha > 0.0, "a fresh action-bar message is fully opaque");
        // It must not have leaked into the chat scrollback.
        assert!(
            sim.recent_chat(10).is_empty(),
            "GameInfo is the action bar, not chat — it must not enter the feed"
        );
    }

    #[test]
    fn player_list_events_fold_into_tab_overlay_rows() {
        use crate::net::NetUpdate;
        use lodestone_model::{ClientEvent, GameMode, PlayerListEntry, Text};
        use uuid::Uuid;

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        let alice = Uuid::from_u128(1);
        let bob = Uuid::from_u128(2);
        feed.send(NetUpdate::TabListEvent(ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: bob,
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Spectator),
                    latency: Some(30),
                    display_name: None,
                    listed: Some(true),
                },
                PlayerListEntry {
                    uuid: alice,
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(12),
                    display_name: Some(Text::literal("Alice the Brave")),
                    listed: Some(true),
                },
            ],
        }))
        .unwrap();
        sim.poll_net();

        assert_eq!(
            sim.player_rows(),
            vec!["Alice the Brave  12ms".to_string(), "Bob  30ms".to_string(),],
            "tab overlay rows must come from lodestone-game's folded TabList state"
        );

        feed.send(NetUpdate::TabListEvent(ClientEvent::PlayerListRemove {
            profile_ids: vec![alice],
        }))
        .unwrap();
        sim.poll_net();
        assert_eq!(sim.player_rows(), vec!["Bob  30ms".to_string()]);
    }

    #[test]
    fn scoreboard_events_fold_into_sidebar_view() {
        use crate::net::NetUpdate;
        use lodestone_model::event::{DisplaySlot, ObjectiveMode, ObjectiveRenderType};
        use lodestone_model::{ClientEvent, Text};

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        for event in [
            ClientEvent::ObjectiveUpdate {
                name: "kills".into(),
                mode: ObjectiveMode::Add,
                display_name: Some(Text::literal("Kills")),
                render_type: Some(ObjectiveRenderType::Integer),
                number_format: None,
            },
            ClientEvent::DisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective: Some("kills".into()),
            },
            ClientEvent::ScoreUpdate {
                holder: "Alice".into(),
                objective: "kills".into(),
                value: 7,
                display: Some(Text::literal("Alice the Brave")),
                number_format: None,
            },
            ClientEvent::ScoreUpdate {
                holder: "Bob".into(),
                objective: "kills".into(),
                value: 3,
                display: None,
                number_format: None,
            },
        ] {
            feed.send(NetUpdate::ScoreboardEvent(event)).unwrap();
        }
        sim.poll_net();

        let sidebar = sim.sidebar().expect("sidebar objective should be visible");
        assert_eq!(sidebar.title, "Kills");
        let rows: Vec<(&str, &str)> = sidebar
            .lines
            .iter()
            .map(|line| (line.label.as_str(), line.score.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![("Alice the Brave", "7"), ("Bob", "3")],
            "sidebar rows must come from lodestone-game's folded Scoreboard state"
        );
    }

    #[test]
    fn hotbar_selection_updates_and_echoes_to_the_server() {
        use lodestone_client::ClientAction;
        let (net, actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        // Vanilla default is slot 0, and selecting it again is a no-op (no
        // redundant packet).
        assert_eq!(sim.selected_slot(), 0);
        sim.select_slot(0);

        // A direct selection moves and echoes exactly one SetCarriedItem.
        sim.select_slot(3);
        assert_eq!(sim.selected_slot(), 3);

        // Out-of-range is ignored (no 10th slot), leaving selection and the
        // wire untouched.
        sim.select_slot(9);
        assert_eq!(sim.selected_slot(), 3);

        // Scroll wraps at both ends: +1 from 3 → 4, and from 8 → 0.
        sim.cycle_slot(1);
        assert_eq!(sim.selected_slot(), 4);
        sim.select_slot(8);
        sim.cycle_slot(1);
        assert_eq!(
            sim.selected_slot(),
            0,
            "scroll past the last slot wraps to 0"
        );
        sim.cycle_slot(-1);
        assert_eq!(
            sim.selected_slot(),
            8,
            "scroll before the first slot wraps to 8"
        );

        let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        // Every *change* echoes SetCarriedItem; the no-op select_slot(0) and the
        // rejected select_slot(9) send nothing, so the wire shows only the moves.
        assert_eq!(
            sent,
            vec![
                ClientAction::SetCarriedItem { slot: 3 },
                ClientAction::SetCarriedItem { slot: 4 },
                ClientAction::SetCarriedItem { slot: 8 },
                ClientAction::SetCarriedItem { slot: 0 },
                ClientAction::SetCarriedItem { slot: 8 },
            ],
            "only real selection changes reach the outbound action seam"
        );
    }

    #[test]
    fn camera_interpolates_between_ticks() {
        // Force a known prev/current split and a half-way alpha, then check the
        // camera eye sits between the two feet positions.
        let mut sim = Sim::new(test_config());
        sim.prev_position = Vec3d::new(0.0, 64.0, 0.0);
        sim.player.position = Vec3d::new(10.0, 64.0, 0.0);
        sim.interp_alpha = 0.5;
        let cam = sim.camera(1.0);
        assert!(
            (cam.position.x - 5.0).abs() < 1e-4,
            "expected midpoint x=5, got {}",
            cam.position.x
        );
    }

    #[test]
    fn frames_per_tick_tracks_ratio() {
        let mut sim = Sim::new(test_config());
        // Two frames of one full tick each ⇒ 2 frames / 2 ticks = 1.0.
        sim.step(1.0 / 20.0);
        sim.step(1.0 / 20.0);
        assert!((sim.frames_per_tick() - 1.0).abs() < 1e-6);
        // A frame with no accumulated tick still counts as a frame, so the
        // frames-per-tick ratio rises above 1.
        sim.step(0.0);
        assert!(sim.frames_per_tick() > 1.0, "extra frame raises the ratio");
    }

    #[test]
    fn sprint_moves_faster_than_walk_via_attribute_seam() {
        // Walk forward for a second, then sprint the same time from the same
        // spot; sprinting must cover more ground. This drives the physics
        // `with_movement_speed` seam from a real caller.
        //
        // The local world is now real vanilla terrain (`lodestone-worldgen`),
        // so spawn sits on a slope and walking north walls the player out after
        // ~0.2 blocks — a wall, not the speed seam, would otherwise decide the
        // result. Flatten a private corridor along the walking line so what we
        // measure is physics speed and nothing else.
        fn distance(sprint: bool) -> f64 {
            let mut sim = Sim::new(test_config());
            // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180).
            // Lay a solid floor and clear head-room along -Z so the walk is
            // unobstructed regardless of the generated surface.
            let feet_y = sim.player.position.y.floor() as i32;
            for dz in -25..=1 {
                for dx in -1..=1 {
                    sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                    sim.set_block_world([dx, feet_y, dz], id::AIR);
                    sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
                    sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
                }
            }
            // Settle on the fresh floor first.
            for _ in 0..20 {
                sim.step(1.0 / 20.0);
            }
            let start = sim.player.position;
            sim.input.set(lodestone_controller::Action::Forward, true);
            sim.input.set(lodestone_controller::Action::Sprint, sprint);
            for _ in 0..20 {
                sim.step(1.0 / 20.0);
            }
            let d = sim.player.position.subtract(start);
            (d.x * d.x + d.z * d.z).sqrt()
        }
        let walk = distance(false);
        let sprint = distance(true);
        assert!(
            sprint > walk * 1.1,
            "sprint ({sprint:.3}) should clearly exceed walk ({walk:.3})"
        );
    }

    #[test]
    fn breaking_the_target_clears_it_and_schedules_a_remesh() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        // Aim straight down at the block under the player's feet.
        let feet = sim.player.position;
        sim.target = Some(crate::raycast::RayHit {
            block: [
                feet.x.floor() as i32,
                feet.y.floor() as i32 - 1,
                feet.z.floor() as i32,
            ],
            normal: [0, 1, 0],
        });
        assert!(sim.break_block(), "should break the solid block");
        assert!(sim.target.is_none(), "target cleared after break");
        assert!(sim.pending_meshes() > 0, "a remesh was scheduled");
    }

    #[test]
    fn chunk_dirty_signal_reschedules_a_loaded_column() {
        // A `ChunkLoaded`/`NetUpdate::Chunk { x, z }` signal must re-mesh the
        // column it names (the §12.24 dirty-region trigger), so the live-world
        // swap is a source change, not new plumbing.
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        assert_eq!(sim.pending_meshes(), 0, "drained to a clean slate");
        let (pos, _) = sim.world.iter().next().expect("local world has a column");
        let (cx, cz) = (pos.x, pos.z);
        sim.mark_column_dirty(cx, cz);
        assert!(
            sim.pending_meshes() > 0,
            "the loaded column was re-scheduled"
        );
    }

    #[test]
    fn chunk_dirty_signal_ignores_an_absent_column() {
        // Columns we don't hold (e.g. before the live world source is wired in)
        // must be a no-op, never a panic or spurious work.
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        sim.mark_column_dirty(9999, 9999);
        assert_eq!(sim.pending_meshes(), 0, "absent column schedules nothing");
    }

    #[test]
    fn placing_against_a_face_adds_a_block() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        let feet = sim.player.position;
        // Target a floor block a few blocks away (clear of the player AABB),
        // place on its top face.
        let bx = feet.x.floor() as i32 + 3;
        let bz = feet.z.floor() as i32;
        let s = crate::worldgen::surface_height(bx, bz);
        sim.target = Some(crate::raycast::RayHit {
            block: [bx, s, bz],
            normal: [0, 1, 0],
        });
        {
            let view = WorldCollision::new(&sim.world);
            assert_eq!(view.block_at(bx, s + 1, bz), id::AIR, "cell starts empty");
        }
        assert!(sim.place_block(), "should place onto the top face");
        let view = WorldCollision::new(&sim.world);
        assert_ne!(view.block_at(bx, s + 1, bz), id::AIR, "block now present");
    }

    #[test]
    fn cannot_place_inside_the_player() {
        let mut sim = Sim::new(test_config());
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let feet = sim.player.position;
        // Target the block under the feet, whose top face is where the player
        // stands — placing there would clip the player, so it must be refused.
        sim.target = Some(crate::raycast::RayHit {
            block: [
                feet.x.floor() as i32,
                feet.y.floor() as i32 - 1,
                feet.z.floor() as i32,
            ],
            normal: [0, 1, 0],
        });
        assert!(!sim.place_block(), "placing inside the player is refused");
    }

    #[test]
    fn fly_mode_ignores_gravity() {
        let mut sim = Sim::new(test_config());
        sim.toggle_fly();
        assert!(sim.flying());
        let y0 = sim.player.position.y;
        // No vertical input: fly holds altitude (physics-walk would fall).
        for _ in 0..40 {
            sim.step(1.0 / 20.0);
        }
        assert!(
            (sim.player.position.y - y0).abs() < 1e-9,
            "fly holds altitude"
        );
        // Jump ascends.
        sim.input.set(lodestone_controller::Action::Jump, true);
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        assert!(sim.player.position.y > y0, "jump lifts in fly mode");
    }
}
