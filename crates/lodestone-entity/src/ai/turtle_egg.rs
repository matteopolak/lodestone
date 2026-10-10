//! Turtle egg block states: how many eggs a cell holds and how far they have
//! cracked.

use std::collections::BTreeMap;

use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;

/// Most eggs one block holds.
pub const MAX_EGGS: u32 = 4;
/// Crack stage at which the next hatch roll opens the egg.
pub const MAX_HATCH: u32 = 2;

/// The egg block holding `eggs` eggs at crack stage `hatch`.
#[must_use]
pub fn state(eggs: u32, hatch: u32) -> Option<StateId> {
    let parts: BTreeMap<String, String> =
        [("eggs".to_owned(), eggs.to_string()), ("hatch".to_owned(), hatch.to_string())].into();
    StateId::from_exact_parts(Block::TurtleEgg.name(), &parts)
}

/// `(eggs, hatch)` of an egg block state; `None` for any other block.
#[must_use]
pub fn counts(state: StateId) -> Option<(u32, u32)> {
    if state.block() != Block::TurtleEgg {
        return None;
    }
    let get = |key: &str| {
        state.properties().iter().find(|&&(k, _)| k == key).and_then(|&(_, v)| v.parse().ok())
    };
    Some((get("eggs")?, get("hatch")?))
}

/// Whether the block is in the sand tag, which an egg must rest on to hatch and
/// a turtle must stand on to lay.
#[must_use]
pub fn is_sand(state: StateId) -> bool {
    lodestone_data::tool::block_tag_contains("minecraft:sand", state.block())
}

/// Whether the state is any kind of air.
#[must_use]
pub fn is_air(state: StateId) -> bool {
    matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_back_what_state_wrote() {
        for eggs in 1..=MAX_EGGS {
            for hatch in 0..=MAX_HATCH {
                let s = state(eggs, hatch).expect("valid egg state");
                assert_eq!(counts(s), Some((eggs, hatch)));
            }
        }
        assert_eq!(counts(Block::Stone.default_state()), None);
    }

    #[test]
    fn only_the_three_sands_are_sand() {
        assert!(is_sand(Block::Sand.default_state()));
        assert!(is_sand(Block::RedSand.default_state()));
        assert!(is_sand(Block::SuspiciousSand.default_state()));
        assert!(!is_sand(Block::Gravel.default_state()));
        assert!(!is_sand(Block::Sandstone.default_state()));
    }
}
