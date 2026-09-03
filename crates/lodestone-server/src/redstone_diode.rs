//! Repeaters and comparators — both real diode-block subclasses,
//! sharing the input/side-input read `crate::redstone` already
//! provides ([`crate::redstone::input_signal`]/[`crate::redstone::alternate_signal`]).
//!
//! # Repeaters, transcribed from the real diode block
//!
//! The real repeater's delay and lock queries, transcribed as the rules
//! they implement: the tick delay is the block's own `DELAY` property times
//! two; it is locked iff its alternate (side) signal is above zero; and it
//! only ever considers *diode* side inputs, never a lever or torch.
//!
//! `DELAY` is `1..=4` (real default `1`), so the real tick delay is
//! `2, 4, 6, 8`. That diodes-only side input is why [`is_locked`] passes
//! `only_diodes = true` into [`crate::redstone::alternate_signal`] — a lever
//! or torch beside a repeater cannot lock it, only another diode's output
//! can.
//!
//! The real diode's scheduled-tick and neighbor-check hooks, transcribed as
//! the rules they implement:
//!
//! On the scheduled tick, if not locked: read the current powered state and
//! recompute whether it should turn on. If it is on and should not be,
//! unlock it to off. If it is off (regardless of what it should become),
//! turn it on — and if it should *not* actually be on, immediately
//! reschedule another tick at very-high priority, one delay period later.
//!
//! On a neighbor change, if not locked: read the current powered state and
//! recompute whether it should turn on. If those two disagree and no tick
//! is already pending, schedule one at the block's own delay — at
//! extremely-high priority if the diode should be prioritized (see below),
//! else very-high priority if currently on, else high priority.
//!
//! The scheduled-tick's "turn on regardless" branch is the "pulse
//! quantization" quirk: an off
//! repeater that receives its scheduled tick **always** turns on for one
//! full delay period, even if the input already dropped again by the
//! time the tick fires — that is what makes a repeater's output pulse never
//! shorter than its own delay. [`run_scheduled_tick`] is this, verbatim.
//!
//! The real diode's should-prioritize check, transcribed as the rule it
//! implements: look at the block directly behind this one (opposite its
//! facing); it should be prioritized iff that neighbour is also a diode and
//! that neighbour's own facing does not point back at this one.
//!
//! # Comparators, transcribed from the real comparator block
//!
//! The real comparator's delay, output-signal and should-turn-on queries,
//! transcribed as the rules they implement:
//!
//! The tick delay is a flat `2`. The output signal is: zero if the main
//! input signal is zero; otherwise zero if the alternate (side) signal
//! exceeds the input; otherwise, in subtract mode, the input minus the
//! alternate, or in compare mode, the input unchanged. It should turn on
//! when the input is non-zero and either strictly exceeds the alternate
//! signal, or equals it while in compare mode.
//!
//! The real comparator's neighbor-check, refresh and scheduled-tick hooks,
//! transcribed as the rules they implement:
//!
//! On a neighbor change, if no tick is already pending: recompute the
//! output signal, and if it differs from the currently banked one, or the
//! powered state disagrees with the should-turn-on check, schedule a tick
//! two ticks out — at high priority if the diode should be prioritized,
//! else normal priority.
//!
//! The refresh step: recompute the output signal and bank it on the block
//! entity; if it changed, or the comparator is in compare mode, correct the
//! powered state to match the should-turn-on check and notify the blocks in
//! front of it. The scheduled tick itself is just this refresh step.
//!
//! The `|| MODE == COMPARE` clause is a genuine, cite-able quirk: a
//! comparator in **compare** mode always re-notifies its front neighbour
//! when its scheduled tick fires, even if neither its output value nor its
//! on/off state actually changed since it was scheduled — **subtract**
//! mode only re-notifies when something really did change.
//! [`run_scheduled_comparator_tick`]'s own `!subtract` disjunct is this
//! clause, verbatim.
//!
//! # The named gap: container/analog-output reading
//!
//! The real comparator's input-signal query additionally reads a
//! two-away block's analog output signal (a hopper/chest's fill level)
//! or an item frame's rotation, when the immediate target is itself a
//! redstone conductor — the "comparator reads a hopper's contents"
//! behaviour is the trap most
//! likely to be skipped while looking done. It **is** skipped here, and
//! explicitly, not silently: this module's real input-signal-query equivalent is
//! [`crate::redstone::input_signal`], the same reduced function repeaters
//! use, which has no block-entity or entity query reachable from
//! `crates/lodestone-server/src/redstone.rs` (`crate::block_entities`
//! exists in this crate but nothing in this module's call chain threads a
//! `BlockEntityRegistry` through it — doing so is real, bounded future
//! work, not a design dead end: the real comparator block entity would only need a
//! banked-output-signal-shaped read from whatever container type sits at
//! `target_pos.relative(facing)`). Every comparator circuit in this
//! module's own tests reads only redstone-native inputs (dust, torches,
//! other diodes) — container-facing comparators are a real, named,
//! uncloseable-today gap, exactly like `crate::growth_tick`'s own
//! tree-growth gap.

