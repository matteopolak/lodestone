//! The cat's hunt of its attack target: sneak up slowly, dash the last four
//! blocks, strike once a second.

use lodestone_model::Vec3;

use super::goal::{Flag, FlagSet, Goal};
use super::mob::MobController;

/// Hits its attack target from inside twice its own width, once every 20 ticks.
#[derive(Debug)]
pub struct StalkAttackGoal {
    reach_sqr: f64,
    target: Option<Vec3>,
    attack_time: i32,
}

fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)
}

impl StalkAttackGoal {
    /// Ticks between strikes.
    const COOLDOWN: i32 = 20;
    /// Beyond 15 blocks the hunt is dropped.
    const GIVE_UP_SQR: f64 = 225.0;

    /// A hunt by a mob `width` blocks wide.
    #[must_use]
    pub fn new(width: f64) -> Self {
        Self { reach_sqr: (width * 2.0).powi(2), target: None, attack_time: 0 }
    }
}

impl Goal for StalkAttackGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.target = mob.attack_target();
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(target) = self.target else { return false };
        if mob.attack_target().is_none() {
            return false;
        }
        distance_sqr(mob.position(), target) <= Self::GIVE_UP_SQR && (!mob.navigation_done() || self.can_use(mob))
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.target = None;
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = mob.attack_target() else { return };
        self.target = Some(target);
        mob.look_at(target);
        let gap_sqr = distance_sqr(mob.position(), target);
        let speed = if gap_sqr > self.reach_sqr && gap_sqr < 16.0 {
            1.33
        } else if gap_sqr < Self::GIVE_UP_SQR {
            0.6
        } else {
            0.8
        };
        mob.chase(target, speed);
        self.attack_time = (self.attack_time - 1).max(0);
        if gap_sqr <= self.reach_sqr && self.attack_time <= 0 {
            self.attack_time = Self::COOLDOWN;
            mob.attack(target);
        }
    }
}
