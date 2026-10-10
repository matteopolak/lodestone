//! A raider walking to the village it is raiding.

use lodestone_model::Vec3;

use super::goal::{Flag, FlagSet, Goal};
use super::mob::MobController;

/// Horizontal distance from the raid centre inside which a raider counts as
/// having reached the village and stops pathing.
pub const VILLAGE_RADIUS: f64 = 32.0;

/// Heads for the raid centre, in hops of up to fifteen blocks, while the mob
/// has no target and is outside the village.
#[derive(Debug)]
pub struct MarchOnRaidGoal {
    speed: f64,
}

impl MarchOnRaidGoal {
    /// A march at `speed` blocks per tick.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self { speed }
    }
}

impl MarchOnRaidGoal {
    fn outside_village(mob: &dyn MobController) -> Option<Vec3> {
        let centre = mob.raid_center()?;
        let here = mob.position();
        let d2 = (centre.x - here.x).powi(2) + (centre.z - here.z).powi(2);
        (d2 > VILLAGE_RADIUS * VILLAGE_RADIUS).then_some(centre)
    }
}

impl Goal for MarchOnRaidGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_none() && Self::outside_village(mob).is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        Self::outside_village(mob).is_some()
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(centre) = Self::outside_village(mob) else { return };
        if mob.navigation_done()
            && let Some(spot) = mob.random_target_towards(centre)
        {
            mob.move_to(spot, self.speed);
        }
    }
}
