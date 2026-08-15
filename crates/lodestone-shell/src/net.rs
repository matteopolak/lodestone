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
use lodestone_model::action::ResourcePackResponseKind;
use lodestone_model::event::SoundCategory;
// `SectionLight` is imported anonymously: it is the trait carrying
// `sky_light`/`block_light` on `WorldSectionLight`, and naming it would collide
// with `lodestone_world::SectionLight`, the *storage* type of the same name that
// `sections_and_light_at` hands back.
use lodestone_render::{SectionLight as _, SkyDefault, WorldSectionLight};

use uuid::Uuid;

use crate::entities::NameTag;
use crate::offline_identity::OfflineIdentity;

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

/// The connecting session's local player UUID, published as soon as the
/// [`LoginProfile`] is built — before the handshake even starts, not just
/// before login completes like [`SharedHandle`]. Same "publish once, read
/// many, no lock contention" shape.
///
/// This exists because nothing between here and `lodestone-client` carries a
/// name→identity answer at all: `lodestone_client::state`'s own docs say the
/// id→name *registry* mapping is "deliberately outside this crate", and that
/// crate's `ClientHandle` has no uuid accessor either — the identity only ever
/// existed as a local variable inside [`run`]. Issue #189's Social
/// Interactions roster needs it to exclude the local player
/// (`crate::menu::social::entries_from_tablist`'s `exclude` parameter), which
/// is the first consumer that made the gap visible.
pub type SharedLocalUuid = Arc<OnceLock<uuid::Uuid>>;

/// The world's weather, published lock-free by the net thread and read once per
/// frame by the render thread.
///
/// # Why this is not a [`NetUpdate`]
///
/// `GAME_EVENT`'s rain and thunder levels arrive **every tick** while the server
/// ramps them (`ServerLevel.java` broadcasts on any change, and the
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

