//! The user-facing handle and event stream.

use std::time::Duration;

use lodestone_model::{
    BlockPos, ChunkPos, ClientAction, ClientEvent, EntityAttributeSnapshot, GameMode, Hand,
    PlayerListEntry, Rotation, Vec3,
};
use tokio::sync::{mpsc, oneshot};

use crate::error::{BotError, ClientClosed, SessionOutcome, WaitError};
use crate::spawn::DriverTask;
use crate::state::{EntityView, OpenMenuSnapshot, PlayerSnapshot, SharedState};
use lodestone_game::bossbar::BossBarSet;
use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::menu::Menu;
use lodestone_game::scoreboard::Scoreboard;
use lodestone_game::tablist::TabList;
use lodestone_world::{ChunkSection, SectionLight};

/// A handle to a running client session.
///
/// This is the programmable surface a bot author uses. Beyond submitting raw
/// [`ClientAction`]s, it exposes a maintained **read-model** (query where you
/// are, your health, nearby entities, loaded blocks), ergonomic **actions**
/// (chat, look, move), and **awaiting** primitives (`wait_for_*`) so bots can be
/// written as `await` sequences.
///
/// All queries read a cheap shared snapshot and never block the driver; all
/// waits are woken by driver state changes and time out rather than hang. The
/// game shell is just another consumer of this same surface: a human player is a
/// bot driven by a keyboard.
///
/// Use it to submit actions, request a clean shutdown, and await the final
/// [`SessionOutcome`]. Dropping the handle does *not* end the session (a
/// fire-and-forget bot can keep receiving events); call [`ClientHandle::shutdown`]
/// to stop it deliberately.
#[derive(Debug)]
pub struct ClientHandle {
    actions: mpsc::UnboundedSender<ClientAction>,
    shutdown: Option<oneshot::Sender<()>>,
    task: DriverTask,
    state: SharedState,
}

impl ClientHandle {
    pub(crate) fn new(
        actions: mpsc::UnboundedSender<ClientAction>,
        shutdown: oneshot::Sender<()>,
        task: DriverTask,
        state: SharedState,
    ) -> Self {
        Self {
            actions,
            shutdown: Some(shutdown),
            task,
            state,
        }
    }

    /// Submits an action to be encoded against the driver's *live* connection
    /// state and written to the server.
    ///
    /// This never blocks. Actions the adapter cannot represent in the current
    /// state are dropped quietly by the driver rather than surfaced here.
    ///
    /// # Errors
    ///
    /// Returns [`ClientClosed`] if the session has already ended.
    pub fn send_action(&self, action: ClientAction) -> Result<(), ClientClosed> {
        self.actions.send(action).map_err(|_| ClientClosed)
    }

    /// Requests a clean local shutdown.
    ///
    /// The driver attempts to send a protocol disconnect (if the adapter
    /// encodes one) and then stops, yielding [`SessionOutcome::LocalClose`].
    /// Idempotent: later calls are no-ops.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }

    /// Returns `true` once the driver task has finished.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Waits for the session to end and returns why.
    ///
    /// Consumes the handle, since no further actions can be submitted once the
    /// session has ended.
    pub async fn join(self) -> SessionOutcome {
        self.task.join().await
    }

    // --- Read-model queries -------------------------------------------------
    //
    // Each of these clones a small snapshot out from behind a short-lived read
    // lock; none of them block the driver.

    /// Returns a snapshot of the local player's state.
    #[must_use]
    pub fn player(&self) -> PlayerSnapshot {
        self.state.player()
    }

    /// Returns the player's current position, or `None` if the server has not
    /// placed the player yet.
    ///
    /// Reads the local echo directly rather than building a whole
    /// [`PlayerSnapshot`], so it takes no ECS lock — this is the read a moving bot
    /// makes most often and there is nothing in the component set it needs.
    #[must_use]
    pub fn position(&self) -> Option<Vec3> {
        self.state.position()
    }

    /// Returns the player's current look direction. Echo-only, like
    /// [`Self::position`].
    #[must_use]
    pub fn rotation(&self) -> Rotation {
        self.state.rotation()
    }

    /// Returns the player's current health in half-hearts, or `None` if the
    /// server has not reported it yet.
    #[must_use]
    pub fn health(&self) -> Option<f32> {
        let player = self.state.player();
        player.health_known.then_some(player.health)
    }

    /// Returns the player's current food level, or `None` if unknown yet.
    #[must_use]
    pub fn food(&self) -> Option<i32> {
        let player = self.state.player();
        player.health_known.then_some(player.food)
    }

