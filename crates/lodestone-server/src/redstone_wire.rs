//! Redstone dust (the wire half of the redstone family): power-level computation —
//! `crate::redstone`'s query layer, composed exactly the way
//! `RedStoneWireBlock`/`DefaultRedstoneWireEvaluator` compose it.
//!
//! # Cited directly
//!
//! `DefaultRedstoneWireEvaluator.calculateTargetStrength`:
//!
//! ```text
//! private int calculateTargetStrength(final Level level, final BlockPos pos) {
//!    int blockSignal = this.getBlockSignal(level, pos);
//!    return blockSignal == 15 ? blockSignal : Math.max(blockSignal, this.getIncomingWireSignal(level, pos));
//! }
//! ```
//!
//! `RedStoneWireBlock.getBlockSignal` is `getBestNeighborSignal`
//! with the wire's own `shouldSignal` flag held false for the call's
//! duration — [`crate::redstone::best_neighbor_signal`]'s `ignore_wire`
//! parameter, `true` here.
//!
//! `RedstoneWireEvaluator.getIncomingWireSignal`:
//!
//! ```text
//! protected int getIncomingWireSignal(final Level level, final BlockPos pos) {
//!    int wireSignal = 0;
//!    for (Direction direction : Direction.Plane.HORIZONTAL) {
//!       BlockPos neighborPos = pos.relative(direction);
//!       BlockState neighborState = level.getBlockState(neighborPos);
//!       wireSignal = Math.max(wireSignal, this.getWireSignal(neighborPos, neighborState));
//!       BlockPos abovePos = pos.above();
//!       if (neighborState.isRedstoneConductor(level, neighborPos) && !level.getBlockState(abovePos).isRedstoneConductor(level, abovePos)) {
//!          BlockPos aboveNeighborPos = neighborPos.above();
//!          wireSignal = Math.max(wireSignal, this.getWireSignal(aboveNeighborPos, level.getBlockState(aboveNeighborPos)));
//!       } else if (!neighborState.isRedstoneConductor(level, neighborPos)) {
//!          BlockPos belowNeighborPos = neighborPos.below();
//!          wireSignal = Math.max(wireSignal, this.getWireSignal(belowNeighborPos, level.getBlockState(belowNeighborPos)));
//!       }
//!    }
//!    return Math.max(0, wireSignal - 1);
//! }
//! ```
//!
//! This is the "wire connects diagonally over a one-block step" mechanic:
//! a wire can read another wire one block *up* across a conductor step (the
//! `abovePos`/`aboveNeighborPos` branch) or one block *down* into a pit (the
//! `belowNeighborPos` branch), each decaying by the same `-1` a same-height
//! neighbour would. [`incoming_wire_signal`] below is this, verbatim,
//! against [`crate::redstone`]'s query layer.
//!
//! # What this module deliberately does not model
//!
//! Vanilla's dust also tracks four `RedstoneSide` connection properties
//! (`RedStoneWireBlock.NORTH`/`EAST`/`SOUTH`/`WEST`, for rendering the wire's visual shape)
//! and can be toggled between a "cross" and
//! a "dot" render by right-clicking (`RedStoneWireBlock.useWithoutItem`). Neither
//! is modeled: this crate's `ChunkColumn` block-state strings carry no shape
//! information anywhere (`crate::chunk`'s own module doc — the render side
//! derives shape from the state string in `lodestone-render`, off-limits to
//! this task), and this module's own scope is signal propagation, not the visual
//! connection graph. A dust block here is always logically connected on all
//! four horizontal sides — the one thing that changes is [`wire_power`]'s
//! `power=N` property.

use crate::neighbor_update::Direction;
use crate::redstone::{self, is_redstone_conductor, wire_power};
use lodestone_model::BlockPos;

pub use crate::redstone::WIRE;

/// Builds the canonical block-state string for dust at `power` — see this
/// module's own doc comment for why no connection properties are encoded.
#[must_use]
pub fn set_power(power: u8) -> String {
    format!("{WIRE}[power={}]", power.min(15))
}

/// `RedstoneWireEvaluator.getIncomingWireSignal` — see this module's own doc
/// comment for the full citation.
#[must_use]
pub fn incoming_wire_signal<F>(lookup: &F, pos: BlockPos) -> u8
where
    F: Fn(BlockPos) -> String,
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

/// `DefaultRedstoneWireEvaluator.calculateTargetStrength` — see this
/// module's own doc comment for the full citation.
#[must_use]
pub fn calculate_target_strength<F>(lookup: &F, pos: BlockPos) -> u8
where
    F: Fn(BlockPos) -> String,
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

    /// A line of dust decays by exactly 1 per block, cited directly from
    /// `getIncomingWireSignal`'s `wireSignal - 1` — but decay only starts
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
        // `getIncomingWireSignal`'s own `-1`, which applies starting from
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
    /// `getBlockSignal` never counts a neighbouring wire as a source, and
    /// `getIncomingWireSignal` reads the OTHER wire's CURRENT (already-zero)
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
    /// cited from `getIncomingWireSignal`'s `abovePos`/`aboveNeighborPos`
    /// branch. Layout: `lowWire` at y=0; a stone block at (east, y=0); a
    /// second wire at (east, y=1), i.e. resting on top of that stone, powered
    /// at 15 by an adjacent torch (not modeled here — its power is asserted
    /// directly as a fixture). The block directly above `lowWire` (y=1) must
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
    /// `belowNeighborPos` branch.
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