/// The `minecraft:worldgen/biome` registry's ordered entry names, published
/// once at `Login` by [`forward`]'s `BiomeRegistryNames` arm and read by the
/// mesh worker threads that resolve a chunk section's biome holder id to a
/// name (`crate::mesher`'s `biome_name_at`) — the live counterpart of that
/// module's provisional `FALLBACK_BIOME_NAMES` table.
///
/// # Why `&'static str`, not `String`
///
/// `lodestone_render::biome_tint::NamedBiomeTint` requires
/// `Fn(BlockPos) -> Option<&'static str>` — a bound this crate does not own
/// and Job 2's scope does not touch. Names arrive as owned `String`s off the
/// wire, so [`Self::apply`] **leaks** each one once (`Box::leak`) to get a
/// `&'static str` a closure of that shape can return. This is deliberate and
/// bounded, not a mistake: the biome registry is a few dozen to a couple
/// hundred short strings, folded once per `Login` (never per-tick, matching
/// [`BiomeClimateCell`]'s own "whole table replaces at once" shape), so a
/// session that reconnects many times leaks at most a few KB total — a cost
/// worth paying once rather than plumbing a lifetime through a trait bound
/// three crates away.
///
/// `Mutex<Vec<&'static str>>`, not lock-free atomics, for the same reason as
/// [`BiomeClimateCell`]: this table's *length* changes with the registry, so
/// per-field atomics could not express it anyway.
#[derive(Debug, Default)]
pub struct BiomeNameCell(Mutex<Vec<&'static str>>);

impl BiomeNameCell {
    /// Replace the whole table, leak-interning each name. Called once, at
    /// `Login`, by [`forward`]'s `BiomeRegistryNames` arm.
    pub(crate) fn apply(&self, names: &[String]) {
        let leaked: Vec<&'static str> = names
            .iter()
            .map(|name| -> &'static str { Box::leak(name.clone().into_boxed_str()) })
            .collect();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = leaked;
    }

    /// A cheap snapshot of the current table — `&'static str` is `Copy`, so
    /// this is one allocation for the `Vec`, not one per name. Empty before
    /// any `registry_data` arrives, or on a version/server that sends none;
    /// callers must fall back to a local table in that case (see
    /// `crate::mesher::biome_name_at`), never treat empty as "id 0".
    #[must_use]
    pub fn snapshot(&self) -> Vec<&'static str> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// A [`BiomeNameCell`] shared between the net thread and the mesh worker
/// threads.
pub type SharedBiomeNames = Arc<BiomeNameCell>;

/// The server's Brigadier command tree, plus the newest reply to a
/// `command_suggestion` request (issues #470/#471).
///
/// # Why a cell rather than a [`NetUpdate`]
///
/// Same shape as [`BiomeNameCell`], which is what
/// [`ClientEvent::CommandTreeUpdated`]'s own doc points at: the whole tree
/// replaces at once and there is nothing to queue. It is also read from the
/// *menu* layer (the chat box and the command-block edit screen), which polls
/// per frame and wants the latest value rather than a stream — a channel would
/// have every reader folding it back into one scalar anyway.
///
/// `Arc<CommandTree>` rather than a clone per read: a real 26.2 server's tree
/// is **2,017 nodes / ~30 kB** (`docs/commands.md`), so cloning it on every
/// keystroke to run a completion would be the wrong default to leave lying
/// around.
///
/// # What consumes this today, honestly
///
/// **The fold, and not yet a screen.** This closes the half of issue #471 that
/// is a live defect — `lodestone_model::event::route` sends both variants to
/// `SHELL`, so with no arm in [`forward`] they reached the terminal `_ =>` and
/// tripped its `debug_assert!` on any debug-build join to a real server. The
/// remaining two steps in #471 — pointing `menu/render/screens.rs`'s
/// `command_block_frame` at [`Self::tree`] instead of the `None` every caller
/// passes, and making the chat box's Tab key call `chat::complete` — live in
/// `chat.rs` and the menu files, which this change does not own.
///
/// **So this is deliberately a half-wire, and saying so is the point**:
/// storing the value where the consumer can reach it is what makes the next
/// step an arm rather than a re-decode, and dropping it in the arm instead
/// would be the island pattern the `debug_assert!` above exists to catch.
#[derive(Debug, Default)]
pub struct CommandTreeCell {
    tree: Mutex<Option<Arc<lodestone_model::command_tree::CommandTree>>>,
    suggestions: Mutex<Option<lodestone_model::command_tree::CommandSuggestionsResponse>>,
}

impl CommandTreeCell {
    /// Replace the tree. Called by [`forward`]'s `CommandTreeUpdated` arm.
    ///
    /// A server may send `minecraft:commands` more than once per session (an
    /// op level change re-sends it), so this replaces rather than sets once —
    /// which is also why it is not a `OnceLock` like [`SharedHandle`].
    pub(crate) fn apply(&self, tree: lodestone_model::command_tree::CommandTree) {
        *self.tree.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(tree));
    }

    /// Record the newest suggestion reply. Called by [`forward`]'s
    /// `CommandSuggestionsReceived` arm.
    ///
    /// Stored whole, **including its transaction id**: the id is the only
    /// thing that lets the consumer discard a reply to a request the input has
    /// since outgrown (vanilla's own
    /// `ClientSuggestionProvider::completeCustomSuggestions` check), so
    /// flattening this to just the strings here would destroy the one field
    /// that makes a stale reply detectable.
    pub(crate) fn apply_suggestions(
        &self,
        response: lodestone_model::command_tree::CommandSuggestionsResponse,
    ) {
        *self.suggestions.lock().unwrap_or_else(PoisonError::into_inner) = Some(response);
    }

    /// The current tree, or `None` before the server sends one (a server that
    /// sends none, or any point before login completes). Callers must treat
    /// `None` as "offer no completions", never as an empty tree.
    #[must_use]
    pub fn tree(&self) -> Option<Arc<lodestone_model::command_tree::CommandTree>> {
        self.tree
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The newest suggestion reply, if any has arrived.
    #[must_use]
    pub fn suggestions(
        &self,
    ) -> Option<lodestone_model::command_tree::CommandSuggestionsResponse> {
        self.suggestions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A [`CommandTreeCell`] shared between the net thread and the menu layer.
pub type SharedCommandTree = Arc<CommandTreeCell>;

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
    /// The session task reached a named step of establishing the session —
    /// issue #449's phase names for the loading screen.
    ///
    /// **Sent only from real boundaries in [`run_session`]**, never on a timer:
    /// see [`crate::menu::loading::ConnectPhase`] for why there are three
    /// phases and not vanilla's six, and for why a phase with no emit site here
    /// would be an island rather than a feature.
    ConnectPhase(crate::menu::loading::ConnectPhase),
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
        /// The sender's profile UUID — issue #419's filter key, mirrored from
        /// [`ClientEvent::Chat`] verbatim. Only v770's signed `player_chat`
        /// carries one; system, disguised, action-bar and every legacy-family
        /// message are `None` (`None` must be shown, never hidden).
        sender: Option<Uuid>,
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
    /// A chunk column left the server's tracking view (`forget_level_chunk`):
    /// the client has **already** dropped it from the one [`lodestone_ecs::ChunkWorld`]
    /// store, so every mesh belonging to it is now geometry for blocks the
    /// client no longer has.
    ///
    /// Issue #479: this variant exists because that eviction previously reached
    /// only collision. `LiveCollision` re-reads the store every tick, so it
    /// tracked the unload for free, while the renderer had no signal at all —
    /// `ClientEvent::ChunkUnloaded` had four producers and no shell consumer and
    /// died in [`forward`]'s terminal arm, the island class `CLAUDE.md` §1 names.
    /// The result was a session whose GPU section map, uploaded-section set and
    /// fixed-capacity origin arena grew monotonically while the store shrank:
    /// walk far enough in one direction and the arena is exhausted, at which
    /// point `upload_section` drops each new section's geometry and you collide
    /// with terrain you cannot see.
    ///
    /// Carries no block data, for the same §12.24 reason [`NetUpdate::Chunk`]
    /// does not: the store is the payload, and this is a signal about it.
    ChunkUnloaded {
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
    /// The server authorised the local player to edit a sign (vanilla's
    /// `ClientboundOpenSignEditorPacket`, decoded as
    /// `ClientEvent::SignEditorOpened`).
    ///
    /// Same shape as [`NetUpdate::BlockEvent`] immediately above: the decode
    /// (`v770`'s `OPEN_SIGN_EDITOR`) and the screen
    /// (`crate::menu::sign_edit::SignEditState`) were both real and both
    /// tested, and this event had **zero consumers** between them — it fell
    /// through [`forward`]'s terminal `_ =>` arm. `Sim::poll_net` is the one
    /// consumer that can read the sign's already-synced block-entity text to
    /// seed the screen with, which is why this crosses raw rather than
    /// pre-resolved.
    SignEditorOpened {
        /// The sign's block position.
        pos: lodestone_model::BlockPos,
        /// Whether the front (vs. back) face is being edited.
        is_front_text: bool,
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
    Respawned {
        /// The destination dimension, carried verbatim off
        /// [`ClientEvent::Respawned`](lodestone_model::event::ClientEvent::Respawned)'s
        /// own field.
        ///
        /// # Why the payload travels rather than being read back at the consumer
        ///
        /// `Sim::apply_respawn` has to decide *whether the dimension changed*, and
        /// that is a question about this event, not about the present moment. The
        /// read model (`lodestone_ecs::session::ServerDimension`, the single owner
        /// of the identity) is folded on the **net thread** the instant this event
        /// is applied, while the shell drains this channel a frame or more later —
        /// so a consumer that read `Sim::dimension()` here would be comparing the
        /// new dimension against itself and would never see a change at all. The
        /// event's own field is ordered with the event; a shared-state read is not.
        ///
        /// `Option` because [`forward`] is the shell's router for **every**
        /// protocol family, and this keeps a family whose adapter cannot report a
        /// dimension from being forced to invent one — `None` is read as "no
        /// change I can justify", not as the overworld.
        dimension: Option<lodestone_client::DimensionId>,
    },
    /// The server signalled `WIN_GAME` (issue #192): the local player exited
    /// the End through the exit portal after the dragon fight. Carries no
    /// data — see [`lodestone_model::event::ClientEvent::WinGame`]'s own doc
    /// for why. `Sim::poll_net` latches this into a `won` flag,
    /// `WindowApp::drive_ui_from_session` notices it and shows the credits
    /// screen (`UiState::show_credits`) — the same shape as
    /// [`NetUpdate::Death`]/`Sim::is_dead`/`UiState::die`.
    WinGame,
    /// The world was published to LAN on `port` (issue #535). Reported rather
    /// than assumed because the caller may have asked for port `0`, and because a
    /// player who cannot see the port cannot tell anyone how to join.
    ///
    /// `Sim::poll_net` turns this into the chat line vanilla's
    /// `menu.multiplayerOptions.publish.started.lan` is.
    LanOpened {
        /// The TCP port the listener actually bound.
        port: u16,
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
    /// An entity played its hurt animation (`hurt_animation`), carrying the yaw
    /// the damage came from.
    ///
    /// Carries `entity_id` **unfiltered**, like [`NetUpdate::EffectApplied`] and
    /// for the same reason: the packet applies to any entity, and the sim decides
    /// whether it is the locally-tracked player before starting the camera tilt.
    ///
    /// # Why the shell needs this at all when `ingest` already handles it
    ///
    /// `lodestone_ecs::ingest` folds the same event into a per-entity `HurtTime`
    /// component, which drives the **red overlay** on the mob that was hit. That is
    /// a different consumer of a different thing: the camera tilt is a
    /// *local-player scalar* living in `Sim`'s `ViewBob`, and — decisively — ingest
    /// discards the `yaw`, which is the entire direction half of `bobHurt`.
    HurtAnimation {
        /// Entity that was hurt.
        entity_id: i32,
        /// `hurtDir`: the yaw the damage came from, in degrees, computed by the
        /// server as `atan2(damage) - playerYaw`, so a hit from straight ahead
        /// is `0`.
        yaw: f32,
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
    /// A transport or setup error. **Always ends the session** —
    /// `Sim::poll_net`'s arm for this variant moves `SessionPhase` to
    /// `Ended(SessionEnd::failed(..))`, the same terminal state a real
    /// disconnect reaches, just with `SessionEndKind::Failed` instead of
    /// `::Disconnected`. This is why every producer of this variant is a
    /// point the net thread's own loop cannot recover from (a missing
    /// adapter, a connect failure, a codec error) and then either `return`s
    /// or `break`s. **Never send this for something the session survives** —
    /// see [`Self::LanPublishError`] for the non-fatal counterpart, added
    /// after this variant was (incorrectly) used for a mid-session "already
    /// published" failure and turned a harmless double-press of the pause
    /// menu's Open to LAN button into a full disconnect.
    Error(String),
    /// A publish-to-LAN request failed server-side — issue #562's own
    /// button pressed twice, or before an integrated server exists to
    /// publish. **Never ends the session**, unlike [`Self::Error`]: the net
    /// thread's `publish_rx` loop stays in its own `loop {}` and keeps
    /// draining `action_rx`/`events` exactly as before, so the connection
    /// this update rides on is exactly as alive after sending it as before.
    /// `Sim::poll_net` turns this into a local chat line, the same
    /// "reported, not assumed" shape [`Self::LanOpened`] already uses for the
    /// success case, rather than [`Self::Error`]'s session-ending one.
    LanPublishError(String),
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
    /// "Open to LAN" requests (issue #562): a port to add a TCP listener on,
    /// drained by the net thread's own loop rather than handed to the client
    /// handle like [`Self::action_tx`] — this is not a wire packet, it is a
    /// local call into the **running** `IntegratedServer` this thread already
    /// holds (`IntegratedServer::publish`). Native only: the capability itself
    /// is (no TCP on wasm32), and every construction path — including the two
    /// `#[cfg(test)]` loopbacks — still creates one so the struct has a single
    /// field list; a request into a loopback's receiver is simply never drained.
    #[cfg(not(target_arch = "wasm32"))]
    publish_tx: Sender<u16>,
    stop: Arc<AtomicBool>,
    /// The driver's OS thread, joined on `Drop`.
    ///
    /// **Native-only.** A browser drives the same future with `spawn_local`, which
    /// yields no join handle at all, so there is nothing to store and nothing to join.
    /// Teardown there is the `stop` flag above and nothing else — which is sufficient
    /// for the same reason it is sufficient natively: the driver's loop checks it every
    /// iteration. What is *lost* is the `Drop`-time guarantee that the driver has
    /// actually finished before `NetClient` goes away; in a browser it observes `stop`
    /// on its next poll and unwinds a moment later.
    #[cfg(not(target_arch = "wasm32"))]
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
    /// The biome registry's ordered entry names, folded by [`forward`]'s
    /// `BiomeRegistryNames` arm at `Login`. See [`BiomeNameCell`] for why this
    /// is a shared cell rather than a [`NetUpdate`], and for the `&'static
    /// str` leak-intern this is the one caller of.
    biome_names: SharedBiomeNames,
    /// The server's command tree and newest suggestion reply, folded by
    /// [`forward`]'s two command arms. See [`CommandTreeCell`] for why this is
    /// a shared cell rather than a [`NetUpdate`], and for exactly how much of
    /// issue #471 it closes.
    command_tree: SharedCommandTree,
    /// The current dimension's absent-sky-light policy. Unlike [`Self::weather`]
    /// the **net thread never writes this** — `Sim::refresh_mesh_policy` is the
    /// sole producer and the render thread's light samplers are the consumers.
    /// It lives here only because `NetClient` is where a per-session shared cell
    /// is already handed out at connect time. See [`SkyDefaultCell`].
    sky_default: SharedSkyDefault,
    /// The local player's UUID, published by the net thread as soon as the
    /// connecting profile is known. See [`SharedLocalUuid`].
    local_uuid: SharedLocalUuid,
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
/// An authenticated Microsoft/Minecraft session, on targets that can obtain one.
///
/// **Uninhabited on wasm32 — `Infallible`, not a stub type.** Two independent
/// reasons, and either alone would be enough: `lodestone_client::Session` is
/// `cfg(not(wasm32))` because deriving one needs the native-only `lodestone-auth`
/// chain, and `Origin::Remote`'s TCP dial does not exist in a browser either. Making
/// the *type* uninhabited rather than gating the `auth` field means `Option<_>` is
/// statically always `None`, so all five sites that construct, debug-print or
/// destructure it compile unchanged on both targets, and the two that need a real
/// `Session` are the only ones that carry a `cfg`.
#[cfg(not(target_arch = "wasm32"))]
type OnlineSession = lodestone_client::Session;
#[cfg(target_arch = "wasm32")]
type OnlineSession = std::convert::Infallible;

/// What identity a remote join should present — a *request*, resolved on the net
/// thread rather than at the call site.
///
/// This replaced a plain `Option<OnlineSession>`, and the reason is the bug it
/// fixes. Resolving an account needs an `await` (a refresh-token round trip to
/// Microsoft), and every caller of [`NetClient::connect`] is synchronous UI code
/// on the render thread; so with an `Option` the only thing an ordinary join
/// could pass was `None`. That is precisely what happened: `connect_online` —
/// the one constructor that could pass `Some` — had **zero callers**, so the
/// account switcher could hold a signed-in, working Microsoft account that no
/// join ever used, and an online-mode server produced "no Microsoft session was
/// configured" while the switcher displayed the player's real username.
///
/// Naming the *intent* here moves the resolution to the one place that already
/// has a runtime ([`run_async`], inside the net thread's `block_on`), so the
/// synchronous constructors stay synchronous and still get a real session.
enum RemoteAuth {
    /// Never authenticate; present the caller's [`OfflineIdentity`] verbatim.
    ///
    /// This is [`NetClient::connect_as`] (live gates, which need a fresh name
    /// per run and must not be diverted onto whatever account the developer
    /// happens to have selected) and every browser join.
    Offline,
    /// Use whichever account [`lodestone_auth::AccountsMetadata::selected`]
    /// names, resolving it on the net thread; fall back to the offline identity
    /// when nothing is selected.
    ///
    /// Native-only because resolution needs the `lodestone-auth` chain, which
    /// is itself `cfg(not(wasm32))`.
    #[cfg(not(target_arch = "wasm32"))]
    SelectedAccount,
    /// A session the caller already resolved — [`NetClient::connect_online`],
    /// for a caller that did its own sign-in and has a live one in hand.
    Session(OnlineSession),
}

impl RemoteAuth {
    /// What a **production** multiplayer join requests.
    ///
    /// A named function rather than a literal at the one call site, because the
    /// literal is exactly what was wrong before: `connect` said `auth: None` and
    /// nothing in the tree disagreed with it. Naming the decision gives a gate a
    /// subject, so a future edit that quietly puts the join back on the offline
    /// path fails a test instead of silently un-signing everyone in.
    fn for_production_join() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        return RemoteAuth::SelectedAccount;
        // Browser: `lodestone-auth` is native-only, so there is nothing to
        // resolve and a browser join presents the offline identity.
        #[cfg(target_arch = "wasm32")]
        return RemoteAuth::Offline;
    }
}

enum Origin {
    /// Dial a real server over TCP. `auth` says which identity to present.
    Remote {
        /// Host to dial.
        host: String,
        /// Port to dial.
        port: u16,
        /// Which identity to join under, resolved in [`run_async`].
        auth: RemoteAuth,
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
        /// World seed for the bundled overworld generator — a **creation**
        /// parameter only. When `world_dir` names a world that already exists,
        /// that world's stored seed wins and this is ignored; see
        /// [`lodestone_server::region_source::resolve_world_seed`].
        seed: i64,
        /// Chunk radius the server streams around the player.
        view_radius: i32,
        /// Where to save this world, or `None` for a throwaway in-memory one.
        ///
        /// `Some` is what makes singleplayer persist (issue #468) — it selects
        /// `IntegratedServer::open_persistent_with_mobs` over the in-memory
        /// constructor. `None` is not dead: it is what `wasm32` gets (a browser
        /// world has no filesystem) and what a test wanting a world that leaves
        /// nothing behind asks for.
        #[cfg(not(target_arch = "wasm32"))]
        world_dir: Option<std::path::PathBuf>,
        /// Open this world to LAN on this TCP port instead of serving it over the
        /// in-memory duplex (issue #535's scope 1). `0` asks the OS for a port.
        ///
        /// `Some` selects `IntegratedServer::open_to_lan`, and the local player
        /// then joins over loopback like any other LAN client — one transport for
        /// everybody, which is what stops the host being a second, privileged
        /// kind of connection. See [`run`]'s own LAN branch for the two
        /// persistence gaps that come with it.
        #[cfg(not(target_arch = "wasm32"))]
        lan_port: Option<u16>,
        /// Run the real RSA/AES online-mode handshake on the LAN listener
        /// `lan_port` opens (issue #273's shell-side control). Ignored when
        /// `lan_port` is `None` — a purely in-memory singleplayer connection
        /// never reads this field at all, so it cannot authenticate no matter
        /// what this is set to. See [`open_lan_world`]'s `lan_online_mode`.
        #[cfg(not(target_arch = "wasm32"))]
        online_mode: bool,
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
                // The *request*, not a resolved session: a `SelectedAccount`
                // join that finds nothing selected still ends up offline, so
                // this is what was asked for, not what happened. Still no
                // token in the output — `RemoteAuth::Session`'s payload is
                // never printed.
                .field(
                    "auth",
                    &match auth {
                        RemoteAuth::Offline => "offline",
                        #[cfg(not(target_arch = "wasm32"))]
                        RemoteAuth::SelectedAccount => "selected-account",
                        RemoteAuth::Session(_) => "session",
                    },
                )
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

/// How long a connected client waits for the server to send **any** packet
/// before declaring the connection dead (issue #280).
///
/// Vanilla arms the same bound at the socket with Netty's
/// `ReadTimeoutHandler(30)` — `Connection.java` — a 30-second stall that
/// disconnects with `disconnect.timeout`. This mirrors it through the client
/// library's `read_packet_timeout`, which is a **per-packet** window reset by
/// every inbound packet. That is why 30 s is safely above the server's keep-alive
/// cadence (15 s in `ServerCommonPacketListenerImpl.java`; our own
/// `lodestone-server` sends on the same `KEEP_ALIVE_INTERVAL` of 15000 ms): a
/// healthy session re-arms the window twice over, and only a server that has
/// stopped sending entirely trips it. When it fires the driver task ends, the
/// event sender drops, and the shell's existing `Ok(None)` → [`NetUpdate::Disconnected`]
/// arm reports the loss — no new disconnect wiring, which is why this is one
/// line on the builder.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Vanilla's well-known Minecraft port — the one a joining player will try
/// first if they only know the host's address.
///
/// **Not what the pause menu's Open to LAN asks for since issue #559.**
/// `PauseButton::OpenToLan` now calls `NetClient::publish_to_lan(0)`, matching
/// vanilla's own `MultiplayerOptionsScreen`, which defaults to
/// `HttpUtil.getAvailablePort()` — an OS-assigned port, reported back through
/// `NetUpdate::LanOpened` once bound — rather than this fixed one. This
/// constant is kept for an explicit-port control (vanilla's `/publish <port>`)
/// to default its text field to, once one exists; it names *a* well-known
/// port, not the one this shell binds automatically.
#[cfg(not(target_arch = "wasm32"))]
pub const LAN_DEFAULT_PORT: u16 = 25565;

/// How often a persistent singleplayer world writes its dirty chunks (issue
/// #468).
///
/// Vanilla autosaves every 6000 ticks (5 minutes). This is far shorter because
/// the cost model is different, not because 5 minutes is wrong: a save here
/// writes **only the dirty set**, so a player standing still writes nothing at
/// all, and three mutated chunks write exactly three columns rather than the
/// ~512 a residency-proportional save would. The work happens inside
/// `spawn_blocking`, off the thread `run_tick_loop` shares. So the interval
/// trades almost nothing for a much smaller window of unsaved building if the
/// process is killed rather than quit cleanly.
///
/// A clean quit does not depend on this at all — `shutdown()` flushes at the
/// end of the session.
#[cfg(not(target_arch = "wasm32"))]
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

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
    ///
    /// # Identity
    ///
    /// **The signed-in Microsoft account when there is one, and the persisted
    /// "Play offline" identity otherwise.** This is the production join, so it
    /// is the one path that consults
    /// [`lodestone_auth::AccountsMetadata::selected`]; the resolution happens on
    /// the net thread ([`RemoteAuth::SelectedAccount`]) because it needs an
    /// `await` and every caller here is synchronous UI code.
    ///
    /// A selected account whose saved session cannot be renewed does **not**
    /// abort the join: an offline-mode server never asks for authentication, so
    /// the connection proceeds under the offline identity carrying the reason,
    /// and only an online-mode server spends it (as
    /// `lodestone_client::ClientError::OnlineModeSessionUnavailable`).
    ///
    /// With nothing selected this is exactly what it always was:
    /// [`OfflineIdentity::load`] — the same name and the same derived UUID every
    /// launch, which is the whole point (see [`crate::offline_identity`]), and
    /// **no network call is made looking for an account**.
    ///
    /// A **live gate must not use this**: a shared offline name is a shared
    /// player file, and a dead player is held on the death screen, which sends
    /// no chunks — and a gate must not silently join as the developer's real
    /// premium account either. Use [`Self::connect_as`] with
    /// `lodestone-testsupport`'s `unique_username()`.
    #[must_use]
    pub fn connect(
        host: String,
        port: u16,
        protocol: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    ) -> Self {
        Self::connect_impl(
            Self::production_origin(host, port),
            protocol,
            session,
            OfflineIdentity::load(),
        )
    }

    /// The [`Origin`] a production multiplayer join dials — extracted from
    /// [`Self::connect`] so a test can assert **which identity a real join asks
    /// for** without spawning a net thread and dialing a server.
    ///
    /// It exists because the thing that broke here was not a wrong value but a
    /// missing one, and a missing value has no natural subject to point a test
    /// at: `connect` hardcoded `auth: None`, which is indistinguishable from
    /// correct until you compare it against `connect_online`, three constructors
    /// away. Naming the choice gives the gate something to read that is *the same
    /// expression production evaluates* — a test that rebuilt the `Origin` itself
    /// would only assert its own opinion.
    /// # Why this forks on `#[cfg(test)]`
    ///
    /// Resolving a selected account opens the OS keychain, POSTs the refresh
    /// token to Microsoft, and — because the refresh token *rotates on every
    /// use* — writes a new one back. A `cargo test` that did that would reach
    /// the network from a unit test and could invalidate the token the owner's
    /// real client is holding: a side effect with a user-visible symptom that no
    /// health check here can see, because the suite still passes. This crate has
    /// had that exact defect once already (a unit test opening `login.live.com`
    /// in the owner's browser on every run), and the lesson recorded from it was
    /// to fork on `#[cfg(test)]` rather than early-return on `cfg!(test)`, so the
    /// interception is *assertable* instead of a silent skip —
    /// [`unit_tests_never_resolve_a_real_microsoft_account`] is that assertion,
    /// and [`a_production_join_requests_the_selected_microsoft_account`] pins the
    /// production decision separately so the fork cannot hide a regression in it.
    ///
    /// [`unit_tests_never_resolve_a_real_microsoft_account`]: tests::unit_tests_never_resolve_a_real_microsoft_account
    /// [`a_production_join_requests_the_selected_microsoft_account`]: tests::a_production_join_requests_the_selected_microsoft_account
    fn production_origin(host: String, port: u16) -> Origin {
        Origin::Remote {
            host,
            port,
            #[cfg(not(test))]
            auth: RemoteAuth::for_production_join(),
            #[cfg(test)]
            auth: RemoteAuth::Offline,
        }
    }

    /// The [`Origin`] a join that must **never** consult the account switcher
    /// dials — [`Self::connect_as`]'s, i.e. every live gate's. Paired with
    /// [`Self::production_origin`] so the two sit next to each other and a gate
    /// can assert they differ.
    fn offline_origin(host: String, port: u16) -> Origin {
        Origin::Remote {
            host,
            port,
            auth: RemoteAuth::Offline,
        }
    }

    /// As [`Self::connect`], but joining under `username` instead of the
    /// persisted offline identity.
    ///
    /// **This exists for live gates, and it is the reason
    /// `lodestone-testsupport` no longer needs to be reachable from production.**
    /// Every gate that dials a shared oracle server needs a fresh name per run —
    /// two sessions sharing an offline name evict each other, and a dead player
    /// under a reused name blacks out chunk data silently — so it passes
    /// `lodestone-testsupport`'s `unique_username()` here. Production goes
    /// through [`Self::connect`], which is stable by design. The underscored
    /// Rust path is deliberately not spelled in this doc comment:
    /// `tests/no_production_source_names_testsupport.rs` scans for it, and prose
    /// that spells it is a false positive.
    ///
    /// `username` is used **verbatim** — not validated, and not persisted. A
    /// name a server will not accept produces its disconnect reason, which is
    /// what a gate driving one wants to see;
    /// [`crate::offline_identity::validate_username`] is the check that belongs
    /// in front of *storing* a name.
    ///
    /// **Stays offline unconditionally**, and that is load-bearing now that
    /// [`Self::connect`] consults the account switcher: a gate asks for an exact
    /// username, so resolving a signed-in account here would silently join under
    /// a *different* name than the one requested — on a developer's machine
    /// only, where an account happens to be selected. Every live gate would then
    /// share one premium player file, which is the eviction/blackout hazard this
    /// constructor exists to avoid, wearing a new hat.
    #[must_use]
    pub fn connect_as(
        host: String,
        port: u16,
        protocol: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
        username: String,
    ) -> Self {
        Self::connect_impl(
            Self::offline_origin(host, port),
            protocol,
            session,
            OfflineIdentity::from_username_unchecked(username),
        )
    }

    /// As [`Self::connect`], but for an **online-mode** server: `auth` is an
    /// authenticated Microsoft/Minecraft session (issue #65 — see
    /// `lodestone_auth::login` for how to obtain one from a cached refresh
    /// token or a completed interactive device-code sign-in) that the net
    /// thread hands to [`lodestone_client::ClientBuilder::online_session`],
    /// and the real profile identity (`auth.profile.name`/`.id`) replaces the
    /// [`crate::offline_identity`] name path for the login-start packet.
    ///
    /// **Prefer [`Self::connect`]**, which now resolves the selected account by
    /// itself. This constructor is for a caller that has *already* driven a
    /// sign-in and holds a live session it does not want re-resolved — a
    /// "sign in and connect straight away" action, or a test supplying a session
    /// without a keychain or a `profiles.json` anywhere.
    ///
    /// # This used to be an island, and the doc comment said so
    ///
    /// It carried, correctly and for a long time, *"still zero callers in the
    /// shell … every join this shell makes is an offline one … the account
    /// switcher can hold a signed-in Microsoft account that no join ever uses"*.
    /// That was the whole bug, and the shape is worth keeping: the constructor
    /// existed, the session type existed, `lodestone_auth::try_cached_session`
    /// existed, `ClientBuilder::online_session` existed, the driver's
    /// `begin_encryption` consumed it — every piece was built and tested, and
    /// **nothing called the first one**, so a player with a working premium
    /// account was told no session was configured.
    ///
    /// The fix was not to find callers for this function. It was to notice
    /// *why* it had none: it demands an already-resolved `Session`, resolving
    /// one needs an `await`, and every join call site is synchronous UI code. So
    /// [`Self::connect`] now passes [`RemoteAuth::SelectedAccount`] and the
    /// resolution happens where a runtime already exists. **When a constructor
    /// has no callers, check whether its signature is the reason** before
    /// wiring a caller to it.
    ///
    /// Native-only: it takes a real [`lodestone_client::Session`], which only the
    /// native `lodestone-auth` chain can produce. A browser join is offline-identity
    /// only, and goes through the relay rather than this path.
    #[cfg(not(target_arch = "wasm32"))]
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
                auth: RemoteAuth::Session(auth),
            },
            protocol,
            session,
            // Unused on this path (the session's own profile wins), but the
            // parameter is not optional: an online session that failed to produce
            // a profile has no business silently falling back to a *different*
            // identity, so the value here is the same one `connect` would use.
            OfflineIdentity::load(),
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
    /// `world_dir` is where the world is saved (issue #468). `Some` opens it
    /// persistently, creating it on first use and writing it back on autosave
    /// and on session end; `None` is the old throwaway in-memory world.
    /// [`crate::saves::default_world_dir`] is what the menu passes.
    ///
    /// `seed` is only consulted when `world_dir` names a world that does not
    /// exist yet — an existing world's own stored seed always wins, or
    /// reopening it would regenerate every unexplored chunk from different
    /// terrain.
    ///
    /// # Identity
    ///
    /// The persisted "Play offline" identity, same as [`Self::connect`], and
    /// singleplayer is where its instability was visible: the integrated server
    /// **echoes the UUID the client presents** (`login_uuid = Some(uuid)`) rather
    /// than deriving one from the name, so the old `Uuid::new_v4()` gave the
    /// player a different account on every launch even before the name did. See
    /// [`crate::offline_identity`].
    #[must_use]
    pub fn open_singleplayer(
        server_protocol: Box<dyn lodestone_server::ServerProtocol>,
        protocol: i32,
        seed: i64,
        view_radius: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
        #[cfg(not(target_arch = "wasm32"))] world_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self::connect_impl(
            Origin::Integrated {
                protocol: server_protocol,
                seed,
                view_radius,
                #[cfg(not(target_arch = "wasm32"))]
                world_dir,
                #[cfg(not(target_arch = "wasm32"))]
                lan_port: None,
                #[cfg(not(target_arch = "wasm32"))]
                online_mode: false,
            },
            protocol,
            session,
            OfflineIdentity::load(),
        )
    }

    /// [`Self::open_singleplayer`], but the world is hosted on a **TCP port** so
    /// other machines can join it — the pause menu's Open to LAN (issue #535).
    ///
    /// Identical in every other respect: same registry-resolved
    /// `ServerProtocol`, same seed and world directory, same offline identity,
    /// same net thread. The local player joins over `127.0.0.1:<port>` rather
    /// than the in-memory duplex, so there is exactly one kind of connection on
    /// this server and the host is not a special case.
    ///
    /// `port` of `0` asks the OS for a free one.
    ///
    /// **Not the pause menu's Open to LAN entry point since issue #562** — a
    /// world already running (the common case, since the button lives on the
    /// pause menu of a session already in progress) publishes in place through
    /// [`Self::publish_to_lan`] instead of coming back through here, which
    /// rebuilds the world from scratch. This constructor is still the right
    /// one for *starting* a session already in LAN mode.
    ///
    /// `online_mode` runs the real RSA/AES handshake and session-server
    /// ownership check on every connection this listener accepts (issue
    /// #273's shell-side control) — `false` matches every other constructor
    /// here and keeps the listener exactly as offline as it has always been.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn open_to_lan(
        server_protocol: Box<dyn lodestone_server::ServerProtocol>,
        protocol: i32,
        seed: i64,
        view_radius: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
        world_dir: Option<std::path::PathBuf>,
        port: u16,
        online_mode: bool,
    ) -> Self {
        Self::connect_impl(
            Origin::Integrated {
                protocol: server_protocol,
                seed,
                view_radius,
                world_dir,
                lan_port: Some(port),
                online_mode,
            },
            protocol,
            session,
            OfflineIdentity::load(),
        )
    }

    /// Shared implementation behind [`Self::connect`]/[`Self::connect_as`]/
    /// [`Self::connect_online`]/[`Self::open_singleplayer`]: spawns the
    /// background net thread and returns immediately.
    ///
    /// `offline` is the identity the login-start packet carries whenever the
    /// origin has no authenticated session — which today is every join. It is a
    /// **parameter rather than a `load()` inside `run`** so the four entry points
    /// above each state where their identity comes from, and so `connect_as` can
    /// supply one without a file at all.
    fn connect_impl(
        origin: Origin,
        protocol: i32,
        session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
        offline: OfflineIdentity,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        #[cfg(not(target_arch = "wasm32"))]
        let (publish_tx, publish_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle: SharedHandle = Arc::new(OnceLock::new());
        let handle_thread = Arc::clone(&handle);
        let weather: SharedWeather = Arc::new(WeatherCell::default());
        let weather_thread = Arc::clone(&weather);
        let biome_climates: SharedBiomeClimates = Arc::new(BiomeClimateCell::default());
        let biome_climates_thread = Arc::clone(&biome_climates);
        let biome_names: SharedBiomeNames = Arc::new(BiomeNameCell::default());
        let biome_names_thread = Arc::clone(&biome_names);
        let command_tree: SharedCommandTree = Arc::new(CommandTreeCell::default());
        let command_tree_thread = Arc::clone(&command_tree);
        let local_uuid: SharedLocalUuid = Arc::new(OnceLock::new());
        let local_uuid_thread = Arc::clone(&local_uuid);

        // Native: the driver gets its own OS thread, so a slow tick never touches the
        // frame loop.
        #[cfg(not(target_arch = "wasm32"))]
        let thread = std::thread::Builder::new()
            .name("lodestone-net".into())
            .spawn(move || {
                run(
                    origin,
                    protocol,
                    tx,
                    action_rx,
                    publish_rx,
                    stop_thread,
                    handle_thread,
                    weather_thread,
                    biome_climates_thread,
                    biome_names_thread,
                    command_tree_thread,
                    local_uuid_thread,
                    session,
                    offline,
                )
            })
            .expect("spawn net thread");

        // Browser: `spawn_local`, onto the page's own event loop.
        //
        // **The `.expect` above is what killed the tab**, and it is worth recording
        // exactly why, because the failure was one step removed from the usual one.
        // `std::thread::Builder::spawn` does *not* trap on wasm32 — measured, executed
        // in a wasm VM, it returns `Err(ErrorKind::Unsupported)`, which is why this
        // site did not appear in the trapping-call census at all. The `.expect()` then
        // turned that graceful error into a panic, and with `panic = "abort"` that is
        // the tab. A degrading call reached through an `.expect()` is as fatal as a
        // trapping one; the census has to look at the *handling*, not only the call.
        //
        // No `JoinHandle` exists here, so `Drop` cannot join — the `stop` flag is the
        // whole teardown, and the driver's own loop already checks it. See the `thread`
        // field.
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(run_async(
            origin,
            protocol,
            tx,
            action_rx,
            stop_thread,
            handle_thread,
            weather_thread,
            biome_climates_thread,
            biome_names_thread,
            command_tree_thread,
            local_uuid_thread,
            session,
            offline,
        ));

        Self {
            rx,
            action_tx,
            #[cfg(not(target_arch = "wasm32"))]
            publish_tx,
            stop,
            #[cfg(not(target_arch = "wasm32"))]
            thread: Some(thread),
            handle,
            weather,
            biome_climates,
            biome_names,
            command_tree,
            sky_default: Arc::new(SkyDefaultCell::default()),
            local_uuid,
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

    /// Ask the net thread to add a TCP listener to the **already-running**
    /// integrated server (issue #562) — "Open to LAN" without a restart.
    /// `port` of `0` asks the OS for one (issue #559); the actual bound port
    /// is reported back through [`NetUpdate::LanOpened`], never the number
    /// passed here.
    ///
    /// Best-effort like [`send_action`](Self::send_action): silently dropped
    /// if the net thread has already gone away, and a
    /// `NetUpdate::LanPublishError` — **not** `NetUpdate::Error`, which would
    /// end the session — comes back (through the ordinary
    /// [`poll`](Self::poll)) if the session running on it is not a
    /// singleplayer world, is one with nothing to publish (see
    /// `IntegratedServer::publish`'s own doc comment for exactly which
    /// constructors build a publishable world), or is already published — a
    /// second press of the pause menu's Open to LAN button before
    /// [`crate::menu::nav::MenuNav`] catches up with [`NetUpdate::LanOpened`]
    /// and stops offering it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn publish_to_lan(&self, port: u16) {
        let _ = self.publish_tx.send(port);
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

    /// Every currently-tracked entity as a raw, version-free [`EntityView`],
    /// straight off the client-owned entity table through the shared handle.
    /// Empty before login.
    ///
    /// Issue #36 deleted the `entity_snapshot`/`EntitySnapshot` lowering this
    /// used to feed — `entities.rs`'s `fold_entities` reads ingest components
    /// directly now, inside `Sim`'s own `World`, so nothing production-side
    /// needs a version-free copy any more. This passthrough survives only for
    /// callers with no ingest `World` of their own to write into — a live
    /// integration test that drives a bare [`crate::entities::EntityInterpolator`]
    /// (which installs no `IngestPlugin`) has to translate this into ingest
    /// components by hand, the same way `resolve_entity_facts` reads them
    /// live; see `tests/live_entity_render.rs`'s `apply_view` for that
    /// translation.
    #[must_use]
    pub fn entities(&self) -> Vec<EntityView> {
        self.handle.get().map_or_else(Vec::new, |h| h.entities())
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

    /// Predict a `key.drop` press on hotbar slot `selected` (`0..9`), returning
    /// whether anything was dropped. A no-op returning `false` before login or
    /// off a live connection.
    ///
    /// # This is half of a pair, and the other half is the caller's
    ///
    /// Call it immediately **before** `send_action(DropSelectedItem …)`, which is
    /// vanilla's order (`LocalPlayer.java`: `removeFromSelected` then
    /// `connection.send`). The send is deliberately *not* folded in here so the
    /// spectator gate stays in one place — `App::drop_selected_action` already
    /// returns `None` for a spectator, and calling this inside that `if let`
    /// gives the prediction the same gate for free.
    ///
    /// # Why the count is wrong without it
    ///
    /// `DROP_ITEM`/`DROP_ALL_ITEMS` are the one inventory change a vanilla server
    /// applies **silently**: `ServerGamePacketListenerImpl.java` calls
    /// `player.drop(…)` and returns without any slot or content packet. So
    /// [`player_menu`](Self::player_menu) — which is what the HUD hotbar and the
    /// inventory screen both read — keeps reporting the pre-drop count forever
    /// unless this runs. Nothing is late here; without the prediction there is no
    /// second chance.
    pub fn predict_drop_selected(&self, selected: usize, all: bool) -> bool {
        self.handle
            .get()
            .is_some_and(|h| h.drop_selected(selected, all))
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

    /// The local player's UUID, or `None` in the short window between thread
    /// spawn and the connecting [`LoginProfile`] being built (a handful of
    /// instructions — see [`SharedLocalUuid`]) or for a loopback client built
    /// with [`Self::loopback_with_feed`], which has no `LoginProfile` at all.
    ///
    /// Issue #189: this is the identity `crate::menu::social::
    /// entries_from_tablist` needs to exclude the local player from the
    /// Social Interactions roster.
    #[must_use]
    pub fn local_uuid(&self) -> Option<uuid::Uuid> {
        self.local_uuid.get().copied()
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

    /// Clone out the `Arc`-backed command-tree cell, for the same reason
    /// [`shared_weather`](Self::shared_weather) exists: the menu layer reads it
    /// per frame, after this `NetClient` has been moved into
    /// `Sim::attach_net`.
    ///
    /// See [`CommandTreeCell`] for what does and does not consume it yet —
    /// today the fold is live and the screens that should read it are still
    /// passing `None` (issue #471, steps 2 and 3).
    #[must_use]
    pub fn shared_command_tree(&self) -> SharedCommandTree {
        Arc::clone(&self.command_tree)
    }

    /// Clone out the `Arc`-backed biome-name cell, for the same reason
    /// [`shared_weather`](Self::shared_weather) exists: `crate::sim` reads
    /// this once at `TerrainMesh` construction/refresh time and hands the
    /// snapshot down into `SectionSnapshot` for the mesh worker threads —
    /// see `crate::mesher::biome_name_at`.
    #[must_use]
    pub fn shared_biome_names(&self) -> SharedBiomeNames {
        Arc::clone(&self.biome_names)
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
            // Its receiver is dropped with `_publish_rx` below, same as `_tx`
            // above: nothing on a loopback ever calls `publish_to_lan`.
            publish_tx: mpsc::channel().0,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            handle: Arc::new(OnceLock::new()),
            weather: Arc::new(WeatherCell::default()),
            biome_climates: Arc::new(BiomeClimateCell::default()),
            biome_names: Arc::new(BiomeNameCell::default()),
            command_tree: Arc::new(CommandTreeCell::default()),
            sky_default: Arc::new(SkyDefaultCell::default()),
            local_uuid: Arc::new(OnceLock::new()),
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
            // See `loopback`'s identical field for why an immediately-dropped
            // receiver is fine here.
            publish_tx: mpsc::channel().0,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            handle: Arc::new(OnceLock::new()),
            weather: Arc::new(WeatherCell::default()),
            biome_climates: Arc::new(BiomeClimateCell::default()),
            biome_names: Arc::new(BiomeNameCell::default()),
            command_tree: Arc::new(CommandTreeCell::default()),
            sky_default: Arc::new(SkyDefaultCell::default()),
            local_uuid: Arc::new(OnceLock::new()),
            // Bound by `Sim::attach_net`; a loopback with no `Sim` folds nothing.
            session: None,
        };
        (client, action_rx, tx)
    }
}

impl Drop for NetClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Browser: there is no handle to join — see the `thread` field. Setting `stop`
        // above is the whole teardown.
        #[cfg(not(target_arch = "wasm32"))]
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

/// The session driver's body: connect, log in, and pump events until `stop`.
///
/// **`async fn`, and the *whole* driver — `run` below is a native wrapper.** The two
/// targets differ only in how this future is driven:
///
/// * **Native.** `run` builds a `current_thread` runtime on its own OS thread and
///   `block_on`s this. Unchanged from before the split, including the property the
///   comments inside rely on: a runtime *is* entered, so the integrated server's
///   serving task has somewhere to go.
/// * **Browser.** `NetClient::spawn` hands this straight to
///   `wasm_bindgen_futures::spawn_local`. There is no thread and no `block_on`, and
///   neither is available: `std::thread::Builder::spawn` returns
///   `Err(Unsupported)` on wasm32, and the old call site `.expect()`ed it — so the
///   graceful error became a panic, and *that* was the last thing between the
///   browser's title screen and a world. `block_on` could not have rescued it
///   either; on a browser main thread there is no second thread to make the future
///   progress.
///
/// Extracted verbatim rather than reimplemented, for the reason `mesh_one` and
/// `finish_bring_up` were: this is ~500 lines of login, world-sync and teardown, and
/// two copies would diverge into a browser session that is subtly wrong rather than
/// one that fails to build.
#[expect(
    clippy::too_many_arguments,
    reason = "the driver's shared cells, one per subsystem the render thread reads; \
              bundling them into a struct would only move the same list"
)]
async fn run_async(
    origin: Origin,
    protocol: i32,
    tx: Sender<NetUpdate>,
    action_rx: Receiver<ClientAction>,
    // Issue #562. Native only — the capability it drives (`IntegratedServer::publish`)
    // needs a real TCP socket, which wasm32 does not have; the wasm `spawn_local`
    // call site below passes nothing for this parameter, matching how `world_dir`/
    // `lan_port` are already cfg-gated on `Origin::Integrated`.
    #[cfg(not(target_arch = "wasm32"))] publish_rx: Receiver<u16>,
    stop: Arc<AtomicBool>,
    shared_handle: SharedHandle,
    weather: SharedWeather,
    biome_climates: SharedBiomeClimates,
    biome_names: SharedBiomeNames,
    command_tree: SharedCommandTree,
    local_uuid: SharedLocalUuid,
    session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    offline: OfflineIdentity,
) {
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
        // The Open-to-LAN world's own save handle (issue #535). `open_to_lan` sets
        // `save: None` and so flushes nothing at shutdown; this is what the
        // teardown below writes through, and it is `None` for every other origin.
        #[cfg(not(target_arch = "wasm32"))]
        let mut lan_autosave: Option<lodestone_server::region_source::WorldSaveHandle> = None;
        let (server, auth, integrated_io) = match origin {
            Origin::Remote { host, port, auth } => (ServerAddress { host, port }, auth, None),
            Origin::Integrated {
                protocol: server_protocol,
                seed,
                view_radius,
                #[cfg(not(target_arch = "wasm32"))]
                world_dir,
                #[cfg(not(target_arch = "wasm32"))]
                lan_port,
                #[cfg(not(target_arch = "wasm32"))]
                online_mode,
            } => {
                // Issue #468: the world's **stored** seed wins over the
                // requested one, so this has to be resolved before the
                // generator is built — the generator is seeded once and never
                // reseeded. A failure here is reported and the session falls
                // back to a non-persistent world rather than being aborted:
                // losing saves is bad, but refusing to open the game at all
                // because a data directory is read-only is worse, and a silent
                // re-roll of the seed (which is what vanilla itself does here)
                // is the exact defect this issue exists to fix.
                #[cfg(not(target_arch = "wasm32"))]
                let seed = match &world_dir {
                    Some(dir) => {
                        match lodestone_server::region_source::resolve_world_seed(dir, seed) {
                            Ok(resolved) => {
                                tracing::info!(
                                    target: "net",
                                    world_dir = %dir.display(),
                                    seed = resolved.seed,
                                    created = resolved.created,
                                    "opening a persistent singleplayer world"
                                );
                                resolved.seed
                            }
                            // Fatal for the same reason the open below is: a
                            // world whose stored seed we cannot read is one we
                            // would silently regenerate differently.
                            Err(e) => {
                                let _ = tx.send(NetUpdate::Error(format!(
                                    "cannot read the world seed for {}: {e}",
                                    dir.display()
                                )));
                                return;
                            }
                        }
                    }
                    None => seed,
                };
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
                // lazy per column, and since issue #453 the initial view is
                // streamed **outward from the player's own column**, one Chebyshev
                // ring generated and encoded before the next is asked for — so the
                // first terrain reaches the client after a single column rather
                // than after all 361. A composed column measures ~222 ms for
                // contiguous columns from one source (see
                // `docs/world-open-latency.md`); the ~12 ms in
                // `docs/chunk-memory-pool-footprint.md` predates carvers, ores and
                // vegetation composing in and is not the figure to reason from.
                let source = lodestone_server::overworld_chunk_source(seed);
                // Open to LAN (issue #535's scope 1). Taken before the
                // in-memory constructors below because it is a *different
                // server*: `IntegratedServer::open_to_lan` binds a TCP listener
                // and every client — this one included — dials it, so there is no
                // duplex to hand back and the transport falls through to the
                // ordinary remote path.
                //
                // # Two persistence gaps, both in the server crate
                //
                // `open_to_lan` sets `save: None`, so it starts no autosave task
                // and flushes nothing at shutdown. This branch therefore drives
                // the save itself: it wraps `source` in a `RegionChunkSource` (so
                // a saved world's existing terrain and edits *load*) and holds the
                // `WorldSaveHandle`, writing on the same `AUTOSAVE_INTERVAL`
                // singleplayer uses and once more before the handle drops.
                //
                // What it still cannot carry: `open_to_lan` builds its own
                // `BlockEntityHandle::default()` rather than taking the source's,
                // so **container contents and scheduled ticks placed while hosting
                // do not persist** — chest lids animate and furnaces cook, and
                // none of it survives a restart. Closing that needs `LanConfig` to
                // grow a world directory and reuse `open_persistent_with_mobs`'s
                // save wiring, which is a `crates/lodestone-server` change.
                //
                // Structured as one `open_lan_world` call whose result is threaded
                // into the same `(server, client_io)` pair the in-memory
                // constructors produce, with `client_io: None` standing for "there
                // is a socket, dial it". A bare `if lan_port.is_some()` around the
                // whole open cannot work here: both branches consume `source` and
                // `server_protocol`, and `#[cfg]` does not attach to an `if` in a
                // match arm's tail.
                #[cfg(not(target_arch = "wasm32"))]
                let (server, client_io, lan_address) = match lan_port {
                    Some(port) => {
                        match open_lan_world(server_protocol, source, &world_dir, view_radius, port, online_mode)
                            .await
                        {
                            Ok((server, address, save)) => {
                                if let Some(save) = save {
                                    lan_autosave = Some(save.clone());
                                    tokio::spawn(async move {
                                        loop {
                                            tokio::time::sleep(AUTOSAVE_INTERVAL).await;
                                            let save = save.clone();
                                            let _ = tokio::task::spawn_blocking(move || save.save())
                                                .await;
                                        }
                                    });
                                }
                                let _ = tx.send(NetUpdate::LanOpened { port: address.port });
                                (server, None, Some(address))
                            }
                            Err(message) => {
                                let _ = tx.send(NetUpdate::Error(message));
                                return;
                            }
                        }
                    }
                    None => {
                // Issue #217: `MobSim` computed AI motion server-side with no
                // production consumer streaming it anywhere — an island by its
                // own module doc's admission. `open_in_memory_with_mobs` is
                // the production wiring: it spawns a task that owns a live
                // `MobSim` over a snapshot of the **same** terrain this
                // connection is served — one shared `ChunkStore` since issue
                // #454, where it used to be a second independent generator that
                // merely agreed — and republishes positions every tick through
                // the entity-sync pass `serve_connection` already runs on this
                // connection's own inbound-packet cadence. `wasm32` gets the old
                // mob-free path: the tick loop needs `tokio::time`, which is
                // unavailable there (see `lodestone_server`'s own doc
                // comment on `mobs::run_mob_tick_loop`) — a real, documented
                // gap, not a silent one.
                    // A small fixed radius around the join spawn (chunk
                    // (0,0), matching `V770ServerProtocol::begin_play`'s
                    // hardcoded `spawn_x`/`spawn_z` = 8) — independent of
                    // the client's own (possibly much larger) view radius,
                    // since this only needs to be big enough for a handful
                    // of wandering mobs, not the whole streamed view.
                    let mob_radius = view_radius.clamp(1, 3);
                    let mob_area = (-mob_radius..=mob_radius, -mob_radius..=mob_radius);
                    // Issue #468: the whole point. `open_persistent_with_mobs`
                    // wraps `source` in a `RegionChunkSource` *below* the
                    // `ChunkStore` and above the generator, so every existing
                    // mutation path (player edits, random ticks, the mob sim's
                    // grazing) is carried without any of them being touched.
                    // The autosave it spawns does its filesystem work inside
                    // `spawn_blocking`, so a full-region write never lands on
                    // the thread `run_tick_loop` shares — deliberate, because
                    // the world-open stall (10.86 s → 75.6 ms,
                    // `docs/world-open-latency.md`) had exactly that shape.
                    //
                    // `min_y`/`height` come off the source rather than being
                    // written as `(-64, 384)` here: `region_source`'s own
                    // gotcha is that they must match the world the columns came
                    // from, and a literal at this call site is a copy that can
                    // drift from the generator that produced them.
                    let (server, client_io) = match &world_dir {
                        Some(dir) => {
                            let (min_y, height) = (source.min_y(), source.height());
                            match lodestone_server::IntegratedServer::open_persistent_with_mobs(
                                server_protocol,
                                dir,
                                source,
                                min_y,
                                height,
                                mob_area,
                                (8, 8),
                                6,
                                view_radius,
                                AUTOSAVE_INTERVAL,
                            ) {
                                // The third element is a second handle to the
                                // same world, for callers that mutate outside
                                // the connection loop. The shell mutates
                                // through the wire like any client, so it
                                // wants none.
                                Ok((server, client_io, _world)) => (server, client_io),
                                // **Fatal, deliberately.** The tempting
                                // alternative is to fall back to an in-memory
                                // world and report a warning, and it is worse:
                                // the player builds for an hour on top of a
                                // toast they did not read, and loses all of it.
                                // Refusing to open a world we cannot save is
                                // the honest failure. (The only cause is the
                                // region directory being uncreatable.)
                                Err(e) => {
                                    let _ = tx.send(NetUpdate::Error(format!(
                                        "cannot open {} for saving: {e}",
                                        dir.display()
                                    )));
                                    return;
                                }
                            }
                        }
                        None => lodestone_server::IntegratedServer::open_in_memory_with_mobs(
                            server_protocol,
                            source,
                            mob_area,
                            (8, 8),
                            6,
                            view_radius,
                        ),
                    };
                    (server, Some(client_io), None)
                    }
                };
                #[cfg(target_arch = "wasm32")]
                let (server, client_io, lan_address): (_, _, Option<ServerAddress>) = {
                    let (server, io) = lodestone_server::IntegratedServer::open_in_memory(
                        server_protocol,
                        source,
                        view_radius,
                    );
                    (server, Some(io), None)
                };
                integrated_server = Some(server);
                match (client_io, lan_address) {
                    // Singleplayer over the in-memory duplex: the address is
                    // synthetic and only echoed into the handshake.
                    (Some(io), _) => {
                        let (host, port) = SINGLEPLAYER_ADDRESS;
                        (
                            ServerAddress {
                                host: host.to_string(),
                                port,
                            },
                            // Never authenticated, and this matches vanilla
                            // rather than merely being convenient:
                            // `handleHello` skips the encryption request for
                            // `isMemoryConnection()`, so a singleplayer host
                            // has nothing to prove and no session to spend.
                            RemoteAuth::Offline,
                            Some(io),
                        )
                    }
                    // Open to LAN: there is a real socket, so the host dials it
                    // over loopback exactly as a remote join would. With
                    // `online_mode` off (the default, and every caller before
                    // this field existed) our own integrated server answers
                    // `online_mode(false)` in its status/login, so this stays
                    // offline too. **With it on, the listener now demands the
                    // real RSA/AES handshake from every connection it accepts —
                    // including this loopback one** — so the host's own join
                    // has to present a real Microsoft session or the listener
                    // it just opened would refuse its own host.
                    //
                    // The inner `cfg` (rather than gating the whole arm) is
                    // load-bearing on wasm32: `online_mode` and
                    // `RemoteAuth::SelectedAccount` are both native-only, and
                    // even though `lan_address` is unconditionally `None` on
                    // wasm32 (this arm is unreachable there), an unreachable
                    // match arm still has to *type-check* — `just wasm-check`
                    // caught exactly this the first time this arm's body
                    // referenced `online_mode` unconditionally.
                    (None, Some(address)) => (
                        address,
                        {
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                if online_mode {
                                    RemoteAuth::SelectedAccount
                                } else {
                                    RemoteAuth::Offline
                                }
                            }
                            #[cfg(target_arch = "wasm32")]
                            {
                                RemoteAuth::Offline
                            }
                        },
                        None,
                    ),
                    // Unreachable by construction — `open_lan_world` returns an
                    // address on success and this arm returns early on failure.
                    (None, None) => unreachable!("an integrated server with no transport"),
                }
            }
        };

        // Turn the [`RemoteAuth`] *request* into either a live session or a
        // reason there isn't one. This is the step whose absence was the whole
        // bug: `ClientBuilder::online_session` had exactly one shell caller
        // (`connect_online`) and that constructor had none, so no join ever
        // carried a session and an online-mode server always answered "no
        // Microsoft session was configured" — while the account switcher, three
        // screens away, displayed the player's real premium username.
        //
        // It happens here rather than at the call site because it `await`s: the
        // refresh token is exchanged with Microsoft over HTTPS. `run_async` is
        // inside the net thread's `block_on`, so a runtime exists; every
        // `NetClient::connect` caller is synchronous render-thread UI code and
        // could never have done this itself. `NetUpdate::Connecting` has already
        // been sent, so the loading screen is up for the duration.
        //
        // **A failure here does not abort the join.** An offline-mode server
        // never sends an encryption request at all — vanilla's
        // `ServerLoginPacketListenerImpl.handleHello` gates it on
        // `usesAuthentication() && !isMemoryConnection()` — so a stale token has
        // no bearing on joining one, and refusing to dial would break joins that
        // work. The reason is handed to the builder instead and only spent if the
        // server turns out to demand online mode.
        #[cfg(not(target_arch = "wasm32"))]
        let (auth, auth_unavailable) = match auth {
            RemoteAuth::Offline => (None, None),
            RemoteAuth::Session(session) => (Some(session), None),
            RemoteAuth::SelectedAccount => {
                // `reqwest::Client::new()` panics with no rustls provider
                // installed, and that is a runtime panic no `cargo check` sees —
                // so the install sits next to the construction, as it does in
                // `lodestone_client`'s driver. Idempotent.
                lodestone_auth::install_crypto_provider();
                let http = reqwest::Client::new();
                match lodestone_auth::resolve_selected_account(&http).await {
                    lodestone_auth::SelectedAccount::Online(session) => {
                        // Logged at info, and it names the account: this line is
                        // how the *next* online-join report is attributable
                        // without a debugger. A join that authenticates and one
                        // that silently fell back to offline used to look
                        // identical from the outside.
                        tracing::info!(
                            target: "auth",
                            account = %session.profile.name,
                            "joining with the selected Microsoft account"
                        );
                        (Some(session), None)
                    }
                    lodestone_auth::SelectedAccount::Offline => {
                        tracing::info!(
                            target: "auth",
                            "no Microsoft account selected; joining with the offline identity"
                        );
                        (None, None)
                    }
                    lodestone_auth::SelectedAccount::Unavailable { account, detail } => {
                        // `warn`, not `error`: against an offline-mode server
                        // this is genuinely harmless and the session that
                        // follows is fine.
                        tracing::warn!(
                            target: "auth",
                            %account,
                            %detail,
                            "could not use the selected Microsoft account; \
                             joining offline (an online-mode server will refuse this)"
                        );
                        (None, Some((account, detail)))
                    }
                }
            }
        };
        // Browser: `RemoteAuth::SelectedAccount` does not exist and
        // `RemoteAuth::Session`'s payload is uninhabited, so this is statically
        // always `None` — `match session {}` says so to the compiler rather than
        // inventing a stand-in.
        #[cfg(target_arch = "wasm32")]
        let auth: Option<OnlineSession> = match auth {
            RemoteAuth::Offline => None,
            RemoteAuth::Session(session) => match session {},
        };

        // Online mode (issue #65) supplies the account's real identity; offline
        // mode presents the caller's [`OfflineIdentity`] — for `connect` /
        // `open_singleplayer` the *persisted* one, so the player is the same
        // account on every launch, and for `connect_as` whatever a live gate
        // asked for.
        //
        // **This arm used to be `unique_username()` + `Uuid::new_v4()`**, which
        // is a new offline account every launch: the name because that helper
        // cannot repeat by construction, and the uuid because it is random. The
        // owner's "I keep spawning in the air even if I rejoin" was the visible
        // half. The comment that lived here justified it by the
        // dead-player-blackout hazard, which is real — but that is a *test*
        // requirement, and it is now met where it belongs, at `connect_as`. See
        // `crate::offline_identity` for why both halves had to change and which
        // server derives which.
        // `OnlineSession` is uninhabited on wasm32, so the `Some` arm cannot be
        // entered there — but it still has to type-check, and `Infallible` has no
        // `profile` field. `match session {}` is the honest spelling of "this branch
        // does not exist" and needs no stand-in profile.
        let profile = match &auth {
            #[cfg(not(target_arch = "wasm32"))]
            Some(session) => LoginProfile {
                username: session.profile.name.clone(),
                uuid: session.profile.id,
            },
            #[cfg(target_arch = "wasm32")]
            Some(session) => match *session {},
            None => offline.login_profile(),
        };
        // Published immediately, not after login: issue #189's roster refresh
        // needs the identity to exclude as soon as a session exists, and there
        // is nothing fallible between here and the value itself (unlike
        // `shared_handle`, which waits on a real handshake).
        let _ = local_uuid.set(profile.uuid);

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
        //
        // Captured before `server` moves into the builder below: the browser
        // join arm further down needs the *destination* the player actually
        // chose to put on the relay URL as a `?host=&port=` query pair (see
        // `crate::platform::relay::relay_ws_url_for`) — the relay dials
        // whatever a connection names, not one fixed `--target`, so this must
        // travel with the connection rather than being assumed on the relay
        // side. Unused on native (`ClientBuilder::connect` reaches the real
        // server directly, no relay involved), hence the `cfg`.
        #[cfg(target_arch = "wasm32")]
        let relay_destination = (server.host.clone(), server.port);
        let mut builder = ClientBuilder::new(server, profile, adapter)
            .connect_timeout(Some(Duration::from_secs(10)))
            // Issue #280: arm the read timeout so a server that hangs (sends
            // nothing) surfaces as a disconnect instead of stalling the session
            // forever. The mechanism is per-packet, so the server's own 15-second
            // keep-alive keeps a healthy session clear of it — see [`READ_TIMEOUT`].
            .read_timeout(Some(READ_TIMEOUT))
            .respawn_policy(RespawnPolicy::Manual);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(session) = auth {
            builder = builder.online_session(session);
        }
        // Mutually exclusive with the arm above by construction — the resolution
        // yields a session or a reason, never both — but wired as an independent
        // `if` so that stays true by the builder's own contract (a real session
        // wins) rather than by this function's control flow.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((account, detail)) = auth_unavailable {
            builder = builder.online_session_unavailable(account, detail);
        }
        // `OnlineSession` is uninhabited here, so this branch cannot be entered and
        // `match session {}` says so to the compiler rather than to a reader.
        // `ClientBuilder::online_session` does not exist on wasm32 either.
        #[cfg(target_arch = "wasm32")]
        if let Some(session) = auth {
            match session {}
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
            None => {
                // Browser: `ClientBuilder::connect` is `cfg(not(wasm32))` because it
                // opens a `TcpStream`, which a page cannot do at any layer. A browser
                // multiplayer join instead dials `lodestone-net`'s `ws-web` transport
                // through the relay and lands on `connect_with` above — the same arm
                // singleplayer's in-memory duplex uses, and everything past the
                // transport (handshake, login, the driver's event loop) is the
                // version adapter's `begin_login`/`Driver::run`, unchanged from the
                // native path. This mirrors `menu/status.rs`'s `relay_probe`, the
                // working precedent for dialling the relay from wasm32 — same URL
                // source (`crate::platform::relay::relay_ws_url_for`, which names
                // *this* connection's destination on top of the relay endpoint —
                // see that function's doc for why the relay cannot be trusted to
                // guess it), same reason `tokio::time::timeout` cannot be used for
                // the deadline (it hangs on its first poll on wasm32 — no timer
                // driver), so this races the dial against
                // `crate::platform::relay::sleep` exactly as the probe does.
                #[cfg(target_arch = "wasm32")]
                {
                    let url = crate::platform::relay::relay_ws_url_for(
                        &relay_destination.0,
                        relay_destination.1,
                    );
                    let dial = lodestone_net::WsWebTransport::connect(&url);
                    let transport = tokio::select! {
                        result = dial => match result {
                            Ok(transport) => transport,
                            Err(e) => {
                                let _ = tx.send(NetUpdate::Error(format!(
                                    "connect: relay ({url}): {e}"
                                )));
                                return;
                            }
                        },
                        // Same budget as the native TCP dial's `connect_timeout`
                        // above, applied by hand here because `connect_with` (the
                        // only entry point wasm32 has) ignores that builder option
                        // outright — it is handed an already-established
                        // transport, so there is nothing left for a connect
                        // timeout to bound.
                        () = crate::platform::relay::sleep(Duration::from_secs(10)) => {
                            let _ = tx.send(NetUpdate::Error(format!(
                                "connect: relay ({url}) did not answer within 10s"
                            )));
                            return;
                        }
                    };
                    builder.connect_with(transport)
                }
                #[cfg(not(target_arch = "wasm32"))]
                match builder.connect().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = tx.send(NetUpdate::Error(format!("connect: {e}")));
                        return;
                    }
                }
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

        // Issue #449: a real boundary — the handshake and login have completed
        // (`connect`/`connect_with` returned a handle), so the loading screen
        // stops saying "Connecting to the server..." and says "Joining
        // world...". `NetUpdate::LoggedIn` moves it on again, from the forward
        // loop below.
        let _ = tx.send(NetUpdate::ConnectPhase(
            crate::menu::loading::ConnectPhase::Joining,
        ));

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
                if handed_actions == 1 {
                    tracing::info!("client: first action sent to server");
                }
                if handed_actions.is_multiple_of(100) {
                    tracing::info!("client: {} actions sent to server so far (incl. movement, keep-alive)", handed_actions);
                }
                if handed_actions == 1 || handed_actions.is_multiple_of(20) {
                    tracing::debug!(target: "net", "handed {handed_actions} action(s) to client handle (encode is the adapter's job)");
                }
            }
            // Issue #562: "Open to LAN" without a restart. `publish_to_lan`
            // requests land here rather than through `handle.send_action` above
            // — this is not a wire packet, it is a local call into the
            // **already-running** `IntegratedServer` this loop is holding for
            // exactly this session (singleplayer only; `integrated_server` is
            // `None` for a remote join, and a request against `None` is
            // reported rather than silently dropped, same as a genuine bind
            // failure below).
            #[cfg(not(target_arch = "wasm32"))]
            while let Ok(port) = publish_rx.try_recv() {
                match integrated_server.as_mut() {
                    Some(server) => {
                        // No discovery motd here: the shell does not yet carry
                        // a running session's world directory down to this
                        // loop (only `Origin::Integrated`'s *opening* arm has
                        // it) — see that arm's `lan_motd`. Discovery is a
                        // "nice to find it without typing the address"
                        // convenience, not the fix this issue is about; a
                        // joiner who knows the host's address is unaffected.
                        match server.publish(("0.0.0.0", port), None).await {
                            Ok(addr) => {
                                let _ = tx.send(NetUpdate::LanOpened { port: addr.port() });
                            }
                            Err(e) => {
                                // Non-fatal: `IntegratedServer::publish` returning
                                // `Err` (already published, or a bind failure)
                                // leaves this loop, `integrated_server` and every
                                // connection it serves untouched — see
                                // `NetUpdate::LanPublishError`'s own doc for why
                                // this must never be `NetUpdate::Error`.
                                let _ = tx
                                    .send(NetUpdate::LanPublishError(format!("open to LAN: {e}")));
                            }
                        }
                    }
                    None => {
                        // Also non-fatal, for the same reason: a race between this
                        // request and a session teardown (or a stray request off a
                        // remote join, which `open_current_world_to_lan` already
                        // guards against client-side) must not take down a
                        // connection that has nothing to do with the request.
                        let _ = tx.send(NetUpdate::LanPublishError(
                            "only a world you are hosting can be opened to LAN".to_string(),
                        ));
                    }
                }
            }
            // A short timeout keeps the outbound drain responsive even when the
            // server is quiet (no inbound events to wake us).
            //
            // **Native only.** `tokio::time::timeout` needs a timer driver, which
            // comes from the entered `current_thread` runtime this function's
            // native caller builds (see this file's `run`) — but on `wasm32`
            // there is no entered runtime at all (`spawn_local` is not
            // `#[tokio::main]`), the same gap `read_packet_timed`'s own doc
            // comment already names for the read side. The two symptoms are not
            // the same, though, and the difference is worth recording: a naive
            // guess would be that `timeout()` just never *fires* (the 15 ms
            // never elapses, degrading gracefully to an untimed wait). Measured
            // instead, with a one-shot `tracing::debug!` bracketing the call
            // during the wasm32 join stall this diagnoses: the "enter" line logs
            // exactly once per session and the "exit" line **never logs at
            // all**, even though `events.recv()` already has a backlog of
            // `ChunkLoaded` events queued and would resolve immediately on its
            // own. That rules out "the sleep never elapses" — the call hangs on
            // its *first* poll, before the inner future is ever reached, which
            // is consistent with `tokio::time::timeout`'s own deadline
            // computation reaching for a clock with no driver behind it on this
            // target (the same `Instant`/`SystemTime` trap class `lodestone-time`
            // exists to confine elsewhere in this crate — see `crate::spawn`'s
            // doc comment for the sibling case in `lodestone-server`).
            //
            // Once this call stops returning, this loop stops looping: it never
            // reaches `events.recv()` again, so the bounded `ClientEvent`
            // channel (`DEFAULT_EVENT_BUFFER` deep) fills, `Driver::run`'s own
            // `self.events.send(event).await` then blocks, and *that* task stops
            // calling `read_packet` — which is what starves the reader half of
            // the `memory_pair` duplex `CLAUDE.md` localised this stall to. The
            // near-full buffer was a downstream symptom of this call never
            // returning, not an undersized constant: `DEFAULT_MEMORY_BUFFER`
            // stays untouched.
            //
            // The fix is not "a working wasm32 timer" — there is no timer driver
            // to build one on top of without a new dependency — it is to stop
            // asking for one. An untimed `events.recv()` gives up the "outbound
            // actions flush even when the server is quiet" property this
            // comment's first line describes, which matters least exactly when
            // it is missing least: a live session's server keeps producing
            // `ClientEvent`s (movement/entity sync, chunk churn) at well under
            // 15 ms in practice, so the loop wakes on its own. A genuinely idle
            // connection (nothing queued to send *and* nothing arriving) has
            // nothing that needs flushing anyway. Same accepted, documented gap
            // as `read_timeout` just above — not a silent one.
            #[cfg(target_arch = "wasm32")]
            tracing::debug!(target: "netbuf", "events:recv (untimed on wasm32)");
            #[cfg(target_arch = "wasm32")]
            let netbuf_timeout_result: Result<Option<ClientEvent>, tokio::time::error::Elapsed> =
                Ok(events.recv().await);
            #[cfg(not(target_arch = "wasm32"))]
            let netbuf_timeout_result =
                tokio::time::timeout(Duration::from_millis(15), events.recv()).await;
            match netbuf_timeout_result {
                Ok(Some(event)) => {
                    // Issue #613: answered here, before generic `forward`ing,
                    // because `handle` (the only thing that can send an
                    // outbound action) is in scope in this loop and not
                    // inside `forward` itself. See `auto_resource_pack_response`'s
                    // own doc for what this is and why.
                    if let Some(action) = auto_resource_pack_response(&event) {
                        let _ = handle.send_action(action);
                    }
                    if forward(
                        &tx,
                        &weather,
                        &biome_climates,
                        &biome_names,
                        &command_tree,
                        event,
                    )
                    .is_err()
                    {
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

        // Singleplayer only: stop the server we started, **and wait for it**.
        //
        // This used to be `trigger_shutdown()`, a fire-and-forget notify, after
        // which the binding dropped and `impl Drop for IntegratedServer`
        // *aborted* the tick and serving tasks. That was a second island one
        // layer above issue #468's: even with the persistent constructor wired
        // in, the final save could never run, because `save_now` lives in
        // `shutdown()` and nothing awaited it. Every edit since the last
        // autosave tick would have been lost on every quit — including, for a
        // short session, all of them.
        //
        // `shutdown()` joins the serving and tick tasks *before* flushing, so
        // an in-flight block edit cannot be dropped between the last tick and
        // the write, and the write itself is a `spawn_blocking` of only the
        // dirty columns. It is awaited here on the net thread, which
        // `NetClient::drop` joins — so quitting to the title screen blocks until
        // the world is on disk. That is intentional and is what vanilla's own
        // "Saving world" screen is: the alternative is a save racing process
        // exit.
        if let Some(server) = integrated_server {
            tracing::info!(target: "net", "stopping the integrated server and saving the world");
            // **`drop`, not `shutdown().await`, for a LAN handle.** `shutdown`
            // joins the accept loop, which is parked in `accept()` where the
            // notify cannot reach it, and joining it hung indefinitely for a
            // `view_radius: 0` handle while #535's own gate was written. Dropping
            // aborts both loops. There is nothing to lose by not joining: a LAN
            // handle has `save: None`, so its flush is the explicit one below.
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(save) = lan_autosave.take() {
                drop(server);
                match tokio::task::spawn_blocking(move || save.save()).await {
                    Ok(Ok(columns)) => {
                        tracing::info!(target: "net", columns, "LAN world saved");
                    }
                    Ok(Err(e)) => tracing::warn!(target: "net", "saving the LAN world failed: {e}"),
                    Err(e) => tracing::warn!(target: "net", "the LAN world save task failed: {e}"),
                }
                tracing::info!(target: "net", "integrated server stopped");
                return;
            }
            server.shutdown().await;
            tracing::info!(target: "net", "integrated server stopped");
        }
}

/// Native entry point: a `current_thread` runtime, blocking on [`run_async`].
#[cfg(not(target_arch = "wasm32"))]
#[expect(
    clippy::too_many_arguments,
    reason = "a pass-through to `run_async`; see that function"
)]
fn run(
    origin: Origin,
    protocol: i32,
    tx: Sender<NetUpdate>,
    action_rx: Receiver<ClientAction>,
    publish_rx: Receiver<u16>,
    stop: Arc<AtomicBool>,
    shared_handle: SharedHandle,
    weather: SharedWeather,
    biome_climates: SharedBiomeClimates,
    biome_names: SharedBiomeNames,
    command_tree: SharedCommandTree,
    local_uuid: SharedLocalUuid,
    session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    offline: OfflineIdentity,
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

    runtime.block_on(run_async(
        origin,
        protocol,
        tx,
        action_rx,
        publish_rx,
        stop,
        shared_handle,
        weather,
        biome_climates,
        biome_names,
        command_tree,
        local_uuid,
        session,
        offline,
    ));
}

/// The world name a LAN ping advertises — the world directory's own final
/// component, or a generic label for a throwaway in-memory world.
///
/// Vanilla's `LanServerPinger` sends `getMotd()`, which for a published
/// singleplayer world is the level name. There is no level-name field on this
/// side (`crate::saves` calls the one implicit world by its directory), so the
/// directory name is the closest true answer rather than a fabricated one.
#[cfg(not(target_arch = "wasm32"))]
fn lan_motd(world_dir: Option<&std::path::Path>) -> String {
    world_dir
        .and_then(|dir| dir.file_name())
        .map_or_else(|| "Lodestone World".to_string(), |name| name.to_string_lossy().into_owned())
}

/// The `LanConfig::online_mode` a LAN-open should carry (issue #273's shell
/// control): `None` when the host did not ask for it — the same `None` every
/// caller here passed before this field existed, so a host who leaves the
/// toggle off gets exactly the old offline behaviour and this makes no network
/// call at all (`OnlineModeConfig::new` only builds an HTTP client and a
/// closure; the session-server request happens per-connection, later, only if
/// a client actually joins).
///
/// `lodestone_auth::install_crypto_provider` only runs on the `true` arm:
/// `reqwest::Client::new()` panics with no rustls provider installed, and
/// skipping the install alongside skipping the client keeps the `false` arm
/// free of any of `Some`'s side effects, not merely of its network traffic.
#[cfg(not(target_arch = "wasm32"))]
fn lan_online_mode(enabled: bool) -> Option<lodestone_server::OnlineModeConfig> {
    if !enabled {
        return None;
    }
    lodestone_auth::install_crypto_provider();
    Some(lodestone_server::OnlineModeConfig::new(reqwest::Client::new()))
}

/// Bind the world to a TCP port with `IntegratedServer::open_to_lan` (issue
/// #535's scope 1), returning the handle, the address the host's own client
/// should dial, and the save handle when there is a world on disk.
///
/// Split out of [`run`]'s already-long `Origin::Integrated` arm, and `async`
/// because `open_to_lan` binds the listener before returning — which is what makes
/// `local_addr` immediately available, and so what lets a caller pass port `0`.
///
/// `Err` carries the message to surface on the Error screen. Both causes are
/// worth failing the whole session for: a world we cannot save must not be
/// published (an hour of building disappears behind a toast nobody read), and a
/// port we cannot bind means nobody can join, which is the entire request.
#[cfg(not(target_arch = "wasm32"))]
async fn open_lan_world(
    protocol: Box<dyn lodestone_server::ServerProtocol>,
    source: lodestone_server::OverworldChunkSource,
    world_dir: &Option<std::path::PathBuf>,
    view_radius: i32,
    port: u16,
    online_mode: bool,
) -> Result<
    (
        lodestone_server::IntegratedServer,
        ServerAddress,
        Option<lodestone_server::region_source::WorldSaveHandle>,
    ),
    String,
> {
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let config = |motd: String| lodestone_server::LanConfig {
        view_radius,
        // The whole point of the feature: without the multicast ping the world
        // never appears in anyone's server list and the address has to be read out
        // loud.
        discovery: Some(lodestone_server::LanDiscovery { motd }),
        online_mode: lan_online_mode(online_mode),
        ..lodestone_server::LanConfig::default()
    };
    let motd = lan_motd(world_dir.as_deref());
    let (server, save) = match world_dir {
        // `min_y`/`height` off the source, never a `(-64, 384)` literal — the
        // same gotcha `open_persistent_with_mobs`' call site carries.
        Some(dir) => {
            let (min_y, height) = (source.min_y(), source.height());
            // Singleplayer publish-to-LAN only ever opens the overworld's own
            // region store — `source` here is an `OverworldChunkSource`, never
            // the Nether or the End.
            let region = lodestone_server::region_source::RegionChunkSource::new(
                source,
                dir,
                lodestone_server::dimension::Dimension::Overworld,
                min_y,
                height,
            )
            .map_err(|e| format!("cannot open {} for saving: {e}", dir.display()))?;
            let save = region.save_handle();
            let server = lodestone_server::IntegratedServer::open_to_lan(
                addr,
                protocol,
                region,
                config(motd),
            )
            .await
            .map_err(|e| format!("cannot open port {port} to LAN: {e}"))?;
            (server, Some(save))
        }
        None => (
            lodestone_server::IntegratedServer::open_to_lan(addr, protocol, source, config(motd))
                .await
                .map_err(|e| format!("cannot open port {port} to LAN: {e}"))?,
            None,
        ),
    };
    // The OS-assigned port when `port` was `0`, the requested one otherwise.
    // Reported because nobody can join a world whose port they cannot see.
    let bound_port = server.local_addr().map_or(port, |a| a.port());
    tracing::info!(target: "net", port = bound_port, "opened the world to LAN");
    Ok((
        server,
        // Loopback, not the bind address: `0.0.0.0` is where the listener
        // accepts, and the host's own client dials the same machine.
        ServerAddress {
            host: "127.0.0.1".to_string(),
            port: bound_port,
        },
        save,
    ))
}

/// Issue #613: `ClientAction::ResourcePackResponse` had a real encoder
/// (`ClientboundResourcePackPushPacket`/`Pop` already decode into
/// `ClientEvent::ResourcePackPushed`/`Popped`) and **zero producers** —
/// `SetFlying`'s own island shape, except a server-marked `required` pack
/// disconnects a client that never answers
/// (`ServerCommonPacketListenerImpl.handleResourcePackResponse` never runs,
/// so vanilla treats the pack as permanently pending and eventually kicks the
/// player). No resource-pack screen exists yet to let the player accept or
/// decline, so this always declines — a real client with no such screen
/// would do the same rather than hang the connection. `ResourcePackPopped`
/// has nothing to answer and returns `None`.
///
/// Factored out of the net loop (rather than inlined at its one call site) so
/// it is unit-testable without the async loop and the live `ClientHandle`
/// around it.
fn auto_resource_pack_response(event: &ClientEvent) -> Option<ClientAction> {
    match event {
        ClientEvent::ResourcePackPushed { id, .. } => Some(ClientAction::ResourcePackResponse {
            id: *id,
            response: ResourcePackResponseKind::Declined,
        }),
        _ => None,
    }
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
    biome_names: &BiomeNameCell,
    command_tree: &CommandTreeCell,
    event: ClientEvent,
) -> Result<(), ()> {
    let update = match event {
        ClientEvent::Login { entity_id, .. } => NetUpdate::LoggedIn { entity_id },
        ClientEvent::Chat {
            text,
            kind,
            sender,
            ..
        } => match kind {
            // GameInfo is the action bar (SystemChat overlay), not the chat feed:
            // route it to the ActionBar overlay so it draws above the hotbar and
            // fades, instead of piling into the scrollback.
            lodestone_model::event::ChatKind::GameInfo => NetUpdate::ActionBar(text),
            _ => NetUpdate::Chat {
                text,
                player: matches!(kind, lodestone_model::event::ChatKind::Chat),
                // Carried verbatim so the sim can filter hidden players (issue
                // #419) — the suppression lives in `net_apply`, not here, so this
                // router keeps its one-job shape and the reader sees *every* chat
                // event routed, filtered or not.
                sender,
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
        // The client-side twin of the arm above, and the other half of the
        // failure Matthew reported as *"join errors dont get printed anywhere,
        // and the client just shows disconnected: stream closed"*. The driver's
        // `SessionOutcome::Failed(ClientError)` is unreachable to us — taking it
        // consumes the `ClientHandle` and we hold an `Arc` — so before this
        // event existed a mid-session transport/adapter/timeout failure reached
        // the shell as nothing at all: the channel closed, and the `Ok(None)`
        // arm in the loop below synthesised `"stream closed"`, which
        // `poll_net` then labelled a *server* disconnect. `NetUpdate::Error` is
        // the right target rather than `Disconnected`, and the difference is
        // visible: it carries `SessionEndKind::Failed`, so the screen title
        // becomes vanilla's `connect.failed` instead of `disconnect.lost`, and
        // `poll_net`'s arm logs the cause.
        //
        // Like `Disconnect`, this ends the forward loop: the driver has already
        // stopped and the only thing that could follow is the channel closing.
        ClientEvent::SessionFailed { reason } => {
            let _ = tx.send(NetUpdate::Error(reason));
            return Err(());
        }
        // No `HealthChanged`/`ExperienceChanged` arms: those fold into the
        // `Vitals`/`Xp` components on the net thread, and forwarding them here as
        // well would put a second writer on the shell side. See `NetUpdate`'s note
        // where the two variants used to be.
        ClientEvent::Death { message } => NetUpdate::Death {
            message: message.to_plain_string(),
        },
        // The dimension travels with the event rather than being read back off the
        // shared handle at the consumer — see `NetUpdate::Respawned::dimension`'s
        // doc for why a shared-state read there structurally cannot detect a
        // change.
        ClientEvent::Respawned { dimension, .. } => NetUpdate::Respawned {
            dimension: Some(dimension),
        },
        // WIN_GAME (issue #192): a pure signal, forwarded unconditionally —
        // `route()` claims this `shell: true, shell_conditional: false`, so
        // this arm is `must_forward()` and its absence would trip `forward`'s
        // own `debug_assert!` on the catch-all below.
        ClientEvent::WinGame => NetUpdate::WinGame,
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
        // Forwarded unfiltered, like the effect arms above: `net_apply` compares
        // against `server_entity_id()`. The filtering deliberately does **not**
        // happen here, so this router keeps its one-job shape and a reader can see
        // that the event is routed at all.
        ClientEvent::EntityHurtAnimation { entity_id, yaw } => {
            NetUpdate::HurtAnimation { entity_id, yaw }
        }
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
        // The eviction twin of the arm above, and it is the arm's *absence* that
        // was issue #479: the adapter had already dropped the column through the
        // `WorldSink`, so collision followed it and only the renderer was left
        // holding geometry for blocks the client no longer has. A notification
        // "with nothing left to do" is exactly what an island looks like from
        // inside — the thing left to do was on the GPU.
        ClientEvent::ChunkUnloaded { pos, .. } => NetUpdate::ChunkUnloaded { x: pos.x, z: pos.z },
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
        ClientEvent::SignEditorOpened { pos, is_front_text } => {
            NetUpdate::SignEditorOpened { pos, is_front_text }
        }
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
        // The same registry generation's entry names (follow-up to issue #96
        // / `eb423ac`), folded into the shared `BiomeNameCell` and
        // deliberately not forwarded — same shape as `BiomeClimates` above:
        // the whole table replaces at once, and the mesh worker threads read
        // it through `Sim`/`TerrainMesh`, not through this channel.
        ClientEvent::BiomeRegistryNames { names } => {
            biome_names.apply(&names);
            return Ok(());
        }
        // The server's Brigadier command tree (issue #470's decode, issue
        // #471's wire). Folded into the shared `CommandTreeCell` and not
        // forwarded — same shape as the two registry arms above: the whole
        // tree replaces at once, and the chat box and command-block screen
        // read it per frame from the menu layer rather than through this
        // channel.
        //
        // **This arm is load-bearing right now even before a screen reads the
        // cell.** `route` claims `shell`/`must_forward` for both command
        // variants, so without these two arms they fell through to the
        // terminal `_ =>` below and its `debug_assert!` fired on any
        // debug-build join to a real 26.2 server — exactly how the
        // `BiomeClimates` gap above was found. Release builds were unaffected,
        // which is what made it easy to miss.
        ClientEvent::CommandTreeUpdated { tree } => {
            command_tree.apply(*tree);
            return Ok(());
        }
        ClientEvent::CommandSuggestionsReceived {
            id,
            start,
            length,
            suggestions,
        } => {
            command_tree.apply_suggestions(
                lodestone_model::command_tree::CommandSuggestionsResponse {
                    id,
                    start,
                    length,
                    suggestions,
                },
            );
            return Ok(());
        }
        // The lightning flash (`ClientLevel.java`). A bolt is an ordinary
        // entity on the wire, so this arm **observes** the spawn and returns
        // without producing a `NetUpdate`: entities already reach the shell
        // through the ECS ingest fold, and forwarding one here would put a second
        // writer on state that has one. Only the *count* is published.
        //
        // This is a spawn-only approximation. Vanilla re-flashes `rand(3) + 1`
        // times per bolt by resetting the entity's `life`
        // (`LightningBolt.java`, `:131-134`), which needs the bolt's own
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

// `entity_snapshot` (the `EntityView` -> `EntitySnapshot` lowering) and
// `entity_snapshots()` above are deleted with issue #36: `entities.rs`'s
// `fold_entities` reads the ingest components directly inside its own write
// guard now, so there is nothing left to lower `EntityView` into. See
// `entities.rs`'s `fold_entities` doc and `docs/entity-components.md`'s
// "Update, and it changes the plan" for what replaced this.

#[cfg(test)]
mod tests {
    use super::*;

    // `unique_username` is a `lodestone-testsupport` helper and this crate now
    // depends on that crate **only** as a dev dependency, so this `use` is
    // reachable from `#[cfg(test)]` and from nowhere else in the lib. That is the
    // structural half of the offline-identity fix — see
    // `crate::offline_identity`'s module docs and
    // `tests/no_production_source_names_testsupport.rs`.
    use lodestone_testsupport::unique_username;

    /// Issue #613: a pushed resource pack must get an immediate, correctly-id'd
    /// decline — the producer half of `ClientAction::ResourcePackResponse`,
    /// which had an encoder and no producer at all before this.
    #[test]
    fn a_pushed_resource_pack_is_auto_declined_with_the_pushed_id() {
        let id = uuid::Uuid::from_u128(0x617);
        let event = ClientEvent::ResourcePackPushed {
            id,
            url: "https://example.invalid/pack.zip".to_owned(),
            hash: String::new(),
            required: true,
            prompt: None,
        };
        match auto_resource_pack_response(&event) {
            Some(ClientAction::ResourcePackResponse { id: answered, response }) => {
                assert_eq!(answered, id, "the response must name the pack that was pushed");
                assert_eq!(response, ResourcePackResponseKind::Declined);
            }
            other => panic!("expected an auto-decline, got {other:?}"),
        }
    }

    /// The companion control: an unrelated event (and `ResourcePackPopped`,
    /// which has nothing to answer) must not produce a response — proving the
    /// match above is actually discriminating on the event, not returning
    /// `Some` unconditionally.
    #[test]
    fn an_unrelated_event_and_a_pack_pop_get_no_auto_response() {
        assert!(auto_resource_pack_response(&ClientEvent::WinGame).is_none());
        assert!(
            auto_resource_pack_response(&ClientEvent::ResourcePackPopped { id: None }).is_none()
        );
    }

    /// The island assertion for the signed-in-account wiring, and the reason it
    /// is worth having at all: every piece of online-mode support existed and was
    /// individually tested — the `Session` type, `try_cached_session`,
    /// `ClientBuilder::online_session`, the driver's `begin_encryption`, the
    /// RSA/AES primitives — while the *first* link, a join that asks for the
    /// account, did not. So the tree was green, every subsystem's own suite
    /// passed, and a player with a working premium account was told no session
    /// was configured.
    ///
    /// This is a pin rather than a discovery, and that is the point: the
    /// regression it guards against is a one-token edit back to `Offline`, which
    /// no other test in this repo would notice, because joining offline is what
    /// every hermetic and live gate here deliberately does.
    #[test]
    fn a_production_join_requests_the_selected_microsoft_account() {
        assert!(
            matches!(RemoteAuth::for_production_join(), RemoteAuth::SelectedAccount),
            "a production multiplayer join must consult the account switcher; \
             requesting Offline here is the island that made a signed-in account unusable"
        );
    }

    /// The other half, and it is not symmetric decoration: a live gate asks for
    /// an exact username, so if `connect_as` resolved the selected account it
    /// would join under a *different* name — on a developer's machine only,
    /// where an account happens to be selected — and every gate would share one
    /// premium player file. That is the shared-offline-name eviction hazard
    /// `connect_as` exists to avoid, so the two origins must differ.
    /// The toggle's own discriminating pair (issue #273's shell-side
    /// control, `WorldCreationConfig::online_mode`): the default must stay
    /// offline — every caller before this field existed passed `false` here
    /// — and the enabled path must actually construct an `OnlineModeConfig`,
    /// not merely flip a bool nothing reads. A test that only exercised
    /// `true` could not show the default is intact, and the default is what
    /// every singleplayer session depends on to never authenticate.
    ///
    /// Neither arm makes a network call: `OnlineModeConfig::new` only builds
    /// an HTTP client and a closure — the session-server request happens
    /// per-connection, later, only if a client actually joins and the
    /// listener is running.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn lan_online_mode_defaults_off_and_the_enabled_path_constructs_real_config() {
        assert!(
            lan_online_mode(false).is_none(),
            "the default (every caller before this field existed) must stay offline"
        );
        assert!(
            lan_online_mode(true).is_some(),
            "the enabled path must actually construct an OnlineModeConfig"
        );
    }

    #[test]
    fn a_gate_join_never_consults_the_account_switcher() {
        let origin = NetClient::offline_origin("example.invalid".into(), 25565);
        let Origin::Remote { auth, .. } = origin else {
            panic!("offline_origin must build a Remote origin");
        };
        assert!(
            matches!(auth, RemoteAuth::Offline),
            "connect_as must stay offline regardless of what is selected"
        );
    }

    /// The `#[cfg(test)]` fork in [`NetClient::production_origin`], asserted
    /// rather than assumed.
    ///
    /// Resolving an account opens the OS keychain, POSTs to Microsoft, and
    /// rotates the stored refresh token — so a unit test that reached it could
    /// invalidate the credential the owner's real client is holding, while the
    /// suite reported green. The interception is deliberate; this test is what
    /// makes it *visible*, so nobody removes it as dead code and nobody is
    /// surprised that `NetClient::connect` behaves differently under `cargo
    /// test`. `a_production_join_requests_the_selected_microsoft_account` covers
    /// the production decision the fork bypasses, so the pair cannot both be
    /// satisfied by simply never resolving anything.
    #[test]
    fn unit_tests_never_resolve_a_real_microsoft_account() {
        let origin = NetClient::production_origin("example.invalid".into(), 25565);
        let Origin::Remote { auth, .. } = origin else {
            panic!("production_origin must build a Remote origin");
        };
        assert!(
            matches!(auth, RemoteAuth::Offline),
            "a unit-test build must not reach the keychain or Microsoft"
        );
    }

    /// `usernames_are_unique_per_call` used to live here, asserting a property
    /// of `lodestone_testsupport::unique_username` — which this crate re-exported
    /// into production. It has moved to where it belongs, beside the helper
    /// (`crates/lodestone-testsupport/tests/unique_username.rs`), and what
    /// replaces it here is the opposite assertion about the *shell's own* join
    /// path: two independent constructions must produce the **same** identity.
    ///
    /// The dead port is the same trick
    /// [`local_uuid_is_published_before_the_connection_even_resolves`] uses:
    /// `local_uuid` is set from the `LoginProfile` before `run` dials anything, so
    /// this observes the real production expression with no server involved.
    ///
    /// This gate reads the developer's real `offline.json` (via
    /// `OfflineIdentity::load`, which is what production calls) and **never
    /// writes** — the expected value is whatever that file says, i.e. it comes
    /// from outside this code. On a machine with no such file that is
    /// `DEFAULT_USERNAME`, which is still an outside constant and still
    /// deterministic; the failure message names the path so the run says which of
    /// the two it landed in rather than skipping.
    ///
    /// **The residual gap, stated rather than hidden:** on a machine with no
    /// `offline.json` this exercises only the *default* world, because
    /// `std::env::set_var` is `unsafe` under this workspace's `deny(unsafe_code)`
    /// so a test cannot point `LODESTONE_DATA_DIR` at a fixture. The
    /// stored-name world is covered hermetically by
    /// `tests/offline_identity_is_stable.rs` against `load_from`, which is the
    /// same function `load` delegates to — so what is untested here is only the
    /// path join, not the parse.
    #[test]
    fn two_offline_sessions_publish_the_same_identity() {
        let expected = crate::offline_identity::OfflineIdentity::load();
        let observed: Vec<Uuid> = (0..2)
            .map(|_| {
                let client = NetClient::connect("127.0.0.1".into(), 1, 776, None);
                let deadline = crate::platform::Instant::now() + Duration::from_secs(5);
                let mut uuid = None;
                while crate::platform::Instant::now() < deadline && uuid.is_none() {
                    uuid = client.local_uuid();
                    std::thread::sleep(Duration::from_millis(5));
                }
                uuid.expect("local_uuid publishes before the dial resolves")
            })
            .collect();

        assert_eq!(
            observed[0], observed[1],
            "two launches must join as the same offline account; \
             a fresh identity per launch is the defect this gate exists for"
        );
        assert_eq!(
            observed[0],
            expected.uuid(),
            "the published identity must be the *persisted* offline one \
             ({:?} at {}), not something the join path minted",
            expected.username(),
            crate::offline_identity::offline_identity_path().display()
        );
        // A random `Uuid::new_v4()` — the expression this replaced — is version
        // 4. A name-derived offline uuid is version 3. This one assertion fails
        // on the pre-fix code no matter what the persisted name happens to be.
        assert_eq!(
            observed[0].get_version_num(),
            3,
            "the offline uuid must be name-derived (version 3), not random"
        );
    }

    /// **The negative control** for the equality above: the same predicate,
    /// applied to the path that is *supposed* to vary, must disagree.
    ///
    /// Without it, "the two uuids matched" is equally consistent with
    /// `local_uuid` publishing a constant — nil, or the same default whatever the
    /// name — in which case the gate above proves nothing about the identity
    /// actually reaching the login packet. It also re-checks the thing the live
    /// gates depend on: [`NetClient::connect_as`] really does carry the caller's
    /// name through to the published identity.
    #[test]
    fn connect_as_varies_the_published_identity_with_the_name() {
        let names = [unique_username(), unique_username()];
        assert_ne!(names[0], names[1], "the fixture itself must differ");
        let observed: Vec<Uuid> = names
            .iter()
            .map(|name| {
                let client =
                    NetClient::connect_as("127.0.0.1".into(), 1, 776, None, name.clone());
                let deadline = crate::platform::Instant::now() + Duration::from_secs(5);
                let mut uuid = None;
                while crate::platform::Instant::now() < deadline && uuid.is_none() {
                    uuid = client.local_uuid();
                    std::thread::sleep(Duration::from_millis(5));
                }
                uuid.expect("local_uuid publishes before the dial resolves")
            })
            .collect();
        assert_ne!(
            observed[0], observed[1],
            "`connect_as` must thread the caller's name through to the login \
             identity, or every live gate silently shares one player file"
        );
        for (name, uuid) in names.iter().zip(&observed) {
            assert_eq!(
                *uuid,
                crate::offline_identity::offline_uuid(name),
                "the published uuid must be derived from the name we passed"
            );
        }
    }

    #[test]
    fn poll_is_empty_before_any_events() {
        // Connecting to a dead port yields an error update eventually, but poll
        // right away should simply be empty (non-blocking).
        let client = NetClient::connect("127.0.0.1".into(), 1, 776, None);
        let _ = client.poll();
    }

    /// Issue #189's other half of the social-roster seam: `local_uuid` must be
    /// published even when the connection itself never succeeds, because
    /// [`LoginProfile`] — and the `local_uuid.set(..)` right after it — is
    /// built *before* `run` ever attempts to dial (see `run`'s own comment at
    /// that call). A dead port (the same one
    /// [`poll_is_empty_before_any_events`] uses) proves this without needing
    /// a real server: if the publish depended on a successful handshake, this
    /// would time out.
    #[test]
    fn local_uuid_is_published_before_the_connection_even_resolves() {
        let client = NetClient::connect("127.0.0.1".into(), 1, 776, None);
        let deadline = crate::platform::Instant::now() + Duration::from_secs(5);
        let mut uuid = None;
        while crate::platform::Instant::now() < deadline && uuid.is_none() {
            uuid = client.local_uuid();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            uuid.is_some(),
            "local_uuid must publish regardless of whether the dial ever succeeds"
        );
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
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
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
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
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
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
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

    /// Issue #192: `ClientEvent::WinGame` must reach `NetUpdate::WinGame`
    /// through the real `forward` function — not a hand-constructed
    /// `NetUpdate` — the same shape as `forward_translates_...` above proves
    /// for `Death`. `route()` claims `shell: true` unconditionally for this
    /// variant, so a missing arm here would trip `forward`'s own
    /// `debug_assert!` on the catch-all in every debug test run; this test
    /// pins the actual translation rather than relying on that assert alone.
    #[test]
    fn forward_translates_win_game_into_the_credits_signal() {
        let (tx, rx) = mpsc::channel();
        forward(
            &tx,
            &WeatherCell::default(),
            &BiomeClimateCell::default(),
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
            ClientEvent::WinGame,
        )
        .expect("forward does not stop the loop");
        match rx.try_recv().expect("WinGame must cross the NetUpdate channel") {
            NetUpdate::WinGame => {}
            other => panic!("expected NetUpdate::WinGame, got {other:?}"),
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
        forward(&tx, &WeatherCell::default(), &BiomeClimateCell::default(), &BiomeNameCell::default(), &CommandTreeCell::default(), event).expect("forward does not stop the loop");
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
        forward(&tx, &WeatherCell::default(), &BiomeClimateCell::default(), &BiomeNameCell::default(), &CommandTreeCell::default(), event).expect("forward does not stop the loop");
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

    /// Player report: "the creeper has a hiss but no explosion sound."
    /// `decode_explode` (`crates/protocol/v770/src/adapter.rs`) already turns
    /// the `explode` packet into a `ClientEvent::Sound`, and that decode is
    /// proven twice over in `lodestone-v770`'s own tests — once against a
    /// hand-assembled fixture transcribed from `ClientboundExplodePacket`'s
    /// wire spec, once against a real vanilla 26.2 server's actual detonation
    /// (`live_creeper_explosion.rs`, `#[ignore]`d). Neither of those calls
    /// `forward`, which is the function `run()` actually calls in production
    /// and the one every other `forward_translates_*` test in this module
    /// pins — so nothing proved the shell's own namespace-stripping survives
    /// the hop into the exact `NetUpdate::Sound` value
    /// `sim/net_apply.rs`'s `NetUpdate::Sound` arm hands to
    /// `ShellAudio::play_sound`.
    ///
    /// This closes that gap without a live server: the packet bytes are fed
    /// to the *real*, registry-resolved `V770Adapter` (the same
    /// `lodestone_registry::adapter_for_protocol` call `run()` makes), and the
    /// decoded event is run through the real `forward()` — no hand-built
    /// `ClientEvent` anywhere in this test, unlike the other
    /// `forward_translates_*` gates.
    #[cfg(feature = "live")]
    #[test]
    fn a_real_explode_packet_forwards_the_correct_explosion_sound() {
        use lodestone_client::{ConnectionState, Directive};

        let adapter = lodestone_registry::adapter_for_protocol(776)
            .expect("the `live` feature compiles a family in for protocol 776");

        // `ClientboundExplodePacket`'s wire order: center (3×f64), radius
        // (f32), blockCount (i32), playerKnockback (Optional<Vec3>),
        // explosionParticle (ParticleOptions), explosionSound
        // (Holder<SoundEvent>). Packet id 36 is `minecraft:explode`'s
        // protocol-776 registration
        // (`lodestone_protocol_v770::generated::packet_ids::play::clientbound::EXPLODE`,
        // itself generated from Mojang's `packets.json`). explosionSound holder id
        // `700` (VarInt bytes `0xBC, 0x05`) references
        // `minecraft:sound_event` registry index `699`, verified against
        // `.cache/mc/26.2/generated/reports/registries.json`'s own
        // `protocol_id` for `minecraft:entity.generic.explode`. These are the
        // identical wire bytes `sound_particle_screen.rs`'s `explode_bytes()`
        // fixture uses at the adapter layer, transcribed from the spec, not
        // encoded with our own writer.
        let mut payload = Vec::new();
        payload.extend_from_slice(&8.0f64.to_be_bytes()); // center.x
        payload.extend_from_slice(&64.0f64.to_be_bytes()); // center.y
        payload.extend_from_slice(&8.0f64.to_be_bytes()); // center.z
        payload.extend_from_slice(&3.0f32.to_be_bytes()); // radius
        payload.extend_from_slice(&0i32.to_be_bytes()); // blockCount
        payload.push(0x00); // playerKnockback absent
        payload.push(29); // explosionParticle: explosion_emitter
        payload.push(0xBC); // explosionSound holder id 700, byte 1
        payload.push(0x05); // explosionSound holder id 700, byte 2

        let mut world = lodestone_world::World::new();
        let directives = adapter
            .handle_packet(&mut world, ConnectionState::Play, 36, &payload)
            .expect("a byte-accurate explode payload must decode");
        // Two directives since #416 (`bf18817`): the particle emitter first,
        // then the sound. This assertion used to read `len() == 1` and broke
        // when the particle arm landed — which is the point of asserting the
        // set rather than a count. A `count` alone cannot say *which*
        // directives are present, so it fails on a correct addition and would
        // equally pass if the sound were replaced by a second particle.
        assert_eq!(
            directives.len(),
            2,
            "an explode packet emits both a particle and a sound"
        );
        let mut sound_event = None;
        let mut saw_particles = false;
        for directive in directives {
            let Directive::Emit(event) = directive else {
                panic!("expected only Emit directives");
            };
            match event {
                // Guards the #416 chain: the renderer for `explosion_emitter`
                // is built and reaches pixels, but was fed by nothing until
                // `decode_explode` emitted this. If it regresses, an explosion
                // goes silent-visual again and only this line notices.
                ClientEvent::Particles { ref particle, .. } => {
                    // `particle` is an `Identifier`, so compare the path —
                    // `assert_eq!(particle, "explosion_emitter")` does not
                    // compile (`Identifier` has no `PartialEq<str>`).
                    assert_eq!(
                        particle.path(),
                        "explosion_emitter",
                        "the seed particle vanilla spawns for an explosion"
                    );
                    saw_particles = true;
                }
                other => sound_event = Some(other),
            }
        }
        assert!(saw_particles, "the explosion particle directive must be emitted");
        let event = sound_event.expect("the explosion sound directive must be emitted");

        let (tx, rx) = mpsc::channel();
        forward(
            &tx,
            &WeatherCell::default(),
            &BiomeClimateCell::default(),
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
            event,
        )
        .expect("forward does not stop the loop");

        match rx.try_recv().expect("a Sound NetUpdate must cross the channel") {
            NetUpdate::Sound {
                name,
                category,
                volume,
                pitch,
                ..
            } => {
                assert_eq!(
                    name, "entity.generic.explode",
                    "namespace must be stripped, matching the NetUpdate::Sound \
                     convention `sim/net_apply.rs`'s arm and `ShellAudio::play_sound` \
                     both assume"
                );
                assert_eq!(category, SoundCategory::Block, "SoundSource.BLOCKS");
                assert_eq!(
                    volume, 4.0,
                    "ClientPacketListener.handleExplosion's client-side constant \
                     (`.cache/mc/26.2/client-src/.../ClientPacketListener.java`)"
                );
                assert!(
                    (0.56..=0.84).contains(&pitch),
                    "pitch {pitch} outside vanilla's (1.0 +/- 0.2) * 0.7 band \
                     (ClientPacketListener.java)"
                );
            }
            other => panic!("expected NetUpdate::Sound, got {other:?}"),
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
        forward(&tx, &WeatherCell::default(), &BiomeClimateCell::default(), &BiomeNameCell::default(), &CommandTreeCell::default(), event).expect("forward does not stop the loop");
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
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
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
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
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
        // polarity at ClientPacketListener.java, not a bug on this side).
        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
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
            &BiomeNameCell::default(),
            &CommandTreeCell::default(),
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

    /// The biome-registry-names twin of the test above (follow-up to issue
    /// #96 / `eb423ac`): before this arm existed, the event reached the
    /// terminal `_ =>` and the `debug_assert!` there would have fired on
    /// every login once `v770` started emitting it (`route` claims
    /// `shell`/`must_forward` for it). Also pins the leak-intern:
    /// `BiomeNameCell::snapshot` must hand back the *same* strings by value
    /// (not merely equal ones), proving the cache is read, not re-leaked, on
    /// every access.
    #[test]
    fn forward_folds_biome_registry_names_into_the_cell_without_using_the_channel() {
        let (tx, rx) = mpsc::channel();
        let weather = WeatherCell::default();
        let names = BiomeNameCell::default();
        assert_eq!(names.snapshot(), Vec::<&str>::new(), "empty before Login");

        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            &names,
            &CommandTreeCell::default(),
            ClientEvent::BiomeRegistryNames {
                names: vec!["minecraft:swamp".to_string(), "minecraft:desert".to_string()],
            },
        )
        .expect("forward does not stop the loop");

        assert_eq!(names.snapshot(), vec!["minecraft:swamp", "minecraft:desert"]);
        assert!(
            rx.try_recv().is_err(),
            "biome registry names must not cross the NetUpdate channel — the whole \
             table replaces at once, exactly like weather/biome climates"
        );
    }

    /// Both command arms fold into [`CommandTreeCell`] and neither crosses the
    /// channel (issue #471).
    ///
    /// The arms are load-bearing before any screen reads the cell:
    /// `lodestone_model::event::route` claims `shell`/`must_forward` for both
    /// variants, so without them they reached `forward`'s terminal `_ =>` and
    /// tripped its `debug_assert!` on any debug-build join to a real 26.2
    /// server. Asserting the *fold* rather than only the absence of a channel
    /// message is what stops this being the island shape: an arm that consumed
    /// the event and dropped it would satisfy `rx.try_recv().is_err()` too.
    #[test]
    fn forward_folds_the_command_tree_and_suggestions_into_the_cell() {
        use lodestone_model::command_tree::{
            CommandSuggestionEntry, CommandTree, NodeKind, RawCommandNode,
        };

        let (tx, rx) = mpsc::channel();
        let weather = WeatherCell::default();
        let cell = CommandTreeCell::default();
        assert!(cell.tree().is_none(), "empty before the server sends one");
        assert!(cell.suggestions().is_none(), "and no reply yet");

        let tree = CommandTree::new(
            vec![RawCommandNode {
                kind: NodeKind::Root,
                executable: false,
                restricted: false,
                redirect: None,
                children: Vec::new(),
            }],
            0,
        )
        .expect("a root-only tree is valid");

        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            &BiomeNameCell::default(),
            &cell,
            ClientEvent::CommandTreeUpdated {
                tree: Box::new(tree.clone()),
            },
        )
        .expect("forward does not stop the loop");
        assert_eq!(
            cell.tree().as_deref(),
            Some(&tree),
            "the decoded tree must land in the cell, not be dropped by the arm"
        );

        forward(
            &tx,
            &weather,
            &BiomeClimateCell::default(),
            &BiomeNameCell::default(),
            &cell,
            ClientEvent::CommandSuggestionsReceived {
                id: 7,
                start: 10,
                length: 0,
                suggestions: vec![CommandSuggestionEntry {
                    text: "creative".to_string(),
                    tooltip: None,
                }],
            },
        )
        .expect("forward does not stop the loop");

        let got = cell.suggestions().expect("the reply must land in the cell");
        assert_eq!(
            got.id, 7,
            "the transaction id must survive the fold — it is the only thing that lets a \
             consumer discard a reply to a request the input has since outgrown"
        );
        assert_eq!(got.start, 10);
        assert_eq!(
            got.suggestions
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["creative"]
        );

        assert!(
            rx.try_recv().is_err(),
            "neither command event may cross the NetUpdate channel — the tree replaces \
             wholesale and the menu layer polls the cell per frame"
        );
    }

    /// `BiomeClimateCell` carrying real vanilla biome data (frozen_peaks and
    /// desert, `temperature`/`has_precipitation`/`downfall` copied verbatim
    /// from `.cache/mc/26.2/src/data/minecraft/worldgen/biome/{frozen_peaks,
    /// desert}.json`) must, once vanilla's own `getPrecipitationAt` predicate
    /// is applied, land on the correct side of the rain/snow line:
    /// `Biome.java`, `return this.getTemperature(pos, seaLevel) >= 0.15F;`
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
        const WARM_ENOUGH_TO_RAIN: f32 = 0.15; // Biome.java
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
             threshold is even consulted (Biome.java)"
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
        forward(&tx, &weather, &BiomeClimateCell::default(), &BiomeNameCell::default(), &CommandTreeCell::default(), spawn("minecraft:zombie")).expect("forward continues");
        assert_eq!(
            weather.snapshot().lightning_seq,
            0,
            "a zombie must not flash the sky"
        );

        forward(&tx, &weather, &BiomeClimateCell::default(), &BiomeNameCell::default(), &CommandTreeCell::default(), spawn("minecraft:lightning_bolt")).expect("forward continues");
        assert_eq!(weather.snapshot().lightning_seq, 1);
        forward(&tx, &weather, &BiomeClimateCell::default(), &BiomeNameCell::default(), &CommandTreeCell::default(), spawn("minecraft:lightning_bolt")).expect("forward continues");
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

    // `bare_entity_view`, the "entity_snapshot_carries_*" tests, and the
    // `name_tag` submodule are deleted with issue #36: they pinned the
    // (now-deleted) `EntityView` -> `EntitySnapshot` boundary. That property
    // — a decoded value survives from ingest to the render component set —
    // moved to `entities.rs`'s own test module, re-aimed at the
    // ingest-components -> `EntityFacts` boundary `resolve_entity_facts` now
    // owns: see `resolve_entity_facts_carries_velocity_and_on_ground_through`,
    // `..._equipment_through`, `..._equipment_dye_through`,
    // `..._variant_through`, `..._creeper_swell_dir_through`,
    // `..._item_count_through`, and the `name_tag` submodule there.

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
