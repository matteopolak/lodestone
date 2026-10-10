//! The play-state loops: the native and browser `serve_play` event loops, vitals ticking, stall watch and the client-loaded gate.

use super::*;

/// Converts wall-clock elapsed time into a tick count at vanilla's normal 20
/// TPS, for the `game_time` the periodic [`ServerProtocol::encode_set_time`]
/// broadcast carries.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn ticks_since(start: crate::tick::PlayTimerInstant) -> i64 {
    (start.elapsed().as_millis() / MILLIS_PER_TICK) as i64
}

/// A pass through [`serve_play`]'s `select!` shorter than this is not a stall —
/// one server tick. Passes at or above it are summed into
/// [`LoopStallWatch::unserviced`] and the worst one is remembered.
#[cfg(not(target_arch = "wasm32"))]
pub(super) const STALL_FLOOR: Duration = Duration::from_millis(MILLIS_PER_TICK as u64);

pub(super) const STALL_REPORT: Duration = Duration::from_millis(200);

/// How long one pass through [`serve_play`]'s `select!` took, and which arm took
/// it.
///
/// # Why the connection loop needs a watchdog at all
///
/// `select!` services exactly one arm per pass, so for the whole duration of that
/// arm this connection reads nothing and writes nothing — the socket is unserviced
/// even though the task is alive and the runtime is healthy. Every arm here awaits
/// something: the longest is `dispatch_play_packet`, which for a
/// `PlayerMoved` that crosses a chunk boundary awaits
/// `ViewTracker::build_batch` over a strip of `2r + 1` columns before returning
/// anything at all.
///
/// That matters twice over, and the second is why this is a type rather than a
/// `tracing::warn!`:
///
/// * **Diagnosis.** A latency symptom needs the *maximum*, not the mean, and it
///   needs to name *where*. An average over a session cannot see a single
///   multi-second gap, and a duration with no site attached gets attributed to
///   whatever the reader already suspected.
/// * **Correctness of the keep-alive timeout.** Vanilla's `keepConnectionAlive`
///   runs on the server tick while its reads happen on a Netty IO thread that
///   never blocks on world generation, so "15 seconds elapsed" and "15 seconds in
///   which the client could have been heard" are the same number there. Here they
///   are not. Denominating the timeout in wall clock therefore lets this server
///   kick a perfectly healthy client for a reply it never gave itself the chance
///   to read — the kick arrives as `disconnect.timeout`, which reads on the client
///   as the *client's* fault. [`unserviced`](Self::unserviced) is what makes the
///   deadline mean the same thing vanilla's does.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct LoopStallWatch {
    /// When the arm body currently running started. `None` between passes.
    ///
    /// **Set at the top of the arm body, not at the bottom of the previous one.**
    /// The interval between two passes is mostly time parked in `select!` waiting
    /// for a timer or a packet — the loop is *idle* there, not stalled, and the
    /// socket is being serviced by definition. Timing pass-to-pass measured
    /// exactly that idle wait: under a `start_paused` runtime, where the clock
    /// jumps straight to the next timer deadline whenever nothing is runnable, it
    /// reported the whole keep-alive interval as a stall and suppressed the
    /// timeout the test was gating. Only the arm body can starve the connection,
    /// so only the arm body is measured.
    pub(super) arm_start: Option<crate::tick::PlayTimerInstant>,
    /// The longest arm body observed, and the arm that owned it. `""` until one
    /// exceeds [`STALL_FLOOR`].
    pub(super) worst: Duration,
    pub(super) worst_arm: &'static str,
    /// Time this loop spent unable to service the socket, summed over every arm
    /// body past [`STALL_FLOOR`]. Reset by
    /// [`clear_unserviced`](Self::clear_unserviced) when a fresh keep-alive
    /// challenge is written, so it always answers "how much of *this* challenge's
    /// window did we eat".
    pub(super) unserviced: Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl LoopStallWatch {
    pub(super) fn new() -> Self {
        Self {
            arm_start: None,
            worst: Duration::ZERO,
            worst_arm: "",
            unserviced: Duration::ZERO,
        }
    }

    /// Opens a pass. Called as the first statement of every `select!` arm body.
    pub(super) fn enter(&mut self) {
        self.arm_start = Some(crate::tick::PlayTimerInstant::now());
    }

    /// Closes the pass that `arm` serviced. A no-op without a matching
    /// [`enter`](Self::enter), so an arm that returns early simply is not measured
    /// rather than being charged someone else's time.
    pub(super) fn pass(&mut self, arm: &'static str) {
        let Some(start) = self.arm_start.take() else {
            return;
        };
        let took = start.elapsed();
        if took < STALL_FLOOR {
            return;
        }
        self.unserviced += took;
        if took > self.worst {
            self.worst = took;
            self.worst_arm = arm;
        }
        if took >= STALL_REPORT {
            tracing::warn!(
                target: "lodestone_server::stall",
                arm,
                millis = took.as_millis() as u64,
                "connection loop serviced nothing for one pass",
            );
        }
    }

    pub(super) fn clear_unserviced(&mut self) {
        self.unserviced = Duration::ZERO;
    }

    /// The worst pass, for a log line on the way out. `None` before any pass has
    /// exceeded [`STALL_FLOOR`].
    pub(super) fn worst(&self) -> Option<(&'static str, Duration)> {
        (!self.worst_arm.is_empty()).then_some((self.worst_arm, self.worst))
    }
}

/// Serves a connection that has just reached [`State::Play`] until the client
/// disconnects.
///
/// This is where [`serve_connection`] hands off once the join sequence and
/// initial chunk view are out: everything here runs on the connection's own
/// schedule rather than strictly in response to one inbound packet —
/// * a server-initiated keep-alive, matching vanilla's fixed 15-second
///   interval and the same-length disconnect timeout
///   (vanilla's own common packet-listener; see the
///   `KEEP_ALIVE_INTERVAL` doc comment for why that is one interval, not two);
/// * a periodic time-of-day broadcast, matching vanilla's every-20-ticks
///   cadence (its own main server loop; see `TIME_SYNC_INTERVAL`);
/// * view streaming (chunk-cache-center, forget, and send) whenever a
///   [`ServerBound::PlayerMoved`] packet crosses into a new chunk column,
///   recentering the tracked view and sending/removing columns as needed;
///
/// all layered over the same entity-streaming pass used by the join sequence;
/// the pass runs on every inbound packet.
///
/// # Why this is a separate function, and why it forks on `wasm32`
///
/// Only this phase needs a real timer racing against the socket read, via
/// `tokio::select!` — and `tokio::time`'s timer is unavailable on `wasm32`
/// (see [`Connection::read_packet_timeout`](lodestone_net::Connection::read_packet_timeout)'s
/// own doc comment, the existing precedent for this split in this workspace).
/// The `wasm32` build below degrades to the old packet-driven-only loop
/// through the same [`dispatch_play_packet`] helper: it still answers
/// keep-alive echoes and streams the view reactively, it just never
/// *initiates* a keep-alive challenge or a periodic time broadcast, since
/// nothing can wake it when the client goes quiet. This is a real, documented
/// gap on that target, not a silent one.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure, or
/// [`ServerError::KeepAliveTimeout`] if the client does not echo a challenge
/// in time (native only — see above).
/// Vitals ticks a joining client may take to report it has loaded before the
/// server treats it as loaded anyway: 60 ticks, three seconds, the same bound
/// a vanilla server applies after a join or respawn.
pub(super) const CLIENT_LOADED_TIMEOUT_TICKS: u32 = 60;

/// Advances the player-loaded timeout by one vitals tick. A client that never
/// sends its loaded report (an older client, a bot, a stalled join) must not
/// hold the world's initial ticks forever. Any return to "not loaded" after a
/// respawn or dimension change restarts the wait, because the counter clears
/// whenever the client is loaded.
pub(super) fn tick_client_load_timeout(client_loaded: &mut bool, waited: &mut u32) {
    if *client_loaded {
        *waited = 0;
        return;
    }
    *waited += 1;
    if *waited >= CLIENT_LOADED_TIMEOUT_TICKS {
        *client_loaded = true;
        *waited = 0;
    }
}

