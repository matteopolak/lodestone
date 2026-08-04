//! Live-server networking, kept strictly behind `lodestone-client`'s public API.
//!
//! The shell selects a version by **protocol number** through
//! [`lodestone_registry::adapter_for_protocol`] and otherwise deals only in the
//! version-free `lodestone-model` types re-exported by the client. It never
//! names a packet, a version, or a version crate. TCP is reached only via
//! [`ClientBuilder::connect`], preserving the [`Transport`] seam a future wasm
//! build needs.
//!
//! ## Singleplayer is the same code with a different transport (issue #287)
//!
//! [`NetClient::open_singleplayer`] starts `lodestone_server`'s
//! `IntegratedServer` on this module's own net thread and speaks to it over an
//! in-memory duplex through [`ClientBuilder::connect_with`] — the seam the
//! paragraph above kept open. Everything downstream of the transport is
//! byte-identical to a multiplayer join: same adapter, same driver, same
//! [`NetUpdate`] fold, same outbound action queue. See this module's `Origin`.
//!
//! The version seam holds in *both* directions. The serverbound encoder comes
//! from [`lodestone_registry::server_protocol_for_protocol`] as a trait object,
//! so this crate names no version for the server either — and a build with no
//! version family compiled in gets `None` and reports it, which is what keeps
//! `cargo check -p lodestone-shell --no-default-features` meaningful.
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
    Arc, Mutex, OnceLock, PoisonError,
    atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::JoinHandle;
use std::time::Duration;

use lodestone_client::{
    BlockPos, ChunkPos, ChunkSection, ClientAction, ClientBuilder, ClientEvent, ClientHandle,
    EntityView, LoginProfile, OpenMenuSnapshot, PlayerListEntry, Reported, RespawnPolicy,
    Rotation, SectionLight, ServerAddress, Vec3, WorldDimensions,
};
use lodestone_game::menu::Menu;
use lodestone_game::scoreboard::Scoreboard;
use lodestone_game::tablist::TabList;
use lodestone_model::Vec3f;
use lodestone_model::event::SoundCategory;
// `SectionLight` is imported anonymously: it is the trait carrying
// `sky_light`/`block_light` on `WorldSectionLight`, and naming it would collide
// with `lodestone_world::SectionLight`, the *storage* type of the same name that
// `sections_and_light_at` hands back.
use lodestone_render::{SectionLight as _, SkyDefault, WorldSectionLight};

pub use lodestone_testsupport::unique_username;

use crate::entities::{EntitySnapshot, NameTag};

/// A handle to the live client, published by the net thread once the session is
/// up and read by the render/mesh thread. `None` until login completes.
///
/// `pub` so a caller that needs its own `'static` lookup closure — e.g.
/// [`RenderState::set_entity_light_source`](crate::gpu::RenderState::set_entity_light_source)
/// — can clone it out of a [`NetClient`] via [`NetClient::shared_handle`]
/// *before* handing the client off to [`Sim::attach_net`](crate::sim::Sim::attach_net),
/// and keep it past the point `NetClient` itself is no longer reachable. It is
/// `Arc`-based (`Send + Sync + 'static`) and resolves lazily once login
/// completes, same as `NetClient`'s own reads.
pub type SharedHandle = Arc<OnceLock<Arc<ClientHandle>>>;

/// The world's weather, published lock-free by the net thread and read once per
/// frame by the render thread.
///
/// # Why this is not a [`NetUpdate`]
///
/// `GAME_EVENT`'s rain and thunder levels arrive **every tick** while the server
/// ramps them (`ServerLevel.java:762-775` broadcasts on any change, and the
/// change is ±0.01 per tick), and the consumer wants only the newest value. That
/// is the same "latest wins, never queue" shape as [`SharedHandle`]: a channel
/// would carry ~20 messages a second whose only purpose is to be superseded, and
/// the render side would have to fold them back into one scalar anyway.
///
/// It also keeps the weather read out of `Sim`. Every other `NetUpdate` is
/// drained by `Sim::poll_net`, which is the right home for anything the
/// simulation acts on; rain level is consumed **only** by the renderer and the
/// audio cadence, both of which `crate::app` reaches directly.
///
/// Rain and thunder are stored as raw `f32` bits rather than behind a lock
/// because they are independent scalars and a torn read between them is
/// indistinguishable from the ordinary one-tick staleness a per-frame poll
/// already has.
#[derive(Debug, Default)]
pub struct WeatherCell {
    rain_bits: AtomicU32,
    thunder_bits: AtomicU32,
    /// Bumped once per `lightning_bolt` spawn. A **sequence number**, not a
    /// countdown: the flash's 5-tick lifetime is timed on the render side against
    /// the game-tick clock, which the net thread does not have. See
    /// [`lodestone_render::weather::LIGHTNING_FLASH_TICKS`].
    lightning_seq: AtomicU64,
}

/// One frame's read of a [`WeatherCell`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WeatherSnapshot {
    /// The server's `RAIN_LEVEL_CHANGE`, or the level a `START_RAINING` /
    /// `STOP_RAINING` implied. **Not** clamped or composed here — hand it to
    /// [`lodestone_render::weather::WeatherState::apply_rain_level`], which does
    /// both exactly as `Level.setRainLevel` does.
    pub rain_level: f32,
    /// The server's `THUNDER_LEVEL_CHANGE`, **raw**: not yet multiplied by the
    /// rain level. `WeatherState::thunder_level` is what composes them; reading
    /// this field into a darkening term directly is the mistake
    /// `lodestone_render::weather`'s module doc warns about.
    pub thunder_level: f32,
    /// Monotonic count of lightning bolts seen this session.
    pub lightning_seq: u64,
}

impl WeatherCell {
    /// Fold one `ClientEvent::WeatherChanged`. Only the `Some` fields are written,
    /// matching the event's own three-optional shape — the adapter emits exactly
    /// one of them per `GAME_EVENT`.
    fn apply(&self, raining: Option<bool>, rain_level: Option<f32>, thunder_level: Option<f32>) {
        // `START_RAINING` → 0.0 and `STOP_RAINING` → 1.0 is vanilla's own
        // inversion, reproduced in `WeatherState::apply_raining`; the polarity
        // lives there so there is one place to read about it, not two.
        if let Some(raining) = raining {
            let mut state = lodestone_render::weather::WeatherState::clear();
            state.apply_raining(raining);
            self.rain_bits
                .store(state.rain_level().to_bits(), Ordering::Relaxed);
        }
        if let Some(level) = rain_level {
            self.rain_bits.store(level.to_bits(), Ordering::Relaxed);
        }
        if let Some(level) = thunder_level {
            self.thunder_bits.store(level.to_bits(), Ordering::Relaxed);
        }
    }

    /// Record a lightning bolt.
    fn strike(&self) {
        self.lightning_seq.fetch_add(1, Ordering::Relaxed);
    }

    /// Read this frame's weather.
    #[must_use]
    pub fn snapshot(&self) -> WeatherSnapshot {
        WeatherSnapshot {
            rain_level: f32::from_bits(self.rain_bits.load(Ordering::Relaxed)),
            thunder_level: f32::from_bits(self.thunder_bits.load(Ordering::Relaxed)),
            lightning_seq: self.lightning_seq.load(Ordering::Relaxed),
        }
    }
}

/// A [`WeatherCell`] shared between the net thread and the render thread.
pub type SharedWeather = Arc<WeatherCell>;

/// One biome's declared climate at its holder id, as
/// [`ClientEvent::BiomeClimates`] carries it — the `temperature`/
/// `has_precipitation` pair `ShellWeatherProbe::precipitation` (issue #25)
/// needs to answer rain vs snow, `downfall` carried alongside for a future
/// grass/foliage tint consumer (see `docs/worldgen-biomes.md`).
///
/// `None` per field mirrors the event's own shape: an entry that failed to
/// parse, not "this biome declares no value" — every real 26.2 biome
/// declares a climate, unlike `sky_color`, which real Nether/End biomes
/// genuinely omit.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BiomeClimateEntry {
    /// Declared (not height-adjusted) temperature.
    pub temperature: Option<f32>,
    /// Downfall; unused by the rain/snow decision itself.
    pub downfall: Option<f32>,
    /// Whether this biome ever rains or snows.
    pub has_precipitation: Option<bool>,
}

/// Every biome's declared climate, published once at `Login` by [`forward`]'s
/// `BiomeClimates` arm and read every frame `ShellWeatherProbe::precipitation`
/// resolves a column.
///
/// `Mutex<Vec<..>>`, not lock-free atomics like [`WeatherCell`]: this table
/// changes once per `Login`, never per-tick, so there is no contention to
/// design around — the whole table is replaced wholesale rather than merged
/// field by field, which a handful of atomics could not express anyway (the
/// table's *length* changes with the registry, not just its values).
#[derive(Debug, Default)]
pub struct BiomeClimateCell(Mutex<Vec<BiomeClimateEntry>>);

impl BiomeClimateCell {
    /// Replace the whole table. Called once, at `Login`, by [`forward`]'s
    /// `BiomeClimates` arm — mirrors [`ClientEvent::BiomeVisuals::sky_colors`]'s
    /// own "indexed by holder id" shape, so the three parallel slices are
    /// zipped by index rather than requiring equal lengths (a biome registry
    /// that fails to parse one field but not another is exactly the case
    /// `Option` per field already exists to carry).
    ///
    /// `pub(crate)`, not private: the app.rs live gate for issue #25
    /// (`live_precipitation_matches_vanillas_own_threshold_for_real_biomes`)
    /// connects through `ClientBuilder` directly, bypassing `forward`
    /// entirely, and calls this by hand with the real event off the raw
    /// stream — proving the exact fold `forward`'s arm makes, not a
    /// stand-in for it.
    pub(crate) fn apply(
        &self,
        temperatures: &[Option<f32>],
        downfall: &[Option<f32>],
        has_precipitation: &[Option<bool>],
    ) {
        let len = temperatures
            .len()
            .max(downfall.len())
            .max(has_precipitation.len());
        let table = (0..len)
            .map(|i| BiomeClimateEntry {
                temperature: temperatures.get(i).copied().flatten(),
                downfall: downfall.get(i).copied().flatten(),
                has_precipitation: has_precipitation.get(i).copied().flatten(),
            })
            .collect();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = table;
    }

    /// The climate at holder id `index`, or `None` when the table is empty
    /// (no biome registry yet, matching [`ClientHandle`]'s own "absent reads
    /// as unknown" convention) or `index` is out of range.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<BiomeClimateEntry> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)
            .copied()
    }
}

/// A [`BiomeClimateCell`] shared between the net thread and the render thread.
pub type SharedBiomeClimates = Arc<BiomeClimateCell>;

/// The current dimension's policy for *absent* sky light, shared between the
/// thread that learns the dimension and the render thread's per-entity light
/// sampler.
///
/// # Why a cell rather than a lookup
///
/// This exists for a shape mismatch, not for concurrency. [`entity_light_at`]
/// needs a [`SkyDefault`], and its caller is the `'static` closure
/// [`RenderState::set_entity_light_source`](crate::gpu::RenderState::set_entity_light_source)
/// installs **once** at connect — so it cannot be handed a per-frame value, while
/// the policy it needs changes mid-session on a portal.
///
/// The obvious alternative is to call `ClientHandle::player()` inside the closure
/// and read `dimension`/`dimension_type` off the snapshot. That costs an ECS read
/// lock and a whole `PlayerSnapshot` clone **per entity per frame**, and
/// `Sim::extract_particles` is a standing warning about precisely that: it used to
/// take the `World` guard per *particle* and was the longest lock hold in the
/// process.
///
/// One `AtomicU8`, `Relaxed`, holding a discriminant. `Sim::refresh_mesh_policy`
/// is the **single** producer — it already computes this value for the mesher, so
/// there is one expression deciding it and no second source of truth. A reader
/// that sees a one-frame-stale value on the frame you step through a portal has
/// exactly the staleness every other per-frame poll here has.
#[derive(Debug)]
pub struct SkyDefaultCell(AtomicU8);

