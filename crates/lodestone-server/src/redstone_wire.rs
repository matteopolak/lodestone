//! Redstone dust (the wire half of the redstone family): power-level computation —
//! `crate::redstone`'s query layer, composed exactly the way
//! the real wire block and its default evaluator compose it.
//!
//! # Transcribed from the real default redstone-wire evaluator
//!
//! The real calculate-target-strength query, transcribed as the rule it
//! implements: read this position's own block signal; if that is already
//! `15`, it wins outright, otherwise the target strength is the greater of
//! the block signal and the incoming wire signal.
//!
//! The real wire block's block-signal query is the best-neighbor-signal
//! query
//! with the wire's own should-signal flag held false for the call's
//! duration — [`crate::redstone::best_neighbor_signal`]'s `ignore_wire`
//! parameter, `true` here.
//!
//! The real incoming-wire-signal query, transcribed as the rule it
//! implements: for each of the four horizontal directions, take the wire
//! signal at that neighbour; additionally, if the neighbour is a redstone
//! conductor and the block directly above *this* position is not, also take
//! the wire signal one block above that neighbour; otherwise, if the
//! neighbour is *not* a redstone conductor, also take the wire signal one
//! block below that neighbour. The result is the greatest of every signal
//! considered this way, minus one, floored at zero.
//!
//! This is the "wire connects diagonally over a one-block step" mechanic:
//! a wire can read another wire one block *up* across a conductor step (the
//! above-neighbour branch) or one block *down* into a pit (the
//! below-neighbour branch), each decaying by the same `-1` a same-height
//! neighbour would. [`incoming_wire_signal`] below is this, verbatim,
//! against [`crate::redstone`]'s query layer.
//!
//! # What this module deliberately does not model
//!
//! The real dust also tracks four connection-side properties
//! (for rendering the wire's visual shape)
//! and can be toggled between a "cross" and
//! a "dot" render by right-clicking. Neither
//! is modeled: this crate's `ChunkColumn` block-state strings carry no shape
//! information anywhere (`crate::chunk`'s own module doc — the render side
//! derives shape from the state string in `lodestone-render`, off-limits to
//! this task), and this module's own scope is signal propagation, not the visual
//! connection graph. A dust block here is always logically connected on all
//! four horizontal sides — the one thing that changes is [`wire_power`]'s
//! `power=N` property.

use crate::neighbor_update::Direction;
use crate::redstone::{self, is_redstone_conductor, wire_power, WorldState};
use lodestone_model::BlockPos;

pub use crate::redstone::WIRE;

/// Builds the canonical block-state string for dust at `power` — see this
/// module's own doc comment for why no connection properties are encoded.
#[must_use]
pub fn set_power(power: u8) -> String {
    format!("{WIRE}[power={}]", power.min(15))
}

/// The real incoming-wire-signal query — see this module's own doc
/// comment for the full derivation.
#[must_use]
pub fn incoming_wire_signal<F>(lookup: &F, pos: BlockPos) -> u8
where
    F: Fn(BlockPos) -> WorldState,
{
    let above_state = lookup(Direction::Up.relative(pos));
    let above_is_conductor = is_redstone_conductor(&above_state);
    let mut wire_signal: u8 = 0;

    for direction in [Direction::North, Direction::South, Direction::West, Direction::East] {
        let neighbor_pos = direction.relative(pos);
        let neighbor_state = lookup(neighbor_pos);
        wire_signal = wire_signal.max(wire_power(&neighbor_state));

        if is_redstone_conductor(&neighbor_state) && !above_is_conductor {
            let above_neighbor = lookup(Direction::Up.relative(neighbor_pos));
            wire_signal = wire_signal.max(wire_power(&above_neighbor));
        } else if !is_redstone_conductor(&neighbor_state) {
            let below_neighbor = lookup(Direction::Down.relative(neighbor_pos));
            wire_signal = wire_signal.max(wire_power(&below_neighbor));
        }
    }

    wire_signal.saturating_sub(1)
}

