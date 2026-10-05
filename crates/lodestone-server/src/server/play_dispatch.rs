//! `dispatch_play_packet`: routes each serverbound play packet to its handler and applies the result to the connection and world.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_play_packet<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    // The dimension the connection joined in, which a respawn is resolved
    // against and returns to.
    home: SourceRef<'_, S>,
    state: &mut State,
    mut pending_relights: Option<&mut PendingRelights>,
    view: &mut ViewTracker,
    // This connection's chunk-residency guard, so a chunk-boundary
    // crossing or a live view-radius change (the `recenter`/`set_view_radius`
    // arms below) can move the same `PLAYER_LOADING`/`PLAYER_SIMULATION`
    // tickets `serve_play` granted at join, rather than leaving them pinned to
    // the join column for the connection's whole lifetime.
    player_ticket_guard: &PlayerTicketGuard,
    pending_keep_alive: &mut Option<i64>,
    pending_break: &mut Option<PendingBreak>,
    pending_prediction_ack: &mut connection_prediction::PendingPredictionAck,
    // The latest server-issued position correction. A matching
    // `TeleportationAccepted` clears it; movement stays inert while it remains.
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    player_pos: &mut Option<(f64, f64, f64)>,
    // The latest position delta and its tick boundary. Projectile launches
    // inherit this connection-local motion; see [`ClientMovement`].
    client_movement: &mut ClientMovement,
    // Mirrors `player_pos` exactly — updated here, read back by
    // the caller, republished to the `PlayerRegistry` so *other* connections
    // stream this player's facing. `Option` because "no angles reported yet"
    // is distinct from "facing due south"; the registry keeps its join
    // default until a packet that actually carries angles arrives.
    player_rot: &mut Option<Rotation>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    burn: &mut crate::burning::BurnState,
    world: &crate::world_state::WorldStateHandle,
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    // Which merchant screen this connection has open, if any — see
    // [`OpenMerchant`]'s own doc for why it is not folded into
    // `open_container`.
    open_merchant: &mut Option<OpenMerchant>,
    container_sync: &mut ContainerSync,
    next_window_id: &mut i32,
    mobs: &MobHandle,
    sprinting: &mut bool,
    // Retained between input packets because the client sends a new bitset
    // only when movement input changes. Placement reads this secondary-use
    // state to bypass a clicked container while placing a block beside it.
    sneaking: &mut bool,
    awaiting_chunk_batch_ack: &mut bool,
    pending_chunk_batches: &mut VecDeque<PendingChunkBatch>,
    // The connection's live column stream, where the caller has one to lend. A
    // chunk-boundary crossing enqueues its newly-visible strip here instead of
    // generating it inline, so the view update costs this function a set difference
    // rather than a `2r + 1`-column `await` — see [`send_view_update`], which owns
    // the decision and the fallback.
    //
    // `Option` reflects the two streaming modes: native `serve_play` drains the
    // deferred stream from a `select!` branch, while the `wasm32` loop drains
    // its join inline and has no deferred-stream consumer.
    mut join_stream: Option<&mut crate::join_scheduler::JoinChunkStream<S>>,
    // `CommandSession` bundles command dispatch with the caller identity used
    // for command execution.
    commands: &CommandSession,
    // This connection's advancement/statistics store and the player
    // key its progress lives under. Threaded only to reach `apply_client_command`
    //'s `REQUEST_STATS` arm, which answers with the player's current stats —
    // see that function's own doc comment.
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    // `Some` only after an online-authenticated Play handoff found a usable
    // Mojang issuer-key cache. This validates announcements independently of
    // whether the host requires signed chat. The cfg preserves the browser's
    // no-auth/degraded surface without linking `lodestone-auth` there.
    #[cfg(not(target_arch = "wasm32"))]
    profile_key_issuers: Option<&lodestone_auth::MojangPublicKeys>,
    // The separate vanilla policy gate: authenticated online connection,
    // `enforce-secure-profile`, and a usable issuer cache. An adopted session
    // still requires signatures even when this is false; this flag governs a
    // player that has announced no valid session.
    enforce_secure_profile: bool,
    // Mirrors `player_pos`/`player_rot` exactly — filled here,
    // read back by the caller, republished to the `PlayerRegistry` so *other*
    // connections see it. An out-parameter rather than two more parameters (a
    // registry and this connection's username) because the caller already
    // owns both, and this function already takes 25.
    outgoing_chat: &mut Vec<String>,
    // This connection's announced chat-signing session (if any) and the
    // verification chain position tracked against it — mirrors
    // `pending_keep_alive`/`player_pos`'s shape exactly: connection-scoped
    // state the caller owns and this function mutates in place. See
    // `crate::chat_session`'s own module doc for what it is and is not used
    // for.
    chat_session: &mut Option<crate::chat_session::ServerChatSession>,
    // The shared player registry, for the `ChatCommand` arm alone: a command's
    // entity selectors resolve against the roster, and a command's effects aimed
    // at *another* player are queued on it.
    //
    // A concrete `Option<&PlayerRegistry>` rather than the generic
    // `EntitySource` the caller holds, so this function gains no type parameter —
    // and an `Option` rather than a required handle because singleplayer builds
    // no registry at all (`open_in_memory`). The `ChatCommand` arm synthesises the
    // caller's own candidate in that case, which is what keeps `@s` working
    // there.
    players: Option<&PlayerRegistry>,
    // Threaded through only to reach `apply_use_item_on`, which
    // needs to ask the world tick loop for a neighbour-update fan-out that
    // outlives this packet — see that function's own parameter comment.
    block_ticks: &BlockTickFeed,
    // Responses to server-pushed resource packs are recorded here for the
    // host; policy decisions remain outside the protocol loop.
    resource_packs: &ResourcePackPushFeed,
    // Set by the client's empty readiness marker; fall simulation waits for
    // this signal so the first placement movement cannot create a false fall.
    client_loaded: &mut bool,
    // This connection's composter roll source — seeded once in
    // `serve_play`, advanced once per right-click (see
    // [`apply_composter_use`]'s `roll` parameter).
    composter_rng: &mut SpawnRng,
    // This connection's bone-meal roll source — seeded once in `serve_play`,
    // advanced by a bone-meal right-click on a growable block. Its own stream, so
    // fertilising a crop cannot shift which roll a later composter insert or
    // block drop sees.
    bone_meal_rng: &mut SpawnRng,
    // This connection's experience — level, bar and lifetime total.
    // `&mut` because closing a furnace pays out its banked smelting XP (the
    // `ContainerClosed` arm), which is currently the only production producer.
    experience: &mut crate::experience::PlayerExperience,
    // This connection's live status effects — written by `/effect` and
    // ticked from `serve_play`'s vitals timer.
    effects: &mut crate::mob_effects::ActiveEffects,
    // This connection's block-drop roll source — seeded once in
    // `serve_play`, advanced by every break that rolls a table (see
    // `apply_block_action`'s parameter comment). A second stream rather than
    // sharing the composter's, so a composter click cannot shift which drop a
    // later break rolls; the two features would otherwise be coupled through
    // nothing but draw order.
    drops_rng: &mut SpawnRng,
    // This connection's declared channel support (register/
    // unregister interpretation happens here, in Play) and the shared registry
    // to dispatch ordinary payloads on.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // This connection's current game mode, `&mut` because the
    // `ChangeGameMode` arm and the built-in `/gamemode` both rewrite it — and
    // because the creative consequences below (instant break, damage immunity)
    // read it on later packets.
    game_mode: &mut GameMode,
    // The live ability record preserves client flight across mode changes.
    abilities: &mut Abilities,
    // The player's per-player respawn point, written by the bed
    // arm of `apply_use_item_on` and threaded through `serve_play`'s session
    // state. Read back by no caller yet — the placement half of P2 is the
    // next consumer (see `crate::world_spawn`'s module doc).
    respawn: &mut Option<RespawnPoint>,
    // The night-skip vote, fed by the two arms below — `lay_down`
    // on a bed click (`UseItemOn`), `get_up` on a wake-up (`PlayerCommand`
    // action 0). `player_entity_id` is this connection's roster key, resolved
    // once in `serve_play` (a `PlayerRegistry` ticket id where one exists,
    // `LOCAL_PLAYER_ENTITY_ID` in singleplayer) — see `serve_play`'s own
    // binding and `crate::sleep`'s module doc.
    sleep_vote: &SleepVote,
    // `ChatCommand`'s `CommandWorld` needs this to reach
    // `/worldborder`'s read/write surface — the same `BorderFeed` `serve_play`
    // already carries for the join broadcast and the vitals-tick damage read.
    border: &BorderFeed,
    player_entity_id: i32,
    // This connection's login name, for the death message
    // (`DeathCause::death_message`'s victim argument).
    username: &str,
    // The world spawn resolved at join, for the respawn teleport. See
    // `apply_client_command`'s own parameter comment.
    world_spawn: Vec3,
    // The server tick this packet is handled on, for
    // `apply_block_action`'s destroy-progress accounting. Native callers pass
    // the elapsed tick count; `wasm32` callers pass `None` because the browser
    // timer does not expose that counter. Hardness and range checks still apply
    // on that target.
    game_tick: Option<u64>,
    // This connection's in-progress bow draw, if any: the server tick
    // the `USE_ITEM` arrived on, so the `RELEASE_USE_ITEM` that ends it can turn
    // the interval into vanilla's own bow-item power-for-time routine. `None` whenever nothing
    // chargeable is being held down.
    //
    // Per-connection rather than shared, exactly like `sprinting` and
    // `player_pos`: two players can be mid-draw at once and neither's charge is
    // the other's.
    bow_draw: &mut Option<BowDraw>,
    // This connection's in-progress *consume* — eating or drinking. Held here for
    // the same reason `bow_draw` is, and separately from it because the two end
    // differently: a draw ends on a packet (`RELEASE_USE_ITEM`), while a consume
    // ends on the **server's own clock**. The per-tick arm in `serve_play`
    // counts the remaining duration and completes the action; the client sends
    // nothing when a steak finishes.
    item_in_use: &mut Option<ItemInUse>,
    // Set when a `ClientCommand`'s `PERFORM_RESPAWN` just fired *and* `source`
    // above was a portal-travelled dimension — see `apply_client_command`'s own
    // parameter comment. Both connection loops rebuild the home view before
    // dispatching another packet or publishing another world update.
    dimension_reset: &mut Option<connection_travel::DimensionReset>,
    // Leaving the End through the exit portal: the client's perform-respawn
    // answer to the win announcement is recorded here for the connection loop.
    end_exit: &mut connection_travel::EndExit,
    packet_id: i32,
    payload: &[u8],
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let packet = proto.decode(*state, packet_id, payload);
    pending_prediction_ack.observe(&packet)?;
    if let ServerBound::TeleportationAccepted { id } = packet {
        if let Some(teleports) = teleport_acknowledgements {
            teleports.accepts(id);
        }
        return Ok(());
    }
    if teleport_acknowledgements
        .as_ref()
        .is_some_and(TeleportAcknowledgements::is_pending)
        && matches!(
            &packet,
            ServerBound::PlayerMoved { .. }
                | ServerBound::PlayerRotated { .. }
                | ServerBound::PlayerStatusOnly { .. }
                | ServerBound::VehicleMoved { .. }
        )
    {
        return Ok(());
    }

    // Admit target, neighbour, and retained-light columns before any action
    // arm below performs its existing synchronous reads/writes. Awaiting here
    // keeps packets ordered and, for the integrated `Shared` source, moves
    // every cold `column()` call off this connection task. A cold admission is
    // never a reason to drop the packet.
    admit_action_footprint(source, &packet).await?;

    match packet {
        ServerBound::KeepAlive { id } => {
            if *pending_keep_alive == Some(id) {
                *pending_keep_alive = None;
            }
        }
        ServerBound::PlayerMoved {
            x,
            y,
            z,
            rotation,
            on_ground,
        } => {
            if abilities.flying {
                fall.reset();
            }
            let previous_pos = *player_pos;
            // Hunger exhaustion for the distance just travelled — vanilla's
            // vanilla's own check-movement-statistics routine, which is driven by the
            // position delta rather than by a per-tick constant. Charged **before**
            // `player_pos` is overwritten, because the delta needs the old value.
            //
            // Vanilla's expression is `0.1F * cm * 0.01F` where
            // `cm = round(sqrt(dx² + dz²) * 100)` — an `int` — so the rounding is
            // reproduced rather than collapsed into `0.1 * blocks`. It matters at
            // small steps: a sub-half-centimetre move rounds to zero centimetres and
            // costs nothing at all, which is what keeps a jittering client from
            // accumulating exhaustion.
            //
            // Only the **sprinting on ground** branch is charged, and that is not a
            // simplification: walking and crouching are literal `0.0F` multiplies in
            // vanilla, so the other on-ground branches genuinely cost nothing. The
            // swimming and eye-underwater branches (`0.01F`) are the real omission —
            // they need `isSwimming`/`isEyeInFluid`, which this arm does not have,
            // and charging sprint's constant for them would be ten times too much.
            if let Some((px, _, pz)) = *player_pos
                && *sprinting
                && on_ground
                && !Abilities::for_mode(*game_mode).invulnerable
            {
                let dx = x - px;
                let dz = z - pz;
                let cm = ((dx * dx + dz * dz).sqrt() as f32 * 100.0).round() as i32;
                if cm > 0 {
                    vitals.add_exhaustion(
                        crate::food::EXHAUSTION_SPRINT_PER_BLOCK * cm as f32 * 0.01,
                    );
                }
            }
            *player_pos = Some((x, y, z));
            let delta = previous_pos.map_or_else(
                || Vec3::new(0.0, 0.0, 0.0),
                |(previous_x, previous_y, previous_z)| {
                    Vec3::new(x - previous_x, y - previous_y, z - previous_z)
                },
            );
            client_movement.observe(delta, on_ground);
            // `move_player_pos_rot` carries angles and
            // `move_player_pos` does not, so this is `if let`, not an
            // assignment — overwriting with `None` on every straight-line
            // step would snap the avatar back to yaw 0 between turns, which
            // is a worse failure than never turning at all because it only
            // shows up while moving.
            if let Some(rotation) = rotation {
                *player_rot = Some(rotation);
            }

            if let Some(registry) = players {
                registry.set_presence(player_entity_id, source.dimension(), Vec3::new(x, y, z));
                if let Some(rotation) = *player_rot {
                    registry.set_rotation(player_entity_id, rotation);
                }
            }
            if world.dimension_runtime(source.dimension()).is_none() {
                let facing = player_rot.unwrap_or_default();
                let yaw = f64::from(facing.yaw).to_radians();
                let pitch = f64::from(facing.pitch).to_radians();
                let perceived = players.map_or_else(|| vec![PerceivedPlayer {
                    identity: Some(PlayerIdentity { uuid: player_uuid, entity_id: player_entity_id }),
                    perception: PlayerPerception {
                        position: Vec3::new(x, y, z),
                        held_item: inventory.selected_item().map(|stack| stack.item.clone()),
                        view_direction: Vec3::new(-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()),
                    },
                }], |registry| registry.perceptions(source.dimension()));
                mobs.with(|sim| { sim.set_players(perceived); });
            }

            // Chunk coordinate = floor(block / 16), not truncating division —
            // `-1.0_f64 / 16.0` must floor to chunk `-1`.
            let cx = (x / 16.0).floor() as i32;
            let cz = (z / 16.0).floor() as i32;
            if world.dimension_runtime(source.dimension()).is_none() {
                world.tick_anchors().publish(players.map_or_else(|| vec![crate::tick_area::TickAnchor {
                    dimension: source.dimension(), cx, cz,
                }], PlayerRegistry::tick_anchors));
            }
            // Read the center before the call, since `recenter` writes
            // `self.center` in place; comparing after would always see the
            // new value and move the ticket pair even on a no-op pass.
            let center_before_recenter = view.center;
            let update = view.recenter(
                proto,
                cx,
                cz,
                // The pose that arrived with this very packet where it carried
                // one, so the newly-visible strip is ordered towards what the
                // player is looking at rather than by `cx` then `cz`.
                player_rot.map(|rotation| rotation.yaw),
            );
            if view.center != center_before_recenter {
                player_ticket_guard.move_to_with_simulation_radius(
                    view.center,
                    view.radius,
                    view.radius.clamp(0, crate::chunk_store::CONCURRENT_TICK_RADIUS),
                );
                source.get().reconcile_ticket_residency();
            }
            send_view_update(
                conn,
                proto,
                source,
                join_stream.as_deref_mut(),
                state,
                view,
                update,
                awaiting_chunk_batch_ack,
                pending_chunk_batches,
            )
            .await?;

            if *client_loaded
                && let Some(sample) = resident_fall_sample(source.get(), x, y, z, on_ground)
                && let Some(raw) = fall.on_player_moved(sample)
                && !Abilities::for_mode(*game_mode).invulnerable
                && vitals.apply_fall_damage(raw as f32).is_some()
            {
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    // Self-facing, per `fall_status_sample`'s own call site comment.
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::Fall,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }
        // A player turning on the spot sends `move_player_rot`
        // and nothing else, so without this arm their avatar only ever
        // re-aimed on ticks where they also happened to walk.
        //
        // No view-streaming recentre here, deliberately: this packet carries
        // no position, so the chunk column cannot have changed and calling
        // `view.recenter` would re-derive the same centre from a stale
        // `player_pos` for no reason.
        ServerBound::PlayerRotated {
            yaw,
            pitch,
            on_ground,
        } => {
            if abilities.flying {
                fall.reset();
            }
            client_movement.observe(Vec3::new(0.0, 0.0, 0.0), on_ground);
            *player_rot = Some(Rotation { yaw, pitch });
            fall_status_sample(
                conn,
                state,
                proto,
                source.get(),
                player_pos,
                fall,
                vitals,
                effects,
                username,
                on_ground,
                *client_loaded,
                Abilities::for_mode(*game_mode).invulnerable,
                advancements,
                player_uuid,
            )
            .await?;
        }
        // Carries only the flags byte, so its whole job is the `on_ground`
        // edge. This records a landing even when the final movement packet
        // carries no position change.
        ServerBound::PlayerStatusOnly { on_ground } => {
            if abilities.flying {
                fall.reset();
            }
            client_movement.observe(Vec3::new(0.0, 0.0, 0.0), on_ground);
            fall_status_sample(
                conn,
                state,
                proto,
                source.get(),
                player_pos,
                fall,
                vitals,
                effects,
                username,
                on_ground,
                *client_loaded,
                Abilities::for_mode(*game_mode).invulnerable,
                advancements,
                player_uuid,
            )
            .await?;
        }
        // `Q` / `Ctrl+Q`. Vanilla refuses in spectator and nowhere else —
        // creative included, where `handleCreativeModeItemDrop` is a no-op on the
        // server and the stack really does leave the inventory.
        ServerBound::ItemDropped { whole_stack } => {
            if !matches!(*game_mode, GameMode::Spectator) {
                let directive = apply_item_dropped(
                    proto,
                    inventory,
                    open_container.as_mut(),
                    *player_pos,
                    *player_rot,
                    whole_stack,
                    drops_rng,
                    mobs,
                );
                if let Some(directive) = directive {
                    apply(conn, state, directive).await?;
                }
            }
        }
        ServerBound::BlockAction {
            action,
            pos,
            face: _,
            sequence: _,
        } => {
            apply_block_action(
                conn,
                proto,
                // The block write is immediate; its light update may be queued.
                source.get(),
                state,
                pending_relights.as_deref_mut(),
                pending_break,
                block_entities,
                open_container,
                container_sync,
                mobs,
                drops_rng,
                inventory.selected_item(),
                // The breaker's feet for the interaction-range test. Use the
                // tracked `player_pos`; `None` means no movement packet exists.
                player_pos.as_ref().map(|&(x, y, z)| Vec3::new(x, y, z)),
                world,
                game_tick,
                block_ticks,
                player_uuid,
                matches!(*game_mode, GameMode::Creative),
                action,
                advancements,
                vitals,
                pos,
            )
            .await?;
        }
        ServerBound::UseItemOn {
            pos,
            face,
            cursor,
            sequence: _,
            hand,
        } => {
            // Draw one roll per right-click, regardless of the clicked block;
            // the composter branch is the only consumer of this stream.
            let roll = composter_rng.next_f64();
            // Same reasoning, `drops_rng`'s own stream: only an enchanting-table
            // open consumes this, but it is drawn unconditionally so opening one
            // does not depend on which block was clicked last.
            let enchant_seed_roll = i64::from(drops_rng.next_int(i32::MAX));
            apply_use_item_on(
                conn,
                proto,
                // The block write is immediate; its light update may be queued.
                source.get(),
                state,
                pending_relights.as_deref_mut(),
                pos,
                face,
                cursor,
                // The player's position, for the bed reach test —
                // `None` until a `PlayerMoved` packet carries one.
                player_pos.as_ref().map(|&(x, y, z)| Vec3::new(x, y, z)),
                respawn,
                // The placing player's yaw and pitch, so
                // `apply_use_item_on` can give directional blocks their
                // placement facing. `None` until a packet carrying angles
                // arrives — placement then uses the block's default state.
                player_rot.map(|rotation| rotation.yaw),
                player_rot.map(|rotation| rotation.pitch),
                *sneaking,
                player_uuid,
                inventory,
                block_entities,
                next_window_id,
                open_container,
                container_sync,
                mobs,
                roll,
                block_ticks,
                sleep_vote,
                player_entity_id,
                bone_meal_rng,
                world.difficulty().0,
                *game_mode,
                enchant_seed_roll,
                hand,
                world.crafting_hooks(),
            )
            .await?;
        }
        ServerBound::DifficultyChanged { difficulty } => {
            // A difficulty change requires permission level `2`. A locked world
            // rejects the mutation, but the confirmation below is sent either
            // way with the value actually stored, keeping the client's display
            // aligned with the server.
            if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                world.set_difficulty(difficulty);
            }
            apply_difficulty_change(conn, proto, state, world).await?;
        }
        ServerBound::DifficultyLockChanged { locked } => {
            // Same gate as `DifficultyChanged` above — vanilla's own
            // lock-difficulty handler checks the identical permission.
            if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                world.set_difficulty_locked(locked);
            }
            apply_difficulty_change(conn, proto, state, world).await?;
        }
        ServerBound::GameRuleChanged { entries } => {
            // Vanilla's own set-game-rule handler's own gate —
            // see `DifficultyChanged`'s own comment above for why
            // `commands.permission_level` is the right check to reuse. A
            // refused request sets nothing, so `apply_game_rule_changed`'s own
            // "confirm with exactly what was set" reply is naturally empty
            // rather than needing a separate no-op branch.
            let entries = if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                entries
            } else {
                Vec::new()
            };
            apply_game_rule_changed(conn, proto, state, world, entries).await?;
        }
        ServerBound::CarriedItemChanged { slot } => {
            // Switching slots cancels an in-progress bite rather than allowing it
            // to complete against the replacement item. `finish_consuming` also
            // re-checks the item because a container click can change the same
            // slot without this packet.
            *item_in_use = None;
            apply_carried_item_changed(inventory, slot);
        }
        ServerBound::ContainerClicked {
            window_id,
            state_id: _,
            slot,
            button,
            click_type,
            changed_slots,
            carried_item,
        } => {
            // A workstation result charges or refunds experience only when the
            // result is taken. Capture the input cells before dispatch because
            // the click handler mutates them; this arm applies the associated
            // experience change after the result transition is confirmed.
            let workstation_take = open_container.as_ref().and_then(|tracked| {
                let MenuKind::ItemCombiner { inputs, station } = tracked.shape else {
                    return None;
                };
                (tracked.window_id == window_id && usize::try_from(slot).ok() == Some(inputs)).then_some(station)
            });
            let pre_click_cells = workstation_take.map(|_| inventory.workstation().map(<[_]>::to_vec).unwrap_or_default());
            // Compared, not assumed dirty: a container click into the crafting
            // grid or a non-equipment slot must not spam an unchanged
            // `update_attributes`, and this is cheaper than working out from
            // `changed_slots` alone whether one of them was an armour/off-hand
            // native index.
            let attrs_before_click = player_attribute_snapshots(inventory);

            let (correction, dropped) = apply_container_clicked(
                proto,
                inventory,
                block_entities,
                open_container.as_mut(),
                window_id,
                Click {
                    slot,
                    button,
                    click_type,
                },
                &changed_slots,
                carried_item.as_ref(),
                *game_mode == GameMode::Creative,
                experience.level(),
                world.crafting_hooks(),
            );
            spawn_dropped_stacks(mobs, *player_pos, *player_rot, drops_rng, dropped);

            let mut experience_changed = false;
            if let (Some(station), Some(cells)) = (workstation_take, pre_click_cells) {
                let get = |i: usize| cells.get(i).and_then(Option::as_ref);
                // A refused take leaves the result input intact, so the
                // pre-click cells alone cannot justify an experience charge.
                // Clearing input cell 0 is the observable transition that
                // confirms a result was taken.
                let took_result = get(0).is_some()
                    && inventory
                        .workstation()
                        .and_then(<[_]>::first)
                        .is_some_and(Option::is_none);
                match station {
                    Station::Anvil => {
                        if took_result {
                            let outcome = crate::anvil::compute(get(0), get(1), inventory.pending_rename(), *game_mode == GameMode::Creative);
                            if outcome.result.is_some() && *game_mode != GameMode::Creative {
                                experience.take_levels(outcome.cost);
                                experience_changed = true;
                            }
                        }
                    }
                    Station::Grindstone => {
                        // A valid grindstone result is available whenever its
                        // inputs produce one; `took_result` also excludes a
                        // click on an empty result slot.
                        if took_result && crate::anvil::grindstone_result(get(0), get(1)).is_some() {
                            let awarded = crate::anvil::grindstone_xp(get(0), get(1), drops_rng);
                            if awarded > 0 {
                                experience.give_points(i32::try_from(awarded).unwrap_or(i32::MAX));
                                experience_changed = true;
                            }
                        }
                    }
                    // Loom, stonecutter, and smithing results do not change
                    // experience; their cost is represented by consumed inputs.
                    Station::Smithing | Station::Loom | Station::Stonecutter => {}
                }
            }
            if experience_changed {
                republish_experience(players, player_uuid, experience);
                apply(
                    conn,
                    state,
                    proto.encode_set_experience(experience.progress(), experience.level(), experience.total()),
                )
                .await?;
            }

            if let Some(correction) = correction {
                apply(conn, state, correction).await?;
            }
            // The armour bar's own packet — see `join_attributes`. A container
            // click is the other way equipment changes (drag/shift-click into
            // the armour slots, not just the right-click swap
            // `UseItemOutcome::Equipped` covers), so it needs the same resync.
            if player_attribute_snapshots(inventory) != attrs_before_click {
                apply(conn, state, join_attributes(proto, inventory)).await?;
            }
        }
        ServerBound::RecipePlaced {
            window_id,
            recipe_index,
            use_max_items,
        } => {
            if let Some(correction) = apply_recipe_placed(
                proto,
                inventory,
                open_container.as_mut(),
                window_id,
                recipe_index,
                use_max_items,
            ) {
                apply(conn, state, correction).await?;
            }
        }
        ServerBound::RecipeBookSettingsChanged {
            book_type,
            open,
            filtering,
        } => {
            inventory.set_recipe_book_settings(book_type, open, filtering);
        }
        ServerBound::RecipeBookRecipeSeen { recipe_index } => {
            if let Some(entry) = record_recipe_book_seen(inventory, recipe_index) {
                apply(conn, state, proto.encode_recipe_book_add(&[entry], false)).await?;
            }
        }
        ServerBound::SeenAdvancements { tab } => {
            let selected = advancements.select_tab(player_uuid, tab);
            apply(conn, state, proto.encode_select_advancements_tab(selected.as_deref())).await?;
        }
        ServerBound::ResourcePackResponse { id, response } => {
            resource_packs.record_response(ResourcePackResponseRecord { id, response });
        }
        ServerBound::PlayerLoaded => {
            *client_loaded = true;
        }
        ServerBound::ClientTickEnded => {
            client_movement.finish_tick();
        }
        ServerBound::PlayerAbilitiesChanged { flying } => {
            abilities.flying = (*game_mode == GameMode::Spectator || flying) && abilities.may_fly;
            if abilities.flying {
                fall.cancel();
            }
        }
        ServerBound::BlockEntityTagQuery { transaction_id, pos } => {
            if let Some(tag) = block_entity_query_tag(block_entities, commands.permission_level, pos) {
                apply(conn, state, proto.encode_tag_query(transaction_id, tag.as_ref())).await?;
            }
        }
        ServerBound::EntityTagQuery { transaction_id, entity_id } => {
            if let Some(tag) = entity_query_tag(mobs, commands.permission_level, entity_id) {
                apply(conn, state, proto.encode_tag_query(transaction_id, Some(&tag))).await?;
            }
        }
        ServerBound::ContainerClosed { window_id } => {
            // Closing returns carried items and virtual crafting/workstation
            // cells to the player's inventory; overflow becomes a dropped
            // stack so closing a menu cannot delete items.
            let mut returning = inventory.take_table_crafting();
            returning.extend(inventory.take_workstation());
            if let Some(carried) = inventory.click_state_mut().carried.take() {
                returning.push(carried);
            }
            inventory.click_state_mut().reset();
            // Bundle selection belongs to the open menu and is cleared with the
            // other menu-local scratch state.
            inventory.clear_selected_bundle_items();
            let mut spilled = Vec::new();
            let mut changed = Vec::new();
            for stack in returning {
                let (written, leftover) = inventory.add(stack);
                changed.extend(written);
                if let Some(leftover) = leftover {
                    spilled.push(leftover);
                }
            }
            changed.sort_unstable();
            changed.dedup();
            for native in changed {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                    )
                    .await?;
                }
            }
            // A beacon payment is dropped directly rather than merged into the
            // inventory. It lives on the block entity, outside virtual menu
            // scratch storage, so read the payment field directly.
            if open_container.as_ref().is_some_and(|open| open.window_id == window_id && open.shape == MenuKind::Beacon)
                && let Some(pos) = open_container.as_ref().map(|open| open.pos)
                && let Some(payment) = block_entities.with(|reg| match reg.get_mut(pos) {
                    Some(BlockEntity::Beacon(beacon)) => beacon.payment.take(),
                    _ => None,
                })
            {
                spilled.push(payment);
            }
            spawn_dropped_stacks(mobs, *player_pos, *player_rot, drops_rng, spilled);
            if open_container.as_ref().is_some_and(|open| open.window_id == window_id) {
                // Furnace experience is paid on close from recipes accumulated
                // since the last drain. Experience orbs are not modeled here, so
                // award the points directly to the player's experience bar.
                let pos = open_container.as_ref().map(|open| open.pos);
                if let Some(pos) = pos {
                    let used = block_entities.with(|reg| match reg.get_mut(pos) {
                        Some(BlockEntity::Furnace(furnace)) => furnace.take_recipes_used(),
                        _ => std::collections::HashMap::new(),
                    });
                    if !used.is_empty() {
                        let points = crate::furnace::experience_for_recipes(&used, || {
                            drops_rng.next_f32()
                        });
                        if points > 0 {
                            experience.give_points(i32::try_from(points).unwrap_or(i32::MAX));
                            republish_experience(players, player_uuid, experience);
                            apply(
                                conn,
                                state,
                                proto.encode_set_experience(
                                    experience.progress(),
                                    experience.level(),
                                    experience.total(),
                                ),
                            )
                            .await?;
                        }
                    }
                }
                *open_container = None;
                *container_sync = ContainerSync::default();
            }
            // Any window close ends the active menu, including a merchant menu.
            // `OpenMerchant` has no window id, so clear it unconditionally.
            *open_merchant = None;
        }
        // Merchant trade-row selection. See `attempt_villager_trade`'s own
        // doc for why this executes the trade in one operation rather than through
        // a payment-slot placement flow.
        ServerBound::SelectTrade { index } => {
            if let Some(OpenMerchant { entity_id }) = *open_merchant
                && let Some(index) = usize::try_from(index).ok()
            {
                // Read back from the villager's *persistent*
                // [`crate::villager_trade::VillagerTrades`], so the charged
                // price reflects accumulated demand and this offer's
                // out-of-stock state, and is
                // derived identically to what `open_merchant_screen` sent.
                let reputation = mobs.with(|sim| sim.villager_reputation(entity_id, player_uuid));
                let hero_of_the_village_amplifier =
                    effects.amplifier_of("minecraft:hero_of_the_village");
                // A read-only priced peek, so a buyer who cannot afford it
                // never moves the villager's uses/demand — only the
                // `try_villager_trade` commit below does that, and only
                // after `attempt_villager_trade` confirms the inventory can
                // actually pay.
                let offer = mobs.with(|sim| {
                    sim.villager_offers(entity_id, reputation, hero_of_the_village_amplifier)
                        .get(index)
                        .copied()
                });
                if let Some(offer) = offer
                    && let Some(next) = attempt_villager_trade(inventory, &offer)
                    && mobs
                        .with(|sim| {
                            sim.try_villager_trade(entity_id, index, reputation, hero_of_the_village_amplifier)
                        })
                        .is_some()
                {
                    *inventory = next;
                    // A completed trade records `Trading` gossip through
                    // `record_reputation_event`, matching the villager-hit
                    // path in `MobSim::attack_from_player`.
                    mobs.with(|sim| {
                        sim.record_reputation_event(
                            entity_id,
                            crate::mobs::villager::reputation::ReputationEventType::Trade,
                            player_uuid,
                        );
                    });
                    // A full window-0 resync rather than a per-slot diff:
                    // the cost items can land anywhere across 36 slots and
                    // the given item anywhere `add` found room, so there
                    // is no fixed pair of menu slots to name — the same
                    // reasoning `join_inventory_snapshot` already
                    // documents for why this packet (not a per-slot one)
                    // is the right shape for an arbitrary multi-slot
                    // change.
                    let items = read_menu(
                        &MenuLayout::player(),
                        inventory,
                        Some(inventory.crafting()),
                        &[],
                    );
                    apply(
                        conn,
                        state,
                        proto.encode_container_content(
                            0,
                            0,
                            &items,
                            inventory.click_state().carried.as_ref(),
                        ),
                    )
                    .await?;
                }
            }
        }
        // The anvil-menu item-name setter. See `apply_rename_item`'s own doc
        // for the gate and the state resent to the client.
        ServerBound::RenameItem { name } => {
            let creative = *game_mode == GameMode::Creative;
            for directive in apply_rename_item(proto, inventory, open_container.as_mut(), &name, creative, world.crafting_hooks()) {
                apply(conn, state, directive).await?;
            }
        }
        // Command-block packets update the mode, conditional flag, command,
        // output tracking, and automatic scheduling. A redstone signal is
        // still required for execution; this packet only changes configuration.
        ServerBound::SetCommandBlock { pos, command, mode, track_output, conditional, automatic } => {
            // Creative mode and permission level `2` are both required. The
            // mode-derived ability and `commands.permission_level` are already
            // resolved on this connection, so this gate only combines them.
            let can_use_game_master_blocks =
                *game_mode == GameMode::Creative && commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL;
            let is_command_block = can_use_game_master_blocks
                && block_entities.with(|reg| matches!(reg.get(pos), Some(BlockEntity::CommandBlock(_))));
            if is_command_block {
                let current_state = source.get().block_state_id(pos.x, pos.y, pos.z);
                let facing = crate::command_block::facing(current_state);
                let base = crate::command_block::base_name_for_mode(mode);
                let new_state = crate::command_block::state_with(base, facing, conditional);
                if new_state != current_state {
                    source.get().set_block(pos.x, pos.y, pos.z, new_state);
                    block_ticks.publish(pos.x, pos.y, pos.z, new_state.clone());
                }
                let new_mode = crate::command_block::mode_for_block(new_state);
                // A conditional command block checks the block directly behind
                // its facing. Read that state before taking the registry lock.
                let predecessor_succeeded = conditional.then(|| {
                    let behind = facing.opposite().relative(pos);
                    let behind_state = source.get().block_state_id(behind.x, behind.y, behind.z);
                    crate::command_block::is_command_block_family(behind_state)
                        && block_entities.with(|reg| {
                            matches!(reg.get(behind), Some(BlockEntity::CommandBlock(d)) if d.success_count > 0)
                        })
                });
                let should_schedule = block_entities.with(|reg| {
                    let Some(BlockEntity::CommandBlock(data)) = reg.get_mut(pos) else { return false };
                    data.set_command(command);
                    data.track_output = track_output;
                    if !track_output {
                        data.last_output = None;
                    }
                    let should_schedule =
                        crate::command_block::on_automatic_changed(new_mode, data.auto, automatic, data.powered);
                    data.auto = automatic;
                    if should_schedule {
                        data.condition_met =
                            crate::command_block::mark_condition_met(conditional, predecessor_succeeded);
                    }
                    should_schedule
                });
                if should_schedule {
                    block_ticks.request_scheduled_ticks(crate::command_block::ticks_after_schedule(pos));
                }
            }
        }
        // Sign updates strip legacy formatting codes from every line, then
        // `SignData` checks the wax and editor fields before writing. The editor
        // is assigned at placement (see `crate::block_entities::SignData`), so
        // each sign accepts its authorized edit.
        ServerBound::SignUpdate { pos, is_front_text, lines } => {
            let stripped = lines.map(|line| crate::block_entities::strip_sign_formatting(&line));
            block_entities.with(|registry| {
                if let Some(entity) = registry.get_mut(pos) {
                    crate::block_entities::apply_sign_update(entity, player_uuid, is_front_text, stripped);
                }
            });
        }
        // Book edits use `apply_edit_book`'s gate and resend the changed item
        // through `CONTAINER_SET_SLOT` on window `0` (the player's inventory,
        // independent of any open menu), the same "window 0,
        // state id 0" pattern every other server-initiated inventory-slot
        // write in this function already uses.
        ServerBound::EditBook { slot, pages, title } => {
            if let Some((native, item)) = apply_edit_book(inventory, slot, pages, title, username)
                && let Some(menu_slot) = window_zero_menu_slot(native)
            {
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, menu_slot, Some(&item)),
                )
                .await?;
            }
        }
        // A bundle-tooltip highlight claim. Stored, not acted on
        // immediately — `container_click::pickup`'s next right-click-on-empty
        // against this slot is what actually reads it
        // (`selected_bundle_item`); the next empty-slot pickup reads that index
        // and extracts the selected item.
        ServerBound::SelectBundleItem { slot_id, selected_item_index } => {
            let Some(slot) = MenuSlot::from_raw(slot_id) else {
                return Ok(());
            };
            let selected = BundleItemSlot::from_wire(selected_item_index);
            inventory.set_bundle_item_selection(slot, selected);
        }
        // Beacon configuration. See `apply_set_beacon`'s own doc for the gate.
        ServerBound::SetBeacon { primary, secondary } => {
            let directives =
                apply_set_beacon(proto, block_entities, open_container.as_mut(), primary, secondary);
            for directive in directives {
                apply(conn, state, directive).await?;
            }
        }
        // The enchanting table's "choose an offer" button. See
        // `apply_container_button_click`'s own doc for the pricing and
        // refusal rules.
        ServerBound::ContainerButtonClick { window_id, button_id } => {
            let creative = *game_mode == GameMode::Creative;
            let lectern = open_container
                .as_ref()
                .is_some_and(|open| open.window_id == window_id && open.shape == MenuKind::Lectern);
            if let Some(pos) = open_container
                .as_ref()
                .filter(|open| open.window_id == window_id)
                .map(|open| open.pos)
            {
                source
                    .admit_columns(column_admission_footprint(
                        pos.x.div_euclid(16),
                        pos.z.div_euclid(16),
                        1,
                    ))
                    .await?;
            }
            // Drawn unconditionally, whether or not the click succeeds — the
            // same "one draw per attempt" reasoning `apply_use_item_on`'s own
            // composter roll already documents.
            let fresh_seed = i64::from(drops_rng.next_int(i32::MAX));
            let directives = if lectern {
                apply_lectern_button_click(
                    proto,
                    block_entities,
                    inventory,
                    open_container.as_mut(),
                    window_id,
                    button_id,
                    !matches!(*game_mode, GameMode::Adventure | GameMode::Spectator),
                )
            } else {
                apply_container_button_click(
                    proto,
                    inventory,
                    open_container.as_mut(),
                    window_id,
                    button_id,
                    source.get(),
                    experience,
                    creative,
                    fresh_seed,
                    world.crafting_hooks(),
                )
            };
            // A no-op when the click was refused (`experience` untouched, so
            // this resends the same level/points it already holds) — cheaper
            // to call unconditionally than to thread a "did it actually spend
            // levels" flag out of `apply_container_button_click` just for this.
            republish_experience(players, player_uuid, experience);
            for directive in directives {
                apply(conn, state, directive).await?;
            }
            if lectern {
                // Keep the timer-driven diff baseline aligned immediately;
                // otherwise the next 50 ms tick would resend the same book
                // removal/page value after this authoritative action.
                if let Some(open) = open_container.as_ref() {
                    let (slots, data) = container_state(block_entities, open.pos);
                    container_sync.slots = slots;
                    container_sync.data = data;
                }
                republish_inventory(players, player_uuid, inventory);
            }
        }
        // A crafter's per-slot enable/disable toggle.
        // No directive to send back: `container_sync_tick`'s existing
        // `sync_open_container` diff already re-reads `data_properties()`
        // every 50ms and pushes whatever changed, the same path a furnace's
        // own background tick uses — there is nothing crafter-specific to
        // wire on the send side.
        ServerBound::ContainerSlotStateChanged { window_id, slot_id, new_state } => {
            let matching_pos = open_container
                .as_ref()
                .filter(|open| open.window_id == window_id)
                .map(|open| open.pos);
            if let Some(pos) = matching_pos
                && let Some(slot) = u8::try_from(slot_id)
                    .ok()
                    .and_then(lodestone_model::CrafterSlot::new)
            {
                block_entities.with(|reg| {
                    if let Some(entity) = reg.get_mut(pos) {
                        entity.set_crafter_slot_state_at(slot, new_state);
                    }
                });
            }
        }
        ServerBound::Attack { entity_id } => {
            // An attack packet also starts the main-hand swing animation. The
            // local client renders its own arm immediately, but every other
            // connection learns that animation through the shared swing log.
            // Record it even when the target is unknown: the wire action is a
            // swing first, while damage validation is a separate concern.
            record_attack_swing(players, player_entity_id);
            apply_attack(
                mobs,
                *player_pos,
                *sprinting,
                inventory,
                effects,
                entity_id,
                player_uuid,
            );
            // Attack exhaustion is charged on every living-target swing, not
            // only when the damage attempt lands.
            if !Abilities::for_mode(*game_mode).invulnerable {
                vitals.add_exhaustion(crate::food::EXHAUSTION_ATTACK);
            }
        }
        // The right-click interaction path covers taming, feeding, sitting,
        // breeding, and vehicle mounting through `MobSim::interact`.
        ServerBound::InteractEntity {
            entity_id,
            hand,
            using_secondary_action,
        } => {
            // Resolve only the main-hand interaction. A client can send both
            // hand values for one click; running both would roll a tame chance
            // twice.
            if hand == 0 {
                // Board boats before generic mob interaction. Boats are
                // vehicles, not tamable mobs, and require a passenger-list
                // update when boarding succeeds.
                //
                // `using_secondary_action` prevents boarding while the player
                // is sneaking.
                if mobs.with(|sim| sim.vehicle_type(entity_id).is_some()) {
                    let boarded = mobs.with(|sim| {
                        sim.mount_vehicle(entity_id, player_entity_id, using_secondary_action)
                    });
                    if boarded {
                        // Send the vehicle's **whole** passenger list rather than
                        // a delta. Without this packet the client
                        // has no way to know it is aboard and
                        // `lodestone_ecs::vehicle::tick_controlled_vehicle` never
                        // engages, so the boat is placeable and unusable.
                        //
                        // `LOCAL_PLAYER_ENTITY_ID`, not `player_entity_id`: this goes
                        // straight to `conn`, this connection's own socket, and the
                        // client only recognises itself among the passengers under
                        // the constant its own login entity-id field claimed — see
                        // `publish_health`'s call sites for the same rule.
                        // `sim.mount_vehicle` above still records the real
                        // `player_entity_id`, which is what a *future* multi-connection
                        // broadcast of this vehicle's passengers would need.
                        apply(
                            conn,
                            state,
                            proto.encode_set_passengers(entity_id, &[LOCAL_PLAYER_ENTITY_ID]),
                        )
                        .await?;
                    }
                    // Boarding consumes no item, and a refused board must not fall
                    // through to the taming chain — a boat is not tameable and the
                    // fall-through would only cost a wasted roll.
                    return Ok(());
                }
                // Minecarts are vehicles rather than tamable mobs, so handle
                // them before generic interaction.
                if let Some(kind) = mobs.with(|sim| sim.minecart_kind(entity_id)) {
                    if kind.is_furnace() {
                        // Coal and charcoal add fuel; consume one item only on
                        // a successful fuel update.
                        let held = inventory.selected_item().map(|stack| stack.item.to_string());
                        if let Some(item) = held {
                            let interacting_pos = player_pos.map_or_else(
                                || {
                                    mobs.with(|sim| sim.minecart_transform(entity_id))
                                        .map_or(Vec3::new(0.0, 0.0, 0.0), |(p, _)| p)
                                },
                                |(x, y, z)| Vec3::new(x, y, z),
                            );
                            let fuelled = mobs.with(|sim| sim.add_minecart_fuel(entity_id, &item, interacting_pos));
                            if fuelled {
                                let native = usize::from(inventory.selected_hotbar_slot());
                                if consume_one(inventory, native, *game_mode) {
                                    let hotbar_slot = i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                                    apply(
                                        conn,
                                        state,
                                        proto.encode_container_slot(0, 0, hotbar_slot, inventory.native(native)),
                                    )
                                    .await?;
                                }
                            }
                        }
                    } else if kind.is_rideable() && !using_secondary_action {
                        // Mount the minecart and send the same passenger-list
                        // handoff used by the boat arm above.
                        let boarded = mobs.with(|sim| sim.mount_minecart(entity_id, player_entity_id));
                        if boarded {
                            apply(
                                conn,
                                state,
                                proto.encode_set_passengers(entity_id, &[LOCAL_PLAYER_ENTITY_ID]),
                            )
                            .await?;
                        }
                    }
                    // Chest, hopper, and TNT minecarts have no modeled
                    // interaction; their slots have no menu wired to them.
                    return Ok(());
                }
                let held = inventory.selected_item().map(|stack| stack.item.clone());
                // A name tag that carries a name names the mob and is consumed;
                // an unnamed tag does nothing and falls through.
                if let Some(stack) = inventory.selected_item()
                    && stack.item.path() == "name_tag"
                    && let Some(name) = stack.components.custom_name.as_ref()
                {
                    let component = lodestone_core::Nbt::String(name.to_plain_string());
                    if mobs.with(|sim| sim.apply_name_tag(entity_id, component)) {
                        let native = usize::from(inventory.selected_hotbar_slot());
                        if consume_one(inventory, native, *game_mode) {
                            let hotbar_slot =
                                i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(0, 0, hotbar_slot, inventory.native(native)),
                            )
                            .await?;
                        }
                        return Ok(());
                    }
                }
                // Leash handling precedes taming, feeding, and breeding. A lead
                // in hand attaches or detaches a leash without rolling another
                // interaction.
                let leash_outcome = mobs.with(|sim| {
                    sim.try_leash(
                        entity_id,
                        player_uuid,
                        held.as_ref().is_some_and(|item| item.to_string() == "minecraft:lead"),
                        *game_mode == GameMode::Creative,
                    )
                });
                let outcome = match leash_outcome {
                    crate::mobs::LeashOutcome::Attached => {
                        // Consume one item through the same `consume_one` and
                        // window-0 synchronization used by other interactions.
                        let native = usize::from(inventory.selected_hotbar_slot());
                        if consume_one(inventory, native, *game_mode) {
                            let hotbar_slot = i32::from(inventory.selected_hotbar_slot())
                                + WINDOW_ZERO_HOTBAR_FIRST;
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(
                                    0,
                                    0,
                                    hotbar_slot,
                                    inventory.native(native),
                                ),
                            )
                            .await?;
                        }
                        None
                    }
                    // `MobSim::try_leash` spawns a dropped lead when required;
                    // this arm has no additional work.
                    crate::mobs::LeashOutcome::Detached { .. } => None,
                    // A non-leashable, out-of-range, or already-owned target
                    // falls through to the ordinary mob interaction.
                    crate::mobs::LeashOutcome::Refused => Some(mobs.with(|sim| {
                        sim.interact(
                            entity_id,
                            PlayerIdentity {
                                uuid: player_uuid,
                                entity_id: player_entity_id,
                            },
                            held.as_ref(),
                        )
                    })),
                };
                // A villager trade outcome opens its screen before generic
                // item-consumption handling; opening the screen is the visible
                // effect and requires no slot synchronization.
                if let Some(crate::mobs::InteractOutcome::OpenTrade { level, .. }) = outcome {
                    let xp = mobs.with(|sim| sim.villager_xp(entity_id));
                    let reputation = mobs.with(|sim| sim.villager_reputation(entity_id, player_uuid));
                    let hero_of_the_village_amplifier =
                        effects.amplifier_of("minecraft:hero_of_the_village");
                    // The villager's *persistent* offer list. The mob supplies
                    // its live profession and level through
                    // `MobSim::villager_offers`.
                    let offers =
                        mobs.with(|sim| sim.villager_offers(entity_id, reputation, hero_of_the_village_amplifier));
                    open_merchant_screen(conn, proto, state, &offers, level, xp, next_window_id).await?;
                    // Record which villager this connection is trading with.
                    // Each open replaces the connection's active merchant
                    // screen and its associated entity id.
                    *open_merchant = Some(OpenMerchant { entity_id });
                }
                // A successful mount is recorded in `MobSim`; send the complete
                // passenger list so the client learns that it is aboard.
                if outcome == Some(crate::mobs::InteractOutcome::Mounted) {
                    apply(
                        conn,
                        state,
                        proto.encode_set_passengers(entity_id, &[LOCAL_PLAYER_ENTITY_ID]),
                    )
                    .await?;
                }
                // `consume_one` handles creative mode. A sit toggle has no item
                // cost, as encoded by `InteractOutcome::consumes_item`.
                //
                // `consume_one` handles the creative case itself, so the game mode
                // goes to it rather than being checked here — and the
                // `encode_container_slot` **is not optional**: without it the server
                // and client disagree about the stack count, which is a worse bug
                // than not consuming at all (the next click sends a stale count and
                // the item appears to come back).
                if let Some(outcome) = outcome
                    && outcome.consumes_item()
                {
                    let native = usize::from(inventory.selected_hotbar_slot());
                    if consume_one(inventory, native, *game_mode) {
                        let hotbar_slot =
                            i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(
                                0,
                                0,
                                hotbar_slot,
                                inventory.native(native),
                            ),
                        )
                        .await?;
                    }
                }
            }
        }
        // The player's projectile-launch path. A successful bow use creates the
        // projectile record consumed by the entity stream.
        ServerBound::UseItem { hand, yaw, pitch, .. } => {
            // Handle boat items before the eat/equip chain. A boat is neither
            // food nor equippable, and its raytrace needs the world source.
            //
            // This branch supplies the world source required by the raytrace;
            // `apply_use_item` receives only inventory, position, and game mode.
            // The eye height comes from the tracked feet position; without one,
            // the launch arm refuses to guess.
            let boat_native = if hand == 1 {
                crate::inventory::OFFHAND_NATIVE
            } else {
                usize::from(inventory.selected_hotbar_slot())
            };
            let boat_item = inventory
                .native(boat_native)
                .map(|stack| stack.item.to_string());
            if let (Some(item), Some((px, py, pz))) = (boat_item.as_deref(), *player_pos) {
                let applied = crate::boat::apply_boat_item(
                    item,
                    Vec3::new(px, py + EYE_HEIGHT, pz),
                    yaw,
                    pitch,
                    crate::boat::block_interaction_range(*game_mode == GameMode::Creative),
                    &|x, y, z| source.get().block_state_id(x, y, z),
                    mobs,
                );
                match applied {
                    crate::boat::BoatApplied::NotABoat => {}
                    // The raytrace missed or the hull would not fit. Nothing is
                    // consumed and the item does not fall through to eat/equip.
                    crate::boat::BoatApplied::Refused => return Ok(()),
                    crate::boat::BoatApplied::Placed { .. } => {
                        // Consume one item after the boat is placed. Creative
                        // players keep their boats; survival players lose one.
                        if consume_one(inventory, boat_native, *game_mode)
                            && *game_mode != GameMode::Creative
                        {
                            // Publish the window-0 slot value so the client count
                            // stays synchronized for the next click.
                            if let Some(menu_slot) = window_zero_menu_slot(boat_native) {
                                let remainder = inventory.native(boat_native).cloned();
                                apply(
                                    conn,
                                    state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                        }
                        // A placement ends any draw or bite in progress.
                        *bow_draw = None;
                        *item_in_use = None;
                        return Ok(());
                    }
                }
            }
            // An eye of ender in the air (not aimed at a frame) flies toward the
            // nearest stronghold. Before `apply_use_item`, which has no world
            // source to locate one with.
            if let Some((px, py, pz)) = *player_pos
                && inventory
                    .native(boat_native)
                    .is_some_and(|stack| stack.item.path() == "ender_eye")
            {
                let sound_roll = drops_rng.next_f32();
                let sound = launch_eye_of_ender(
                    mobs,
                    inventory,
                    boat_native,
                    *game_mode,
                    Vec3::new(px, py, pz),
                    yaw,
                    pitch,
                    source.dimension(),
                    &|x, y, z| source.get().block_state_id(x, y, z),
                    &|from| source.get().locate_stronghold(from),
                    sound_roll,
                );
                if let Some(sound) = sound {
                    block_ticks.publish_effect(sound);
                    if *game_mode != GameMode::Creative
                        && let Some(menu_slot) = window_zero_menu_slot(boat_native)
                    {
                        let remainder = inventory.native(boat_native).cloned();
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                        )
                        .await?;
                    }
                }
                *bow_draw = None;
                *item_in_use = None;
                return Ok(());
            }
            let outcome = apply_use_item(
                mobs,
                effects,
                inventory,
                *player_pos,
                *client_movement,
                *game_mode,
                vitals.food().food_level(),
                Abilities::for_mode(*game_mode).invulnerable,
                hand,
                yaw,
                pitch,
                player_entity_id,
                player_uuid,
            );
            // Both state slots are reset for each `USE_ITEM`: a chargeable item
            // starts a new draw or bite, while another item cancels any active
            // use.
            *bow_draw = None;
            *item_in_use = None;
            match outcome {
                UseItemOutcome::Nothing => {}
                UseItemOutcome::Draw(draw) => *bow_draw = Some(draw),
                UseItemOutcome::Consuming(started) => *item_in_use = Some(started),
                UseItemOutcome::Equipped(swap) => {
                    // Every slot the swap touched, so the client's own prediction
                    // is corrected rather than left to drift. The armour slots are
                    // menu `5..=8` in window 0 (`window_zero_menu_slot`), which is
                    // what makes the piece show up in the player's own inventory
                    // screen and on the player model. It does **not** touch the
                    // armour *bar* — that reads `update_attributes`, sent
                    // separately below.
                    let mut touched = vec![swap.equipment.0, swap.hand.0];
                    touched.extend(swap.inventory.iter().copied());
                    for native in touched {
                        let Some(menu_slot) = window_zero_menu_slot(native) else {
                            continue;
                        };
                        let held = inventory.native(native).cloned();
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, held.as_ref()),
                        )
                        .await?;
                    }
                    // The armour bar's own packet — see `join_attributes`. A
                    // right-click equip is exactly the mutation this resync
                    // exists for: `swap.equipment` is always one of the four
                    // armour slots or the off-hand.
                    apply(conn, state, join_attributes(proto, inventory)).await?;
                    // A full inventory sends the displaced equipment to the
                    // world as a dropped stack.
                    if let Some(spilled) = swap.spilled {
                        spawn_dropped_stacks(
                            mobs,
                            *player_pos,
                            *player_rot,
                            drops_rng,
                            vec![spilled],
                        );
                    }
                }
            }
        }
        ServerBound::ReleaseUseItem => {
            // A release before the consume clock expires cancels the use with
            // no food applied.
            *item_in_use = None;
            if let Some(draw) = bow_draw.take() {
                let fired = apply_release_use_item(
                    mobs,
                    inventory,
                    *player_pos,
                    *client_movement,
                    *player_rot,
                    *game_mode,
                    draw,
                    player_uuid,
                );
                // Bow shots have no exhaustion cost, so this arm charges none.
                let _ = fired;
            }
        }
        // The `F`-key hand swap. See `ServerBound::SwapItemInHand`'s own doc
        // comment for why both directives below are the only place either
        // slot's new contents ever reaches the client — there is no local
        // client prediction to correct, unlike `RenameItem`/`EditBook`'s
        // resends just above.
        ServerBound::SwapItemInHand => {
            let main_native = usize::from(inventory.selected_hotbar_slot());
            let main_item = inventory.native(main_native).cloned();
            let off_item = inventory.native(OFFHAND_NATIVE).cloned();
            inventory.set_native(main_native, off_item.clone());
            inventory.set_native(OFFHAND_NATIVE, main_item.clone());
            if let Some(menu_slot) = window_zero_menu_slot(main_native) {
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, menu_slot, off_item.as_ref()),
                )
                .await?;
            }
            if let Some(menu_slot) = window_zero_menu_slot(OFFHAND_NATIVE) {
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, menu_slot, main_item.as_ref()),
                )
                .await?;
            }
        }
        // The steering packet is an authoritative report from the client. Store
        // its position and yaw in the boat snapshot so every viewer's
        // `move_entity` diff follows.
        //
        // `apply_vehicle_move` resolves the vehicle from this player rather than
        // an id on the wire, so a connection cannot drag a boat it is not riding.
        ServerBound::VehicleMoved {
            position,
            yaw,
            pitch,
        } => {
            // Pitch is decoded and dropped because vehicle movement stores only
            // position and yaw. Keep the binding named so the decoded field is
            // visible at this call site.
            let _ = pitch;
            // A player occupies at most one vehicle map. Try the mob map only
            // when the vehicle map refused, so the shared movement packet uses
            // the appropriate mounted entity.
            mobs.with(|sim| {
                if sim
                    .apply_vehicle_move(player_entity_id, position, yaw)
                    .is_none()
                {
                    sim.apply_mob_move(player_entity_id, position, yaw);
                }
            });
        }
        // `PADDLE_BOAT` is purely cosmetic (see
        // `MobSim::apply_boat_paddle`'s own doc) so there is no directive to
        // send here; the next `snapshots()` diff carries it to every other
        // connected client via `MetadataField::BoatPaddles`.
        ServerBound::PaddleBoat { left, right } => {
            mobs.with(|sim| {
                sim.apply_boat_paddle(player_entity_id, left, right);
            });
        }
        ServerBound::PlayerInput { sprint, shift, jump } => {
            *sprinting = sprint;
            *sneaking = shift;
            // A jump request starts the camel dash when the mount accepts it.
            if jump {
                mobs.with(|sim| sim.trigger_camel_dash(player_entity_id));
            }
            // The client sends a true shift bit on the input edge. Try the
            // vehicle, minecart, and mob mounts in sequence; only one can carry
            // this player at a time.
            if shift {
                let rotation = player_rot.unwrap_or_default();
                let terrain = source.get();
                let dismounted = mobs.with(|sim| {
                    if let Some(vehicle_id) = sim.vehicle_ridden_by(player_entity_id) {
                        let position = sim.vehicle_dismount_position(
                            vehicle_id,
                            rotation.yaw,
                            &|x, y, z| terrain.block_state_id(x, y, z),
                        );
                        sim.dismount_rider(player_entity_id)
                            .map(|id| (id, position))
                    } else {
                        sim.dismount_minecart_rider(player_entity_id)
                            .or_else(|| sim.dismount_mob(player_entity_id))
                            .map(|id| (id, None))
                    }
                });
                if let Some((vehicle_id, dismount_position)) = dismounted {
                    // Send the vehicle's complete, empty passenger list.
                    apply(conn, state, proto.encode_set_passengers(vehicle_id, &[])).await?;
                    if let Some(position) = dismount_position {
                        // Apply the authoritative dismount location locally and
                        // send it before processing the next movement delta.
                        *player_pos = Some((position.x, position.y, position.z));
                        *player_rot = Some(rotation);
                        apply(
                            conn,
                            state,
                            proto.encode_teleport_with_id(
                                issue_teleport_id(teleport_acknowledgements),
                                position.x,
                                position.y,
                                position.z,
                                rotation.yaw,
                                rotation.pitch,
                            ),
                        )
                        .await?;
                    }
                }
            }
        }
        ServerBound::CreativeModeSlotSet { slot, item } => {
            apply_creative_mode_slot_set(inventory, slot, item, *game_mode == GameMode::Creative);
        }
        ServerBound::ClientCommand { action } => {
            apply_client_command(
                conn,
                proto,
                state,
                vitals,
                burn,
                fall,
                teleport_acknowledgements,
                world_spawn,
                respawn,
                source.get(),
                home.get(),
                world,
                advancements,
                player_uuid,
                action,
                commands.permission_level,
                client_loaded,
                dimension_reset,
                end_exit,
                *game_mode,
                block_ticks,
            )
            .await?;
        }
        ServerBound::ClientInformationChanged { view_distance } => {
            // **No host-side clamp here.** `ViewTracker::set_view_radius` applies
            // the server's configured ceiling, stored in `ViewTracker::max_radius`.
            // The connection's requested distance can therefore shrink or grow
            // within that ceiling during a session.
            // Read the current radius so a changed value can move the player's
            // ticket to the requested view centre.
            let radius_before_resize = view.radius;
            let update = view.set_view_radius(
                proto,
                source,
                i32::from(view_distance),
                player_rot.map(|rotation| rotation.yaw),
            );
            if view.radius != radius_before_resize {
                player_ticket_guard.move_to_with_simulation_radius(
                    view.center,
                    view.radius,
                    view.radius.clamp(0, crate::chunk_store::CONCURRENT_TICK_RADIUS),
                );
                source.get().reconcile_ticket_residency();
            }
            send_view_update(
                conn,
                proto,
                source,
                join_stream.as_deref_mut(),
                state,
                view,
                update,
                awaiting_chunk_batch_ack,
                pending_chunk_batches,
            )
            .await?;
        }
        ServerBound::ChunkBatchAcknowledged { .. } => {
            *awaiting_chunk_batch_ack = false;
            if let Some(next) = pending_chunk_batches.pop_front() {
                *awaiting_chunk_batch_ack = true;
                send_pending_chunk_batch(conn, proto, state, view, next).await?;
            }
        }
        // Chat commands run through the built-in tree, with host dispatch as
        // the fallback. Effects for this connection are applied inline; effects
        // targeting another player enter that player's effect queue. The
        // resolved permission level gates both command execution and completion,
        // while an absent host dispatcher fails closed.
        ServerBound::ChatCommand { command } => {
            // Captured before the `source` binding below shadows the chunk
            // source with the command's own `CommandSource` — `Effect::SetBlock`/
            // `Fill` need the former and nothing else in this arm has a name for
            // it once the shadow takes effect.
            let chunk_source = source;
            // The connection may already be in the Nether or End.  Keep that
            // live source dimension in the command stack so `/execute ... run`
            // passes the actual context on to a host dispatcher rather than
            // silently manufacturing an overworld context.
            let command_dimension = chunk_source
                .dimension()
                .key()
                .parse()
                .expect("server dimensions always have valid resource keys");
            // The roster the command's selectors resolve against.
            //
            // With no registry — singleplayer, where `open_in_memory` builds no
            // `PlayerRegistry` at all — the caller is synthesised as the sole
            // candidate. That is not a courtesy: without it `@s` resolves to
            // nothing and `/gamemode creative` fails in single-player, which is
            // the single most common use of the command.
            let mut candidates = players.map(PlayerRegistry::candidates).unwrap_or_default();
            let position = player_pos
                .map_or(world_spawn, |(x, y, z)| Vec3::new(x, y, z));
            if !candidates.iter().any(|c| c.uuid == player_uuid) {
                candidates.push(crate::commands::PlayerCandidate {
                    uuid: player_uuid,
                    entity_id: player_entity_id,
                    username: username.to_owned(),
                    position,
                    rotation: player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 }),
                    game_mode: *game_mode,
                    // No registry to have republished into — this connection
                    // *is* the one live source, read directly rather than
                    // through the mirror `set_experience` maintains for
                    // everyone else's roster entry.
                    xp_level: experience.level(),
                    xp_points: experience.query_points(),
                });
            }
            let respawn_dimension = source.dimension();
            let source = crate::commands::CommandSource::player(
                player_uuid,
                player_entity_id,
                username,
                position,
                player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 }),
                command_dimension,
                commands.permission_level,
            );
            let command_world = crate::commands::CommandWorld {
                rules: world,
                players: &candidates,
                state: world,
                // `/summon`'s synchronous spawn entry point — the same shared
                // `MobHandle` `dispatch_play_packet` already holds, so a
                // spawned mob is picked up by the tick loop's own next
                // publish (see `crate::commands::summon`'s module doc).
                mobs: Some(mobs),
                // `/worldborder`'s read/write surface — the same
                // shared `BorderFeed` this connection already holds.
                border: Some(border),
                // No access list is attached to packet dispatch, so access
                // management commands return the fail-closed refusal. RCON
                // supplies the access handle when those commands are needed.
                #[cfg(not(target_arch = "wasm32"))]
                access: None,
                // `/execute if`/`unless block`'s read-only surface — the same
                // `chunk_source` captured above `Effect::SetBlock`/`Fill`
                // already reach through this arm's own `apply_own_effect`.
                blocks: Some(chunk_source.get()),
            };
            match commands.builtins.run_with_contextual_dispatch(
                &command_world,
                &source,
                &command,
                &commands.dispatch,
                &commands.caller,
            ) {
                Some(outcome) => {
                    // Command effects are still one ordered packet action, but
                    // their block coordinates are resolved only after the
                    // command tree runs. Admit every owned target and its
                    // light neighbours before applying the effects so a
                    // `/setblock` or `/fill` cannot re-enter cold terrain on
                    // the connection task.
                    let mut effect_columns = HashSet::new();
                    for directed in &outcome.effects {
                        if directed.target != player_uuid {
                            continue;
                        }
                        let mut admit = |x: i32, z: i32| {
                            effect_columns.extend(column_admission_footprint(
                                x.div_euclid(16),
                                z.div_euclid(16),
                                1,
                            ));
                        };
                        match &directed.effect {
                            crate::commands::Effect::SetBlock { pos: (x, _y, z), .. } => {
                                admit(*x, *z);
                            }
                            crate::commands::Effect::Fill { positions, .. } => {
                                for &(x, _y, z) in positions {
                                    admit(x, z);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !effect_columns.is_empty() {
                        chunk_source
                            .admit_columns(effect_columns.into_iter().collect())
                            .await?;
                    }
                    for directed in outcome.effects {
                        if directed.target != player_uuid {
                            if let Some(registry) = players {
                                registry.push_effect(directed.target, directed.effect);
                            }
                            continue;
                        }
                        // World/broadcast effects are always self-targeted for
                        // delivery only (see `crate::commands::Effect`'s own doc)
                        // and applied here, inline, because this is the only
                        // place with `chunk_source`/`block_ticks`/the player
                        // registry/`respawn` all in scope. Everything else is a
                        // genuine per-player effect and goes through
                        // `apply_own_effect`.
                        match directed.effect {
                            crate::commands::Effect::SetBlock { pos: (x, y, z), block } => {
                                chunk_source.get().set_block(x, y, z, block);
                                block_ticks.publish(x, y, z, block);
                            }
                            crate::commands::Effect::Fill { positions, block } => {
                                for (x, y, z) in positions {
                                    chunk_source.get().set_block(x, y, z, block);
                                    block_ticks.publish(x, y, z, block);
                                }
                            }
                            crate::commands::Effect::Broadcast { sender, message } => {
                                if let Some(registry) = players {
                                    registry.say(&sender, &message);
                                } else {
                                    // Singleplayer builds no registry at all —
                                    // the same fallback the `@s`-synthesis above
                                    // uses. Rendered identically to
                                    // `ChatLine::rendered` so a `/say` reads no
                                    // differently than ordinary chat would.
                                    apply(
                                        conn,
                                        state,
                                        proto.encode_system_chat(&format!("<{sender}> {message}")),
                                    )
                                    .await?;
                                }
                            }
                            crate::commands::Effect::SetRespawnPoint { pos } => {
                                *respawn = Some(RespawnPoint::forced(
                                    pos,
                                    respawn_dimension,
                                    0.0,
                                    0.0,
                                ));
                            }
                            other => {
                                apply_own_effect(
                                    conn,
                                    proto,
                                    state,
                                    game_mode,
                                    abilities,
                                    inventory,
                                    players,
                                    player_uuid,
                                    other,
                                    advancements,
                                    world,
                                    effects,
                                    vitals,
                                    experience,
                                    player_entity_id,
                                    username,
                                    player_pos,
                                    player_rot,
                                    teleport_acknowledgements,
                                )
                                .await?;
                            }
                        }
                    }
                    for line in outcome.response.chat_lines() {
                        apply(conn, state, proto.encode_system_chat_component(&line)).await?;
                    }
                }
                // No built-in root matched: delegate the command to the host
                // dispatcher.
                None => {
                    let response = if commands.dispatch.is_installed() {
                        let caller = commands.plugin_caller();
                        commands.dispatch.run(&caller, &command)
                    } else {
                        commands.dispatch.run(&commands.caller, &command)
                    };
                    for line in response.chat_lines() {
                        apply(conn, state, proto.encode_system_chat_component(&line)).await?;
                    }
                }
            }
        }
        // A tab-completion request. See `ServerBound::CommandSuggestion`'s own
        // doc comment for the wire shape and
        // `crate::commands::ServerCommands::suggest_response` for the
        // start/length arithmetic and the `/`-stripping this delegates to it,
        // gated by `commands.permission_level` — the same resolved-once level
        // `ChatCommand` above uses. A host registry is consulted only when the
        // built-in tree has no visible candidate, preserving root precedence
        // while making plugin completions reach the same production path as
        // plugin execution.
        ServerBound::CommandSuggestion { id, command } => {
            let mut response =
                commands.builtins.suggest_response(id, &command, commands.permission_level);
            // Built-ins retain precedence. If they have no visible completion,
            // ask the host's permission-aware registry for the plugin roots and
            // branches that the same authenticated caller may use. The helper
            // above already computed the wire token range, so plugin results
            // cannot disagree with client replacement offsets.
            if response.suggestions.is_empty() && commands.dispatch.is_installed() {
                let caller = commands.plugin_caller();
                response.suggestions = commands
                    .dispatch
                    .suggest(&caller, &command)
                    .into_iter()
                    .map(|text| CommandSuggestionEntry { text, tooltip: None })
                    .collect();
            }
            apply(conn, state, proto.encode_command_suggestions(&response)).await?;
        }
        // A game-mode request is answered with directives for the mode the
        // server accepted. Permission level 2 is required for the change.
        ServerBound::ChangeGameMode { mode } => {
            if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                *game_mode = mode;
            }
            for directive in game_mode_directives(proto, *game_mode, abilities) {
                apply(conn, state, directive).await?;
            }
        }
        // Spectator teleport resolves connected players only, preserves the
        // requester's facing, and ignores requests outside spectator mode or
        // without a matching player. The resulting position uses the normal
        // teleport effect path.
        ServerBound::TeleportToEntity { uuid } => {
            if *game_mode == GameMode::Spectator
                && let Some(target) = players
                    .map(PlayerRegistry::candidates)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|c| c.uuid == uuid)
            {
                let current = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
                *player_pos = Some((target.position.x, target.position.y, target.position.z));
                *player_rot = Some(current);
                let directive = proto.encode_teleport_with_id(
                    issue_teleport_id(teleport_acknowledgements),
                    target.position.x,
                    target.position.y,
                    target.position.z,
                    current.yaw,
                    current.pitch,
                );
                apply(conn, state, directive).await?;
            }
        }
        // An arm swing. See `ServerBound::Swing`'s own doc comment for why
        // this pushes to the shared broadcast log rather than replying
        // directly (same "every connection reads it on its own drain" shape
        // as `Chat` below), and for why the log's own reader excludes the
        // sender. Singleplayer has no registry and therefore nobody else to
        // tell, so this is silently a no-op there.
        ServerBound::Swing { hand } => {
            if let Some(registry) = players {
                registry.swing(player_entity_id, hand);
            }
        }
        // A spectator can attach its camera to a nearby entity when
        // `apply_spectator_action` accepts the target. Invalid or out-of-range
        // requests are ignored and produce no failure reply.
        ServerBound::SpectatorAction { target_entity_id } => {
            if let Some(target_id) =
                apply_spectator_action(*game_mode, target_entity_id, *player_pos, mobs, players)
            {
                apply(conn, state, proto.encode_set_camera(target_id)).await?;
            }
        }
        // Chat is placed in the shared broadcast queue; each connection drains
        // that queue, including the sender, on its normal outgoing pass.
        //
        // Empty messages are malformed and are dropped rather than broadcast.
        //
        // `crate::chat_session::decide` verifies the message before broadcast.
        // A rejection is sent to the sender and never enters `outgoing_chat`.
        ServerBound::Chat {
            message,
            timestamp_millis,
            salt,
            signature,
        } => {
            if !message.trim().is_empty() {
                let decision = crate::chat_session::decide(
                    chat_session,
                    player_uuid,
                    enforce_secure_profile,
                    signature.as_ref().map(|s| s.as_slice()),
                    &message,
                    timestamp_millis,
                    salt,
                    crate::chat_session::now_millis(),
                );
                match decision {
                    crate::chat_session::ChatDecision::Accept { .. } => {
                        outgoing_chat.push(message);
                    }
                    crate::chat_session::ChatDecision::Reject { reason } => {
                        apply(
                            conn,
                            state,
                            proto.encode_system_chat(&format!("Your message was not sent: {reason}")),
                        )
                        .await?;
                    }
                }
            }
        }
        // A session announcement replaces the connection's session; verification
        // reads the session data supplied by that announcement.
        ServerBound::ChatSessionAnnounced {
            session_id,
            expires_at_millis,
            public_key,
            key_signature,
        } => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let data = lodestone_auth::ProfilePublicKeyData {
                    // `player_uuid` is the identity the session server returned
                    // after `hasJoined`, never the UUID the client claimed in
                    // LoginStart. Mojang signs this exact UUID into the
                    // certificate payload.
                    profile_id: player_uuid,
                    expires_at_millis,
                    public_key_der: public_key,
                    key_signature,
                };
                if let Some(session) = crate::chat_session::adopt_announced_session(
                    profile_key_issuers,
                    player_uuid,
                    session_id,
                    data,
                ) {
                    if chat_session
                        .as_ref()
                        .is_some_and(|current| session.expires_before(current))
                    {
                        let directive = proto.encode_disconnect(
                            *state,
                            &expired_profile_public_key_reason(),
                        );
                        apply(conn, state, directive).await?;
                        return Err(ServerError::ProfilePublicKeyRollback);
                    }
                    *chat_session = Some(session);
                } else if profile_key_issuers.is_some() {
                    // With an issuer set, an invalid update clears the active
                    // session. Without one, retain the active session.
                    *chat_session = None;
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                // Browser integrated play has no online-authentication or
                // Mojang issuer service. Match the unavailable-service native
                // path: ignore the untrusted announcement and retain the
                // existing session rather than installing a self-asserted key.
                let _ = (session_id, expires_at_millis, public_key, key_signature);
            }
        }
        // Plugin register/unregister channels update this connection's supported
        // set; other channels go to their registered handler or are dropped.
        ServerBound::CustomPayload { channel, data } => {
            if !client_channels.apply_custom_payload(&channel, &data) {
                plugin_channels.dispatch(&channel, &data);
            }
        }
        // `PlayerCommand` action 0 is `STOP_SLEEPING` — the "wake
        // up" a client sends when the player climbs out of bed or dies. It is
        // the only ordinal the version crates surface (the others decode to
        // `Ignored`; see `ServerBound::PlayerCommand`'s own doc comment), and
        // the packet carries no player identity — the `get_up` roster key is
        // this connection's own `player_entity_id`, resolved once in
        // `serve_play` (see `crate::sleep::SleepVote` for why the wire cannot
        // supply it).
        ServerBound::PlayerCommand { action } => {
            if action == 0 {
                sleep_vote.get_up(player_entity_id);
            }
        }
        // `ServerboundPingRequestPacket` shares one wire struct across Status
        // and Play (see the decode arm's own comment), so `PingRequest` reaches
        // here too, unlike its `Handshake`/`LoginStart`/etc. siblings below.
        // Vanilla's own ping-request handler is exactly "echo the
        // time back" — the same body the Status-state arm above uses, minus the
        // connection close, since a Play-state ping must not end the session.
        ServerBound::PingRequest { time } => {
            apply(conn, state, proto.encode_pong_response(time)).await?;
        }
        // `Pong` is the reply to the server-originated `ping` control packet.
        // The hosted protocol has no ping producer or pending-id state, so a
        // valid reply deliberately produces no packet or state mutation.
        // Keeping it distinct from `Ignored` makes that accepted no-op
        // boundary explicit without inventing acknowledgement bookkeeping.
        ServerBound::Pong { id } => {
            let _ = id;
        }
        // Middle-click selection uses `crate::item_use::try_pick_item` for the
        // inventory destination and slot rules. This arm resolves the clicked
        // block's clone stack and checks interaction range and live block state.
        // `include_data` is ignored because this crate has no consumer that
        // copies block-entity data onto the selected item.
        ServerBound::PickItemFromBlock { pos, include_data: _ } => {
            let feet = player_pos.map(|(x, y, z)| Vec3::new(x, y, z));
            if crate::block_breaking::within_interaction_range(feet, pos) {
                let block_state = source.get().block_state_id(pos.x, pos.y, pos.z);
                if let Some(stack) = crate::item_use::clone_item_stack_for_block(block_state) {
                    let creative = *game_mode == GameMode::Creative;
                    let outcome = crate::item_use::try_pick_item(inventory, stack, creative);
                    apply(conn, state, proto.encode_set_held_slot(outcome.selected)).await?;
                    for native in outcome.changed {
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        // The entity-pick request uses the same split, aimed at the entity's
        // derived item result instead of a block's clone stack. Only the
        // `Mob` override (a spawn egg) is modelled; see
        // `crate::item_use::spawn_egg_for_entity_type`'s doc comment for the
        // entities this refuses. `include_data` also gates a game-master
        // avatar-profile debug command in vanilla (`FetchProfileCommand`),
        // which this crate has no command channel for, so it is unread here
        // too.
        ServerBound::PickItemFromEntity { entity_id, include_data: _ } => {
            let target = mobs.with(|sim| {
                sim.get(entity_id).map(|mob| (mob.entity_type().to_string(), mob.position()))
            });
            if let Some((entity_type, entity_pos)) = target {
                let feet = player_pos.map(|(x, y, z)| Vec3::new(x, y, z));
                if crate::item_use::within_entity_pick_range(feet, entity_pos)
                    && let Some(stack) = crate::item_use::spawn_egg_for_entity_type(&entity_type)
                {
                    let creative = *game_mode == GameMode::Creative;
                    let outcome = crate::item_use::try_pick_item(inventory, stack, creative);
                    apply(conn, state, proto.encode_set_held_slot(outcome.selected)).await?;
                    for native in outcome.changed {
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        // The pre-Play phase signals, unreachable here by construction: a
        // connection in `State::Play` cannot decode a handshake, a login, or
        // a Status-phase status request, because every `ServerProtocol::decode`
        // arm for those is gated on the state.
        ServerBound::Handshake { .. }
        | ServerBound::LoginStart { .. }
        // `EncryptionResponse` is `State::Login`-only too, same
        // as `LoginStart`/`LoginAcknowledged` beside it.
        | ServerBound::EncryptionResponse { .. }
        | ServerBound::LoginAcknowledged
        | ServerBound::ConfigurationFinished
        | ServerBound::StatusRequest
        | ServerBound::TeleportationAccepted { .. }
        | ServerBound::Ignored => {}
    }
    Ok(())
}
