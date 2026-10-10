//! Applying status effects to the connection's own player: saturation, air supply, max-health sync and the entity-flag republish.

use super::*;

/// Mirrors effect-driven base-entity flags into the shared player registry
/// before a stream pass. The registry keeps only remote-visible player state;
/// the connection-local effect set remains the timer and gameplay authority.
pub(super) fn republish_effect_entity_flags(
    players: Option<&PlayerRegistry>,
    ticket: Option<&PlayerTicket>,
    effects: &crate::mob_effects::ActiveEffects,
) {
    let Some((registry, ticket)) = players.zip(ticket) else {
        return;
    };
    let invisible = effects.amplifier_of("minecraft:invisibility").is_some();
    registry.set_shared_flags(ticket.entity_id(), if invisible { 0x20 } else { 0 });
}

/// Runs the player air rule with the same active-effect store that drives the
/// connection's status-effect packets and gameplay consumers.
///
/// Keeping this at the server boundary means [`PlayerVitals`] stays a pure
/// value type: terrain decides submersion and the effect store decides only
/// the hold/refill mode. Both native and browser timer loops call this helper.
pub(super) fn tick_player_air_supply(
    vitals: &mut PlayerVitals,
    eye_in_water: bool,
    invulnerable: bool,
    effects: &crate::mob_effects::ActiveEffects,
) -> crate::vitals::VitalsTick {
    vitals.tick_with_underwater_breathing(
        eye_in_water && !invulnerable,
        effects.underwater_breathing(),
    )
}

/// Applies the instant-Saturation part of an effect tick through the
/// authoritative food state. Both timer loops use this seam before deciding
/// whether a new `SetHealth` packet is needed.
pub(super) fn apply_effect_saturation(vitals: &mut PlayerVitals, food_points: u32) -> bool {
    food_points > 0
        && vitals.apply_saturation_effect(
            i32::try_from(food_points).unwrap_or(i32::MAX),
        )
}