/// The real calculate-target-strength query — see this
/// module's own doc comment for the full derivation.
#[must_use]
pub fn calculate_target_strength<F>(lookup: &F, pos: BlockPos) -> u8
where
    F: Fn(BlockPos) -> WorldState,
{
    let block_signal = redstone::best_neighbor_signal(lookup, pos, true);
    if block_signal >= 15 {
        return 15;
    }
    block_signal.max(incoming_wire_signal(lookup, pos))
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
    fn set_power_round_trips_through_wire_power() {
        for p in 0..=15u8 {
            let state = set_power(p);
            assert_eq!(wire_power(&state), p);
        }
    }

    #[test]
    fn set_power_clamps_above_fifteen() {
        assert_eq!(set_power(255), "minecraft:redstone_wire[power=15]");
    }

    /// A lit torch adjacent to a wire gives it target strength 15 directly
    /// (the `block_signal == 15` short-circuit) — a magnitude check, not
    /// just "nonzero".
    #[test]
    fn a_wire_next_to_a_lit_torch_reads_full_strength() {
        let wire_pos = pos(0, 0, 0);
        let torch_pos = Direction::West.relative(wire_pos);
        let w = world(&[(torch_pos, "minecraft:redstone_torch[lit=true]")]);
        assert_eq!(calculate_target_strength(&w, wire_pos), 15);
    }

    /// A line of dust decays by exactly 1 per block, transcribed directly
    /// from the real incoming-wire-signal query's final `- 1` — but decay only starts
    /// counting from the first wire that is NOT itself touching the source:
    /// `torch(15) - dustA(15, direct neighbour) - dustB(14, one wire away)`.
    #[test]
    fn a_chain_of_dust_decays_by_exactly_one_per_block() {
        let dust_a = pos(1, 0, 0);
        let dust_b = pos(2, 0, 0);
        let torch_pos = pos(0, 0, 0);
        // `dust_a` is a DIRECT neighbour of the torch, so `best_neighbor_signal`
        // finds it via the plain (non-decaying) `block_signal` path and it
        // settles at full strength (15), not 14 — decay only enters through
        // the real incoming-wire-signal query's own `-1`, which applies starting from
        // `dust_b` reading `dust_a`'s *stored* power. An earlier version of
        // this test predicted `dust_a = 14` by mistake (treating the torch's
        // own adjacency as if it already decayed once) and failed with `15`
        // where it expected `14` — the code was right, the hand-derived
        // prediction was not.
        let w = world(&[
            (torch_pos, "minecraft:redstone_torch[lit=true]"),
            (dust_a, &set_power(15)), // already-settled state, as if placed and updated once
        ]);
        assert_eq!(calculate_target_strength(&w, dust_a), 15, "dust_a is adjacent to the torch: full strength, no decay yet");
        assert_eq!(calculate_target_strength(&w, dust_b), 14, "dust_b: dust_a(15) - 1 = 14");
    }

    /// Negative control: a lone dust block with nothing feeding it settles
    /// to zero, not some nonzero default.
    #[test]
    fn an_unpowered_dust_block_settles_to_zero() {
        let w = world(&[]);
        assert_eq!(calculate_target_strength(&w, pos(0, 0, 0)), 0);
    }

    /// `ignore_wire`'s real purpose, proven end to end: two dust blocks
    /// side by side with nothing else feeding either must NOT bootstrap each
    /// other's power from nothing — both must settle at 0, since
    /// the real block-signal query never counts a neighbouring wire as a source, and
    /// the real incoming-wire-signal query reads the OTHER wire's CURRENT (already-zero)
    /// power, decayed by one, floored at zero (not negative).
    #[test]
    fn two_adjacent_dust_blocks_with_no_source_do_not_bootstrap_each_other() {
        let a = pos(0, 0, 0);
        let b = pos(1, 0, 0);
        let w = world(&[(a, &set_power(0)), (b, &set_power(0))]);
        assert_eq!(calculate_target_strength(&w, a), 0);
        assert_eq!(calculate_target_strength(&w, b), 0);
    }

    /// The "step up over a conductor" diagonal read: a wire on the ground
    /// reads a wire one block *higher*, across a conductor, decayed by one —
    /// transcribed from the real incoming-wire-signal query's above-neighbour
    /// branch. Layout: the low wire at y=0; a stone block at (east, y=0); a
    /// second wire at (east, y=1), i.e. resting on top of that stone, powered
    /// at 15 by an adjacent torch (not modeled here — its power is asserted
    /// directly as a fixture). The block directly above the low wire (y=1) must
    /// be non-conductor (air) for the branch to fire.
    #[test]
    fn a_wire_reads_a_higher_wire_across_a_one_block_conductor_step() {
        let low_wire = pos(0, 0, 0);
        let step_conductor = pos(1, 0, 0);
        let high_wire = pos(1, 1, 0);
        let w = world(&[(step_conductor, "minecraft:stone"), (high_wire, &set_power(15))]);
        assert_eq!(
            calculate_target_strength(&w, low_wire),
            14,
            "expected the higher wire's power (15) decayed by one across the step"
        );
    }

    /// The mirror case: a wire reads a lower wire one block *down*, through
    /// open air (no conductor at the neighbour position) — the
    /// below-neighbour branch.
    #[test]
    fn a_wire_reads_a_lower_wire_through_open_air() {
        let high_wire = pos(0, 5, 0);
        let low_wire = pos(1, 4, 0);
        // The neighbour at (1,5,0) is air (not a conductor), so the "look
        // one below" branch fires.
        let w = world(&[(low_wire, &set_power(15))]);
        assert_eq!(calculate_target_strength(&w, high_wire), 14);
    }
}
