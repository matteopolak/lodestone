//! Publishing health and fall state: the health/food packets and the per-tick fall-damage sample.

use super::*;

/// Publishes the player's post-damage health, **and the death notification when
/// that damage was the hit that killed them.**
///
/// # Why every damage site must go through here
///
/// Health reaching zero does not by itself produce the death screen, animation,
/// sound, or statistic. This function is the single choke point for all five
/// damage sites, so it adds those cues exactly once.
///
/// # Why no "already announced" latch is needed
///
/// Every [`PlayerVitals`] damage entry point returns `None` once `health <= 0.0`
/// (its own first guard), so the caller only reaches this function on a hit that
/// *landed*, and a landed hit can cross zero exactly once per life. The kill
/// packet therefore fires once, without state to keep. [`PlayerVitals::respawn`]
/// re-arms it by construction. That is a property of the guards rather than of
/// this function, so `death_is_announced_exactly_once_per_life` pins it.
///
/// # The animation and sound cues, and why they are here rather than at each site
///
/// A hit also has to be *seen and heard*, and neither `set_health` nor
/// `player_combat_kill` carries any animation or sound: vanilla plays the camera
/// damage tilt off `hurt_animation`, tips the body over off `entity_event` byte
/// 3, and plays `playHurtSound`/`getDeathSound` alongside — this crate encoded
/// none of the three until this function grew them, so singleplayer damage was
/// silent and a death was a screen with a motionless, silent avatar behind it.
///
/// All three cues belong at this choke point for the same reason the death
/// *count* does — the guards above already make "a hit landed" and "the hit
/// that killed them" exactly-once properties, and re-deriving any of them at
/// fourteen call sites is how one of them ends up sending twice on a tick that
/// both burned and starved, or silent on the one path nobody remembered.
///
/// The hurt/death sound comes from [`crate::effects::mob_vocalisation`] with
/// `"minecraft:player"` — entity-type-generic despite the name, and it already
/// resolves the real registered `minecraft:entity.player.hurt`/`.death` sound
/// events. Pitch and the sound-variant seed are both held constant: this
/// function has no RNG source threaded to it, and neither player sound event has
/// more than one variant to pick between, so a constant seed costs nothing here
/// (contrast [`crate::effects::WorldEffect::Sound`]'s own doc, which explains why
/// a constant seed usually would).
///
/// `hurt` is what distinguishes a **hit** from a mere publish: two of the call
/// sites (the status-effect arm and the food arm) reach here for a *heal* or a
/// bare food-bar change, and flashing the screen red on a regeneration tick is a
/// worse bug than not flashing it at all. `None` there; `Some` only where damage
/// actually landed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn publish_health<T, P>(
    conn: &mut Connection<T>,
    state: &mut State,
    proto: &P,
    vitals: &PlayerVitals,
    effects: &crate::mob_effects::ActiveEffects,
    // The position the hurt/death sound is centred on (`WorldEffect::Sound`'s
    // wire form quantises it to eighths of a block, so a stale or zeroed
    // position only ever costs spatialisation accuracy, never a dropped
    // packet). Every call site already tracks this player's last reported
    // position for its own damage-source logic; a caller with no reported
    // position yet (joined and never moved) passes `Vec3::default()`.
    pos: Vec3,
    // Every caller passes `LOCAL_PLAYER_ENTITY_ID`, never a `PlayerRegistry`
    // ticket id: every packet built from this reaches `conn` directly, this
    // connection's own socket, and the client only recognises itself under
    // the constant its own login entity-id field (`begin_play_at`) claimed —
    // see the call sites' own comments. Kept as a plain parameter rather than
    // inlining the constant here so a future caller broadcasting to *other*
    // connections is not tempted to reuse this function for that; it never
    // varies today, and that is the point.
    player_entity_id: i32,
    username: &str,
    cause: crate::vitals::DeathCause,
    // The statistics store, for the `minecraft:deaths` custom counter. This is the
    // right site rather than each damage source: the function's own guards already
    // make crossing zero happen exactly once per life (see the doc comment above),
    // which is precisely the property a death *count* needs. Awarding it at each
    // `apply_*` call site would double-count a tick that both drowned and fell.
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    // `Some(direction)` when this publish follows a hit that landed — see the doc
    // comment's third section. Every production site passes
    // `HurtDirection::PURE_ROLL`, and that is vanilla's own answer rather than a
    // stub: every damage type this crate has is `no_knockback`-tagged, so
    // `indicateDamage` would never see a non-zero offset for any of them.
    hurt: Option<crate::vitals::HurtDirection>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    // Ahead of the health packet, matching vanilla's order: `indicateDamage`
    // fires inside `hurtServer`, while the health value rides vanilla's own
    // per-player tick routine. The client folds this into the view bob's countdown,
    // so it wants to arrive with (or before) the health drop it explains.
    if let Some(direction) = hurt {
        apply(
            conn,
            state,
            proto.encode_hurt_animation(player_entity_id, direction.yaw_degrees()),
        )
        .await?;
        // Vanilla's own `hurtServer`'s `playHurtSound`/`die`'s death sound,
        // folded into this same choke point for the reason this function's doc
        // gives. `died` picks the death sound instead of the hurt one on the
        // killing blow, matching the `encode_entity_event` branch below rather
        // than re-deriving its own health check.
        if let Some(effect) = crate::effects::mob_vocalisation(
            "minecraft:player",
            pos,
            vitals.health() <= 0.0,
            false,
            1.0,
            0,
        ) {
            apply(conn, state, proto.encode_world_effect(&effect)).await?;
        }
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
    if vitals.health() <= 0.0 {
        if matches!(effects.death_trigger(), Some(crate::mob_effects::DeathTrigger::WindCharged)) {
            apply(
                conn,
                state,
                proto.encode_world_effect(&crate::effects::wind_charged_death(pos)),
            )
            .await?;
        }
        advancements.award_stat(
            player_uuid,
            crate::advancements::StatKey::new(
                crate::advancements::StatType::Custom,
                "minecraft:deaths",
            ),
            1,
        );
        let message = cause.death_message(username);
        apply(
            conn,
            state,
            proto.encode_player_combat_kill(player_entity_id, &message),
        )
        .await?;
        // Vanilla's own entity-die routine's own broadcast, which its own
        // level broadcast-entity-event routine
        // sends to the dying player too (its own chunk-map broadcast-and-send routine). It is what
        // starts the client's `deathTime` counter — the fall-over tilt the red
        // overlay persists through. The death *screen* comes from the packet above;
        // this is the body behind it, and without it the avatar stands upright
        // through its own death.
        apply(
            conn,
            state,
            proto.encode_entity_event(player_entity_id, crate::protocol::entity_event::DEATH),
        )
        .await?;
    }
    Ok(())
}