/// [`SkyDefault::None`] as stored in a [`SkyDefaultCell`].
const SKY_DEFAULT_NONE: u8 = 0;
/// [`SkyDefault::Full`] as stored in a [`SkyDefaultCell`].
const SKY_DEFAULT_FULL: u8 = 1;

impl Default for SkyDefaultCell {
    /// [`SkyDefault::Full`], which is what `sky_default_for_dimension(None, None)`
    /// answers for "dimension not yet known". Defaulting to `None` instead would
    /// black out every mob in the first frames of a join, before the dimension
    /// type arrives — the failure this whole cell exists to fix.
    fn default() -> Self {
        Self(AtomicU8::new(SKY_DEFAULT_FULL))
    }
}

impl SkyDefaultCell {
    /// Publish the policy for the dimension we are now in.
    pub fn set(&self, policy: SkyDefault) {
        let bits = match policy {
            SkyDefault::Full => SKY_DEFAULT_FULL,
            SkyDefault::None => SKY_DEFAULT_NONE,
        };
        self.0.store(bits, Ordering::Relaxed);
    }

    /// This frame's policy.
    #[must_use]
    pub fn get(&self) -> SkyDefault {
        if self.0.load(Ordering::Relaxed) == SKY_DEFAULT_FULL {
            SkyDefault::Full
        } else {
            SkyDefault::None
        }
    }
}

/// A [`SkyDefaultCell`] shared between `Sim` and the render thread's samplers.
pub type SharedSkyDefault = Arc<SkyDefaultCell>;

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
    /// A `block_event` (vanilla's `ClientboundBlockEventPacket`): two opaque
    /// parameter bytes for the block at `pos`.
    ///
    /// Deliberately **uninterpreted here**. The two bytes mean completely
    /// different things per block type — `b0 == 1` on a chest is "viewer count in
    /// `b1`" (`ChestBlockEntity.triggerEvent`), on a note block it is a pitch, on
    /// a piston a direction — and the adapter already declines to interpret them
    /// for exactly that reason. `Sim::poll_net` forwards them to the one consumer
    /// that knows the rule.
    ///
    /// Added for issue #23: this variant is why a chest lid opens at all. The
    /// event was decoded by `v770`'s adapter and reached
    /// `ClientEvent::BlockEvent` with **no consumer anywhere** — it fell through
    /// [`forward`]'s terminal `_ =>` arm and was dropped silently, so a chest
    /// could only ever be drawn shut.
    BlockEvent {
        /// Absolute block position.
        pos: [i32; 3],
        /// First parameter byte — the event *kind*, per block type.
        b0: u8,
        /// Second parameter byte — the event's payload.
        b1: u8,
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
    // `Health` and `Experience` used to live here, forwarded from
    // `ClientEvent::{HealthChanged, ExperienceChanged}` and folded by
    // `Sim::poll_net` into the `Vitals`/`Xp` components. **Both are deleted**, for
    // the same reason Stage 3 deleted `TabListEvent` and `ScoreboardEvent`: the
    // net thread's `SharedState::apply` now folds those events into those same
    // components (`lodestone_ecs::session::apply_local_player_state`), so a shell
    // arm would be a *second* writer of one component. The HUD reads
    // `Sim::health`/`food`/`experience` exactly as before; only the writer moved.
    //
    // `Death` and `Respawned` deliberately stayed: they drive the driver's own
    // `Dead` marker and `RespawnCount`, which are not folds of the server's view
    // (see `lodestone_ecs::session::ServerAlive`'s docs on why the two liveness
    // rules must not merge).
    /// The player died. A transient state, not the end of the session: the
    /// shell shows the death screen (issue #103) and [`NetUpdate::Respawned`]
    /// follows once the player clicks Respawn and the server confirms it — the
    /// client library no longer auto-respawns (`RespawnPolicy::Manual`, set on
    /// the `ClientBuilder` in [`run`]), which is the actual behaviour change
    /// this issue asked for; a screen with no gate behind it would have
    /// nothing to show.
    Death {
        /// The server's own death message (`ClientEvent::Death`'s `message`
        /// field), flattened to plain text with `to_plain_string()` in
        /// [`forward`] — **not** resolved through the language table.
        /// Untranslated components (most death causes) render as their raw
        /// key. [`Self::Disconnected`] used to flatten the same way and no
        /// longer does (issue #68); this variant is the one that still does,
        /// named as a deliberate, separate follow-up in
        /// `docs/death-screen.md`'s "What was deliberately left out" section
        /// rather than fixed here.
        message: String,
    },
    /// The server confirmed a respawn (post-death, dimension change, or
    /// `/respawn`). The fresh position arrives in the placement
    /// [`NetUpdate::Teleport`] that follows.
    Respawned,
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
    /// An item entity was collected (`take_item_entity`), for the fly-to-collector
    /// animation — issue #365.
    ///
    /// Carried as the raw [`ClientEvent::ItemPickup`], like [`Self::TitleEvent`]:
    /// the consumer is [`lodestone_game::mining::PickupFeed`], whose `apply` folds
    /// a `&ClientEvent` directly, so a re-typed struct variant here would be a
    /// second spelling of the same three fields.
    ///
    /// **This is the animation only, never an inventory change.** The stack that
    /// actually lands in the player's inventory arrives separately as
    /// `set_player_inventory`/`container_set_slot`, which `Menus` folds — see
    /// `PickupFeed`'s own "This is not an inventory" note for why folding a count
    /// from here would be a second, silently-diverging source of truth.
    ItemPickup(ClientEvent),
    /// A title/subtitle delta for the shell-owned
    /// [`lodestone_game::player_state::TitleState`] fold.
    TitleEvent(ClientEvent),
    /// An action-bar (GameInfo) message for the shell-owned
    /// [`lodestone_game::player_state::ActionBar`] fold.
    ActionBar(lodestone_model::Text),
    /// The session ended (clean or with a reason), as an unresolved
    /// [`lodestone_model::Text`] component — same convention as
    /// [`Self::Chat`]/[`Self::ActionBar`]: translation keys survive here and
    /// are resolved through `Sim::translator()` at the read boundary
    /// ([`Sim::poll_net`]'s `Disconnected` arm), so a kick reason like
    /// `multiplayer.disconnect.kicked` reaches `Screen::Error` as English
    /// rather than the raw key (issue #68). The two synthetic senders in
    /// this module (`"stream closed"`, and `sim.rs`'s test-only
    /// `"Server closed"`) use [`lodestone_model::Text::literal`]: they are
    /// not vanilla translation keys, so wrapping them in a `Text` that
    /// merely carries their literal English through the same pipe is
    /// correct — the translator is a no-op on a `Literal` node, it only
    /// rewrites `Translate` nodes.
    Disconnected(Box<lodestone_model::Text>),
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
    /// The world's rain/thunder levels and lightning count, folded by
    /// [`forward`]'s `WeatherChanged` arm. See [`WeatherCell`] for why this is a
    /// shared cell rather than a [`NetUpdate`].
    weather: SharedWeather,
    /// Every biome's declared climate, folded by [`forward`]'s `BiomeClimates`
    /// arm at `Login`. See [`BiomeClimateCell`] for why this is a shared cell
    /// rather than a [`NetUpdate`] — the same reasoning as [`Self::weather`].
    biome_climates: SharedBiomeClimates,
    /// The current dimension's absent-sky-light policy. Unlike [`Self::weather`]
    /// the **net thread never writes this** — `Sim::refresh_mesh_policy` is the
    /// sole producer and the render thread's light samplers are the consumers.
    /// It lives here only because `NetClient` is where a per-session shared cell
    /// is already handed out at connect time. See [`SkyDefaultCell`].
    sky_default: SharedSkyDefault,
    /// The driver's `World` and session entity, for a **loopback** client that has
    /// no `ClientBuilder` to hand them to.
    ///
    /// Production goes through [`Self::connect`], where the real client adopts the
    /// handle at build time; a loopback has no connection at all, so
    /// `Sim::attach_net` binds it afterwards ([`Self::bind_session`]) and
    /// [`Self::ingest_session_event`] folds through the **same** systems the net
    /// thread runs, into the **same** `World` the shell reads. A test *double* for
    /// the transport, not for the fold. Compiled out of every production build.
    #[cfg(test)]
    session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
}

/// Where the net thread's connection comes from.
///
/// The two arms are the *whole* difference between multiplayer and singleplayer
/// in this shell: same client, same adapter, same event fold, same thread, same
/// outbound action queue — a different `Transport`. That is vanilla's own
/// architecture and the reason `IntegratedServer` exposes an in-memory duplex
/// (see its module docs), and it is why this is an enum threaded into one `run`
/// rather than a second net thread with its own loop.
enum Origin {
    /// Dial a real server over TCP. `auth` is `Some` for an online-mode join.
    Remote {
        /// Host to dial.
        host: String,
        /// Port to dial.
        port: u16,
        /// An authenticated Microsoft/Minecraft session, for online mode.
        auth: Option<lodestone_client::Session>,
    },
    /// Host `lodestone-server`'s integrated server in **this thread's runtime**
    /// and speak to it over an in-memory duplex — singleplayer (issue #287).
    ///
    /// `protocol` is a trait object resolved by
    /// [`lodestone_registry::server_protocol_for_protocol`], which is what keeps
    /// this crate from naming a version: the shell holds a protocol *number*, and
    /// the registry is the only thing that turns one into an encoder.
    Integrated {
        /// The serverbound half of the version family, from the registry.
        protocol: Box<dyn lodestone_server::ServerProtocol>,
        /// World seed for the bundled overworld generator.
        seed: i64,
        /// Chunk radius the server streams around the player.
        view_radius: i32,
    },
}

impl std::fmt::Debug for Origin {
    // Hand-written because `ServerProtocol` does not require `Debug` (it is
    // implemented by version crates, and forcing a bound on the seam to satisfy
    // a lint would be the wrong trade). `auth` is deliberately not printed: it
    // holds a live access token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Remote { host, port, auth } => f
                .debug_struct("Remote")
                .field("host", host)
                .field("port", port)
                .field("online", &auth.is_some())
                .finish(),
            Origin::Integrated {
                seed, view_radius, ..
            } => f
                .debug_struct("Integrated")
                .field("seed", seed)
                .field("view_radius", view_radius)
                .finish_non_exhaustive(),
        }
    }
}

/// The `ServerAddress` a singleplayer session presents to the client.
///
/// The client needs one for its handshake (`begin_login` puts the host and port
/// in the intention packet), and there is no socket here. Vanilla's integrated
/// server answers this question the same way — its `ServerData` for a
/// single-player world is synthetic. Nothing routes on the value; it is only
/// echoed into the handshake the server's own decoder ignores.
const SINGLEPLAYER_ADDRESS: (&str, u16) = ("singleplayer", 0);

impl NetClient {
    /// Spawn a background thread that connects to `host:port` speaking the given
    /// protocol number and forwards events. Returns immediately.
    ///
    /// # `session`, and why it is threaded down here
    ///
    /// `docs/bevy-migration.md` §4.1(c). `Some((world, entity))` makes the client
    /// fold its read-model into a `World` the **caller** already owns, hanging the
    /// session components off `entity` — so a component the net thread's ingest
    /// writes is visible to a `GameTick` system on the driver thread, which is the
    /// entire point. `None` lets the client mint its own `World`, which is what a
    /// bare `NetClient` in a live gate (no `Sim` anywhere) wants.
    ///
    /// The handle travels *down*, never up: `lodestone_shell::sim::Sim` owns the
    /// `World` and hands it here. Adopting the client's instead would change the
    /// `World`'s identity at every connect and invalidate `Sim.local`, the local
    /// player's `Entity`, which the voluntary-teardown path holds across
    /// `Sim::end_session`.
    ///
    /// Prefer [`crate::sim::Sim::connect`] over calling this with `Some(..)` by
    /// hand: passing `None` where a `Sim` exists is silent — the session fold lands
    /// in a `World` nothing reads and every HUD accessor returns an empty default.
    #[must_use]
    pub fn connect(
        host: String,
        port: u16,
        protocol: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    ) -> Self {
        Self::connect_impl(
            Origin::Remote {
                host,
                port,
                auth: None,
            },
            protocol,
            session,
        )
    }

