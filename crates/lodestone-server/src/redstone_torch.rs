//! Redstone torches (the torch half of the redstone family): the 2-tick-delayed
//! on/off inversion — powered means unlit, unpowered means lit.
//!
//! # Cited directly
//!
//! `RedstoneTorchBlock.hasNeighborSignal`/`neighborChanged`/`tick`:
//!
//! ```text
//! protected boolean hasNeighborSignal(final Level level, final BlockPos pos, final BlockState state) {
//!    return level.hasSignal(pos.below(), Direction.DOWN);
//! }
//! protected void neighborChanged(...) {
//!    if (state.getValue(LIT) == this.hasNeighborSignal(level, pos, state) && !level.getBlockTicks().willTickThisTick(pos, this)) {
//!       level.scheduleTick(pos, this, 2);
//!    }
//! }
//! protected void tick(...) {
//!    boolean neighborSignal = this.hasNeighborSignal(level, pos, state);
//!    ...
//!    if (state.getValue(LIT)) {
//!       if (neighborSignal) { level.setBlock(pos, state.setValue(LIT, false), 3); ...toggle-frequency bookkeeping... }
//!    } else if (!neighborSignal && !isToggledTooFrequently(level, pos, false)) {
//!       level.setBlock(pos, state.setValue(LIT, true), 3);
//!    }
//! }
//! ```
//!
//! `RedstoneWallTorchBlock.hasNeighborSignal` overrides only which
//! neighbour is checked (the block it's mounted against, not "below"):
//!
//! ```text
//! protected boolean hasNeighborSignal(final Level level, final BlockPos pos, final BlockState state) {
//!    Direction opposite = state.getValue(FACING).getOpposite();
//!    return level.hasSignal(pos.relative(opposite), opposite);
//! }
//! ```
//!
//! # Named deviation: the anti-oscillation "burnout" guard is not modeled
//!
//! `RedstoneTorchBlock.isToggledTooFrequently`/`RECENT_TOGGLES`/`RESTART_DELAY`
//! is vanilla's defence against a torch clock (a redstone circuit that
//! flips a torch every 2 ticks forever): after 8 toggles within 60 ticks, the
//! torch locks unlit for 160 ticks. This crate has no per-level, per-position
//! toggle-history table (nothing else in this crate keeps one either), and a
//! torch clock is not a circuit this landing's own test suite builds (a
//! straight-line dust/torch circuit, the scope this module covers, never oscillates
//! fast enough to trip it) — so [`run_scheduled_tick`] always flips exactly
//! per the two `if`/`else if` branches above, with no burnout. A future
//! landing that specifically builds a torch-clock oracle test is the right
//! place to add the history table; adding it now with no producer to
//! exercise it would be the exact kind of correct-in-isolation code this
//! repo's own "islands" rule warns against.

use crate::neighbor_update::Direction;
use crate::redstone::{self, is_wall_torch, torch_lit, wall_torch_facing};
use lodestone_model::BlockPos;

pub use crate::redstone::{TORCH, WALL_TORCH};

/// Builds the canonical block-state string for a standing torch at `lit`.
#[must_use]
pub fn set_standing_lit(lit: bool) -> String {
    format!("{TORCH}[lit={lit}]")
}

/// Builds the canonical block-state string for a wall torch at `lit`,
/// preserving its existing `facing`.
#[must_use]
pub fn set_wall_lit(facing: Direction, lit: bool) -> String {
    format!("{WALL_TORCH}[facing={},lit={lit}]", redstone::direction_to_str(facing))
}

/// Dispatches to [`set_standing_lit`]/[`set_wall_lit`] based on which torch
/// `state` already is, preserving a wall torch's `facing`.
#[must_use]
pub fn set_lit(state: &str, lit: bool) -> String {
    if is_wall_torch(state) {
        set_wall_lit(wall_torch_facing(state), lit)
    } else {
        set_standing_lit(lit)
    }
}

/// `RedstoneTorchBlock.hasNeighborSignal`/`RedstoneWallTorchBlock.hasNeighborSignal`
/// — see this module's own doc comment for the full citation of both.
#[must_use]
pub fn has_neighbor_signal<F>(lookup: &F, pos: BlockPos, state: &str) -> bool
where
    F: Fn(BlockPos) -> String,
{
    let watch_direction = if is_wall_torch(state) { wall_torch_facing(state).opposite() } else { Direction::Down };
    redstone::signal_at(lookup, watch_direction.relative(pos), watch_direction, false) > 0
}

/// `true` iff a scheduled recheck should be queued right now — the
/// `neighborChanged` gate (`state.LIT == hasNeighborSignal`, before the
/// `willTickThisTick` de-dup, which [`crate::scheduled_tick::ScheduledTickQueue::has_scheduled`]
/// already provides at the call site).
#[must_use]
pub fn should_schedule_check(state: &str, has_signal: bool) -> bool {
    torch_lit(state) == has_signal
}

