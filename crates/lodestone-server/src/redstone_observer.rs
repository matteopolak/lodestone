//! Observers (issue #317): a 1-tick-wide pulse out the back face whenever
//! the block the observer faces changes.
//!
//! # Cited directly
//!
//! `ObserverBlock.updateShape`/`startSignal`/`tick` (`ObserverBlock.java:62-84,50-60`):
//!
//! ```text
//! protected BlockState updateShape(..., final Direction directionToNeighbour, ...) {
//!    if (state.getValue(FACING) == directionToNeighbour && !state.getValue(POWERED)) {
//!       this.startSignal(level, ticks, pos);
//!    }
//!    return super.updateShape(...);
//! }
//! private void startSignal(final LevelReader level, final ScheduledTickAccess ticks, final BlockPos pos) {
//!    if (!level.isClientSide() && !ticks.getBlockTicks().hasScheduledTick(pos, this)) {
//!       ticks.scheduleTick(pos, this, 2);
//!    }
//! }
//! protected void tick(...) {
//!    if (state.getValue(POWERED)) {
//!       level.setBlock(pos, state.setValue(POWERED, false), 2);
//!    } else {
//!       level.setBlock(pos, state.setValue(POWERED, true), 2);
//!       level.scheduleTick(pos, this, 2);
//!    }
//!    this.updateNeighborsInFront(level, pos, state);
//! }
//! ```
//!
//! `updateShape`'s `directionToNeighbour` is "the direction travelled *from*
//! the observer *to* the neighbour that changed" — the opposite of
//! `crate::neighbor_update::Notification::from` (which this crate's own
//! doc comment defines as "the direction travelled from the *causing*
//! block into the notified position"). So the observer's own trigger
//! condition, restated in this crate's convention: a [`Notification`] at the
//! observer's own position triggers a check iff
//! `notification.from == facing.opposite()` — [`watch_direction`] below,
//! used at the call site rather than duplicating the derivation there.
//!
//! `tick()` is an unconditional toggle: powered -> unpowered (no
//! reschedule), unpowered -> powered (**with** a reschedule so the pulse is
//! always exactly 2 ticks wide, never held high). [`run_scheduled_tick`] is
//! this, verbatim.
//!
//! # Named gaps
//!
//! **Placement/removal-specific behaviour is not modeled.** `onPlace`
//! (`:114-123`, forcing a freshly-placed observer unpowered without a pulse
//! if it happened to load already-`POWERED`) and
//! `affectNeighborsAfterRemoval` (`:125-130`, firing a final pulse on
//! removal if one was mid-flight) both exist for save/load and
//! player-placement edge cases. This crate has no player-driven block
//! placement pipeline for any redstone component yet (#314's own "what
//! exists" survey: nothing) — the same "no producer to exercise it yet"
//! reasoning `crate::redstone_torch`'s own module doc gives for skipping the
//! anti-oscillation guard.
//!
//! **The trigger surface is narrower than vanilla's `updateShape`.**
//! Vanilla's hook fires on *any* state change at the watched position,
//! including a piston pushing a block there or a block-entity data change
//! vanilla specifically excludes (issue #317's own brief names this
//! precisely). This crate only ever issues a [`Notification`] from the
//! handful of mutation families `crate::random_tick`'s reaction dispatch
//! already covers (grass/dirt, crop/sapling/leaf, gravity, dust, torches,
//! diodes) — the same "narrower, but real" trigger-surface deviation
//! `crate::gravity_tick`'s own module doc already accepts for its trigger.

use crate::neighbor_update::Direction;
use crate::redstone::{observer_facing, observer_powered, OBSERVER};

/// Builds the canonical block-state string for an observer.
#[must_use]
pub fn set_observer(facing: Direction, powered: bool) -> String {
    format!("{OBSERVER}[facing={},powered={}]", crate::redstone::direction_to_str(facing), powered)
}

/// The [`crate::neighbor_update::Notification::from`] value that identifies
/// "the block this observer watches just changed" — see this module's own
/// doc comment for the direction-convention derivation.
#[must_use]
pub fn watch_direction(state: &str) -> Direction {
    observer_facing(state).opposite()
}

/// `true` iff a pulse should be scheduled right now: not already powered
/// (`updateShape`'s own `!state.getValue(POWERED)` guard — the
/// "not already scheduled" half of `startSignal`'s guard is provided by
/// [`crate::scheduled_tick::ScheduledTickQueue::has_scheduled`] at the call
/// site, matching `ScheduledTickAccess.getBlockTicks().hasScheduledTick`).
#[must_use]
pub fn should_start_signal(state: &str) -> bool {
    !observer_powered(state)
}

/// `ObserverBlock.tick` — the unconditional toggle. Returns the new state
/// and whether a follow-up tick must be scheduled (only when turning ON, so
/// the pulse is always exactly one scheduled-tick period wide).
#[must_use]
pub fn run_scheduled_tick(state: &str) -> (String, bool) {
    let facing = observer_facing(state);
    if observer_powered(state) {
        (set_observer(facing, false), false)
    } else {
        (set_observer(facing, true), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_direction_is_the_opposite_of_facing() {
        assert_eq!(watch_direction("minecraft:observer[facing=north,powered=false]"), Direction::South);
        assert_eq!(watch_direction("minecraft:observer[facing=east,powered=false]"), Direction::West);
    }

    #[test]
    fn should_start_signal_is_false_while_already_powered() {
        assert!(should_start_signal("minecraft:observer[facing=north,powered=false]"));
        assert!(!should_start_signal("minecraft:observer[facing=north,powered=true]"));
    }

    #[test]
    fn scheduled_tick_turns_on_and_requests_a_reschedule() {
        let (new_state, reschedule) = run_scheduled_tick("minecraft:observer[facing=north,powered=false]");
        assert_eq!(new_state, "minecraft:observer[facing=north,powered=true]");
        assert!(reschedule, "the ON half of the pulse must schedule its own OFF half");
    }

    #[test]
    fn scheduled_tick_turns_off_with_no_further_reschedule() {
        let (new_state, reschedule) = run_scheduled_tick("minecraft:observer[facing=north,powered=true]");
        assert_eq!(new_state, "minecraft:observer[facing=north,powered=false]");
        assert!(!reschedule, "control failed: the OFF half must not reschedule itself, or the pulse would never end");
    }

    /// End-to-end pulse-width check: starting unpowered, two scheduled ticks
    /// must return to unpowered — a magnitude check (exactly two ticks:
    /// on-then-off), not merely "it changed".
    #[test]
    fn a_full_pulse_is_exactly_two_scheduled_ticks_wide() {
        let start = "minecraft:observer[facing=north,powered=false]";
        let (after_first, reschedule_first) = run_scheduled_tick(start);
        assert!(reschedule_first);
        let (after_second, reschedule_second) = run_scheduled_tick(&after_first);
        assert!(!reschedule_second);
        assert_eq!(after_second, start, "after exactly two ticks the observer must be back to unpowered");
    }
}