    /// As [`Self::connect`], but for an **online-mode** server: `auth` is an
    /// authenticated Microsoft/Minecraft session (issue #65 — see
    /// `lodestone_auth::login` for how to obtain one from a cached refresh
    /// token or a completed interactive device-code sign-in) that the net
    /// thread hands to [`lodestone_client::ClientBuilder::online_session`],
    /// and the real profile identity (`auth.profile.name`/`.id`) replaces the
    /// [`unique_username`] offline-mode name path for the login-start packet.
    ///
    /// This is purely additive: [`Self::connect`] is completely unchanged and
    /// remains the offline-mode default every existing caller uses. Nothing
    /// in the shell calls this yet — wiring an actual "sign in" UI action to
    /// it is issue #66's job; this method is the seam that work connects to.
    #[must_use]
    pub fn connect_online(
        host: String,
        port: u16,
        protocol: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
        auth: lodestone_client::Session,
    ) -> Self {
        Self::connect_impl(
            Origin::Remote {
                host,
                port,
                auth: Some(auth),
            },
            protocol,
            session,
        )
    }

    /// Start the **integrated server** in-process and connect to it —
    /// singleplayer (issue #287).
    ///
    /// `server_protocol` is the serverbound half of a version family, obtained
    /// from [`lodestone_registry::server_protocol_for_protocol`]; `protocol` is
    /// the same number, used for the *client* adapter. Both come from one
    /// registry lookup pair in `crate::app`'s `launch_singleplayer`, which is also
    /// where a build with no version family is turned into a reported error
    /// instead of a thread that starts and finds nothing.
    ///
    /// The server and the client share the net thread's current-thread runtime.
    /// That is deliberate rather than incidental: `IntegratedServer::open_in_memory`
    /// spawns its serving task on whatever runtime is entered, so hosting it here
    /// keeps the whole session — server tick, client driver, and the event fold —
    /// on one thread with no cross-thread synchronisation at all, and makes the
    /// server's lifetime exactly the session's. The shell's render loop is
    /// unaffected; it still drains [`NetUpdate`]s once per frame.
    ///
    /// `session` means what it does for [`Self::connect`] (§4.1(c)): pass the
    /// caller's `World` or the fold lands somewhere nothing reads. Prefer
    /// [`crate::sim::Sim`]'s own launch path over calling this with `None`.
    #[must_use]
    pub fn open_singleplayer(
        server_protocol: Box<dyn lodestone_server::ServerProtocol>,
        protocol: i32,
        seed: i64,
        view_radius: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    ) -> Self {
        Self::connect_impl(
            Origin::Integrated {
                protocol: server_protocol,
                seed,
                view_radius,
            },
            protocol,
            session,
        )
    }

    /// Shared implementation behind [`Self::connect`]/[`Self::connect_online`]/
    /// [`Self::open_singleplayer`]: spawns the background net thread and returns
    /// immediately.
    fn connect_impl(
        origin: Origin,
        protocol: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle: SharedHandle = Arc::new(OnceLock::new());
        let handle_thread = Arc::clone(&handle);
        let weather: SharedWeather = Arc::new(WeatherCell::default());
        let weather_thread = Arc::clone(&weather);
        let biome_climates: SharedBiomeClimates = Arc::new(BiomeClimateCell::default());
        let biome_climates_thread = Arc::clone(&biome_climates);

        let thread = std::thread::Builder::new()
            .name("lodestone-net".into())
            .spawn(move || {
                run(
                    origin,
                    protocol,
                    tx,
                    action_rx,
                    stop_thread,
                    handle_thread,
                    weather_thread,
                    biome_climates_thread,
                    session,
                )
            })
            .expect("spawn net thread");

        Self {
            rx,
            action_tx,
            stop,
            thread: Some(thread),
            handle,
            weather,
            biome_climates,
            sky_default: Arc::new(SkyDefaultCell::default()),
            #[cfg(test)]
            session: None,
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
            // A player's name tag is its tab-list display name (issue #100),
            // never its metadata custom name — read once per poll rather than
            // once per entity, same reasoning as `tab_list`/`players` above.
            let tab_list = h.tab_list();
            h.entities()
                .into_iter()
                .map(|view| entity_snapshot(view, &tab_list))
                .collect()
        })
    }

    /// The folded scoreboard — the one copy in the process since Stage 3.
    ///
    /// Empty off a live connection, which draws no sidebar. The shell used to
    /// keep its own `lodestone_game::scoreboard::Scoreboard` and fold
    /// `NetUpdate::ScoreboardEvent` into it; that field and that `NetUpdate`
    /// variant are gone, so this read is the only route to the state.
    #[must_use]
    pub fn scoreboard(&self) -> Scoreboard {
        #[cfg(test)]
        if let Some((ecs, entity)) = &self.session {
            return ecs
                .read()
                .get::<lodestone_ecs::SessionScoreboard>(*entity)
                .map(|board| board.0.clone())
                .unwrap_or_default();
        }
        self.handle.get().map(|h| h.scoreboard()).unwrap_or_default()
    }

    /// The folded tab list — likewise the one copy. Empty off a live connection.
    #[must_use]
    pub fn tab_list(&self) -> TabList {
        #[cfg(test)]
        if let Some((ecs, entity)) = &self.session {
            return ecs
                .read()
                .get::<lodestone_ecs::SessionTabList>(*entity)
                .map(|list| list.0.clone())
                .unwrap_or_default();
        }
        self.handle.get().map(|h| h.tab_list()).unwrap_or_default()
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

    /// Clone out the `Arc`-backed handle this `NetClient` publishes into once
    /// login completes. Unlike every other read on `self`, this survives the
    /// `NetClient` being moved (e.g. into [`crate::sim::Sim::attach_net`]),
    /// because the net thread was handed its own clone of the same `Arc` at
    /// [`connect`](Self::connect) and keeps publishing into it regardless of
    /// what happens to this struct. Exists so a `'static` closure — the shape
    /// [`RenderState::set_entity_light_source`](crate::gpu::RenderState::set_entity_light_source)
    /// requires — can be built once at connect time instead of needing a
    /// borrow of a `NetClient` that won't be around a frame later.
    #[must_use]
    pub fn shared_handle(&self) -> SharedHandle {
        Arc::clone(&self.handle)
    }

    /// Clone out the `Arc`-backed weather cell, for the same reason
    /// [`shared_handle`](Self::shared_handle) exists: `crate::app` builds a
    /// `'static` per-frame closure at connect time, and this `NetClient` is moved
    /// into `Sim::attach_net` immediately afterwards.
    #[must_use]
    pub fn shared_weather(&self) -> SharedWeather {
        Arc::clone(&self.weather)
    }

    /// Clone out the `Arc`-backed biome-climate cell, for the same reason
    /// [`shared_weather`](Self::shared_weather) exists: `crate::app` builds a
    /// `'static` per-frame `ShellWeatherProbe` at connect time, and this
    /// `NetClient` is moved into `Sim::attach_net` immediately afterwards.
    #[must_use]
    pub fn shared_biome_climates(&self) -> SharedBiomeClimates {
        Arc::clone(&self.biome_climates)
    }

    /// Clone out the `Arc`-backed absent-sky-light policy cell, for the same
    /// reason [`shared_weather`](Self::shared_weather) exists. Two callers:
    /// `crate::app` hands it to the `'static` entity-light closure it installs at
    /// connect, and `Sim::refresh_mesh_policy` publishes into it.
    #[must_use]
    pub fn shared_sky_default(&self) -> SharedSkyDefault {
        Arc::clone(&self.sky_default)
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
            weather: Arc::new(WeatherCell::default()),
            biome_climates: Arc::new(BiomeClimateCell::default()),
            sky_default: Arc::new(SkyDefaultCell::default()),
            // Bound by `Sim::attach_net`; a loopback with no `Sim` folds nothing.
            session: None,
        };
        (client, action_rx)
    }

    /// Point a loopback client at the driver's `World`, so a hermetic test folds
    /// into the same store the shell reads. Called by `Sim::attach_net`.
    #[cfg(test)]
    pub(crate) fn bind_session(
        &mut self,
        world: lodestone_ecs::EcsHandle,
        entity: lodestone_ecs::ecs::entity::Entity,
    ) {
        self.session = Some((world, entity));
    }

    /// Fold one `ClientEvent` through the session systems of the `World` this
    /// loopback is bound to, exactly as the net thread's `SharedState::apply` does.
    ///
    /// One event per schedule run, deliberately — the same rule the real ingest
    /// follows, so a test's cross-family ordering matches production's.
    #[cfg(test)]
    pub(crate) fn ingest_session_event(&self, event: ClientEvent) {
        let Some((ecs, _)) = &self.session else {
            return;
        };
        let mut world = ecs.write();
        world
            .resource_mut::<lodestone_ecs::ingest::IngestQueue>()
            .push(event);
        world.run_schedule(lodestone_ecs::NetIngest);
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
            weather: Arc::new(WeatherCell::default()),
            biome_climates: Arc::new(BiomeClimateCell::default()),
            sky_default: Arc::new(SkyDefaultCell::default()),
            // Bound by `Sim::attach_net`; a loopback with no `Sim` folds nothing.
            session: None,
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

/// Sample packed sky/block light for a world block position through a
/// [`SharedHandle`], for the entity-lighting seam
/// ([`RenderState::set_entity_light_source`](crate::gpu::RenderState::set_entity_light_source)).
///
/// This is the same lookup [`Sim::extract_particles`](crate::sim::Sim::extract_particles)
/// does per particle-frame (see that function's comments for the traps this
/// repeats):
/// - [`ClientHandle::sections_and_light_at`] takes `lodestone_client::ChunkPos`,
///   built here from the raw coordinates — deliberately not the
///   `lodestone_world` one used elsewhere in the shell.
/// - Light section `i` covers block section `i-1`, so a lookup for block
///   section `n` asks for light section `n + 1`. Not a bug to "align".
/// - `None` here means "outside a loaded chunk / before login", never
///   darkness; the caller (here, `EntityLightSource::sample`) substitutes
///   full brightness, exactly like the particle path's fallback.
///
/// # `sky_default` is load-bearing, and omitting it blacked out mobs in open air
///
/// `SectionLight::sky_at` resolves [`LightData::Missing`] to **`0`** — its own doc
/// says so, and says a caller wanting vanilla's above-the-world default of `15`
/// must branch on the public `sky` field itself, because that default depends on
/// the dimension and on whether the section sits above the heightmap. This
/// function used to call `sky_at` with no such branch.
///
/// The server sends no sky array for sections above the top of the lit column, so
/// every such section arrived `Missing` and read as sky `0`. Vanilla's
/// `SkyLightSectionStorage` returns `15` there instead. The visible result was
/// two player-reported bugs that looked unrelated: **mobs flashing black** when
/// they jumped or swam up into an empty section, and **the first-person arm going
/// black over an ocean** — the arm samples at the camera, and over open water the
/// camera sits in exactly such a section, while on land it is usually inside one
/// that carries data. That is why it looked ocean-specific rather than
/// height-specific.
///
/// The policy is applied through [`WorldSectionLight`], which is the **same
/// adapter the terrain draw uses**, rather than a second `match` restating `15`
/// here — per CLAUDE.md, derive from the expression the draw uses. It only ever
/// touches `Missing`; stored data, including a nether section's `Uniform(0)`, is
/// returned verbatim, so this cannot manufacture a too-bright nether.
///
/// Returns the packed byte the entity shader unpacks
/// (`crates/lodestone-render/src/entity_pipeline.rs`: `sky = (light >> 4) &
/// 15; block = light & 15`) — sky light in the high nibble, block light in
/// the low nibble, i.e. `sky << 4 | block`.
#[must_use]
pub fn entity_light_at(
    handle: &SharedHandle,
    x: i32,
    y: i32,
    z: i32,
    sky_default: SkyDefault,
) -> Option<u8> {
    let h = handle.get()?;
    let dims = h.world_dimensions()?;
    let section = (y - dims.min_y).div_euclid(16);
    if section < 0 || section >= dims.section_count() as i32 {
        return None;
    }
    let pos = ChunkPos {
        x: x.div_euclid(16),
        z: z.div_euclid(16),
    };
    let got = h.sections_and_light_at(&[(pos, section as usize, section as usize + 1)]);
    let (_, light) = got.into_iter().next()?;
    let light = light?;
    let lx = x.rem_euclid(16) as usize;
    let ly = (y - dims.min_y).rem_euclid(16) as usize;
    let lz = z.rem_euclid(16) as usize;
    // The dimension policy for absent sky data, through the same adapter the
    // terrain draw uses. `sky_at`/`block_at` directly would resolve `Missing` sky
    // to 0 — see this function's doc for the two bugs that produced.
    let resolved = WorldSectionLight::new(&light, sky_default);
    let block = resolved.block_light(lx, ly, lz);
    let sky = resolved.sky_light(lx, ly, lz);
    Some((sky << 4) | block)
}

fn run(
    origin: Origin,
    protocol: i32,
    tx: Sender<NetUpdate>,
    action_rx: Receiver<ClientAction>,
    stop: Arc<AtomicBool>,
    shared_handle: SharedHandle,
    weather: SharedWeather,
    biome_climates: SharedBiomeClimates,
    session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
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

        // Start the integrated server *before* the client, when this is a
        // singleplayer session. Its serving task goes onto this thread's runtime
        // (we are inside `block_on`, so a runtime is entered), and the handle is
        // held for the whole session below: **dropping `IntegratedServer` aborts
        // the serving task**, so binding it inside a `match` arm would kill the
        // server the instant the arm ended.
        let mut integrated_server = None;
        let (server, auth, integrated_io) = match origin {
            Origin::Remote { host, port, auth } => (ServerAddress { host, port }, auth, None),
            Origin::Integrated {
                protocol: server_protocol,
                seed,
                view_radius,
            } => {
                tracing::info!(
                    target: "net",
                    seed,
                    view_radius,
                    "starting the integrated server (singleplayer)"
                );
                // The bundled, JVM-verified overworld generator — the same one
                // `crate::worldgen` calls directly for the dev world, reached
                // through the server's `ChunkSource` so the client sees it over
                // the real wire instead of by a local shortcut. Generation is
                // lazy per column, but the *initial view* is generated before the
                // client can finish loading: at ~12 ms/column (see
                // `docs/chunk-memory-pool-footprint.md`) a radius-8 view is a few
                // seconds on this thread, which is why the shell stays on the
                // loading screen rather than the world appearing instantly.
                let source = lodestone_server::overworld_chunk_source(seed);
                // Issue #217: `MobSim` computed AI motion server-side with no
                // production consumer streaming it anywhere — an island by its
                // own module doc's admission. `open_in_memory_with_mobs` is
                // the production wiring: it spawns a second task that owns a
                // live `MobSim` over its own snapshot of the same
                // (deterministically regenerated — same seed) terrain and
                // republishes positions every tick through the entity-sync
                // pass `serve_connection` already runs on this connection's
                // own inbound-packet cadence. `wasm32` gets the old
                // mob-free path: the tick loop needs `tokio::time`, which is
                // unavailable there (see `lodestone_server`'s own doc
                // comment on `mobs::run_mob_tick_loop`) — a real, documented
                // gap, not a silent one.
                #[cfg(not(target_arch = "wasm32"))]
                let (server, client_io) = {
                    // A small fixed radius around the join spawn (chunk
                    // (0,0), matching `V770ServerProtocol::begin_play`'s
                    // hardcoded `spawn_x`/`spawn_z` = 8) — independent of
                    // the client's own (possibly much larger) view radius,
                    // since this only needs to be big enough for a handful
                    // of wandering mobs, not the whole streamed view.
                    let mob_radius = view_radius.clamp(1, 3);
                    let mob_world_source = lodestone_server::overworld_chunk_source(seed);
                    lodestone_server::IntegratedServer::open_in_memory_with_mobs(
                        server_protocol,
                        source,
                        mob_world_source,
                        (-mob_radius..=mob_radius, -mob_radius..=mob_radius),
                        (8, 8),
                        6,
                        view_radius,
                    )
                };
                #[cfg(target_arch = "wasm32")]
                let (server, client_io) = lodestone_server::IntegratedServer::open_in_memory(
                    server_protocol,
                    source,
                    view_radius,
                );
                integrated_server = Some(server);
                let (host, port) = SINGLEPLAYER_ADDRESS;
                (
                    ServerAddress {
                        host: host.to_string(),
                        port,
                    },
                    None,
                    Some(client_io),
                )
            }
        };

        // Online mode (issue #65) supplies the account's real identity;
        // offline mode keeps the existing unique-per-run name so
        // `lodestone-testsupport`'s dead-player-blackout hazard (see that
        // crate's docs and `CLAUDE.md`) stays avoided for every test oracle,
        // every one of which is an offline server. Singleplayer takes the offline
        // path too: the integrated server persists no player file, so the hazard
        // does not exist there, but there is also no account to name it after.
        let profile = match &auth {
            Some(session) => LoginProfile {
                username: session.profile.name.clone(),
                uuid: session.profile.id,
            },
            None => LoginProfile {
                username: unique_username(),
                uuid: uuid::Uuid::new_v4(),
            },
        };

        // §4.1(c): fold into the driver's `World` when we were given one. The
        // builder installs no plugins and spawns no entity — the shell's `App`
        // already carries `IngestPlugin`/`SessionPlugin` and the entity is
        // `Sim.local` — because `add_systems` does not deduplicate.
        //
        // `RespawnPolicy::Manual` (issue #103): the library's default,
        // `Automatic`, answers every `Death` event with an unconditional
        // `ClientAction::Respawn`, which is what let the shell ride through
        // death with no screen at all — the death packet arrived and left
        // again inside one library call, before the shell ever got a chance to
        // react. Manual makes death a real gate: `Sim` marks the player dead
        // and waits, the shell shows the death screen, and only a click on its
        // Respawn button (`Sim::respawn`) sends the action.
        let mut builder = ClientBuilder::new(server, profile, adapter)
            .connect_timeout(Some(Duration::from_secs(10)))
            .respawn_policy(RespawnPolicy::Manual);
        if let Some(session) = auth {
            builder = builder.online_session(session);
        }
        if let Some((world, entity)) = session {
            builder = builder.ecs(world, entity);
        }
        // The `Transport` seam, and the only line where the two session kinds
        // diverge after setup. `connect_with` is infallible — the duplex is
        // already open, there is nothing to dial and no timeout to miss — which
        // is why singleplayer cannot produce a "connect:" error.
        let (handle, mut events) = match integrated_io {
            Some(client_io) => builder.connect_with(client_io),
            None => match builder.connect().await {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(NetUpdate::Error(format!("connect: {e}")));
                    return;
                }
            },
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
                    if forward(&tx, &weather, &biome_climates, event).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(NetUpdate::Disconnected(Box::new(
                        lodestone_model::Text::literal("stream closed"),
                    )));
                    break;
                }
                Err(_timeout) => {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }

        // Singleplayer only: stop the server we started. Dropping the handle at
        // the end of this block would do it too, but saying so is what keeps the
        // *reason* `integrated_server` is a binding at all from looking like an
        // unused variable — and it is the read that stops the compiler saying so.
        if let Some(server) = integrated_server {
            tracing::info!(target: "net", "stopping the integrated server");
            server.trigger_shutdown();
        }
    });
}

