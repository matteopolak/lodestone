//! Template **processors** — the per-block filters vanilla runs between a
//! template's palette and the world (issue #514's S2).
//!
//! # What it is
//!
//! The `StructureProcessor` kinds the structures wired in S2 actually name:
//! `BlockIgnoreProcessor` (drop air / structure blocks), `BlockRotProcessor`
//! (integrity — the reason a ruin is a ruin) and `RuleProcessor` (match-and-replace
//! block states). A [`Processor`] is a pure function of `(world position, state)`,
//! which is what lets [`super::template::StructureTemplate::place`] run one piece
//! independently in every chunk it touches.
//!
//! # How it works
//!
//! Every processor that needs randomness derives it from the block's **absolute
//! position**: `RandomSource.create(Mth.getSeed(pos))`, a fresh legacy LCG per
//! block. There is no shared stream, so no draw order to preserve across chunks —
//! which is the whole reason integrity survives our per-chunk pipeline. Getting
//! this wrong (a per-piece stream) would make the same shipwreck rot differently
//! in each chunk it spans.
//!
//! # How to change it
//!
//! * `Processor::process` returning `None` means **drop this block**, exactly as
//!   vanilla's `@Nullable StructureBlockInfo` does. It is not an error path.
//! * The 40 bundled `worldgen/processor_list/*.json` documents are referenced by
//!   *template pools*, not by any structure this unit places, so nothing here
//!   parses JSON yet. When S4's jigsaw needs them, add a `parse` beside these
//!   variants and ledger any `processor_type` it does not cover — the pattern
//!   [`super::StructureRegistry::unsupported`] already carries.
//! * Two processor kinds the S2 structures *do* reference are deliberately absent
//!   and named in that ledger: `capped` (ocean ruins' 5 suspicious sand/gravel
//!   blocks, which need the archaeology loot pass and a shuffled-index walk over
//!   the whole processed list) and `gravity`.

use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, get_seed};

use super::template::BlockState;

/// A `RuleTest` — the input half of a [`ProcessorRule`].
#[derive(Debug, Clone)]
pub enum RuleTest {
    /// `always_true`.
    AlwaysTrue,
    /// `block_match` — the block id, ignoring properties.
    BlockMatch(String),
    /// `blockstate_match` — the exact canonical state.
    BlockStateMatch(String),
    /// `random_block_match`.
    RandomBlockMatch(String, f32),
}

impl RuleTest {
    fn test(&self, state: &BlockState, random: &mut LegacyRandomSource) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::BlockMatch(name) => &state.name == name,
            Self::BlockStateMatch(spec) => state.canonical() == *spec,
            Self::RandomBlockMatch(name, probability) => {
                &state.name == name && random.next_float() < *probability
            }
        }
    }
}

/// One `ProcessorRule`: match the template state, emit `output`.
///
/// Vanilla also carries a `location_predicate` (tested against the *world* state
/// at the target) and a `position_predicate`. Neither is represented here, and
/// nothing this unit places uses anything but `always_true` for them.
#[derive(Debug, Clone)]
pub struct ProcessorRule {
    /// `input_predicate`.
    pub input: RuleTest,
    /// `output_state`.
    pub output: BlockState,
}

/// A processor in a piece's chain.
#[derive(Debug, Clone)]
pub enum Processor {
    /// `BlockIgnoreProcessor` — drop a block whose id is in the list.
    ///
    /// The most load-bearing processor in the whole unit: `STRUCTURE_AND_AIR`
    /// drops the template's air, which is why a shipwreck's hull keeps the sand
    /// it is buried in instead of being cleared to an air box.
    BlockIgnore(Vec<String>),
    /// `BlockRotProcessor` — keep a block only when
    /// `random.nextFloat() <= integrity`.
    BlockRot {
        /// `integrity`, `0.0..=1.0`.
        integrity: f32,
    },
    /// `RuleProcessor` — first matching rule replaces the state.
    Rule(Vec<ProcessorRule>),
}

impl Processor {
    /// `processBlock` — the state to place, or `None` to drop this block.
    #[must_use]
    pub fn process(&self, world: [i32; 3], state: BlockState) -> Option<BlockState> {
        match self {
            Self::BlockIgnore(ignored) => {
                if ignored.iter().any(|name| *name == state.name) {
                    None
                } else {
                    Some(state)
                }
            }
            Self::BlockRot { integrity } => {
                let mut random = LegacyRandomSource::new(get_seed(world[0], world[1], world[2]));
                if random.next_float() <= *integrity {
                    Some(state)
                } else {
                    None
                }
            }
            Self::Rule(rules) => {
                let mut random = LegacyRandomSource::new(get_seed(world[0], world[1], world[2]));
                for rule in rules {
                    if rule.input.test(&state, &mut random) {
                        return Some(rule.output.clone());
                    }
                }
                Some(state)
            }
        }
    }

    /// `BlockIgnoreProcessor.STRUCTURE_AND_AIR`.
    #[must_use]
    pub fn structure_and_air() -> Self {
        Self::BlockIgnore(vec![
            "minecraft:air".to_string(),
            "minecraft:structure_block".to_string(),
        ])
    }

    /// `BlockIgnoreProcessor.STRUCTURE_BLOCK`.
    #[must_use]
    pub fn structure_block() -> Self {
        Self::BlockIgnore(vec!["minecraft:structure_block".to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_ignore_drops_air_and_keeps_everything_else() {
        let processor = Processor::structure_and_air();
        assert!(processor.process([0, 0, 0], BlockState::of("minecraft:air")).is_none());
        assert!(
            processor
                .process([0, 0, 0], BlockState::of("minecraft:structure_block"))
                .is_none()
        );
        assert!(
            processor
                .process([0, 0, 0], BlockState::of("minecraft:spruce_planks"))
                .is_some()
        );
    }

    /// Integrity 1.0 keeps everything, 0.0 keeps nothing, and the answer for a
    /// given position does not depend on when it is asked — the property that
    /// lets two chunks place two halves of one piece.
    #[test]
    fn block_rot_is_position_deterministic_and_respects_the_extremes() {
        let keep_all = Processor::BlockRot { integrity: 1.0 };
        let drop_all = Processor::BlockRot { integrity: 0.0 };
        let state = BlockState::of("minecraft:stone_bricks");
        let mut kept = 0;
        for x in 0..64 {
            let pos = [x, 62, -13];
            assert!(keep_all.process(pos, state.clone()).is_some());
            assert!(drop_all.process(pos, state.clone()).is_none());
            let half = Processor::BlockRot { integrity: 0.5 };
            let first = half.process(pos, state.clone()).is_some();
            assert_eq!(first, half.process(pos, state.clone()).is_some());
            kept += usize::from(first);
        }
        // 0.5 integrity over 64 positions must be a mixture, not all-or-nothing.
        assert!((8..56).contains(&kept), "integrity 0.5 kept {kept}/64");
    }
}
