//! Live-server networking, kept strictly behind `lodestone-client`'s public API.
//!
//! The shell selects a version by **protocol number** through
//! [`lodestone_registry::adapter_for_protocol`] and otherwise deals only in the
//! version-free `lodestone-model` types re-exported by the client. It never
//! names a packet, a version, or a version crate. TCP is reached only via
//! [`ClientBuilder::connect`], preserving the [`Transport`] seam a future wasm
//! build needs.
//!
//! The client is async (tokio); the shell's render loop is not. So a background
//! thread owns a current-thread runtime and the [`ClientHandle`]/`EventStream`,
//! and forwards decoded events as [`NetUpdate`]s down a synchronous channel the
//! app drains once per frame.
//!
//! ## What actually arrives (measured, see the report)
//!
//! With the `v770` family compiled in, this connects and logs in against the
//! live 26.2 server, and `Login`/`KeepAlive`/`Chat`/`Disconnect` surface as
//! events. **`ChunkLoaded` carries only a [`ChunkPos`] — no block data — and
//! per the §12.24 ruling it stays that way**: it is a "region dirty at `pos`,
//! re-mesh it" signal, not a payload. World state must never be reconstructible
//! only from a bounded/lossy event stream, so the decoded blocks live in a
//! `World` owned by the client; consumers *query* that world rather than
//! accumulate it from events.
//!
//! ## Outbound (S6)
//!
//! Movement flows the other way through the same thread: the sim queues a
//! [`ClientAction::Move`] every 20 Hz tick onto an `mpsc` sender; the net loop
//! drains it each iteration and hands it to [`ClientHandle::send_action`], which
//! the version adapter lowers into the concrete movement packet. The shell never
//! names that packet.
//!
//! **Seam status (verified 2026-07-27):** the v770 adapter now has a `Move`
//! encode arm (→ `move_player_pos_rot`) and a `SwingArm` arm (→ `swing`), so the
//! `Move`s the controller queues each tick now reach the wire instead of being
//! dropped as `Ok(None)`. As a side-effect the client's read-model records our
//! own outgoing position (a local echo), so `ClientHandle::position()` returns
//! `Some` once we start moving. Two things remain out of *this* crate's hands:
//! whether the server accepts our physics without a corrective teleport is
//! `impl-physics`'s live gate (its negative control), and server-authoritative
//! reconciliation arrives as [`ClientEvent::TeleportPlayer`], which the shell
//! will consume alongside the live-world swap.
//!
//! ## Reading the client-owned world (the section-source seam)
//!
//! The read path now exists: [`ClientHandle::sections_at`] hands back owned
//! `Arc<ChunkSection>` snapshots for a batch of `(ChunkPos, section_index)`
//! requests under a single lock acquisition, and [`ClientHandle::loaded_chunks`]
//! enumerates which columns are resident. The net thread owns the `ClientHandle`
//! but publishes an `Arc` clone of it into a shared [`OnceLock`] the moment the
//! session is up, so the render/mesh thread can pull a live 27-section
//! neighbourhood out of the client's world without touching tokio and without
//! blocking the net loop. [`NetClient::sections_at`] / [`NetClient::loaded_chunks`]
//! / [`NetClient::server_position`] are that surface; before login they return
//! empty, never panic.
//!
//! **Both seams that used to be missing now exist** (landed by `impl-client`,
//! verified 2026-07-29): [`ClientHandle::sections_and_light_at`] reads a whole
//! neighbourhood's blocks *and* light under one lock, and
//! [`ClientHandle::world_dimensions`] hands back the column geometry
//! (`min_y` / `height`) needed to place streamed sections at their true
//! world-`y`. [`NetClient::sections_and_light_at`] / [`NetClient::world_dimensions`]
//! wrap them here, ready for a live mesher to consume.
//!
//! **Live terrain now renders** (landed 2026-07-29, commits `93a2c1e` +
//! `f5800d9`). The last blocker was never a client seam — it was the
//! *classifier*: the shell used to mesh with [`crate::blocks::DemoClassifier`],
//! whose palette is a hand-built 10-id demo namespace ([`crate::blocks::id`]),
//! while a live 26.2 server streams *vanilla* block-state ids (tens of
//! thousands). Everything outside those 10 ids classified to non-occluding air,
//! so the live world meshed to nothing — and, critically, **any lighting gate
//! over it would have passed vacuously**, because an empty world is trivially
//! not full-bright.
//!
//! [`crate::resources::BlockResources::load`] now builds a vanilla
//! `state_id → sprite` classifier from `blocks_json_registry` + `BlockAtlas`,
//! and `mark_column_dirty` (sim.rs) meshes live columns through it. Two
//! invariants that are easy to "fix" into bugs:
//!
//! - **MP consumes server light; SP computes it.** Do not run
//!   `compute_column_light` on live columns — `merge_light` already carries the
//!   server's seam-complete cross-chunk light, and recomputing replaces
//!   authoritative values with a partial result.
//! - **Light section indexing is off-by-one by design**: light section `i`
//!   covers block section `i−1` (26 light sections for 24 block sections), which
//!   is why [`NetClient::sections_and_light_at`] takes an explicit `(n, n+1)`.
//!
//! If the vanilla pack is missing, `load` falls back to the demo palette and
//! logs a banner naming the fix rather than silently rendering an empty world.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::JoinHandle;
use std::time::Duration;