    /// Returns whether the player is currently alive.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.state.player().alive
    }

    /// Returns the player's progress toward the next level (`0.0..1.0`), or
    /// `None` if the server has not reported it yet.
    #[must_use]
    pub fn experience_progress(&self) -> Option<f32> {
        let player = self.state.player();
        player.xp_known.then_some(player.xp_progress)
    }

    /// Returns the player's current experience level, or `None` if unknown yet.
    #[must_use]
    pub fn experience_level(&self) -> Option<i32> {
        let player = self.state.player();
        player.xp_known.then_some(player.xp_level)
    }

    /// Returns the player's total accumulated experience points, or `None` if
    /// unknown yet.
    #[must_use]
    pub fn total_experience(&self) -> Option<i32> {
        let player = self.state.player();
        player.xp_known.then_some(player.xp_total)
    }

    /// Returns the player's current game mode, or `None` if unknown yet.
    #[must_use]
    pub fn game_mode(&self) -> Option<GameMode> {
        self.state.player().game_mode
    }

    /// Returns a view of a tracked entity by id, if present.
    ///
    /// Never returns the **local player**, even when handed our own
    /// [`PlayerSnapshot::entity_id`]: we carry no
    /// `EntityKind`/`Position`/`Rotation`/`HeadYaw` (they would duplicate the
    /// driver's own physics state), and an [`EntityView`] cannot be built without
    /// them. Use [`Self::local_player_attributes`] for the one piece of
    /// entity-shaped state the local player does fold.
    #[must_use]
    pub fn entity(&self, entity_id: i32) -> Option<EntityView> {
        self.state.entity(entity_id)
    }

    /// Returns views of all currently tracked entities, **excluding the local
    /// player**.
    #[must_use]
    pub fn entities(&self) -> Vec<EntityView> {
        self.state.entities()
    }

    /// Returns the local player's own attributes, as `update_attributes` last
    /// reported them. Empty before login.
    ///
    /// # Why this needed a fix rather than just an accessor
    ///
    /// `lodestone_ecs::ingest::EntityIndex` used to be populated only by
    /// `ClientEvent::EntitySpawned`, and **vanilla never sends an `AddEntity` for
    /// yourself — only `Login`**. So `apply_entity_attributes` silently dropped
    /// every snapshot naming our own id and this would have returned an empty list
    /// forever, however correct the accessor was. See
    /// `lodestone_ecs::ingest::apply_local_player_login`.
    ///
    /// Fold a value out of these with
    /// `lodestone_entity::attribute::attribute_value`, which applies vanilla's
    /// three-stage `AttributeInstance::calculateValue` order. Do not read `base`
    /// and ignore the modifiers.
    #[must_use]
    pub fn local_player_attributes(&self) -> Vec<EntityAttributeSnapshot> {
        self.state.local_attributes()
    }

    /// Returns the currently known player-list entries, flattened to the
    /// model's wire shape.
    ///
    /// A *derived* view of the folded [`TabList`] — see
    /// [`ClientHandle::tab_list`] for the richer one, which is what you want if
    /// you need vanilla display order or the header/footer.
    #[must_use]
    pub fn players(&self) -> Vec<PlayerListEntry> {
        self.state.players()
    }

    /// Returns a snapshot of the folded tab list.
    ///
    /// The one fold: Stage 3 of `docs/bevy-migration.md` deleted this crate's
    /// own `HashMap<Uuid, PlayerListEntry>` and the shell's second `TabList`,
    /// leaving `lodestone_game::tablist::TabList` behind one
    /// `SessionTabList` component.
    #[must_use]
    pub fn tab_list(&self) -> TabList {
        self.state.tab_list()
    }

    /// Returns a snapshot of the folded scoreboard — objectives, per-objective
    /// scores, the nineteen display-slot assignments and teams.
    ///
    /// Query the returned [`Scoreboard`] with its accessors (`objective`,
    /// `sorted_scores`, `displayed`, `sidebar_for_color`, `team_of`, ...). The
    /// snapshot is a point-in-time clone; call again to observe later updates.
    ///
    /// **This type changed in Stage 3.** It used to be a second, poorer
    /// `Scoreboard` defined in this crate: three display slots instead of
    /// nineteen, no team decoration, and a `ScoreUpdate` that invented an
    /// objective-less bucket where `lodestone-game` drops it. That type is
    /// deleted; this is the aggregate the HUD has always rendered from.
    #[must_use]
    pub fn scoreboard(&self) -> Scoreboard {
        self.state.scoreboard()
    }

    /// Returns the currently active boss bars, in server insertion (render)
    /// order (`BossBarSet::iter`).
    ///
    /// Also changed in Stage 3, and for a sharper reason: this crate had its own
    /// `Vec<BossBar>` fold while `lodestone_game::bossbar::BossBarSet` was a
    /// complete, unit-tested implementation of the same thing that **nothing
    /// called**. The island is now the live one.
    #[must_use]
    pub fn boss_bars(&self) -> BossBarSet {
        self.state.boss_bars()
    }

    /// Returns the folded player inventory menu (window 0), in vanilla menu-slot
    /// order. The returned snapshot is owned and safe to keep for rendering.
    #[must_use]
    pub fn player_menu(&self) -> Menu {
        self.state.player_menu()
    }

    /// Returns the folded non-player menu currently open, if any.
    #[must_use]
    pub fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        self.state.open_menu()
    }

    /// Predicts a container click locally and submits the resulting action to
    /// the server.
    ///
    /// The prediction happens inside the read-model
    /// ([`SharedState::menu_click`]), not on a snapshot: [`player_menu`](Self::player_menu)
    /// and [`open_menu`](Self::open_menu) each hand back a clone, and a clone
    /// has nowhere for the click's mutation to land. This call predicts
    /// against the one live `Menus` the state owns, so the screen updates
    /// immediately and [`ClientEvent`]s the server sends back
    /// (`container_set_slot`, `set_cursor_item`, ...) reconcile over the same
    /// prediction rather than a stale copy.
    ///
    /// # Errors
    ///
    /// Returns [`ClientClosed`] if the session has already ended.
    pub fn menu_click(&self, click: Click, ctx: PlayerCtx) -> Result<(), ClientClosed> {
        self.send_action(self.state.menu_click(click, ctx))
    }

    /// Returns the block-state id at `pos`, or `None` if that block's chunk is
    /// not currently loaded.
    ///
    /// The value is the version-free block-state id; mapping it to a block name
    /// is a registry concern, deliberately outside this crate.
    #[must_use]
    pub fn block_at(&self, pos: BlockPos) -> Option<u32> {
        self.state.block_at(pos)
    }

    /// The client-owned chunk store, as the ECS `Resource` handle
    /// (`docs/bevy-migration.md` §4.1(d), `docs/chunk-world-resource.md`).
    ///
    /// A **handle onto the one store**, not a snapshot: clones share the `World`
    /// the net thread writes decoded columns into, and the same handle is already
    /// installed as a resource in this client's own ECS `World`. A driver adopts
    /// this instead of keeping a world of its own — which is what makes "there is
    /// one chunk store in the process" a checkable property (`ChunkWorld::is_same_store`)
    /// rather than an aspiration.
    ///
    /// Prefer the narrow accessors ([`block_at`](Self::block_at),
    /// [`section_at`](Self::section_at), [`sections_and_light_at`](Self::sections_and_light_at))
    /// for one-off reads: they take the lock for exactly as long as the read.
    /// Reach for this when you need the store itself — to install it as a
    /// resource, or to build a `'static` closure over it.
    #[must_use]
    pub fn chunk_world(&self) -> lodestone_ecs::ChunkWorld {
        self.state.chunk_world()
    }

    /// Returns whether the chunk at `pos` is currently loaded.
    #[must_use]
    pub fn is_chunk_loaded(&self, pos: ChunkPos) -> bool {
        self.state.is_chunk_loaded(pos)
    }

    /// Returns the number of currently loaded chunk columns.
    #[must_use]
    pub fn loaded_chunk_count(&self) -> usize {
        self.state.loaded_chunk_count()
    }

    /// Returns the positions of all currently loaded chunk columns.
    #[must_use]
    pub fn loaded_chunks(&self) -> Vec<ChunkPos> {
        self.state.loaded_chunks()
    }

    /// Returns an owned snapshot of the chunk section at `section_index` within
    /// the column at `pos`, or `None` if that chunk is not loaded or the section
    /// is elided (all air).
    ///
    /// The returned [`Arc`](std::sync::Arc) carries no borrow into the client's
    /// world and pins no lock: hold it for the whole duration of a mesh while
    /// chunk streaming continues. A later edit of that section forks it
    /// copy-on-write, so the snapshot you hold stays valid and unchanged. This is
    /// the seam a mesher reads through; single-block reads should use
    /// [`block_at`](ClientHandle::block_at).
    ///
    /// This hands out block-state sections only. Lit meshing also needs the
    /// column's light, served in parallel by [`section_light`](ClientHandle::section_light)
    /// and [`lights_at`](ClientHandle::lights_at).
    #[must_use]
    pub fn section_at(
        &self,
        pos: ChunkPos,
        section_index: usize,
    ) -> Option<std::sync::Arc<ChunkSection>> {
        self.state.section_at(pos, section_index)
    }

    /// Returns one owned section snapshot per requested `(chunk, section_index)`,
    /// in order, acquiring the internal world lock exactly once.
    ///
    /// This is the bulk-read primitive for meshing: pull a whole 27-section
    /// neighbourhood in a single lock acquisition, then work off the returned
    /// [`Arc`](std::sync::Arc)s with no lock held. A request that is not loaded
    /// (or an all-air section) yields a `None` slot rather than being omitted, so
    /// the result stays aligned with the input.
    #[must_use]
    pub fn sections_at(
        &self,
        requests: &[(ChunkPos, usize)],
    ) -> Vec<Option<std::sync::Arc<ChunkSection>>> {
        self.state.sections_at(requests)
    }

    /// Returns an owned [`SectionLight`] snapshot of light section
    /// `light_section_index` within the column at `pos`, or `None` if that chunk is
    /// not loaded or the light section is out of range.
    ///
    /// This is the light-side companion to [`section_at`](ClientHandle::section_at),
    /// so a mesher can read block state through one and light through the other. Two
    /// things differ from `section_at` and both are deliberate:
    ///
    /// - **Indexing is light-section, not block-section.** Light section `0` is the
    ///   boundary below the world and light section `i` covers block-section
    ///   `i - 1`. That offset is what lets a mesher reach the boundary light
    ///   sections above and below the build range — positions block-section index
    ///   has no name for. Add one to a block-section index to get its light section.
    /// - **An all-air section still has light.** Where `section_at` returns `None`
    ///   for an elided (all-air) section, `section_light` returns `Some`: air
    ///   carries light, and a face meshed against it must sample that light or
    ///   render black.
    ///
    /// Like the section snapshot, the returned value carries no borrow into the
    /// world and pins no lock: hold it across a whole mesh while streaming
    /// continues. It is a cheap value (each light layer is `Arc`-backed
    /// copy-on-write), so a later relight forks it and leaves your snapshot intact.
    #[must_use]
    pub fn section_light(&self, pos: ChunkPos, light_section_index: usize) -> Option<SectionLight> {
        self.state.section_light(pos, light_section_index)
    }

    /// Returns one owned light snapshot per requested `(chunk, light_section_index)`,
    /// in order, acquiring the internal world lock exactly once — the light-side
    /// twin of [`sections_at`](ClientHandle::sections_at) for pulling a whole
    /// meshing neighbourhood under a single lock.
    ///
    /// A request whose chunk is not loaded (or whose light section is out of range)
    /// yields a `None` slot rather than being omitted, so the result stays aligned
    /// with the input. Note that, unlike `sections_at`, an all-air section is
    /// `Some` here (see [`section_light`](ClientHandle::section_light)).
    #[must_use]
    pub fn lights_at(&self, requests: &[(ChunkPos, usize)]) -> Vec<Option<SectionLight>> {
        self.state.lights_at(requests)
    }

    /// Returns a `(block section, light section)` snapshot pair for each requested
    /// `(chunk, block_section_index, light_section_index)`, in order, acquiring the
    /// internal world lock exactly once for the whole batch.
    ///
    /// This is the atomic companion to [`sections_at`](ClientHandle::sections_at)
    /// and [`lights_at`](ClientHandle::lights_at): calling those two separately
    /// pulls blocks and light under *different* lock epochs, so a `BLOCK_UPDATE` or
    /// `LIGHT_UPDATE` landing between them could hand a mesher geometry from one
    /// tick and light from another. This call reads both halves under one lock, so
    /// a whole meshing neighbourhood is internally consistent.
    ///
    /// The two indices are **distinct spaces and are passed through unchanged** —
    /// there is deliberately no silent `+1`. The first is a block-section index
    /// (selecting the `Arc<ChunkSection>`, `None` when the section is unloaded or
    /// all-air/elided); the second is a light-section index (selecting the
    /// [`SectionLight`], where `0` is the boundary below the world and light
    /// section `i` covers block section `i - 1`, and where an all-air section still
    /// yields `Some`). A mesher meshing block section `n` typically asks for
    /// `(pos, n, n + 1)`. Each half carries no borrow into the world and pins no
    /// lock, exactly like the singular reads.
    #[must_use]
    pub fn sections_and_light_at(
        &self,
        requests: &[(ChunkPos, usize, usize)],
    ) -> Vec<(Option<std::sync::Arc<ChunkSection>>, Option<SectionLight>)> {
        self.state.sections_and_light_at(requests)
    }

    /// Returns the connected dimension's vertical extent, or `None` before the
    /// dimension's terrain is known (pre-login / pre-first-chunk).
    ///
    /// A live mesher needs this to place a streamed column's block and light
    /// sections at the correct world-`y`:
    /// [`section_count`](WorldDimensions::section_count) is `height / 16`, and
    /// light sections span `0..=section_count + 1` (`0` is the below-world
    /// boundary and light section `i` covers block section `i - 1`), matching
    /// [`sections_and_light_at`](ClientHandle::sections_and_light_at). A
    /// `DimensionId` alone cannot supply this — it is only a resource key and
    /// carries no geometry, and probing the light accessor reveals a section
    /// *count* but never the `min_y` anchor.
    #[must_use]
    pub fn world_dimensions(&self) -> Option<WorldDimensions> {
        self.state
            .world_extent()
            .map(|(min_y, height)| WorldDimensions { min_y, height })
    }

    /// Returns `(world_age, time_of_day)` as last reported by the server.
    ///
    /// Backed by the `bevy_ecs` [`WorldTime`](lodestone_ecs::WorldTime)
    /// resource since Stage 0 of `docs/bevy-migration.md` (previously two
    /// scalar fields on the read-model's private `Inner`, now deleted rather
    /// than mirrored — the migration's authority test). The shape stays
    /// `(i64, i64)`, not `WorldTime`, so this signature is unaffected by the
    /// cutover.
    #[must_use]
    pub fn world_time(&self) -> (i64, i64) {
        self.state.time()
    }

    // --- Ergonomic actions --------------------------------------------------

    /// Sends a chat message.
    ///
    /// # Errors
    ///
    /// Returns [`ClientClosed`] if the session has already ended.
    pub fn chat(&self, text: impl Into<String>) -> Result<(), ClientClosed> {
        self.send_action(ClientAction::SendChat { text: text.into() })
    }

    /// Sends a command (the leading slash is added by the adapter as the
    /// protocol requires; pass the command without it).
    ///
    /// # Errors
    ///
    /// Returns [`ClientClosed`] if the session has already ended.
    pub fn command(&self, command: impl Into<String>) -> Result<(), ClientClosed> {
        self.send_action(ClientAction::SendCommand {
            command: command.into(),
        })
    }

    /// Swings the given arm (a visible, server-observable animation).
    ///
    /// # Errors
    ///
    /// Returns [`ClientClosed`] if the session has already ended.
    pub fn swing(&self, hand: Hand) -> Result<(), ClientClosed> {
        self.send_action(ClientAction::SwingArm { hand })
    }

    /// Sends the player's complete movement state for a single tick —
    /// absolute position, look rotation, ground contact, and horizontal
    /// collision — as one [`ClientAction::Move`].
    ///
    /// This is the **lowest-level movement primitive**, and the one a tick-driven
    /// controller wants: a real client (or `lodestone-shell`) integrates position
    /// from held keys and gravity every tick and emits exactly one move per tick.
    /// Compute the next state with your own physics and call this once per tick.
    /// [`set_position`](Self::set_position), [`look_at`](Self::look_at),
    /// [`step_toward`](Self::step_toward) and [`walk_to`](Self::walk_to) are all
    /// thin conveniences layered over it for goal-seeking bot usage.
    ///
    /// The client performs **no physics of its own** — gravity, collision and
    /// input integration are the caller's. `on_ground` and
    /// `horizontal_collision` are simulation outputs, not something a version
    /// adapter derives; pass through whatever your physics step produced. A
    /// version adapter that sends movement at vanilla's own cadence uses the
    /// deltas between successive calls (and these two flags) to pick which
    /// concrete wire packet to emit — that choice never changes what you pass
    /// here. The read-model updates *optimistically*: the predicted position is
    /// written locally before the server confirms it, and the server only
    /// overrides it via a corrective teleport. So a position read back after
    /// this call is a local prediction, not a server-confirmed location.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::Closed`] if the session has ended.
    pub fn move_to(
        &self,
        pos: Vec3,
        rotation: Rotation,
        on_ground: bool,
        horizontal_collision: bool,
    ) -> Result<(), BotError> {
        self.send_action(ClientAction::Move {
            pos,
            rotation,
            on_ground,
            horizontal_collision,
        })?;
        Ok(())
    }

    /// Moves the player to an absolute position, keeping the current look
    /// direction. A goal-seeking convenience over [`move_to`](Self::move_to); the
    /// read-model updates optimistically (a local prediction, not a server
    /// confirmation).
    ///
    /// This helper runs no physics of its own, so it always reports
    /// `horizontal_collision: false` — a hand-scripted goal-seeker never
    /// detects a collision it didn't simulate. Callers that need faithful
    /// collision reporting should drive [`move_to`](Self::move_to) directly
    /// from their own physics step.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::Closed`] if the session has ended.
    pub fn set_position(&self, pos: Vec3) -> Result<(), BotError> {
        let player = self.state.player();
        self.move_to(pos, player.rotation, player.on_ground, false)
    }

    /// Turns the player to face `target`, keeping the current position. A
    /// convenience over [`move_to`](Self::move_to).
    ///
    /// This helper runs no physics of its own, so it always reports
    /// `horizontal_collision: false` (see [`set_position`](Self::set_position)).
    ///
    /// # Errors
    ///
    /// Returns [`BotError::PositionUnknown`] if the server has not placed the
    /// player yet, or [`BotError::Closed`] if the session has ended.
    pub fn look_at(&self, target: Vec3) -> Result<(), BotError> {
        let player = self.state.player();
        let pos = player.position.ok_or(BotError::PositionUnknown)?;
        let rotation = look_at_rotation(eye_of(pos), target);
        self.move_to(pos, rotation, player.on_ground, false)
    }

    /// Takes a single step of at most `max_distance` blocks toward `target`,
    /// facing it. A goal-seeking convenience over [`move_to`](Self::move_to) that
    /// a caller can drive from its own per-tick loop; [`walk_to`](Self::walk_to)
    /// loops it until arrival.
    ///
    /// This helper runs no physics of its own, so it always reports
    /// `horizontal_collision: false` (see [`set_position`](Self::set_position)).
    ///
    /// # Errors
    ///
    /// Returns [`BotError::PositionUnknown`] if the server has not placed the
    /// player yet, or [`BotError::Closed`] if the session has ended.
    pub fn step_toward(&self, target: Vec3, max_distance: f64) -> Result<(), BotError> {
        let player = self.state.player();
        let pos = player.position.ok_or(BotError::PositionUnknown)?;
        let delta = target - pos;
        let distance = delta.length();
        let next = if distance <= max_distance || distance == 0.0 {
            target
        } else {
            pos + delta.normalize().scale(max_distance)
        };
        let rotation = look_at_rotation(eye_of(pos), target);
        self.move_to(next, rotation, player.on_ground, false)
    }

    /// Walks toward `target`, stepping every tick until the local prediction is
    /// within `tolerance` blocks (horizontally) or the `timeout` elapses.
    ///
    /// A goal-seeking convenience loop over [`step_toward`](Self::step_toward)
    /// (itself over [`move_to`](Self::move_to)); it drives simple straight-line
    /// movement and does not path around obstacles. A tick-driven controller
    /// should drive [`move_to`](Self::move_to) directly instead — one function
    /// cannot serve both a per-tick input loop and a goal-with-arrival call.
    /// Native-only, because it relies on a runtime timer that does not exist on
    /// `wasm32`; on the browser, loop [`step_toward`](Self::step_toward) yourself.
    ///
    /// # What the outcome means
    ///
    /// This tracks the driver's optimistic **local prediction**, so
    /// [`WalkOutcome::Arrived`] means the prediction converged on the target —
    /// **not** that the server acknowledged the displacement (server-confirmed
    /// movement is a stronger, separate gate: observe your own entity from a
    /// second connection). It is nonetheless a real signal: it is *not* reached
    /// if the timeout is too short for the distance, if the session ends, or if
    /// the server rubber-bands the player back with corrective teleports faster
    /// than the walk can advance. On timeout it returns
    /// [`WalkOutcome::TimedOut`] carrying the distance still remaining, so a
    /// caller can retry or treat it as blocked — the previous "always `Ok`"
    /// return could express none of this.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::PositionUnknown`] if the server has not placed the
    /// player yet, or [`BotError::Closed`] if the session ends before arriving.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn walk_to(
        &self,
        target: Vec3,
        tolerance: f64,
        timeout: Duration,
    ) -> Result<WalkOutcome, BotError> {
        /// One server tick. Movement is sent at roughly this cadence, as a real
        /// client does; blasting positions instantly is trivially detectable.
        const TICK: Duration = Duration::from_millis(50);
        /// Blocks per tick (~4.3 b/s, close to vanilla walking speed).
        const STEP: f64 = 0.215;

        let horizontal_distance = |pos: Vec3| -> f64 {
            let dx = pos.x - target.x;
            let dz = pos.z - target.z;
            (dx * dx + dz * dz).sqrt()
        };

        let walk = async {
            loop {
                let pos = self.position().ok_or(BotError::PositionUnknown)?;
                if horizontal_distance(pos) <= tolerance {
                    return Ok(());
                }
                if self.is_finished() {
                    return Err(BotError::Closed);
                }
                self.step_toward(target, STEP)?;
                crate::native_time::sleep(TICK).await;
            }
        };

        match crate::native_time::timeout(timeout, walk).await {
            Ok(Ok(())) => Ok(WalkOutcome::Arrived),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                let remaining = self.position().map_or(f64::INFINITY, horizontal_distance);
                Ok(WalkOutcome::TimedOut { remaining })
            }
        }
    }

    // --- Awaiting -----------------------------------------------------------

    /// Waits until `predicate` returns `true`, or the `timeout` elapses.
    ///
    /// The predicate is re-evaluated every time the read-model changes, and it
    /// is given this handle so it can query any part of the read-model:
    ///
    /// ```no_run
    /// # async fn demo(handle: lodestone_client::ClientHandle) {
    /// use std::time::Duration;
    /// handle
    ///     .wait_for(Duration::from_secs(5), |h| h.health().unwrap_or(0.0) > 10.0)
    ///     .await
    ///     .ok();
    /// # }
    /// ```
    ///
    /// Awaiting never blocks the driver: the driver keeps processing packets and
    /// answering keep-alives while a bot waits.
    ///
    /// # Errors
    ///
    /// Returns [`WaitError::Timeout`] if the condition is not met in time, or
    /// [`WaitError::Closed`] if the session ends first.
    pub async fn wait_for<F>(&self, timeout: Duration, predicate: F) -> Result<(), WaitError>
    where
        F: FnMut(&Self) -> bool,
    {
        #[cfg(not(target_arch = "wasm32"))]
        {
            match crate::native_time::timeout(timeout, self.wait_loop(predicate)).await {
                Ok(result) => result,
                Err(_) => Err(WaitError::Timeout),
            }
        }
        // wasm32 has no runtime timer (a timeout would panic like a wall-clock
        // read), so the timeout is not enforced there; the wait is
        // still cancellable by the session ending.
        #[cfg(target_arch = "wasm32")]
        {
            let _ = timeout;
            self.wait_loop(predicate).await
        }
    }

    /// Core wait loop shared by both targets. Registers interest *before*
    /// checking the predicate to avoid a lost-wakeup race with the driver.
    async fn wait_loop<F>(&self, mut predicate: F) -> Result<(), WaitError>
    where
        F: FnMut(&Self) -> bool,
    {
        let notify = self.state.notifier();
        let notified = notify.notified();
        tokio::pin!(notified);
        loop {
            // `enable()` registers this waiter now, so a notify that races the
            // predicate check below is not missed.
            notified.as_mut().enable();
            if predicate(self) {
                return Ok(());
            }
            if self.is_finished() {
                return Err(WaitError::Closed);
            }
            notified.as_mut().await;
            notified.set(notify.notified());
        }
    }

    /// Waits until the client has entered the world (a `Login` event was seen).
    ///
    /// # Errors
    ///
    /// See [`ClientHandle::wait_for`].
    pub async fn wait_for_login(&self, timeout: Duration) -> Result<(), WaitError> {
        self.wait_for(timeout, |h| h.player().entity_id.is_some())
            .await
    }

    /// Waits until the server has placed the player (a position is known).
    ///
    /// # Errors
    ///
    /// See [`ClientHandle::wait_for`].
    pub async fn wait_for_spawn(&self, timeout: Duration) -> Result<(), WaitError> {
        self.wait_for(timeout, |h| h.position().is_some()).await
    }

    /// Waits until the chunk at `pos` is loaded.
    ///
    /// # Errors
    ///
    /// See [`ClientHandle::wait_for`].
    pub async fn wait_for_chunk(&self, pos: ChunkPos, timeout: Duration) -> Result<(), WaitError> {
        self.wait_for(timeout, move |h| h.is_chunk_loaded(pos))
            .await
    }

    /// Waits until at least `count` chunk columns are loaded.
    ///
    /// # Errors
    ///
    /// See [`ClientHandle::wait_for`].
    pub async fn wait_for_chunks(&self, count: usize, timeout: Duration) -> Result<(), WaitError> {
        self.wait_for(timeout, move |h| h.loaded_chunk_count() >= count)
            .await
    }
}

