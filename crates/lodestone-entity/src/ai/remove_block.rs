//! A goal that walks to a block of one kind and breaks it after standing on it.

use lodestone_data::block::Block;

use super::block_edit::{BlockEdit, BlockExpect};
use super::block_seek::BlockSeek;
use super::goal::{Flag, FlagSet, Goal, reduced_tick_delay};
use super::mob::MobController;
use super::turtle_egg::is_air;

/// Ticks on the block before it breaks.
const BREAK_AFTER_TICKS: i32 = 60;

/// Seeks the nearest `block` with two air cells above it inside the mob's home,
/// walks to it and breaks it once it has been in reach for 60 ticks.
#[derive(Debug)]
pub struct BreakBlockGoal {
    block: Block,
    seek: BlockSeek,
    accepted: f64,
    ticks_in_reach: i32,
}

impl BreakBlockGoal {
    /// A goal breaking `block`, walking at `speed`, scanning `vertical_range`
    /// layers and counting a mob within `accepted` blocks of the target as
    /// arrived.
    #[must_use]
    pub fn new(block: Block, speed: f64, vertical_range: i32, accepted: f64) -> Self {
        Self { block, seek: BlockSeek::new(speed, 24, vertical_range), accepted, ticks_in_reach: 0 }
    }

    fn is_target(block: Block, mob: &dyn MobController, (x, y, z): (i32, i32, i32)) -> bool {
        mob.block_state_at((x, y, z)).is_some_and(|s| s.block() == block)
            && mob.block_state_at((x, y + 1, z)).is_some_and(is_air)
            && mob.block_state_at((x, y + 2, z)).is_some_and(is_air)
    }

    /// The cell underfoot or beside the mob that holds the block: its own cell,
    /// the four neighbours, then one and two below.
    fn block_in_reach(&self, mob: &dyn MobController) -> Option<(i32, i32, i32)> {
        let here = mob.position();
        let (x, y, z) = (here.x.floor() as i32, here.y.floor() as i32, here.z.floor() as i32);
        [(x, y, z), (x, y - 1, z), (x - 1, y, z), (x + 1, y, z), (x, y, z - 1), (x, y, z + 1), (x, y - 2, z)]
            .into_iter()
            .find(|&cell| mob.block_state_at(cell).is_some_and(|s| s.block() == self.block))
    }
}

impl Goal for BreakBlockGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Jump])
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !mob.mob_griefing() {
            return false;
        }
        if self.seek.waiting() {
            return false;
        }
        let block = self.block;
        if self.seek.find(mob, &|m, cell| Self::is_target(block, m, cell)) {
            self.seek.set_next_start(reduced_tick_delay(20));
            true
        } else {
            let delay = BlockSeek::default_delay(mob);
            self.seek.set_next_start(delay);
            false
        }
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        let block = self.block;
        self.seek.can_continue(mob, &|m, cell| Self::is_target(block, m, cell))
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.seek.start(mob);
        self.ticks_in_reach = 0;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.seek.tick(mob, self.accepted);
        if !self.seek.reached() {
            return;
        }
        if let Some(cell) = self.block_in_reach(mob) {
            if self.ticks_in_reach > BREAK_AFTER_TICKS {
                mob.request_block_edit(BlockEdit::new(cell, BlockExpect::Block(self.block), None));
            }
            self.ticks_in_reach += 1;
        }
    }
}
