//! The per-player gameplay tick one connection runs every 50 ms vitals pass, native and browser alike:
//! deferred dig completion, brush strokes, item-use completion, border, drowning, melee hits and
//! shield, burning, beacons, omens, queued mob effects, dragon deaths, status effects and hunger;
//! and the shared feeds a connection drains on its container-sync pass (chat, swings, sleep skips,
//! resource packs, plugin channels, effects aimed at it).

use super::*;

/// Everything one pass reads or mutates. The play loop owns the locals; this borrows them so the
/// pass is a single function.
pub(super) struct PlayerTick<'a, T, P, S>
where
    T: Transport,
    S: ChunkSource + 'static,
{
    pub conn: &'a mut Connection<T>,
    pub proto: &'a P,
    pub source: SourceRef<'a, S>,
    pub home: SourceRef<'a, S>,
    pub state: &'a mut State,
    pub world: &'a crate::world_state::WorldStateHandle,
    pub border: &'a BorderFeed,
    pub game_mode: GameMode,
    pub player_uuid: uuid::Uuid,
    pub username: &'a str,
    pub player_pos: Option<(f64, f64, f64)>,
    pub player_rot: Option<Rotation>,
    pub player_environment: &'a mut crate::player_environment::PlayerEnvironment,
    pub sprinting: bool,
    pub sneaking: bool,
    pub flying: bool,
    pub vitals: &'a mut PlayerVitals,
    pub inventory: &'a mut PlayerInventory,
    pub advancements: &'a mut AdvancementManager,
    pub drops_rng: &'a mut SpawnRng,
    pub burn: &'a mut crate::burning::BurnState,
    pub burn_rng: &'a mut SpawnRng,
    pub effects: &'a mut crate::mob_effects::ActiveEffects,
    pub item_in_use: &'a mut Option<ItemInUse>,
    pub shield: &'a mut crate::shield::ShieldState,
    pub raid_omen_position: &'a mut Option<BlockPos>,
    pub mobs: &'a MobHandle,
    pub block_ticks: &'a BlockTickFeed,
    pub block_entities: &'a BlockEntityHandle,
    pub pending_break: &'a mut Option<PendingBreak>,
    /// The loop's tick count on its own clock, the one a dig's start was stamped with.
    pub dig_tick: u64,
    pub open_container: &'a mut Option<OpenContainer>,
    pub container_sync: &'a mut ContainerSync,
    pub relights: Option<&'a mut PendingRelights>,
}