use crate::neighbor_update::Direction;
use crate::redstone::{
    self, comparator_mode_subtract, comparator_output, diode_facing, diode_powered, direction_to_str, is_diode,
    repeater_delay_ticks, repeater_locked, WorldState, COMPARATOR, REPEATER,
};
use crate::scheduled_tick::TickPriority;
use lodestone_model::BlockPos;

/// Builds the canonical block-state string for a repeater.
#[must_use]
pub fn set_repeater(facing: Direction, delay_ticks: u32, locked: bool, powered: bool) -> String {
    format!(
        "{REPEATER}[facing={},delay={},locked={},powered={}]",
        direction_to_str(facing),
        delay_ticks.clamp(1, 4),
        locked,
        powered
    )
}

fn with_repeater_powered(state: &str, powered: bool) -> String {
    set_repeater(diode_facing(state), repeater_delay_ticks(state), repeater_locked(state), powered)
}

fn with_repeater_locked(state: &str, locked: bool) -> String {
    set_repeater(diode_facing(state), repeater_delay_ticks(state), locked, diode_powered(state))
}

/// Builds the canonical block-state string for a comparator — `output` is
/// this module's stand-in for the real comparator block entity's own banked
/// output signal, see this
/// module's own doc comment and [`crate::redstone::comparator_output`]'s.
#[must_use]
pub fn set_comparator(facing: Direction, subtract: bool, powered: bool, output: u8) -> String {
    format!(
        "{COMPARATOR}[facing={},mode={},powered={},output={}]",
        direction_to_str(facing),
        if subtract { "subtract" } else { "compare" },
        powered,
        output.min(15)
    )
}

/// The real delay query for a repeater — `DELAY * 2`, so `2, 4, 6, 8` for
/// `DELAY = 1..=4`.
#[must_use]
pub fn repeater_delay(state: &str) -> u32 {
    repeater_delay_ticks(state) * 2
}

/// The real diode's should-prioritize check — see this module's own doc
/// comment for the full derivation.
#[must_use]
pub fn should_prioritize<F>(lookup: &F, pos: BlockPos, facing: Direction) -> bool
where
    F: Fn(BlockPos) -> WorldState,
{
    let direction = facing.opposite();
    let opposite_state = lookup(direction.relative(pos));
    is_diode(&opposite_state) && diode_facing(&opposite_state) != direction
}

/// The real repeater's is-locked check — `alternate_signal` with `only_diodes = true`.
#[must_use]
pub fn is_locked<F>(lookup: &F, pos: BlockPos, facing: Direction) -> bool
where
    F: Fn(BlockPos) -> WorldState,
{
    redstone::alternate_signal(lookup, pos, facing, true) > 0
}

/// A repeater has no analog side-channel — the real should-turn-on check
/// reduces to
/// "input signal above zero" (the base diode's own check,
/// unmodified by the real repeater block).
#[must_use]
pub fn repeater_should_turn_on<F>(lookup: &F, pos: BlockPos, facing: Direction) -> bool
where
    F: Fn(BlockPos) -> WorldState,
{
    redstone::input_signal(lookup, pos, facing) > 0
}

/// `true` iff a delayed recheck should be scheduled — the powered state
/// disagrees with the should-turn-on check,
/// only evaluated when not locked (a locked repeater ignores front-input
/// changes entirely, per the real diode's own neighbor-check hook's
/// not-locked guard).
#[must_use]
pub fn should_schedule_repeater_check(state: &str, should_turn_on: bool) -> bool {
    !repeater_locked(state) && diode_powered(state) != should_turn_on
}

/// The real diode's neighbor-check hook's priority selection.
#[must_use]
pub fn repeater_schedule_priority<F>(lookup: &F, pos: BlockPos, facing: Direction, currently_on: bool) -> TickPriority
where
    F: Fn(BlockPos) -> WorldState,
{
    if should_prioritize(lookup, pos, facing) {
        TickPriority::ExtremelyHigh
    } else if currently_on {
        TickPriority::VeryHigh
    } else {
        TickPriority::High
    }
}