/// The player's eye position: a block above the feet, so `look_at` aims from the
/// head as a player does.
fn eye_of(feet: Vec3) -> Vec3 {
    Vec3::new(feet.x, feet.y + 1.62, feet.z)
}

/// Computes the Minecraft `(yaw, pitch)` that points from `eye` to `target`.
fn look_at_rotation(eye: Vec3, target: Vec3) -> Rotation {
    let delta = target - eye;
    let horizontal = (delta.x * delta.x + delta.z * delta.z).sqrt();
    // Minecraft yaw: 0 faces +Z, increasing toward -X.
    let yaw = (-delta.x).atan2(delta.z).to_degrees() as f32;
    let pitch = (-delta.y).atan2(horizontal).to_degrees() as f32;
    Rotation::new(yaw, pitch)
}

/// The outcome of a [`ClientHandle::walk_to`] call that ran without a hard
/// error.
///
/// `walk_to` speaks only about the driver's optimistic **local prediction** —
/// each `Move` is folded into the read-model before the server confirms it — so
/// [`Arrived`](WalkOutcome::Arrived) means the prediction converged on the
/// target, not that the server acknowledged the displacement. Confirmed
/// movement is a separate, stronger gate (observe your own entity from a second
/// connection).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum WalkOutcome {
    /// The local prediction reached within `tolerance` of the target before the
    /// timeout elapsed.
    Arrived,

    /// The timeout elapsed first. `remaining` is the horizontal distance in
    /// blocks still separating the prediction from the target (or infinity if
    /// the position became unknown); a caller may `walk_to` again to continue or
    /// treat it as blocked.
    TimedOut {
        /// Horizontal blocks still remaining to the target when time ran out.
        remaining: f64,
    },
}