/// Forward one event; `Err` signals the loop to stop.
///
/// `weather` is folded in place for the two arms that publish into it instead of
/// producing a [`NetUpdate`] — see [`WeatherCell`] for why. Those arms still live
/// **here**, in the router, rather than being intercepted in the net loop above:
/// this `match` is the one place a reader looks to answer "does anything consume
/// event X", and an event handled outside it is invisible to that reading. Three
/// separate islands have already been found in this one function.
fn forward(
    tx: &Sender<NetUpdate>,
    weather: &WeatherCell,
    biome_climates: &BiomeClimateCell,
    event: ClientEvent,
) -> Result<(), ()> {
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
            // Unlike `Death`'s `message` (flattened to plain text below, a
            // known, separately-tracked gap — see `docs/death-screen.md`),
            // `reason` is passed through unresolved: `Sim::poll_net` is the
            // read boundary that owns translation for this class of event
            // (issue #68), so flattening here would throw the translation
            // key away before it ever reaches `Sim::translator()`.
            let _ = tx.send(NetUpdate::Disconnected(Box::new(reason)));
            return Err(());
        }
        // No `HealthChanged`/`ExperienceChanged` arms: those fold into the
        // `Vitals`/`Xp` components on the net thread, and forwarding them here as
        // well would put a second writer on the shell side. See `NetUpdate`'s note
        // where the two variants used to be.
        ClientEvent::Death { message } => NetUpdate::Death {
            message: message.to_plain_string(),
        },
        ClientEvent::Respawned { .. } => NetUpdate::Respawned,
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
        // The tab-list and scoreboard families used to be forwarded here as
        // `NetUpdate::{TabListEvent, ScoreboardEvent}` for the shell to fold a
        // *second* time. Since Stage 3 of `docs/bevy-migration.md` the client's
        // own `NetIngest` systems are the only fold and the shell reads the
        // result through `NetClient::{scoreboard, tab_list}`, so forwarding them
        // would be re-creating the duplicate this stage deleted.
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
        // Block events, forwarded raw (issue #23). Until this arm existed the
        // event reached the terminal `_ =>` below and was dropped, which is why
        // chest lids never moved. The two bytes are per-block-type and are
        // interpreted by `Sim::poll_net`'s one consumer, not here.
        ClientEvent::BlockEvent { pos, b0, b1, .. } => NetUpdate::BlockEvent {
            pos: [pos.x, pos.y, pos.z],
            b0,
            b1,
        },
        // The item-pickup fly-to-collector animation (issue #365), forwarded
        // **raw** for the same reason `TitleEvent` is: the one consumer is a
        // `lodestone-game` fold that already takes a `&ClientEvent`
        // (`lodestone_game::mining::PickupFeed::apply`), and re-typing the three
        // fields here only to rebuild the event on the far side would put a second
        // spelling of the same record in the tree.
        //
        // Until this arm existed the event fell through the terminal `_ =>` below —
        // the decode (`v770`'s `TAKE_ITEM_ENTITY`) and the fold (`PickupFeed`) were
        // both correct, both tested, and reached zero pixels. Third instance of the
        // island in this one router, after `BLOCK_EVENT`.
        event @ ClientEvent::ItemPickup { .. } => NetUpdate::ItemPickup(event),
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
        // World weather (`GAME_EVENT` codes 1, 2, 7, 8). Folded into the shared
        // [`WeatherCell`] and **deliberately not** forwarded: the levels change
        // every tick while the server ramps them and only the newest value
        // matters, so a channel would carry ~20 superseded messages a second.
        //
        // Until this arm existed the event reached the terminal `_ =>` below and
        // was dropped. The decode has been correct and hermetically tested since
        // it was written (`crates/protocol/v770/tests/world_events.rs` has five
        // `game_event_*` tests, including one asserting rain **and** thunder
        // levels are surfaced) and `ClientEvent::WeatherChanged` had **zero**
        // consumers anywhere in the tree — not in `ingest::handles_event`, not in
        // `session::handles_event`, not here. Fourth island in this router, after
        // `BLOCK_EVENT`, `ItemPickup`, and the sound family.
        //
        // Neither ECS router was the right home, per `CLAUDE.md`'s rule of thumb:
        // rain level is not per-entity state (so not `ingest`) and not a
        // local-player scalar (so not `session`) — it is *world* state, which
        // travels this stream, exactly as `BlockEvent` does.
        ClientEvent::WeatherChanged {
            raining,
            rain_level,
            thunder_level,
        } => {
            weather.apply(raining, rain_level, thunder_level);
            return Ok(());
        }
        // Every biome's declared climate (issue #25/#26's shared biome lane),
        // emitted at the same `Login` moment as `BiomeVisuals`. Folded into the
        // shared `BiomeClimateCell` and **deliberately not forwarded** — same
        // reasoning as `WeatherChanged` just above: the whole table replaces
        // at once, so there is nothing to queue.
        //
        // Until this arm existed the event reached the terminal `_ =>` below
        // and the `debug_assert!` there fired on every login once `v770`
        // started emitting it (`route` claims `shell`/`must_forward` for this
        // variant), which is how this gap was found rather than merely
        // theorised — `app::tests::pressing_play_reaches_a_running_integrated_server`
        // was red on `main` from a background-thread panic before this arm
        // existed.
        ClientEvent::BiomeClimates {
            temperatures,
            downfall,
            has_precipitation,
        } => {
            biome_climates.apply(&temperatures, &downfall, &has_precipitation);
            return Ok(());
        }
        // The lightning flash (`ClientLevel.java:264-268`). A bolt is an ordinary
        // entity on the wire, so this arm **observes** the spawn and returns
        // without producing a `NetUpdate`: entities already reach the shell
        // through the ECS ingest fold, and forwarding one here would put a second
        // writer on state that has one. Only the *count* is published.
        //
        // This is a spawn-only approximation. Vanilla re-flashes `rand(3) + 1`
        // times per bolt by resetting the entity's `life`
        // (`LightningBolt.java:47`, `:131-134`), which needs the bolt's own
        // per-tick state; see `lodestone_render::weather::LIGHTNING_FLASH_TICKS`.
        ClientEvent::EntitySpawned { ref entity_type, .. }
            if entity_type.path() == "lightning_bolt" =>
        {
            weather.strike();
            return Ok(());
        }
        // Everything else (keep-alive, entities, time, player list, chunk
        // unloads) isn't needed by the shell yet.
        //
        // The `debug_assert!` is the only thing standing between this arm and the
        // island class `CLAUDE.md` §1 names — **four** have already been found in
        // this one function (`BLOCK_EVENT`, `ItemPickup`, the sound family,
        // `WeatherChanged`), each a correct, tested decode reaching zero pixels.
        // `lodestone_model::event::route` is an *exhaustive* table beside the
        // `ClientEvent` declaration (`#[non_exhaustive]` does not bind inside the
        // defining crate), so a variant that says it belongs on the shell's stream
        // and has no arm above now fails loudly in every debug test and oracle run
        // instead of quietly costing a chest lid.
        //
        // This function deliberately stays non-exhaustive: a ~100-arm match does
        // not belong in a file this contended. `must_forward()` excludes the two
        // *guarded* arms above — `LevelEvent`'s literal `2001` and
        // `EntitySpawned`'s `lightning_bolt` — whose other values reach here
        // legitimately. See `docs/event-routing.md`.
        ref other => {
            debug_assert!(
                !lodestone_model::event::route(other).must_forward(),
                "`lodestone_model::event::route` routes this event to the shell, but \
                 `forward` has no arm for it, so it reaches zero pixels: {other:?}"
            );
            return Ok(());
        }
    };
    tx.send(update).map_err(|_| ())
}