/// The outcome of one repeater [`run_scheduled_tick`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepeaterTickOutcome {
    /// Locked: the real diode's scheduled-tick body is skipped entirely.
    Locked,
    /// Already in the state its own input demands — a real no-op (only
    /// reachable if the input reverted between scheduling and running).
    NoChange,
    /// Was on, input dropped: turns off immediately, no reschedule.
    TurnedOff(String),
    /// Was off: turns on unconditionally for one full delay period.
    /// `reschedule` is `true` when the input had *already* dropped again by
    /// the time this tick ran (the should-turn-on check now says no) — the "pulse quantization"
    /// quirk cited in this module's own doc comment.
    TurnedOn { new_state: String, reschedule: bool },
}

/// The real diode's scheduled-tick hook for a repeater — see this module's
/// own doc comment for the full derivation.
#[must_use]
pub fn run_scheduled_tick(state: &str, should_turn_on: bool) -> RepeaterTickOutcome {
    if repeater_locked(state) {
        return RepeaterTickOutcome::Locked;
    }
    let on = diode_powered(state);
    if on && !should_turn_on {
        RepeaterTickOutcome::TurnedOff(with_repeater_powered(state, false))
    } else if !on {
        RepeaterTickOutcome::TurnedOn {
            new_state: with_repeater_powered(state, true),
            reschedule: !should_turn_on,
        }
    } else {
        RepeaterTickOutcome::NoChange
    }
}

/// Recomputes `LOCKED` immediately (the real repeater's own shape-update-triggered
/// recompute) — `Some(new_state)` iff the value
/// actually changed. Deliberately unconditional on which neighbour changed
/// (the real engine gates this on the *axis* of the changed neighbour being
/// perpendicular to `FACING`; this crate has no cheap way to know which
/// specific neighbour triggered a given notification at this call site, so
/// it is recomputed on every notification instead — a named, harmless
/// over-approximation: recomputing more often than vanilla never produces a
/// wrong *value*, only a few redundant recomputes that land on the same
/// answer).
#[must_use]
pub fn recompute_locked<F>(lookup: &F, pos: BlockPos, state: &str) -> Option<String>
where
    F: Fn(BlockPos) -> WorldState,
{
    let facing = diode_facing(state);
    let new_locked = is_locked(lookup, pos, facing);
    if new_locked == repeater_locked(state) {
        None
    } else {
        Some(with_repeater_locked(state, new_locked))
    }
}

// ---------------------------------------------------------------------------
// Comparators
// ---------------------------------------------------------------------------

/// The real comparator's calculate-output-signal query — see this module's
/// own doc comment for the full derivation.
#[must_use]
pub fn calculate_comparator_output(input: u8, side: u8, subtract: bool) -> u8 {
    if input == 0 {
        0
    } else if side > input {
        0
    } else if subtract {
        input - side
    } else {
        input
    }
}

/// The real comparator's should-turn-on check.
#[must_use]
pub fn comparator_should_turn_on(input: u8, side: u8, subtract: bool) -> bool {
    if input == 0 {
        false
    } else if input > side {
        true
    } else {
        input == side && !subtract
    }
}

/// The real comparator's neighbor-check hook's scheduling condition.
#[must_use]
pub fn should_schedule_comparator_check(state: &str, input: u8, side: u8) -> bool {
    let subtract = comparator_mode_subtract(state);
    let output = calculate_comparator_output(input, side, subtract);
    let should_be_on = comparator_should_turn_on(input, side, subtract);
    output != comparator_output(state) || diode_powered(state) != should_be_on
}

/// The real comparator's neighbor-check hook's priority selection (`HIGH`/`NORMAL`,
/// distinct from the repeater's own three-way choice above).
#[must_use]
pub fn comparator_schedule_priority<F>(lookup: &F, pos: BlockPos, facing: Direction) -> TickPriority
where
    F: Fn(BlockPos) -> WorldState,
{
    if should_prioritize(lookup, pos, facing) {
        TickPriority::High
    } else {
        TickPriority::Normal
    }
}