pub(super) fn player_tick_ready(world: &crate::world_state::WorldStateHandle, client_loaded: bool) -> bool {
    if client_loaded {
        world.resume_initial_ticks();
    }
    client_loaded
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_play<T, P, S, E>(
    service: &crate::connection_service::ConnectionService,
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    // The primary source the connection joined from.  `source` may already
    // be a restored Nether/End sibling; portal return must still resolve back
    // through the primary world's sibling graph.
    home_source: SourceRef<'_, S>,
    entities: &E,
    mut state: State,
    initial_teleport_id: Option<i32>,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Keep the ticket guard owned by the connection task. Its `Drop` performs
    // player deregistration on disconnect, error, and cancellation; borrowing
    // it could let the guard outlive the task that owns the connection.
    player_ticket: Option<PlayerTicket>,
    // Captured with player registration so join-time swings remain visible.
    initial_swing_cursor: Option<u64>,
    // The guard withdraws this connection's `PLAYER_LOADING` and
    // `PLAYER_SIMULATION` tickets when the task exits. Move it with each
    // tracked-view recenter or radius change so residency follows the player.
    mut player_ticket_guard: PlayerTicketGuard,
    mut view: ViewTracker,
    username: String,
    // World spawn for death-screen respawn. It is computed during join and
    // reused here; `find_initial_spawn` may inspect up to 121 columns, so a
    // respawn does not repeat that search. Read by `apply_client_command`'s
    // `PERFORM_RESPAWN` arm.
    world_spawn: Vec3,
    mut chunks_sent: usize,
    // The deferred portion of the join view (`JOIN_PRESTREAM_RADIUS`) belongs
    // to this connection and is drained alongside socket reads and timers.
    mut join_stream: crate::join_scheduler::JoinChunkStream<S>,
    // Optional per-stage join timing shared with the generation workers.
    join_trace: Option<Arc<JoinTrace>>,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // Weather transitions published by the world tick loop's
    // `WeatherState`, drained on this same timer — see that arm's comment.
    weather: &WeatherFeed,
    // The night-skip vote (see `serve_connection_inner`'s
    // parameter comment). `dispatch_play_packet` records this connection's
    // player `lay_down`/`get_up` on it, and the `container_sync_tick` arm
    // feeds it the voter count from the shared `PlayerRegistry`.
    sleep_vote: &SleepVote,
    // Where this connection learns a night skip happened — drained
    // on `container_sync_tick` into a real `encode_set_time`, same timer as
    // the weather drain (see that arm's comment).
    sleep_feed: &SleepFeed,
    // Command dispatch and the authenticated caller identity for this
    // connection.
    commands: CommandSession,
    // The connection's server-authoritative advancement/statistics
    // store, built in `serve_connection_inner` (which already sent its
    // first-packet `update_advancements` at join). Mutable because both the
    // per-packet flush below and the `REQUEST_STATS` reply in
    // `dispatch_play_packet` award into / read from it.
    mut advancements: AdvancementManager,
    // The player key this connection's advancement/statistic
    // progress is stored under — the same `login_uuid` that built
    // `CommandSession`'s caller, resolved the same way (a nil uuid fails
    // closed: the connection tracks nothing).
    player_uuid: uuid::Uuid,
    // A host-shared snapshot fetched after this connection has completed
    // online authentication. `Some` permits announcement validation;
    // `None` deliberately preserves vanilla's service-unavailable degradation.
    profile_key_issuers: Option<lodestone_auth::MojangPublicKeys>,
    // Separate from issuer availability: whether the host requires a player
    // with no adopted session to sign chat.
    enforce_secure_profile: bool,
    // Shared world-border state read by the vitals timer when calculating
    // damage. The default feed represents the full-size static border; see
    // `serve_connection_inner`'s parameter comment.
    border: &BorderFeed,
    // Server-initiated resource pack pushes, drained on
    // `container_sync_tick` — same timer as the block-tick/explosion/weather
    // drains below, for the same reason: a push is published by the host (not
    // by an inbound packet) and needs this connection's own timer to notice.
    resource_packs: &ResourcePackPushFeed,
    // The connection's declared channel support (the filter the
    // broadcast drain below applies) and the shared wire-level registry whose
    // broadcast queue that drain reads. `client_channels` is owned, not
    // borrowed: it was created here for this connection and dies with it.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // The mode this connection joined in (`serve_connection_inner`'s own), owned
    // because the `change_game_mode` and `/gamemode` arms mutate it and nothing
    // outside this loop reads it.
    mut game_mode: GameMode,
    // The world's shared game rules, difficulty, and clock — the handle
    // `run_tick_loop` updates so every connection observes one state.
    world: &crate::world_state::WorldStateHandle,
    // Published once per iteration
    // of this function's own `select!` loop below — see
    // `crate::live_save::LiveSaveSlot`'s own doc comment for why a
    // continuously-refreshed mirror exists at all: `IntegratedServer::
    // shutdown`'s connection-task race drops this whole function's future
    // mid-`.await` on an ordinary singleplayer quit, so the disconnect-save
    // arm below (the `conn.read_packet()` returning `Ok(None)` branch) is
    // structurally unreachable on that path, and only this mirror survives
    // the cancellation to be read back afterwards.
    live_save: &crate::live_save::LiveSaveSlot,
    // The selected native backend's bounded locator session. Full player state
    // continues through the Anvil store above; this value only supplies the
    // native join/read and cancellation-safe locator save sidecar.
    native_player: Option<NativePlayerSession>,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<PendingBreak> = None;
    let mut pending_prediction_ack = connection_prediction::PendingPredictionAck::default();
    let mut pending_relights = PendingRelights::default();
    let mut pending_tick_updates = VecDeque::new();
    let mut detached_relight: Option<DetachedRelight> = None;
    let mut teleport_acknowledgements = initial_teleport_id.map(TeleportAcknowledgements::after_initial);
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut client_movement = ClientMovement::default();
    // A protocol with no player-loaded packet is loaded from the start.
    let mut client_loaded = !proto.sends_player_loaded();
    let mut client_load_wait = 0;
    player_tick_ready(world, client_loaded);
    let mut abilities = Abilities::for_mode(game_mode);
    // The rotation is stored alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment. Restore the native locator's bounded rotation when
    // present; the complete Anvil player record remains authoritative for all
    // other state.
    let native_player = native_player.as_ref();
    let mut player_rot: Option<Rotation> = native_player
        .and_then(NativePlayerSession::initial_rotation);
    // Resolve the per-player store once from the source. `player_uuid` is the
    // key for the stored file, and the loop reuses this handle for its saves.
    let player_store = player_store(source.get());
    let saved_player = player_store
        .as_ref()
        .and_then(|store| store.read(player_uuid).ok().flatten());
    let native_runtime = native_player.and_then(NativePlayerSession::runtime);
    // Preserve fields `crate::player_data` does not model—hunger, experience,
    // the ender chest, and the recipe book—in every save. This keeps a full
    // load/modify/save cycle lossless; see `PlayerData::preserved`.
    let mut preserved_player_fields: Vec<(String, lodestone_core::Nbt)> =
        saved_player.as_ref().map(|d| d.preserved.clone()).unwrap_or_default();
    let mut vitals = saved_player
        .as_ref()
        .map(|data| PlayerVitals::restored(data.health, data.air_supply))
        .or_else(|| {
            native_runtime.map(|runtime| {
                PlayerVitals::restored(runtime.health, runtime.air_supply)
            })
        })
        .unwrap_or_default();
    let mut fall = FallTracker::default();
    let mut inventory = saved_player
        .as_ref()
        .map(crate::player_data::PlayerData::to_inventory)
        .or_else(|| native_player.and_then(NativePlayerSession::inventory))
        .unwrap_or_default();
    republish_inventory(entities.players(), player_uuid, &inventory);
    // Send the book only after this connection's inventory exists: its
    // highlight flags are a per-connection acknowledgement state, while the
    // corpus itself is shared. The client uses the ids for `PLACE_RECIPE` too.
    apply(
        conn,
        &mut state,
        proto.encode_recipe_book_add(&recipe_book_snapshot(&inventory), true),
    )
    .await?;
    let mut open_container: Option<OpenContainer> = None;
    let mut open_merchant: Option<OpenMerchant> = None;
    let mut container_sync = ContainerSync::default();
    // This connection's last-known `ServerBound::PlayerInput` sprint flag —
    // see `apply_attack`'s own doc comment for the one thing it feeds
    // (the melee knockback sprint bonus).
    let mut sprinting = false;
    // This connection's last-known secondary-use (sneak) input. Block
    // placement uses it to bypass a clicked container, so shift-right-clicking
    // a chest can place a block beside it instead of opening the chest.
    let mut sneaking = false;
    let mut player_environment = crate::player_environment::PlayerEnvironment::default();
    // This connection's in-progress bow draw — see this parameter's
    // own comment on `dispatch_play_packet`.
    let mut bow_draw: Option<BowDraw> = None;
    // This connection's in-progress eat or drink — see `item_in_use` on
    // `dispatch_play_packet`. Finished by the per-tick arm below, not by a packet.
    let mut item_in_use: Option<ItemInUse> = None;
    // Window identifiers start at `0`; each open increments before use and
    // wraps with [`open_container_screen`]'s `% 100 + 1` rule.
    let mut next_window_id: i32 = 0;
    // This connection's composter roll stream — see
    // `COMPOSTER_BEHAVIOR_SEED` and `dispatch_play_packet`'s parameter comment.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    let mut bone_meal_rng = SpawnRng::new(BONE_MEAL_BEHAVIOR_SEED);
    // Restore experience from the player file alongside `vitals` and
    // `inventory`. `PlayerData::preserved` carries fields not modeled by this
    // crate, while this value keeps the live session and subsequent saves
    // consistent with the stored experience.
    let mut experience = saved_player
        .as_ref()
        .map(|data| data.experience)
        .or_else(|| native_runtime.map(|runtime| runtime.experience))
        .unwrap_or_default();
    // Experience-orb pickup starts with no delay, so the first nearby orb is
    // absorbed immediately; `collect_nearby_orbs` decrements this delay after
    // each pickup.
    let mut take_xp_delay: i32 = 0;
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let mut burn = crate::burning::BurnState::new();
    // The fire-contact ramp draws one value from the inclusive range `1..=3`.
    // Keep that draw on its own stream so standing in fire cannot shift which
    // roll a later block drop or composter insert sees.
    let mut burn_rng = SpawnRng::new(BURN_BEHAVIOR_SEED);
    // This connection's block-drop roll stream — see
    // `block_drops::BLOCK_DROPS_BEHAVIOR_SEED` and `dispatch_play_packet`'s
    // parameter comment for why it is separate from the composter's.
    let mut drops_rng = SpawnRng::new(crate::block_drops::BLOCK_DROPS_BEHAVIOR_SEED);
    // This connection's per-player respawn point, written by the bed
    // interaction handler. `apply_client_command` reads it when resolving a
    // death-screen respawn.
    // Restored from the player file, so a bed or anchor set in an earlier
    // session still applies.
    let mut respawn: Option<RespawnPoint> = crate::respawn::load(&preserved_player_fields);
    // Block position recorded when Bad Omen converts to Raid Omen. The
    // `vitals_tick` arm reads and clears it on the final omen tick.
    let mut raid_omen_position: Option<BlockPos> = None;
    // This connection's server-side entity id is the key the
    // night-skip vote stores this player under. A `PlayerRegistry` ticket
    // carries it where a registry exists (LAN, and every `serve_play` gate);
    // singleplayer has no registry, and `LOCAL_PLAYER_ENTITY_ID` is the same
    // constant the v770 encoder uses for the local player — see that const's
    // doc comment.
    let player_entity_id =
        player_ticket.as_ref().map_or(LOCAL_PLAYER_ENTITY_ID, |t| t.entity_id());
    if let (Some(registry), Some(rotation)) = (entities.players(), player_rot) {
        registry.set_rotation(player_entity_id, rotation);
    }
    // Chunk-batch flow-control gate (`ServerBound::ChunkBatchAcknowledged`,
    // see `send_view_update`'s own doc comment). It begins `true` for the
    // outstanding initial join batch; the first acknowledgement this loop
    // receives therefore clears that batch, while later acknowledgements cover
    // `recenter` or `set_view_radius` batches.
    //
    // The deferred join stream is finite and required for the loading screen,
    // so it is not gated on acknowledgements. The gate applies to reactive
    // streams that can emit a batch for every chunk boundary indefinitely.
    // Gating the join stream on a reply can stall fixtures whose protocol
    // implementation does not answer the acknowledgement.
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<PendingChunkBatch> = VecDeque::new();
    // Packet dispatch fills this queue; the loop drains it immediately after
    // the call returns to publish the message.
    let mut outgoing_chat: Vec<String> = Vec::new();
    // This connection's announced chat-signing session, if any —
    // `None` until a `chat_session_update` arrives, exactly like every other
    // per-connection `Option` this loop threads (`pending_keep_alive`,
    // `player_pos`'s rotation half). See `crate::chat_session`'s own doc.
    let mut chat_session: Option<crate::chat_session::ServerChatSession> = None;
    // This connection's read position in the shared chat log. Initialize it at
    // the log's *current end* so a joining player receives only messages
    // published during this session.
    let mut chat_cursor = entities.players().map_or(0, PlayerRegistry::chat_cursor);
    // The registration-time snapshot excludes older swings while retaining
    // events appended before this loop reaches its first drain.
    let mut swing_cursor = initial_swing_cursor.unwrap_or(0);
    // This connection's read position in the shared plugin-channel
    // broadcast queue. Started at 0 — unlike chat, a *broadcast* is
    // host-published state a new connection legitimately receives: a client
    // that announces `minecraft:brand` support at join is owed the brand
    // payload that was queued before it arrived.
    let mut plugin_channel_cursor: u64 = 0;
    let mut keep_alive_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + KEEP_ALIVE_INTERVAL,
        KEEP_ALIVE_INTERVAL,
    );
    // **`Delay`, not tokio's default `Burst`, and this is the difference between a
    // stall and a disconnect.** `Burst` makes up for missed ticks by firing them
    // back to back with no delay in between, so a pass that overran two intervals
    // resolves `tick()` twice in immediate succession: the first fires and finds no
    // challenge pending, writes one, and the second fires *in the same instant* and
    // finds it unanswered. The client is given literally zero time to reply and is
    // kicked with `disconnect.timeout` for a stall that was entirely ours. `Delay`
    // collapses the backlog into one tick and restarts the period from it, which is
    // what "check the client every 15 seconds" was always supposed to mean.
    keep_alive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // When the outstanding challenge was written. The timeout is measured from
    // here plus `LoopStallWatch::unserviced` rather than from the interval's own
    // cadence — see that type's doc comment.
    let mut keep_alive_sent_at = crate::tick::PlayTimerInstant::now();
    let mut watch = LoopStallWatch::new();
    // `interval_at`, not the bare `interval` constructor: `Interval::tick`'s
    // *first* call resolves immediately for an interval built with
    // `tokio::time::interval`, which would otherwise fire a redundant
    // game-time-only broadcast in the same instant as the join-time full
    // sync `serve_connection` just sent. Anchoring the first tick a full
    // `TIME_SYNC_INTERVAL` out avoids that, and mirrors `keep_alive_tick`
    // above for the same reason.
    let mut time_sync_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + TIME_SYNC_INTERVAL,
        TIME_SYNC_INTERVAL,
    );
    // Same reasoning as `time_sync_tick`: anchored one interval out so the
    // first vitals tick does not fire in the same instant as join.
    let mut vitals_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + VITALS_TICK_INTERVAL,
        VITALS_TICK_INTERVAL,
    );
    // Same reasoning again: anchored one interval out so the first sync
    // does not fire in the same instant as join (there is nothing open yet
    // at join, so this is cosmetic here, but consistent with every other
    // timer in this function).
    let mut container_sync_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + CONTAINER_SYNC_INTERVAL,
        CONTAINER_SYNC_INTERVAL,
    );
    // Anchor the first entity-stream tick one interval after the join state is
    // sent. An immediate tick would duplicate a diff over unchanged state.
    let mut entity_stream_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + ENTITY_STREAM_INTERVAL,
        ENTITY_STREAM_INTERVAL,
    );
    // Delay missed intervals so an overrun (a chunk strip or container click)
    // produces one streaming pass at a time. Bursting would issue back-to-back
    // diffs over state that cannot have changed between those passes.
    entity_stream_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let play_start = crate::tick::PlayTimerInstant::now();
    let mut next_keep_alive_id: i64 = 0;
    // The deferred join stream's own batch bookkeeping — see
    // `JOIN_STREAM_BATCH_COLUMNS`. `open` is whether a `begin_chunk_batch` has
    // been sent whose `end_chunk_batch` has not; `size` is how many columns are
    // inside it.
    let mut join_batch_open = false;
    let mut join_batch_size: i32 = 0;
    // This countdown is decremented on `vitals_tick` — see that arm.
    let mut player_save_countdown = PLAYER_SAVE_EVERY_VITALS_TICKS;

    // Send the restored inventory once so a rejoining player's items are
    // visible before any slot interaction; see `join_inventory_snapshot`.
    apply(conn, &mut state, join_inventory_snapshot(proto, &inventory)).await?;
    // Send the initial experience snapshot so the client's bar reflects the
    // restored values; see `join_experience`.
    apply(conn, &mut state, join_experience(proto, &experience)).await?;
    republish_experience(entities.players(), player_uuid, &experience);
    // Send the initial attribute snapshot so the client's derived armor display
    // reflects the restored inventory; see `join_attributes`.
    apply(conn, &mut state, join_attributes(proto, &inventory)).await?;

    let mut travel = connection_travel::TravelController::new(source);
    let mut dimension_reset: Option<connection_travel::DimensionReset> = None;
    let mut end_exit = connection_travel::EndExit::new(
        connection_travel::credits_seen_in(&preserved_player_fields),
    );
    let mut pending_join_encodes = PendingJoinEncodes::new();

    loop {
        service.admit_pass().await;
        if join_stream.is_done() && pending_join_encodes.is_empty() {
            if join_batch_open {
                apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                join_batch_open = false;
                join_batch_size = 0;
            }
            world.mark_initial_view_drained();
        }
        travel.promote();
        let home = home_source;
        let active_source = travel.source();
        let source = match active_source.as_ref() {
            Some(other) => SourceRef::Dimension(other),
            None => home,
        };
        let dimension_handles = dimension_scoped_handles(active_source.as_ref());
        let block_entities = dimension_handles.block_entities.as_ref().unwrap_or(block_entities);
        let block_ticks = dimension_handles.block_ticks.as_ref().unwrap_or(block_ticks);
        let active_entities = ActiveEntities::new(world, entities, source.dimension());
        let mobs = active_entities.runtime.as_ref().map_or(mobs, |runtime| runtime.mobs());
        let entities = &active_entities;
        let mut synchronous_relight = true;
        if let Some(coordinate) = pending_relights.front().filter(|_| pending_relights.ready()) {
            if let (Some(shared), Some(compute)) = (
                source.shared_arc(),
                proto.detached_light_compute()
                    .filter(|_| proto.retains_initial_column_light()),
            )
            {
                synchronous_relight = false;
                if detached_relight.is_none() {
                    if !view.delivered.contains(&coordinate) {
                        pending_relights.pop_front();
                    } else {
                        let dimension = source.dimension();
                        let cross_column = proto.uses_cross_column_light();
                        let batch_compute = proto.detached_resident_light_compute();
                        let batch = pending_relights.batch(&view.delivered, batch_compute.is_some());
                        let coordinates = batch.coordinates.clone();
                        let resident_only = !pending_relights.requires_generation(coordinate);
                        let work = move || {
                            if coordinates.len() > 1 {
                                if let Some(batch_compute) = batch_compute {
                                    let result = compute_detached_relight_batch(
                                        shared.as_ref(), &coordinates, dimension, batch_compute,
                                    );
                                    if !matches!(result, RelightBatchOutcome::Unsupported | RelightBatchOutcome::Deferred) {
                                        return result;
                                    }
                                }
                            }
                            match compute_detached_relight(
                                shared.as_ref(),
                                coordinate,
                                dimension,
                                cross_column,
                                resident_only,
                                compute,
                            ) {
                                Some(light) => RelightBatchOutcome::Committed(vec![(coordinate, light)]),
                                None => RelightBatchOutcome::Deferred,
                            }
                        };
                        if let Ok(handle) = crate::worldgen_dispatch::try_spawn(work) {
                            pending_relights.admit(&batch);
                            detached_relight = Some(DetachedRelight {
                                batch,
                                handle,
                            });
                        }
                    }
                }
            }
        }
        if let Some(error) = world.initial_seed_error() {
            return return_initial_seed_error(
                conn, proto, &mut state,
                if join_batch_open { Some(join_batch_size) } else { None }, error,
            ).await;
        }
        tokio::select! {
            error = world.wait_initial_seed_failure() => {
                return return_initial_seed_error(
                    conn, proto, &mut state,
                    if join_batch_open { Some(join_batch_size) } else { None }, error,
                ).await;
            }
            prepared = std::future::poll_fn(|cx| travel.poll_prepared(cx, player_pos)), if travel.is_preparing() => {
                watch.enter();
                let Some(prepared) = prepared? else {
                    watch.pass("travel_declined");
                    continue;
                };
                let dimension_changed = matches!(&prepared, connection_travel::PreparedTravel::Dimension { .. });
                if dimension_changed {
                    detached_relight = None;
                    if join_batch_open {
                        apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                        join_batch_open = false;
                        join_batch_size = 0;
                    }
                }
                if let Some(arrival) = connection_travel::commit(
                    &mut travel, prepared, conn, proto, source, home, &mut state, &mut view,
                    &mut join_stream, &mut pending_join_encodes, &mut pending_chunk_batches,
                    &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                    &mut teleport_acknowledgements, &mut player_pos, player_rot,
                    &mut client_movement, &mut fall, &mut client_loaded, game_mode, world,
                    &mut player_ticket_guard, &mut streamer, entities.players(), player_entity_id,
                ).await? {
                    if dimension_changed {
                        pending_break = None;
                        bow_draw = None;
                        item_in_use = None;
                        open_container = None;
                        open_merchant = None;
                    }
                    live_publish_player(
                        live_save, player_store.as_ref(), player_uuid, player_pos, player_rot,
                        world_spawn, &vitals, game_mode, &inventory, &experience,
                        &preserved_player_fields, arrival.dimension,
                    );
                    publish_native_player(
                        native_player, live_save, player_pos, player_rot, world_spawn,
                        arrival.dimension, game_mode, &vitals, &experience, &inventory,
                    );
                }
                watch.pass("travel_commit");
                continue;
            }
            _ = std::future::ready(()), if !pending_tick_updates.is_empty() => {
                watch.enter();
                send_pending_tick_block_updates(
                    conn,
                    proto,
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                    &mut pending_tick_updates,
                )
                .await?;
                watch.pass("tick_block_updates");
            }
            _ = std::future::ready(()), if synchronous_relight && pending_relights.ready() => {
                watch.enter();
                send_next_relight(
                    conn,
                    proto,
                    source.get(),
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                )
                .await?;
                watch.pass("tick_relight");
            }
            result = std::future::poll_fn(|cx| match detached_relight.as_mut() {
                Some(job) => std::future::Future::poll(std::pin::Pin::new(&mut job.handle), cx),
                None => std::task::Poll::Pending,
            }), if detached_relight.is_some() => {
                watch.enter();
                let batch = detached_relight
                    .take()
                    .expect("the completed relight has an owned handle")
                    .batch;
                match result {
                    Ok(RelightBatchOutcome::Committed(lights)) => {
                        let completed = lights.iter().map(|(coordinate, _)| *coordinate).collect::<Vec<_>>();
                        let remainder = RelightBatch {
                            coordinates: batch.coordinates.into_iter().filter(|coordinate| !completed.contains(coordinate)).collect(),
                            generation_required: batch.generation_required,
                        };
                        if !remainder.coordinates.is_empty() {
                            pending_relights.defer(&remainder);
                        }
                        send_committed_relights(
                            conn, proto, source.get(), &mut state, &view.delivered,
                            &mut pending_relights, lights,
                        ).await?;
                    }
                    Ok(RelightBatchOutcome::Failed(error)) => return Err(error.into()),
                    _ => pending_relights.defer(&batch),
                }
                watch.pass("tick_relight");
            }
            // Finish the source-aware encode that was started by the join arm.
            // The future owns only the source reference and the payload, so it
            // remains safe to leave it pending while packet/timer arms win the
            // race. Keeping this as a separate branch is what prevents a cold
            // neighbour admission from becoming an unserviceable connection
            // pass.
            encoded = std::future::poll_fn(|cx| pending_join_encodes.poll_next(cx)), if !pending_join_encodes.is_empty() => {
                watch.enter();
                let ((cx, cz), (incarnation, directive)) = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        return return_chunk_encode_error(
                            conn,
                            proto,
                            &mut state,
                            if join_batch_open { Some(join_batch_size) } else { None },
                            error,
                        )
                        .await;
                    }
                };
                if !view.needs_delivery((cx, cz), incarnation, directive.stage) {
                    watch.pass("join_encode_obsolete");
                    continue;
                }
                if !join_batch_open {
                    apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                    join_batch_open = true;
                    join_batch_size = 0;
                }
                if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                    continue;
                }
                if let Some(trace) = join_trace.as_ref() {
                    trace.mark("delivered", cx, cz);
                }
                chunks_sent += 1;
                join_batch_size += 1;
                if join_batch_size as usize >= JOIN_STREAM_BATCH_COLUMNS
                    || (join_stream.is_done() && pending_join_encodes.is_empty())
                {
                    apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                    join_batch_open = false;
                }
                watch.pass("join_encode");
            }
            // Stream the deferred join view (`JOIN_PRESTREAM_RADIUS`) while this
            // loop services digs, damage, and container clicks.
            //
            // Disabled once drained, so this is not a branch that returns `None`
            // forever. `select!` polls its branches in a random order, so a ready
            // packet is never starved by a ready column.
            //
            // Both `JoinChunkStream::next` arms are cancel-safe; a canceled
            // column must not silently leave a hole in the client's terrain.
            chunk = tokio::time::timeout(
                crate::join_scheduler::JOIN_STREAM_SERVICE_BUDGET,
                join_stream.next(source),
            ), if pending_join_encodes.can_admit() && !join_stream.is_done() => {
                watch.enter();
                let chunk = match chunk {
                    // A worker can legitimately take hundreds of milliseconds
                    // on a cold terrain column. Dropping only this borrowed
                    // future is safe: `ColumnPipeline::next` leaves the head
                    // handle in place until it has been emitted, so the next
                    // pass observes the same ordered result. Most importantly,
                    // the socket-read arm gets polled at least once per budget.
                    Err(_) => {
                        watch.pass("join_stream");
                        continue;
                    }
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(error)) => {
                        return return_chunk_encode_error(
                            conn,
                            proto,
                            &mut state,
                            if join_batch_open { Some(join_batch_size) } else { None },
                            error,
                        )
                        .await;
                    }
                };
                if let Some(((cx, cz), payload)) = chunk {
                    let Some(incarnation) = view.incarnation((cx, cz)) else {
                        continue;
                    };
                    let owned_source: Option<Arc<dyn ChunkSource>> = match source {
                        SourceRef::Shared(source) => {
                            let source: Arc<dyn ChunkSource> = source.clone();
                            Some(source)
                        }
                        SourceRef::Dimension(source) => Some(Arc::clone(source)),
                        SourceRef::Borrowed(_) => None,
                    };
                    if let Some(owned_source) = owned_source {
                        // Do not await source admission in this select arm. The
                        // owned future is polled by its own branch on subsequent
                        // passes, while this loop continues accepting packets and
                        // timers.
                        let trace = join_trace.clone();
                        pending_join_encodes.push(Box::pin(async move {
                            encode_column_owned(
                                proto,
                                owned_source,
                                cx,
                                cz,
                                trace,
                                payload,
                            )
                            .await
                            .map(|directive| ((cx, cz), (incarnation, directive)))
                        }));
                    } else {
                        // Borrowed sources exist for protocol-level controls and
                        // cannot outlive this loop iteration. They retain the
                        // original inline path; production integrated sources
                        // always take one of the owned arms above.
                        let directive = match encode_column(
                            proto,
                            source,
                            cx,
                            cz,
                            join_trace.as_deref(),
                            payload,
                        )
                        .await {
                            Ok(directive) => directive,
                            Err(error) => {
                                return return_chunk_encode_error(
                                    conn,
                                    proto,
                                    &mut state,
                                    if join_batch_open { Some(join_batch_size) } else { None },
                                    error,
                                )
                                .await;
                            }
                        };
                        if !join_batch_open {
                            apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                            join_batch_open = true;
                            join_batch_size = 0;
                        }
                        if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                            continue;
                        }
                        if let Some(trace) = join_trace.as_ref() {
                            trace.mark("delivered", cx, cz);
                        }
                        chunks_sent += 1;
                        join_batch_size += 1;
                        if join_batch_size as usize >= JOIN_STREAM_BATCH_COLUMNS
                            || join_stream.is_done()
                        {
                            apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                            join_batch_open = false;
                        }
                    }
                }
                watch.pass("join_stream");
            }
            packet = conn.read_packet() => {
                watch.enter();
                let Some((packet_id, payload)) = packet? else {
                    // Persist the disconnect snapshot while the loop's state is
                    // still intact. The periodic save on `vitals_tick` covers
                    // crashes, cancellation, and propagated errors.
                    persist_player(
                        player_store.as_ref(),
                        player_uuid,
                        player_pos,
                        player_rot,
                        world_spawn,
                        &vitals,
                        game_mode,
                        &inventory,
                        &experience,
                        &preserved_player_fields,
                        source.dimension(),
                    );
                    persist_native_player(
                        native_player,
                        player_pos,
                        player_rot,
                        world_spawn,
                        source.dimension(),
                        game_mode,
                        &vitals,
                        &experience,
                        &inventory,
                    );
                    return Ok(ServeSummary { username, chunks_sent, inventory });
                };
                let pending_keep_alive_before_packet = pending_keep_alive;
                {
                    let dispatch = std::pin::pin!(dispatch_play_packet(
                        conn,
                        proto,
                        source,
                        home,
                        &mut state,
                        (!matches!(source, SourceRef::Borrowed(_))
                            && proto.retains_initial_column_light()
                            && proto.detached_light_compute().is_some())
                            .then_some(&mut pending_relights),
                        &mut view,
                        &player_ticket_guard,
                        &mut pending_keep_alive,
                        &mut pending_break,
                        &mut pending_prediction_ack,
                        &mut teleport_acknowledgements,
                        &mut player_pos,
                        &mut client_movement,
                        &mut player_rot,
                        &mut fall,
                        &mut vitals,
                        &mut burn,
                        world,
                        &mut inventory,
                        block_entities,
                        &mut open_container,
                        &mut open_merchant,
                        &mut container_sync,
                        &mut next_window_id,
                        mobs,
                        &mut sprinting,
                        &mut sneaking,
                        &mut awaiting_chunk_batch_ack,
                        &mut pending_chunk_batches,
                        // The live stream this loop's own `select!` branch drains, lent
                        // for the length of the call so a chunk-boundary crossing can
                        // enqueue into it rather than generate inline. Safe to reborrow
                        // here: `select!` drops every branch future before running the
                        // handler, which is the same property the `reprioritise` call
                        // further down this arm already relies on.
                        Some(&mut join_stream),
                        &commands,
                        &mut advancements,
                        player_uuid,
                        profile_key_issuers.as_ref(),
                        enforce_secure_profile,
                        &mut outgoing_chat,
                        &mut chat_session,
                        entities.players(),
                        block_ticks,
                        resource_packs,
                        &mut client_loaded,
                        &mut composter_rng,
                        &mut bone_meal_rng,
                        &mut experience,
                        &mut effects,
                        &mut drops_rng,
                        client_channels,
                        plugin_channels,
                        &mut game_mode,
                        &mut abilities,
                        &mut respawn,
                        sleep_vote,
                        border,
                        player_entity_id,
                        &username,
                        world_spawn,
                        // This loop counts ticks from `play_start` for the
                        // time-of-day broadcast; the break validator reads that
                        // monotonic clock, so a dig's start and stop use one counter.
                        Some(u64::try_from(ticks_since(play_start)).unwrap_or(0)),
                        &mut bow_draw,
                        &mut item_in_use,
                        &mut dimension_reset,
                        &mut end_exit,
                        packet_id,
                        &payload,
                    ));
                    crate::worldgen_progress::measure_polls(
                        WorldgenTimingPhase::ConnectionDispatch, dispatch,
                    ).await?;
                }
                player_tick_ready(world, client_loaded);
                if let Some(id) = pending_keep_alive_before_packet
                    && pending_keep_alive.is_none()
                {
                    tracing::debug!(
                        target: "lodestone_keepalive",
                        id,
                        round_trip_millis = keep_alive_sent_at.elapsed().as_millis() as u64,
                        "server accepted keep-alive response"
                    );
                }
                republish_inventory(entities.players(), player_uuid, &inventory);
                if end_exit.take_respawn() {
                    dimension_reset = connection_travel::perform_respawn(
                        conn, proto, &mut state, home.get(), source.get(), &mut respawn, world_spawn, game_mode,
                        &mut teleport_acknowledgements, block_ticks, true,
                    ).await?;
                }
                // A bed, anchor or `/spawnpoint` this packet set (or a respawn
                // that cleared the point) reaches the next save through the
                // preserved fields.
                crate::respawn::store(&mut preserved_player_fields, respawn.as_ref());
                if let Some(connection_travel::DimensionReset { target, route }) = dimension_reset.take() {
                    detached_relight = None;
                    // The respawn's own dimension: the home one, the one already
                    // being viewed, or a sibling reached through the world.
                    let (destination, arrival_dimension) = match &route {
                        crate::respawn::Route::Home => (home.get(), home.dimension()),
                        crate::respawn::Route::Current => (source.get(), source.dimension()),
                        crate::respawn::Route::Sibling(other) => (
                            other.as_ref(),
                            other.dimension().unwrap_or(crate::dimension::Dimension::Overworld),
                        ),
                    };
                    let ticket_transfer = connection_travel::prepare_ticket_transfer(
                        &player_ticket_guard, home.get(), destination, target, view.radius,
                        world.simulation_distance(),
                    )?;
                    if join_batch_open {
                        apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                        join_batch_open = false;
                        join_batch_size = 0;
                    }
                    connection_travel::reset_stream(
                        conn, proto, &mut state, target, &mut view, &mut join_stream,
                        &mut pending_join_encodes, &mut pending_chunk_batches,
                        &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                    ).await?;
                    for directive in streamer.reset_dimension(proto) {
                        apply(conn, &mut state, directive).await?;
                    }
                    connection_travel::finish_ticket_transfer(
                        &mut player_ticket_guard, ticket_transfer, source.get(), destination,
                    );
                    connection_travel::reset_player(
                        target, arrival_dimension, &mut player_pos, &mut client_movement,
                        &mut fall, &mut client_loaded, proto.sends_player_loaded(), world, entities.players(), player_entity_id,
                    );
                    match route {
                        crate::respawn::Route::Home => travel.stage(connection_travel::Destination::Home),
                        crate::respawn::Route::Current => {}
                        crate::respawn::Route::Sibling(other) => {
                            travel.stage(connection_travel::Destination::Dimension(other));
                        }
                    }
                    pending_break = None;
                    bow_draw = None;
                    item_in_use = None;
                    open_container = None;
                    open_merchant = None;
                    live_publish_player(
                        live_save, player_store.as_ref(), player_uuid, player_pos, player_rot,
                        world_spawn, &vitals, game_mode, &inventory, &experience,
                        &preserved_player_fields, arrival_dimension,
                    );
                    publish_native_player(
                        native_player, live_save, player_pos, player_rot, world_spawn,
                        arrival_dimension, game_mode, &vitals, &experience, &inventory,
                    );
                    watch.pass("dimension_reset");
                    continue;
                }
                // Flush advancement changes caused by the packet just granted.
                // Advancement producers are packet-driven, so flushing after
                // dispatch avoids a separate timer. `flush_dirty` returns
                // `None` when nothing changed, keeping the common case packet-free.
                if let Some(update) = advancements.flush_dirty(player_uuid, true) {
                    apply(conn, &mut state, proto.encode_update_advancements(&update)).await?;
                }
                // Republish this player's chat for every connection, including
                // this one. The registry holds the sender and recipient views.
                //
                // With no registry — singleplayer, where `open_in_memory`
                // builds no `PlayerRegistry` at all — there is nobody else to
                // broadcast to, so the message is echoed straight back to its
                // sender. That still matches vanilla, whose broadcast loop
                // includes the sender; it is the same rule with a roster of
                // one, not a special case.
                for message in outgoing_chat.drain(..) {
                    let line = ChatLine {
                        sender: username.clone(),
                        message,
                    };
                    match entities.players() {
                        Some(registry) => registry.say(&line.sender, &line.message),
                        None => {
                            apply(conn, &mut state, proto.encode_system_chat(&line.rendered()))
                                .await?;
                        }
                    }
                }
                // Republish this player's position for other connections.
                // Read the value updated by packet dispatch so the registry
                // receives movement without another state parameter.
                if let (Some(ticket), Some(registry), Some((x, y, z))) = (
                    player_ticket.as_ref(),
                    entities.players(),
                    player_pos,
                ) {
                    registry.set_position(ticket.entity_id(), Vec3::new(x, y, z));
                    registry.set_on_ground(ticket.entity_id(), client_movement.on_ground);
                }
                // Re-key pending join columns after movement or facing changes:
                // distance from the player's current column comes first, then
                // the view cone (`join_scheduler::priority_key`). Read back the
                // position and rotation updated by packet dispatch; unchanged
                // center and quantized yaw leave the queue untouched.
                if !join_stream.is_done() {
                    if let Some((x, _, z)) = player_pos {
                        join_stream.reprioritise(
                            (
                                (x / 16.0).floor() as i32,
                                (z / 16.0).floor() as i32,
                            ),
                            player_rot.map(|rotation| rotation.yaw),
                        );
                    }
                }
                // Collect drops at the player's current position.
                // Here, and not in `dispatch_play_packet`, for the same reason
                // the position republish above is here: `player_pos` has just
                // been updated by whichever movement packet arrived, and the
                // `stream_pass` below will carry the resulting
                // `REMOVE_ENTITIES` for the collected item in the very same
                // pass — so the item vanishes from the world and appears in the
                // hotbar together rather than a packet apart.
                if let Some((x, y, z)) = player_pos {
                    let pickups = collect_nearby_items(
                        mobs,
                        &mut inventory,
                        Vec3::new(x, y, z),
                        &mut advancements,
                        player_uuid,
                        world.time().game_time.saturating_mul(50),
                        matches!(game_mode, GameMode::Creative),
                    );
                    // **The pickup animation, and it must go out before the
                    // `stream_pass` below.** That pass derives `REMOVE_ENTITIES`
                    // from the removal `collect_nearby_items` just performed, and
                    // the client keeps the item entity alive precisely so it can
                    // interpolate it toward the collector — it removes the entity
                    // itself when the animation completes. Announce the take after
                    // the removal has been broadcast and the client has nothing
                    // left to animate, so the packet is present, correct, and
                    // invisible.
                    for take in &pickups.takes {
                        apply(
                            conn,
                            &mut state,
                            proto.encode_take_item_entity(
                                take.item_entity_id,
                                // Self-facing (sent only to `conn`): the collector
                                // must be `LOCAL_PLAYER_ENTITY_ID`, matching this
                                // rule's other call sites — see `publish_health`'s.
                                LOCAL_PLAYER_ENTITY_ID,
                                take.amount,
                            ),
                        )
                        .await?;
                    }
                    for native in pickups.changed {
                        // Window `0`, menu slot for this native index. `state_id`
                        // `0` matches every other server-initiated slot write in
                        // this file (`apply_container_clicked` applies a click's
                        // diff verbatim and never validates a stale id).
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                &mut state,
                                proto.encode_container_slot(
                                    0,
                                    0,
                                    menu_slot,
                                    inventory.native(native),
                                ),
                            )
                            .await?;
                        }
                    }
                    // Experience orbs, on the same movement cadence and for the same
                    // reason: the `stream_pass` below carries the `REMOVE_ENTITIES` for
                    // an orb that was fully absorbed, so it vanishes and the bar moves
                    // together rather than a pass apart.
                    //
                    // `TAKE_ITEM_ENTITY` with amount `1` is vanilla's own
                    // `player.take(this, 1)` — the same packet an item pickup uses, and
                    // what drives the client's absorption animation and pickup sound.
                    // Sent *before* the removal for the item path's reason.
                    if let Some(absorbed) = collect_nearby_orbs(
                        mobs,
                        Vec3::new(x, y, z),
                        &mut experience,
                        &mut take_xp_delay,
                    ) {
                        apply(
                            conn,
                            &mut state,
                            proto.encode_take_item_entity(
                                absorbed.orb_entity_id,
                                // Self-facing, per the item-pickup call site above.
                                LOCAL_PLAYER_ENTITY_ID,
                                1,
                            ),
                        )
                        .await?;
                        // The mutation-then-send rule `join_experience`'s doc states: a
                        // `give_points` with no `set_experience` behind it is the exact
                        // shape that made the XP bar invisible in the first place.
                        debug_assert!(absorbed.points > 0, "an absorbed orb pays out points");
                        republish_experience(entities.players(), player_uuid, &experience);
                        apply(
                            conn,
                            &mut state,
                            proto.encode_set_experience(
                                experience.progress(),
                                experience.level(),
                                experience.total(),
                            ),
                        )
                        .await?;
                    }
                }
                // Republish facing separately because rotation and position
                // arrive on different packets; requiring both values would
                // omit a player who turns without moving.
                if let (Some(ticket), Some(registry), Some(rotation)) =
                    (player_ticket.as_ref(), entities.players(), player_rot)
                {
                    registry.set_rotation(ticket.entity_id(), rotation);
                }
                republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }
                // The arm that owns the view update, and therefore the longest pass
                // this loop has. A `PlayerMoved` that crosses a chunk boundary
                // awaits a whole strip of columns inside `dispatch_play_packet`.
                watch.pass("read_packet");
            }

            // The server-driven streaming pass. Identical body to the one at
            // the tail of the `read_packet` arm above, which stays where it is:
            // that one has to run *after* the packet it just handled (a move
            // republishes this player's position into the registry, a dig
            // removes an entity), so folding the two into this timer would
            // delay every such consequence by up to a tick. This arm is what
            // covers the other direction — everything that changes while this
            // connection says nothing. See [`ENTITY_STREAM_INTERVAL`].
            _ = entity_stream_tick.tick() => {
                watch.enter();
                republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }
                watch.pass("entity_stream_tick");
            }

            _ = keep_alive_tick.tick() => {
                watch.enter();
                // **Forgive an unanswered challenge only when this loop had no
                // serviced time at all to hear the answer in.** A reply needs
                // milliseconds of serviced time to be read, so the only honest
                // excuse is a stall that swallowed the *whole* window — hence the
                // comparison against a full interval rather than against any stall
                // at all. Without this the server kicks a client that answered
                // promptly, because the reply sat unread in the receive buffer while
                // an arm body was awaiting terrain; `disconnect.timeout` then reads
                // on the client as the client's own fault. See `LoopStallWatch`.
                //
                // Bounded, which a wall-clock deadline extension would not be: the
                // excuse has to be re-earned in full inside every window, since
                // `clear_unserviced` zeroes the accounting each time a challenge is
                // written. With no stall the behaviour is identical to having no
                // clause here, which is why the timeout gates are untouched.
                if pending_keep_alive.is_some() && watch.unserviced >= KEEP_ALIVE_INTERVAL {
                    tracing::warn!(
                        target: "lodestone_server::stall",
                        unserviced_millis = watch.unserviced.as_millis() as u64,
                        waited_millis = keep_alive_sent_at.elapsed().as_millis() as u64,
                        worst_arm = watch.worst().map(|(arm, _)| arm).unwrap_or(""),
                        worst_millis = watch.worst().map(|(_, d)| d.as_millis() as u64).unwrap_or(0),
                        "keep-alive unanswered, but this loop ate the whole window — not kicking",
                    );
                    keep_alive_sent_at = crate::tick::PlayTimerInstant::now();
                    watch.clear_unserviced();
                    watch.pass("keep_alive_tick");
                    continue;
                }
                if pending_keep_alive.is_some() {
                    // Tell the client why the connection is closing.
                    //
                    // The write is best-effort: a peer that stopped answering
                    // keep-alives may well be gone, so a failed write must still
                    // produce `KeepAliveTimeout` rather than masking it as a
                    // transport error. That is what `let _ =` buys here, and it is
                    // the one place in this loop where dropping an error is right.
                    // Built before the `&mut state` borrow, not inline in the
                    // call: `apply` takes `&mut state` and `encode_disconnect`
                    // reads it, which the borrow checker rejects as an argument
                    // expression.
                    // **Who initiated the disconnect, in the log, at the moment it
                    // happens.** "Timed out" is the *server's* verdict, and this is
                    // the only producer of it in the workspace; a reader who sees the
                    // string on a client has no way to tell it from a client-side
                    // give-up without this line. The stall figures come with it
                    // because they are what distinguishes a client that stopped
                    // answering from a server that stopped listening.
                    tracing::warn!(
                        target: "lodestone_server::stall",
                        waited_millis = keep_alive_sent_at.elapsed().as_millis() as u64,
                        worst_arm = watch.worst().map(|(arm, _)| arm).unwrap_or(""),
                        worst_millis = watch.worst().map(|(_, d)| d.as_millis() as u64).unwrap_or(0),
                        "server is disconnecting this client for an unanswered keep-alive",
                    );
                    let directive = proto.encode_disconnect(state, &timeout_reason());
                    let _ = apply(conn, &mut state, directive).await;
                    return Err(ServerError::KeepAliveTimeout);
                }
                next_keep_alive_id += 1;
                pending_keep_alive = Some(next_keep_alive_id);
                keep_alive_sent_at = crate::tick::PlayTimerInstant::now();
                watch.clear_unserviced();
                tracing::debug!(
                    target: "lodestone_keepalive",
                    id = next_keep_alive_id,
                    "server sent keep-alive challenge"
                );
                apply(conn, &mut state, proto.encode_keep_alive(next_keep_alive_id)).await?;
                // Refresh the world-spawn ticket from the connection's
                // keep-alive timer. Compatibility entry points use a detached
                // ticket handle, so this call is a no-op there.
                player_ticket_guard.refresh_world_spawn();
                watch.pass("keep_alive_tick");
            }

            _ = time_sync_tick.tick() => {
                watch.enter();
                // **Use the shared world clock.** Encode its long `game_time`
                // and optional day-time so every connection observes the same
                // authoritative sky clock. Sending day-time also lets a frozen
                // time rule keep the client's sun anchored.
                let time = world.time();
                apply(
                    conn,
                    &mut state,
                    proto.encode_set_time(time.game_time, Some(time.day_time)),
                )
                .await?;
                watch.pass("time_sync_tick");
            }

            _ = vitals_tick.tick() => {
                watch.enter();
                if let Some(sequence) = pending_prediction_ack.take() {
                    apply(conn, &mut state, proto.encode_block_changed_ack(sequence)).await?;
                }
                tick_client_load_timeout(&mut client_loaded, &mut client_load_wait);
                if !player_tick_ready(world, client_loaded) {
                    watch.pass("vitals_waiting_for_player_loaded");
                    continue;
                }
                // Count periodic saves in 50 ms vitals ticks rather than wall
                // time. This avoids unsupported wall-clock calls in wasm32.
                // Periodic saves cover disconnect, cancellation, and crash paths
                // that cannot run the disconnect cleanup.
                player_save_countdown = player_save_countdown.saturating_sub(1);
                if player_save_countdown == 0 {
                    player_save_countdown = PLAYER_SAVE_EVERY_VITALS_TICKS;
                    persist_player(
                        player_store.as_ref(),
                        player_uuid,
                        player_pos,
                        player_rot,
                        world_spawn,
                        &vitals,
                        game_mode,
                        &inventory,
                        &experience,
                        &preserved_player_fields,
                        source.dimension(),
                    );
                }

                // Advance deferred block breaks on each 50 ms vitals tick. This
                // completes hold-and-release digs whose stop packet leaves
                // progress below `0.7`. Skip the work when the block is air;
                // another world update may have removed it, and re-breaking air
                // would emit duplicate drops.
                if let Some(dig) = pending_break.filter(|dig| {
                    dig.deferred_break_ready(
                        Some(u64::try_from(ticks_since(play_start)).unwrap_or(0)),
                    )
                }) {
                    // A timer probe must not turn a cold deferred break into
                    // synchronous generation. Keep the pending break intact;
                    // the next tick retries after the view stream admits it.
                    if let Some(current) = resident_block_state(
                        source.get(),
                        dig.pos.x,
                        dig.pos.y,
                        dig.pos.z,
                    ) {
                        pending_break = None;
                        if current.block() != Block::Air {
                            let allowed = adjudicate_block_break(
                                conn,
                                proto,
                                source.get(),
                                &mut state,
                                world,
                                dig.pos,
                                player_uuid,
                            )
                            .await?;
                            if allowed {
                                destroy_block(
                                conn,
                                proto,
                                source.get(),
                                &mut state,
                                (!matches!(source, SourceRef::Borrowed(_))
                                    && proto.retains_initial_column_light()
                                    && proto.detached_light_compute().is_some())
                                    .then_some(&mut pending_relights),
                                block_entities,
                                &mut open_container,
                                &mut container_sync,
                                mobs,
                                &mut drops_rng,
                                inventory.selected_item(),
                                block_ticks,
                                player_uuid,
                                !matches!(game_mode, GameMode::Creative) && world.block_drops(),
                                world.block_drops(),
                                dig.pos,
                                &mut advancements,
                                (!matches!(game_mode, GameMode::Creative)).then_some(&mut vitals),
                                )
                                .await?;
                            }
                        }
                    }
                }

                // Vanilla's own update-using-item routine: a consume ends on the server's
                // own clock, not on a packet — the client sends nothing when a
                // steak finishes, so without this arm every bite starts and none
                // ever lands. Read against `MobSim`'s tick counter because that is
                // the clock `apply_use_item` stamped `finish_tick` from; mixing it
                // with this loop's `ticks_since(play_start)` would compare two
                // unrelated counters.
                if let Some(started) = item_in_use.clone() {
                    let now = mobs.with(|sim| sim.tick_count());
                    // The periodic eating/drinking sound —
                    // vanilla's own on-use-tick → emit-particles-and-sounds chain.
                    // **Sound only**: the crumbs are the client's own prediction,
                    // because the server-side particle hook is a no-op, and
                    // the sound is *only* the server's, because
                    // the client-side seeded-sound hook drops the particle-only call.
                    // Splitting one game action across the two sides looks like
                    // an omission on each of them; it is the whole mechanism.
                    if now < started.finish_tick
                        && let Some(pos) = player_pos
                        && let Some(consumable) =
                            lodestone_game::consumable::consumable_for_item(&started.item)
                    {
                        let remaining =
                            u32::try_from(started.finish_tick - now).unwrap_or(u32::MAX);
                        let already = started.last_effect_remaining == Some(remaining);
                        if !already
                            && lodestone_game::consumable::should_emit_consume_effects(
                                consumable.consume_ticks,
                                remaining,
                            )
                        {
                            let seed = i64::from(drops_rng.next_int(i32::MAX));
                            let roll = drops_rng.next_f32();
                            if let Some(effect) = crate::effects::item_consumed_tick(
                                &started.item,
                                Vec3::new(pos.0, pos.1, pos.2),
                                roll,
                                seed,
                            ) {
                                block_ticks.publish_effect_except(player_uuid, effect);
                            }
                        }
                        // Latched whether or not this tick emitted, so the guard
                        // tracks "already looked at this tick" rather than "already
                        // played", which is the property that makes it idempotent.
                        if let Some(live) = item_in_use.as_mut() {
                            live.last_effect_remaining = Some(remaining);
                        }
                    }
                    if now >= started.finish_tick {
                        item_in_use = None;
                        if let Some((native, remainder)) =
                            finish_consuming(&mut inventory, &mut vitals, &started, game_mode)
                        {
                            // Vanilla's own food-properties on-consume routine: the consumable sound again,
                            // louder and on `NEUTRAL`, plus the burp. Both are on the
                            // **food** component, so they are published here — inside
                            // the `finish_consuming` success arm, which already
                            // required a food — rather than beside the periodic sound
                            // above, which is `Consumable`'s and fires for potions too.
                            if let Some(pos) = player_pos {
                                let at = Vec3::new(pos.0, pos.1, pos.2);
                                let seed = i64::from(drops_rng.next_int(i32::MAX));
                                if let Some(effect) = crate::effects::item_consume_finished(
                                    &started.item,
                                    at,
                                    drops_rng.next_f32(),
                                    seed,
                                ) {
                                    block_ticks.publish_effect(effect);
                                }
                                block_ticks.publish_effect(crate::effects::player_burped(
                                    at,
                                    drops_rng.next_f32(),
                                    i64::from(drops_rng.next_int(i32::MAX)),
                                ));
                            }
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            // The food bar frame carries health, food, and
                            // saturation together, so resend all three values
                            // whenever one of them changes.
                            apply(
                                conn,
                                &mut state,
                                proto.encode_set_health(
                                    vitals.health(),
                                    vitals.food().food_level(),
                                    vitals.food().saturation(),
                                ),
                            )
                            .await?;

                            // Apply item-specific effects after the food-bar
                            // frame. These use separate status-effect packets,
                            // not health-bar fields; the supported item set is
                            // defined by `food_consume_effects`.
                            for grant in crate::mob_effects::food_consume_effects(&started.item) {
                                if drops_rng.next_f32() >= grant.probability {
                                    continue;
                                }
                                effects.apply(grant.effect_id, grant.duration, grant.amplifier);
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_update_mob_effect(
                                        LOCAL_PLAYER_ENTITY_ID,
                                        grant.effect_id,
                                        grant.amplifier,
                                        grant.duration,
                                        false,
                                        true,
                                        true,
                                        false,
                                    ),
                                )
                                .await?;
                            }
                            if crate::mob_effects::removes_poison_on_consume(&started.item)
                                && effects.remove("minecraft:poison")
                            {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_remove_mob_effect(
                                        LOCAL_PLAYER_ENTITY_ID,
                                        "minecraft:poison",
                                    ),
                                )
                                .await?;
                            }
                        } else if let Some((native, remainder)) = finish_drinking_ominous_bottle(
                            &mut inventory,
                            &mut effects,
                            &started,
                            game_mode,
                        ) {
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            apply(
                                conn,
                                &mut state,
                                proto.encode_update_mob_effect(
                                    LOCAL_PLAYER_ENTITY_ID,
                                    "minecraft:bad_omen",
                                    0,
                                    120_000,
                                    true,
                                    true,
                                    true,
                                    false,
                                ),
                            )
                            .await?;
                        } else if let Some((native, remainder, potion_effects)) =
                            finish_drinking_potion(&mut inventory, &started, game_mode)
                        {
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            // Vanilla's own potion-contents apply-to-living-entity routine's own split: an
                            // instantaneous effect heals/damages immediately (no
                            // `MobEffectInstance` is ever stored for one, so no
                            // `update_mob_effect` follows — only the health bar
                            // moves), a timed one is stored and announced.
                            let mut health_changed = false;
                            for effect in potion_effects {
                                match effect {
                                    crate::mob_effects::SplashEffect::Instant {
                                        effect_id,
                                        amount,
                                    } => {
                                        match effect_id {
                                            lodestone_data::mob_effects::MobEffectId::INSTANT_HEALTH => {
                                                vitals.heal(amount)
                                            }
                                            lodestone_data::mob_effects::MobEffectId::INSTANT_DAMAGE => {
                                                vitals.apply_effect_damage(amount);
                                            }
                                            _ => continue,
                                        }
                                        health_changed = true;
                                    }
                                    crate::mob_effects::SplashEffect::Timed {
                                        effect_id,
                                        duration,
                                        amplifier,
                                    } => {
                                        effects.apply(effect_id, duration, amplifier);
                                        let effect_name =
                                            lodestone_data::mob_effects::mob_effect_name_for(effect_id);
                                        apply(
                                            conn,
                                            &mut state,
                                            proto.encode_update_mob_effect(
                                                LOCAL_PLAYER_ENTITY_ID,
                                                effect_name,
                                                amplifier,
                                                duration,
                                                false,
                                                true,
                                                true,
                                                false,
                                            ),
                                        )
                                        .await?;
                                    }
                                }
                            }
                            if health_changed {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_set_health(
                                        vitals.health(),
                                        vitals.food().food_level(),
                                        vitals.food().saturation(),
                                    ),
                                )
                                .await?;
                            }
                        } else if let Some((native, remainder, cleared)) = finish_drinking_milk(
                            &mut inventory,
                            &mut effects,
                            &started,
                            game_mode,
                        ) {
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            for effect_id in cleared {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_remove_mob_effect(
                                        LOCAL_PLAYER_ENTITY_ID,
                                        &effect_id,
                                    ),
                                )
                                .await?;
                            }
                        }
                    }
                }

                // No position yet (client has not sent a single move since
                // join): nothing to test submersion against, so skip rather
                // than guess a spawn position this version-free crate does
                // not otherwise track (see `crate::vitals`'s module docs).
                if let Some((x, y, z)) = player_pos {
                    // Apply border damage before the submersion check because
                    // the player timer processes the border branch first.
                    // Read one border snapshot per timer tick and calculate
                    // damage for the tracked position; `apply_border_damage`
                    // returns `Some` only when the hit lands, while a dead
                    // player is a no-op. A player outside the safe zone takes
                    // `max(1, floor(d*0.2))` every tick. With a default
                    // full-size border, `damage_for_position` is always `None`:
                    // nothing is sent, at the cost of one clone and one
                    // distance scan per 50 ms.
                    let border_state = border.get();
                    let invulnerable = Abilities::for_mode(game_mode).invulnerable;
                    if let Some(damage) =
                        border_state.damage_for_position(x, z).filter(|_| !invulnerable)
                    {
                        if vitals.apply_border_damage(damage).is_some() {
                            publish_health(
                                conn,
                                &mut state,
                                proto,
                                &vitals,
                                &effects,
                                Vec3::new(x, y, z),
                                // Self-facing, per `publish_health`'s own call sites.
                                LOCAL_PLAYER_ENTITY_ID,
                                &username,
                                crate::vitals::DeathCause::OutsideBorder,
                                &mut advancements,
                                player_uuid,
                                Some(crate::vitals::HurtDirection::PURE_ROLL),
                            )
                            .await?;
                        }
                    }

                    if let Some(eye_in_water) = player_environment.eye_in_water(
                        lodestone_physics::Vec3d::new(x, y, z),
                        sprinting,
                        sneaking,
                        abilities.flying,
                        &|bx, by, bz| resident_block_state(source.get(), bx, by, bz),
                    ) {
                        // `!invulnerable &&`: a creative player's air bar does not
                        // deplete and they never drown. Suppressed here rather than
                        // at the damage below because `PlayerVitals` is mode-free by
                        // design, and a depleting bar that can never hurt is worse
                        // than no bar at all. A missing resident cell defers the
                        // complete air probe rather than treating it as air.
                        let outcome = tick_player_air_supply(
                            &mut vitals,
                            eye_in_water,
                            invulnerable,
                            &effects,
                        );
                        if let Some(air) = outcome.air_changed {
                            apply(conn, &mut state, proto.encode_air_supply_update(air)).await?;
                        }
                        if outcome.damage.is_some() {
                            publish_health(
                                conn,
                                &mut state,
                                proto,
                                &vitals,
                                &effects,
                                Vec3::new(x, y, z),
                                // Self-facing, per `publish_health`'s own call sites.
                                LOCAL_PLAYER_ENTITY_ID,
                                &username,
                                crate::vitals::DeathCause::Drown,
                                &mut advancements,
                                player_uuid,
                                Some(crate::vitals::HurtDirection::PURE_ROLL),
                            )
                            .await?;
                        }
                    }

                    // hostile-mob melee damage against this
                    // player, drained from `MobSim::take_player_hits`. See
                    // that method's, and `PlayerHit`'s, own doc comments for
                    // how a mob's attack target position resolves to a
                    // player identity and the one disclosed miss (a stale
                    // grudge target). `!invulnerable` matches the border and
                    // drowning arms just above: a creative/spectator player
                    // takes no damage from any source here.
                    //
                    // Filtered to `player_uuid` because the drain empties for
                    // whichever connection reads it first — the same
                    // single-consumer caveat `ExplosionFeed`/`BlockTickFeed`
                    // document — which matches this crate's one
                    // connection-per-mob-tick-loop shape today (singleplayer,
                    // and `bind`'s per-connection LAN wrapper — see those
                    // types' own doc comments).
                    if !invulnerable {
                        for hit in mobs.with(|sim| sim.take_player_hits()) {
                            if hit.identity.uuid != player_uuid {
                                continue;
                            }
                            let flags = lodestone_entity::DamageFlags::for_damage_type_name(
                                "mob_attack",
                            )
                            .expect("mob_attack is a real damage type");
                            if vitals
                                .apply_damage(
                                    hit.raw_damage,
                                    &effects.overlay_defenses(inventory.combat_stats().defenses),
                                    flags,
                                )
                                .is_some()
                            {
                                if hit.poison_ticks > 0 {
                                    effects.apply("minecraft:poison", hit.poison_ticks, 0);
                                    apply(
                                        conn,
                                        &mut state,
                                        proto.encode_update_mob_effect(
                                            LOCAL_PLAYER_ENTITY_ID,
                                            "minecraft:poison",
                                            0,
                                            hit.poison_ticks,
                                            false,
                                            true,
                                            true,
                                            false,
                                        ),
                                    )
                                    .await?;
                                }
                                let direction = crate::vitals::HurtDirection::from_source(
                                    hit.attacker_pos,
                                    Vec3::new(x, y, z),
                                    player_rot.unwrap_or_default().yaw,
                                );
                                publish_health(
                                    conn,
                                    &mut state,
                                    proto,
                                    &vitals,
                                    &effects,
                                    Vec3::new(x, y, z),
                                    // Self-facing, per `publish_health`'s own call sites.
                                    LOCAL_PLAYER_ENTITY_ID,
                                    &username,
                                    crate::vitals::DeathCause::Generic,
                                    &mut advancements,
                                    player_uuid,
                                    Some(direction),
                                )
                                .await?;
                            }
                        }
                    }
                }

                // Drain elder-guardian pulses for this player. The queue uses
                // the same single-consumer, UUID-filtered shape as
                // `take_player_hits`; each matching pulse applies mining
                // fatigue and emits game event kind `10`.
                if matches!(game_mode, GameMode::Survival | GameMode::Adventure) {
                    for aura in mobs.with(|sim| sim.take_mining_fatigue_auras()) {
                        if aura.target.uuid != player_uuid {
                            continue;
                        }
                        effects.apply(
                            "minecraft:mining_fatigue",
                            crate::mobs::ELDER_GUARDIAN_EFFECT_DURATION,
                            crate::mobs::ELDER_GUARDIAN_EFFECT_AMPLIFIER,
                        );
                        apply(
                            conn,
                            &mut state,
                            proto.encode_update_mob_effect(
                                LOCAL_PLAYER_ENTITY_ID,
                                "minecraft:mining_fatigue",
                                crate::mobs::ELDER_GUARDIAN_EFFECT_AMPLIFIER,
                                crate::mobs::ELDER_GUARDIAN_EFFECT_DURATION,
                                true,
                                true,
                                true,
                                false,
                            ),
                        )
                        .await?;
                        apply(conn, &mut state, proto.encode_game_event(10, 1.0)).await?;
                    }
                }

                // Burning. The ignition producer and the burn consumer in one place,
                // because both need the same feet-cell read — vanilla splits them
                // (its own fire-block entity-inside routine ignites, its own base-tick routine consumes)
                // only because the block and the entity are different objects.
                //
                // The **feet** cell, not the eye: `entityInside` fires for any cell the
                // bounding box overlaps, and the feet cell is the one this crate
                // tracks. Reading the eye instead would let a player stand in fire
                // unharmed up to their chin.
                //
                // `!invulnerable`: fire immunity applies to the entity type and
                // `invulnerable` applies inside the damage path; a creative
                // player is the second. Passed as `fire_immune` because the observable
                // is the same — the fire goes out and nothing hurts — and this crate
                // has no per-entity-type immunity table to consult.
                if let Some((x, y, z)) = player_pos {
                    if let Some(feet) = resident_block_state(
                        source.get(),
                        x.floor() as i32,
                        y.floor() as i32,
                        z.floor() as i32,
                    ) {
                        let standing_in = crate::burning::BurnSource::for_block(feet);
                        let creative = Abilities::for_mode(game_mode).invulnerable;
                        // Fire Resistance refuses the damage and leaves the counter
                        // running — see `crate::burning`'s doc for why that is not the
                        // same as putting the fire out.
                        let resistant = effects
                            .amplifier_of("minecraft:fire_resistance")
                            .is_some();
                        if let Some(source_kind) = standing_in
                            && !creative
                        {
                            match source_kind {
                            // Vanilla's own fire-block ignite routine — the player ramp, which is
                            // why running across one fire block can leave you unburnt.
                            // One draw per contact tick, from this connection's own
                            // stream.
                            crate::burning::BurnSource::Fire
                            | crate::burning::BurnSource::SoulFire => {
                                let ramp = 1 + i32::from(burn_rng.next_f32() < 0.5);
                                burn.fire_ignite(true, ramp);
                            }
                            // Vanilla's own entity lava-ignite routine — a flat 15 seconds, no ramp.
                            crate::burning::BurnSource::Lava => {
                                burn.ignite_for_ticks(crate::burning::LAVA_IGNITE_TICKS);
                            }
                        }
                        }
                        let out = burn.tick(standing_in, creative, resistant);
                        if out.damage > 0.0 {
                            vitals.apply_effect_damage(out.damage);
                            publish_health(
                                conn,
                                &mut state,
                                proto,
                                &vitals,
                                &effects,
                                Vec3::new(x, y, z),
                                // Self-facing, per `publish_health`'s own call sites.
                                LOCAL_PLAYER_ENTITY_ID,
                                &username,
                                crate::vitals::DeathCause::OnFire,
                                &mut advancements,
                                player_uuid,
                                Some(crate::vitals::HurtDirection::PURE_ROLL),
                            )
                            .await?;
                        }
                    }
                }

                // Beacon effects use an 80-tick world-time gate and run per
                // connection because `effects` and the wire notification are
                // connection state. The gate is independent of
                // `effects.is_empty()`, allowing a beacon to apply a first
                // effect to a player with no active effects.
                if let Some((px, py, pz)) = player_pos
                    && world.time().game_time % 80 == 0
                {
                    let candidates: Vec<(
                        BlockPos,
                        Option<crate::beacon::BeaconPower>,
                        Option<crate::beacon::BeaconPower>,
                    )> = block_entities
                        .with(|reg| {
                            reg.iter()
                                .filter_map(|(pos, entity)| match entity {
                                    BlockEntity::Beacon(b) if b.primary_effect.is_some() => Some((
                                        *pos,
                                        b.primary_effect.clone(),
                                        b.secondary_effect.clone(),
                                    )),
                                    _ => None,
                                })
                                .collect()
                        });
                    for (pos, primary, secondary) in candidates {
                        // Recomputed live, not read from the block entity's
                        // own (possibly stale — `BeaconData::levels`'s own
                        // doc) stored field: effect application must not
                        // outlive a pyramid the player has since broken.
                        let Some(levels) = resident_beacon_levels(
                            source.get(),
                            pos.x,
                            pos.y,
                            pos.z,
                        ) else {
                            continue;
                        };
                        let Some(beam_unobstructed) = resident_beam_unobstructed(
                            source.get(),
                            pos.x,
                            pos.y,
                            pos.z,
                            384,
                        ) else {
                            continue;
                        };
                        if levels == 0 || !beam_unobstructed {
                            continue;
                        }
                        let (range, application) =
                            crate::beacon::beacon_effects(levels, primary, secondary);
                        // The effect area reaches `range` blocks horizontally,
                        // from `range` below the beacon to the top of the
                        // world. Approximate the unbounded upper edge as
                        // "no lower than `range` below" because
                        // since `ChunkSource` has no height accessor of its
                        // own (`crate::beacon`'s own module doc names the
                        // same gap).
                        let dx = px - f64::from(pos.x);
                        let dz = pz - f64::from(pos.z);
                        let dy = py - f64::from(pos.y);
                        if dx.mul_add(dx, dz * dz) > range * range || dy < -range {
                            continue;
                        }
                        for grant in &application {
                            effects.apply(
                                grant.effect.key(),
                                grant.duration_ticks,
                                grant.amplifier,
                            );
                            apply(
                                conn,
                                &mut state,
                                proto.encode_update_mob_effect(
                                    // Self-facing, per every other
                                    // `encode_update_mob_effect`-adjacent
                                    // call in this loop.
                                    LOCAL_PLAYER_ENTITY_ID,
                                    grant.effect.key(),
                                    grant.amplifier,
                                    grant.duration_ticks,
                                    true,
                                    true,
                                    true,
                                    false,
                                ),
                            )
                            .await?;
                        }
                    }
                }

                // A qualifying player near an occupied village point converts
                // Bad Omen into Raid Omen and records the player's position.
                // When the effect reaches its final tick, that position feeds
                // [`MobSim::create_or_extend_raid`], which queries occupied
                // village points within 64 blocks. Read the duration before
                // decrementing it so the final tick can perform the conversion.
                if let Some((x, y, z)) = player_pos
                    && game_mode != GameMode::Spectator
                {
                    let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
                    let (difficulty, _) = world.difficulty();
                    if difficulty != lodestone_model::Difficulty::Peaceful
                        && let Some(amplifier) = effects.amplifier_of("minecraft:bad_omen")
                        && !mobs.with(|sim| sim.occupied_village_pois_in_range(pos, 64)).is_empty()
                    {
                        effects.remove("minecraft:bad_omen");
                        apply(
                            conn,
                            &mut state,
                            proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:bad_omen"),
                        )
                        .await?;
                        effects.apply("minecraft:raid_omen", 600, amplifier);
                        apply(
                            conn,
                            &mut state,
                            proto.encode_update_mob_effect(
                                LOCAL_PLAYER_ENTITY_ID,
                                "minecraft:raid_omen",
                                amplifier,
                                600,
                                false,
                                true,
                                true,
                                true,
                            ),
                        )
                        .await?;
                        raid_omen_position = Some(pos);
                    }

                    let expiring_raid_omen = effects
                        .get("minecraft:raid_omen")
                        .map(|instance| (instance.duration(), instance.amplifier()));
                    if let Some((1, amplifier)) = expiring_raid_omen
                        && let Some(origin) = raid_omen_position.take()
                    {
                        effects.remove("minecraft:raid_omen");
                        apply(
                            conn,
                            &mut state,
                            proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:raid_omen"),
                        )
                        .await?;
                        mobs.with(|sim| sim.create_or_extend_raid(origin, difficulty, amplifier));
                    }
                }

                // Untamed mounts that threw this player off.
                for mount in mobs.with(|sim| sim.take_ejections_of(player_entity_id)) {
                    apply(conn, &mut state, proto.encode_set_passengers(mount, &[])).await?;
                }
                // The raid-completion queue carries the effect a player earns for
                // a killing blow. It fires when a raid this player earned a killing
                // blow in reaches `RaidStatus::Victory`. That transition
                // happens inside the shared background sim task, which has no
                // connection's `ActiveEffects` to grant an effect onto —
                // `MobSim::take_hero_of_the_village_grants` is the queue this
                // drains instead (see its own doc). Checked every tick,
                // independent of whether *this* connection's player currently
                // carries Bad Omen or Raid Omen at all: the killing blow that
                // earned the grant may have landed many ticks, and waves,
                // before the raid's last one actually clears.
                for amplifier in mobs.with(|sim| sim.take_hero_of_the_village_grants(player_uuid)) {
                    let amplifier = u32::try_from(amplifier).unwrap_or(0);
                    effects.apply("minecraft:hero_of_the_village", 48_000, amplifier);
                    apply(
                        conn,
                        &mut state,
                        proto.encode_update_mob_effect(
                            LOCAL_PLAYER_ENTITY_ID,
                            "minecraft:hero_of_the_village",
                            amplifier,
                            48_000,
                            true,
                            true,
                            true,
                            false,
                        ),
                    )
                    .await?;
                }

                // Potion effects a splash left on this player.
                for (effect, duration, amplifier) in mobs.with(|sim| sim.take_player_effects(player_uuid)) {
                    effects.apply(effect, duration, amplifier);
                    apply(
                        conn,
                        &mut state,
                        proto.encode_update_mob_effect(
                            LOCAL_PLAYER_ENTITY_ID,
                            lodestone_data::mob_effects::mob_effect_name_for(effect),
                            amplifier,
                            duration,
                            true,
                            true,
                            true,
                            false,
                        ),
                    )
                    .await?;
                }

                // The shared mob simulation queues exit-portal geometry and
                // egg placement because it has no connection on which to
                // publish world changes. Resolve the sibling world through
                // `home`, so any connected player's timer can apply the queue.
                for death in mobs.with(|sim| sim.take_dragon_deaths()) {
                    if let Some(destination) = home.get().sibling(crate::dimension::Dimension::End) {
                        // The death queue is drained from this connection's
                        // 20 Hz timer, so its writes must not be the first
                        // access to a cold End column. Admit every target and
                        // its retained-light neighbours before the existing
                        // ordered write sequence below.
                        let mut admission = HashSet::new();
                        let mut admit_position = |pos: BlockPos| {
                            admission.extend(column_admission_footprint(
                                pos.x.div_euclid(16),
                                pos.z.div_euclid(16),
                                1,
                            ));
                        };
                        for (pos, _) in &death.exit_portal_blocks {
                            admit_position(*pos);
                        }
                        for (pos, _) in &death.gateway_blocks {
                            admit_position(*pos);
                        }
                        if death.outcome.place_dragon_egg {
                            admit_position(death.origin);
                        }
                        if !admission.is_empty() {
                            let _ = generate_columns_offloaded(
                                Arc::clone(&destination),
                                admission.into_iter().collect(),
                            )
                            .await;
                        }
                        for (pos, state) in &death.exit_portal_blocks {
                            let state = StateId::from_state_str(state)
                                .expect("dragon exit portal emits a built-in state");
                            destination.set_block(pos.x, pos.y, pos.z, state);
                        }
                        if death.outcome.place_dragon_egg {
                            // Place the egg on the first solid surface above
                            // the portal. Scan down from `origin.y + 33` so a
                            // column that contains player-built blocks uses
                            // its actual top surface.
                            let mut egg_y = death.origin.y + 33;
                            let mut resident_scan = true;
                            while egg_y > death.origin.y {
                                match resident_block_state(
                                    &*destination,
                                    death.origin.x,
                                    egg_y,
                                    death.origin.z,
                                ) {
                                    Some(state) if state.block() == Block::Air => egg_y -= 1,
                                    Some(_) => break,
                                    None => {
                                        resident_scan = false;
                                        break;
                                    }
                                }
                            }
                            if resident_scan {
                                destination.set_block(
                                    death.origin.x,
                                    egg_y + 1,
                                    death.origin.z,
                                    Block::DragonEgg.default_state(),
                                );
                            }
                        }
                        // `death.gateway_blocks` contains the positions from
                        // `outcome.spawn_gateway`'s formula and its shuffled
                        // 20-slice pool. The list is empty when spawning is
                        // disabled or the pool is exhausted. Publish the centre
                        // block's empty destination into the End registry as
                        // well as writing the visible structure; the contact
                        // loop resolves its delayed safe exit on use.
                        for (pos, state) in &death.gateway_blocks {
                            let state = StateId::from_state_str(state)
                                .expect("dragon gateway emits a built-in state");
                            destination.set_block(pos.x, pos.y, pos.z, state);
                            if state.block() == crate::portal::END_GATEWAY_BLOCK
                                && let Some(registries) = destination.world_registries()
                            {
                                registries.block_entities.with(|registry| {
                                    registry.insert(
                                        *pos,
                                        BlockEntity::EndGateway {
                                            exit: None,
                                            exact: false,
                                        },
                                    );
                                });
                            }
                        }
                    }
                }

                // Status effects, ahead of hunger. The order matters for one arm:
                // `hunger`
                // charges exhaustion, so it must land before the exhaustion is spent
                // rather than a tick late.
                //
                // `game_tick` is the entity tick count needed **only** for an
                // infinite effect; a finite one counts against its remaining
                // duration.
                // Apply a newly granted/replaced Health Boost before periodic
                // effects read their regeneration ceiling. The second sync
                // below catches expiry and hidden-effect restoration.
                sync_effect_max_health(conn, &mut state, proto, &mut vitals, &effects).await?;
                if !effects.is_empty() {
                    let out = effects.tick(
                        i32::try_from(world.time().game_time.max(0)).unwrap_or(i32::MAX),
                        vitals.health(),
                        vitals.max_health(),
                    );
                    if out.exhaustion > 0.0 {
                        vitals.add_exhaustion(out.exhaustion);
                    }
                    let saturation_changed = apply_effect_saturation(&mut vitals, out.saturation);
                    // Poison's `health > 1.0` guard is already applied inside the
                    // registry, so this is an unconditional subtraction of an amount
                    // that was only produced when the guard allowed it.
                    let mut moved = saturation_changed;
                    // Tracked separately from `moved`, because regeneration reaches
                    // this publish too and a heal must not flash the screen red or
                    // tilt the camera. This is the one arm where "health changed"
                    // and "a hit landed" genuinely differ.
                    let mut hurt_landed = false;
                    if out.heal > 0.0 {
                        vitals.heal(out.heal);
                        moved = true;
                    }
                    if out.poison_damage > 0.0 {
                        vitals.apply_effect_damage(out.poison_damage);
                        moved = true;
                        hurt_landed = true;
                    }
                    if out.wither_damage > 0.0 {
                        vitals.apply_effect_damage(out.wither_damage);
                        moved = true;
                        hurt_landed = true;
                    }
                    if moved {
                        publish_health(
                            conn,
                            &mut state,
                            proto,
                            &vitals,
                            &effects,
                            // No terrain read backs this arm (status effects tick
                            // regardless of a reported position), so this falls
                            // back to the origin on a connection that has never
                            // moved — see `publish_health`'s own parameter doc.
                            player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                            // Self-facing, per `publish_health`'s own call sites.
                            LOCAL_PLAYER_ENTITY_ID,
                            &username,
                            crate::vitals::DeathCause::Wither,
                            &mut advancements,
                            player_uuid,
                            hurt_landed.then_some(crate::vitals::HurtDirection::PURE_ROLL),
                        )
                        .await?;
                    }
                    sync_effect_max_health(conn, &mut state, proto, &mut vitals, &effects).await?;
                }

                // Hunger, after the air block — vanilla's own order
                // (its own base-tick routine's water-breath block, then
                // the per-player hunger tick. Runs whether or not a
                // position has been reported, unlike drowning: hunger needs no
                // terrain, only the difficulty and a game rule, and a player who
                // has not moved since joining still starves.
                //
                // `!invulnerable`: the exhaustion gate means a
                // creative player accumulates no exhaustion at all and their bar can
                // never move. Skipping the whole tick is equivalent and cheaper —
                // with no exhaustion there is nothing to spend, and the regeneration
                // arms are moot for a player who cannot be hurt.
                if !Abilities::for_mode(game_mode).invulnerable {
                    let (difficulty, _) = world.difficulty();
                    let food_out = vitals.tick_food(difficulty, world.natural_health_regeneration());
                    // A heal or a starve moves health, and a food/saturation change
                    // moves the HUD — either way the client needs the packet, and
                    // `SetHealth` carries all three fields in one.
                    if !food_out.is_empty() {
                        publish_health(
                            conn,
                            &mut state,
                            proto,
                            &vitals,
                            &effects,
                            // Hunger needs no terrain and runs even before the
                            // first movement packet — see the wither arm just
                            // above for the same fallback.
                            player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                            // Self-facing, per `publish_health`'s own call sites.
                            LOCAL_PLAYER_ENTITY_ID,
                            &username,
                            crate::vitals::DeathCause::Starve,
                            &mut advancements,
                            player_uuid,
                            // `food_out.starve`, not `!food_out.is_empty()`: this
                            // publish also carries a pure food/saturation change and
                            // the natural-regeneration heal, neither of which is a
                            // hit.
                            food_out
                                .starve
                                .map(|_| crate::vitals::HurtDirection::PURE_ROLL),
                        )
                        .await?;
                    }
                }

                travel.tick(
                    home, source, block_entities, mobs, player_entity_id,
                    player_pos, game_mode, world,
                );
                if travel.take_end_exit_contact() {
                    connection_travel::begin_end_exit(
                        conn, proto, &mut state, &mut end_exit, &mut preserved_player_fields,
                    ).await?;
                    live_publish_player(
                        live_save, player_store.as_ref(), player_uuid, player_pos, player_rot,
                        world_spawn, &vitals, game_mode, &inventory, &experience,
                        &preserved_player_fields, source.dimension(),
                    );
                }
                republish_inventory(entities.players(), player_uuid, &inventory);
                watch.pass("vitals_tick");
            }

            _ = container_sync_tick.tick() => {
                watch.enter();
                // The piece with no inbound packet driving it at all: the
                // server's unified tick loop (`crate::tick::run_tick_loop`,
                // mutates the registry independently of any
                // connection, so this connection needs its own timer to notice — see
                // `sync_open_container`'s own doc comment.
                publish_open_container(
                    conn, proto, &mut state, block_entities,
                    &mut open_container, &mut container_sync,
                )
                .await?;
                // Keep tick updates for visible columns ordered, but send them
                // outside this timer arm so a burst cannot block socket reads.
                queue_tick_block_updates(
                    &mut pending_tick_updates,
                    &view.delivered,
                    block_ticks.drain_all(),
                );
                // Drain the feed's effect lane: world-tick sounds, particles,
                // and level events. These effects share the feed's single
                // consumer, as described by `BlockTickFeed`.
                for effect in block_ticks.drain_effects_for(player_uuid) {
                    // Handle piston pushes before generic world-effect
                    // encoding. `PistonPlayerPush` has no packet
                    // representation in `encode_world_effect`; it carries the
                    // displacement needed to correct this connection's
                    // tracked position, so intercept it here.
                    if let crate::effects::WorldEffect::PistonPlayerPush { source, dest, push_delta } = effect {
                        if let Some((px, py, pz)) = player_pos
                            && player_overlaps_piston_sweep(px, py, pz, source, dest)
                        {
                            let (nx, ny, nz) =
                                (px + push_delta.x, py + push_delta.y, pz + push_delta.z);
                            let current = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
                            player_pos = Some((nx, ny, nz));
                            apply(
                                conn,
                                &mut state,
                                proto.encode_teleport_with_id(
                                    issue_teleport_id(&mut teleport_acknowledgements),
                                    nx,
                                    ny,
                                    nz,
                                    current.yaw,
                                    current.pitch,
                                ),
                            )
                            .await?;
                        }
                        continue;
                    }
                    apply(conn, &mut state, proto.encode_world_effect(&effect)).await?;
                }
                // Drain explosions emitted by the shared mob simulation. The
                // feed has one consumer for each in-memory world instance.
                for detonation in explosions.drain_all() {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_explode(detonation.centre, detonation.radius),
                    )
                    .await?;
                }
                // The per-entity animation cues for hits the mob sim resolved:
                // vanilla's `broadcastDamageEvent` (which we send as
                // `hurt_animation` — see `ServerProtocol::encode_hurt_animation`
                // for why the route differs and the pixels do not). The entity
                // death event carries the same byte value used by the client.
                //
                // Without this a mob beaten to death never flashed and never tipped
                // over: it simply disappeared when the next entity diff dropped it,
                // which reads as a despawn rather than a kill.
                //
                // **Drained straight off the `MobHandle` rather than through a
                // feed**, unlike the three drains above. The feeds exist to carry
                // world-global events from the *tick task* to a connection; these
                // are already per-entity and the sim is already shared with this
                // task (`apply_attack` mutates it from here), so a feed would add a
                // hop and nothing else. It inherits the same single-consumer
                // caveat: with two connections sharing one sim the first to reach
                // this line takes the queue. The current LAN wiring gives
                // `IntegratedServer::bind`'s LAN worlds get a `MobHandle::default`
                // with no population — and a second player needs per-connection
                // tracking here, not a feed.
                for animation in mobs.with(crate::mobs::MobSim::take_entity_animations) {
                    let directive = match animation {
                        crate::mobs::MobAnimation::Hurt { entity_id } => {
                            // `0.0` is the fixed hurt-animation direction for a
                            // non-player entity; player-specific direction data
                            // is handled by the connection path.
                            proto.encode_hurt_animation(entity_id, 0.0)
                        }
                        crate::mobs::MobAnimation::Died { entity_id } => proto
                            .encode_entity_event(
                                entity_id,
                                crate::protocol::entity_event::DEATH,
                            ),
                    };
                    apply(conn, &mut state, directive).await?;
                }
                // Drain weather transitions published by the world tick loop.
                // The feed has one consumer for each in-memory world instance.
                for event in weather.drain_all() {
                    let (kind, value) = event.wire();
                    apply(conn, &mut state, proto.encode_game_event(kind, value)).await?;
                }
                // Update the sleep vote and deliver night-skip notifications.
                // Both use this connection's regular timer:
                //
                // 1. Feed the voter count. Vanilla excludes spectators
                //    (vanilla's own update-sleeping-players routine); this crate has no
                //    spectator concept, so every player in the shared
                //    `PlayerRegistry` counts. Where no registry exists
                //    (singleplayer), nothing is fed and
                //    `SleepState::sleepers_needed`'s `max(1, …)` floor yields
                //    exactly 1 — the correct single-player vote.
                if let Some(registry) = entities.players() {
                    sleep_vote.set_active(registry.len() as u32);
                }
                // 2. Learn of a skip. `SleepEvent::SkippedNight` re-anchors
                //    this client's day clock to the morning — `encode_set_time`
                //    with a `Some` day-time — exactly the broadcast vanilla's
                //    skip path sends: the world's clock jumped, so every
                //    connection must re-anchor. `SleepFeed::drain_all` is
                //    single-consumer for the same reason the drains above are
                //    (see that type's own doc comment).
                for event in sleep_feed.drain_all() {
                    match event {
                        SleepEvent::SkippedNight { game_time, morning } => {
                            apply(
                                conn,
                                &mut state,
                                proto.encode_set_time(game_time, Some(morning)),
                            )
                            .await?;
                        }
                    }
                }
                // Drain server-initiated resource-pack pushes. The feed is
                // published by host control paths and has one consumer for
                // each in-memory world instance.
                for push in resource_packs.drain_all() {
                    apply(conn, &mut state, proto.encode_resource_pack_push(&push)).await?;
                }
                // Drain each connection's chat cursor from the shared
                // append-only log, so every connection receives every line.
                if let Some(registry) = entities.players() {
                    for line in registry.chat_since(&mut chat_cursor) {
                        apply(conn, &mut state, proto.encode_system_chat(&line.rendered()))
                            .await?;
                    }
                    // Broadcast arm swings from the same shared log while
                    // excluding the connection that produced each event.
                    for event in registry.swings_since(&mut swing_cursor) {
                        if event.entity_id != player_entity_id {
                            apply(
                                conn,
                                &mut state,
                                proto.encode_animate(event.entity_id, swing_action(event.hand)),
                            )
                            .await?;
                        }
                    }
                    // Command effects another player's command aimed at *this*
                    // connection — `/gamemode creative Steve` typed by someone
                    // else, or `/give @a diamond`.
                    //
                    // A **drain**, not a cursor, and that is the difference from
                    // chat two lines up: the queue is per-uuid and this is its
                    // only reader, so taking it is what makes the delivery
                    // directed. A cursor over a shared log would hand Steve's
                    // game-mode change to everyone.
                    for effect in registry.take_effects(player_uuid) {
                        apply_own_effect(
                            conn,
                            proto,
                            &mut state,
                            &mut game_mode,
                            &mut abilities,
                            &mut inventory,
                            Some(registry),
                            player_uuid,
                            effect,
                            &mut advancements,
                            world,
                            &mut effects,
                            &mut vitals,
                            &mut experience,
                            player_entity_id,
                            &username,
                            &mut player_pos,
                            &mut player_rot,
                            &mut teleport_acknowledgements,
                        )
                        .await?;
                    }
                }
                // Drain host-published plugin-channel broadcasts through each
                // connection's cursor, filtering to channels this client
                // announced. Unsupported channels are skipped.
                for (channel, data) in plugin_channels.outbound_since(
                    &mut plugin_channel_cursor,
                    client_channels,
                ) {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_custom_payload(&channel, &data),
                    )
                    .await?;
                }
                watch.pass("container_sync_tick");
            }
        }
        // Publish the cancellation-safe snapshot — see `live_publish_player`'s
        // and `crate::live_save::LiveSaveSlot`'s own doc comments. Once per
        // iteration, after whichever arm above completed, so the mirror is at
        // most one packet or timer tick behind whatever the cancellation
        // below would otherwise drop entirely. Placed here rather than inside
        // each arm individually: every arm reaches this same point on a
        // normal completion, and an arm that instead returns via `?` or the
        // disconnect arm's own explicit `return` skips it — correctly, since
        // both of those are real completions with their own save story
        // already (a genuine error, or the `persist_player` call right
        // above).
        live_publish_player(
            live_save,
            player_store.as_ref(),
            player_uuid,
            player_pos,
            player_rot,
            world_spawn,
            &vitals,
            game_mode,
            &inventory,
            &experience,
            &preserved_player_fields,
            source.dimension(),
        );
        // The native locator is deliberately a separate, typed sidecar. It is
        // published in memory only; IntegratedServer::shutdown writes the last
        // snapshot after joining this task, while a real socket disconnect uses
        // the synchronous path above.
        publish_native_player(
            native_player,
            live_save,
            player_pos,
            player_rot,
            world_spawn,
            source.dimension(),
            game_mode,
            &vitals,
            &experience,
            &inventory,
        );
    }
}

