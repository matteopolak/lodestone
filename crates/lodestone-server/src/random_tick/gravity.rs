//! Gravity-block settling is a pure decision family.
//!
//! The scheduled-tick consumer owns the entity handoff and world persistence;
//! this module only determines whether a block should begin falling and where it lands.

use super::*;

/// What `FallingBlock.tick` decided at one position: the block is unsupported and
/// is about to become a `FallingBlockEntity`.
///
/// Returned rather than applied because the two halves live in different places —
/// the world mutation is the caller's (`crate::tick`'s drain owns the column and
/// the outbound feed) and the entity is `crate::mobs::MobSim`'s. Keeping the
/// decision pure is also what stops this function reintroducing the teleport it
/// used to be: it can no longer write a block anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GravitySettle {
    /// The state that leaves the world and rides the entity.
    pub state: lodestone_data::block_states::BlockStateValue,
    /// Where the entity will come to rest —
    /// [`gravity_tick::find_landing_y`]'s answer against the world as it is now.
    pub landing_y: i32,
}
/// `FallingBlock.tick`: whether the gravity block at world `(x, y, z)` is
/// unsupported and should become a falling entity, and where it will land.
///
/// `None` for anything that is not a gravity block, for one that is still
/// supported, and for the (unreachable) case where the landing scan does not
/// move. Draws no RNG — `FallingBlock.tick` itself draws none — so this needs no
/// `&mut self`, and it no longer needs `&mut column` either: **it mutates
/// nothing.**
///
/// # What changed, and why it is not a regression
///
/// This used to move the block: `set_block(y, AIR)` plus
/// `set_block(landing_y, state)` in one step, returning both as
/// [`RandomTickEvent`]s. That was the whole of the reported *"it just teleports to
/// its final place at the bottom instead of falling down and landing"* — a
/// documented simplification standing in for a falling-block entity that did not
/// exist. It does now, so the teleport is gone and this function answers the
/// question rather than acting on it.
///
/// `pub(crate)` for `crate::tick`'s scheduled-tick drain, which dispatches
/// `gravity_tick::TICK_GRAVITY` **straight here** rather than through
/// [`propagate_and_react`]. That is not a shortcut: `propagate` notifies an
/// origin's six neighbours and not the origin, while vanilla's `onPlace` tick
/// fires on the placed block itself, so the propagate route would settle the
/// wrong cells. See `crate::gravity_tick`'s module doc.
pub(crate) fn settle_gravity_at(
    column: &crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
) -> Option<GravitySettle> {
    let lx = x - min_x;
    let lz = z - min_z;
    let state = column.block_state(lx, y, lz).to_string();
    if !gravity_tick::is_gravity_block(base_name(&state)) {
        return None;
    }
    let below = column.block_state(lx, y - 1, lz).to_string();
    if !gravity_tick::is_free(&below) {
        return None;
    }
    let landing_y = gravity_tick::find_landing_y(
        |probe_y| gravity_tick::is_free(column.block_state(lx, probe_y, lz)),
        y,
        column.min_y,
    );
    if landing_y == y {
        // Shouldn't happen (we already confirmed `below` is free, so the
        // scan must move at least one step) — defensive, not reachable.
        return None;
    }
    Some(GravitySettle {
        state: lodestone_data::block_states::BlockStateValue::parse(&state),
        landing_y,
    })
}
