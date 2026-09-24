//! Version-free updates forwarded from the background networking session.
//!
//! The event enum is the shell boundary between async protocol handling and
//! the synchronous simulation loop. It contains actions and observations,
//! while latest-value state remains in [`super::state`].

use super::{BlockStateRef, ClientEvent, LookAnchor, MobEffectId, ParticleOptions, Rotation,
    SoundCategory, Uuid, Vec3, Vec3f};

/// A decoded, version-free update the app can act on without touching tokio.
#[derive(Debug, Clone)]
pub enum NetUpdate {
    /// The background task is attempting to connect.
    Connecting,
    /// The session task reached a named step of establishing the session —
    /// phase names for the loading screen.
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
    /// A server-authoritative entity velocity.
    ///
    /// The ECS ingest fold remains the source for remote entities. This mirror
    /// exists for the local player because the frame thread must observe the
    /// replacement in its early network drain before running the next physics
    /// tick and sending the resulting position back to the server.
    EntityVelocity {
        /// Server entity id whose velocity changed.
        entity_id: i32,
        /// Complete replacement velocity, in blocks per tick.
        velocity: Vec3,
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
        /// The sender's profile UUID — filter key, mirrored from
        /// [`ClientEvent::Chat`] verbatim. Only v770's signed `player_chat`
        /// carries one; system, disguised, action-bar and every legacy-family
        /// message are `None` (`None` must be shown, never hidden).
        sender: Option<Uuid>,
        /// Whether this message's signature was checked against the sender's
        /// announced public key and found valid — [`lodestone_model::event::
        /// ChatAckInfo::verified`], raised by the client driver and never by a
        /// wire decoder.
        ///
        /// **`false` is not "forged", it is "unproven".** It covers a system
        /// message (which carries no `ack` at all), a player message with no
        /// signature, a sender whose public key we never saw, and — because the
        /// driver's verification is compiled out for `wasm32` — every message
        /// in a browser session. A consumer must read it as vanilla reads
        /// `ChatTrustLevel`: verified, or not verified, with no third state
        /// claimed. In particular it cannot express vanilla's `MODIFIED`, which
        /// needs the signed content compared against the displayed content and
        /// is not computed anywhere here.
        verified: bool,
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
    /// This variant carries the unload signal separately from collision.
    /// `LiveCollision` re-reads the store every tick, so it tracks the unload
    /// for free, while the renderer needs an explicit signal —
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
    /// The server changed the radius of the client's streamed chunk view.
    ChunkCacheRadiusChanged {
        /// New streamed radius, in chunks.
        radius: i32,
    },
    /// The server moved the center of its streamed chunk view.
    ChunkCacheCenterChanged {
        /// New center chunk X.
        x: i32,
        /// New center chunk Z.
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
    /// The server processed every predicted block change through `sequence`.
    /// The simulation uses this acknowledgement to retire matching prediction
    /// snapshots without carrying block payloads through the network queue.
    BlockChangedAck {
        /// Highest processed prediction sequence.
        sequence: lodestone_model::PredictionSequence,
    },
    /// An authoritative correction to the local player's yaw and pitch.
    PlayerRotationSet {
        /// Absolute yaw or relative yaw delta, in degrees.
        y_rot: f32,
        /// Whether `y_rot` is relative to the current yaw.
        relative_y: bool,
        /// Absolute pitch or relative pitch delta, in degrees.
        x_rot: f32,
        /// Whether `x_rot` is relative to the current pitch.
        relative_x: bool,
    },
    /// The server directed the local player to look at an already-resolved
    /// world-space target. The anchor decides whether that direction starts at
    /// the player's feet or current eye.
    PlayerLookAt {
        /// Local origin anchor selected by the server.
        from_anchor: LookAnchor,
        /// Absolute target position.
        target: Vec3,
    },
    /// The server selected an entity for the client camera to follow. The
    /// simulation resolves the id against shared entity state every frame.
    CameraSet {
        /// Server entity id selected as the camera subject.
        entity_id: i32,
    },
    /// A `block_event` (vanilla's `ClientboundBlockEventPacket`): two opaque
    /// parameter bytes for the block at `pos`.
    ///
    /// Deliberately **uninterpreted here**. The two bytes mean completely
    /// different things per block type — `b0 == 1` on a chest is "viewer count in
    /// `b1`" (vanilla's own chest block-event handling), on a note block it is a pitch, on
    /// a piston a direction — and the adapter already declines to interpret them
    /// for exactly that reason. `Sim::poll_net` forwards them to the one consumer
    /// that knows the rule.
    ///
    /// This variant lets a chest lid open. The
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
    /// An explosion — forwarded raw, mirroring [`NetUpdate::BlockEvent`]'s
    /// "carries no interpretation" shape: see
    /// `lodestone_model::event::ClientEvent::Explosion`'s own doc for why
    /// `affected_blocks` below is unconditionally empty on protocols 774 and
    /// 776, and what that does and does not mean.
    Explosion {
        /// World-space explosion centre.
        pos: Vec3,
        /// Blast radius, in blocks.
        radius: f32,
        /// Blocks removed, as offsets from `pos` — see
        /// `ClientEvent::Explosion::affected_blocks`'s doc for which
        /// families populate this and which never do.
        affected_blocks: Vec<[i8; 3]>,
        /// This client's own knockback impulse, if the packet carried one.
        knockback: Option<Vec3>,
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
    /// The server requested that the book in one of the player's hands opens
    /// (`ClientboundOpenBookPacket`, decoded as [`ClientEvent::BookOpened`]).
    ///
    /// This remains a hand selector rather than carrying book contents: the
    /// matching inventory update is the authoritative source of the item's
    /// components, and `Sim` owns the hand-to-book projection.
    BookOpened {
        /// `true` for the main hand and `false` for the off hand.
        main_hand: bool,
    },
    /// The server reported a block being destroyed at `pos`, carrying the state
    /// id it had **before** breaking.
    ///
    /// This is vanilla's `LevelEvent.PARTICLES_DESTROY_BLOCK` (2001), whose
    /// payload is a block state id (its own state-by-id lookup in
    /// its own level-event handler). It is the authoritative signal that a block broke:
    /// the client cannot derive it from `BLOCK_UPDATE`, because by the time that
    /// arrives the cell is already air and the texture the debris needs is gone.
    BlockDestroyed {
        /// Block position that broke.
        pos: lodestone_model::BlockPos,
        /// The block state the cell held before breaking. Its source tag stays
        /// intact until the particle resolver reaches its generated-model
        /// boundary.
        state: BlockStateRef,
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
        /// cutoff (vanilla's own client-level add-particle override-limiter, `1024.0`
        /// being `32.0` squared).
        long_distance: bool,
        /// Whether the particle survives the **Minimal** particle setting --
        /// vanilla's own client-level particle-level calculation's one-in-ten reprieve, not an
        /// exemption. Independent of `long_distance`, which is the *distance*
        /// gate; `false` on every legacy family because the field does not
        /// exist on their particle packets.
        always_show: bool,
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
        /// The particle type's own extra payload, if it carries one. See
        /// [`lodestone_model::event::ParticleOptions`].
        options: ParticleOptions,
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
    /// shell shows the death screen and [`NetUpdate::Respawned`]
    /// follows once the player clicks Respawn and the server confirms it — the
    /// client library no longer auto-respawns (`RespawnPolicy::Manual`, set on
    /// the `ClientBuilder` in [`run`]), which is the actual behaviour change
    /// this issue asked for; a screen with no gate behind it would have
    /// nothing to show.
    Death {
        /// The server's own death message
        /// ([`ClientEvent::Death`](lodestone_model::event::ClientEvent::Death)'s
        /// `message` field), carried through **unflattened**: this thread has
        /// no language table, so flattening here would throw one away before
        /// [`Sim::poll_net`](crate::sim::Sim::poll_net) ever gets a chance to
        /// resolve it. The death-message format keys this workspace's own
        /// built-in fallback table lists (`death.attack.mob` and siblings)
        /// are `"%1$s was slain by %2$s"`-shaped, and vanilla substitutes the
        /// killer's *display name* component there, which for a player is
        /// exactly the kind of name that carries a `hoverEvent`/`clickEvent`
        /// in chat — so a flatten here would lose more than the wording.
        /// `Sim::poll_net`'s `Death` arm resolves this against the real
        /// language table (`Sim::resolve_text`) and flattens it through
        /// [`lodestone_model::ResolvedText::to_interactive_spans`], so the
        /// death screen keeps the same style/interactivity chat does.
        message: lodestone_model::Text,
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
    /// The server signalled `WIN_GAME`: the local player exited
    /// the End through the exit portal after the dragon fight. Carries no
    /// data — see [`lodestone_model::event::ClientEvent::WinGame`]'s own doc
    /// for why. `Sim::poll_net` latches this into a `won` flag,
    /// `WindowApp::drive_ui_from_session` notices it and shows the credits
    /// screen (`UiState::show_credits`) — the same shape as
    /// [`NetUpdate::Death`]/`Sim::is_dead`/`UiState::die`.
    WinGame,
    /// The world was published to LAN on `port`. Reported rather
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
    /// Stop live sounds created by server sound packets. Both filters are
    /// optional wildcards: absent `name` and `category` stops every such voice.
    SoundStopped {
        /// Sound event key path with its namespace stripped, if restricted.
        name: Option<String>,
        /// Source bus restriction, if any.
        category: Option<SoundCategory>,
    },
    /// A mob effect (potion effect) was applied to or refreshed on an entity
    /// (`update_mob_effect`). Carries `entity_id` unfiltered — the packet
    /// applies to any entity, not just the local player — so the sim decides
    /// whether it is the locally-tracked player before folding it into
    /// [`lodestone_physics::PlayerState::effects`].
    EffectApplied {
        /// Entity the effect applies to.
        entity_id: i32,
        /// Validated built-in effect id.
        effect: MobEffectId,
        /// Effect amplifier (0 = level I).
        amplifier: u32,
        /// Remaining duration in ticks; `-1` means infinite.
        duration_ticks: i32,
        /// Whether the effect is ambient (beacon/aura source): the HUD draws it
        /// fainter.
        ambient: bool,
        /// Whether the effect emits its normal particles.
        show_particles: bool,
        /// Whether the effect shows a HUD icon at all.
        show_icon: bool,
        /// Whether effect-specific visual transitions should animate.
        blend: bool,
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
        /// Validated built-in effect id.
        effect: MobEffectId,
    },
    /// An item entity was collected (`take_item_entity`), for the fly-to-collector
    /// animation.
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
    /// rather than the raw key. The synthetic senders in this
    /// module (`"stream closed"`, the `TransferRequested`-derived "server
    /// transferred you to …" message `run_async` builds when the stream
    /// closes right after a transfer, and `sim.rs`'s test-only `"Server
    /// closed"`) use [`lodestone_model::Text::literal`]: they are not
    /// vanilla translation keys, so wrapping them in a `Text` that merely
    /// carries their literal English through the same pipe is correct —
    /// the translator is a no-op on a `Literal` node, it only rewrites
    /// `Translate` nodes.
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
    /// A publish-to-LAN request failed server-side — own
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
        /// Velocity correction carried by newer protocol families.
        velocity: Option<lodestone_model::event::TeleportVelocity>,
    },
}
