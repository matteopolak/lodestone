//! A rabbit's raid on a ripe carrot crop.

use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_model::Vec3;

use super::block_edit::{BlockEdit, BlockExpect};
use super::block_seek::BlockSeek;
use super::goal::{Flag, FlagSet, Goal};
use super::mob::MobController;

/// Ticks a rabbit stays satisfied after eating.
const SATIATED_TICKS: i32 = 40;
/// Ticks between raids once one has been attempted.
const RETRY_DELAY: i32 = 10;
/// The age of a fully grown carrot crop.
const RIPE_AGE: u32 = 7;

fn carrot_age(state: StateId) -> Option<u32> {
    if state.block() != Block::Carrots {
        return None;
    }
    state.properties().iter().find(|&&(k, _)| k == "age").and_then(|&(_, v)| v.parse().ok())
}

fn carrots_at(age: u32) -> Option<StateId> {
    let parts = [("age".to_owned(), age.to_string())].into();
    StateId::from_exact_parts(Block::Carrots.name(), &parts)
}

/// Walks to farmland under a ripe carrot crop and eats one growth stage of it.
///
/// A raid that starts away from the crop is dropped on the next tick; only a
/// rabbit that begins within reach of the crop eats.
#[derive(Debug)]
pub struct CropRaidGoal {
    seek: BlockSeek,
    can_raid: bool,
}

impl CropRaidGoal {
    /// The rabbit goal: speed 0.7, sixteen-block search.
    #[must_use]
    pub fn new() -> Self {
        Self { seek: BlockSeek::new(0.7, 16, 1), can_raid: false }
    }

    fn supports_crop(mob: &dyn MobController, (x, y, z): (i32, i32, i32)) -> Option<StateId> {
        let below = mob.block_state_at((x, y, z))?;
        if !lodestone_data::tool::block_tag_contains("minecraft:supports_crops", below.block()) {
            return None;
        }
        mob.block_state_at((x, y + 1, z))
    }
}

impl Default for CropRaidGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for CropRaidGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.seek.waiting() {
            return false;
        }
        if !mob.mob_griefing() {
            return false;
        }
        self.can_raid = false;
        let delay = BlockSeek::default_delay(mob);
        self.seek.set_next_start(delay);
        let wants = mob.wants_more_food();
        let found = self.seek.find(mob, &|m, cell| {
            wants && Self::supports_crop(m, cell).and_then(carrot_age) == Some(RIPE_AGE)
        });
        self.can_raid = found;
        found
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        // The crop test passes only while a raid is not armed, and continuing
        // needs one armed, so the goal ends after its first tick.
        false
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.seek.start(mob);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.seek.tick(mob, 1.0);
        let (x, y, z) = self.seek.block();
        mob.look_at(Vec3::new(f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5));
        if !self.seek.reached() {
            return;
        }
        let crop = (x, y + 1, z);
        if self.can_raid
            && let Some(state) = mob.block_state_at(crop)
            && let Some(age) = carrot_age(state)
        {
            let set = if age == 0 { None } else { carrots_at(age - 1) };
            mob.request_block_edit(BlockEdit { cell: crop, expect: BlockExpect::State(state), set });
            mob.set_more_carrot_ticks(SATIATED_TICKS);
        }
        self.can_raid = false;
        self.seek.set_next_start(RETRY_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carrot_ages_round_trip() {
        for age in 0..=RIPE_AGE {
            assert_eq!(carrots_at(age).and_then(carrot_age), Some(age));
        }
        assert_eq!(carrot_age(Block::Stone.default_state()), None);
    }
}