/// Runs one pass of the per-player tick.
pub(super) async fn run<T, P, S>(tick: PlayerTick<'_, T, P, S>) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let PlayerTick {
        conn,
        proto,
        source,
        home,
        state,
        world,
        border,
        game_mode,
        player_uuid,
        username,
        player_pos,
        player_rot,
        player_environment,
        sprinting,
        sneaking,
        flying,
        vitals,
        inventory,
        advancements,
        drops_rng,
        burn,
        burn_rng,
        effects,
        item_in_use,
        shield,
        raid_omen_position,
        mobs,
        block_ticks,
        block_entities,
        pending_break,
        dig_tick,
        open_container,
        container_sync,
        relights,
    } = tick;

    // A hold-and-release dig whose stop packet left progress below the break threshold completes
    // here. A block another update already removed is skipped, since breaking air would emit
    // duplicate drops; a column that is not resident keeps the dig pending for the next pass
    // rather than turning a timer probe into synchronous generation.
    if let Some(dig) = (*pending_break).filter(|dig| dig.deferred_break_ready(Some(dig_tick)))
        && let Some(current) = resident_block_state(source.get(), dig.pos.x, dig.pos.y, dig.pos.z)
    {
        *pending_break = None;
        if current.block() != Block::Air {
            let allowed = adjudicate_block_break(conn, proto, source.get(), state, world, dig.pos, player_uuid).await?;
            if allowed {
                let creative = matches!(game_mode, GameMode::Creative);
                destroy_block(
                    conn,
                    proto,
                    source.get(),
                    state,
                    relights,
                    block_entities,
                    open_container,
                    container_sync,
                    mobs,
                    drops_rng,
                    inventory.selected_item(),
                    block_ticks,
                    player_uuid,
                    !creative && world.block_drops(),
                    world.block_drops(),
                    dig.pos,
                    advancements,
                    (!creative).then_some(&mut *vitals),
                    creative,
                )
                .await?;
            }
        }
    }

    // A held brush strokes on its own clock the same way, and a tick
    // whose ray finds no block ends the use.
    if let Some(started) = item_in_use.clone()
        && started.item == brushing::BRUSH_ITEM
    {
        let now = mobs.with(|sim| sim.tick_count());
        let remaining = started.finish_tick.saturating_sub(now);
        let from = started.last_effect_remaining.map_or(0, |r| brushing::USE_TICKS.saturating_sub(u64::from(r)));
        let to = brushing::USE_TICKS.saturating_sub(remaining);
        let held_ok = brushing::is_brush(inventory.native(started.native));
        if !held_ok {
            *item_in_use = None;
        } else if to > from
            && let (Some((x, y, z)), Some(rotation)) = (player_pos, player_rot)
        {
            if let Some(live) = item_in_use.as_mut() {
                live.last_effect_remaining = Some(u32::try_from(remaining).unwrap_or(u32::MAX));
            }
            let brush_world = brushing::BrushWorld {
                source: source.get(),
                block_entities,
                block_ticks,
                mobs,
                tables: crate::block_drops::bundled_tables(),
                game_time: u64::try_from(world.time().game_time).unwrap_or(0),
                creative: game_mode == GameMode::Creative,
            };
            let aim = brushing::Aim { feet: Vec3::new(x, y, z), yaw: rotation.yaw, pitch: rotation.pitch };
            let result = brushing::tick(&brush_world, &aim, drops_rng, from, to);
            if result.release {
                *item_in_use = None;
            }
            if result.wear > 0 && game_mode != GameMode::Creative {
                wear_item_in_slot(conn, state, proto, inventory, started.native, result.wear).await?;
            }
        }
    }

    // Vanilla's own update-using-item routine: a consume ends on the server's
    // own clock, not on a packet — the client sends nothing when a
    // steak finishes, so without this arm every bite starts and none
    // ever lands. Read against `MobSim`'s tick counter because that is
    // the clock `apply_use_item` stamped `finish_tick` from; mixing it
    // with the play loop's wall-clock tick count would compare two
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
            *item_in_use = None;
            if let Some((native, remainder)) =
                finish_consuming(inventory, vitals, &started, game_mode)
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
                // The food bar frame carries health, food, and
                // saturation together, so resend all three values
                // whenever one of them changes.
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
                        proto.encode_remove_mob_effect(
                            LOCAL_PLAYER_ENTITY_ID,
                            "minecraft:poison",
                        ),
                    )
                    .await?;
                }
            } else if let Some((native, remainder)) = finish_drinking_ominous_bottle(
                inventory,
                effects,
                &started,
                game_mode,
            ) {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
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
            } else if let Some((native, remainder, cleared)) = finish_drinking_milk(
                inventory,
                effects,
                &started,
                game_mode,
            ) {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
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
                for effect_id in cleared {
                    apply(
                        conn,
                        state,
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
                    state,
                    proto,
                    &vitals,
                    &effects,
                    Vec3::new(x, y, z),
                    // Self-facing, per `publish_health`'s own call sites.
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
            // `!invulnerable &&`: a creative player's air bar does not
            // deplete and they never drown. Suppressed here rather than
            // at the damage below because `PlayerVitals` is mode-free by
            // design, and a depleting bar that can never hurt is worse
            // than no bar at all. A missing resident cell defers the
            // complete air probe rather than treating it as air.
            let outcome = tick_player_air_supply(
                vitals,
                eye_in_water,
                invulnerable,
                &effects,
            );
            if let Some(air) = outcome.air_changed {
                apply(conn, state, proto.encode_air_supply_update(air)).await?;
            }
            if outcome.damage.is_some() {
                publish_health(
                    conn,
                    state,
                    proto,
                    &vitals,
                    &effects,
                    Vec3::new(x, y, z),
                    // Self-facing, per `publish_health`'s own call sites.
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
                let flags = lodestone_entity::DamageFlags::for_damage_type_name(hit.damage_type)
                    .expect("a player hit names a real damage type");
                // A raised shield takes the hit before armour sees it.
                let now = u64::try_from(world.time().game_time).unwrap_or(0);
                let feet = Vec3::new(x, y, z);
                let blocked = shield.block(
                    |slot| inventory.native(slot).cloned(),
                    now,
                    &crate::shield::IncomingHit {
                        damage: hit.raw_damage,
                        damage_type: hit.damage_type,
                        source: hit.attacker_pos,
                        disable_seconds: hit.shield_disable_seconds,
                    },
                    feet,
                    player_rot.unwrap_or_default().yaw,
                );
                if let Some(blocked) = blocked {
                    wear_item_in_slot(conn, state, proto, inventory, blocked.native, blocked.item_damage)
                        .await?;
                    if blocked.disable_ticks > 0 {
                        shield.disable_until(now + blocked.disable_ticks);
                        apply(
                            conn,
                            state,
                            proto.encode_item_cooldown(
                                crate::shield::COOLDOWN_GROUP,
                                i32::try_from(blocked.disable_ticks).unwrap_or(i32::MAX),
                            ),
                        )
                        .await?;
                        let seed = i64::from(drops_rng.next_int(i32::MAX));
                        if let Some(effect) =
                            crate::effects::shield_sound(true, feet, drops_rng.next_f32(), seed)
                        {
                            block_ticks.publish_effect(effect);
                        }
                    }
                }
                let remaining = hit.raw_damage - blocked.map_or(0.0, |b| b.damage);
                let landed = vitals
                    .apply_damage(
                        remaining,
                        &effects.overlay_defenses(inventory.combat_stats().defenses),
                        flags,
                    )
                    .is_some();
                if blocked.is_some() && landed {
                    let seed = i64::from(drops_rng.next_int(i32::MAX));
                    if let Some(effect) = crate::effects::shield_sound(false, feet, drops_rng.next_f32(), seed) {
                        block_ticks.publish_effect(effect);
                    }
                }
                // A fully blocked hit starts the invulnerability window and
                // nothing else: no health change, hurt animation or knockback.
                if landed && remaining > 0.0 {
                    if hit.poison_ticks > 0 {
                        effects.apply("minecraft:poison", hit.poison_ticks, 0);
                        apply(
                            conn,
                            state,
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
                        state,
                        proto,
                        &vitals,
                        &effects,
                        Vec3::new(x, y, z),
                        // Self-facing, per `publish_health`'s own call sites.
                        LOCAL_PLAYER_ENTITY_ID,
                        username,
                        crate::vitals::DeathCause::Generic,
                        advancements,
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
                state,
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
            apply(conn, state, proto.encode_game_event(10, 1.0)).await?;
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
                    state,
                    proto,
                    &vitals,
                    &effects,
                    Vec3::new(x, y, z),
                    // Self-facing, per `publish_health`'s own call sites.
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
                    state,
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
                state,
                proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:bad_omen"),
            )
            .await?;
            effects.apply("minecraft:raid_omen", 600, amplifier);
            apply(
                conn,
                state,
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
            *raid_omen_position = Some(pos);
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
                state,
                proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:raid_omen"),
            )
            .await?;
            mobs.with(|sim| sim.create_or_extend_raid(origin, difficulty, amplifier));
        }
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
            state,
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
            state,
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
    sync_effect_max_health(conn, state, proto, vitals, &effects).await?;
    if !effects.is_empty() {
        let out = effects.tick(
            i32::try_from(world.time().game_time.max(0)).unwrap_or(i32::MAX),
            vitals.health(),
            vitals.max_health(),
        );
        if out.exhaustion > 0.0 {
            vitals.add_exhaustion(out.exhaustion);
        }
        let saturation_changed = apply_effect_saturation(vitals, out.saturation);
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
                state,
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
                username,
                crate::vitals::DeathCause::Wither,
                advancements,
                player_uuid,
                hurt_landed.then_some(crate::vitals::HurtDirection::PURE_ROLL),
            )
            .await?;
        }
        sync_effect_max_health(conn, state, proto, vitals, &effects).await?;
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
                state,
                proto,
                &vitals,
                &effects,
                // Hunger needs no terrain and runs even before the
                // first movement packet — see the wither arm just
                // above for the same fallback.
                player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                // Self-facing, per `publish_health`'s own call sites.
                LOCAL_PLAYER_ENTITY_ID,
                username,
                crate::vitals::DeathCause::Starve,
                advancements,
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

    Ok(())
}

/// Everything the world published since the last pass that this player's wire is owed: sounds,
/// particles and level events from the effect lane (an eating bite, a brush stroke, a finished
/// dig), explosions, mob hurt and death cues, and weather changes.
#[allow(clippy::too_many_arguments)]
pub(super) async fn publish_world_events<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    weather: &WeatherFeed,
    mobs: &MobHandle,
    player_uuid: uuid::Uuid,
    player_pos: &mut Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
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
            if let Some((px, py, pz)) = *player_pos
                && player_overlaps_piston_sweep(px, py, pz, source, dest)
            {
                let (nx, ny, nz) =
                    (px + push_delta.x, py + push_delta.y, pz + push_delta.z);
                let current = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
                *player_pos = Some((nx, ny, nz));
                apply(
                    conn,
                    state,
                    proto.encode_teleport_with_id(
                        issue_teleport_id(teleport_acknowledgements),
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
        apply(conn, state, proto.encode_world_effect(&effect)).await?;
    }
    // Drain explosions emitted by the shared mob simulation. The
    // feed has one consumer for each in-memory world instance.
    for detonation in explosions.drain_all() {
        apply(
            conn,
            state,
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
        apply(conn, state, directive).await?;
    }
    // Drain weather transitions published by the world tick loop.
    // The feed has one consumer for each in-memory world instance.
    for event in weather.drain_all() {
        let (kind, value) = event.wire();
        apply(conn, state, proto.encode_game_event(kind, value)).await?;
    }

    Ok(())
}

/// One connection's read positions in the world's append-only feeds.
pub(super) struct ConnectionFeeds {
    chat: u64,
    swing: u64,
    plugin: u64,
    sleep: crate::sleep::SleepCursor,
}

impl ConnectionFeeds {
    /// Chat starts at the log's current end so a joining player receives only messages published
    /// this session. Swings start at the registration-time snapshot, which excludes older swings
    /// but keeps those appended before the first drain. Plugin broadcasts start at zero: a
    /// broadcast is host-published state, and a client announcing `minecraft:brand` support at
    /// join is owed the brand payload queued before it arrived.
    pub(super) fn new(
        players: Option<&PlayerRegistry>,
        initial_swing_cursor: Option<u64>,
        world: &crate::world_state::WorldStateHandle,
    ) -> Self {
        Self {
            chat: players.map_or(0, PlayerRegistry::chat_cursor),
            swing: initial_swing_cursor.unwrap_or(0),
            plugin: 0,
            sleep: world.sleep_feed().cursor(),
        }
    }
}

/// What a connection's feed pass reads and mutates beyond the feed cursors.
pub(super) struct FeedDrain<'a> {
    pub players: Option<&'a PlayerRegistry>,
    pub world: &'a crate::world_state::WorldStateHandle,
    pub resource_packs: &'a ResourcePackPushFeed,
    pub plugin_channels: &'a PluginChannelRegistry,
    pub client_channels: &'a mut ClientChannels,
    pub player_uuid: uuid::Uuid,
    pub player_entity_id: i32,
    pub username: &'a str,
    pub game_mode: &'a mut GameMode,
    pub abilities: &'a mut Abilities,
    pub inventory: &'a mut PlayerInventory,
    pub advancements: &'a mut AdvancementManager,
    pub effects: &'a mut crate::mob_effects::ActiveEffects,
    pub vitals: &'a mut PlayerVitals,
    pub experience: &'a mut crate::experience::PlayerExperience,
    pub player_pos: &'a mut Option<(f64, f64, f64)>,
    pub player_rot: &'a mut Option<Rotation>,
    pub teleport_acknowledgements: &'a mut Option<TeleportAcknowledgements>,
}

/// Feeds the sleep vote its voter count and delivers everything the shared feeds owe this
/// connection: night skips, resource-pack pushes, other players' chat and arm swings, effects
/// another player's command aimed at this one, and plugin-channel broadcasts.
pub(super) async fn publish_connection_feeds<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    feeds: &mut ConnectionFeeds,
    drain: FeedDrain<'_>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    let FeedDrain {
        players,
        world,
        resource_packs,
        plugin_channels,
        client_channels,
        player_uuid,
        player_entity_id,
        username,
        game_mode,
        abilities,
        inventory,
        advancements,
        effects,
        vitals,
        experience,
        player_pos,
        player_rot,
        teleport_acknowledgements,
    } = drain;
    // The voter count. Every player in the registry counts; with no registry (singleplayer)
    // nothing is fed and the vote's one-sleeper floor is the correct single-player rule.
    if let Some(registry) = players {
        world.sleep_vote().set_active(registry.len() as u32);
    }
    // A skipped night re-anchors this client's day clock to the morning. Every connection of the
    // world reads the one feed through its own cursor.
    for event in world.sleep_feed().since(&mut feeds.sleep) {
        match event {
            SleepEvent::SkippedNight { game_time, morning } => {
                apply(conn, state, proto.encode_set_time(game_time, Some(morning))).await?;
            }
        }
    }
    for push in resource_packs.drain_all() {
        apply(conn, state, proto.encode_resource_pack_push(&push)).await?;
    }
    if let Some(registry) = players {
        for line in registry.chat_since(&mut feeds.chat) {
            apply(conn, state, proto.encode_system_chat(&line.rendered())).await?;
        }
        // Arm swings, excluding the connection that produced each event.
        for event in registry.swings_since(&mut feeds.swing) {
            if event.entity_id != player_entity_id {
                apply(conn, state, proto.encode_animate(event.entity_id, swing_action(event.hand))).await?;
            }
        }
        // A drain, not a cursor: the queue is per-uuid with this connection as its only reader,
        // which is what makes the delivery directed. A cursor over a shared log would hand one
        // player's game-mode change to everyone.
        for effect in registry.take_effects(player_uuid) {
            apply_own_effect(
                conn,
                proto,
                state,
                game_mode,
                abilities,
                inventory,
                Some(registry),
                player_uuid,
                effect,
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
    // Host-published plugin-channel broadcasts, filtered to channels this client announced.
    for (channel, data) in plugin_channels.outbound_since(&mut feeds.plugin, client_channels) {
        apply(conn, state, proto.encode_custom_payload(&channel, &data)).await?;
    }
    Ok(())
}