/// The real comparator's scheduled-tick hook and its refresh-output-state
/// step — see this module's own doc
/// comment for why the `!subtract` disjunct (compare mode always cascades)
/// is real and derived, not an approximation.
#[must_use]
pub fn run_scheduled_comparator_tick(state: &str, input: u8, side: u8) -> Option<String> {
    let subtract = comparator_mode_subtract(state);
    let output = calculate_comparator_output(input, side, subtract);
    let should_be_on = comparator_should_turn_on(input, side, subtract);
    let old_output = comparator_output(state);
    let old_on = diode_powered(state);
    if old_output != output || old_on != should_be_on || !subtract {
        Some(set_comparator(diode_facing(state), subtract, should_be_on, output))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

        fn world(entries: &[(BlockPos, &str)]) -> impl Fn(BlockPos) -> WorldState + use<> {
        let entries: Vec<(BlockPos, WorldState)> = entries.iter().map(|(p, s)| (*p, WorldState::from(*s))).collect();
        move |p: BlockPos| {
            entries
                .iter()
                .find(|(pos, _)| *pos == p)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(crate::chunk::air_state_arc)
        }
    }

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn repeater_delay_scales_with_the_delay_property() {
        assert_eq!(repeater_delay(&set_repeater(Direction::North, 1, false, false)), 2);
        assert_eq!(repeater_delay(&set_repeater(Direction::North, 4, false, false)), 8);
    }

    #[test]
    fn repeater_should_turn_on_reads_a_lit_torch_facing_into_it() {
        let origin = pos(0, 0, 0);
        let torch_pos = Direction::East.relative(origin);
        let w = world(&[(torch_pos, "minecraft:redstone_torch[lit=true]")]);
        assert!(repeater_should_turn_on(&w, origin, Direction::East));
        assert!(!repeater_should_turn_on(&world(&[]), origin, Direction::East));
    }

    /// Lock predicate end to end: a powered repeater facing NORTH sitting
    /// to the east (a `clockwise` side input for a repeater facing EAST)
    /// locks the repeater; a lit TORCH in the same spot does not (torches
    /// aren't diodes — `sideInputDiodesOnly = true`).
    #[test]
    fn repeater_lock_only_responds_to_a_diode_side_input() {
        let origin = pos(0, 0, 0);
        // Repeater facing EAST: clockwise(East) = South, so the lock check
        // reads the neighbour at `South.relative(origin)` with
        // `direction = South` (the direction travelled FROM origin TO that
        // neighbour) — `direct_signal` for a diode fires when its own
        // `FACING` equals that travelled direction (`weak_signal`'s own doc
        // comment states the convention), so the side repeater needs
        // `FACING = south`, not `west` (an earlier version of this fixture's
        // mistake — it read `0` instead of the predicted `15`).
        let side_pos = Direction::South.relative(origin);
        let diode_side = world(&[(side_pos, "minecraft:repeater[facing=south,delay=1,locked=false,powered=true]")]);
        assert!(is_locked(&diode_side, origin, Direction::East));

        let torch_side = world(&[(side_pos, "minecraft:redstone_torch[lit=true]")]);
        assert!(!is_locked(&torch_side, origin, Direction::East), "control failed: a torch must not lock a repeater");
    }

    #[test]
    fn schedule_check_fires_only_on_mismatch_and_never_while_locked() {
        let off_locked = "minecraft:repeater[facing=north,delay=1,locked=true,powered=false]";
        assert!(!should_schedule_repeater_check(off_locked, true), "locked repeaters never schedule, even on a real mismatch");

        let off_unlocked = "minecraft:repeater[facing=north,delay=1,locked=false,powered=false]";
        assert!(should_schedule_repeater_check(off_unlocked, true));
        assert!(!should_schedule_repeater_check(off_unlocked, false), "already steady: no recheck");
    }

    #[test]
    fn scheduled_tick_turns_off_an_on_repeater_when_input_drops() {
        let on = "minecraft:repeater[facing=north,delay=1,locked=false,powered=true]";
        assert_eq!(
            run_scheduled_tick(on, false),
            RepeaterTickOutcome::TurnedOff("minecraft:repeater[facing=north,delay=1,locked=false,powered=false]".to_string())
        );
    }

    /// The pulse-quantization quirk: an off repeater whose tick fires while
    /// the input has ALREADY dropped again still turns on, AND reports it
    /// must reschedule — proving the repeater commits to one full delay
    /// period regardless of the input's state at the instant the tick runs.
    #[test]
    fn scheduled_tick_always_turns_on_and_flags_reschedule_when_input_already_dropped() {
        let off = "minecraft:repeater[facing=north,delay=1,locked=false,powered=false]";
        match run_scheduled_tick(off, false) {
            RepeaterTickOutcome::TurnedOn { new_state, reschedule } => {
                assert_eq!(new_state, "minecraft:repeater[facing=north,delay=1,locked=false,powered=true]");
                assert!(reschedule, "must flag a reschedule so the pulse still ends after exactly one delay period");
            }
            other => panic!("expected TurnedOn, got {other:?}"),
        }
    }

    /// Negative control: an off repeater whose input is STILL high when the
    /// tick fires turns on with no reschedule flagged — the steady-state
    /// case, proving `reschedule` actually discriminates.
    #[test]
    fn scheduled_tick_turns_on_with_no_reschedule_when_input_is_still_high() {
        let off = "minecraft:repeater[facing=north,delay=1,locked=false,powered=false]";
        match run_scheduled_tick(off, true) {
            RepeaterTickOutcome::TurnedOn { reschedule, .. } => assert!(!reschedule),
            other => panic!("expected TurnedOn, got {other:?}"),
        }
    }

    #[test]
    fn locked_repeater_never_changes_state_on_a_scheduled_tick() {
        let locked = "minecraft:repeater[facing=north,delay=1,locked=true,powered=false]";
        assert_eq!(run_scheduled_tick(locked, true), RepeaterTickOutcome::Locked);
    }

    // -- Comparators --------------------------------------------------------

    #[test]
    fn compare_mode_passes_input_through_when_side_does_not_exceed_it() {
        assert_eq!(calculate_comparator_output(10, 4, false), 10);
        assert_eq!(calculate_comparator_output(10, 10, false), 10);
    }

    #[test]
    fn subtract_mode_subtracts_the_side_input() {
        assert_eq!(calculate_comparator_output(10, 4, true), 6);
        assert_eq!(calculate_comparator_output(10, 10, true), 0);
    }

    #[test]
    fn a_side_input_stronger_than_the_main_input_blocks_output_in_both_modes() {
        assert_eq!(calculate_comparator_output(4, 10, false), 0);
        assert_eq!(calculate_comparator_output(4, 10, true), 0);
    }

    #[test]
    fn zero_input_never_produces_output_regardless_of_side() {
        assert_eq!(calculate_comparator_output(0, 0, false), 0);
        assert_eq!(calculate_comparator_output(0, 5, true), 0);
    }

    /// The real should-turn-on check's compare/subtract split at the tie (`input == side`):
    /// compare mode still turns on, subtract mode does not (its own output
    /// would be exactly zero).
    #[test]
    fn should_turn_on_ties_favour_compare_mode_only() {
        assert!(comparator_should_turn_on(8, 8, false), "compare mode: a tie still turns on");
        assert!(!comparator_should_turn_on(8, 8, true), "subtract mode: a tie means zero output, so off");
    }

    #[test]
    fn should_turn_on_is_false_whenever_input_is_zero() {
        assert!(!comparator_should_turn_on(0, 0, false));
        assert!(!comparator_should_turn_on(0, 0, true));
    }

    #[test]
    fn schedule_check_fires_when_the_output_would_change() {
        // Currently stored output=6 (subtract, 10-4); input now reads 10 and
        // side reads 5, so the fresh output would be 5 — a real change.
        let state = "minecraft:comparator[facing=north,mode=subtract,powered=true,output=6]";
        assert!(should_schedule_comparator_check(state, 10, 5));
        // Negative control: identical inputs to what produced the stored
        // state must NOT reschedule.
        assert!(!should_schedule_comparator_check(state, 10, 4));
    }

    /// The compare-mode quirk end to end: a scheduled tick that computes the
    /// exact same output and on/off state STILL returns `Some` (cascades)
    /// in compare mode, but `None` (no cascade) in subtract mode for the
    /// identical inputs — proving the `!subtract` disjunct is load-bearing,
    /// not decorative.
    #[test]
    fn compare_mode_always_cascades_even_with_no_change_subtract_mode_does_not() {
        let compare_state = "minecraft:comparator[facing=north,mode=compare,powered=true,output=10]";
        assert!(
            run_scheduled_comparator_tick(compare_state, 10, 0).is_some(),
            "compare mode must cascade even when nothing changed"
        );

        let subtract_state = "minecraft:comparator[facing=north,mode=subtract,powered=true,output=10]";
        assert!(
            run_scheduled_comparator_tick(subtract_state, 10, 0).is_none(),
            "control failed: subtract mode must NOT cascade when nothing changed"
        );
    }

    #[test]
    fn scheduled_tick_updates_the_stored_output_when_it_changed() {
        let state = "minecraft:comparator[facing=east,mode=subtract,powered=true,output=6]";
        let new_state = run_scheduled_comparator_tick(state, 10, 5).expect("output changed from 6 to 5");
        assert_eq!(new_state, "minecraft:comparator[facing=east,mode=subtract,powered=true,output=5]");
    }
}