/// The stream of [`ClientEvent`]s produced by a session.
///
/// Call [`EventStream::recv`] in a loop; it returns `None` once the session has
/// ended and all buffered events have been drained.
#[derive(Debug)]
pub struct EventStream {
    rx: mpsc::Receiver<ClientEvent>,
}

impl EventStream {
    pub(crate) fn new(rx: mpsc::Receiver<ClientEvent>) -> Self {
        Self { rx }
    }

    /// Waits for the next event, returning `None` when the session has ended.
    pub async fn recv(&mut self) -> Option<ClientEvent> {
        self.rx.recv().await
    }

    /// Attempts to receive an event without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`tokio::sync::mpsc::error::TryRecvError`] when no event is ready
    /// or the session has ended.
    pub fn try_recv(&mut self) -> Result<ClientEvent, mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }
}

/// The connected dimension's vertical extent — the geometry a mesher needs to
/// place a live column's sections at the correct world-`y`.
///
/// `min_y` is the lowest world-`y` (e.g. `-64` for the overworld) and `height`
/// the number of blocks tall (a multiple of 16, e.g. `384`). Block sections are
/// `height / 16` ([`section_count`](WorldDimensions::section_count)); light
/// sections run one section past the block range at both ends
/// ([`light_section_count`](WorldDimensions::light_section_count)), where light
/// section `0` is the below-world boundary and light section `i` covers block
/// section `i - 1` — the exact indexing
/// [`sections_and_light_at`](ClientHandle::sections_and_light_at) and
/// [`section_light`](ClientHandle::section_light) use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldDimensions {
    /// Lowest world-`y` in the dimension.
    pub min_y: i32,
    /// Dimension height in blocks (a multiple of 16).
    pub height: u32,
}

impl WorldDimensions {
    /// Number of 16-block-tall block sections in a column (`height / 16`).
    #[must_use]
    pub const fn section_count(self) -> usize {
        (self.height / 16) as usize
    }

    /// Number of light sections, which extend one section past the block range
    /// at both ends (`section_count + 2`): light section `0` is the below-world
    /// boundary and the last covers the boundary above the top of the world.
    #[must_use]
    pub const fn light_section_count(self) -> usize {
        self.section_count() + 2
    }
}
