//! Read-only lookups against resident chunk state: fall samples, block states, columns and beacon levels.

use super::*;

/// Decodes and applies one inbound packet once the connection is in
/// [`State::Play`]: matches a keep-alive echo against the pending challenge
/// (clearing it, so the next keep-alive tick does not mistake a live client
/// for a dead one), streams the view when the player's chunk column changed,
/// tracks the player's latest position for [`PlayerVitals`]' submersion test,
/// feeds [`FallTracker`] and applies any resulting fall damage, applies a
/// block break/placement (see [`apply_block_action`]/[`apply_use_item_on`]),
/// applies a difficulty/game-rule change (see
/// [`apply_difficulty_change`]/[`apply_game_rule_changed`]), applies a
/// respawn/game-rule-request `client_command` (see [`apply_client_command`]),
/// resizes the streamed view on a settings change (see
/// [`ViewTracker::set_view_radius`]), advances the chunk-batch
/// flow-control gate (see [`send_view_update`]), or applies a hotbar
/// selection/container click/creative-slot write against [`PlayerInventory`]
/// (see
/// [`apply_carried_item_changed`]/[`apply_container_clicked`]/[`apply_creative_mode_slot_set`]).
/// The `PlayerLoaded` marker is folded into the connection's readiness state;
/// fall simulation begins only after that marker, and is re-armed after a
/// respawn. Other unmodeled packets remain [`ServerBound::Ignored`] in
/// `State::Play`.
/// Reads the two cells needed by fall tracking without admitting terrain.
///
/// Movement and status packets arrive on the same task that drives the
/// integrated connection. A missing resident cell therefore means "try again
/// after the view stream catches up", not "ask the source to generate a
/// column now". Production packet/timer paths use this gate.
pub(super) fn resident_fall_sample<S: ChunkSource + ?Sized>(
    source: &S,
    x: f64,
    y: f64,
    z: f64,
    on_ground: bool,
) -> Option<FallSample> {
    let bx = x.floor() as i32;
    let bz = z.floor() as i32;
    let feet = resident_block_state(source, bx, y.floor() as i32, bz)?;
    let below = resident_block_state(source, bx, (y - 0.2).floor() as i32, bz)?;
    Some(FallSample {
        y,
        on_ground,
        in_water: feet.block() == Block::Water,
        fall_resetting: crate::fall::is_fall_damage_resetting(feet),
        block_damage_modifier: crate::fall::block_damage_modifier(below),
    })
}

/// Reads a state id from a retained column without admitting terrain.
pub(super) fn resident_block_state<S: ChunkSource + ?Sized>(source: &S, x: i32, y: i32, z: i32) -> Option<StateId> {
    match source.try_resident_block_state_id(x, y, z) {
        Some(crate::chunk_store::TryResident::Present(state)) => Some(state),
        Some(crate::chunk_store::TryResident::Busy)
        | Some(crate::chunk_store::TryResident::Absent) => None,
        None => Some(source.resident_block_state_id(x, y, z).unwrap_or_else(|| source.block_state_id(x, y, z))),
    }
}

/// Captures a resident column through the source's atomic try gate when it has
/// one. `Busy` and `Absent` both defer the caller; only legacy sources that do
/// not expose the gate use the older resident snapshot method.
pub(super) fn resident_column<S: ChunkSource + ?Sized>(source: &S, cx: i32, cz: i32) -> Option<ChunkColumn> {
    match source.try_resident_column(cx, cz) {
        Some(crate::chunk_store::TryResident::Present(column)) => Some(column),
        Some(crate::chunk_store::TryResident::Busy)
        | Some(crate::chunk_store::TryResident::Absent) => None,
        None => source.resident_column(cx, cz),
    }
}

/// Recomputes a beacon pyramid only from retained cells. A failed layer is a
/// complete answer (`Some(0..=3)`); a missing cell in a layer that otherwise
/// passes defers the periodic effect probe instead of entering the generating
/// beacon helper.
pub(super) fn resident_beacon_levels<S: ChunkSource + ?Sized>(
    source: &S,
    x: i32,
    y: i32,
    z: i32,
) -> Option<u8> {
    let mut levels = 0u8;
    for step in 1..=4i32 {
        let ly = y - step;
        let mut layer_ok = true;
        'layer: for lx in (x - step)..=(x + step) {
            for lz in (z - step)..=(z + step) {
                let state = resident_block_state(source, lx, ly, lz)?;
                if !crate::beacon::BASE_BLOCKS.contains(&state.block()) {
                    layer_ok = false;
                    break 'layer;
                }
            }
        }
        if !layer_ok {
            break;
        }
        levels = u8::try_from(step).unwrap_or(4);
    }
    Some(levels)
}

/// Checks a beacon beam without allowing a cold column to enter the 20 Hz
/// connection task. `None` means the complete scan is not resident yet;
/// `Some(false)` is a resident obstruction and is therefore a real answer.
pub(super) fn resident_beam_unobstructed<S: ChunkSource + ?Sized>(
    source: &S,
    x: i32,
    y: i32,
    z: i32,
    scan_height: i32,
) -> Option<bool> {
    let max_y = source
        .dimension()
        .map(crate::dimension::Dimension::max_y)
        .or_else(|| {
            resident_column(source, x.div_euclid(16), z.div_euclid(16))
                .map(|column| column.min_y + column.height - 1)
        })
        .unwrap_or(y.saturating_add(scan_height));
    for dy in 1..=scan_height {
        if y.saturating_add(dy) > max_y {
            break;
        }
        let state = resident_block_state(source, x, y + dy, z)?;
        if state.block() == Block::Bedrock {
            continue;
        }
        let transparent = lodestone_data::light_props::dampening(state) < 15;
        if !transparent {
            return Some(false);
        }
    }
    Some(true)
}
