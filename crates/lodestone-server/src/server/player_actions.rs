//! Smaller player action handlers: attacks and swings, spectator actions, and recipe-book bookkeeping.

use super::*;

/// Resolves a `minecraft:attack` request against the live mob
/// simulation: runs the damage pipeline and, for a sprinting attacker, the
/// melee knockback bonus, through [`MobSim::attack`](crate::MobSim::attack).
///
/// **No reply packet is sent from here.** The attack request has no direct
/// acknowledgement. The entity-streaming pass
/// (`EntityStreamer::sync`, called immediately after
/// [`dispatch_play_packet`] returns, on every inbound packet including this
/// one) to carry the result to every connection tracking the target: a
/// knocked-back mob's new position/velocity, or its removal on a killing
/// blow, both flow through [`MobHandle`]'s [`EntitySource`] implementation.
/// The `mobs` handle is shared with [`crate::tick::run_tick_loop`], so the
/// stream observes the updated snapshot. See [`MobSim::attack`](crate::MobSim::attack)'s own doc
/// comment for why `attacker_pos` (not a tracked player yaw — this crate
/// tracks no player rotation at all) stands in for
/// [`lodestone_physics::knockback::attack_direction`]'s real facing formula.
///
/// A connection with no tracked position (`player_pos` is `None`) still lands the
/// damage; only the knockback direction needs a position, so it is skipped
/// entirely in that case (`attacker_pos` defaults to the origin and
/// `knockback_power` is forced to `0.0`) rather than guessing one, the same
/// "no data yet, don't guess" gate `vitals_tick`'s own submersion check
/// already uses for a fresh session.
///
/// `sprinting` is this connection's last-known [`ServerBound::PlayerInput`]
/// sprint flag — see [`SPRINT_ATTACK_KNOCKBACK_POWER`]'s own doc comment for
/// why a non-sprinting attack's knockback power is correctly `0.0`, not a
/// bug.
///
/// Routes through [`MobSim::attack_from_player`] so the mob simulation receives
/// the attacking account identity and can record villager reputation events.
/// Uses `LOCAL_PLAYER_ENTITY_ID` for [`PlayerIdentity::entity_id`], matching
/// every other self-facing identity built in this file.
pub(super) fn apply_attack(
    mobs: &MobHandle,
    player_pos: Option<(f64, f64, f64)>,
    sprinting: bool,
    inventory: &PlayerInventory,
    effects: &crate::mob_effects::ActiveEffects,
    entity_id: i32,
    player_uuid: uuid::Uuid,
) {
    let (attacker_pos, knockback_power) = match player_pos {
        Some((x, y, z)) => (
            Vec3::new(x, y, z),
            if sprinting {
                SPRINT_ATTACK_KNOCKBACK_POWER
            } else {
                0.0
            },
        ),
        None => (Vec3::new(0.0, 0.0, 0.0), 0.0),
    };
    // The weapon feed resolves the held item through the `ATTACK_DAMAGE`
    // attribute fold. An empty hand uses the player's attribute base with no
    // modifiers.
    let raw_damage = effects.melee_damage(inventory.combat_stats().attack_damage);
    mobs.with(|sim| {
        sim.attack_from_player(
            entity_id,
            Some(PlayerIdentity {
                uuid: player_uuid,
                entity_id: LOCAL_PLAYER_ENTITY_ID,
            }),
            attacker_pos,
            raw_damage,
            DamageFlags::default(),
            knockback_power,
        )
    });
}

/// Records the main-hand animation implied by a serverbound attack packet.
///
/// Every hosted protocol uses a main-hand attack action. The connection that
/// sent it already animates locally, so only a multiplayer registry needs the
/// event; singleplayer has no remote observer.
pub(super) fn record_attack_swing(players: Option<&PlayerRegistry>, player_entity_id: i32) {
    if let Some(registry) = players {
        registry.swing(player_entity_id, lodestone_model::Hand::Main);
    }
}

/// Resolves `ServerBound::SpectatorAction`'s target against this crate's two
/// id-keyed entity sources — the mob simulation and the player registry —
/// and returns the entity id to attach the camera to, or `None` when any of
/// vanilla's own gates (spectator mode, a target present, a resolvable
/// position, in range) fail. See `ServerBound::SpectatorAction`'s own doc
/// comment for the narrowing from vanilla's box-aware range/`isPickable`
/// checks down to a plain centre-to-centre distance.
pub(super) fn apply_spectator_action(
    game_mode: GameMode,
    target_entity_id: Option<i32>,
    player_pos: Option<(f64, f64, f64)>,
    mobs: &MobHandle,
    players: Option<&PlayerRegistry>,
) -> Option<i32> {
    if game_mode != GameMode::Spectator {
        return None;
    }
    let target_id = target_entity_id?;
    let (px, py, pz) = player_pos?;
    let target_pos = mobs.with(|sim| sim.position(target_id)).or_else(|| {
        players
            .map(PlayerRegistry::candidates)
            .unwrap_or_default()
            .into_iter()
            .find(|c| c.entity_id == target_id)
            .map(|c| c.position)
    })?;
    // Vanilla's own is-within-entity-interaction-range check grows the target's actual
    // bounding box by this constant (its own `INTERACTION_RANGE` for
    // spectator-camera checks) before measuring; this crate tracks no
    // per-entity bounding box, so a plain centre-to-centre distance against
    // the same 3-block figure is the disclosed narrowing.
    const SPECTATOR_INTERACTION_RANGE: f64 = 3.0;
    let dx = target_pos.x - px;
    let dy = target_pos.y - py;
    let dz = target_pos.z - pz;
    if dx * dx + dy * dy + dz * dz <= SPECTATOR_INTERACTION_RANGE * SPECTATOR_INTERACTION_RANGE {
        Some(target_id)
    } else {
        None
    }
}

/// Maps main/off-hand ordinals to animation action bytes; invalid hands use main.
pub(super) fn swing_action(hand: lodestone_model::Hand) -> u8 {
    match hand {
        lodestone_model::Hand::Main => 0,
        lodestone_model::Hand::Off => 3,
    }
}

/// Folds one recipe-book acknowledgement and returns the one-entry update that
/// exposes the cleared flag back to the client. The wire id is an opaque
/// position in the server-owned *advertised* entries, not every recipe in the
/// corpus: entries without a display must not manufacture acknowledgement
/// state from a malformed packet.
pub(super) fn record_recipe_book_seen(
    inventory: &mut PlayerInventory,
    recipe_index: i32,
) -> Option<crate::crafting::RecipeBookEntry> {
    let mut entry = crate::crafting::recipe_book_entries()
        .iter()
        .find(|entry| entry.id == recipe_index)?
        .clone();
    inventory.mark_recipe_book_entry_seen(recipe_index);
    entry.highlight = false;
    Some(entry)
}

/// Makes a connection-specific recipe-book snapshot from the shared immutable
/// corpus. A fresh display id highlights until this connection acknowledges it;
/// the clone keeps that mutable flag out of the shared recipe definitions.
pub(super) fn recipe_book_snapshot(inventory: &PlayerInventory) -> Vec<crate::crafting::RecipeBookEntry> {
    crate::crafting::recipe_book_entries()
        .iter()
        .cloned()
        .map(|mut entry| {
            entry.highlight = inventory.recipe_book_entry_is_highlighted(entry.id);
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests;
