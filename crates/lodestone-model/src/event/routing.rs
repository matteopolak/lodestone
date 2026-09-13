//! Event routing ownership and its exhaustive coverage tests.

use super::client::ClientEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Route {
    /// `lodestone_ecs::ingest` folds it: **per-entity ECS state** — components
    /// hanging off an entity in the client-owned world.
    pub ingest: bool,
    /// `lodestone_ecs::session` folds it: **local-player and session scalars** —
    /// vitals, xp, abilities, game mode, menus, scoreboard, tab list, boss bars.
    pub session: bool,
    /// `lodestone_shell`'s `net::forward` has an arm for it: **block and world
    /// state**, plus anything the renderer, HUD or audio reads off the shell's own
    /// `NetUpdate` stream. Such events need no `handles_event` arm at all.
    pub shell: bool,
    /// The shell's arm is **conditional or intercepted** — a match guard or a
    /// literal field pattern in `forward`, or a pre-forward shell consumer — so
    /// the `debug_assert!` on `forward`'s catch-all must not demand an
    /// unconditional forwarding arm.
    ///
    /// Three variants currently use this escape hatch, and all are a property
    /// of `net.rs` as it stands rather than of the event: `LevelEvent` (only
    /// sub-event `2001` is consumed), `EntitySpawned` (only `lightning_bolt`, to
    /// count flashes), and `ResourcePackPopped` (the connection loop clears the
    /// live pack before `forward` sees the event). If a guarded arm becomes
    /// unconditional, or the pre-forward consumer moves into `forward`, clear
    /// or adjust this and the assert gets stricter for free.
    pub shell_conditional: bool,
    /// Consumed inside `lodestone-client` itself by something that is **not** one
    /// of the three routers, so [`Route::NOWHERE`] can mean "nothing anywhere"
    /// rather than "nothing I happened to check". Exactly four such places:
    ///
    /// * `Driver::emit`'s auto-response switch (keep-alive, chat acknowledgement,
    ///   `player_loaded`, auto-respawn, cookie response/store, transfer outcome) —
    ///   a protocol reply or session result, not screen state.
    /// * `LocalEcho::apply`, which is down to `TeleportPlayer` alone.
    /// * `SharedState::apply`'s own `TimeChanged` arm, which writes `WorldTime`
    ///   ahead of consulting either `handles_event`.
    /// * `SharedState::apply`'s optional `GameEventBus`, which carries every
    ///   event to installed client plugins; the shipped brand-channel plugin
    ///   consumes `CustomPayload` there.
    ///
    /// Chunk payloads are a fourth path but not a router: the version adapter
    /// writes them straight through the `lodestone_world::WorldSink`, and the
    /// event is only a dirty-region signal.
    pub client: bool,
}

impl Route {
    /// Claimed by nothing. A legal, and sometimes correct, answer — see
    /// [`route`]'s note on what it costs to write it.
    pub const NOWHERE: Self = Self {
        ingest: false,
        session: false,
        shell: false,
        shell_conditional: false,
        client: false,
    };

    /// `true` when `lodestone_shell::net::forward` must have an **unconditional**
    /// arm for this event. The `debug_assert!` on that function's catch-all reads
    /// exactly this.
    #[must_use]
    pub const fn must_forward(self) -> bool {
        self.shell && !self.shell_conditional
    }

    /// `true` when nothing in the tree consumes the event: decoded, tested, and
    /// reaching zero pixels. Not a bug by itself — plenty of packets are decoded
    /// ahead of a consumer — but it is the shape `CLAUDE.md` §1 calls an island,
    /// and it is what `docs/event-routing.md` keeps a list of.
    #[must_use]
    pub const fn is_island(self) -> bool {
        !self.ingest && !self.session && !self.shell && !self.client
    }
}