/// Applies one command [`Effect`](crate::Effect) to **this** connection.
///
/// The counterpart to `PlayerRegistry::push_effect`: an effect aimed at the
/// caller's own connection never goes through the registry at all, because
/// everything it needs — `game_mode`, `inventory`, `proto`, `conn` — is right
/// here and nothing else can reach it. The two paths are the reason
/// [`crate::Effect`] exists; see its module doc.
///
/// The `SetGameMode` arm also republishes to the registry, so another
/// connection's `@a[gamemode=creative]` reads the truth. Forgetting that
/// republish is silent: this connection behaves correctly and every *other*
/// connection's selector is wrong.
#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_own_effect<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    game_mode: &mut GameMode,
    abilities: &mut Abilities,
    inventory: &mut PlayerInventory,
    players: Option<&PlayerRegistry>,
    player_uuid: uuid::Uuid,
    effect: crate::commands::Effect,
    // `/give` is a `minecraft:inventory_changed` producer, so this arm
    // grants criteria exactly as a floor pickup does — see the `GiveItems` arm.
    advancements: &mut AdvancementManager,
    // For the world-clock timestamp the grant is stamped with, which must be
    // tick-derived rather than `Instant::now()` (this crate links into wasm32).
    world: &crate::world_state::WorldStateHandle,
    // This player's live status effects — the store `/effect give` and
    // `/effect clear` write through.
    effects: &mut crate::mob_effects::ActiveEffects,
    // `/kill`'s health write and the `publish_health` death sequence it
    // triggers.
    vitals: &mut PlayerVitals,
    // `/xp`'s read/write surface.
    experience: &mut crate::experience::PlayerExperience,
    // `publish_health`'s own parameters, for the `Kill` arm — see that
    // function's doc for why they are not derivable from anything else
    // already passed here.
    player_entity_id: i32,
    username: &str,
    // `/tp`'s `Teleport` arm. This connection's own tracked position/rotation
    // — read to preserve facing when the effect carries no `yaw`/`pitch`
    // (`Effect::Teleport`'s own doc explains why that resolution can only
    // happen here, at application time, never at the executor that produced
    // the effect), and written so this connection's own `player_pos`/
    // `player_rot` agree with the teleport it just sent — the same
    // `player_pos`/`player_rot` `dispatch_play_packet`'s movement arms keep in
    // sync, so a later relative move is computed from the post-teleport
    // position rather than a stale pre-teleport one.
    player_pos: &mut Option<(f64, f64, f64)>,
    player_rot: &mut Option<Rotation>,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    match effect {
        crate::commands::Effect::SetGameMode(mode) => {
            *game_mode = mode;
            if let Some(registry) = players {
                registry.set_game_mode(player_uuid, mode);
            }
            for directive in game_mode_directives(proto, mode, abilities) {
                apply(conn, state, directive).await?;
            }
            // The tab-list entry's own game mode (`UPDATE_GAME_MODE`, action
            // ordinal 2). Without it the player's mode changes and every client's
            // tab list keeps reporting the mode they joined in — including their
            // own, which is what makes a spectator still show as survival there.
            for directive in proto.encode_player_info_game_mode(&[(player_uuid, mode)]) {
                apply(conn, state, directive).await?;
            }
        }
        crate::commands::Effect::GiveItems(stacks) => {
            for stack in stacks {
                // The second `minecraft:inventory_changed` producer, and the one a
                // player can reach deliberately: `/give @s crafting_table` must grant
                // `story/root` exactly as picking one off the floor does. Vanilla's
                // criterion is about *having* the item, not about how it arrived,
                // which is precisely why the trigger lives at the inventory seam.
                let given_id = stack.item.to_string();
                let (written, leftover) = inventory.add(stack);
                if leftover.is_none() || !written.is_empty() {
                    advancements.on_inventory_changed(
                        player_uuid,
                        &given_id,
                        world.time().game_time.saturating_mul(50),
                    );
                }
                for native in written {
                    // Window `0`, `state_id` `0` — matching every other
                    // server-initiated slot write in this file.
                    if let Some(menu_slot) = window_zero_menu_slot(native) {
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                        )
                        .await?;
                    }
                }
                if leftover.is_some() {
                    // Unfitted items would normally become an item entity. This
                    // crate has no command-spawned drop path, so the surplus is reported rather
                    // than silently discarded — the player is told, which is
                    // strictly better than an item vanishing.
                    apply(
                        conn,
                        state,
                        proto.encode_system_chat("Your inventory was full — some items were not given"),
                    )
                    .await?;
                }
            }
        }
        crate::commands::Effect::ApplyEffect {
            effect,
            duration,
            amplifier,
        } => {
            // Apply the complete stacking rule, including the hidden-effect
            // chain, so a second application of the same effect behaves
            // correctly rather than overwriting.
            //
            // Read whether the effect is present before calling `apply`; this
            // distinguishes a fresh instance from a refreshed one and supplies
            // the encoded packet's `blend` flag (see
            // `ServerProtocol::encode_update_mob_effect`'s own doc). Without this
            // arm, `/effect give` changed real server state — movement speed,
            // damage taken, hunger drain — with zero client feedback: no icon, no
            // particles, no screen tint.
            let already_present = effects.get(&effect).is_some();
            if effects.apply(&effect, duration, amplifier)
                && let Some(instance) = effects.get(&effect)
            {
                apply(
                    conn,
                    state,
                    proto.encode_update_mob_effect(
                        player_entity_id,
                        &effect,
                        instance.amplifier(),
                        instance.duration(),
                        false,
                        true,
                        true,
                        !already_present,
                    ),
                )
                .await?;
            }
        }
        crate::commands::Effect::ClearEffects { effect } => {
            // The counterpart to `ApplyEffect` above — the single-effect
            // removal and all-effects removal paths each
            // send remove-mob-effect packet per cleared effect, so
            // `/effect clear` must tell the client which icons to drop rather
            // than leaving them stuck on screen.
            match effect {
                Some(id) => {
                    if effects.remove(&id) {
                        apply(conn, state, proto.encode_remove_mob_effect(player_entity_id, &id)).await?;
                    }
                }
                None => {
                    let cleared: Vec<String> =
                        effects.active().into_iter().map(|(id, _)| id.to_owned()).collect();
                    effects.clear();
                    for id in cleared {
                        apply(conn, state, proto.encode_remove_mob_effect(player_entity_id, &id)).await?;
                    }
                }
            }
        }
        crate::commands::Effect::Message(line) => {
            apply(conn, state, proto.encode_system_chat(&line)).await?;
        }
        crate::commands::Effect::Kill => {
            // Kill sets health directly to zero without armour or defenses.
            vitals.kill();
            publish_health(
                conn,
                state,
                proto,
                vitals,
                effects,
                // No sound fires for this call (`hurt` below is
                // `None`, and `publish_health` only plays one on a landed hit),
                // but a position is still owed to the parameter.
                player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                player_entity_id,
                username,
                crate::vitals::DeathCause::GenericKill,
                advancements,
                player_uuid,
                None,
            )
            .await?;
        }
        crate::commands::Effect::GiveExperience { levels, amount } => {
            if levels {
                // `take_levels` is a level *subtraction*; negating the delta is
                // exactly `giveExperienceLevels`'s own addition.
                experience.take_levels(-amount);
            } else {
                experience.give_points(amount);
            }
            republish_experience(players, player_uuid, experience);
            apply(
                conn,
                state,
                proto.encode_set_experience(experience.progress(), experience.level(), experience.total()),
            )
            .await?;
        }
        crate::commands::Effect::SetExperience { levels, amount } => {
            // Zeroed first — see `crate::commands::experience`'s module doc for
            // why this is an approximation of vanilla's absolute setters rather
            // than a byte-exact port of them.
            *experience = crate::experience::PlayerExperience::default();
            if levels {
                experience.take_levels(-amount);
            } else {
                experience.give_points(amount);
            }
            republish_experience(players, player_uuid, experience);
            apply(
                conn,
                state,
                proto.encode_set_experience(experience.progress(), experience.level(), experience.total()),
            )
            .await?;
        }
        crate::commands::Effect::ClearInventory { item, max_count } => {
            let mut remaining = max_count.map(|n| u32::try_from(n).unwrap_or(0));
            let mut cleared: u32 = 0;
            for index in 0..crate::inventory::PLAYER_NATIVE_SIZE {
                if matches!(remaining, Some(0)) {
                    break;
                }
                let Some(stack) = inventory.native(index) else { continue };
                if let Some(filter) = &item {
                    if &stack.item.to_string() != filter {
                        continue;
                    }
                }
                let count = stack.count;
                let take = remaining.map_or(count, |cap| count.min(cap));
                if take == 0 {
                    continue;
                }
                if take >= count {
                    inventory.set_native(index, None);
                } else {
                    let mut left = stack.clone();
                    left.count -= take;
                    inventory.set_native(index, Some(left));
                }
                cleared += take;
                if let Some(cap) = remaining.as_mut() {
                    *cap -= take;
                }
                if let Some(menu_slot) = crate::inventory::window_zero_menu_slot(index) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, inventory.native(index)),
                    )
                    .await?;
                }
            }
            if cleared == 0 {
                apply(conn, state, proto.encode_system_chat("No items were found on the player")).await?;
            }
        }
        // World/broadcast/connection-local effects. Always self-targeted by the
        // executors that produce them (see `crate::commands::Effect`'s own doc)
        // and applied inline by the `ChatCommand` arm *before* it reaches this
        // function — that arm has `chunk_source`/`block_ticks`/the player
        // registry/`respawn`, none of which this function receives. A directed
        // effect of this kind reaching a *different* connection's drain would be
        // a registration bug in whichever executor produced it (every one of
        // them resolves `ctx.source.uuid()`, never a selector target); no-op
        // rather than panic, because a connection task must not go down for it.
        crate::commands::Effect::SetBlock { .. }
        | crate::commands::Effect::Fill { .. }
        | crate::commands::Effect::Broadcast { .. }
        | crate::commands::Effect::SetRespawnPoint { .. } => {}
        // `/tp`/`/teleport`. Unlike the world/broadcast effects above, this one
        // genuinely reaches any connected player, so it is an ordinary
        // per-uuid effect applied right here — for the caller inline, for a
        // directed target by that target's own connection loop. A missing
        // `yaw`/`pitch` means "keep this connection's current facing", which
        // is exactly `player_rot`'s own last-known value; a connection with no
        // facing on record yet (never sent one since join) falls back to
        // `0.0`/`0.0`, matching the join sequence's own default.
        crate::commands::Effect::Teleport { x, y, z, yaw, pitch } => {
            let current = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
            let yaw = yaw.unwrap_or(current.yaw);
            let pitch = pitch.unwrap_or(current.pitch);
            *player_pos = Some((x, y, z));
            *player_rot = Some(Rotation { yaw, pitch });
            let teleport_id = issue_teleport_id(teleport_acknowledgements);
            apply(
                conn,
                state,
                proto.encode_teleport_with_id(teleport_id, x, y, z, yaw, pitch),
            )
            .await?;
        }
    }
    // A command can add, replace, restore, or clear Health Boost. Publish the
    // folded attribute in this same command turn, rather than waiting for the
    // periodic status tick and briefly leaving the client's heart capacity
    // stale.
    sync_effect_max_health(conn, state, proto, vitals, effects).await?;
    Ok(())
}

/// Publishes the one effect-derived attribute that changes the authoritative
/// health ceiling, plus the current-health packet that must be clamped when an
/// expiring effect lowers that ceiling.
///
/// Calling this after effect application and after the timer's expiry pass
/// makes add, amplifier replacement, hidden-chain restoration, and removal
/// share one transition. A no-op effect tick produces no packets.
pub(super) async fn sync_effect_max_health<T, P>(
    conn: &mut Connection<T>,
    state: &mut State,
    proto: &P,
    vitals: &mut PlayerVitals,
    effects: &crate::mob_effects::ActiveEffects,
) -> Result<bool, ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if !vitals.set_max_health(effects.max_health()) {
        return Ok(false);
    }
    let snapshot = max_health_snapshot(vitals.max_health());
    apply(conn, state, proto.encode_update_attributes(std::slice::from_ref(&snapshot))).await?;
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
    Ok(true)
}
