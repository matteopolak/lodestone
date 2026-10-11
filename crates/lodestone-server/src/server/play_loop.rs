//! The play-state loops: the native and browser `serve_play` event loops, vitals ticking, stall watch and the client-loaded gate.

use super::*;

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

/// Serves a connection that has just reached [`State::Play`] until the client
/// disconnects. One loop for every target.
///
/// This is where [`serve_connection`] hands off once the join sequence and initial chunk view are
/// out. Everything here runs on the connection's own schedule rather than strictly in response to
/// one inbound packet:
/// * a server-initiated keep-alive at vanilla's fixed 15-second interval, with the same-length
///   disconnect timeout (see `KEEP_ALIVE_INTERVAL` for why that is one interval, not two);
/// * a periodic time-of-day broadcast at vanilla's every-20-ticks cadence (`TIME_SYNC_INTERVAL`);
/// * the 50 ms vitals pass ([`player_tick`]), the container-sync pass that publishes the world's
///   changes and the shared feeds, and the entity-stream pass;
/// * view streaming whenever a [`ServerBound::PlayerMoved`] crosses into a new chunk column;
///
/// all layered over the same entity-streaming pass the join sequence uses.
///
/// # Platform seams
///
/// A browser has no tokio timer, no threads and no directory, so the differences live in
/// [`play_platform`] and the persistence seam rather than in a second loop: [`PlayInterval`] is the
/// host timer there and never fires the keep-alive (the in-process stream's peer cannot go quiet
/// independently), [`RelightDriver`] settles light cooperatively instead of on a worker,
/// [`within_budget`] bounds the join stream wait, and the player record saves into the memory
/// store of [`crate::live_save::LiveSaveSlot`].
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure, or
/// [`ServerError::KeepAliveTimeout`] if the client does not echo a challenge in time.
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
    #[cfg(not(target_arch = "wasm32"))]
    profile_key_issuers: Option<lodestone_auth::MojangPublicKeys>,
    // Separate from issuer availability: whether the host requires a player
    // with no adopted session to sign chat.
    #[cfg(not(target_arch = "wasm32"))]
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
    let mut relight = RelightDriver::new(proto);
    #[cfg(target_arch = "wasm32")]
    let enforce_secure_profile = false;
    let mut teleport_acknowledgements = initial_teleport_id.map(TeleportAcknowledgements::after_initial);
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut client_movement = ClientMovement::default();
    // A protocol with no player-loaded packet is loaded from the start.
    let mut client_loaded = !proto.sends_player_loaded();
    let mut client_load_wait = 0;
    player_tick_ready(world, client_loaded);
    let mut abilities = Abilities::for_mode(game_mode);
    let native_player = native_player.as_ref();
    // Resolve the per-player store once from the source. `player_uuid` is the
    // key for the stored file, and the loop reuses this handle for its saves.
    let player_store = player_store(source.get(), live_save);
    let mut map_session = start_map_session(world, player_store.as_ref(), player_uuid, &username);
    let saved_player = player_store
        .as_ref()
        .and_then(|store| store.read(player_uuid).ok().flatten());
    // Fields `crate::player_data` does not model ride in `preserved_player_fields`, so a full
    // load/modify/save cycle stays lossless; see `PlayerData::preserved`.
    let RestoredPlayer {
        preserved: mut preserved_player_fields,
        mut vitals,
        mut inventory,
        mut experience,
        rotation,
    } = restore_player(saved_player.as_ref(), native_player);
    // The rotation is stored alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = rotation;
    let mut fall = FallTracker::default();
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
    // Closes this player's shulker lid however the task ends.
    let _lid_guard = crate::lid_viewers::LidGuard::new(block_entities.clone(), block_ticks.clone(), player_uuid);
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
    // This connection's raised shield and disable cooldown.
    let mut shield = crate::shield::ShieldState::default();
    // Window identifiers start at `0`; each open increments before use and
    // wraps with [`open_container_screen`]'s `% 100 + 1` rule.
    let mut next_window_id: i32 = 0;
    // This connection's composter roll stream — see
    // `COMPOSTER_BEHAVIOR_SEED` and `dispatch_play_packet`'s parameter comment.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    let mut bone_meal_rng = SpawnRng::new(BONE_MEAL_BEHAVIOR_SEED);
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
    // Leaving the connection by any path gets the player out of bed, which
    // clears their vote entry and destroys a straw bed they lay in.
    // The vote and the skip broadcast are the world's: `dispatch_play_packet`
    // records this connection's `lay_down`/`get_up` on the vote, the
    // container-sync pass feeds it the voter count, and a skip reaches this
    // connection through its own cursor on the feed.
    let sleep_vote = world.sleep_vote();
    let _sleep_seat = crate::sleep::SleepSeat::new(sleep_vote, player_entity_id);
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
    // This connection's read positions in the world's chat, swing, plugin-channel and sleep feeds.
    let mut feeds = player_tick::ConnectionFeeds::new(entities.players(), initial_swing_cursor, world);
    // The keep-alive timer collapses a backlog into one tick (`Delay`, not tokio's default
    // `Burst`): a pass that overran two intervals must not write a challenge and find it
    // unanswered in the same instant, which would kick a healthy client with
    // `disconnect.timeout` for a stall that was entirely ours.
    let mut keep_alive_tick = PlayInterval::keep_alive(KEEP_ALIVE_INTERVAL);
    // When the outstanding challenge was written. The timeout is measured from
    // here plus `LoopStallWatch::unserviced` rather than from the interval's own
    // cadence — see that type's doc comment.
    let mut keep_alive_sent_at = crate::tick::PlayTimerInstant::now();
    let mut watch = LoopStallWatch::new();
    let mut probe = PlayProbe::start();
    // Every timer is anchored one period out, so none fires in the instant the join sequence
    // finished.
    let mut time_sync_tick = PlayInterval::catching_up(TIME_SYNC_INTERVAL);
    let mut vitals_tick = PlayInterval::catching_up(VITALS_TICK_INTERVAL);
    let mut container_sync_tick = PlayInterval::catching_up(CONTAINER_SYNC_INTERVAL);
    // Delayed, so an overrun (a chunk strip or container click) produces one streaming pass at a
    // time rather than back-to-back diffs over state that cannot have changed between them.
    let mut entity_stream_tick = PlayInterval::delayed(ENTITY_STREAM_INTERVAL);
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
    use crate::connection_progress::ConnectionActivity;

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
        probe.observe(|| crate::connection_progress::ConnectionProgress {
            running: true,
            elapsed: Duration::ZERO,
            passes: 0,
            client_loaded,
            center: view.center,
            radius: view.radius,
            owed_columns: view.loaded.len(),
            delivered_columns: view.delivered.len(),
            chunks_sent,
            remaining: join_stream.remaining() + usize::from(!pending_join_encodes.is_empty()),
            activity: ConnectionActivity::Select,
            activity_elapsed: Duration::ZERO,
            target: None,
            packet_id: None,
        });
        let synchronous_relight =
            relight.pump(proto, source, home, &mut pending_relights, &view.delivered);
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
                probe.activity(ConnectionActivity::Publication, None, None);
                let Some(prepared) = prepared? else {
                    watch.pass("travel_declined");
                    continue;
                };
                let dimension_changed = matches!(&prepared, connection_travel::PreparedTravel::Dimension { .. });
                if dimension_changed {
                    relight.clear();
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
                        crate::lid_viewers::release_and_publish(block_entities, block_ticks, player_uuid);
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
                probe.activity(ConnectionActivity::TickUpdates, None, None);
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
                probe.activity(ConnectionActivity::Relight, None, None);
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
            outcome = relight.finished(), if relight.is_running() => {
                watch.enter();
                probe.activity(ConnectionActivity::Relight, None, None);
                relight
                    .complete(outcome, conn, proto, source.get(), &mut state, &view.delivered, &mut pending_relights)
                    .await?;
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
                let _timing = PhaseTimer::start(WorldgenTimingPhase::WireSend, 1);
                probe.activity(ConnectionActivity::JoinColumn, Some((cx, cz)), None);
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
                if join_batch_size as usize >= JOIN_BATCH_COLUMNS
                    || (join_stream.is_done() && pending_join_encodes.is_empty())
                {
                    apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                    join_batch_open = false;
                }
                probe.wire_delivered((cx, cz), chunks_sent, join_stream.remaining(), join_stream.is_done() && pending_join_encodes.is_empty());
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
            chunk = within_budget(
                crate::join_scheduler::JOIN_STREAM_SERVICE_BUDGET,
                join_stream.next(source),
            ), if pending_join_encodes.can_admit() && !join_stream.is_done() => {
                watch.enter();
                probe.activity(ConnectionActivity::JoinAdmission, None, None);
                let chunk = match chunk {
                    // A worker can legitimately take hundreds of milliseconds
                    // on a cold terrain column. Dropping only this borrowed
                    // future is safe: `ColumnPipeline::next` leaves the head
                    // handle in place until it has been emitted, so the next
                    // pass observes the same ordered result. Most importantly,
                    // the socket-read arm gets polled at least once per budget.
                    None => {
                        watch.pass("join_stream");
                        continue;
                    }
                    Some(Ok(chunk)) => chunk,
                    Some(Err(error)) => {
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
                        if join_batch_size as usize >= JOIN_BATCH_COLUMNS
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
                probe.activity(ConnectionActivity::Dispatch, None, Some(packet_id));
                let pending_keep_alive_before_packet = pending_keep_alive;
                {
                    let dispatch = std::pin::pin!(dispatch_play_packet(
                        conn,
                        proto,
                        source,
                        home,
                        &mut state,
                        RelightDriver::takes_queue(proto, source).then_some(&mut pending_relights),
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
                        #[cfg(not(target_arch = "wasm32"))]
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
                        &mut shield,
                        &mut dimension_reset,
                        &mut end_exit,
                        packet_id,
                        &payload,
                    ));
                    crate::worldgen_progress::measure_polls(
                        WorldgenTimingPhase::ConnectionDispatch, dispatch,
                    ).await?;
                }
                probe.activity(ConnectionActivity::Publication, None, Some(packet_id));
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
                    relight.clear();
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
                    crate::lid_viewers::release_and_publish(block_entities, block_ticks, player_uuid);
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
                stream_and_place(
                    conn, &mut state, proto, entities, &mut streamer, &mut player_list,
                    player_ticket.as_ref(), mobs, &mut player_pos, &mut player_rot, &mut teleport_acknowledgements,
                ).await?;
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
                stream_and_place(
                    conn, &mut state, proto, entities, &mut streamer, &mut player_list,
                    player_ticket.as_ref(), mobs, &mut player_pos, &mut player_rot, &mut teleport_acknowledgements,
                ).await?;
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
                probe.activity(ConnectionActivity::Vitals, None, None);
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

                tick_maps(
                    conn,
                    proto,
                    &mut state,
                    &mut map_session,
                    world,
                    source.get(),
                    source.dimension(),
                    player_pos,
                    player_rot,
                    &inventory,
                    mobs,
                )
                .await?;

                player_tick::run(player_tick::PlayerTick {
                    conn: &mut *conn,
                    proto,
                    source,
                    home,
                    state: &mut state,
                    world,
                    border,
                    game_mode,
                    player_uuid,
                    username: &username,
                    player_pos,
                    player_rot,
                    player_environment: &mut player_environment,
                    sprinting,
                    sneaking,
                    flying: abilities.flying,
                    vitals: &mut vitals,
                    inventory: &mut inventory,
                    advancements: &mut advancements,
                    drops_rng: &mut drops_rng,
                    burn: &mut burn,
                    burn_rng: &mut burn_rng,
                    effects: &mut effects,
                    item_in_use: &mut item_in_use,
                    shield: &mut shield,
                    raid_omen_position: &mut raid_omen_position,
                    mobs,
                    block_ticks,
                    block_entities,
                    pending_break: &mut pending_break,
                    dig_tick: u64::try_from(ticks_since(play_start)).unwrap_or(0),
                    open_container: &mut open_container,
                    container_sync: &mut container_sync,
                    relights: RelightDriver::takes_queue(proto, source).then_some(&mut pending_relights),
                })
                .await?;

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
                    conn, proto, source.get(), &mut state, &mut inventory, block_entities, block_ticks, mobs,
                    entities.players(), player_uuid, player_pos, player_rot, &mut drops_rng, &mut experience,
                    &mut open_container, &mut container_sync, &mut open_merchant,
                )
                .await?;
                // Keep tick updates for visible columns ordered, but send them
                // outside this timer arm so a burst cannot block socket reads.
                queue_tick_block_updates(
                    &mut pending_tick_updates,
                    &view.delivered,
                    block_ticks.drain_all(),
                );
                player_tick::publish_world_events(
                    conn,
                    proto,
                    &mut state,
                    block_ticks,
                    explosions,
                    weather,
                    mobs,
                    player_uuid,
                    &mut player_pos,
                    player_rot,
                    &mut teleport_acknowledgements,
                )
                .await?;
                // Everything the shared feeds owe this connection: the sleep vote's voter
                // count and night skips, resource packs, chat, swings, directed command
                // effects and plugin-channel broadcasts.
                player_tick::publish_connection_feeds(
                    conn,
                    proto,
                    &mut state,
                    &mut feeds,
                    player_tick::FeedDrain {
                        players: entities.players(),
                        world,
                        resource_packs,
                        plugin_channels,
                        client_channels,
                        player_uuid,
                        player_entity_id,
                        username: &username,
                        game_mode: &mut game_mode,
                        abilities: &mut abilities,
                        inventory: &mut inventory,
                        advancements: &mut advancements,
                        effects: &mut effects,
                        vitals: &mut vitals,
                        experience: &mut experience,
                        player_pos: &mut player_pos,
                        player_rot: &mut player_rot,
                        teleport_acknowledgements: &mut teleport_acknowledgements,
                    },
                )
                .await?;
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

/// Wears the item in `native` by `amount`, resending the slot and, when it
/// breaks, removing it and sending the break event first so the client plays the
/// break sound and particles from the stack that is still there.
pub(super) async fn wear_item_in_slot<T, P>(
    conn: &mut lodestone_net::Connection<T>,
    state: &mut State,
    proto: &P,
    inventory: &mut PlayerInventory,
    native: usize,
    amount: u32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if amount == 0 {
        return Ok(());
    }
    let Some(mut stack) = inventory.native(native).cloned() else { return Ok(()) };
    let broke = crate::shield::wear(&mut stack, amount);
    if broke {
        // 47 breaks the main-hand item and 48 the off-hand one.
        let event = if native == OFFHAND_NATIVE { 48 } else { 47 };
        apply(conn, state, proto.encode_entity_event(LOCAL_PLAYER_ENTITY_ID, event)).await?;
    }
    let remaining = (!broke).then_some(stack);
    inventory.set_native(native, remaining.clone());
    if let Some(menu_slot) = window_zero_menu_slot(native) {
        apply(conn, state, proto.encode_container_slot(0, 0, menu_slot, remaining.as_ref())).await?;
    }
    Ok(())
}
