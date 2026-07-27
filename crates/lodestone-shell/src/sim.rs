//! The windowless, GPU-less **simulation**: the generated world, the player
//! driven by the real physics engine, the off-thread mesh scheduler, and the
//! optional live connection. Keeping this free of winit and wgpu is what lets
//! the interesting logic — stepping, meshing, camera derivation — be unit tested
//! headlessly, with the windowed layer in [`crate::app`] staying a thin driver.

use std::time::Instant;

use lodestone_controller::{InputState, apply_look, move_action, movement_intent};
use lodestone_physics::{MovementInput, PhysicsProfile, PlayerState, Vec3d, tick};
use lodestone_render::Camera;
use lodestone_world::{ChunkPos, World};

use crate::blocks::id;
use crate::camera_rig::build_camera;
use crate::chat::{ChatLog, compose_chat_action};
use crate::collision::WorldCollision;
use crate::overlay::{BossBarView, Sidebar};
use crate::config::Config;
use crate::entities::{EntityDraw, EntityInterpolator};
use crate::hud::{DebugStats, process_rss_bytes};
use crate::mesher::{MeshScheduler, Meshed, SectionKey, snapshot_section};
use crate::net::{NetClient, NetUpdate};
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
    /// Latest server-reported health in `0..=20`, `None` until the server sends
    /// one (i.e. on the local dev world it stays `None` and no bar is drawn).
    health: Option<f32>,
    /// Latest server-reported food level in `0..=20`, `None` until reported.
    food: Option<i32>,
    /// Per-entity interpolation, smoothing the 20 Hz snapshot stream into the
    /// render-rate transforms the entity pass draws. Empty off a live server.
    entity_interp: EntityInterpolator,
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
            health: None,
            food: None,
            entity_interp: EntityInterpolator::new(),
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

    /// The most recent chat/system lines (oldest-first) for the HUD to draw.
    #[must_use]
    pub fn recent_chat(&self, n: usize) -> Vec<&str> {
        self.chat_log.recent(n)
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

    /// The current tab-list, formatted as `NAME  <latency>ms` rows sorted by
    /// name. Empty off a live server. Reads the client-owned player list through
    /// the net handle each call (cheap; only invoked while Tab is held).
    #[must_use]
    pub fn player_rows(&self) -> Vec<String> {
        let Some(net) = &self.net else {
            return Vec::new();
        };
        let mut rows: Vec<String> = net
            .players()
            .into_iter()
            .map(|p| {
                let name = p.name.unwrap_or_else(|| "?".to_string());
                match p.latency {
                    Some(ms) if ms >= 0 => format!("{name}  {ms}ms"),
                    _ => format!("{name}  --"),
                }
            })
            .collect();
        rows.sort();
        rows
    }

    /// The scoreboard sidebar to draw, or `None` when none is displayed (or off
    /// a live server). Folded from the client snapshot through [`crate::overlay`].
    #[must_use]
    pub fn sidebar(&self) -> Option<Sidebar> {
        self.net.as_ref().and_then(NetClient::sidebar)
    }

    /// The active boss bars to draw, in render order. Empty off a live server.
    #[must_use]
    pub fn boss_bars(&self) -> Vec<BossBarView> {
        self.net.as_ref().map_or_else(Vec::new, NetClient::boss_bars)
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
        let Some(net) = &self.net else { return };
        for update in net.poll() {
            match update {
                NetUpdate::Connecting => {
                    self.status = "connecting…".into();
                    self.phase = SessionPhase::Connecting;
                }
                NetUpdate::LoggedIn { entity_id } => {
                    self.status = format!("connected (entity {entity_id})");
                    self.phase = SessionPhase::Connected;
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
                NetUpdate::Chat(msg) => {
                    tracing::info!(target: "chat", "{msg}");
                    self.chat_log.push(msg);
                }
                NetUpdate::Health { health, food } => {
                    self.health = Some(health);
                    self.food = Some(food);
                    if health <= 0.0 {
                        self.status = "server: player dead (no chunks)".into();
                    }
                }
                NetUpdate::Death => {
                    self.status = "server: died".into();
                    self.phase = SessionPhase::Ended("player died".into());
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
        feed.send(NetUpdate::Chat("hello world".into())).unwrap();
        sim.poll_net();
        assert_eq!(
            sim.recent_chat(10),
            vec!["hello world"],
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
        // Walk forward for a second on flat ground, then sprint the same time
        // from the same spot; sprinting must cover more ground. This drives the
        // physics `with_movement_speed` seam from a real caller.
        fn distance(sprint: bool) -> f64 {
            let mut sim = Sim::new(test_config());
            // Settle on the ground first.
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
