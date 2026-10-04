//! Release-validated movement properties indexed by canonical block identity.
//!
//! The shared numeric rows are authenticated captures; release membership is
//! checked before indexing them. Context-sensitive stuck multipliers are not
//! properties in this census and remain a separate behavior rule.

use lodestone_model::BlockMovement;

use crate::{GameDataVersion, block_states::StateId, generated_block_movement};

#[must_use]
pub fn for_state(version: GameDataVersion, state: StateId) -> Option<BlockMovement> {
    if !version.supports_state(state) {
        return None;
    }
    let (friction, speed_factor, jump_factor, bounce_restitution, climbable, suppresses_bounce) =
        generated_block_movement::MOVEMENT[usize::from(state.block().registry_id())];
    Some(BlockMovement {
        friction,
        speed_factor,
        jump_factor,
        bounce_restitution,
        climbable,
        suppresses_bounce,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::Block;

    #[test]
    fn captured_movement_preserves_every_26_2_row_bit_for_bit() {
        let capture = include_str!("../tests/support/block_physics_jvm.txt");
        let mut count = 0;
        for row in capture.lines().filter(|row| row.starts_with("K ")) {
            let fields: Vec<_> = row.split_whitespace().collect();
            let block = Block::from_name(fields[1]).unwrap();
            let state = GameDataVersion::V26_2.default_state(block).unwrap();
            let facts = for_state(GameDataVersion::V26_2, state).unwrap();
            for (actual, expected) in [facts.friction, facts.speed_factor,
                facts.jump_factor, facts.bounce_restitution].into_iter().zip(&fields[2..6]) {
                assert_eq!(actual.to_bits(), u32::from_str_radix(expected, 16).unwrap(), "{}", fields[1]);
            }
            assert_eq!(facts.climbable, fields[6] == "1", "{}", fields[1]);
            assert_eq!(facts.suppresses_bounce, fields[7] == "1", "{}", fields[1]);
            count += 1;
        }
        assert_eq!(count, 1196);
    }

    #[test]
    fn captured_new_bounce_is_release_scoped() {
        let block = Block::from_name("minecraft:shelf_mushroom").unwrap();
        let state = GameDataVersion::V26_3.default_state(block).unwrap();
        assert!(for_state(GameDataVersion::V26_2, state).is_none());
        let facts = for_state(GameDataVersion::V26_3, state).unwrap();
        // Independent 26.3 capture: restitution bits 0x3f400000, no suppression.
        assert_eq!(facts.bounce_restitution.to_bits(), 0x3f400000);
        assert_eq!(facts.effective_bounce_restitution(), 0.75);
        assert!(!facts.suppresses_bounce);
        assert_eq!(facts.friction.to_bits(), 0x3f19999a);
        let honey = for_state(GameDataVersion::V26_3, Block::HoneyBlock.default_state()).unwrap();
        assert!(honey.suppresses_bounce);
        let suppressed = BlockMovement { suppresses_bounce: true, ..facts };
        assert_eq!(suppressed.effective_bounce_restitution(), 0.0);
    }
}
