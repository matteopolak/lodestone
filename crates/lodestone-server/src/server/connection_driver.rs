//! The shared connection driver: handshake, status and login handling, the initial chunk stream and join sequence, then hand-off to the play loop.

use super::*;

pub(super) async fn apply<T: Transport>(
    conn: &mut Connection<T>,
    state: &mut State,
    directive: ServerDirective,
) -> Result<(), ServerError> {
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            conn.write_packet_in(*state, packet_id, &payload).await?;
        }
        ServerDirective::SetState(next) => *state = next,
        ServerDirective::SetCompression(threshold) => conn.set_compression(threshold),
        ServerDirective::EnableEncryption(secret) => conn.enable_encryption(&secret)?,
        ServerDirective::None => {}
    }
    Ok(())
}

/// Ends an already-written chunk batch before reporting an encoding failure.
///
/// Callers that only accumulated directives locally pass `None`, so the client
/// never observes an unmatched batch marker. A batch beginning on the wire must
/// always have its matching end marker before the connection is disconnected.
pub(super) async fn return_chunk_encode_error<T, P, R>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    written_batch_size: Option<i32>,
    error: ChunkEncodeError,
) -> Result<R, ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if let Some(batch_size) = written_batch_size {
        apply(conn, state, proto.end_chunk_batch(batch_size)).await?;
    }
    apply(
        conn,
        state,
        proto.encode_disconnect(*state, &chunk_encode_failure_reason()),
    )
    .await?;
    Err(ServerError::ChunkEncode(error))
}

pub(super) async fn return_initial_seed_error<T, P, R>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    written_batch_size: Option<i32>,
    error: ChunkEncodeError,
) -> Result<R, ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if let Some(batch_size) = written_batch_size {
        apply(conn, state, proto.end_chunk_batch(batch_size)).await?;
    }
    let reason = Text::literal(format!("Failed to prepare world population: {error}"));
    apply(conn, state, proto.encode_disconnect(*state, &reason)).await?;
    Err(ServerError::InitialSeed(error))
}