/// Lower a client [`EntityView`] into a version-free [`EntitySnapshot`] for the
/// renderer: the type key's `path()` selects the model, and the `baby` flag maps
/// to a uniform render scale. Baby scale is a single 0.5 approximation for every
/// ageable mob (vanilla varies it per type); good enough to read a baby as
/// smaller, and noted as a refinement rather than a fake.
///
/// # What the item stack loses here, and why the loss is on this side
///
/// [`EntitySnapshot`] deliberately depends on neither `lodestone-client` nor
/// `lodestone-model` for its *typed* payloads — that is what lets `entities.rs`
/// be unit-tested with no server and no GPU — so the model's
/// [`ItemStack`](lodestone_model::ItemStack) cannot cross into it wholesale.
/// This function is the one place that knows both types, so the conversion
/// lives here and keeps only the item *key*, as a
/// [`ResourceLocation`](lodestone_assets::ResourceLocation), plus its `count`
/// as a plain `u32` (see [`EntitySnapshot::count`] — a bare integer needs no
/// model dependency, unlike the key). One thing is still dropped:
///
/// * **`components`** — dyed leather colour, custom model data, trim, and the
///   `has_unmodeled` marker. These change how an item *looks* but not *which*
///   item it is, and nothing in the item pipeline reads them yet.
///
/// `count` used to be dropped here too, and that loss was *visible*: vanilla's
/// `ItemEntityRenderer` draws up to five jittered copies of the model for a
/// large stack (`ItemClusterRenderState::getRenderedAmount`: 1 copy at count ≤
/// 1, then 2, 3, 4, 5 as the count passes 1, 16, 32 and 48), so a stack of 64
/// diamonds drew as a single diamond. The multi-copy *draw* is not wired yet —
/// see `docs/dropped-items.md` — but the count itself now reaches
/// [`EntityDraw::count`](crate::entities::EntityDraw::count) with no model
/// dependency needed to get it there.
///
/// The three states are preserved: [`Reported::Unreported`] stays "never
/// reported", [`Reported::Reported(None)`](Reported::Reported) stays
/// "explicitly empty". A key that somehow fails `ResourceLocation` validation
/// degrades to `Unreported` rather than to `Reported(None)`, so a malformed id
/// can never masquerade as the server clearing the stack.
///
/// [`EntityView::equipment`] goes through the same narrowing — `EquipmentSlot`
/// is a `lodestone-model` type, which `EntitySnapshot` *does* depend on (it is a
/// plain data enum with no wire or version knowledge), so only the `ItemStack` is
/// reduced to its key. Same `count`/`components` losses apply, and they are
/// currently invisible for equipment: nothing renders a stack size or a dye
/// colour in a mob's hand.
fn entity_snapshot(view: EntityView, tab_list: &TabList) -> EntitySnapshot {
    let scale = if view.baby == Some(true) { 0.5 } else { 1.0 };
    // Borrowed, ahead of the by-value `item` match below: `count` only exists
    // on the wire's `ItemStack`, which that match consumes converting the key.
    // `1` is the neutral default for every case where there is no stack to
    // count — matches `EntitySnapshot::count`'s documented contract.
    let count = match &view.item {
        Reported::Reported(Some(stack)) => stack.count,
        _ => 1,
    };
    let item = match view.item {
        Reported::Unreported => Reported::Unreported,
        Reported::Reported(None) => Reported::Reported(None),
        // A failed conversion must collapse to `Unreported` ("nothing
        // reported"), never to `Reported(None)`, which downstream reads as
        // the server clearing the stack.
        Reported::Reported(Some(stack)) => {
            match lodestone_assets::ResourceLocation::new(stack.item.namespace(), stack.item.path())
            {
                Ok(id) => Reported::Reported(Some(id)),
                Err(_) => Reported::Unreported,
            }
        }
    };
    // `EntityView::equipment` is the *accumulated* per-slot state
    // (`lodestone_client::state` merges each `set_equipment` update into it and
    // never clears), so every poll carries the complete current set and the
    // consumer can replace wholesale. The nesting is preserved exactly as the
    // view documents it: a slot **absent** from the list is "the server has
    // never mentioned it", a slot present with `None` is an explicit "this slot
    // is empty". Collapsing the two would make an armourless mob
    // indistinguishable from one whose armour the server has confirmed gone.
    //
    // A key that fails `ResourceLocation` validation drops the whole *entry*
    // rather than degrading to `Some(slot, None)` — same rule as `item` above:
    // a malformed id must read as "not reported", never as the server clearing
    // the slot.
    let equipment = view
        .equipment
        .iter()
        .filter_map(|eq| match &eq.item {
            None => Some((eq.slot, None)),
            Some(stack) => {
                lodestone_assets::ResourceLocation::new(stack.item.namespace(), stack.item.path())
                    .ok()
                    .map(|id| (eq.slot, Some(id)))
            }
        })
        .collect();
    // Narrowed the same way `equipment` is, per `EntityDraw::equipment_dye`'s
    // own doc: a slot only carries a dye if its item is present *and* its id
    // validates. Emitting a dye for a slot `equipment` dropped would describe
    // a tint on an item the renderer was never told about.
    let equipment_dye = view
        .equipment
        .iter()
        .filter_map(|eq| {
            let stack = eq.item.as_ref()?;
            lodestone_assets::ResourceLocation::new(stack.item.namespace(), stack.item.path())
                .ok()?;
            Some((eq.slot, stack.components.dyed_color?))
        })
        .collect();
    // Nametag resolution (issue #100). Two entirely different rules, per the
    // real 26.2 client:
    //
    // * **A player's tag is always its tab-list display name.**
    //   `Player.shouldShowName()` unconditionally returns `true`
    //   (`Player.java:1637`), overriding `Entity.shouldShowName() =
    //   isCustomNameVisible()` (`Entity.java:3372`) that every other entity
    //   uses — a player is never gated on `CUSTOM_NAME_VISIBLE`. The name
    //   itself comes from `Entity.getDisplayName()`, which for a player is
    //   scoreboard-team-prefixed in vanilla; team colouring/prefixes are out
    //   of scope here (issue #100), so this is the plain tab-list name.
    // * **Every other entity's tag is its custom name**, gated on
    //   `CUSTOM_NAME_VISIBLE` (`LivingEntity.shouldShowName()` =
    //   `isCustomNameVisible()`, `LivingEntity.java:2364`/`2365`) — vanilla
    //   would additionally fall back to the entity's translated type name
    //   when `CUSTOM_NAME_VISIBLE` is set with no custom name, which this
    //   deliberately does not reproduce (out of scope: "entities with a
    //   non-empty custom name" per the issue).
    let is_player = view.entity_type.path() == "player";
    let name_tag = if is_player {
        view.uuid
            .and_then(|id| tab_list.get(&id))
            .map(|entry| entry.effective_name().to_plain_string())
            .filter(|name| !name.is_empty())
    } else {
        match &view.custom_name {
            Reported::Reported(Some(name))
                if view.custom_name_visible == Some(true) && !name.is_empty() =>
            {
                Some(name.clone())
            }
            _ => None,
        }
    }
    .map(|text| NameTag {
        text,
        // `Entity.isDiscrete()` is `isShiftKeyDown()` (`Entity.java:2703`,
        // `:2704`), bit 1 of the shared-flags byte
        // (`FLAG_SHIFT_KEY_DOWN = 1`, `Entity.java:262`). Vanilla submits the
        // see-through pass only when `!isDiscrete()`
        // (`SubmitNodeCollection.java:109`) — a sneaking entity's tag never
        // shows through terrain. `flags` unknown (no metadata yet) defaults
        // open, matching every other not-yet-reported boolean here.
        see_through: view.flags.map_or(true, |f| f & 0x02 == 0),
    });
    EntitySnapshot {
        // `EntityView::creeper_swell_dir` is populated by `CreeperSwellDir`
        // (`lodestone-ecs::entity`), folded in `ingest.rs`'s `apply_metadata`.
        // `entities.rs`'s `CreeperFuse`/`tick_creeper_fuse`/
        // `EntityDraw::creeper_swelling` chain reads this value directly —
        // see `docs/entity-rendering.md`'s "Creeper swell" section.
        creeper_swell_dir: view.creeper_swell_dir,
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
        item,
        // The client-owned read-model already decodes `add_entity`'s and
        // `set_entity_motion`'s velocity into `EntityView::velocity` (see
        // `lodestone_client::state`'s `EntityMoved`/`EntityVelocity` arms) —
        // it was simply never read past that point. `EntityInterpolator`
        // (`entities.rs`) is the consumer: a dropped item needs this to arc
        // under gravity instead of easing between the ~1/s position
        // corrections vanilla's `ItemEntity` actually sends. See that
        // module's docs for why.
        velocity: view.velocity.map(|v| {
            glam::Vec3::new(v.x as f32, v.y as f32, v.z as f32)
        }),
        on_ground: view.on_ground,
        // The second half of the same gap the velocity note above describes:
        // `SET_EQUIPMENT` already decoded into `EntityView::equipment` and it was
        // simply never read past this boundary, so `EntityInterpolator` had no way
        // to know a zombie was holding a sword even though the wire data was
        // sitting right here. Nothing downstream of this function could see it.
        equipment,
        // `docs/armour-rendering.md`'s "hop 2", now closed. The comment this
        // replaces said `EntityView` carried no dye data — true when written,
        // and stale as of `64cfdcb`, which decoded `minecraft:dyed_color` into
        // `ItemComponents::dyed_color`. The dye was already arriving *inside*
        // `view.equipment`'s own `ItemStack`s; nothing new had to reach this
        // function, which is why the stale note is worth naming rather than
        // just deleting.
        equipment_dye,
        // A third instance of the same gap: `EntityView::variant` has been fully
        // decoded (down to `EntityVariant::Dyed`'s sheep colour/shear bit) since
        // `lodestone_client::state`'s `Variant` component fold, and this function
        // simply never read it either. See `docs/entity-rendering.md`'s "Render
        // layers: sheep wool" section for the rest of the chain this unblocks.
        variant: view.variant,
        count,
        name_tag,
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
        let client = NetClient::connect("127.0.0.1".into(), 1, 776, None);
        let _ = client.poll();
    }

    /// The vitals events must **not** cross this channel any more.
    ///
    /// This replaces `forward_translates_experience_changed`, and it is the
    /// negative control for the collapse: `SharedState::apply` folds
    /// `HealthChanged`/`ExperienceChanged` into `Vitals`/`Xp` on the net thread, so
    /// a `NetUpdate` for either would be a second writer of a component that
    /// already has one. Re-adding a `forward` arm for them silently reintroduces
    /// the duplicate fold this test exists to forbid.
    #[test]
    fn the_vitals_events_are_not_forwarded_to_the_shell_at_all() {
        let (tx, rx) = mpsc::channel();
        let weather = WeatherCell::default();
        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            ClientEvent::ExperienceChanged {
                progress: 0.25,
                level: 5,
                total: 55,
            },
        )
        .expect("forward does not stop the loop");
        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            ClientEvent::HealthChanged {
                health: 12.0,
                food: 8,
                saturation: 1.5,
            },
        )
        .expect("forward does not stop the loop");
        assert!(
            rx.try_recv().is_err(),
            "the vitals fold lives in `lodestone_ecs::session`; nothing may cross this channel"
        );
        // …and the control that `forward` is genuinely running: an event that
        // *does* have a shell-side reaction still arrives, and carries its
        // message flattened to plain text (issue #103's death screen reads
        // this straight off `NetUpdate::Death`, through `Sim::death_message`).
        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            ClientEvent::Death {
                message: lodestone_client::Text::literal("you died"),
            },
        )
        .expect("forward does not stop the loop");
        match rx.try_recv().expect("Death still crosses") {
            NetUpdate::Death { message } => assert_eq!(message, "you died"),
            other => panic!("expected NetUpdate::Death, got {other:?}"),
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
        forward(&tx, &WeatherCell::default(), &BiomeClimateCell::default(), event).expect("forward does not stop the loop");
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
        forward(&tx, &WeatherCell::default(), &BiomeClimateCell::default(), event).expect("forward does not stop the loop");
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
        forward(&tx, &WeatherCell::default(), &BiomeClimateCell::default(), event).expect("forward does not stop the loop");
        match rx.try_recv().expect("an update was forwarded") {
            NetUpdate::EffectRemoved { entity_id, effect } => {
                assert_eq!(entity_id, 99);
                assert_eq!(effect, "levitation");
            }
            other => panic!("expected EffectRemoved, got {other:?}"),
        }
    }

    /// The island this feature closed: `ClientEvent::WeatherChanged` was decoded,
    /// hermetically tested in `protocol/v770/tests/world_events.rs`, and consumed
    /// by **nothing** in the tree. This asserts the fold happens *and* that it
    /// stays off the channel.
    #[test]
    fn forward_folds_weather_into_the_cell_without_using_the_channel() {
        let (tx, rx) = mpsc::channel();
        let weather = WeatherCell::default();
        assert_eq!(weather.snapshot(), WeatherSnapshot::default());

        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            ClientEvent::WeatherChanged {
                raining: None,
                rain_level: Some(0.75),
                thunder_level: None,
            },
        )
        .expect("forward does not stop the loop");
        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            ClientEvent::WeatherChanged {
                raining: None,
                rain_level: None,
                thunder_level: Some(0.5),
            },
        )
        .expect("forward does not stop the loop");

        let snapshot = weather.snapshot();
        assert_eq!(snapshot.rain_level, 0.75, "RAIN_LEVEL_CHANGE must land");
        assert_eq!(
            snapshot.thunder_level, 0.5,
            "THUNDER_LEVEL_CHANGE must land, and must land *raw* — composing it \
             with rain is `WeatherState::thunder_level`'s job"
        );
        assert!(
            rx.try_recv().is_err(),
            "weather must not cross the NetUpdate channel: only the newest level \
             matters and the server sends one every tick while it ramps"
        );

        // The `START_RAINING` inversion reaches here intact (see
        // `lodestone_render::weather`'s module doc — this is vanilla's own
        // polarity at ClientPacketListener.java:1543, not a bug on this side).
        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            ClientEvent::WeatherChanged {
                raining: Some(true),
                rain_level: None,
                thunder_level: None,
            },
        )
        .expect("forward does not stop the loop");
        assert_eq!(weather.snapshot().rain_level, 0.0);
        assert_eq!(
            weather.snapshot().thunder_level, 0.5,
            "a start/stop must not disturb the thunder level"
        );
    }

    /// The `BiomeClimates` twin of the `WeatherChanged` test above: before
    /// this arm existed, the event reached the terminal `_ =>` and the
    /// `debug_assert!` there fired on every login (`route` claims
    /// `shell`/`must_forward` for it) — this asserts the fold happens *and*
    /// stays off the channel, matching `WeatherChanged`'s own shape.
    #[test]
    fn forward_folds_biome_climates_into_the_cell_without_using_the_channel() {
        let (tx, rx) = mpsc::channel();
        let weather = WeatherCell::default();
        let climates = BiomeClimateCell::default();
        assert_eq!(climates.get(0), None, "empty before Login");

        forward(
            &tx,
            &weather,
            &climates,
            ClientEvent::BiomeClimates {
                temperatures: vec![Some(-0.7), Some(2.0)],
                downfall: vec![Some(0.9), Some(0.0)],
                has_precipitation: vec![Some(true), Some(false)],
            },
        )
        .expect("forward does not stop the loop");

        assert_eq!(
            climates.get(0),
            Some(BiomeClimateEntry {
                temperature: Some(-0.7),
                downfall: Some(0.9),
                has_precipitation: Some(true),
            }),
            "holder id 0 must round-trip exactly"
        );
        assert_eq!(
            climates.get(1),
            Some(BiomeClimateEntry {
                temperature: Some(2.0),
                downfall: Some(0.0),
                has_precipitation: Some(false),
            })
        );
        assert_eq!(climates.get(2), None, "out of range must not fabricate an entry");
        assert!(
            rx.try_recv().is_err(),
            "biome climates must not cross the NetUpdate channel — the whole \
             table replaces at once, exactly like weather"
        );
    }

    /// `BiomeClimateCell` carrying real vanilla biome data (frozen_peaks and
    /// desert, `temperature`/`has_precipitation`/`downfall` copied verbatim
    /// from `.cache/mc/26.2/src/data/minecraft/worldgen/biome/{frozen_peaks,
    /// desert}.json`) must, once vanilla's own `getPrecipitationAt` predicate
    /// is applied, land on the correct side of the rain/snow line:
    /// `Biome.java:176`, `return this.getTemperature(pos, seaLevel) >= 0.15F;`
    /// (called from `getPrecipitationAt` at `:108`, gated on `hasPrecipitation()`
    /// at `:105-106`).
    ///
    /// This is the exact-threshold assertion the #25 report's gate asks for,
    /// kept hermetic (no live server needed) by testing `BiomeClimateCell`
    /// directly rather than the full `ClientHandle`-dependent hop — that hop
    /// is covered by `app::tests::
    /// live_precipitation_matches_vanillas_own_threshold_for_real_biomes`
    /// against a real oracle, where spawn-biome data happens to be warm
    /// (`Rain`) every run; this test is what proves the `Snow` branch is
    /// reachable at all, deterministically.
    #[test]
    fn a_real_frozen_biome_crosses_vanillas_own_rain_snow_threshold_and_a_dry_one_does_not() {
        let climates = BiomeClimateCell::default();
        climates.apply(
            // 0 = frozen_peaks, 1 = desert — real 26.2 values, not invented.
            &[Some(-0.7), Some(2.0)],
            &[Some(0.9), Some(0.0)],
            &[Some(true), Some(false)],
        );

        let frozen_peaks = climates.get(0).expect("holder id 0 must resolve");
        let desert = climates.get(1).expect("holder id 1 must resolve");

        // Vanilla's threshold, 0.15, applied at sea level (no height falloff
        // at y == sea_level, so `getHeightAdjustedTemperature` is a no-op —
        // this isolates the threshold itself from the height term).
        const WARM_ENOUGH_TO_RAIN: f32 = 0.15; // Biome.java:176
        assert!(
            frozen_peaks.temperature.unwrap() < WARM_ENOUGH_TO_RAIN,
            "frozen_peaks' real temperature ({:?}) must be below vanilla's \
             own 0.15 threshold, or this test is not exercising the branch \
             it claims to",
            frozen_peaks.temperature
        );
        assert_eq!(
            frozen_peaks.has_precipitation,
            Some(true),
            "frozen_peaks really does have precipitation — snow, not a dry cold snap"
        );
        assert_eq!(
            desert.has_precipitation,
            Some(false),
            "desert has no precipitation regardless of temperature — the \
             `has_precipitation` gate must short-circuit before the \
             threshold is even consulted (Biome.java:105-106)"
        );
    }

    /// The lightning arm: it must fire for a bolt, must **not** fire for any other
    /// entity, and must never put an entity spawn on the channel (entities reach
    /// the shell through the ECS fold, which is the only writer).
    #[test]
    fn forward_counts_lightning_bolts_and_ignores_every_other_spawn() {
        use lodestone_client::ResourceKey;
        use std::str::FromStr;

        let spawn = |kind: &str| ClientEvent::EntitySpawned {
            entity_id: 7,
            uuid: None,
            entity_type: ResourceKey::from_str(kind).unwrap(),
            pos: Vec3::new(0.0, 64.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        };

        let (tx, rx) = mpsc::channel();
        let weather = WeatherCell::default();

        // The negative control first, so a passing positive cannot be "every
        // spawn bumps it".
        forward(&tx, &weather, &BiomeClimateCell::default(), spawn("minecraft:zombie")).expect("forward continues");
        assert_eq!(
            weather.snapshot().lightning_seq,
            0,
            "a zombie must not flash the sky"
        );

        forward(&tx, &weather, &BiomeClimateCell::default(), spawn("minecraft:lightning_bolt")).expect("forward continues");
        assert_eq!(weather.snapshot().lightning_seq, 1);
        forward(&tx, &weather, &BiomeClimateCell::default(), spawn("minecraft:lightning_bolt")).expect("forward continues");
        assert_eq!(
            weather.snapshot().lightning_seq,
            2,
            "the counter is a sequence, so two bolts in one session are two flashes"
        );
        assert!(
            rx.try_recv().is_err(),
            "an entity spawn must not cross this channel — the ECS ingest fold owns it"
        );
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

    /// Builds a minimal [`EntityView`] for [`entity_snapshot`] tests — only
    /// the fields that function actually reads need real values, the rest are
    /// "never reported".
    fn bare_entity_view(velocity: Option<Vec3>, on_ground: bool) -> EntityView {
        use lodestone_client::ResourceKey;
        use std::str::FromStr;

        EntityView {
            entity_id: 9,
            uuid: None,
            entity_type: ResourceKey::from_str("minecraft:item").unwrap(),
            position: Vec3::new(1.0, 64.0, 2.0),
            rotation: Rotation::new(0.0, 0.0),
            head_yaw: 0.0,
            velocity,
            on_ground,
            flags: None,
            custom_name: Reported::Unreported,
            custom_name_visible: None,
            pose: None,
            health: None,
            baby: None,
            variant: None,
            creeper_swell_dir: None,
            attributes: Vec::new(),
            equipment: Vec::new(),
            item: Reported::Unreported,
        }
    }

    /// The gap this fix closed: `SET_ENTITY_MOTION`/`add_entity` already
    /// decoded into `EntityView::velocity` (see
    /// `lodestone_client::state`'s fold), and `EntityView::on_ground` has
    /// always been tracked — but `entity_snapshot` dropped both on the floor
    /// before they ever reached `EntitySnapshot`, so `EntityInterpolator` had
    /// no way to know a dropped item's velocity even though the wire data was
    /// sitting right there. Before this change neither field existed on
    /// `EntitySnapshot` at all.
    #[test]
    fn entity_snapshot_carries_velocity_and_on_ground_through() {
        let view = bare_entity_view(Some(Vec3::new(0.08, 0.2, 0.0)), false);
        let snap = entity_snapshot(view, &TabList::new());
        assert_eq!(
            snap.velocity,
            Some(glam::Vec3::new(0.08, 0.2, 0.0)),
            "the decoded velocity must survive the EntityView -> EntitySnapshot boundary"
        );
        assert!(!snap.on_ground);

        let grounded = bare_entity_view(None, true);
        let snap = entity_snapshot(grounded, &TabList::new());
        assert_eq!(
            snap.velocity, None,
            "a never-reported velocity must stay None, not collapse to zero"
        );
        assert!(snap.on_ground);
    }

    /// The same shape of gap as the velocity fix above, one field over:
    /// `SET_EQUIPMENT` already folded into `EntityView::equipment` (see
    /// `lodestone_client::state`'s `EntityEquipmentUpdate` arm) and this function
    /// dropped it, so `EntityInterpolator` could never learn that a mob was
    /// holding anything. Before this change `EntitySnapshot` had no such field at
    /// all, which is why nothing downstream reported an error.
    #[test]
    fn entity_snapshot_carries_equipment_through() {
        use lodestone_model::ItemStack;
        use lodestone_model::event::{EntityEquipment, EquipmentSlot};
        use std::str::FromStr;

        let mut view = bare_entity_view(None, true);
        view.equipment = vec![
            EntityEquipment {
                slot: EquipmentSlot::MainHand,
                item: Some(ItemStack::new(
                    lodestone_client::ResourceKey::from_str("minecraft:diamond_sword").unwrap(),
                    1,
                )),
            },
            // An explicit clear: present in the list, empty in the slot. This
            // must survive as `Some(slot, None)`, not vanish.
            EntityEquipment {
                slot: EquipmentSlot::Head,
                item: None,
            },
        ];

        let snap = entity_snapshot(view, &TabList::new());
        assert_eq!(
            snap.equipment.len(),
            2,
            "both an occupied and an explicitly-cleared slot must cross the boundary"
        );
        let main = snap
            .equipment
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::MainHand)
            .expect("main hand survived");
        assert_eq!(
            main.1.as_ref().map(ToString::to_string).as_deref(),
            Some("minecraft:diamond_sword")
        );
        let head = snap
            .equipment
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::Head)
            .expect("head slot survived");
        assert_eq!(
            head.1, None,
            "an explicitly-empty slot must stay present-and-empty, not be dropped"
        );

        // Control: a mob the server has said nothing about carries nothing, so a
        // consumer cannot mistake "no data" for "empty hands confirmed".
        let bare = entity_snapshot(bare_entity_view(None, true), &TabList::new());
        assert!(bare.equipment.is_empty());
    }

    /// The last hop of `docs/armour-rendering.md`'s dye chain. `64cfdcb` decoded
    /// `minecraft:dyed_color` and taught `prepare_armour` to tint with it, but
    /// this function passed `Vec::new()`, so every leather item rendered undyed
    /// while the wire data sat inside `view.equipment`'s own `ItemStack`s.
    ///
    /// The expected value comes from outside our code: vanilla's own default
    /// leather RGB is the literal `10511680` in
    /// `ItemStackComponentizationFix.java:250`, which writes it as `dyed_color`'s
    /// `rgb` when an old stack carries no explicit colour. That is `0x00A06540`.
    #[test]
    fn entity_snapshot_carries_equipment_dye_through() {
        use lodestone_model::ItemStack;
        use lodestone_model::event::{EntityEquipment, EquipmentSlot};
        use std::str::FromStr;

        const VANILLA_DEFAULT_LEATHER: u32 = 0x00A0_6540;

        let dyed = |path: &str, colour: Option<u32>| {
            let mut stack = ItemStack::new(
                lodestone_client::ResourceKey::from_str(path).unwrap(),
                1,
            );
            stack.components.dyed_color = colour;
            stack
        };

        let mut view = bare_entity_view(None, true);
        view.equipment = vec![
            EntityEquipment {
                slot: EquipmentSlot::Chest,
                item: Some(dyed(
                    "minecraft:leather_chestplate",
                    Some(VANILLA_DEFAULT_LEATHER),
                )),
            },
            // An undyeable item in an occupied slot must contribute no entry at
            // all — not a zero, which would read as "dyed pure black".
            EntityEquipment {
                slot: EquipmentSlot::Head,
                item: Some(dyed("minecraft:iron_helmet", None)),
            },
        ];

        let snap = entity_snapshot(view, &TabList::new());
        assert_eq!(
            snap.equipment_dye,
            vec![(EquipmentSlot::Chest, VANILLA_DEFAULT_LEATHER)],
            "only the dyed slot contributes, and it carries vanilla's exact RGB"
        );
        assert_eq!(
            snap.equipment.len(),
            2,
            "narrowing the dye list must not narrow `equipment` itself"
        );

        // Control: the same two items with the dye component absent must produce
        // an empty list, so the assertion above cannot pass on a build that
        // simply forwards every occupied slot with some placeholder colour.
        let mut undyed_view = bare_entity_view(None, true);
        undyed_view.equipment = vec![EntityEquipment {
            slot: EquipmentSlot::Chest,
            item: Some(dyed("minecraft:leather_chestplate", None)),
        }];
        let undyed = entity_snapshot(undyed_view, &TabList::new());
        assert!(
            undyed.equipment_dye.is_empty(),
            "no dye component reported means no dye, never a default"
        );
    }

    /// A third instance of the velocity/equipment gap:
    /// `EntityView::variant` was already fully decoded and simply never read
    /// past this boundary. This is the fix `docs/entity-rendering.md`'s
    /// "Render layers: sheep wool" section describes as the missing last hop.
    #[test]
    fn entity_snapshot_carries_variant_through() {
        use lodestone_model::event::EntityVariant;

        let mut view = bare_entity_view(None, true);
        view.variant = Some(EntityVariant::Dyed {
            color: 14,
            sheared: false,
        });
        let snap = entity_snapshot(view, &TabList::new());
        assert_eq!(
            snap.variant,
            Some(EntityVariant::Dyed {
                color: 14,
                sheared: false
            }),
            "a decoded variant must survive the EntityView -> EntitySnapshot boundary"
        );

        // Control: a mob the server has never sent a variant for must read as
        // `None`, not as some default variant.
        let bare = entity_snapshot(bare_entity_view(None, true), &TabList::new());
        assert_eq!(bare.variant, None);
    }

    /// The last hop of the creeper-swell chain `docs/entity-rendering.md`'s
    /// "Creeper swell" section names: `Creeper.DATA_SWELL_DIR` is fully
    /// decoded down to `EntityView::creeper_swell_dir`
    /// (`lodestone_client::state`'s `entity_view`, fed by
    /// `lodestone-ecs::CreeperSwellDir`) — this function was the one place
    /// that dropped it on the floor, hardcoding `None` regardless of what the
    /// server actually reported. `entities.rs`'s
    /// `CreeperFuse`/`tick_creeper_fuse`/`EntityDraw::creeper_swelling` chain
    /// reads only `EntitySnapshot::creeper_swell_dir`, so this is the
    /// boundary that made every creeper swell invisible end to end.
    #[test]
    fn entity_snapshot_carries_creeper_swell_dir_through() {
        let mut view = bare_entity_view(None, true);
        view.creeper_swell_dir = Some(1);
        let snap = entity_snapshot(view, &TabList::new());
        assert_eq!(
            snap.creeper_swell_dir,
            Some(1),
            "a decoded swell direction must survive the EntityView -> EntitySnapshot boundary"
        );

        // Control: an entity the server has never reported a swell direction
        // for (i.e. every non-creeper) must read as `None`, not some default
        // "growing" or "shrinking" direction.
        let bare = entity_snapshot(bare_entity_view(None, true), &TabList::new());
        assert_eq!(bare.creeper_swell_dir, None);
    }

    /// The visible half of the stack-count gap `docs/dropped-items.md`
    /// describes: `ItemStack::count` was decoded all the way to
    /// `EntityView::item` and dropped exactly at this conversion, so a stack of
    /// 64 diamonds and a single diamond were indistinguishable past this point.
    #[test]
    fn entity_snapshot_carries_item_count_through() {
        use lodestone_client::ResourceKey;
        use std::str::FromStr;

        let mut view = bare_entity_view(None, true);
        view.item = Reported::Reported(Some(lodestone_model::ItemStack::new(
            ResourceKey::from_str("minecraft:diamond").unwrap(),
            64,
        )));
        let snap = entity_snapshot(view, &TabList::new());
        assert_eq!(snap.count, 64);

        // Control: no stack at all must read as the neutral `1`, not `0` — a
        // consumer that multiplies by count must never draw zero copies of
        // nothing.
        let bare = entity_snapshot(bare_entity_view(None, true), &TabList::new());
        assert_eq!(bare.count, 1);
    }

    /// Issue #100's two nametag rules, each pinned directly against
    /// `entity_snapshot`'s real boundary rather than against the render
    /// path — the render-level pixel gate
    /// (`tests/nametag_pixels.rs`) proves the wiring end to end, this proves
    /// the *resolution logic* in isolation.
    mod name_tag {
        use std::str::FromStr;

        use lodestone_game::tablist::{GameProfile, PlayerListEntry};
        use uuid::Uuid;

        use super::*;

        fn player_view(uuid: Uuid) -> EntityView {
            let mut view = bare_entity_view(None, true);
            view.entity_type = lodestone_client::ResourceKey::from_str("minecraft:player").unwrap();
            view.uuid = Some(uuid);
            view
        }

        /// A player's tag is always its tab-list display name —
        /// `Player.shouldShowName()` returns `true` unconditionally
        /// (`Player.java:1637`), never gated on any metadata flag.
        #[test]
        fn a_player_entitys_tag_is_its_tab_list_display_name() {
            let id = Uuid::from_u128(1);
            let mut tabs = TabList::new();
            tabs.insert(PlayerListEntry::new(GameProfile::new(id, "Steve")));

            let snap = entity_snapshot(player_view(id), &tabs);
            assert_eq!(
                snap.name_tag.map(|t| t.text),
                Some("Steve".to_string()),
                "a player entity must show its tab-list name unconditionally"
            );
        }

        /// The other half: no matching tab-list entry (the player left, or a
        /// synthetic/demo entity claiming to be a player) draws nothing
        /// rather than a blank or placeholder tag.
        #[test]
        fn a_player_entity_with_no_tab_list_entry_has_no_tag() {
            let snap = entity_snapshot(player_view(Uuid::from_u128(2)), &TabList::new());
            assert_eq!(snap.name_tag, None);
        }

        /// Every other entity's tag is its `CUSTOM_NAME`, gated on
        /// `CUSTOM_NAME_VISIBLE` — `LivingEntity.shouldShowName() =
        /// isCustomNameVisible()` (`LivingEntity.java:2364`/`:2365`), unlike
        /// a player.
        #[test]
        fn a_mob_with_a_visible_custom_name_shows_it() {
            let mut view = bare_entity_view(None, true);
            view.custom_name = Reported::Reported(Some("Babe".to_string()));
            view.custom_name_visible = Some(true);
            let snap = entity_snapshot(view, &TabList::new());
            assert_eq!(snap.name_tag.map(|t| t.text), Some("Babe".to_string()));
        }

        /// The gate the base `Entity.shouldShowName()` predicate is: a
        /// custom name with `CUSTOM_NAME_VISIBLE` unset (or `false`) shows
        /// nothing, even though the name itself is known.
        #[test]
        fn a_mob_with_a_custom_name_but_not_visible_shows_nothing() {
            let mut view = bare_entity_view(None, true);
            view.custom_name = Reported::Reported(Some("Babe".to_string()));
            view.custom_name_visible = Some(false);
            let snap = entity_snapshot(view, &TabList::new());
            assert_eq!(
                snap.name_tag, None,
                "CUSTOM_NAME_VISIBLE=false must suppress the tag even though a name is known"
            );

            // Same for "never reported" — the common case for most mobs.
            let bare = entity_snapshot(bare_entity_view(None, true), &TabList::new());
            assert_eq!(bare.name_tag, None);
        }

        /// An explicitly empty custom name must not draw a zero-width
        /// visible tag — same rule the issue's scope names ("a non-empty
        /// custom name").
        #[test]
        fn a_mob_with_an_empty_custom_name_shows_nothing_even_if_visible() {
            let mut view = bare_entity_view(None, true);
            view.custom_name = Reported::Reported(Some(String::new()));
            view.custom_name_visible = Some(true);
            let snap = entity_snapshot(view, &TabList::new());
            assert_eq!(snap.name_tag, None);
        }

        /// `Entity.isDiscrete()` (`isShiftKeyDown()`, bit 1 of the shared
        /// flags byte) gates the see-through pass off while sneaking
        /// (`SubmitNodeCollection.java:109`).
        #[test]
        fn sneaking_suppresses_see_through_but_not_the_tag_itself() {
            let mut view = bare_entity_view(None, true);
            view.custom_name = Reported::Reported(Some("Babe".to_string()));
            view.custom_name_visible = Some(true);
            view.flags = Some(0x02); // FLAG_SHIFT_KEY_DOWN
            let snap = entity_snapshot(view, &TabList::new());
            let tag = snap.name_tag.expect("the tag itself must still draw while sneaking");
            assert_eq!(tag.text, "Babe");
            assert!(!tag.see_through, "sneaking must suppress the see-through pass");
        }

        /// The default (no metadata reported yet) must not suppress
        /// see-through — most entities aren't sneaking.
        #[test]
        fn unknown_flags_default_to_see_through_enabled() {
            let mut view = bare_entity_view(None, true);
            view.custom_name = Reported::Reported(Some("Babe".to_string()));
            view.custom_name_visible = Some(true);
            assert_eq!(view.flags, None, "control: this test is about the unreported case");
            let snap = entity_snapshot(view, &TabList::new());
            assert!(snap.name_tag.expect("tag must draw").see_through);
        }
    }

    /// The hermetic half of the entity-light contract: before login, the
    /// `SharedHandle`'s `OnceLock` is unset, so [`entity_light_at`] must read
    /// `None` rather than panic or fabricate a byte — the same "no world yet"
    /// case `EntityLightSource::sample` (`gpu.rs`) turns into full brightness,
    /// never darkness.
    ///
    /// This cannot exercise the "returns a real value for a loaded position"
    /// half: [`lodestone_client::ClientHandle`] has no public constructor
    /// (its `new` is `pub(crate)` to that crate), so a handle with real,
    /// loaded sections can only come from an actual session. That half is
    /// covered by the `#[ignore]`d live gate below.
    #[test]
    fn entity_light_at_reads_none_before_login() {
        let shared: SharedHandle = Arc::new(OnceLock::new());
        assert_eq!(entity_light_at(&shared, 0, 64, 0, SkyDefault::Full), None);
    }

    /// The `Missing` sky default is honoured, and only for `Missing`.
    ///
    /// This is the unit half of the regression that blacked out mobs in open air
    /// and the first-person arm over an ocean: [`SectionLight::sky_at`] resolves
    /// [`LightData::Missing`] to `0`, so `entity_light_at` reading it directly
    /// reported "no sky" for every section above the top of the lit column.
    ///
    /// It asserts against [`WorldSectionLight`] rather than against a session,
    /// because `ClientHandle` has no public constructor — but that is the point:
    /// the fix routes through the *same adapter the terrain draw uses*, so this
    /// pins the adapter's contract and the live gate below pins the wiring.
    ///
    /// The two `Uniform(0)` cases are the control that matters. If the fix had
    /// been "default absent sky to 15" rather than "ask the policy", the nether
    /// row would read 15 and the overworld-stored-zero row would too — the
    /// too-bright-nether bug. Both must stay `0`.
    #[test]
    fn a_missing_sky_section_takes_the_dimension_default_and_stored_zero_does_not() {
        use lodestone_world::{LightData, SectionLight as WorldSectionLightData};

        let missing = WorldSectionLightData {
            sky: LightData::Missing,
            block: LightData::Missing,
        };
        let stored_dark = WorldSectionLightData {
            sky: LightData::Uniform(0),
            block: LightData::Missing,
        };

        // Absent sky, overworld: vanilla's above-the-top default.
        assert_eq!(
            WorldSectionLight::new(&missing, SkyDefault::Full).sky_light(0, 0, 0),
            15,
            "absent sky in a skylit dimension is full daylight, not 0"
        );
        // Absent sky, nether: absent must stay dark there.
        assert_eq!(
            WorldSectionLight::new(&missing, SkyDefault::None).sky_light(0, 0, 0),
            0,
            "absent sky in a dimension with no skylight must not default up"
        );
        // Stored zero is real data in *either* dimension and is never defaulted up.
        assert_eq!(
            WorldSectionLight::new(&stored_dark, SkyDefault::Full).sky_light(0, 0, 0),
            0,
            "a stored 0 is measured darkness (a cell inside a room), not absence"
        );
        assert_eq!(
            WorldSectionLight::new(&stored_dark, SkyDefault::None).sky_light(0, 0, 0),
            0
        );
    }

    /// The cell's default is [`SkyDefault::Full`], and it round-trips.
    ///
    /// The default is load-bearing rather than arbitrary: it is what
    /// `sky_default_for_dimension(None, None)` answers for "dimension not yet
    /// known", so the frames between connect and the first `registry_data` render
    /// mobs lit instead of black.
    #[test]
    fn the_sky_default_cell_defaults_to_full_and_round_trips() {
        let cell = SkyDefaultCell::default();
        assert_eq!(cell.get(), SkyDefault::Full, "pre-login must not be dark");
        cell.set(SkyDefault::None);
        assert_eq!(cell.get(), SkyDefault::None);
        cell.set(SkyDefault::Full);
        assert_eq!(cell.get(), SkyDefault::Full);
    }

    /// Live gate for the entity-lighting seam: [`entity_light_at`] — the
    /// function `WindowApp::resumed` (`app.rs`) closes over and installs into
    /// [`crate::gpu::RenderState::set_entity_light_source`] — must return a
    /// real packed light byte for a position inside a chunk the oracle has
    /// actually streamed, and `None` for one far outside it. `None` here is
    /// the "unloaded neighbour" case, never darkness, so the assertion is
    /// specifically `is_some()`/`is_none()`, not a light-level comparison.
    ///
    /// Connects directly through `ClientBuilder`, bypassing the `NetClient`
    /// background thread, so the test controls exactly when the handle is
    /// published into the `SharedHandle` — mirroring what `run()` above does
    /// at `shared_handle.set(...)`.
    ///
    /// ```text
    /// cargo test -p lodestone-shell --features live --lib \
    ///     net::tests::live_entity_light_at_distinguishes_loaded_from_unloaded \
    ///     -- --ignored --nocapture
    /// ```
    #[cfg(feature = "live")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires the lodestone-survival server on 127.0.0.1:25565"]
    async fn live_entity_light_at_distinguishes_loaded_from_unloaded() {
        use lodestone_testsupport::poll_until;

        let user = unique_username();
        let protocol = 776; // vanilla 26.2 — the `live` feature's compiled-in family
        let adapter = lodestone_registry::adapter_for_protocol(protocol)
            .expect("the `live` feature compiles a family in for protocol 776");
        let (handle, mut events) = ClientBuilder::new(
            ServerAddress {
                host: "127.0.0.1".into(),
                port: 25565,
            },
            LoginProfile {
                username: user.clone(),
                uuid: uuid::Uuid::new_v4(),
            },
            adapter,
        )
        .connect()
        .await
        .expect("connect to lodestone-survival on 127.0.0.1:25565");
        // Drain the event stream so the driver's bounded channel never blocks.
        let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });

        assert!(
            poll_until(
                Duration::from_secs(30),
                Duration::from_millis(100),
                || async {
                    handle
                        .players()
                        .into_iter()
                        .find(|p| p.name.as_deref() == Some(user.as_str()))
                }
            )
            .await
            .is_some(),
            "player {user} never reached Play on the oracle"
        );

        let dims = poll_until(
            Duration::from_secs(10),
            Duration::from_millis(100),
            || async { handle.world_dimensions() },
        )
        .await
        .expect("world dimensions never arrived");

        let loaded = poll_until(
            Duration::from_secs(15),
            Duration::from_millis(200),
            || async {
                let chunks = handle.loaded_chunks();
                if chunks.is_empty() {
                    None
                } else {
                    Some(chunks)
                }
            },
        )
        .await
        .expect("no chunks streamed in within 15s of login");

        let shared: SharedHandle = Arc::new(OnceLock::new());
        shared
            .set(Arc::new(handle))
            .expect("first (only) publish into a fresh OnceLock");

        // Loaded: the middle of a chunk the oracle actually streamed, at a Y
        // comfortably inside the dimension's build range.
        let chunk = loaded[0];
        let y = dims.min_y + (dims.height as i32 / 2);
        // The oracle is the overworld, so `Full` is the honest policy here.
        let lit = entity_light_at(&shared, chunk.x * 16 + 8, y, chunk.z * 16 + 8, SkyDefault::Full);
        assert!(
            lit.is_some(),
            "expected a real light byte for a loaded chunk {chunk:?}, got None"
        );

        // Unloaded: a chunk address far outside any sane render distance from
        // spawn, so this reads the "outside loaded chunks" branch, not a
        // fluke miss.
        // Still `None`, and the policy must not change that: `SkyDefault` resolves
        // absent *data within a resident section*, never a missing chunk. If this
        // ever returns `Some`, the fix has leaked into the unloaded branch.
        let far = entity_light_at(&shared, 1_000_000, y, 1_000_000, SkyDefault::Full);
        assert_eq!(
            far, None,
            "an unloaded neighbour must read None (full-bright fallback), not a stale byte"
        );

        drain.abort();
    }
}
