//! An enderman picking up and putting down blocks.


use lodestone_data::block_states::StateId;

use super::block_edit::{BlockEdit, BlockExpect};
use super::goal::{FlagSet, Goal, reduced_tick_delay};
use super::mob::MobController;
use super::turtle_egg::is_air;

/// Whether `state` is a block an enderman will pick up.
#[must_use]
pub fn is_holdable(state: StateId) -> bool {
    lodestone_data::tool::block_tag_contains("minecraft:enderman_holdable", state.block())
}

/// Whether a straight horizontal line from the centre of the cell the mob stands
/// in to the centre of `target`, at the target's height, reaches `target` before
/// any other solid cell.
fn clear_line(mob: &dyn MobController, from: (i32, i32), target: (i32, i32, i32)) -> bool {
    let (tx, ty, tz) = target;
    let (sx, sz) = (f64::from(from.0) + 0.5, f64::from(from.1) + 0.5);
    let (ex, ez) = (f64::from(tx) + 0.5, f64::from(tz) + 0.5);
    let steps = (((ex - sx).abs().max((ez - sz).abs())) * 8.0).ceil().max(1.0) as i32;
    for i in 0..=steps {
        let t = f64::from(i) / f64::from(steps);
        let cell = ((sx + (ex - sx) * t).floor() as i32, ty, (sz + (ez - sz) * t).floor() as i32);
        if cell == (tx, ty, tz) {
            return true;
        }
        if mob.block_state_at(cell).is_some_and(|s| !is_air(s)) {
            return false;
        }
    }
    true
}

/// Picks up a random holdable block within two blocks, one tick in twenty.
#[derive(Debug, Default)]
pub struct PickUpBlockGoal;

impl Goal for PickUpBlockGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.carried_block().is_none()
            && !mob.edit_pending()
            && mob.mob_griefing()
            && mob.next_i32(reduced_tick_delay(20)) == 0
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let here = mob.position();
        let cell = (
            (here.x - 2.0 + mob.next_f64() * 4.0).floor() as i32,
            (here.y + mob.next_f64() * 3.0).floor() as i32,
            (here.z - 2.0 + mob.next_f64() * 4.0).floor() as i32,
        );
        let Some(state) = mob.block_state_at(cell) else { return };
        if is_holdable(state) && clear_line(mob, (here.x.floor() as i32, here.z.floor() as i32), cell) {
            let edit = BlockEdit::new(cell, BlockExpect::State(state), None);
            mob.request_carry_edit(edit, Some(state.block().default_state()));
        }
    }
}

/// Puts the carried block down on a random nearby cell, one tick in two thousand.
#[derive(Debug, Default)]
pub struct PutDownBlockGoal;

impl Goal for PutDownBlockGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.carried_block().is_some()
            && !mob.edit_pending()
            && mob.mob_griefing()
            && mob.next_i32(reduced_tick_delay(2000)) == 0
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(carried) = mob.carried_block() else { return };
        let here = mob.position();
        let cell = (
            (here.x - 1.0 + mob.next_f64() * 2.0).floor() as i32,
            (here.y + mob.next_f64() * 2.0).floor() as i32,
            (here.z - 1.0 + mob.next_f64() * 2.0).floor() as i32,
        );
        let edit = BlockEdit::new(cell, BlockExpect::Air, Some(carried)).checked_placement();
        mob.request_carry_edit(edit, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_data::block::Block;

    #[test]
    fn holdable_blocks_follow_the_tag() {
        assert!(is_holdable(Block::Sand.default_state()));
        assert!(is_holdable(Block::GrassBlock.default_state()));
        assert!(!is_holdable(Block::Stone.default_state()));
        assert!(!is_holdable(Block::Bedrock.default_state()));
    }
}
