//! Whether a mob's checked block placement may land in its cell.

use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_entity::ai::turtle_egg::is_air;

/// A checked placement lands when the cell still holds what the mob saw, no
/// creature stands in it, a full solid block that is not bedrock lies below,
/// and the placed state `survives` there.
#[must_use]
pub(crate) fn placement_site_ok(
    holds: bool,
    creature_in_cell: bool,
    below: Option<StateId>,
    set: Option<StateId>,
    survives: impl FnOnce(StateId) -> bool,
) -> bool {
    holds
        && !creature_in_cell
        && below.is_some_and(|b| !is_air(b) && b.block() != Block::Bedrock && crate::fluid::is_full_cube(b))
        && set.is_none_or(survives)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> StateId {
        StateId::from_state_str(name).expect("known block state")
    }

    /// Every conjunct is the sole reason for a refusal in its own case, and
    /// the all-good case is accepted (the control for each refusal).
    #[test]
    fn a_placement_needs_every_condition_and_each_alone_refuses_it() {
        let stone = state("minecraft:stone");
        let egg = state("minecraft:turtle_egg");
        let ok = |holds, creature, below, set, survives: bool| placement_site_ok(holds, creature, below, set, |_| survives);
        assert!(ok(true, false, Some(stone), Some(egg), true), "the control placement lands");
        assert!(!ok(false, false, Some(stone), Some(egg), true), "the cell changed since the mob looked");
        assert!(!ok(true, true, Some(stone), Some(egg), true), "a creature stands in the cell");
        assert!(!ok(true, false, None, Some(egg), true), "the cell below is unknown");
        assert!(!ok(true, false, Some(state("minecraft:air")), Some(egg), true), "nothing below");
        assert!(!ok(true, false, Some(state("minecraft:bedrock")), Some(egg), true), "bedrock below");
        assert!(!ok(true, false, Some(state("minecraft:oak_slab[type=bottom]")), Some(egg), true), "a half slab is not a full cube");
        assert!(!ok(true, false, Some(stone), Some(egg), false), "the state cannot stand there");
        assert!(ok(true, false, Some(stone), None, false), "removing a block never needs to survive");
    }
}