/// Advances player vitals for a `wasm32` connection: air supply and drowning,
/// world-border damage, burning, beacon effects, status effects, hunger, and
/// item-use completion. [`crate::browser_timer::BrowserInterval`] supplies the
/// timer boundary, and the beacon sweep reads `block_entities` for eligible
/// effects.
///
/// # Why this function is separate
///
/// The helper keeps the browser timer boundary small while calling the
/// production operations (`PlayerVitals::tick`, `BurnState::tick`,
/// `ActiveEffects::tick`, `finish_consuming`, and `publish_health`).
///
/// # What it excludes
///
/// - **Periodic player save.** `wasm32` has no filesystem or `PlayerDataStore`.
/// - **Deferred block-break continuation.** The wasm32 caller passes `None` for
///   `start_tick`, so `PendingBreak::defer` returns `None` and no deferred break
///   enters this loop.
/// - **Hostile-mob melee damage.** `MobHandle::take_player_hits` has no producer
///   on wasm32, so its queue is empty.
///
/// World-border damage, burning, status effects, and hunger are included because
/// each has a packet-reachable producer: border commands, block reads, or the
/// `item_in_use` arm.
#[allow(clippy::too_many_arguments)]
#[cfg(target_arch = "wasm32")]
pub(super) async fn wasm_vitals_tick<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    state: &mut State,
    world: &crate::world_state::WorldStateHandle,
    border: &BorderFeed,
    game_mode: GameMode,
    player_uuid: uuid::Uuid,
    username: &str,
    player_pos: Option<(f64, f64, f64)>,
    player_environment: &mut crate::player_environment::PlayerEnvironment,
    sprinting: bool,
    sneaking: bool,
    flying: bool,
    vitals: &mut PlayerVitals,
    inventory: &mut PlayerInventory,
    advancements: &mut AdvancementManager,
    drops_rng: &mut SpawnRng,
    burn: &mut crate::burning::BurnState,
    burn_rng: &mut SpawnRng,
    effects: &mut crate::mob_effects::ActiveEffects,
    item_in_use: &mut Option<ItemInUse>,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    // Read every tracked beacon block entity for the per-connection sweep.
    block_entities: &BlockEntityHandle,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let invulnerable = Abilities::for_mode(game_mode).invulnerable;

    // Apply periodic eat/drink effects, then finish the item use when
    // `finish_tick` is reached.
    if let Some(started) = item_in_use.clone() {
        let now = mobs.with(|sim| sim.tick_count());
        if now < started.finish_tick
            && let Some(pos) = player_pos
            && let Some(consumable) =
                lodestone_game::consumable::consumable_for_item(&started.item)
        {
            let remaining = u32::try_from(started.finish_tick - now).unwrap_or(u32::MAX);
            let already = started.last_effect_remaining == Some(remaining);
            if !already
                && lodestone_game::consumable::should_emit_consume_effects(
                    consumable.consume_ticks,
                    remaining,
                )
            {
                let seed = i64::from(drops_rng.next_int(i32::MAX));
                let roll = drops_rng.next_f32();
                if let Some(effect) = crate::effects::item_consumed_tick(
                    &started.item,
                    Vec3::new(pos.0, pos.1, pos.2),
                    roll,
                    seed,
                ) {
                    block_ticks.publish_effect_except(player_uuid, effect);
                }
            }
            if let Some(live) = item_in_use.as_mut() {
                live.last_effect_remaining = Some(remaining);
            }
        }
        if now >= started.finish_tick {
            *item_in_use = None;
            if let Some((native, remainder)) =
                finish_consuming(inventory, vitals, &started, game_mode)
            {
                if let Some(pos) = player_pos {
                    let at = Vec3::new(pos.0, pos.1, pos.2);
                    let seed = i64::from(drops_rng.next_int(i32::MAX));
                    if let Some(effect) = crate::effects::item_consume_finished(
                        &started.item,
                        at,
                        drops_rng.next_f32(),
                        seed,
                    ) {
                        block_ticks.publish_effect(effect);
                    }
                    block_ticks.publish_effect(crate::effects::player_burped(
                        at,
                        drops_rng.next_f32(),
                        i64::from(drops_rng.next_int(i32::MAX)),
                    ));
                }
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                apply(
                    conn,
                    state,
                    proto.encode_set_health(
                        vitals.health(),
                        vitals.food().food_level(),
                        vitals.food().saturation(),
                    ),
                )
                .await?;

                // Apply supported food-consumption effects as status-effect
                // frames.
                for grant in crate::mob_effects::food_consume_effects(&started.item) {
                    if drops_rng.next_f32() >= grant.probability {
                        continue;
                    }
                    effects.apply(grant.effect_id, grant.duration, grant.amplifier);
                    apply(
                        conn,
                        state,
                        proto.encode_update_mob_effect(
                            LOCAL_PLAYER_ENTITY_ID,
                            grant.effect_id,
                            grant.amplifier,
                            grant.duration,
                            false,
                            true,
                            true,
                            false,
                        ),
                    )
                    .await?;
                }
                if crate::mob_effects::removes_poison_on_consume(&started.item)
                    && effects.remove("minecraft:poison")
                {
                    apply(
                        conn,
                        state,
                        proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:poison"),
                    )
                    .await?;
                }
            } else if let Some((native, remainder)) =
                finish_drinking_ominous_bottle(inventory, effects, &started, game_mode)
            {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                apply(
                    conn,
                    state,
                    proto.encode_update_mob_effect(
                        LOCAL_PLAYER_ENTITY_ID,
                        "minecraft:bad_omen",
                        0,
                        120_000,
                        true,
                        true,
                        true,
                        false,
                    ),
                )
                .await?;
            } else if let Some((native, remainder, potion_effects)) =
                finish_drinking_potion(inventory, &started, game_mode)
            {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                let mut health_changed = false;
                for effect in potion_effects {
                    match effect {
                        crate::mob_effects::SplashEffect::Instant { effect_id, amount } => {
                            match effect_id {
                                lodestone_data::mob_effects::MobEffectId::INSTANT_HEALTH => {
                                    vitals.heal(amount)
                                }
                                lodestone_data::mob_effects::MobEffectId::INSTANT_DAMAGE => {
                                    vitals.apply_effect_damage(amount)
                                }
                                _ => continue,
                            }
                            health_changed = true;
                        }
                        crate::mob_effects::SplashEffect::Timed {
                            effect_id,
                            duration,
                            amplifier,
                        } => {
                            effects.apply(effect_id, duration, amplifier);
                            let effect_name =
                                lodestone_data::mob_effects::mob_effect_name_for(effect_id);
                            apply(
                                conn,
                                state,
                                proto.encode_update_mob_effect(
                                    LOCAL_PLAYER_ENTITY_ID,
                                    effect_name,
                                    amplifier,
                                    duration,
                                    false,
                                    true,
                                    true,
                                    false,
                                ),
                            )
                            .await?;
                        }
                    }
                }
                if health_changed {
                    apply(
                        conn,
                        state,
                        proto.encode_set_health(
                            vitals.health(),
                            vitals.food().food_level(),
                            vitals.food().saturation(),
                        ),
                    )
                    .await?;
                }
            } else if let Some((native, remainder, cleared)) =
                finish_drinking_milk(inventory, effects, &started, game_mode)
            {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                for effect_id in cleared {
                    apply(
                        conn,
                        state,
                        proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, &effect_id),
                    )
                    .await?;
                }
            }
        }
    }

    // Apply border damage, then process drowning and air supply.
    if let Some((x, y, z)) = player_pos {
        let border_state = border.get();
        if let Some(damage) = border_state.damage_for_position(x, z).filter(|_| !invulnerable) {
            if vitals.apply_border_damage(damage).is_some() {
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::OutsideBorder,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }

        if let Some(eye_in_water) = player_environment.eye_in_water(
            lodestone_physics::Vec3d::new(x, y, z),
            sprinting,
            sneaking,
            flying,
            &|bx, by, bz| resident_block_state(source.get(), bx, by, bz),
        ) {
            // `!invulnerable &&` keeps creative players from depleting air or
            // drowning. A missing resident cell defers this probe until a later
            // timer or movement packet.
            let outcome = tick_player_air_supply(
                vitals,
                eye_in_water,
                invulnerable,
                effects,
            );
            if let Some(air) = outcome.air_changed {
                apply(conn, state, proto.encode_air_supply_update(air)).await?;
            }
            if outcome.damage.is_some() {
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::Drown,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }
    }

    // Burning reads the block at the player's feet, updates burn state, and
    // publishes health when damage applies.
    if let Some((x, y, z)) = player_pos {
        if let Some(feet) = resident_block_state(
            source.get(),
            x.floor() as i32,
            y.floor() as i32,
            z.floor() as i32,
        ) {
            let standing_in = crate::burning::BurnSource::for_block(feet);
            let creative = Abilities::for_mode(game_mode).invulnerable;
            let resistant = effects.amplifier_of("minecraft:fire_resistance").is_some();
            if let Some(source_kind) = standing_in
                && !creative
            {
                match source_kind {
                    crate::burning::BurnSource::Fire | crate::burning::BurnSource::SoulFire => {
                        let ramp = 1 + i32::from(burn_rng.next_f32() < 0.5);
                        burn.fire_ignite(true, ramp);
                    }
                    crate::burning::BurnSource::Lava => {
                        burn.ignite_for_ticks(crate::burning::LAVA_IGNITE_TICKS);
                    }
                }
            }
            let out = burn.tick(standing_in, creative, resistant);
            if out.damage > 0.0 {
                vitals.apply_effect_damage(out.damage);
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::OnFire,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }
    }

    // Beacons run on an 80-tick cadence and feed `effects` before the status
    // effect tick. **Not** gated on `!effects.is_empty()` —
    // a beacon must be able to apply a *first* effect to a player who
    // currently has none.
    if let Some((px, py, pz)) = player_pos
        && world.time().game_time % 80 == 0
    {
        let candidates: Vec<(
            BlockPos,
            Option<crate::beacon::BeaconPower>,
            Option<crate::beacon::BeaconPower>,
        )> = block_entities.with(|reg| {
            reg.iter()
                .filter_map(|(pos, entity)| match entity {
                    BlockEntity::Beacon(b) if b.primary_effect.is_some() => {
                        Some((*pos, b.primary_effect.clone(), b.secondary_effect.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (pos, primary, secondary) in candidates {
            let Some(levels) = resident_beacon_levels(source.get(), pos.x, pos.y, pos.z) else {
                continue;
            };
            let Some(beam_unobstructed) = resident_beam_unobstructed(
                source.get(),
                pos.x,
                pos.y,
                pos.z,
                384,
            ) else {
                continue;
            };
            if levels == 0 || !beam_unobstructed {
                continue;
            }
            let (range, application) =
                crate::beacon::beacon_effects(levels, primary, secondary);
            let dx = px - f64::from(pos.x);
            let dz = pz - f64::from(pos.z);
            let dy = py - f64::from(pos.y);
            if dx.mul_add(dx, dz * dz) > range * range || dy < -range {
                continue;
            }
            for grant in &application {
                effects.apply(
                    grant.effect.key(),
                    grant.duration_ticks,
                    grant.amplifier,
                );
                apply(
                    conn,
                    state,
                    proto.encode_update_mob_effect(
                        LOCAL_PLAYER_ENTITY_ID,
                        grant.effect.key(),
                        grant.amplifier,
                        grant.duration_ticks,
                        true,
                        true,
                        true,
                        false,
                    ),
                )
                .await?;
            }
        }
    }

    // Tick status effects before hunger so their exhaustion is included when
    // hunger consumes exhaustion.
    // Apply a newly granted/replaced Health Boost before periodic effects read
    // their regeneration ceiling. The second sync below catches expiry and
    // hidden-effect restoration.
    sync_effect_max_health(conn, state, proto, vitals, effects).await?;
    if !effects.is_empty() {
        let out = effects.tick(
            i32::try_from(world.time().game_time.max(0)).unwrap_or(i32::MAX),
            vitals.health(),
            vitals.max_health(),
        );
        if out.exhaustion > 0.0 {
            vitals.add_exhaustion(out.exhaustion);
        }
        let mut moved = apply_effect_saturation(vitals, out.saturation);
        let mut hurt_landed = false;
        if out.heal > 0.0 {
            vitals.heal(out.heal);
            moved = true;
        }
        if out.poison_damage > 0.0 {
            vitals.apply_effect_damage(out.poison_damage);
            moved = true;
            hurt_landed = true;
        }
        if out.wither_damage > 0.0 {
            vitals.apply_effect_damage(out.wither_damage);
            moved = true;
            hurt_landed = true;
        }
        if moved {
            publish_health(
                conn,
                state,
                proto,
                vitals,
                effects,
                // Terrain data is not needed for this health publication.
                player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                LOCAL_PLAYER_ENTITY_ID,
                username,
                crate::vitals::DeathCause::Wither,
                advancements,
                player_uuid,
                hurt_landed.then_some(crate::vitals::HurtDirection::PURE_ROLL),
            )
            .await?;
        }
        sync_effect_max_health(conn, state, proto, vitals, effects).await?;
    }

    // Hunger runs after air checks and does not require a reported position.
    if !Abilities::for_mode(game_mode).invulnerable {
        let (difficulty, _) = world.difficulty();
        let food_out = vitals.tick_food(difficulty, world.natural_health_regeneration());
        if !food_out.is_empty() {
            publish_health(
                conn,
                state,
                proto,
                vitals,
                effects,
                player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                LOCAL_PLAYER_ENTITY_ID,
                username,
                crate::vitals::DeathCause::Starve,
                advancements,
                player_uuid,
                food_out.starve.map(|_| crate::vitals::HurtDirection::PURE_ROLL),
            )
            .await?;
        }
    }

    Ok(())
}

/// Drives a `wasm32` play connection with [`dispatch_play_packet`]. A
/// `crate::browser_timer::BrowserInterval` handles status, effects, hunger,
/// and item-consumption work, together with feed drains that have browser
/// producers.
///
/// Browser connections use an in-process `DuplexStream`, so the peer cannot go
/// quiet independently and keep-alive expiration is not applicable. The
/// world-spawn ticket still has a 20-tick countdown driven by
/// `ChunkStore::maybe_tick_tickets`; the `PLAYER_LOADING`/`PLAYER_SIMULATION`
/// pair uses `timeout: 0` and does not expire. A browser world therefore pays
/// only for the world-spawn ring when no refresh reaches the ticket store.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_play<T, P, S, E>(
    service: &crate::connection_service::ConnectionService,
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    home_source: SourceRef<'_, S>,
    entities: &E,
    mut state: State,
    initial_teleport_id: Option<i32>,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Keep the ticket guard alive for the entire connection. Movement remains
    // packet-driven through `FallTracker`; the browser timer separately runs
    // the shared entity diff for world changes that occur while idle.
    player_ticket: Option<PlayerTicket>,
    _initial_swing_cursor: Option<u64>,
    // The guard withdraws this connection's `PLAYER_LOADING` and
    // `PLAYER_SIMULATION` tickets when the task exits. Move it with each
    // tracked-view recenter or radius change so residency follows the player.
    mut player_ticket_guard: PlayerTicketGuard,
    mut view: ViewTracker,
    username: String,
    // World spawn for death-screen respawn. The join computation may inspect up
    // to 121 columns, so `apply_client_command` reuses this value when no
    // usable per-player bed point exists.
    world_spawn: Vec3,
    mut chunks_sent: usize,
    mut join_stream: crate::join_scheduler::JoinChunkStream<S>,
    // Optional per-stage join timing. Browser builds normally leave this off.
    join_trace: Option<Arc<JoinTrace>>,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Inbound placement packets publish neighbour-update requests here. The
    // browser timer below also drains the world-tick block lane, so changes
    // made while the client is idle reach the wire without waiting for input.
    block_ticks: &BlockTickFeed,
    // The browser timer drains explosions emitted by the shared world tick.
    explosions: &ExplosionFeed,
    // Weather changes are world-tick events. They remain a follow-up for this
    // connection loop because the shared weather feed is currently drained by
    // the native container-sync path.
    _weather: &WeatherFeed,
    // Bed clicks and wake-up packets are handled here. Voter counts and
    // skipped-night notifications require a container-sync timer, which is
    // not present on the browser target.
    sleep_vote: &SleepVote,
    _sleep_feed: &SleepFeed,
    // Commands are packet-driven: a chat-command frame arrives, the sink
    // answers, and system chat is sent back through the same dispatch path.
    commands: CommandSession,
    // Advancements and statistics are packet-driven: inbound packets update
    // criteria or request statistics, and the response uses the same dispatch
    // path.
    mut advancements: AdvancementManager,
    player_uuid: uuid::Uuid,
    // Border damage is produced by `BorderFeed::with`; the browser vitals
    // timer applies it alongside drowning, burning, effects, and hunger.
    border: &BorderFeed,
    // Resource-pack pushes have no browser timer producer, so this feed is not
    // drained by the browser loop.
    _resource_packs: &ResourcePackPushFeed,
    // Channel registration and inbound dispatch are packet-driven here.
    // Broadcast-queue draining requires a container-sync timer, so browser
    // connections do not receive queued broadcasts from this loop.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // The connection's game mode. The `change_game_mode` and `/gamemode` arms
    // mutate it locally.
    mut game_mode: GameMode,
    // The world's shared game rules, difficulty, and clock; `run_tick_loop`
    // updates this handle for every connection.
    world: &crate::world_state::WorldStateHandle,
    // This target has no filesystem-backed player store. The slot remains in
    // the signature so the shared connection setup can pass the same state;
    // no browser code publishes it.
    _live_save: &crate::live_save::LiveSaveSlot,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<PendingBreak> = None;
    let mut pending_prediction_ack = connection_prediction::PendingPredictionAck::default();
    let mut pending_relights = PendingRelights::default();
    let mut pending_tick_updates = VecDeque::new();
    let mut teleport_acknowledgements = initial_teleport_id.map(TeleportAcknowledgements::after_initial);
    let mut sprinting = false;
    let mut sneaking = false;
    let mut player_environment = crate::player_environment::PlayerEnvironment::default();
    let mut bow_draw: Option<BowDraw> = None;
    // `wasm_vitals_tick` applies item-use completion rules from its browser
    // timer, so a bite started here reaches its completion result.
    let mut item_in_use: Option<ItemInUse> = None;
    // `player_pos` feeds movement and vital checks. The browser timer applies
    // drowning, border damage, burning, status effects, and hunger; fall damage
    // remains driven by inbound `PlayerMoved` packets.
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut client_movement = ClientMovement::default();
    // A protocol with no player-loaded packet is loaded from the start.
    let mut client_loaded = !proto.sends_player_loaded();
    let mut client_load_wait = 0;
    let mut abilities = Abilities::for_mode(game_mode);
    // The rotation is stored alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = None;
    let mut vitals = PlayerVitals::default();
    let mut fall = FallTracker::default();
    let mut inventory = PlayerInventory::default();
    republish_inventory(entities.players(), player_uuid, &inventory);
    apply(
        conn,
        &mut state,
        proto.encode_recipe_book_add(&recipe_book_snapshot(&inventory), true),
    )
    .await?;
    // Opening and clicking a window are packet-driven here. Background
    // container synchronization requires the native container-sync timer and
    // is not run by the browser loop.
    let mut open_container: Option<OpenContainer> = None;
    let mut open_merchant: Option<OpenMerchant> = None;
    let mut container_sync = ContainerSync::default();
    let mut next_window_id: i32 = 0;
    // Composter rolls have no timer or native-only dependency on this target.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    let mut bone_meal_rng = SpawnRng::new(BONE_MEAL_BEHAVIOR_SEED);
    // Browser builds have no filesystem-backed player store, so experience
    // starts at its default values.
    let mut experience = crate::experience::PlayerExperience::default();
    let mut take_xp_delay: i32 = 0;
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let mut burn = crate::burning::BurnState::new();
    // The fire-contact ramp draws one value from the inclusive range `1..=3`.
    // Keep that draw on its own stream so standing in fire cannot shift which
    // roll a later block drop or composter insert sees.
    let mut burn_rng = SpawnRng::new(BURN_BEHAVIOR_SEED);
    // Block-drop rolls have no timer or native-only dependency on this target.
    let mut drops_rng = SpawnRng::new(crate::block_drops::BLOCK_DROPS_BEHAVIOR_SEED);
    // The per-player respawn point has no timer or native-only dependency.
    let mut respawn: Option<RespawnPoint> = None;
    // Leaving the End; this target keeps no player file, so only the in-session
    // credits flag is tracked.
    let mut end_exit = connection_travel::EndExit::new(false);
    let mut end_exit_preserved: Vec<(String, lodestone_core::Nbt)> = Vec::new();
    // The night-skip vote uses the player's roster key. Browser packet handlers
    // can register and wake voters; timer-fed vote counts remain native-only.
    let player_entity_id =
        player_ticket.as_ref().map_or(LOCAL_PLAYER_ENTITY_ID, |t| t.entity_id());
    // The initial join dump is unacknowledged, so this gate begins `true`.
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<PendingChunkBatch> = VecDeque::new();
    // Outgoing chat waits here until the shared broadcast queue can be drained.
    let mut outgoing_chat: Vec<String> = Vec::new();
    // Session announcements are stored for secure-profile validation.
    let mut chat_session: Option<crate::chat_session::ServerChatSession> = None;

    // Send the window-0 inventory snapshot before draining the inline join
    // stream. Browser inventory has default values because no player store is
    // available; the snapshot establishes the menu state used by later clicks.
    apply(conn, &mut state, join_inventory_snapshot(proto, &inventory)).await?;
    // Send the initial experience snapshot. Browser experience has default
    // values because no filesystem restore is available; explicit zeroes
    // initialize the client's bar.
    apply(conn, &mut state, join_experience(proto, &experience)).await?;
    republish_experience(entities.players(), player_uuid, &experience);
    // Send the armor/attribute snapshot derived from the current inventory so
    // the client can initialize its derived armor display.
    apply(conn, &mut state, join_attributes(proto, &inventory)).await?;

    // The browser timer uses a macrotask via the active page/worker global's
    // `setTimeout`; it drives both player vitals and publication of world
    // changes once per `WASM_VITALS_TICK_INTERVAL` period.
    let mut vitals_interval =
        crate::browser_timer::BrowserInterval::new(WASM_VITALS_TICK_INTERVAL);
    let browser_play_started = lodestone_time::Instant::now();
    let mut browser_vitals_ticks = 0_u64;
    let mut cooperative_relights = proto.detached_resident_light_compute().is_some();
    let mut cooperative_relight: Option<CooperativeRelight<'_>> = None;
    let mut connection_probe = crate::connection_progress::ConnectionProbe::start();
    use crate::connection_progress::ConnectionActivity;
    let mut pending_join_encodes = PendingJoinEncodes::new();
    let mut travel = connection_travel::TravelController::new(source);
    loop {
        service.admit_pass().await;
        travel.promote();
        let home = home_source;
        let active_source = travel.source();
        let source = match active_source.as_ref() {
            Some(other) => SourceRef::Dimension(other),
            None => home,
        };
        let dimension_handles = dimension_scoped_handles(active_source.as_ref());
        let block_entities = dimension_handles.block_entities.as_ref().unwrap_or(block_entities);
        let block_ticks = dimension_handles.block_ticks.as_ref().unwrap_or(block_ticks);
        let active_entities = ActiveEntities::new(world, entities, source.dimension());
        let mobs = active_entities.runtime.as_ref().map_or(mobs, |runtime| runtime.mobs());
        let entities = &active_entities;
        if join_stream.is_done() && pending_join_encodes.is_empty() {
            world.mark_initial_view_drained();
        }
        if let Some(probe) = connection_probe.as_mut() {
            probe.observe(crate::connection_progress::ConnectionProgress {
                running: true,
                elapsed: std::time::Duration::ZERO,
                passes: 0,
                client_loaded,
                center: view.center,
                radius: view.radius,
                owed_columns: view.loaded.len(),
                delivered_columns: view.delivered.len(),
                chunks_sent,
                remaining: join_stream.remaining() + usize::from(!pending_join_encodes.is_empty()),
                activity: ConnectionActivity::Select,
                activity_elapsed: std::time::Duration::ZERO,
                target: None,
                packet_id: None,
            });
        }
        let activity = |phase, target, packet_id| {
            if let Some(probe) = connection_probe.as_ref() {
                probe.activity(phase, target, packet_id);
            }
        };
        if cooperative_relights && cooperative_relight.is_none() && pending_relights.ready() {
            if pending_relights.front().is_some_and(|coordinate| !view.delivered.contains(&coordinate)) {
                pending_relights.pop_front();
            } else {
                let batch = pending_relights.batch(&view.delivered, true);
                let coordinates = batch.coordinates.clone();
                pending_relights.admit(&batch);
                let owned_source = source.shared_arc();
                let borrowed_home = home.get();
                cooperative_relight = Some(CooperativeRelight {
                    batch,
                    future: Box::pin(async move {
                        let source = owned_source.as_deref().unwrap_or(borrowed_home);
                        crate::worldgen_progress::measure_polls(
                            WorldgenTimingPhase::ConnectionRelight,
                            std::pin::pin!(compute_cooperative_relight(proto, source, coordinates)),
                        ).await
                    }),
                });
            }
        }
        if let Some(error) = world.initial_seed_error() {
            return return_initial_seed_error(conn, proto, &mut state, None, error).await;
        }
        let packet = tokio::select! {
            error = world.wait_initial_seed_failure() => {
                return return_initial_seed_error(conn, proto, &mut state, None, error).await;
            }
            prepared = std::future::poll_fn(|cx| travel.poll_prepared(cx, player_pos)), if travel.is_preparing() => {
                activity(ConnectionActivity::Publication, None, None);
                let Some(prepared) = prepared? else { continue; };
                let dimension_changed = matches!(&prepared, connection_travel::PreparedTravel::Dimension { .. });
                if dimension_changed { cooperative_relight = None; }
                if connection_travel::commit(
                    &mut travel, prepared, conn, proto, source, home, &mut state, &mut view,
                    &mut join_stream, &mut pending_join_encodes, &mut pending_chunk_batches,
                    &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                    &mut teleport_acknowledgements, &mut player_pos, player_rot,
                    &mut client_movement, &mut fall, &mut client_loaded, game_mode, world,
                    &mut player_ticket_guard, &mut streamer, entities.players(), player_entity_id,
                ).await?.is_some() && dimension_changed {
                    pending_break = None;
                    bow_draw = None;
                    item_in_use = None;
                    open_container = None;
                    open_merchant = None;
                }
                continue;
            }
            _ = std::future::ready(()), if !pending_tick_updates.is_empty() => {
                activity(ConnectionActivity::TickUpdates, None, None);
                send_pending_tick_block_updates(
                    conn,
                    proto,
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                    &mut pending_tick_updates,
                )
                .await?;
                None
            }
            result = std::future::poll_fn(|cx| match cooperative_relight.as_mut() {
                Some(job) => job.future.as_mut().poll(cx),
                None => std::task::Poll::Pending,
            }), if cooperative_relight.is_some() => {
                activity(ConnectionActivity::Relight, None, None);
                let batch = cooperative_relight.take().expect("the completed relight owns its batch").batch;
                match result {
                    RelightBatchOutcome::Committed(lights) => send_committed_relights(
                        conn, proto, source.get(), &mut state, &view.delivered,
                        &mut pending_relights, lights,
                    ).await?,
                    RelightBatchOutcome::Deferred => pending_relights.defer(&batch),
                    RelightBatchOutcome::Unsupported => {
                        cooperative_relights = false;
                        pending_relights.defer(&batch);
                    }
                    RelightBatchOutcome::Failed(error) => return Err(error.into()),
                }
                None
            }
            _ = std::future::ready(()), if !cooperative_relights && pending_relights.ready() => {
                activity(ConnectionActivity::Relight, None, None);
                send_next_relight(
                    conn,
                    proto,
                    source.get(),
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                )
                .await?;
                None
            }
            packet = conn.read_packet() => {
                match packet? {
                    Some(packet) => Some(packet),
                    // Clean disconnect. Browser connections have no
                    // filesystem-backed player store to persist here.
                    None => return Ok(ServeSummary { username, chunks_sent, inventory }),
                }
            }
            // Cancel-safe timer polling: a ready packet leaves the interval's
            // deadline untouched, so no timer event is lost.
            _ = vitals_interval.tick() => {
                activity(ConnectionActivity::Vitals, None, None);
                if let Some(sequence) = pending_prediction_ack.take() {
                    apply(conn, &mut state, proto.encode_block_changed_ack(sequence)).await?;
                }
                tick_client_load_timeout(&mut client_loaded, &mut client_load_wait);
                if player_tick_ready(world, client_loaded) {
                    wasm_vitals_tick(
                        conn,
                        proto,
                        source,
                        &mut state,
                        world,
                        border,
                        game_mode,
                        player_uuid,
                        &username,
                        player_pos,
                        &mut player_environment,
                        sprinting,
                        sneaking,
                        abilities.flying,
                        &mut vitals,
                        &mut inventory,
                        &mut advancements,
                        &mut drops_rng,
                        &mut burn,
                        &mut burn_rng,
                        &mut effects,
                        &mut item_in_use,
                        mobs,
                        block_ticks,
                        block_entities,
                    )
                    .await?;
                    travel.tick(
                        home, source, block_entities, mobs, player_entity_id,
                        player_pos, game_mode, world,
                    );
                    if travel.take_end_exit_contact() {
                        connection_travel::begin_end_exit(
                            conn, proto, &mut state, &mut end_exit, &mut end_exit_preserved,
                        ).await?;
                    }
                    browser_vitals_ticks = browser_vitals_ticks.saturating_add(1);
                    if browser_vitals_ticks == 1 || browser_vitals_ticks.is_multiple_of(20) {
                        tracing::debug!(
                            elapsed_ms = browser_play_started.elapsed().as_millis(),
                            vitals_ticks = browser_vitals_ticks,
                            world_tick = world.time().game_time,
                            chunks_sent,
                            join_remaining = join_stream.remaining(),
                            "browser play-loop heartbeat",
                        );
                    }
                }
                republish_inventory(entities.players(), player_uuid, &inventory);
                // The world tick can publish changes without inbound packets.
                publish_open_container(
                    conn, proto, &mut state, block_entities,
                    &mut open_container, &mut container_sync,
                )
                .await?;
                queue_tick_block_updates(
                    &mut pending_tick_updates,
                    &view.delivered,
                    block_ticks.drain_all(),
                );
                // Explosions have the same producer/consumer shape as block
                // changes: the shared world tick publishes them independently
                // of client input, so the timer must forward them as well.
                for detonation in explosions.drain_all() {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_explode(detonation.centre, detonation.radius),
                    )
                    .await?;
                }
                // Entity snapshots are the browser's item/mob visibility
                // path. Run the same diff used by the native connection timer
                // so a dropped item appears and falls even when the player
                // sends no movement packets.
                republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }
                None
            }
            encoded = std::future::poll_fn(|cx| pending_join_encodes.poll_next(cx)), if !pending_join_encodes.is_empty() => {
                let ((cx, cz), (incarnation, directive)) = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, &mut state, Some(0), error)
                            .await;
                    }
                };
                if !view.needs_delivery((cx, cz), incarnation, directive.stage) {
                    continue;
                }
                {
                    let _timing = PhaseTimer::start(WorldgenTimingPhase::WireSend, 1);
                    activity(ConnectionActivity::JoinBegin, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                    activity(ConnectionActivity::JoinColumn, Some((cx, cz)), None);
                    if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                        apply(conn, &mut state, proto.end_chunk_batch(0)).await?;
                        continue;
                    }
                    activity(ConnectionActivity::JoinEnd, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.end_chunk_batch(1)).await?;
                }
                if let Some(trace) = join_trace.as_ref() {
                    trace.mark("delivered", cx, cz);
                }
                chunks_sent += 1;
                if chunks_sent == 1 || chunks_sent.is_multiple_of(16)
                    || (join_stream.is_done() && pending_join_encodes.is_empty())
                {
                    crate::worldgen_progress::emit(
                        crate::worldgen_progress::WorldgenProgress::wire_delivered(
                            (cx, cz),
                            chunks_sent,
                            join_stream.remaining(),
                        ),
                    );
                    tracing::debug!(
                        elapsed_ms = browser_play_started.elapsed().as_millis(),
                        chunks_sent,
                        join_remaining = join_stream.remaining(),
                        world_tick = world.time().game_time,
                        "browser join stream progress",
                    );
                }
                continue;
            }
            next = async {
                tokio::select! {
                    next = join_stream.next(source) => Some(next),
                    _ = lodestone_time::browser_sleep(
                        crate::join_scheduler::JOIN_STREAM_SERVICE_BUDGET,
                    ) => None,
                }
            }, if pending_join_encodes.can_admit() && !join_stream.is_done() => {
                let Some(next) = next else {
                    continue;
                };
                let Some(((cx, cz), payload)) = (match next {
                    Ok(next) => next,
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, &mut state, Some(0), error)
                            .await;
                    }
                }) else {
                    continue;
                };
                activity(ConnectionActivity::JoinAdmission, Some((cx, cz)), None);
                let Some(incarnation) = view.incarnation((cx, cz)) else {
                    continue;
                };
                if let Some(owned_source) = source.shared_arc() {
                    let trace = join_trace.clone();
                    pending_join_encodes.push(Box::pin(async move {
                        encode_column_owned(proto, owned_source, cx, cz, trace, payload)
                            .await
                            .map(|directive| ((cx, cz), (incarnation, directive)))
                    }));
                    continue;
                }
                let directive = match encode_column(
                    proto,
                    source,
                    cx,
                    cz,
                    join_trace.as_deref(),
                    payload,
                )
                .await {
                    Ok(directive) => directive,
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, &mut state, Some(0), error)
                            .await;
                    }
                };
                {
                    let _timing = PhaseTimer::start(WorldgenTimingPhase::WireSend, 1);
                    activity(ConnectionActivity::JoinBegin, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                    activity(ConnectionActivity::JoinColumn, Some((cx, cz)), None);
                    if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                        apply(conn, &mut state, proto.end_chunk_batch(0)).await?;
                        continue;
                    }
                    activity(ConnectionActivity::JoinEnd, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.end_chunk_batch(1)).await?;
                }
                if let Some(trace) = join_trace.as_ref() {
                    trace.mark("delivered", cx, cz);
                }
                chunks_sent += 1;
                if chunks_sent == 1 || chunks_sent.is_multiple_of(16) || join_stream.is_done() {
                    crate::worldgen_progress::emit(
                        crate::worldgen_progress::WorldgenProgress::wire_delivered(
                            (cx, cz),
                            chunks_sent,
                            join_stream.remaining(),
                        ),
                    );
                    tracing::debug!(
                        elapsed_ms = browser_play_started.elapsed().as_millis(),
                        chunks_sent,
                        join_remaining = join_stream.remaining(),
                        world_tick = world.time().game_time,
                        "browser join stream progress",
                    );
                }
                continue;
            }
        };
        if let Some((packet_id, payload)) = packet {
            activity(ConnectionActivity::Dispatch, None, Some(packet_id));
            let mut dimension_reset = None;
            {
                let dispatch = std::pin::pin!(dispatch_play_packet(
                    conn,
                    proto,
                    source,
                    home,
                    &mut state,
                    proto.retains_initial_column_light().then_some(&mut pending_relights),
                    &mut view,
                    &player_ticket_guard,
                    &mut pending_keep_alive,
                    &mut pending_break,
                    &mut pending_prediction_ack,
                    &mut teleport_acknowledgements,
                    &mut player_pos,
                    &mut client_movement,
                    &mut player_rot,
                    &mut fall,
                    &mut vitals,
                    &mut burn,
                    world,
                    &mut inventory,
                    block_entities,
                    &mut open_container,
                    &mut open_merchant,
                    &mut container_sync,
                    &mut next_window_id,
                    mobs,
                    &mut sprinting,
                    &mut sneaking,
                    &mut awaiting_chunk_batch_ack,
                    &mut pending_chunk_batches,
                    Some(&mut join_stream),
                    &commands,
                    &mut advancements,
                    player_uuid,
                    false,
                    &mut outgoing_chat,
                    &mut chat_session,
                    entities.players(),
                    block_ticks,
                    _resource_packs,
                    &mut client_loaded,
                    &mut composter_rng,
                    &mut bone_meal_rng,
                    &mut experience,
                    &mut effects,
                    &mut drops_rng,
                    client_channels,
                    plugin_channels,
                    &mut game_mode,
                    &mut abilities,
                    &mut respawn,
                    sleep_vote,
                    border,
                    player_entity_id,
                    &username,
                    world_spawn,
                    // `None`: no timer tick counter is available for dig duration.
                    // Hardness and range still validate; only the timing check is
                    // skipped.
                    None,
                    &mut bow_draw,
                    &mut item_in_use,
                    &mut dimension_reset,
                    &mut end_exit,
                    packet_id,
                    &payload,
                ));
                crate::worldgen_progress::measure_polls(
                    WorldgenTimingPhase::ConnectionDispatch, dispatch,
                ).await?;
            }
            activity(ConnectionActivity::Publication, None, Some(packet_id));
            player_tick_ready(world, client_loaded);
            republish_inventory(entities.players(), player_uuid, &inventory);
            if end_exit.take_respawn() {
                dimension_reset = connection_travel::perform_respawn(
                    conn, proto, &mut state, home.get(), source.get(), &mut respawn, world_spawn, game_mode,
                    &mut teleport_acknowledgements, block_ticks, true,
                ).await?;
            }
            if let Some(connection_travel::DimensionReset { target, route }) = dimension_reset.take() {
                cooperative_relight = None;
                let (destination, arrival_dimension) = match &route {
                    crate::respawn::Route::Home => (home.get(), home.dimension()),
                    crate::respawn::Route::Current => (source.get(), source.dimension()),
                    crate::respawn::Route::Sibling(other) => (
                        other.as_ref(),
                        other.dimension().unwrap_or(crate::dimension::Dimension::Overworld),
                    ),
                };
                let ticket_transfer = connection_travel::prepare_ticket_transfer(
                    &player_ticket_guard, home.get(), destination, target, view.radius,
                    world.simulation_distance(),
                )?;
                connection_travel::reset_stream(
                    conn, proto, &mut state, target, &mut view, &mut join_stream,
                    &mut pending_join_encodes, &mut pending_chunk_batches,
                    &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                ).await?;
                for directive in streamer.reset_dimension(proto) {
                    apply(conn, &mut state, directive).await?;
                }
                connection_travel::finish_ticket_transfer(
                    &mut player_ticket_guard, ticket_transfer, source.get(), destination,
                );
                connection_travel::reset_player(
                    target, arrival_dimension, &mut player_pos, &mut client_movement,
                    &mut fall, &mut client_loaded, proto.sends_player_loaded(), world, entities.players(), player_entity_id,
                );
                match route {
                    crate::respawn::Route::Home => travel.stage(connection_travel::Destination::Home),
                    crate::respawn::Route::Current => {}
                    crate::respawn::Route::Sibling(other) => {
                        travel.stage(connection_travel::Destination::Dimension(other));
                    }
                }
                pending_break = None;
                bow_draw = None;
                item_in_use = None;
                open_container = None;
                open_merchant = None;
                continue;
            }
            // Flush advancement changes caused by the packet just dispatched.
            if let Some(update) = advancements.flush_dirty(player_uuid, true) {
                apply(conn, &mut state, proto.encode_update_advancements(&update)).await?;
            }
            // Publish chat to the shared registry when one exists. Without a
            // registry, echo it directly to this connection.
            for message in outgoing_chat.drain(..) {
                let line = ChatLine {
                    sender: username.clone(),
                    message,
                };
                match entities.players() {
                    Some(registry) => registry.say(&line.sender, &line.message),
                    None => {
                        apply(conn, &mut state, proto.encode_system_chat(&line.rendered())).await?;
                    }
                }
            }
            // Update the player's streamed position from the packet state.
            if let (Some(ticket), Some(registry), Some((x, y, z))) =
                (player_ticket.as_ref(), entities.players(), player_pos)
            {
                registry.set_position(ticket.entity_id(), Vec3::new(x, y, z));
                registry.set_on_ground(ticket.entity_id(), client_movement.on_ground);
            }
            // The pickup sweep is packet-driven, so run it after each dispatched
            // packet while the player's position is available.
            if let Some((x, y, z)) = player_pos {
                let pickups = collect_nearby_items(
                    mobs,
                    &mut inventory,
                    Vec3::new(x, y, z),
                    &mut advancements,
                    player_uuid,
                    world.time().game_time.saturating_mul(50),
                    matches!(game_mode, GameMode::Creative),
                );
                // Send pickup frames before slot updates and entity streaming so
                // the client can animate an item entity that still exists.
                for take in &pickups.takes {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_take_item_entity(
                            take.item_entity_id,
                            // Use the local player entity id for the pickup reply.
                            LOCAL_PLAYER_ENTITY_ID,
                            take.amount,
                        ),
                    )
                    .await?;
                }
                for native in pickups.changed {
                    if let Some(menu_slot) = window_zero_menu_slot(native) {
                        apply(
                            conn,
                            &mut state,
                            proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                        )
                        .await?;
                    }
                }
                // Absorb nearby experience orbs as part of the packet-driven sweep;
                // browser players receive the resulting pickup behavior here.
                if let Some(absorbed) = collect_nearby_orbs(
                    mobs,
                    Vec3::new(x, y, z),
                    &mut experience,
                    &mut take_xp_delay,
                ) {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_take_item_entity(
                            absorbed.orb_entity_id,
                            LOCAL_PLAYER_ENTITY_ID,
                            1,
                        ),
                    )
                    .await?;
                    republish_experience(entities.players(), player_uuid, &experience);
                    apply(
                        conn,
                        &mut state,
                        proto.encode_set_experience(
                            experience.progress(),
                            experience.level(),
                            experience.total(),
                        ),
                    )
                    .await?;
                }
            }
            // Publish the latest player rotation to the entity registry.
            if let (Some(ticket), Some(registry), Some(rotation)) =
                (player_ticket.as_ref(), entities.players(), player_rot)
            {
                registry.set_rotation(ticket.entity_id(), rotation);
            }
            republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
            for directive in stream_pass(
                proto,
                entities,
                &mut streamer,
                &mut player_list,
                player_ticket.as_ref(),
            ) {
                apply(conn, &mut state, directive).await?;
            }
        }

    }
}