/// Which routers claim `event`, as a single exhaustive table.
///
/// # The convention, which is the whole decision this match exists to force
///
/// * **per-entity state** → `ingest`. Components on an ECS entity: position,
///   metadata, equipment, hurt animation.
/// * **local-player scalars** → `session`. Anything scoped to *this* session:
///   health, xp, abilities, open menus, the scoreboard.
/// * **block and world state** → `shell`. It travels the shell's own `NetUpdate`
///   stream and needs no `handles_event` arm at all — the chest-lid work needed
///   none.
///
/// Guessing the `ingest`/`session` fork wrong has cost work twice
/// (`DimensionTypeChanged`, `AbilitiesChanged`): both compile, both unit-test
/// green, and neither runs, because `SharedState::apply` forwards only what one of
/// the two predicates lists.
///
/// # The trade
///
/// Adding a `ClientEvent` variant now costs **one mandatory one-line arm here**,
/// and in exchange it **cannot** island silently. Before this table it cost
/// nothing and risked silence. [`Route::NOWHERE`] is still available, but it has
/// to be typed on purpose, with a reason next to it — which is the difference
/// between a decision and the defect.
///
/// # This table describes what the code does, not what it should do
///
/// It was transcribed from the three routers, arm for arm. Where it says
/// [`Route::NOWHERE`] and a fold nevertheless exists somewhere, that is a finding
/// recorded in `docs/event-routing.md`, **not** something this function quietly
/// fixes: changing a route here changes runtime behaviour and belongs in its own
/// reviewable commit.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn route(event: &ClientEvent) -> Route {
    const INGEST: Route = Route {
        ingest: true,
        ..Route::NOWHERE
    };
    const SESSION: Route = Route {
        session: true,
        ..Route::NOWHERE
    };
    const SHELL: Route = Route {
        shell: true,
        ..Route::NOWHERE
    };
    // The shell has an arm but it is guarded; see `Route::shell_conditional`.
    const SHELL_PARTIAL: Route = Route {
        shell: true,
        shell_conditional: true,
        ..Route::NOWHERE
    };
    const CLIENT: Route = Route {
        client: true,
        ..Route::NOWHERE
    };

    match event {
        // ---- the local player's arrival, claimed by everything ----------------
        //
        // `ingest` takes the entity id and the `EntityIndex` entry, `session`
        // takes the game mode / dimension / alive scalars, the shell takes
        // `NetUpdate::LoggedIn`, and the driver arms its `player_loaded` latch.
        // Four disjoint writes, one event.
        ClientEvent::Login { .. } => Route {
            ingest: true,
            session: true,
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // `Respawned` is the same shape minus `ingest`: no entity id is reassigned.
        ClientEvent::Respawned { .. } => Route {
            session: true,
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // `Death` drives the death screen (shell), the `alive` scalar (session),
        // and the driver's automatic respawn.
        ClientEvent::Death { .. } => Route {
            session: true,
            shell: true,
            client: true,
            ..Route::NOWHERE
        },

        // ---- per-entity ECS state -------------------------------------------
        ClientEvent::EntityMoved { .. }
        | ClientEvent::EntityTeleported { .. }
        | ClientEvent::EntityRemoved { .. }
        | ClientEvent::EntityHeadRotation { .. }
        | ClientEvent::EntityMetadataUpdated { .. }
        | ClientEvent::EntityAttributesUpdated { .. }
        | ClientEvent::EntityEquipmentUpdated { .. }
        | ClientEvent::EntityDamaged { .. }
        // Per-entity, not a shell-stream fact: the block state becomes a component
        // on the ingest entity and the render extract bridges it through
        // `EntityIndex`, exactly as `HurtTime` and `ItemUse` are. Routing it to the
        // shell instead would compile, test green, and never run — `apply` consults
        // both switches and only forwards what each lists.
        | ClientEvent::FallingBlockState { .. }
        // The other reading of the same spawn field, routed the same way and for
        // the same reason: the owner id becomes `ProjectileOwner` on the ingest
        // entity and the render extract bridges it through `EntityIndex`.
        | ClientEvent::ProjectileOwner { .. }
        // Per-entity despite carrying no entity id, and the distinction is worth
        // stating because "no id" reads as a local-player scalar. The server sends
        // the move vehicle packet only to *reject* a position the
        // client-authoritative rider reported, and what it changes is the
        // vehicle's own `Position`/`Rotation` — components `ingest` already owns
        // the sole writer of. The subject comes from `session::Riding`, exactly as
        // the seat pin already resolves its vehicle from that same scalar.
        | ClientEvent::VehicleMoved { .. }
        // The mob's own leash state (`SET_ENTITY_LINK`) — per-entity
        // like every other row in this block, folded by `lodestone_ecs::ingest::
        // apply_entity_leash` into `Leashed`. Used to be unclaimed entirely (see
        // the "claimed by nothing" block below, which is where this line lived
        // until the fold existed).
        | ClientEvent::EntityLeashed { .. }
        | ClientEvent::EntityAnimation { .. } => INGEST,
        // Remote velocity is per-entity ECS state. The shell also mirrors this
        // event so a packet naming the local player reaches its early network
        // drain before the next physics tick and outbound movement packet.
        ClientEvent::EntityVelocity { .. } => Route {
            ingest: true,
            shell: true,
            ..Route::NOWHERE
        },
        // Both halves, and neither supersedes the other: `ingest` turns this into
        // the per-entity `HurtTime` countdown and destructures with `..`,
        // **discarding the yaw**, while the shell's own `forward` reads that yaw to
        // aim the damage camera tilt. Listing it as `INGEST` alone was not a
        // functional gap — `forward`'s `debug_assert` is one-directional, so the
        // wiring worked — but this table is what a reader consults to answer "does
        // anything consume event X", and understating a consumer here is exactly the
        // authoritative-looking-and-quietly-wrong record this repo pays for most.
        ClientEvent::EntityHurtAnimation { .. } => Route {
            ingest: true,
            shell: true,
            ..Route::NOWHERE
        },
        // Riding is genuinely both halves — the component pair one side, the local
        // player's own `Riding` scalar the other.
        ClientEvent::EntityPassengersChanged { .. } => Route {
            ingest: true,
            session: true,
            ..Route::NOWHERE
        },
        // Most statuses remain per-entity `ingest` state: byte 3 starts a
        // `DeathTime` on the entity the packet names. The local player additionally
        // consumes bytes 24..28 as its permission level, so the one queued event
        // also reaches the session fold. The two systems write disjoint components.
        ClientEvent::EntityStatus { .. } => Route {
            ingest: true,
            session: true,
            ..Route::NOWHERE
        },
        // `ingest` spawns the entity; the shell arm is guarded on
        // `lightning_bolt` and only counts flashes, so every other spawn
        // legitimately reaches `forward`'s catch-all.
        ClientEvent::EntitySpawned { .. } => Route {
            ingest: true,
            shell: true,
            shell_conditional: true,
            ..Route::NOWHERE
        },
        ClientEvent::PlayerProfileNamed { .. } => INGEST,

        // ---- local-player and session scalars --------------------------------
        ClientEvent::HealthChanged { .. }
        | ClientEvent::ExperienceChanged { .. }
        | ClientEvent::GameModeChanged { .. }
        | ClientEvent::AbilitiesChanged { .. }
        | ClientEvent::DimensionTypeChanged { .. }
        | ClientEvent::BiomeVisuals { .. } => SESSION,
        // Two of the three `HudState`-shaped islands this table found
        // (`docs/event-routing.md`): a fold existed and was unit-tested, but
        // `lodestone_game::player_state::HudState` itself has no production
        // caller (Stage 3 superseded it with the session components above and
        // never re-homed these two onto one). Both are local-player scalars,
        // so `session` is correct either way — the fix was writing
        // `crate::player::SelectedSlot` / `ServerDifficulty` from
        // `apply_local_player_state`, not reviving `HudState::apply`.
        | ClientEvent::HeldSlotChanged { .. }
        | ClientEvent::DifficultyChanged { .. } => SESSION,
        // The third: `lodestone_game::mining::BlockDestructionOverlays::apply`
        // existed and was unit-tested with no caller anywhere. This is about
        // *other players'* blocks, so it is tempting to read it as "block/world
        // state" and route it `shell` the way the chest-lid `BlockEvent` is —
        // but `BlockDestructionOverlays` is a per-session collection keyed by
        // breaking-entity id (one entity breaks one block at a time), the same
        // shape as `SessionBossBars`/`SessionTabList` just above, not a
        // world-geometry fact the mesher owns. Folded into
        // `SessionBlockDestruction` alongside them.
        ClientEvent::BlockDestruction { .. } => SESSION,
        // scoreboard, tab list, boss bars
        ClientEvent::ObjectiveUpdate { .. }
        | ClientEvent::DisplayObjective { .. }
        | ClientEvent::ScoreUpdate { .. }
        | ClientEvent::ScoreReset { .. }
        | ClientEvent::TeamUpdate { .. }
        | ClientEvent::PlayerListUpdate { .. }
        | ClientEvent::PlayerListRemove { .. }
        | ClientEvent::PlayerListRemoveByName { .. }
        | ClientEvent::BossBarUpdate { .. } => SESSION,
        // menus / containers
        ClientEvent::ScreenOpened { .. }
        | ClientEvent::ScreenClosed { .. }
        | ClientEvent::ContainerContent { .. }
        | ClientEvent::ContainerSlot { .. }
        | ClientEvent::ContainerData { .. }
        | ClientEvent::CursorItemChanged { .. }
        | ClientEvent::InventorySlotChanged { .. } => SESSION,

        // ---- the shell's own stream ------------------------------------------
        ClientEvent::Disconnect { .. }
        // `SessionFailed` is `Disconnect`'s client-side twin and takes the same
        // route, established from the consumer rather than from the shape: the
        // only thing in the tree that ends a session is
        // `SessionPhase::Ended`, and the only writer of that is
        // `lodestone_shell::sim::Sim::set_phase`, called from `poll_net`'s
        // `NetUpdate` arms. Nothing in `lodestone_ecs::session` folds a `Phase`
        // from a `ClientEvent` at all, so a `session` route here would compile,
        // test green, and reach no screen — and a terminal session failure is
        // not per-entity state either, which rules out `ingest`.
        | ClientEvent::SessionFailed { .. }
        | ClientEvent::Particles { .. }
        | ClientEvent::Sound { .. }
        | ClientEvent::EntitySound { .. }
        | ClientEvent::MobEffectApplied { .. }
        | ClientEvent::MobEffectRemoved { .. }
        | ClientEvent::TitleText { .. }
        | ClientEvent::SubtitleText { .. }
        | ClientEvent::TitlesAnimation { .. }
        | ClientEvent::TitlesCleared { .. }
        | ClientEvent::SectionBlocksChanged { .. }
        | ClientEvent::BlockEvent { .. }
        // A blast, same category as the two block-update variants just
        // above: world/block state, not per-entity (no entity owns a
        // world position) and not a local-player scalar (the knockback it
        // may carry is a one-shot impulse, not persisted session state) —
        // see `route`'s own doc for the convention this follows.
        | ClientEvent::Explosion { .. }
        | ClientEvent::ItemPickup { .. }
        | ClientEvent::WeatherChanged { .. }
        | ClientEvent::BiomeClimates { .. }
        // Same shape as `BiomeClimates` just above: a registry-generation
        // table folded into a shell-owned cell (`net::BiomeNameCell`), read by
        // the mesher at mesh time. No `handles_event` arm needed.
        | ClientEvent::BiomeRegistryNames { .. }
        // The credits screen: a pure world/session signal with no
        // per-entity or per-session scalar to fold, forwarded to the shell's
        // own `NetUpdate` stream exactly like `WeatherChanged`.
        | ClientEvent::WinGame
        // Same shape as `BiomeRegistryNames` just above — a
        // registry-generation table with one obvious consumer (the chat
        // box), no per-entity or per-session scalar to fold. Both travel the
        // shell's own stream; no `handles_event` arm needed for either.
        | ClientEvent::CommandTreeUpdated { .. }
        | ClientEvent::CommandSuggestionsReceived { .. }
        // `net.rs`'s `forward` already has an unconditional arm for this
        // (`NetUpdate::SignEditorOpened`, consumed by `sim/net_apply.rs`); this
        // entry used to live in the "claimed by nothing" block below, stale
        // from before that consumer landed.
        | ClientEvent::SignEditorOpened { .. }
        // The book-open signal follows the same shell-only path: `forward`
        // turns it into `NetUpdate::BookOpened`, and `Sim` holds it until the
        // app projects the selected hand into the book screen.
        | ClientEvent::BookOpened { .. } => SHELL,
        // `run_session` consumes this before calling `forward`: it clears the
        // live server pack and any matching prompt. There is no `NetUpdate` to
        // enqueue, so this is a shell interception rather than an unconditional
        // `forward` arm; `SHELL_PARTIAL` keeps the catch-all assertion honest.
        ClientEvent::ResourcePackPopped { .. } => SHELL_PARTIAL,
        // Chat reaches the shell feed *and* the driver's signed-message
        // acknowledgement valve.
        ClientEvent::Chat { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // The shell camera adopts the authoritative pose; `LocalEcho` keeps the
        // read-model's `position()` honest; the driver consumes its
        // `player_loaded` latch on the first one after entering the world.
        ClientEvent::TeleportPlayer { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // A dirty-region signal to the shell; the payload was already written
        // through the `WorldSink` by the adapter.
        ClientEvent::ChunkLoaded { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // The eviction twin, and this entry used to read `CLIENT` with the
        // comment "the adapter has already dropped the column through the
        // `WorldSink`, so the event is a notification with nothing left to do."
        // That was true of the *world* and false of the *renderer*:
        // collision re-reads the store every tick and so tracked the
        // unload for free, while the GPU kept every section the column ever
        // uploaded — for the whole session, unculled, against a fixed-capacity
        // origin arena. Kept as a worked example of the failure mode `CLAUDE.md`
        // §2 warns about: a routing claim that is accurate about one consumer and
        // silently wrong about another, which nothing about it looks stale.
        ClientEvent::ChunkUnloaded { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // Only sub-event 2001 (block-break effect) is consumed; the rest fall
        // through on purpose, so adding a consumer later is a new arm and not a
        // new packet.
        ClientEvent::LevelEvent { .. } => SHELL_PARTIAL,

        // ---- consumed inside `lodestone-client` ------------------------------
        // Answered by `Driver::emit`, and the tick surrogate that flushes pending
        // chat acknowledgements.
        ClientEvent::KeepAlive { .. } => CLIENT,
        // `Driver::emit` immediately turns the challenge into a pong action
        // before the shell event loop runs, so this is a client-internal
        // consumer rather than an island.
        ClientEvent::Ping { .. } => CLIENT,
        // The client retains the echoed request timestamp in its local
        // read-model; the shell compares it with its portable current clock for
        // the F3 round-trip-time line.
        ClientEvent::PongReceived { .. } => CLIENT,
        // `Driver::emit` answers from its in-memory cookie store immediately;
        // the resulting `CookieResponse` action is sent before the event reaches
        // the shell, so this is a client-internal consumer rather than an island.
        ClientEvent::CookieRequested { .. } => CLIENT,
        // `Driver::emit` writes the received payload into the same in-memory
        // cookie store used by `CookieRequested`, before the event reaches any
        // router. It is therefore consumed by the client even though no action
        // is emitted for the store operation itself.
        ClientEvent::CookieStored { .. } => CLIENT,
        // `Driver::emit` immediately answers the pushed pack before surfacing
        // the event, so configuration cannot stall waiting for the shell.
        ClientEvent::ResourcePackPushed { .. } => CLIENT,
        // `Driver::emit` records the existing `SessionOutcome::Transferred`
        // result before surfacing this event, so a caller can reconnect with
        // the target and the driver's preserved cookie store.
        ClientEvent::TransferRequested { .. } => CLIENT,
        // `Driver::emit` removes the deleted full signature from its pending
        // acknowledgement tracker before surfacing the event, so the server
        // is not acknowledged for a message it withdrew.
        ClientEvent::ChatMessageDeleted { .. } => CLIENT,
        // `SharedState::apply`'s own arm, ahead of both `handles_event` calls:
        // straight into the `WorldTime` resource.
        ClientEvent::TimeChanged { .. } => CLIENT,
        // `SharedState::apply` publishes this through the optional `GameEvent`
        // bus before routing. The shipped app installs a brand-channel plugin,
        // whose typed decoder and state fold consume this channel in production.
        ClientEvent::CustomPayload { .. } => CLIENT,

        // ---- world-level admin state, folded by `session` ---------------------
        //
        // All nine of these were in the island block below. They are `session`
        // rather than `ingest` because none is per-entity: they are scalars scoped
        // to the world this session is connected to, which is the same category
        // `DimensionTypeChanged` and `AbilitiesChanged` fall into — and both of
        // those cost work by being guessed as `ingest` first.
        //
        // `TabListChanged` is the one that needed no new fold at all:
        // `lodestone_game::tablist::TabList::apply` has had a header/footer arm
        // and `session::apply_tab_list` has been registered since before this
        // routing fix, so the event was decoded, folded-capable, and simply never
        // asked for. The other eight got folds in the same commit as this flag,
        // per the instruction in the block below.
        ClientEvent::TabListChanged { .. } => SESSION,
        // `lodestone_game::recipe::RecipeBookSettings` via
        // `apply_recipe_book_settings`. Not a world scalar like its neighbours
        // here — it is per-*player* UI state the server persists — but `session`
        // for exactly that reason, and certainly not `ingest`.
        ClientEvent::RecipeBookSettingsChanged { .. } => SESSION,
        // Keyed on map id, not on an entity: several players and several item
        // frames can show the same map, so this is session-scoped state
        // (`SessionMaps`) and never a component on the holder.
        ClientEvent::MapItemData { .. } => SESSION,
        // The tree and progress are the local player's, so `session`
        // (`SessionAdvancements`). The advancements *screen* reads that
        // component; it needs no `forward` arm.
        ClientEvent::AdvancementsUpdated { .. } => SESSION,
        // The selected advancement tab is separate from advancement progress:
        // the former is a UI cursor while the latter is the criterion tree.
        // Both are local-player session state and reach the screen through
        // their respective `Session*` components.
        ClientEvent::AdvancementsTabSelected { .. } => SESSION,
        // `lodestone_game::worldborder::WorldBorder` via `apply_world_border`.
        // The largest single cluster in `docs/event-routing.md`'s island list.
        ClientEvent::WorldBorderCenterChanged { .. }
        | ClientEvent::WorldBorderSizeLerping { .. }
        | ClientEvent::WorldBorderSizeChanged { .. }
        | ClientEvent::WorldBorderWarningDelayChanged { .. }
        | ClientEvent::WorldBorderWarningDistanceChanged { .. }
        | ClientEvent::WorldBorderInitialized { .. } => SESSION,
        // `lodestone_game::levelstate::SpawnPoint` via `apply_spawn_point` — the
        // compass target every legacy family's packet doc names.
        ClientEvent::SpawnPositionChanged { .. } => SESSION,
        // `lodestone_game::levelstate::GameRuleValues` via `apply_game_rules`.
        // Note this is *not* the typed registry, which is server-side and
        // unbuilt.
        ClientEvent::GameRulesChanged { .. } => SESSION,

        // ---- the remaining clientbound set, all session --------------------
        //
        // Every one of these folds into a `Session*` component in
        // `lodestone_ecs::session`, the same way the scoreboard, the tab list,
        // maps and advancements already do. That is why none of them appears in
        // `net::forward` and why the `debug_assert!` on its catch-all stays
        // quiet: `shell` is false on purpose, not by omission.
        //
        // `DebugEntityValue` is the one worth arguing about. It names an entity,
        // and `route`'s convention says per-entity state is `ingest` — but a
        // debug feed is keyed by *subscription* and outlives the entity's ECS
        // row, so folding it as a component would resurrect rows the client has
        // already dropped. It is session state about an entity, not entity state.
        // The server's own `minecraft:enchantment` order. Routed to `session`
        // rather than `shell` (where `BiomeRegistryNames` goes) on purpose: a
        // `shell` route needs an unconditional arm in `net::forward` or its
        // `debug_assert!` fires, and a session component reaches the same
        // consumer -- `Sim` holds the session `World` -- with no shell edit and
        // no second table. `BiomeRegistryNames` predates the session-fold
        // convention; it is not a precedent to copy.
        // The recipe/trade tranche folds into `SessionRecipeBook` and
        // `SessionTrades`; `MerchantOffersReceived` is a *menu* the way the other
        // container events are, so it is session state and not per-entity state
        // about the villager.
        ClientEvent::RecipeBookAdded { .. }
        | ClientEvent::RecipeBookRemoved { .. }
        | ClientEvent::GhostRecipeShown { .. }
        | ClientEvent::RecipePropertySetsUpdated { .. }
        | ClientEvent::MerchantOffersReceived { .. }
        | ClientEvent::EnchantmentRegistryNames { .. }
        | ClientEvent::StatisticsAwarded { .. }
        | ClientEvent::ChatCompletionsChanged { .. }
        | ClientEvent::DebugBlockValue { .. }
        | ClientEvent::DebugChunkValue { .. }
        | ClientEvent::DebugEntityValue { .. }
        | ClientEvent::DebugEvent { .. }
        | ClientEvent::DebugSample { .. }
        | ClientEvent::GameTestHighlightPos { .. }
        | ClientEvent::LowDiskSpaceWarning
        | ClientEvent::CustomReportDetails { .. }
        | ClientEvent::ServerLinksReceived { .. }
        | ClientEvent::WaypointUpdated { .. }
        | ClientEvent::TagQueryResponse { .. }
        | ClientEvent::TickingStateChanged { .. }
        | ClientEvent::TickingStepped { .. }
        | ClientEvent::TestInstanceBlockStatus { .. }
        | ClientEvent::DialogShown { .. }
        | ClientEvent::DialogCleared => SESSION,

        // ---- claimed by nothing ---------------------------------------------
        //
        // Decoded and tested, consumed nowhere. Each line here is a candidate
        // island; `docs/event-routing.md` records which of these are simply
        // ahead of a consumer. The three that had a fold sitting unwired behind
        // them (`BlockDestruction`, `HeldSlotChanged`, `DifficultyChanged`) were
        // fixed above and are no longer in this list, and neither are the nine
        // world-level scalars in the block immediately above.
        //
        // Do not "fix" one by flipping a flag: the flag only says who is *asked*,
        // and a router that is asked but has no system for the event drops it just
        // as silently. Write the system, then the flag, in one commit.
        // The placement predictor owns the single prediction sequence and its
        // pending snapshot ledger. `net::forward` carries this acknowledgement
        // to `Sim::settle_placement_predictions`, which retires every snapshot
        // the server has processed; leaving the event unclaimed made that ledger
        // grow once for every optimistic placement for the whole session.
        ClientEvent::BlockChangedAck { .. } => SHELL,
        // This is a local-player correction, but it belongs on the shell stream
        // rather than `session`: `PhysicsState` is the camera/raycast/egress pose
        // owner, and `net::forward` gives its frame-thread consumer both the
        // absolute-versus-relative flags. A session scalar would compile and leave
        // the rendered view pointed at the old direction.
        ClientEvent::PlayerRotationSet { .. } => SHELL,
        // The server's stream center is distinct from the local player during
        // the loading hand-off. `net::forward` carries it to the loading-grid
        // producer, whose cell queries must follow the server's center rather
        // than a predicted player position.
        ClientEvent::ChunkCacheCenterChanged { .. } => SHELL,
        ClientEvent::ProjectilePowerChanged { .. } => INGEST,
        ClientEvent::ItemCooldown { .. } => SESSION,
        // This is a server-owned world scalar, but it is only a local client
        // fact: the session fold retains it and the F3 instrument panel reads
        // that one value. It is not the streamed view radius, which remains a
        // shell route because it sizes the loading-grid consumer.
        ClientEvent::SimulationDistanceChanged { .. } => SESSION,
        ClientEvent::ServerDataReceived { .. } => SESSION,
        ClientEvent::MountScreenOpened { .. } => SESSION,
        // Combat tracking is a local-player session fact. The fold retains
        // active versus ended and the exact end duration; the F3 HUD reads it
        // rather than manufacturing a local combat timer.
        ClientEvent::PlayerCombatEntered | ClientEvent::PlayerCombatEnded { .. } => SESSION,
        // A stop packet names a sound/category filter, while the mixer owns
        // live voices by opaque handles. `net::forward` carries the filters to
        // `ShellAudio`, which keeps the packet-created name/category-to-handle
        // index and cancels every matching audible voice.
        ClientEvent::SoundStopped { .. } => SHELL,
        // The target is already server-resolved, so the shell can derive the
        // local view direction from the current feet or eye anchor. `PhysicsState`
        // is the existing camera, raycast, audio-listener, and movement-egress
        // consumer; retaining a second target record would reach none of them.
        ClientEvent::PlayerLookAt { .. } => SHELL,
        // `net::forward` carries the selected entity id to `Sim`, which reads
        // that entity's shared pose every frame to drive the rendered camera.
        ClientEvent::CameraSet { .. } => SHELL,
        // The server's actual streamed radius, not the launcher's request.
        // `net::forward` carries it to `Sim::set_view_radius`, which sets the
        // loading screen's chunk-grid size and progress denominator.
        ClientEvent::ChunkCacheRadiusChanged { .. } => SHELL,
    }
}

#[cfg(test)]
mod equipment_slot_tests {
    use crate::EquipmentSlot;

    /// `from_name` is the inverse of `name` for **every** slot, checked against
    /// `ALL` rather than against a list restated here.
    ///
    /// Two hand-written matches that are supposed to be inverses is exactly the
    /// shape that drifts, and a spot-check of two or three slots would not see
    /// it. Iterating `ALL` means adding a variant fails this test until both
    /// matches learn about it.
    #[test]
    fn equipment_slot_names_round_trip() {
        for slot in EquipmentSlot::ALL {
            assert_eq!(
                EquipmentSlot::from_name(slot.name()),
                Some(slot),
                "{slot:?} did not survive name -> from_name"
            );
        }
        // The count is asserted too, so a variant added to the enum but not to
        // `ALL` cannot make the loop above vacuously pass over a short list.
        assert_eq!(EquipmentSlot::ALL.len(), 8);
    }

    /// The control: an unrecognised name is `None`, not a default. If this ever
    /// returns `Some`, the loop above is measuring a function that says yes to
    /// everything.
    #[test]
    fn an_unknown_equipment_slot_name_is_refused() {
        for name in ["", "chestplate", "CHEST", "minecraft:chest", "hand"] {
            assert_eq!(
                EquipmentSlot::from_name(name),
                None,
                "{name:?} must not resolve to a slot"
            );
        }
    }
}

#[cfg(test)]
mod block_state_ref_tests {
    use crate::{BlockStateRef, LevelEventData};

    #[test]
    fn canonical_and_protocol_local_state_ids_keep_the_same_raw_value_distinct() {
        // A deliberately small value: accepting a protocol-local state just
        // because it fits a generated census is the boundary error this type
        // prevents.
        const RAW: u32 = 1;
        let canonical = BlockStateRef::canonical(RAW);
        let local = BlockStateRef::protocol_local(RAW);

        assert_eq!(canonical.raw(), RAW);
        assert_eq!(local.raw(), RAW);
        assert_ne!(canonical, local);
        assert!(matches!(canonical, BlockStateRef::Canonical(RAW)));
        assert!(matches!(local, BlockStateRef::ProtocolLocal(RAW)));
    }

    #[test]
    fn protocol_local_state_ids_preserve_the_full_unsigned_domain() {
        let local = BlockStateRef::protocol_local(u32::MAX);
        assert_eq!(local.raw(), u32::MAX);
        assert!(matches!(local, BlockStateRef::ProtocolLocal(u32::MAX)));
    }

    #[test]
    fn level_event_data_keeps_raw_payload_bits_when_it_tags_a_state() {
        let raw = LevelEventData::Raw(-1);
        let tagged = LevelEventData::BlockState(BlockStateRef::protocol_local(u32::MAX));

        assert_eq!(raw.raw_i32(), -1);
        assert_eq!(tagged.raw_i32(), -1);
        assert_ne!(raw, tagged, "a state source must not collapse into raw event data");
    }
}

#[cfg(test)]
mod route_tests {
    use super::{ClientEvent, Route, route};
    use crate::{Difficulty, LevelEventData, PackedMessageSignature};
    use uuid::Uuid;
    use crate::{LookAnchor, PredictionSequence, Vec3, ids::Identifier, math::BlockPos};

    /// **The guard that protects the guard.**
    ///
    /// [`route`]'s whole value is that a new [`ClientEvent`] variant is a compile
    /// error (`E0004`) until it is routed. The obvious wrong way to silence that
    /// error is the one rustc itself suggests — `_ => todo!()`, or its friendlier
    /// cousin `_ => Route::NOWHERE`. Either one restores the exact wildcard that
    /// `#[non_exhaustive]` forces on every *other* consumer, deletes the guarantee
    /// in one line, and leaves a green tree behind. So the absence of a catch-all
    /// is asserted, not assumed.
    ///
    /// Reads this file's own source, in the spirit of
    /// `lodestone_shell`'s `no_wgsl_is_inlined_in_rust_sources`.
    #[test]
    fn route_has_no_catch_all_arm() {
        let source = include_str!("routing.rs");
        let body = source
            .split_once("pub fn route(event: &ClientEvent) -> Route {")
            .expect("route() must exist in this file")
            .1;
        let body = body
            .split_once("\n#[cfg(test)]")
            .map_or(body, |(before, _)| before);

        let found = catch_all_lines(body);
        assert!(
            found.is_empty(),
            "`route` has a catch-all arm ({found:?}), which restores the wildcard \
             `#[non_exhaustive]` forces everywhere else and deletes the compile \
             error that is this function's entire purpose. Write the arm instead — \
             `Route::NOWHERE` is a legal answer, but per variant and on purpose."
        );

        // The control, per `CLAUDE.md`: an assertion of an absence is worth only
        // as much as the evidence the detector fires. These are the two spellings
        // rustc's own `E0004` help text suggests.
        assert_eq!(
            catch_all_lines("        _ => Route::NOWHERE,\n").len(),
            1,
            "the detector must see a bare wildcard arm"
        );
        assert_eq!(
            catch_all_lines("        _ => todo!(),\n").len(),
            1,
            "the detector must see rustc's suggested `todo!()` wildcard"
        );
        // …and must not fire on the `{ .. }` in every ordinary arm, which also
        // contains a `..` before a `=>`.
        assert!(
            catch_all_lines("        ClientEvent::Ping { .. } => Route::NOWHERE,\n").is_empty(),
            "the detector must not read an ordinary struct pattern as a wildcard"
        );
    }

    /// The public routing document quotes both the remaining terminal islands
    /// and the exhaustive variant total. Keep those numbers derived from this
    /// source, not remembered after an otherwise-correct routing edit.
    #[test]
    fn the_island_count_in_the_docs_matches_this_source() {
        let source = include_str!("routing.rs");
        let route_body = source
            .split_once("pub fn route(event: &ClientEvent) -> Route {")
            .expect("route() must exist in this file")
            .1
            .split_once("\n#[cfg(test)]")
            .expect("the route tests must follow route()").0;

        let islands = route_body
            .match_indices("=> Route::NOWHERE,")
            .map(|(end, _)| {
                let arm_start = route_body[..end]
                    .rfind("\n        ClientEvent::")
                    .expect("every terminal NOWHERE arm must start with ClientEvent");
                route_body[arm_start..end].matches("ClientEvent::").count()
            })
            .sum::<usize>();

        let enum_source = include_str!("client.rs");
        let enum_body = enum_source
            .split_once("pub enum ClientEvent {")
            .expect("ClientEvent must remain an enum")
            .1
            .split_once("\n}\n")
            .expect("ClientEvent enum must end before the next item").0;
        let variants = enum_body
            .lines()
            .filter(|line| {
                line.strip_prefix("    ").is_some_and(|rest| {
                    !rest.starts_with("    ")
                        && rest
                            .chars()
                            .next()
                            .is_some_and(char::is_uppercase)
                })
            })
            .count();

        let counts = include_str!("../../../../docs/event-routing.md")
            .lines()
            .find(|line| line.contains("variants are currently `Route::NOWHERE`"))
            .and_then(|line| line.strip_prefix("**").and_then(|line| line.split_once("**")))
            .map(|(counts, _)| counts)
            .expect("event-routing.md must declare its island count");
        let (documented_islands, documented_variants) = counts
            .split_once(" of ")
            .map(|(islands, variants)| {
                (
                    islands.parse::<usize>().expect("documented island count"),
                    variants.parse::<usize>().expect("documented variant count"),
                )
            })
            .expect("event-routing.md count must read **N of M** variants");

        assert_eq!(documented_islands, islands);
        assert_eq!(documented_variants, variants);
    }



    /// Lines that would make [`route`]'s match exhaustive by accident.
    fn catch_all_lines(body: &str) -> Vec<&str> {
        body.lines()
            .map(str::trim)
            .filter(|line| {
                let stripped = line.strip_prefix('|').unwrap_or(line).trim_start();
                stripped == "_" || stripped.starts_with("_ =>") || stripped.starts_with("_ if")
            })
            .collect()
    }

    /// The reason [`Route`] is four booleans and not an enum, asserted rather than
    /// asserted-in-prose: one event is genuinely claimed by two routers at once.
    ///
    /// `ingest` folds the per-entity `Passengers`/`Vehicle` pair; `session` folds
    /// the local player's own `Riding` scalar. An enum would have forced one of
    /// those two folds to be dropped from the table on day one, and whichever half
    /// lost would have become an island with a green unit test behind it.
    #[test]
    fn one_event_can_be_claimed_by_two_routers_at_once() {
        let riding = ClientEvent::EntityPassengersChanged {
            vehicle_id: 1,
            passenger_ids: vec![2],
        };
        let r = route(&riding);
        assert!(r.ingest, "the component pair is per-entity ECS state");
        assert!(r.session, "the local player's own ride state is a session scalar");
        assert!(!r.is_island());
    }

    /// `Driver::emit` consumes a cookie request by producing the matching
    /// `CookieResponse` action before the event reaches any router. The route
    /// must record that client-internal consumer so this automatically answered
    /// event is not counted as an island.
    #[test]
    fn cookie_request_reaches_the_client_driver() {
        let event = ClientEvent::CookieRequested {
            key: Identifier::new("lodestone", "route-test").unwrap(),
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver answers cookie requests automatically");
        assert!(!r.is_island());
    }

    /// `Driver::emit` stores a cookie before the event reaches any router, so
    /// the route must record this client-internal consumer as well as the
    /// matching request's automatic response.
    #[test]
    fn cookie_store_reaches_the_client_driver() {
        let event = ClientEvent::CookieStored {
            key: Identifier::new("lodestone", "route-test").unwrap(),
            payload: vec![0xAA, 0xBB],
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver stores cookies automatically");
        assert!(!r.is_island());
    }

    /// `Driver::emit` answers a pushed resource pack before the event reaches
    /// any router. The route must record that client-internal response so the
    /// automatically answered event is not counted as an island.
    #[test]
    fn resource_pack_push_reaches_the_client_driver() {
        let event = ClientEvent::ResourcePackPushed {
            id: Uuid::nil(),
            url: "https://example.invalid/pack.zip".into(),
            hash: String::new(),
            required: false,
            prompt: None,
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver answers resource-pack pushes automatically");
        assert!(!r.is_island());
    }

    /// The shell's connection loop clears a pushed pack and its pending prompt
    /// before the event reaches generic `forward`. The route must record that
    /// existing consumer without requiring a `NetUpdate` arm for an event that
    /// has already been handled.
    #[test]
    fn resource_pack_pop_reaches_the_shell_interceptor() {
        let event = ClientEvent::ResourcePackPopped { id: Some(Uuid::nil()) };
        let r = route(&event);
        assert!(r.shell, "the connection loop clears popped server packs");
        assert!(
            r.shell_conditional,
            "the pop is consumed before generic forwarding"
        );
        assert!(!r.must_forward());
        assert!(!r.is_island());
    }

    /// A block-change acknowledgement is not merely protocol bookkeeping: it
    /// releases the placement predictor's pending snapshots after the server has
    /// applied their authoritative block writes. The shell route makes that
    /// lifecycle consumer visible to the exhaustive table.
    #[test]
    fn block_changed_ack_reaches_the_placement_prediction_consumer() {
        let r = route(&ClientEvent::BlockChangedAck {
            sequence: PredictionSequence::new(7),
        });
        assert!(r.shell, "the shell owns the placement prediction ledger");
        assert!(r.must_forward(), "the acknowledgement needs a NetUpdate arm");
        assert!(!r.is_island());
    }

    /// A stop packet has no ECS state to fold: only the shell can translate its
    /// name/category filters back into the opaque mixer handles that are making
    /// an already audible server sound play.
    #[test]
    fn sound_stopped_reaches_the_shell_playback_consumer() {
        let r = route(&ClientEvent::SoundStopped {
            sound: None,
            category: None,
        });
        assert!(r.shell, "the shell owns live mixer voices");
        assert!(r.must_forward(), "the filters need the NetUpdate relay");
        assert!(!r.is_island());
    }

    /// The rotation correction travels to the frame-thread pose owner. Its
    /// relative flags are meaningful only against that live pose, so the route
    /// must be the shell stream rather than a passive session record.
    #[test]
    fn player_rotation_set_reaches_the_shell_pose_consumer() {
        let r = route(&ClientEvent::PlayerRotationSet {
            y_rot: 20.0,
            relative_y: true,
            x_rot: -5.0,
            relative_x: false,
        });
        assert!(r.shell, "the shell owns the drawn and egress pose");
        assert!(r.must_forward(), "the correction needs a NetUpdate arm");
        assert!(!r.is_island());
    }

    /// A server-directed look is not session history: it immediately changes
    /// the local pose the camera and outgoing movement read.
    #[test]
    fn player_look_at_reaches_the_shell_pose_consumer() {
        let r = route(&ClientEvent::PlayerLookAt {
            from_anchor: LookAnchor::Eyes,
            target: Vec3::new(4.0, 70.0, -8.0),
            at_entity: None,
        });
        assert!(r.shell, "the shell owns the live camera and movement pose");
        assert!(r.must_forward(), "the look target needs a NetUpdate arm");
        assert!(!r.is_island());
    }

    /// The simulation distance is a server-reported scalar, distinct from the
    /// streamed-view radius. The session component owns it and the F3 panel
    /// reads that component; forwarding it to the loading-grid path would make
    /// that panel report a different server decision.
    #[test]
    fn simulation_distance_reaches_the_session_instrument_panel() {
        let r = route(&ClientEvent::SimulationDistanceChanged { distance: 11 });
        assert!(r.session, "the F3 panel reads the session scalar");
        assert!(!r.ingest && !r.shell && !r.client);
        assert!(!r.is_island());

        let combat = route(&ClientEvent::PlayerCombatEntered);
        assert!(combat.session, "combat enter reaches its session fold");
        assert!(!combat.is_island());
    }

    /// Public server data is a server-owned session fact. The F3 overlay reads
    /// the folded message, while the session retains the optional icon for a
    /// later in-session identity screen.
    #[test]
    fn server_data_reaches_the_session_hud_consumer() {
        let r = route(&ClientEvent::ServerDataReceived {
            motd: crate::Text::literal("Copper Canyon"),
            icon: Some(vec![0x89, 0x50, 0x4e, 0x47]),
        });
        assert!(r.session, "the F3 overlay reads the session record");
        assert!(!r.ingest && !r.shell && !r.client);
        assert!(!r.is_island());

        let combat = route(&ClientEvent::PlayerCombatEnded { duration_ticks: 240 });
        assert!(combat.session, "combat end reaches its session fold");
        assert!(!combat.is_island());
    }

    /// The mount-open packet has no companion `ScreenOpened`: it supplies both
    /// a window id and the inventory's column count itself. The menu session
    /// consumes that directly so the shell's existing open-menu screen can draw
    /// before ordinary container content arrives.
    #[test]
    fn mount_screen_open_reaches_the_session_menu_consumer() {
        let r = route(&ClientEvent::MountScreenOpened {
            container_id: 1,
            inventory_columns: 3,
            entity_id: 7,
        });
        assert!(r.session, "the menu session builds the announced mount screen");
        assert!(!r.ingest && !r.shell && !r.client);
        assert!(!r.is_island());

        assert!(
            route(&ClientEvent::Ping { id: 7 }).client,
            "control: an unrelated ping must stay out of the menu session"
        );
    }

    /// `Driver::emit` withdraws a deleted full signature from its pending
    /// acknowledgement tracker before the event reaches the shell. The route
    /// must record that client-internal consumer so the event is not counted as
    /// an island.
    #[test]
    fn deleted_chat_reaches_the_client_driver() {
        let event = ClientEvent::ChatMessageDeleted {
            signature: PackedMessageSignature::Full(vec![0; 256]),
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver withdraws deleted chat signatures");
        assert!(!r.is_island());
    }

    /// The three `HudState`-shaped islands this table found
    /// (`docs/event-routing.md`) are fixed: each now reaches `session`, and
    /// none is an island any more. This is the routing half only — the
    /// control that the *fold* actually runs lives in
    /// `lodestone_ecs::session`'s own tests.
    #[test]
    fn the_three_hudstate_islands_are_fixed() {
        let held_slot = ClientEvent::HeldSlotChanged { slot: 3 };
        let r = route(&held_slot);
        assert!(r.session, "held-slot is a local-player scalar");
        assert!(!r.is_island());

        let difficulty = ClientEvent::DifficultyChanged {
            difficulty: Difficulty::Hard,
            locked: false,
        };
        let r = route(&difficulty);
        assert!(r.session);
        assert!(!r.is_island());

        let block_destruction = ClientEvent::BlockDestruction {
            entity_id: 7,
            pos: BlockPos::new(1, 2, 3),
            progress: 4,
        };
        let r = route(&block_destruction);
        assert!(
            r.session,
            "a per-session collection keyed by breaking-entity id, the same \
             shape as the scoreboard/tab-list/boss-bar family"
        );
        assert!(!r.is_island());
    }

    /// `shell_conditional` exists for exactly this: `LevelEvent`'s arm in
    /// `net::forward` matches the literal sub-event `2001`, so every *other*
    /// level event legitimately reaches the terminal `_ =>` — and the
    /// `debug_assert!` there must not fire on it.
    #[test]
    fn a_guarded_shell_arm_is_not_required_to_forward() {
        let level = ClientEvent::LevelEvent {
            event: 1234,
            pos: BlockPos::new(0, 0, 0),
            data: LevelEventData::Raw(0),
            global: false,
        };
        let r = route(&level);
        assert!(r.shell, "the shell does consume one sub-event of this variant");
        assert!(r.shell_conditional);
        assert!(
            !r.must_forward(),
            "a guarded arm must not be asserted on, or every non-2001 level event \
             trips the assert in `net::forward`"
        );

        // The contrast that gives the flag meaning: an unconditional shell arm.
        let cleared = ClientEvent::TitlesCleared { reset_times: true };
        assert!(route(&cleared).must_forward());
    }

    /// The cache-radius update has an unconditional shell forward: it changes
    /// the live loading views, so this route must keep the forward assertion on.
    #[test]
    fn cache_radius_update_is_routed_to_the_shell() {
        let route = route(&ClientEvent::ChunkCacheRadiusChanged { radius: 7 });
        assert!(route.shell, "the shell owns the loading-view radius");
        assert!(route.must_forward(), "the radius must cross net::forward");
        assert!(!route.is_island());
    }

    /// The stream center is a separate scalar from the radius: the visible
    /// loading grid must query the columns around the server's center, even
    /// while the local player still names its previous chunk.
    #[test]
    fn cache_center_update_is_routed_to_the_shell() {
        let route = route(&ClientEvent::ChunkCacheCenterChanged { x: -4, z: 9 });
        assert!(route.shell, "the shell owns the loading-grid center");
        assert!(route.must_forward(), "the center must cross net::forward");
        assert!(!route.is_island());
    }

    #[test]
    fn camera_set_is_routed_to_the_rendered_camera_consumer() {
        let route = route(&ClientEvent::CameraSet { entity_id: 99 });
        assert!(route.shell, "the shell resolves the selected camera entity");
        assert!(route.must_forward(), "the id must cross net::forward");
        assert!(!route.is_island());
    }

    /// `Route::NOWHERE` means nothing anywhere — including nothing in
    /// `lodestone-client`, which is what the `client` flag is for. Without that
    /// flag `TimeChanged` would read as an island while in fact
    /// `SharedState::apply` folds it into `WorldTime` ahead of both predicates.
    #[test]
    fn the_client_flag_keeps_nowhere_honest() {
        let time = ClientEvent::TimeChanged {
            world_age: 1,
            time_of_day: 2,
        };
        let r = route(&time);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(r.client, "but `SharedState::apply` has its own arm for it");
        assert!(!r.is_island(), "so it is not an island");

        let transfer = ClientEvent::TransferRequested {
            host: "backend.example".into(),
            port: 25565,
        };
        let r = route(&transfer);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(r.client, "but `Driver::emit` records the transfer outcome");
        assert!(!r.is_island(), "so it is not an island");

        let custom_payload = ClientEvent::CustomPayload {
            channel: "minecraft:brand".parse().unwrap(),
            data: vec![6, b'r', b'o', b'u', b't', b'e', b'd'],
        };
        let r = route(&custom_payload);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(
            r.client,
            "SharedState publishes it to the production plugin event bus"
        );
        assert!(!r.is_island(), "the installed brand channel consumes it");

        let pong = ClientEvent::PongReceived { time: 1_700_000_123_456 };
        let r = route(&pong);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(r.client, "the client preserves the echoed ping timestamp");
        assert!(!r.is_island(), "the F3 latency reader consumes it");

        let cooldown = route(&ClientEvent::ItemCooldown {
            group: "minecraft:ender_pearl".parse().unwrap(),
            duration_ticks: 80,
        });
        assert!(cooldown.session, "the hotbar cooldown veil reads the session fold");
        assert!(!cooldown.is_island());

        let combat = route(&ClientEvent::PlayerCombatEnded { duration_ticks: 240 });
        assert!(combat.session, "the combat HUD reads the session fold");
        assert!(!combat.is_island());
        assert_eq!(Route::NOWHERE, Route::default());
    }
}