use lodestone_client::{
    BlockPos, ChunkPos, ChunkSection, ClientAction, ClientBuilder, ClientEvent, ClientHandle,
    EntityView, LoginProfile, OpenMenuSnapshot, PlayerListEntry, Rotation, SectionLight,
    ServerAddress, Vec3, WorldDimensions,
};
use lodestone_game::menu::Menu;
use lodestone_model::Vec3f;
use lodestone_model::event::SoundCategory;

pub use lodestone_testsupport::unique_username;

use crate::entities::EntitySnapshot;
use crate::overlay::{BossBarView, Sidebar, boss_bars_from, sidebar_from};

/// A handle to the live client, published by the net thread once the session is
/// up and read by the render/mesh thread. `None` until login completes.
type SharedHandle = Arc<OnceLock<Arc<ClientHandle>>>;

/// A decoded, version-free update the app can act on without touching tokio.
#[derive(Debug, Clone)]
pub enum NetUpdate {
    /// The background task is attempting to connect.
    Connecting,
    /// Login completed; the local player entity id.
    LoggedIn {
        /// Server-assigned entity id for the local player.
        entity_id: i32,
    },
    /// A chat/system message as a version-free [`lodestone_model::Text`]
    /// component — **not** pre-flattened, so its colour and formatting survive
    /// for the shell to fold into the canonical [`lodestone_game::chat::ChatFeed`]
    /// (colour reaches pixels once the adapter preserves it). Translation keys
    /// are already resolved through the model's built-in table. `player` marks a
    /// signed/player chat message (fed as a `Player` entry) versus a system or
    /// game-info message (fed as `System`).
    Chat {
        /// The message component.
        text: lodestone_model::Text,
        /// Whether this is player chat (vs system/game-info).
        player: bool,
    },
    /// A chunk became dirty at this position: the server sent (and the client
    /// applied to its world) chunk data here, so any mesh covering this column
    /// should be rebuilt. Block data is *not* carried — it is queried from the
    /// client-owned world per the §12.24 ruling (see the module docs).
    Chunk {
        /// Chunk X.
        x: i32,
        /// Chunk Z.
        z: i32,
    },
    /// Blocks changed inside one already-loaded section (a break, a place,
    /// another player's edits). The client has applied them to its world;
    /// `blocks` carries only the section-relative coordinates, so a consumer can
    /// re-mesh this section and only the neighbours a boundary cell touches.
    /// Block data is *not* carried — it is queried from the client-owned world
    /// per the §12.24 ruling.
    SectionBlocks {
        /// Section X (block >> 4).
        x: i32,
        /// Section Y (block >> 4).
        y: i32,
        /// Section Z (block >> 4).
        z: i32,
        /// Section-relative `(x, y, z)`, each `0..16`, of every changed cell.
        blocks: Vec<[u8; 3]>,
    },
    /// The server reported a block being destroyed at `pos`, carrying the state
    /// id it had **before** breaking.
    ///
    /// This is vanilla's `LevelEvent.PARTICLES_DESTROY_BLOCK` (2001), whose
    /// payload is a block state id (`Block.stateById(data)` in
    /// `LevelEventHandler`). It is the authoritative signal that a block broke:
    /// the client cannot derive it from `BLOCK_UPDATE`, because by the time that
    /// arrives the cell is already air and the texture the debris needs is gone.
    BlockDestroyed {
        /// Block position that broke.
        pos: lodestone_model::BlockPos,
        /// The block state id the cell held before breaking.
        state: u32,
    },
    /// The server asked for a burst of particles at a world position
    /// (`LEVEL_PARTICLES`) — vanilla's general particle-effect packet, as
    /// opposed to the `LevelEvent` 2001 shortcut [`Self::BlockDestroyed`]
    /// covers for the one case that has its own code path. `kind` is the
    /// particle type's namespace-stripped path (e.g. `"flame"`), matching the
    /// [`NetUpdate::Sound`] convention. See
    /// [`crate::particles::Particles::spawn_particles`] for what
    /// `offset`/`max_speed`/`count` actually mean — vanilla overloads
    /// `count == 0` to mean something other than "spawn nothing".
    Particles {
        /// Particle type, namespace stripped (e.g. `"flame"`, `"smoke"`).
        kind: String,
        /// Whether the particle renders past vanilla's 32-block distance
        /// cutoff (`ClientLevel.doAddParticle`'s `overrideLimiter`, `1024.0`
        /// being `32.0` squared).
        long_distance: bool,
        /// World-space origin.
        pos: Vec3,
        /// Randomized per-axis offset bound when `count > 0`, or a raw
        /// velocity direction when `count == 0` — see
        /// `Particles::spawn_particles`.
        offset: Vec3f,
        /// Speed parameter; scales initial velocity.
        max_speed: f32,
        /// Number of particles to spawn. `0` is vanilla's special case for
        /// exactly one particle with a non-randomized velocity.
        count: i32,
    },
    /// Player health/food changed.
    Health {
        /// Current health in `0..=20`.
        health: f32,
        /// Current food level in `0..=20`.
        food: i32,
    },
    /// The player died. A transient state, not the end of the session: the
    /// client library auto-respawns, and [`NetUpdate::Respawned`] follows.
    Death,
    /// The server confirmed a respawn (post-death, dimension change, or
    /// `/respawn`). The fresh position arrives in the placement
    /// [`NetUpdate::Teleport`] that follows.
    Respawned,
    /// Player experience changed (`set_experience`): progress toward the next
    /// level, the level itself, and total accumulated points. The HUD's XP bar
    /// must draw these real numbers, not a locally-faked value — there is no
    /// vanilla formula the shell could derive them from that would match the
    /// server's own (possibly modded) leveling curve.
    Experience {
        /// Progress toward the next level, in `0.0..1.0`.
        progress: f32,
        /// Current experience level.
        level: i32,
        /// Total accumulated experience points.
        total: i32,
    },
    /// A positioned sound to play (`SOUND` packet). `name` is the sound event
    /// key's path (namespace stripped, e.g. `"entity.slime.squish"`); `seed` is
    /// the server-rolled value that makes weighted variant selection
    /// deterministic across clients. `category` is the source bus.
    Sound {
        /// Sound event key path (namespace stripped).
        name: String,
        /// Source bus (master/blocks/hostile/…).
        category: SoundCategory,
        /// World-space origin.
        pos: Vec3,
        /// Packet volume multiplier.
        volume: f32,
        /// Packet pitch multiplier.
        pitch: f32,
        /// Server RNG seed for variant selection.
        seed: i64,
    },
    /// An entity-attached sound (`SOUND_ENTITY` packet). The origin is resolved
    /// from `entity_id`'s live position when the sound is played.
    EntitySound {
        /// Sound event key path (namespace stripped).
        name: String,
        /// Source bus.
        category: SoundCategory,
        /// Entity the sound is attached to.
        entity_id: i32,
        /// Packet volume multiplier.
        volume: f32,
        /// Packet pitch multiplier.
        pitch: f32,
        /// Server RNG seed for variant selection.
        seed: i64,
    },
    /// A mob effect (potion effect) was applied to or refreshed on an entity
    /// (`update_mob_effect`). Carries `entity_id` unfiltered — the packet
    /// applies to any entity, not just the local player — so the sim decides
    /// whether it is the locally-tracked player before folding it into
    /// [`lodestone_physics::PlayerState::effects`].
    EffectApplied {
        /// Entity the effect applies to.
        entity_id: i32,
        /// Canonical effect id, namespace stripped (e.g. `"speed"`), matching
        /// the [`NetUpdate::Sound`] convention.
        effect: String,
        /// Effect amplifier (0 = level I).
        amplifier: u32,
        /// Remaining duration in ticks; `-1` means infinite.
        duration_ticks: i32,
        /// Whether the effect is ambient (beacon/aura source): the HUD draws it
        /// fainter.
        ambient: bool,
        /// Whether the effect shows a HUD icon at all.
        show_icon: bool,
    },
    /// A mob effect was removed from an entity (`remove_mob_effect`).
    EffectRemoved {
        /// Entity the effect was removed from.
        entity_id: i32,
        /// Canonical effect id, namespace stripped.
        effect: String,
    },
    /// A tab-list delta for the shell-owned [`lodestone_game::tablist::TabList`]
    /// fold.
    TabListEvent(ClientEvent),
    /// A scoreboard delta for the shell-owned
    /// [`lodestone_game::scoreboard::Scoreboard`] fold.
    ScoreboardEvent(ClientEvent),
    /// A title/subtitle delta for the shell-owned
    /// [`lodestone_game::player_state::TitleState`] fold.
    TitleEvent(ClientEvent),
    /// An action-bar (GameInfo) message for the shell-owned
    /// [`lodestone_game::player_state::ActionBar`] fold.
    ActionBar(lodestone_model::Text),
    /// The session ended (clean or with a reason).
    Disconnected(String),
    /// A transport or setup error.
    Error(String),
    /// The server placed or relocated the player (`TeleportPlayer`): the
    /// authoritative position/rotation the shell's camera must adopt. The shell
    /// runs its own physics and streams an optimistic position every tick, so on
    /// a server whose spawn is far from the origin the first thing that reaches
    /// the wire is a bogus "I'm at my demo spawn" claim; the server ignores it and
    /// keeps us at the real spawn, streaming chunks *there*. Without consuming this
    /// event the camera is stranded at the demo spawn while the world renders
    /// hundreds of blocks away — the "standing on invisible blocks" bug. `flags`
    /// marks any component that is a *delta* from the current pose rather than
    /// absolute; the shell resolves them against its own camera state.
    Teleport {
        /// Target position, or per-axis delta where `flags` marks it relative.
        pos: Vec3,
        /// Target rotation, or per-component delta where `flags` marks it relative.
        rotation: Rotation,
        /// Which components of `pos`/`rotation` are relative to the current pose.
        flags: lodestone_model::event::TeleportFlags,
    },
}