/// Shared implementation for the connection wrappers. Feed-carrying wrappers
/// pass live handles; compatibility wrappers pass defaults. [`SourceRef`]
/// selects borrowed or shared chunk generation without changing packet flow.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_connection_inner<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    entities: &E,
    view_radius: i32,
    // The largest radius this connection may request; it is separate from the
    // `view_radius` used for the initial join. Compatibility wrappers use the
    // join radius, while integrated hosts supply their configured ceiling.
    max_view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Chunk-ticket state shared with the store that owns the connection's
    // loaded columns. A default handle gives compatibility wrappers isolated
    // ticket state; integrated hosts pass `ChunkStore::tickets()`.
    tickets: &TicketStoreHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // World-tick weather transitions drained by `serve_play`'s
    // `container_sync_tick` arm. A default feed produces no transitions.
    weather: &WeatherFeed,
    // Night-skip votes recorded by packet dispatch and consumed by the world
    // tick loop. A default vote/feed pair leaves night skipping disabled.
    sleep_vote: &SleepVote,
    // Night-skip notifications drained by `serve_play` into `encode_set_time`.
    sleep_feed: &SleepFeed,
    // Command handlers. `CommandDispatch::none()` provides fail-closed
    // behavior for wrappers without a host command dispatcher.
    commands: &CommandDispatch,
    // World border state used for join snapshots and per-tick border damage.
    // A default feed describes an unconfigured border.
    border: &BorderFeed,
    // Server-initiated resource-pack pushes drained by `serve_play`.
    resource_packs: &ResourcePackPushFeed,
    // Wire-level plugin messaging handlers and the server-to-client broadcast
    // queue, drained by `serve_play` alongside resource-pack pushes.
    plugin_channels: &PluginChannelRegistry,
    // Shared game rules, difficulty, and world clock. Integrated hosts pass the
    // handle updated by their world tick; isolated wrappers use a default.
    world: &crate::world_state::WorldStateHandle,
    // Continuously refreshed player-save mirror published by `serve_play`.
    // Integrated shutdown reads the shared slot; isolated wrappers use a
    // default slot.
    live_save: &crate::live_save::LiveSaveSlot,
    // Ops, whitelist, and ban lists consulted at `LoginStart`. A default access
    // handle admits everyone and grants no operator role; hosts may provide
    // configured lists.
    #[cfg(not(target_arch = "wasm32"))] access: &crate::access::AccessHandle,
    // The remote address this connection came from, for the IP ban list. `None`
    // for an in-memory duplex, which has no address — and an IP ban therefore
    // cannot apply to singleplayer, which is correct rather than a gap.
    #[cfg(not(target_arch = "wasm32"))] peer_ip: Option<std::net::IpAddr>,
    // `None` selects offline mode and sends `login_success` directly. `Some`
    // selects the encryption handshake and sends `login_success` after the
    // session server confirms the client's identity.
    #[cfg(not(target_arch = "wasm32"))] online_mode: Option<&OnlineModeConfig>,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    conn.set_outbound_ids(proto.outbound_id_map());
    let mut state = State::Handshaking;
    let mut username: Option<String> = None;
    // Keep the player entity UUID alongside `username`; it must match the UUID
    // echoed by `login_success` so clients resolve the spawned player correctly.
    let mut login_uuid: Option<uuid::Uuid> = None;
    // A configured online-mode listener is not enough: only this flag, set
    // after `hasJoined` replaces the claimed identity, authorizes profile-key
    // provenance enforcement in Play.
    #[cfg(not(target_arch = "wasm32"))]
    let mut online_authenticated = false;
    // `Some` while an encryption response is outstanding: retain the RSA
    // keypair needed to decrypt it and the verify-token challenge it must echo.
    // The response arm consumes this value, so a second response finds no
    // outstanding challenge and cannot reuse a keypair.
    #[cfg(not(target_arch = "wasm32"))]
    let mut pending_encryption: Option<(ServerKeyPair, [u8; lodestone_net::VERIFY_TOKEN_LEN])> =
        None;
    let mut streamer = EntityStreamer::default();
    let mut player_list = PlayerListStreamer::default();
    // Vanilla's own status packet listener's own "has requested status"
    // field: one status reply per
    // connection, a second request is a disconnect.
    let mut status_requested = false;
    // This connection's declared channel support, populated from
    // its `minecraft:register`/`minecraft:unregister` custom payloads — first
    // during Configuration (the arm below), then in Play via the same
    // `ServerBound::CustomPayload` arm in `dispatch_play_packet`. It is the
    // per-connection filter the broadcast drain in `serve_play` applies.
    let mut client_channels = ClientChannels::default();
    // The mode this connection joins in, absent a saved per-player value
    // below: `WorldStateHandle::default_game_mode`, `/defaultgamemode`'s own
    // read side — Survival until a host changes it. A runtime switch (the
    // `change_game_mode` packet, or `/gamemode`) moves it from there.
    // `serve_play` takes ownership of it at the Play handoff.
    let game_mode = world.default_game_mode();

    // A legacy protocol can finish login without receiving the modern
    // configuration acknowledgements. Queue the same play-transition event
    // used by the wire path so both routes share the complete join sequence.
    let mut pending_event: Option<ServerBound> = None;
    loop {
        let event = if let Some(event) = pending_event.take() {
            event
        } else {
            let Some((packet_id, payload)) = conn.read_packet().await? else {
                break;
            };
            proto.decode(state, packet_id, &payload)
        };

        match event {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            // Status requests are one-shot per connection: answer the first
            // request and disconnect on any subsequent one. Repeating requests
            // would let a peer hold the connection open without useful work.
            ServerBound::StatusRequest => {
                if status_requested {
                    return Err(ServerError::StatusRequestHandled);
                }
                status_requested = true;
                // `players_online` is reported as `0` because this crate has no
                // cross-connection player registry to count: a status request
                // arrives on its *own* connection, before and independent of
                // any join, so a per-connection loop cannot see the sessions
                // other connections are serving. Everything else in the row a
                // client renders (MOTD, cap, version, protocol) is real. See
                // `docs/server-status.md` for what a truthful count needs.
                let directive = proto.encode_status_response(
                    STATUS_MOTD,
                    0,
                    STATUS_MAX_PLAYERS,
                    &[],
                    None,
                    false,
                );
                apply(conn, &mut state, directive).await?;
            }
            // Ping requests echo the payload, then close. A ping does not
            // require a preceding status request.
            ServerBound::PingRequest { time } => {
                apply(conn, &mut state, proto.encode_pong_response(time)).await?;
                return Err(ServerError::StatusRequestHandled);
            }
            ServerBound::LoginStart {
                username: name,
                uuid,
            } => {
                // Validate the username at login start and send a reason when
                // it fails. Offline-mode UUIDs derive from the username and
                // player data is persisted under that UUID, so control
                // characters must not reach storage.
                //
                // Not merely cosmetic: an offline-mode server derives the account
                // uuid from the username and persists player data under it, so a
                // name carrying control characters is a name that reaches storage.
                if !is_valid_player_name(&name) {
                    let directive = proto.encode_disconnect(state, &invalid_username_reason());
                    apply(conn, &mut state, directive).await?;
                    return Err(ServerError::InvalidUsername);
                }
                // Apply access-list checks after username validation and before
                // `login_success`, so a refused player never reaches
                // Configuration. The online-player count is `0` because this
                // per-connection loop has no cross-connection registry; the
                // player limit is therefore inert while bans and whitelists
                // remain active.
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(refusal) = access.may_join(uuid, peer_ip, 0) {
                    let reason = Text::literal(refusal.message());
                    let directive = proto.encode_disconnect(state, &reason);
                    apply(conn, &mut state, directive).await?;
                    return Err(ServerError::AccessDenied(refusal.message()));
                }
                username = Some(name.clone());
                login_uuid = Some(uuid);

                // online mode sends an encryption request instead
                // of finishing login now — `login_success` is deferred to the
                // `EncryptionResponse` arm below, once the session server has
                // confirmed the client's identity. `encode_encryption_request`
                // returning `ServerDirective::None` means this protocol has no
                // wire support for it (the default every implementor but
                // `V770ServerProtocol` gets), so that also falls back to an
                // offline login rather than sending a request nothing would
                // answer.
                #[cfg(not(target_arch = "wasm32"))]
                let sent_encryption_request = if online_mode.is_some() {
                    let keypair = ServerKeyPair::generate()?;
                    let verify_token = generate_verify_token();
                    let directive =
                        proto.encode_encryption_request(keypair.public_key_der(), &verify_token);
                    if matches!(directive, ServerDirective::None) {
                        false
                    } else {
                        apply(conn, &mut state, directive).await?;
                        pending_encryption = Some((keypair, verify_token));
                        true
                    }
                } else {
                    false
                };
                #[cfg(target_arch = "wasm32")]
                let sent_encryption_request = false;

                if !sent_encryption_request {
                    for directive in proto.login_success(&name, uuid) {
                        apply(conn, &mut state, directive).await?;
                    }
                    if !proto.has_configuration_phase() {
                        state = State::Configuration;
                        pending_event = Some(ServerBound::ConfigurationFinished);
                    }
                }
            }
            // Handle the client's answer to the encryption challenge sent by
            // `LoginStart`. Everything up to and including this
            // packet travels in the clear; `ServerDirective::EnableEncryption`
            // below must be applied before anything is sent in reply, or the
            // two sides disagree about which layer started where —
            // `ServerDirective::EnableEncryption`'s own doc comment names the
            // same ordering hazard `SetCompression` already documents for
            // itself.
            #[cfg(not(target_arch = "wasm32"))]
            ServerBound::EncryptionResponse {
                shared_secret,
                verify_token,
            } => {
                let Some(cfg) = online_mode else {
                    return Err(ServerError::UnexpectedEncryptionResponse);
                };
                let Some((keypair, expected_token)) = pending_encryption.take() else {
                    return Err(ServerError::UnexpectedEncryptionResponse);
                };
                let decrypted_token = keypair.decrypt(&verify_token)?;
                if decrypted_token != expected_token {
                    return Err(ServerError::VerifyTokenMismatch);
                }
                let secret = keypair.decrypt(&shared_secret)?;
                apply(conn, &mut state, ServerDirective::EnableEncryption(secret.clone())).await?;

                // Vanilla's server-id is always the empty string
                // (its own generic login-phase packet listener's own server-id field); the hash is
                // taken over it, the secret, and the exact public-key DER
                // bytes the client encrypted against.
                let hash = lodestone_auth::server_hash("", &secret, keypair.public_key_der());
                let name = username.clone().unwrap_or_default();
                match (cfg.verify)(cfg.http.clone(), name, hash).await {
                    Ok(Some(profile)) => {
                        login_uuid = Some(profile.id);
                        username = Some(profile.name.clone());
                        online_authenticated = true;
                        for directive in proto.login_success(&profile.name, profile.id) {
                            apply(conn, &mut state, directive).await?;
                        }
                        if !proto.has_configuration_phase() {
                            state = State::Configuration;
                            pending_event = Some(ServerBound::ConfigurationFinished);
                        }
                    }
                    Ok(None) => {
                        let directive =
                            proto.encode_disconnect(state, &unverified_username_reason());
                        apply(conn, &mut state, directive).await?;
                        return Err(ServerError::UnverifiedUsername);
                    }
                    Err(error) => {
                        let directive =
                            proto.encode_disconnect(state, &auth_servers_down_reason());
                        apply(conn, &mut state, directive).await?;
                        return Err(ServerError::AuthServiceUnavailable(error));
                    }
                }
            }
            // Online-mode encryption is native-only (`lodestone-net::crypto`'s
            // and `lodestone-auth`'s own doc comments): a `wasm32` build never
            // sends an `EncryptionRequest` in the first place (`LoginStart`'s
            // `sent_encryption_request` is unconditionally `false` there), so
            // a real client answering one it was never sent is a protocol
            // violation exactly like the native "nothing outstanding" case.
            #[cfg(target_arch = "wasm32")]
            ServerBound::EncryptionResponse { .. } => {
                return Err(ServerError::UnexpectedEncryptionResponse);
            }
            ServerBound::LoginAcknowledged => {
                state = State::Configuration;
                // Send the registries needed to resolve the dimension and
                // world-clock holders — the
                // `dimension_type` ids `login`/`respawn` carry, the
                // `world_clock` keys `set_time` uses — must arrive **before**
                // the finish signal, or the client cannot make sense of them.
                for directive in proto.encode_registry_data() {
                    apply(conn, &mut state, directive).await?;
                }
                for directive in proto.begin_configuration() {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::ConfigurationFinished => {
                // PERF INSTRUMENT: timing the whole configuration→play transition
                let t_cfg = JoinStopwatch::now();
                // The world spawn point is a *search*, not a fixed local `(8, 8)`
                // in the origin column. `world_spawn::find_initial_spawn` walks
                // a ±5-chunk spiral and selects the first chunk with a valid
                // surface. A plains origin yields `(8, y, 8)`, while an invalid
                // origin moves the spawn to the nearest valid chunk instead of
                // stranding the player under water.
                // **Read the world's own spawn first.** The spiral runs at world
                // creation and persists to `level.dat`; a join reuses that value.
                // A missing value triggers the search, avoiding a repeated
                // 121-column search on every connection and allowing
                // `/setworldspawn` to persist.
                //
                // `None` here means "no search has happened for this world yet",
                // which is exactly a fresh world; the first join resolves it and the
                // next autosave writes it. See
                // `WorldStateHandle::world_spawn`.
                let spawn = match world.world_spawn() {
                    Some(stored)
                        if crate::world_spawn::is_spawn_position_clear(
                            source.get(),
                            crate::world_spawn::player_position_for_spawn_anchor(stored.pos),
                        ) =>
                    {
                        stored
                    }
                    Some(stored) => {
                        tracing::warn!(
                            position = ?stored.pos,
                            "stored world spawn is obstructed; searching for a clear replacement"
                        );
                        let found = source.find_initial_spawn().await;
                        world.set_world_spawn(found);
                        found
                    }
                    None => {
                        world
                            .resolve_world_spawn(|| source.find_initial_spawn())
                            .await
                    }
                };
                // Level data stores the world spawn as a block anchor. The
                // entity must enter at that block's bottom centre, matching
                // the collision probe used while selecting the anchor; using
                // the integer corner lets the 0.6-wide body overlap a
                // neighbouring column and can place it inside terrain.
                let spawn_position =
                    crate::world_spawn::player_position_for_spawn_anchor(spawn.pos);

                let home_tickets = source.get().ticket_store().unwrap_or_else(|| tickets.clone());
                let spawn_chunk = (
                    (spawn.pos.x / 16.0).floor() as i32,
                    (spawn.pos.z / 16.0).floor() as i32,
                );
                home_tickets.set_ticket_with_radius(
                    TicketOwner::Spawn,
                    TicketKind::PlayerSpawn,
                    spawn_chunk,
                    PLAYER_SPAWN_RADIUS,
                );
                source.get().reconcile_ticket_residency();

                // this player's own saved state, if this world has
                // any. Reached through `ChunkSource::world_registries` rather
                // than a new parameter — see `crate::chunk::WorldRegistries`'s
                // `player_data` field for why that routing was chosen over
                // threading a 31st argument through eleven wrappers.
                //
                // An in-memory world answers `None` and every existing caller
                // therefore behaves exactly as before.
                let home_source = source;
                #[cfg(not(target_arch = "wasm32"))]
                let saved_player = player_store(source.get()).and_then(|store| {
                    match store.read(login_uuid.unwrap_or_default()) {
                        Ok(data) => data,
                        // Logged, never swallowed. A save we cannot read is not a
                        // player with no save: joining them empty-handed would
                        // overwrite the file they still own on the first
                        // autosave, which is the one outcome that loses the
                        // inventory irrecoverably.
                        Err(err) => {
                            tracing::error!(
                                "player data for {:?} could not be read and will NOT be \
                                 overwritten this session: {err}",
                                login_uuid,
                            );
                            None
                        }
                    }
                });
                #[cfg(target_arch = "wasm32")]
                let saved_player: Option<()> = None;
                #[cfg(not(target_arch = "wasm32"))]
                let mut native_player = NativePlayerSession::load(
                    source.get(),
                    login_uuid.unwrap_or_default(),
                );
                #[cfg(not(target_arch = "wasm32"))]
                let restored_dimension_source = native_player.as_mut().and_then(|native| {
                    let dimension = native.dimension()?;
                    let protocol_supports_dimension = !proto
                        .encode_dimension_change_with_teleport_id(
                            1,
                            dimension.key(),
                            spawn_position,
                            game_mode,
                        )
                        .is_empty();
                    native.restored_source(source.get(), protocol_supports_dimension)
                });
                #[cfg(not(target_arch = "wasm32"))]
                let source = restored_dimension_source
                    .as_ref()
                    .map_or(source, SourceRef::Dimension);

                // Where the player actually re-enters the world. `spawn.pos`
                // stays the **world** spawn: it is what `serve_play` uses for a
                // respawn, and overwriting it with the player's last position
                // would respawn a dead player back where they died. See
                // `join_position_for_saved_player`'s own doc comment for why a
                // saved position is not always trusted verbatim.
                #[cfg(not(target_arch = "wasm32"))]
                let join_pos = {
                    let anvil_pos =
                        join_position_for_saved_player(saved_player.as_ref(), spawn_position);
                    let candidate = native_player
                        .as_ref()
                        .map_or(anvil_pos, |native| native.join_position(anvil_pos));
                    if saved_player.is_some() || native_player.is_some() {
                        // A restored Overworld position may have been written by
                        // an older session before terrain clearance was enforced.
                        // Re-read the live source before trusting it; a saved
                        // position inside a wall falls back to the verified world
                        // spawn. A locator restored into a sibling dimension is
                        // deliberately skipped because its source is not the home
                        // Overworld terrain.
                        source
                            .admit_columns(column_admission_footprint(
                                (candidate.x.floor() as i32).div_euclid(16),
                                (candidate.z.floor() as i32).div_euclid(16),
                                1,
                            ))
                            .await?;
                        if restored_dimension_source.is_none()
                            && !crate::world_spawn::is_spawn_position_clear(source.get(), candidate)
                        {
                            tracing::warn!(
                                position = ?candidate,
                                fallback = ?spawn_position,
                                "saved player position is obstructed; using the world spawn"
                            );
                            spawn_position
                        } else {
                            candidate
                        }
                    } else {
                        spawn_position
                    }
                };
                #[cfg(target_arch = "wasm32")]
                let join_pos = spawn_position;
                // Vanilla's own player-game-type field, restored — a player who typed
                // `/gamemode survival` and quit comes back in survival. Shadowed
                // rather than assigned so a world with no save keeps the mode the
                // host opened with.
                #[cfg(not(target_arch = "wasm32"))]
                let game_mode = saved_player
                    .as_ref()
                    .and_then(|data| data.game_mode)
                    .or_else(|| {
                        native_player
                            .as_ref()
                            .and_then(NativePlayerSession::game_mode)
                    })
                    .unwrap_or(game_mode);

                state = State::Play;
                #[cfg(not(target_arch = "wasm32"))]
                let restored_other_dimension = restored_dimension_source.is_some();
                #[cfg(target_arch = "wasm32")]
                let restored_other_dimension = false;
                let initial_teleport_id = proto
                    .uses_teleport_acknowledgements()
                    .then_some(i32::from(restored_other_dimension));
                for directive in proto.begin_play(&crate::protocol::JoinGame {
                    view_distance: view_radius,
                    simulation_distance: world.simulation_distance(),
                    spawn: join_pos,
                    mode: game_mode,
                    teleport_id: 0,
                }) {
                    apply(conn, &mut state, directive).await?;
                }
                if restored_other_dimension {
                    for directive in proto.encode_dimension_change_with_teleport_id(
                        initial_teleport_id.unwrap_or(0),
                        source.dimension().key(),
                        join_pos,
                        game_mode,
                    ) {
                        apply(conn, &mut state, directive).await?;
                    }
                }
                // Vanilla's own "place new player" step sends the abilities
                // packet right after the login packet, and it is not optional:
                // the login packet's `game_type` tells the client *what* mode it
                // is in, while flight permission and instant build live only
                // here. Sending one without the other is how "creative mode that
                // cannot fly" happens.
                apply(
                    conn,
                    &mut state,
                    proto.encode_player_abilities(Abilities::for_mode(game_mode)),
                )
                .await?;

                // The Brigadier command tree. **Nothing sent this before, so the
                // client had no autocomplete and no command highlighting at all** —
                // `lodestone-shell`'s chat box, its `CommandTreeCell` and the whole
                // suggestion UX were complete and starved of input.
                //
                // Position taken from vanilla's own "place new player" step, which calls
                // its own "send player permission level" step — the method that sends the tree —
                // after the abilities packet and before its own "send level info" step. So it goes
                // here: after abilities above, before the clock sync below, and
                // before any chunk goes out. Appending it after chunk streaming
                // would have been easier (the `CommandSession` that owns the tree
                // is built down there) and it is the wrong place.
                //
                // The tree is pruned to this connection's own permission level, as
                // vanilla's own "send commands" step prunes with
                // its own "fill usable commands" helper: a level-0 player is not sent `/gamemode`'s
                // node, which is what stops the client suggesting a command the
                // server will refuse.
                //
                // `login_uuid` cannot be `None` here: reaching Play requires
                // `ConfigurationFinished`, which follows a successful `LoginStart`
                // (or its online-mode login completion) either through
                // `LoginAcknowledged` for modern protocols or through the queued
                // legacy transition. The `unwrap_or_default` is a total fallback
                // rather than a panic because a nil uuid resolves to no player
                // and therefore no permissions — failing closed, not open. On
                // `wasm32` there is no `AccessHandle` in this signature at all
                // (the whole ops/whitelist/ban feature is native-only). The
                // browser build uses level 4 for its local world so the built-in
                // game-mode command remains available there.
                //
                // All three bindings are consumed again by the `CommandSession`
                // further down; see its own comment for why reuse rather than a
                // second construction.
                let player_uuid = login_uuid.unwrap_or_default();
                #[cfg(not(target_arch = "wasm32"))]
                let permission_level = access.command_permission_level(player_uuid);
                #[cfg(target_arch = "wasm32")]
                let permission_level = 4;
                let builtins = crate::commands::ServerCommands::new();
                apply(
                    conn,
                    &mut state,
                    proto.encode_commands(&builtins.wire_tree_for(permission_level)),
                )
                .await?;

                // Full clock sync at join. Send the **world's** clock, not `(0, 0)`;
                // a world loaded from
                // disk starts at the value returned by
                // `WorldStateHandle::load_level_data`.
                let joined_at = world.time();
                apply(
                    conn,
                    &mut state,
                    proto.encode_set_time(joined_at.game_time, Some(joined_at.day_time)),
                )
                .await?;

                apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                // Generate columns with a bounded worker window. The ring order
                // is a pure function of `view_radius`, so worker completion order
                // cannot change the encoded byte sequence or RNG-derived content.
                // Shared sources perform generation and encoding in blocking
                // workers; the connection task emits the resulting frames.
                //
                // The inner `JOIN_PRESTREAM_RADIUS` rings are sent first so the
                // player's column arrives after one generated column. Remaining
                // columns flow through `JoinChunkStream` while the play loop
                // dispatches packets. One begin/end chunk-batch pair covers the
                // complete stream, preserving client flow-control accounting.
                //
                // `join_view_rings` yields offsets `(dx, dz)`, so add the
                // player's absolute chunk `(join_cx, join_cz)` before encoding.
                // At `view_radius = 9` the square contains 361 columns; at
                // `view_radius = 16` it contains 1,089. The absolute centre keeps
                // the streamed terrain aligned with the view tracker for joins
                // away from the origin.
                let join_cx = (join_pos.x / 16.0).floor() as i32;
                let join_cz = (join_pos.z / 16.0).floor() as i32;
                let player_ticket_guard = {
                    let bits = login_uuid.unwrap_or_else(uuid::Uuid::nil).as_u128();
                    let id = (bits as u64) ^ ((bits >> 64) as u64);
                    let store = connection_travel::ticket_store_for_source(
                        source.get(), home_source.get(), &home_tickets,
                    )?;
                    let guard = store.grant_player_with_simulation_radius(
                        id, (join_cx, join_cz), view_radius,
                        view_radius.clamp(0, world.simulation_distance()),
                    ).with_spawn_store(&home_tickets);
                    source.get().reconcile_ticket_residency();
                    guard
                };
                let t_chunks = JoinStopwatch::now();
                let join_trace = JoinTrace::new();
                let mut batch_size = 0;
                let mut prestream_deliveries = Vec::new();
                let window = crate::join_scheduler::generation_window();
                let rings: Vec<Vec<(i32, i32)>> = join_view_rings(view_radius)
                    .into_iter()
                    .map(|ring| {
                        ring.into_iter()
                            .map(|(dx, dz)| (join_cx + dx, join_cz + dz))
                            .collect()
                    })
                    .collect();
                let ring_count = rings.len();
                // How much has to be on the wire before the player may act. See
                // `JOIN_PRESTREAM_RADIUS`.
                let prestream: usize = rings
                    .iter()
                    .take(JOIN_PRESTREAM_RADIUS as usize + 1)
                    .map(Vec::len)
                    .sum();
                let join_stream;
                match &source {
                    SourceRef::Shared(src) => {
                        let coords: Vec<(i32, i32)> = rings.into_iter().flatten().collect();
                        // `prioritised` keys deferred columns by distance from
                        // the player, with an in-frustum bonus; `serve_play`
                        // re-keys the queue when the player moves or turns.
                        // At join, the absent rotation makes this key equal the
                        // `join_view_rings` order.
                        //
                        // The priority centre is the player's own column, not
                        // `(0, 0)`: it is compared against the absolute
                        // coordinates in `coords`, and `serve_play`'s
                        // `reprioritise` compares against the player's absolute
                        // chunk. The origin is not a valid substitute for that
                        // centre when a player joins elsewhere.
                        //
                        // `encoding_with`: protocol encode runs **inside** the
                        // per-column `spawn_blocking` closure, so this task only
                        // writes frames. The measured cost is 62 M instructions /
                        // ≈2.4 ms per column (≈2.6 s for serial encoding); see
                        // `crate::protocol::ChunkEncoder`. Worker completion
                        // cannot change the wire because queue order controls
                        // emission.
                        let pipeline = crate::join_scheduler::ColumnPipeline::prioritised(
                            Arc::clone(src),
                            coords,
                            window,
                            (join_cx, join_cz),
                            None,
                        )
                        .with_generation_band(
                            (join_cx, join_cz),
                            crate::join_scheduler::DEFAULT_FULL_GENERATION_RADIUS,
                        );
                        let mut pipeline = pipeline
                            .with_trace(join_trace.clone())
                            .encoding_with(if proto.uses_cross_column_light()
                                || proto.retains_initial_column_light()
                            {
                                None
                            } else {
                                proto.chunk_encoder()
                            });
                        while batch_size < prestream {
                            let next = match pipeline.next().await {
                                Ok(next) => next,
                                Err(error) => {
                                    return return_chunk_encode_error(
                                        conn,
                                        proto,
                                        &mut state,
                                        Some(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                                        error,
                                    )
                                    .await;
                                }
                            };
                            let Some(((cx, cz), payload)) = next else {
                                break;
                            };
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
                                        Some(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                                        error,
                                    )
                                    .await;
                                }
                            };
                            let sent = matches!(directive.directive, ServerDirective::Send { .. });
                            let stage = directive.stage;
                            apply(conn, &mut state, directive.directive).await?;
                            if sent {
                                prestream_deliveries.push(((cx, cz), stage));
                            }
                            if let Some(trace) = join_trace.as_ref() {
                                trace.mark("delivered", cx, cz);
                            }
                            batch_size += 1;
                        }
                        join_stream = crate::join_scheduler::JoinChunkStream::windowed(pipeline);
                    }
                    // The `Dimension` arm reaches this path when a native player
                    // locator restores directly into a sibling dimension. Its
                    // erased source cannot inhabit the generic shared pipeline,
                    // so it uses the same ordered ring path as a borrowed source.
                    SourceRef::Borrowed(_) | SourceRef::Dimension(_) => {
                        // A borrowed source is not `'static`, so it cannot be
                        // spawned; each ring on this arm blocks until its
                        // columns finish generating. A window cannot overlap
                        // generation and encoding here. Ring cumulative sizes
                        // are `1 + 4r(r + 1)`, always ≡ 1 (mod 8); with a window
                        // of 8, ring 8's 64 columns therefore form eight serial
                        // batches rather than one.
                        //
                        // Both arms walk the same flattened ring sequence and
                        // emit the player's column first. The ordering checks
                        // `join_streams_the_view_outward_from_the_players_own_column`
                        // and `the_shared_arm_streams_the_view_outward_too` cover
                        // the borrowed and shared paths.
                        //
                        // Rings `0..=JOIN_PRESTREAM_RADIUS` are generated and
                        // encoded here; the remaining rings are handed to
                        // `serve_play` as whole rings. The emitted sequence is
                        // follows the ring order, with one barrier per ring.
                        let mut rings = rings;
                        let deferred = rings.split_off(
                            (JOIN_PRESTREAM_RADIUS as usize + 1).min(rings.len()),
                        );
                        for ring in &rings {
                            let columns = source.generate(ring.clone()).await?;
                            for (&(cx, cz), column) in ring.iter().zip(columns.iter()) {
                                if let Some(trace) = join_trace.as_ref() {
                                    trace.mark("generated", cx, cz);
                                }
                                let directive = match encode_chunk_with_source_receipt(
                                    proto,
                                    source.get(),
                                    cx,
                                    cz,
                                    column,
                                ) {
                                    Ok(directive) => directive,
                                    Err(error) => {
                                        return return_chunk_encode_error(
                                            conn,
                                            proto,
                                            &mut state,
                                            Some(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                                            error,
                                        )
                                        .await;
                                    }
                                };
                                if let Some(trace) = join_trace.as_ref() {
                                    trace.mark("encoded", cx, cz);
                                }
                                let sent = matches!(directive.directive, ServerDirective::Send { .. });
                                let stage = directive.stage;
                                apply(conn, &mut state, directive.directive).await?;
                                if sent {
                                    prestream_deliveries.push(((cx, cz), stage));
                                }
                                if let Some(trace) = join_trace.as_ref() {
                                    trace.mark("delivered", cx, cz);
                                }
                                batch_size += 1;
                            }
                        }
                        join_stream = crate::join_scheduler::JoinChunkStream::ringed(deferred);
                    }
                }
                // `batch_size` is a `usize` because it is compared against
                // `prestream` above; the wire field is an `i32`.
                apply(
                    conn,
                    &mut state,
                    proto.end_chunk_batch(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                )
                .await?;
                world.mark_join_ready();
                let chunk_ms = t_chunks.elapsed().as_millis();
                let chunks_sent = batch_size;
                tracing::info!(
                    "join chunks: {} columns inline in {}ms ({:.0} col/s), {} deferred to the \
                     play loop, {} rings, window {}",
                    chunks_sent,
                    chunk_ms,
                    chunks_sent as f64 / (chunk_ms as f64 / 1000.0),
                    join_stream.remaining(),
                    ring_count,
                    window,
                );

                let t_welcome = JoinStopwatch::now();
                for directive in proto.welcome_message() {
                    apply(conn, &mut state, directive).await?;
                }

                // `ConfigurationFinished` cannot be reached without a successful
                // `LoginStart`/login completion in any correct `ServerProtocol`:
                // modern protocols receive the two acknowledgements, while a
                // legacy protocol queues this transition after login. `username`
                // is therefore always `Some` here; falling back to an empty
                // string rather than panicking keeps a protocol that violates
                // that contract merely wrong, not a crash.
                let username = username.clone().unwrap_or_default();

                let join_entities = ActiveEntities::new(world, entities, source.dimension());
                let entities = &join_entities;

                // Register this connection as a player entity before initial
                // sync. Other connections then see it on their next pass, and
                // this connection can exclude its own entity. The ticket moves
                // into `serve_play`, whose `Drop` implementation deregisters
                // the player on every exit path.
                let (player_ticket, initial_swing_cursor) =
                    entities.players().map_or((None, None), |registry| {
                        let (ticket, cursor) = registry.join_in_dimension_with_swing_cursor(
                            &username,
                            login_uuid.unwrap_or_else(uuid::Uuid::nil),
                            join_pos,
                            source.dimension(),
                        );
                        (Some(ticket), Some(cursor))
                    });

                // Initial entity sync sends tab-list additions and other
                // players' spawns in the order defined by [`stream_pass`].
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }

                // derive view centre from the actual spawn
                // chunk rather than assuming (0, 0). For spawn at (8, ~64,
                // 8) both floor to 0, so the centre does not change today;
                // the derivation is the point — when the spawn column or
                // the X/Z offsets move, this follows automatically.
                // `join_pos`, not `spawn.pos`: a restored player standing 400
                // blocks from world spawn must be sent the chunks under *their*
                // feet. Centring on world spawn instead would stream a square of
                // terrain the player cannot see and leave them suspended over
                // nothing — a total chunk blackout with a perfectly healthy join.
                //
                // Reused from the binding the chunk stream above already derived,
                // rather than recomputed: the tracker's `loaded` set is a claim
                // about the square that stream actually emitted, so a second
                // derivation is a second chance for the two to disagree — and
                // when they did, the columns under the player's feet were marked
                // sent without ever being sent.
                let (spawn_cx, spawn_cz) = (join_cx, join_cz);
                // Keep the join radius and its configured maximum distinct:
                // the square is streamed at the first value, while
                // `ClientInformationChanged` may request any value up to
                // `ViewTracker::max_radius`.
                // Shared progressive sources start with a shaped far band, so
                // the tracker seeds the same stage ledger the join pipeline
                // used. Sources that require a complete packet (or retain the
                // legacy borrowed/ringed path) keep the all-full ledger.
                let mut view = match source {
                    SourceRef::Shared(src)
                        if src
                            .packet_generation_stage(ChunkGenerationStage::Shaped)
                            .is_none() => ViewTracker::new_banded(
                        (spawn_cx, spawn_cz),
                        view_radius,
                        max_view_radius,
                        crate::join_scheduler::DEFAULT_FULL_GENERATION_RADIUS,
                    ),
                    _ => ViewTracker::new((spawn_cx, spawn_cz), view_radius, max_view_radius),
                };
                for (coord, stage) in prestream_deliveries {
                    if let Some(incarnation) = view.incarnation(coord) {
                        view.record_delivery(coord, incarnation, stage);
                    }
                }
                // `player_uuid`, `permission_level` and
                // `builtins` are the bindings the `COMMANDS` send above already
                // derived, reused rather than recomputed — the tree the client was
                // sent and the tree this session dispatches against **must** be the
                // same one, and two constructions are two chances for them to
                // differ. That is the failure mode the whole `WireDescriptor`
                // arrangement exists to prevent, and rebuilding here would reopen
                // it one level up.
                //
                // Their derivation is above, at the send, and stated there: the uuid
                // is the one `login_success` echoed to this client, the level comes
                // from that authenticated uuid and never from a command's text, and
                // `username` is the name that survived `is_valid_player_name`.
                // Nothing the player later *sends* can change either, which is
                // exactly the property the seam needs — see the
                // `ServerBound::ChatCommand` arm in `dispatch_play_packet`.
                let commands = CommandSession {
                    builtins,
                    dispatch: commands.clone(),
                    caller: CommandCaller::with_permission_level(
                        player_uuid,
                        username.clone(),
                        permission_level,
                    ),
                    #[cfg(not(target_arch = "wasm32"))]
                    plugin_access: access.clone(),
                    permission_level,
                };
                // The server-authoritative advancement/statistics
                // store for this connection, created at the Play handoff and
                // carried into `serve_play` so the per-packet flush and the
                // `REQUEST_STATS` reply can reach it. The first packet is sent
                // here, at join, exactly where vanilla's own per-player
                // advancements tracker's own "flush dirty" first-packet path fires:
                // `reset` true, the whole builtin tree as `added`, and every
                // advancement's (currently empty) progress — the client builds
                // its screen from this one packet and nothing after it until a
                // criterion actually flips. A protocol without an
                // `encode_update_advancements` override simply sends nothing
                // (the trait default), so this is a silent no-op rather than a
                // failure on such a version.
                let mut advancements = AdvancementManager::builtin();
                let initial = advancements.initial_update(player_uuid, true);
                apply(conn, &mut state, proto.encode_update_advancements(&initial)).await?;
                let total_ms = t_cfg.elapsed().as_millis();
                // `saturating_sub`, not `- 1`: `as_millis()` is `u128`, and over an
                // in-memory or loopback transport the welcome phase completes in
                // under a millisecond, so the plain subtraction underflows. That
                // panicked every integrated-server test in debug and wrapped
                // silently in release, which is why no `cargo check` and no
                // `cargo run --release` could see it.
                let welcome_ms = t_welcome.elapsed().as_millis().saturating_sub(1); // approx, minus advancement encode
                tracing::info!(
                    "Configuration -> Play: {}ms total (chunks={}ms, welcome/entities/advancements={}ms)",
                    total_ms,
                    chunk_ms,
                    welcome_ms,
                );
                // Vanilla validates an announced profile key whenever this
                // connection completed online authentication and the Mojang
                // issuer service is usable. `enforce-secure-profile` is a
                // separate policy: it decides whether a player must sign, not
                // whether an otherwise-valid announcement is adopted. Fetching
                // remains outside the cache lock in `profile_key_issuers`.
                #[cfg(not(target_arch = "wasm32"))]
                let profile_key_issuers = if online_authenticated {
                    online_mode
                        .expect("an online-authenticated connection has OnlineModeConfig")
                        .profile_key_issuers(crate::chat_session::now_millis())
                        .await
                } else {
                    None
                };
                #[cfg(not(target_arch = "wasm32"))]
                let enforce_secure_profile = online_authenticated
                    && entities
                        .players()
                        .is_some_and(PlayerRegistry::enforce_secure_profile)
                    && profile_key_issuers.is_some();
                if let Some(error) = world.initial_seed_error() {
                    return return_initial_seed_error(conn, proto, &mut state, None, error).await;
                }
                let service = crate::connection_service::ConnectionService::new();
                let play = crate::connection_service::pin_future(|| serve_play(
                    &service,
                    conn,
                    proto,
                    source,
                    home_source,
                    entities,
                    state,
                    initial_teleport_id,
                    streamer,
                    player_list,
                    player_ticket,
                    initial_swing_cursor,
                    player_ticket_guard,
                    view,
                    username,
                    spawn_position,
                    chunks_sent,
                    join_stream,
                    join_trace,
                    block_entities,
                    mobs,
                    block_ticks,
                    explosions,
                    weather,
                    sleep_vote,
                    sleep_feed,
                    commands,
                    advancements,
                    player_uuid,
                    #[cfg(not(target_arch = "wasm32"))]
                    profile_key_issuers,
                    #[cfg(not(target_arch = "wasm32"))]
                    enforce_secure_profile,
                    border,
                    resource_packs,
                    &mut client_channels,
                    plugin_channels,
                    game_mode,
                    world,
                    live_save,
                    #[cfg(not(target_arch = "wasm32"))]
                    native_player,
                ));
                return service.run(play).await;
            }
            // Wire-level plugin messaging, Configuration-phase: a
            // client announces the channels it supports here (via
            // `minecraft:register`) before the Play handoff, so the broadcast
            // drain in `serve_play` filters against them from the first drain
            // onward. Same interpretation as the `dispatch_play_packet` arm:
            // control channels update this connection's supported set, anything
            // else is dispatched to its registered handler (silently dropped
            // when the server registered no interest).
            ServerBound::CustomPayload { channel, data } => {
                if !client_channels.apply_custom_payload(&channel, &data) {
                    plugin_channels.dispatch(&channel, &data);
                }
            }
            ServerBound::KeepAlive { .. }
            | ServerBound::PlayerMoved { .. }
            | ServerBound::PlayerRotated { .. }
            | ServerBound::PlayerStatusOnly { .. }
            | ServerBound::BlockAction { .. }
            | ServerBound::ItemDropped { .. }
            | ServerBound::UseItemOn { .. }
            | ServerBound::ChangeGameMode { .. }
            | ServerBound::DifficultyChanged { .. }
            | ServerBound::DifficultyLockChanged { .. }
            | ServerBound::GameRuleChanged { .. }
            | ServerBound::CarriedItemChanged { .. }
            | ServerBound::ContainerClicked { .. }
            | ServerBound::RecipePlaced { .. }
            | ServerBound::RecipeBookSettingsChanged { .. }
            | ServerBound::RecipeBookRecipeSeen { .. }
            | ServerBound::SeenAdvancements { .. }
            | ServerBound::ResourcePackResponse { .. }
            | ServerBound::PlayerLoaded
            | ServerBound::ClientTickEnded
            | ServerBound::TeleportationAccepted { .. }
            | ServerBound::PlayerAbilitiesChanged { .. }
            | ServerBound::BlockEntityTagQuery { .. }
            | ServerBound::EntityTagQuery { .. }
            | ServerBound::ContainerClosed { .. }
            | ServerBound::Attack { .. }
            | ServerBound::InteractEntity { .. }
            | ServerBound::UseItem { .. }
            | ServerBound::ReleaseUseItem
            | ServerBound::SwapItemInHand
            | ServerBound::VehicleMoved { .. }
            | ServerBound::PlayerInput { .. }
            | ServerBound::CreativeModeSlotSet { .. }
            | ServerBound::ClientCommand { .. }
            | ServerBound::ClientInformationChanged { .. }
            | ServerBound::ChunkBatchAcknowledged { .. }
            // Unreachable here by construction, like the Play-phase variants
            // above it: every `ServerProtocol::decode` arm producing this is
            // gated on `State::Play`, and this loop hands off to `serve_play`
            // the moment Play is reached. Listed rather than folded into a
            // wildcard so that adding a variant stays a compile error.
            | ServerBound::ChatCommand { .. }
            | ServerBound::Chat { .. }
            | ServerBound::ChatSessionAnnounced { .. }
            | ServerBound::PlayerCommand { .. }
            | ServerBound::RenameItem { .. }
            | ServerBound::ContainerButtonClick { .. }
            | ServerBound::ContainerSlotStateChanged { .. }
            | ServerBound::SetCommandBlock { .. }
            | ServerBound::SignUpdate { .. }
            | ServerBound::EditBook { .. }
            | ServerBound::SetBeacon { .. }
            | ServerBound::SelectTrade { .. }
            | ServerBound::SelectBundleItem { .. }
            | ServerBound::PaddleBoat { .. }
            | ServerBound::PickItemFromBlock { .. }
            | ServerBound::PickItemFromEntity { .. }
            | ServerBound::Pong { .. }
            | ServerBound::TeleportToEntity { .. }
            | ServerBound::Swing { .. }
            | ServerBound::SpectatorAction { .. }
            | ServerBound::CommandSuggestion { .. }
            | ServerBound::Ignored => {}
        }
    }

    match username {
        Some(username) => Ok(ServeSummary {
            username,
            chunks_sent: 0,
            inventory: PlayerInventory::default(),
        }),
        None => Err(ServerError::ClosedBeforeLogin),
    }
}