/// The delayed flip itself (`tick()`), evaluated against a **freshly
/// re-read** `has_signal` (vanilla re-derives `neighborSignal` when the
/// scheduled tick actually runs, not when it was scheduled two ticks
/// earlier) — `None` if the signal changed back in the meantime and neither
/// branch applies (a real, faithful no-op: the recheck simply finds nothing
/// to do).
#[must_use]
pub fn run_scheduled_tick(state: &str, has_signal: bool) -> Option<String> {
    let lit = torch_lit(state);
    if lit && has_signal {
        Some(set_lit(state, false))
    } else if !lit && !has_signal {
        Some(set_lit(state, true))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(entries: &[(BlockPos, &str)]) -> impl Fn(BlockPos) -> String + use<> {
        let entries: Vec<(BlockPos, String)> = entries.iter().map(|(p, s)| (*p, s.to_string())).collect();
        move |p: BlockPos| {
            entries
                .iter()
                .find(|(pos, _)| *pos == p)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| "minecraft:air".to_string())
        }
    }

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn set_lit_preserves_wall_torch_facing() {
        let wall = set_wall_lit(Direction::East, true);
        assert_eq!(wall, "minecraft:redstone_wall_torch[facing=east,lit=true]");
        assert_eq!(set_lit(&wall, false), "minecraft:redstone_wall_torch[facing=east,lit=false]");
    }

    /// Standing torch: `has_neighbor_signal` reads the block BELOW it —
    /// a lever-equivalent source there (a lit torch, for this reduced
    /// model) must be detected.
    #[test]
    fn standing_torch_detects_signal_from_below() {
        let torch_pos = pos(0, 1, 0);
        let source_pos = pos(0, 0, 0);
        let w = world(&[(source_pos, "minecraft:redstone_torch[lit=true]")]);
        // The source torch signals DOWN and every horizontal direction, but
        // NOT up — so a standing torch resting on top must see it via the
        // strong-power (direct signal) path since the source is a torch,
        // not a conductor... actually the check is `signal_at`, which for a
        // NON-conductor position just reads `weak_signal`. The source here
        // is itself a torch (non-conductor), so this exercises `weak_signal`
        // for `direction = Down` — the "every direction except UP" case.
        assert!(has_neighbor_signal(&w, torch_pos, "minecraft:redstone_torch[lit=true]"));
    }

    #[test]
    fn standing_torch_detects_no_signal_when_nothing_is_below() {
        let w = world(&[]);
        assert!(!has_neighbor_signal(&w, pos(0, 1, 0), "minecraft:redstone_torch[lit=true]"));
    }

    /// Wall torch mounted facing NORTH (attached to the block north of it):
    /// `has_neighbor_signal` must check the block to the NORTH, not below.
    #[test]
    fn wall_torch_detects_signal_from_its_own_mount_face_not_below() {
        let torch_pos = pos(0, 5, 0);
        // `RedstoneWallTorchBlock.hasNeighborSignal` checks
        // `pos.relative(FACING.getOpposite())` — for `facing = north` that is
        // the block to the SOUTH (the wall the torch is mounted against),
        // not north (an earlier version of this fixture checked the wrong
        // side and failed the assertion below).
        let mount_pos = Direction::South.relative(torch_pos);
        let below_pos = Direction::Down.relative(torch_pos);
        let state = "minecraft:redstone_wall_torch[facing=north,lit=true]";
        let w_mount_powered = world(&[(mount_pos, "minecraft:redstone_torch[lit=true]")]);
        assert!(has_neighbor_signal(&w_mount_powered, torch_pos, state));
        // Negative control: a source BELOW the wall torch must not count.
        let w_below_powered = world(&[(below_pos, "minecraft:redstone_torch[lit=true]")]);
        assert!(!has_neighbor_signal(&w_below_powered, torch_pos, state), "control failed: below must not be checked for a wall torch");
    }

    #[test]
    fn schedule_check_fires_exactly_on_the_two_mismatched_combinations() {
        let lit = "minecraft:redstone_torch[lit=true]";
        let unlit = "minecraft:redstone_torch[lit=false]";
        assert!(should_schedule_check(lit, true), "lit AND signaled: should turn off");
        assert!(should_schedule_check(unlit, false), "unlit AND unsignaled: should turn on");
        assert!(!should_schedule_check(lit, false), "lit AND unsignaled: steady state, no recheck");
        assert!(!should_schedule_check(unlit, true), "unlit AND signaled: steady state, no recheck");
    }

    #[test]
    fn scheduled_tick_turns_off_a_lit_torch_once_signaled() {
        let lit = "minecraft:redstone_torch[lit=true]";
        assert_eq!(run_scheduled_tick(lit, true), Some("minecraft:redstone_torch[lit=false]".to_string()));
    }

    #[test]
    fn scheduled_tick_turns_on_an_unlit_torch_once_unsignaled() {
        let unlit = "minecraft:redstone_torch[lit=false]";
        assert_eq!(run_scheduled_tick(unlit, false), Some("minecraft:redstone_torch[lit=true]".to_string()));
    }

    /// Negative control: if the signal changed back before the delayed tick
    /// ran (lit and NOT signaled — already steady), the scheduled tick must
    /// be a no-op, not force a flip.
    #[test]
    fn scheduled_tick_is_a_no_op_if_the_signal_already_reverted() {
        let lit = "minecraft:redstone_torch[lit=true]";
        assert_eq!(run_scheduled_tick(lit, false), None);
        let unlit = "minecraft:redstone_torch[lit=false]";
        assert_eq!(run_scheduled_tick(unlit, true), None);
    }
}