/// A live client running on a background thread. Drop to request shutdown.
#[derive(Debug)]
pub struct NetClient {
    rx: Receiver<NetUpdate>,
    /// Outbound actions (movement, swings, chat) queued for the net thread to
    /// hand to the client. Kept off the render thread; the net loop drains it.
    action_tx: Sender<ClientAction>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Published by the net thread once login completes; lets the render/mesh
    /// thread read the client-owned world lock-free of tokio.
    handle: SharedHandle,
}

impl NetClient {
    /// Spawn a background thread that connects to `host:port` speaking the given
    /// protocol number and forwards events. Returns immediately.
    #[must_use]
    pub fn connect(host: String, port: u16, protocol: i32) -> Self {
        let (tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle: SharedHandle = Arc::new(OnceLock::new());
        let handle_thread = Arc::clone(&handle);

        let thread = std::thread::Builder::new()
            .name("lodestone-net".into())
            .spawn(move || {
                run(
                    host,
                    port,
                    protocol,
                    tx,
                    action_rx,
                    stop_thread,
                    handle_thread,
                )
            })
            .expect("spawn net thread");

        Self {
            rx,
            action_tx,
            stop,
            thread: Some(thread),
            handle,
        }
    }

    /// Drain all updates received since the last poll (non-blocking).
    #[must_use]
    pub fn poll(&self) -> Vec<NetUpdate> {
        let mut out = Vec::new();
        while let Ok(u) = self.rx.try_recv() {
            out.push(u);
        }
        out
    }

    /// Queue an outbound action for the net thread to submit through the client
    /// handle. Best-effort: if the session has ended the send is silently
    /// dropped (the shell keeps rendering regardless).
    pub fn send_action(&self, action: ClientAction) {
        let _ = self.action_tx.send(action);
    }

    /// Read the single block state at a world position from the client-owned
    /// world, or `None` when that column/section is not held (before login, or
    /// outside the loaded region). A cheap one-position read used by the live
    /// dig loop to notice a block that has already become air.
    #[must_use]
    pub fn block_at(&self, pos: BlockPos) -> Option<u32> {
        self.handle.get().and_then(|h| h.block_at(pos))
    }

    /// Batch-read owned section snapshots from the client-owned world, one lock
    /// acquisition for the whole request. Empty (all `None`) before login or for
    /// columns/sections the client doesn't hold — never blocks, never panics.
    ///
    /// This is the section-source seam the render/mesh layer consumes: it hands
    /// out block-state sections only. Placing them at their true world-Y and
    /// lighting them needs a light read; both now exist and are wrapped below
    /// ([`sections_and_light_at`](Self::sections_and_light_at) /
    /// [`world_dimensions`](Self::world_dimensions)).
    #[must_use]
    pub fn sections_at(&self, requests: &[(ChunkPos, usize)]) -> Vec<Option<Arc<ChunkSection>>> {
        match self.handle.get() {
            Some(h) => h.sections_at(requests),
            None => vec![None; requests.len()],
        }
    }

    /// Batch-read owned `(block section, light section)` snapshot pairs from the
    /// client-owned world under a single lock acquisition — the atomic block+light
    /// companion to [`sections_at`](Self::sections_at). Each request is
    /// `(chunk, block_section_index, light_section_index)`; the two indices are
    /// **distinct spaces passed through unchanged** (a mesher for block section
    /// `n` asks `(pos, n, n + 1)` — light section `0` is the below-world boundary
    /// and light section `i` covers block section `i - 1`). Returns all
    /// `(None, None)` before login. Never blocks, never panics.
    #[must_use]
    pub fn sections_and_light_at(
        &self,
        requests: &[(ChunkPos, usize, usize)],
    ) -> Vec<(Option<Arc<ChunkSection>>, Option<SectionLight>)> {
        match self.handle.get() {
            Some(h) => h.sections_and_light_at(requests),
            None => vec![(None, None); requests.len()],
        }
    }

    /// The connected dimension's vertical extent (`min_y` / `height`), or `None`
    /// before the terrain geometry is known (pre-login / pre-first-chunk). A live
    /// mesher needs this to place streamed sections at their true world-`y`:
    /// `section_count = height / 16`, and light sections span
    /// `0..=section_count + 1`, matching
    /// [`sections_and_light_at`](Self::sections_and_light_at).
    #[must_use]
    pub fn world_dimensions(&self) -> Option<WorldDimensions> {
        self.handle.get().and_then(|h| h.world_dimensions())
    }

    /// The columns the client currently holds. Empty before login.
    #[must_use]
    pub fn loaded_chunks(&self) -> Vec<ChunkPos> {
        self.handle
            .get()
            .map_or_else(Vec::new, |h| h.loaded_chunks())
    }

    /// Whether the column at `pos` is currently loaded in the client-owned world.
    /// `false` before login. Cheaper than scanning [`loaded_chunks`](Self::loaded_chunks)
    /// and used by the live-collision path to decide whether the ground under the
    /// player is known yet (vs. holding the player until its column streams in).
    #[must_use]
    pub fn is_chunk_loaded(&self, pos: ChunkPos) -> bool {
        self.handle.get().is_some_and(|h| h.is_chunk_loaded(pos))
    }

    /// The current tab-list entries (version-free `PlayerListEntry`), read from
    /// the client-owned state through the shared handle. Empty before login and
    /// never blocks — same lock-free read path as [`sections_at`](Self::sections_at).
    #[must_use]
    pub fn players(&self) -> Vec<PlayerListEntry> {
        self.handle.get().map_or_else(Vec::new, |h| h.players())
    }

    /// Every currently-tracked entity as a version-free [`EntitySnapshot`] for
    /// interpolation and rendering. Empty before login. Reads the client-owned
    /// entity table through the shared handle; the type key's `path()` and the
    /// `baby` flag are lowered here so the render side never touches a wire type.
    #[must_use]
    pub fn entity_snapshots(&self) -> Vec<EntitySnapshot> {
        self.handle.get().map_or_else(Vec::new, |h| {
            h.entities().into_iter().map(entity_snapshot).collect()
        })
    }

    /// The scoreboard sidebar to draw, folded from the live snapshot, or `None`
    /// when no objective occupies the sidebar slot (or before login).
    #[must_use]
    pub fn sidebar(&self) -> Option<Sidebar> {
        self.handle
            .get()
            .and_then(|h| sidebar_from(&h.scoreboard()))
    }

    /// The active boss bars to draw, folded from the live snapshot, in server
    /// render order. Empty when none are shown (or before login).
    #[must_use]
    pub fn boss_bars(&self) -> Vec<BossBarView> {
        self.handle
            .get()
            .map_or_else(Vec::new, |h| boss_bars_from(&h.boss_bars()))
    }

    /// The folded player inventory menu (window 0), when a live client handle
    /// exists. Empty before login or off a live connection.
    #[must_use]
    pub fn player_menu(&self) -> Option<Menu> {
        self.handle.get().map(|h| h.player_menu())
    }

    /// The currently open non-player menu, if the server has one open.
    #[must_use]
    pub fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        self.handle.get().and_then(|h| h.open_menu())
    }

