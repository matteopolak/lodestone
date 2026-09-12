//! Shared, latest-value state published by the network session.
//!
//! These cells are deliberately separate from the bounded event relay: they
//! represent state where consumers need the newest value and never need to
//! replay every intermediate update.

use std::sync::{Arc, Mutex, PoisonError};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

use lodestone_render::SkyDefault;
use uuid::Uuid;

/// The latest weather levels and lightning sequence published by a session.
///
/// # Why this is not a [`NetUpdate`]
///
/// `GAME_EVENT`'s rain and thunder levels arrive **every tick** while the server
/// ramps them (vanilla's own server-side level broadcasts on any change, and the
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
    /// both exactly as vanilla's own set-rain-level does.
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
    pub(super) fn apply(&self, raining: Option<bool>, rain_level: Option<f32>, thunder_level: Option<f32>) {
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
    pub(super) fn strike(&self) {
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
/// `has_precipitation` pair `ShellWeatherProbe::precipitation`
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
    /// `pub(crate)`, not private: the app.rs live gate for
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
/// `command_suggestion` request.
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
/// **The fold, and not yet a screen.** This closes the live forwarding gap that
/// is a live defect — `lodestone_model::event::route` sends both variants to
/// `SHELL`, so with no arm in [`forward`] they reached the terminal `_ =>` and
/// tripped its `debug_assert!` on any debug-build join to a real server. The
/// remaining screen integration steps — pointing `menu/render/screens.rs`'s
/// `command_block_frame` at [`Self::tree`] instead of the `None` every caller
/// passes, and making the chat box's Tab key call `chat::complete` — live in
/// `chat.rs` and the menu files, outside this module.
///
/// **This is deliberately a half-wire:** storing the value where the consumer
/// can reach it makes the next integration step an arm rather than a re-decode;
/// dropping it in the arm would leave the unreachable route the assertion
/// above exists to catch.
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

/// One server-pushed resource pack awaiting the player's accept/decline
/// answer — `ClientboundResourcePackPushPacket`'s fields, plus the message
/// this dialog draws.
///
/// `message` is vanilla's own pack-prompt header text, with the server's own optional prompt
/// component appended — pre-flattened to plain text and folded onto one
/// line, the same "one clipped line, not a wrapped `MultiLineTextWidget`"
/// simplification [`crate::menu::confirm`]'s module doc already makes and
/// names for the identical reason: there is no font at menu-frame-build time
/// to wrap against.
#[derive(Debug, Clone)]
pub struct PendingResourcePackPrompt {
    /// Pack id, echoed back in the response.
    pub id: Uuid,
    /// Download URL, already known to parse as `http`/`https` — see
    /// [`parse_resource_pack_url`].
    pub(super) url: String,
    /// SHA-1 hash the server supplied (hex; may be empty).
    pub(super) hash: String,
    /// Whether declining disconnects — vanilla will not silently drop a
    /// player over a pack they never answered, so this also decides the
    /// button labels (`Proceed`/`Disconnect` vs `Yes`/`No`).
    pub required: bool,
    /// `multiplayer.{requiredT,t}exturePrompt.line1` — the title line.
    pub title: String,
    /// The body line described above.
    pub message: String,
}

#[cfg(test)]
impl PendingResourcePackPrompt {
    /// A minimal prompt for a menu-side test that only needs *some* live
    /// prompt to exist (routing/hit-test gates) rather than one shaped by a
    /// real push — `url`/`hash` are private to this module, so a sibling
    /// module's test cannot build the struct literal directly.
    pub fn for_test(id: Uuid, required: bool) -> Self {
        Self {
            id,
            url: "https://example.invalid/pack.zip".to_string(),
            hash: String::new(),
            required,
            title: "t".to_string(),
            message: "m".to_string(),
        }
    }
}

/// The pending-prompt cell: written by the net thread when it decides a push
/// needs the player's own answer, read every frame by
/// `app/session.rs`'s `drive_ui_from_session` to reconcile
/// `Screen::ResourcePackPrompt`, and cleared by [`apply_pack_response`] once
/// the net thread's own loop actually *drains* an answer — **not** the
/// moment [`NetClient::respond_to_resource_pack`] queues one; that call only
/// sends on a channel this cell knows nothing about, and the drain can lag
/// it by up to 15 ms. `MenuNav::resource_pack_answered_id` exists precisely
/// because a caller once read this doc as "synchronous" and reconciled
/// against this cell as if it already reflected the answer.
#[derive(Debug, Default)]
pub struct PackPromptCell(Mutex<Option<PendingResourcePackPrompt>>);

impl PackPromptCell {
    pub(super) fn set(&self, prompt: PendingResourcePackPrompt) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(prompt);
    }

    /// Clears the cell only if it currently holds `id` — an answer to a
    /// superseded prompt must not erase a *newer* one it raced with.
    pub(super) fn clear_if(&self, id: Uuid) -> Option<PendingResourcePackPrompt> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.as_ref().is_some_and(|p| p.id == id) {
            guard.take()
        } else {
            None
        }
    }

    /// Unconditionally clears the cell — `ClientboundResourcePackPopPacket`
    /// with no id, vanilla's "remove every pack".
    pub(super) fn clear_all(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// This frame's pending prompt, if any.
    #[must_use]
    pub fn get(&self) -> Option<PendingResourcePackPrompt> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// A [`PackPromptCell`] shared between the net thread and the render/menu
/// thread.
pub type SharedPackPrompt = Arc<PackPromptCell>;

/// A decoded, version-free update the app can act on without touching tokio.