/// Feeds one `on_ground` sample to the [`FallTracker`] from a movement packet
/// that carried **no** y coordinate, reusing the last position associated with
/// this connection.
///
/// Reusing the remembered y is not an approximation: `move_player_rot` and
/// `move_player_status_only` are precisely the two packets vanilla's own
/// client-side send-position routine picks when position did *not* change this tick,
/// so the last reported y is the current y by construction. Feeding it back
/// with the new `on_ground` is therefore the same `(y, on_ground)` pair the
/// tracker would have seen had the client sent a position packet.
///
/// Returns without touching the tracker when no position has been reported
/// yet — a status packet before the first movement packet has no y to pair
/// with, and inventing one (say, the spawn point) would fabricate a fall.
#[allow(clippy::too_many_arguments)]
pub(super) async fn fall_status_sample<T, P, S>(
    conn: &mut Connection<T>,
    state: &mut State,
    proto: &P,
    // Read the retained terrain at the player's feet for water, climbable, and
    // landing block facts. A missing cell defers this sample until a later
    // packet; it must not turn a movement/status probe into cold generation.
    source: &S,
    player_pos: &Option<(f64, f64, f64)>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    effects: &crate::mob_effects::ActiveEffects,
    username: &str,
    on_ground: bool,
    client_loaded: bool,
    // `invulnerable` — creative and spectator. `fall` is not in
    // `#minecraft:bypasses_invulnerability` (only `out_of_world` and
    // `generic_kill` are), so an invulnerable player takes none of it. The
    // *tracker* still samples, so the fall is still tracked; only the hit is
    // skipped, matching the damage-immunity rule.
    invulnerable: bool,
    // `minecraft:deaths` counter, threaded only to reach
    // `publish_health` — see its own parameter comment for why the count belongs
    // there and not at each damage source.
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    if !client_loaded {
        return Ok(());
    }
    let Some((x, y, z)) = *player_pos else {
        return Ok(());
    };
    let Some(sample) = resident_fall_sample(source, x, y, z, on_ground) else {
        return Ok(());
    };
    if let Some(raw) = fall.on_player_moved(sample)
        && !invulnerable
        && vitals.apply_fall_damage(raw as f32).is_some()
    {
        publish_health(
            conn,
            state,
            proto,
            vitals,
            effects,
            Vec3::new(x, y, z),
            // Always `LOCAL_PLAYER_ENTITY_ID`, never the registry ticket's id:
            // this packet goes straight to `conn`, this player's own socket,
            // and vanilla's own login entity-id field (`begin_play_at`) always claims that
            // constant regardless of whether a `PlayerRegistry` exists — see
            // `LOCAL_PLAYER_ENTITY_ID`'s own doc comment. The ticket's real id
            // is for *other* connections' view of this player, never this one.
            LOCAL_PLAYER_ENTITY_ID,
            username,
            crate::vitals::DeathCause::Fall,
            advancements,
            player_uuid,
            // `minecraft:fall` is `no_knockback`-tagged, so vanilla's own
            // `indicateDamage` offset for it is `(0, 0)`.
            Some(crate::vitals::HurtDirection::PURE_ROLL),
        )
        .await?;
    }
    Ok(())
}