    /// The server-known position of the local player, once movement or a
    /// teleport has established it. `None` before then.
    #[must_use]
    pub fn server_position(&self) -> Option<Vec3> {
        self.handle.get().and_then(|h| h.position())
    }

    /// A server-less client used only in tests: no thread, no connection. It
    /// captures every [`send_action`](Self::send_action) on the returned
    /// receiver so the outbound path can be asserted without a live server.
    #[cfg(test)]
    pub(crate) fn loopback() -> (Self, Receiver<ClientAction>) {
        // `rx`'s sender is dropped immediately, so `poll` just yields nothing.
        let (_tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let client = Self {
            rx,
            action_tx,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            handle: Arc::new(OnceLock::new()),
        };
        (client, action_rx)
    }

    /// Like [`loopback`](Self::loopback) but also hands back the inbound
    /// [`NetUpdate`] sender, so tests can drive the phase/status mapping in
    /// [`crate::sim`] without a live server. Also returns the captured-action
    /// receiver so a test can both push the session to `Connected` and assert
    /// the outbound movement it then produces.
    #[cfg(test)]
    pub(crate) fn loopback_with_feed() -> (Self, Receiver<ClientAction>, Sender<NetUpdate>) {
        let (tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let client = Self {
            rx,
            action_tx,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            handle: Arc::new(OnceLock::new()),
        };
        (client, action_rx, tx)
    }
}

impl Drop for NetClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    host: String,
    port: u16,
    protocol: i32,
    tx: Sender<NetUpdate>,
    action_rx: Receiver<ClientAction>,
    stop: Arc<AtomicBool>,
    shared_handle: SharedHandle,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(NetUpdate::Error(format!("runtime: {e}")));
            return;
        }
    };

    runtime.block_on(async move {
        let Some(adapter) = lodestone_registry::adapter_for_protocol(protocol) else {
            let _ = tx.send(NetUpdate::Error(format!(
                "no version family compiled in for protocol {protocol}; build with the `live` feature"
            )));
            return;
        };

        let _ = tx.send(NetUpdate::Connecting);
        let profile = LoginProfile {
            username: unique_username(),
            uuid: uuid::Uuid::new_v4(),
        };
        let server = ServerAddress { host, port };

        let (handle, mut events) =
            match ClientBuilder::new(server, profile, adapter)
                .connect_timeout(Some(Duration::from_secs(10)))
                .connect()
                .await
            {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(NetUpdate::Error(format!("connect: {e}")));
                    return;
                }
            };

        // Publish the handle so the render/mesh thread can read the client-owned
        // world (sections_at / loaded_chunks / position). `send_action` is `&self`
        // so the net loop keeps driving outbound movement through the same `Arc`.
        // `shutdown` is `&mut self` and unreachable through a shared `Arc`; the
        // stop path below instead breaks the loop, dropping the runtime and
        // tearing the driver (and its connection) down — a TCP close rather than
        // a protocol disconnect, which the shell treats as equivalent.
        let handle = Arc::new(handle);
        let _ = shared_handle.set(Arc::clone(&handle));

        let mut handed_actions: u64 = 0;
        loop {
            // Flush queued outbound actions first so player movement (queued at
            // 20 Hz) reaches the client promptly rather than waiting on the next
            // inbound event. `send_action` is sync and cheap.
            //
            // NB: this counts actions *handed to the client handle*, not bytes on
            // the wire. Whether an action produces a packet is the version
            // adapter's `encode_action`; on v770 a `Move`/`SwingArm` in the Play
            // state now encodes (→ `move_player_pos_rot` / `swing`), while actions
            // the adapter can't represent in the current state are dropped quietly
            // by the driver. This counter is a queue-liveness signal, never proof
            // of wire delivery — that lives in `impl-physics`'s live gate.
            while let Ok(action) = action_rx.try_recv() {
                let _ = handle.send_action(action);
                handed_actions += 1;
                if handed_actions == 1 || handed_actions.is_multiple_of(20) {
                    tracing::debug!(target: "net", "handed {handed_actions} action(s) to client handle (encode is the adapter's job)");
                }
            }
            // A short timeout keeps the outbound drain responsive even when the
            // server is quiet (no inbound events to wake us).
            match tokio::time::timeout(Duration::from_millis(15), events.recv()).await {
                Ok(Some(event)) => {
                    if forward(&tx, event).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(NetUpdate::Disconnected("stream closed".into()));
                    break;
                }
                Err(_timeout) => {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
    });
}

/// Forward one event; `Err` signals the loop to stop.
fn forward(tx: &Sender<NetUpdate>, event: ClientEvent) -> Result<(), ()> {
    let update = match event {
        ClientEvent::Login { entity_id, .. } => NetUpdate::LoggedIn { entity_id },
        ClientEvent::Chat { text, kind, .. } => match kind {
            // GameInfo is the action bar (SystemChat overlay), not the chat feed:
            // route it to the ActionBar overlay so it draws above the hotbar and
            // fades, instead of piling into the scrollback.
            lodestone_model::event::ChatKind::GameInfo => NetUpdate::ActionBar(text),
            _ => NetUpdate::Chat {
                text,
                player: matches!(kind, lodestone_model::event::ChatKind::Chat),
            },
        },
        // 2001 is the only level event the shell acts on today; the rest are
        // decoded and dropped here rather than in the adapter, so adding a
        // consumer later is a new arm and not a new packet.
        ClientEvent::LevelEvent {
            event: 2001,
            pos,
            data,
            ..
        } => NetUpdate::BlockDestroyed {
            pos,
            // Vanilla reads this as an unsigned state id; a negative here would
            // be an out-of-range id that the model lookup rejects anyway.
            state: data as u32,
        },
        // The general particle-effect packet. `long_distance` is named after
        // what the field actually controls downstream (see
        // `ClientLevel.doAddParticle`'s distance cutoff) rather than the
        // wire/model field name `override_limiter` it is decoded from.
        ClientEvent::Particles {
            particle,
            long_distance,
            pos,
            offset,
            max_speed,
            count,
        } => NetUpdate::Particles {
            kind: particle.path().to_string(),
            long_distance,
            pos,
            offset,
            max_speed,
            count,
        },
        ClientEvent::Disconnect { reason } => {
            let _ = tx.send(NetUpdate::Disconnected(reason.to_plain_string()));
            return Err(());
        }
        ClientEvent::HealthChanged { health, food, .. } => NetUpdate::Health { health, food },
        ClientEvent::Death { .. } => NetUpdate::Death,
        ClientEvent::Respawned { .. } => NetUpdate::Respawned,
        ClientEvent::ExperienceChanged {
            progress,
            level,
            total,
        } => NetUpdate::Experience {
            progress,
            level,
            total,
        },
        // Sound events: strip the namespace to the `sounds.json` key path and
        // pass the server's seed through unchanged (client-side variant
        // selection would desync every client). `fixed_range` is intentionally
        // dropped — client attenuation uses the `sounds.json` entry distance,
        // not the packet's server-side culling range (see `lodestone-sound`).
        ClientEvent::Sound {
            sound,
            category,
            pos,
            volume,
            pitch,
            seed,
            ..
        } => NetUpdate::Sound {
            name: sound.path().to_string(),
            category,
            pos,
            volume,
            pitch,
            seed,
        },
        ClientEvent::EntitySound {
            sound,
            category,
            entity_id,
            volume,
            pitch,
            seed,
            ..
        } => NetUpdate::EntitySound {
            name: sound.path().to_string(),
            category,
            entity_id,
            volume,
            pitch,
            seed,
        },
        // Effects apply to any entity on the wire; the amplifier is a
        // non-negative wire VarInt widened to `i32` by the model, so the
        // narrowing back to `u32` is defensive only (never observed negative).
        ClientEvent::MobEffectApplied {
            entity_id,
            effect,
            amplifier,
            duration_ticks,
            ambient,
            show_icon,
            ..
        } => NetUpdate::EffectApplied {
            entity_id,
            effect: effect.path().to_string(),
            amplifier: u32::try_from(amplifier).unwrap_or(0),
            duration_ticks,
            ambient,
            show_icon,
        },
        ClientEvent::MobEffectRemoved { entity_id, effect } => NetUpdate::EffectRemoved {
            entity_id,
            effect: effect.path().to_string(),
        },
        event @ (ClientEvent::PlayerListUpdate { .. } | ClientEvent::PlayerListRemove { .. }) => {
            NetUpdate::TabListEvent(event)
        }
        event @ (ClientEvent::ObjectiveUpdate { .. }
        | ClientEvent::DisplayObjective { .. }
        | ClientEvent::ScoreUpdate { .. }
        | ClientEvent::ScoreReset { .. }
        | ClientEvent::TeamUpdate { .. }) => NetUpdate::ScoreboardEvent(event),
        event @ (ClientEvent::TitleText { .. }
        | ClientEvent::SubtitleText { .. }
        | ClientEvent::TitlesAnimation { .. }
        | ClientEvent::TitlesCleared { .. }) => NetUpdate::TitleEvent(event),
        // §12.24: the shell treats `ChunkLoaded` as a *dirty-region signal* and
        // ignores any payload — the ruling is that decoded chunks live in a
        // client-owned `World` that consumers query, not in the (bounded,
        // backpressuring) event stream. `impl-world` has since widened this
        // event to also carry `column`; we deliberately do not consume it, both
        // to honour the ruling and to stay robust if that field is reverted.
        ClientEvent::ChunkLoaded { pos, .. } => NetUpdate::Chunk { x: pos.x, z: pos.z },
        ClientEvent::SectionBlocksChanged { section, blocks } => NetUpdate::SectionBlocks {
            x: section.x,
            y: section.y,
            z: section.z,
            blocks,
        },
        // The server placing/relocating the player. The shell camera must adopt
        // this authoritative pose — the read-model's own `position()` is an
        // optimistic echo of our outbound moves, so it cannot substitute here.
        ClientEvent::TeleportPlayer {
            pos,
            rotation,
            flags,
        } => NetUpdate::Teleport {
            pos,
            rotation,
            flags,
        },
        // Everything else (keep-alive, entities, time, player list, chunk
        // unloads) isn't needed by the shell yet.
        _ => return Ok(()),
    };
    tx.send(update).map_err(|_| ())
}

/// Lower a client [`EntityView`] into a version-free [`EntitySnapshot`] for the
/// renderer: the type key's `path()` selects the model, and the `baby` flag maps
/// to a uniform render scale. Baby scale is a single 0.5 approximation for every
/// ageable mob (vanilla varies it per type); good enough to read a baby as
/// smaller, and noted as a refinement rather than a fake.
fn entity_snapshot(view: EntityView) -> EntitySnapshot {
    let scale = if view.baby == Some(true) { 0.5 } else { 1.0 };
    EntitySnapshot {
        id: view.entity_id,
        type_path: view.entity_type.path().to_string(),
        feet: glam::Vec3::new(
            view.position.x as f32,
            view.position.y as f32,
            view.position.z as f32,
        ),
        yaw: view.rotation.yaw,
        head_yaw: view.head_yaw,
        pitch: view.rotation.pitch,
        scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_unique_per_call() {
        // The shell relies on a fresh name per run: offline UUIDs derive from the
        // *name*, so reusing a dead shared player blacks out every later join.
        // Uniqueness is the load-bearing property, not any particular prefix.
        let a = unique_username();
        let b = unique_username();
        assert_ne!(a, b, "two runs must not collide on a username");
        assert!(
            !a.is_empty() && a.len() <= 16,
            "not a valid Minecraft username length: {a:?}"
        );
    }

    #[test]
    fn poll_is_empty_before_any_events() {
        // Connecting to a dead port yields an error update eventually, but poll
        // right away should simply be empty (non-blocking).
        let client = NetClient::connect("127.0.0.1".into(), 1, 776);
        let _ = client.poll();
    }

    #[test]
    fn forward_translates_experience_changed() {
        let (tx, rx) = mpsc::channel();
        let event = ClientEvent::ExperienceChanged {
            progress: 0.25,
            level: 5,
            total: 55,
        };
        forward(&tx, event).expect("forward does not stop the loop");
        match rx.try_recv().expect("an update was forwarded") {
            NetUpdate::Experience {
                progress,
                level,
                total,
            } => {
                assert_eq!(progress, 0.25);
                assert_eq!(level, 5);
                assert_eq!(total, 55);
            }
            other => panic!("expected Experience, got {other:?}"),
        }
    }

    #[test]
    fn forward_translates_mob_effect_applied_with_stripped_namespace() {
        use lodestone_client::ResourceKey;
        use std::str::FromStr;

        let (tx, rx) = mpsc::channel();
        let event = ClientEvent::MobEffectApplied {
            entity_id: 42,
            effect: ResourceKey::from_str("minecraft:speed").unwrap(),
            amplifier: 1,
            duration_ticks: 200,
            ambient: false,
            visible: true,
            show_icon: true,
            blend: false,
        };
        forward(&tx, event).expect("forward does not stop the loop");
        match rx.try_recv().expect("an update was forwarded") {
            NetUpdate::EffectApplied {
                entity_id,
                effect,
                amplifier,
                duration_ticks,
                ambient,
                show_icon,
            } => {
                assert_eq!(entity_id, 42);
                // Namespace stripped, matching the `NetUpdate::Sound` convention.
                assert_eq!(effect, "speed");
                assert_eq!(amplifier, 1);
                assert_eq!(duration_ticks, 200, "duration must reach the HUD model");
                assert!(!ambient);
                assert!(show_icon);
            }
            other => panic!("expected EffectApplied, got {other:?}"),
        }
    }

    /// The gap this whole feature closed: before this arm existed,
    /// `ClientEvent::Particles` fell into `forward`'s catch-all `_ => return
    /// Ok(())` and never reached `NetUpdate` at all. Pins both the namespace
    /// stripping (matching `NetUpdate::Sound`/`EffectApplied`) and the
    /// `override_limiter` → `long_distance` rename.
    #[test]
    fn forward_translates_particles_with_stripped_namespace() {
        use lodestone_client::ResourceKey;
        use std::str::FromStr;

        let (tx, rx) = mpsc::channel();
        let event = ClientEvent::Particles {
            particle: ResourceKey::from_str("minecraft:flame").unwrap(),
            long_distance: true,
            pos: Vec3::new(1.0, 2.0, 3.0),
            offset: Vec3f::new(0.1, 0.2, 0.3),
            max_speed: 0.5,
            count: 12,
        };
        forward(&tx, event).expect("forward does not stop the loop");
        match rx.try_recv().expect("an update was forwarded") {
            NetUpdate::Particles {
                kind,
                long_distance,
                pos,
                offset,
                max_speed,
                count,
            } => {
                assert_eq!(kind, "flame", "namespace must be stripped, matching Sound");
                assert!(long_distance);
                assert_eq!(pos, Vec3::new(1.0, 2.0, 3.0));
                assert_eq!(offset, Vec3f::new(0.1, 0.2, 0.3));
                assert_eq!(max_speed, 0.5);
                assert_eq!(count, 12);
            }
            other => panic!("expected Particles, got {other:?}"),
        }
    }

    #[test]
    fn forward_translates_mob_effect_removed_and_carries_any_entity() {
        use lodestone_client::ResourceKey;
        use std::str::FromStr;

        // Effects are not narrowed to the local player at the wire/forward
        // layer — a remote mob's effect must still come through so the sim can
        // decide whether it is "us" downstream.
        let (tx, rx) = mpsc::channel();
        let event = ClientEvent::MobEffectRemoved {
            entity_id: 99,
            effect: ResourceKey::from_str("minecraft:levitation").unwrap(),
        };
        forward(&tx, event).expect("forward does not stop the loop");
        match rx.try_recv().expect("an update was forwarded") {
            NetUpdate::EffectRemoved { entity_id, effect } => {
                assert_eq!(entity_id, 99);
                assert_eq!(effect, "levitation");
            }
            other => panic!("expected EffectRemoved, got {other:?}"),
        }
    }

    #[test]
    fn loopback_captures_sent_actions_in_order() {
        use lodestone_client::{ClientAction, Rotation, Vec3};
        let (client, actions) = NetClient::loopback();
        let a = ClientAction::Move {
            pos: Vec3::new(1.0, 2.0, 3.0),
            rotation: Rotation::new(45.0, -10.0),
            on_ground: true,
            horizontal_collision: false,
        };
        let b = ClientAction::SwingArm {
            hand: lodestone_client::Hand::Main,
        };
        client.send_action(a.clone());
        client.send_action(b.clone());
        assert_eq!(actions.try_recv().unwrap(), a);
        assert_eq!(actions.try_recv().unwrap(), b);
        assert!(actions.try_recv().is_err());
    }
}
