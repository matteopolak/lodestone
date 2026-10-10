//! Parrot goals: flying to a perch in the trees, and trailing other mobs.

use lodestone_model::Vec3;

use super::goal::{Flag, FlagSet, Goal, reduced_tick_delay};
use super::mob::MobController;
use super::target_class::TargetClass;

/// Flies to a leaf or log top within a few blocks, or anywhere in the air.
#[derive(Debug)]
pub struct PerchWanderGoal {
    speed: f64,
    target: Option<Vec3>,
}

impl PerchWanderGoal {
    /// A wander at `speed`.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self { speed, target: None }
    }

    /// The first empty cell with empty air above it and a leaf or log under it,
    /// in a box 3 blocks to each side and 6 above and below.
    fn perch(mob: &dyn MobController) -> Option<Vec3> {
        let here = mob.position();
        let (cx, cy, cz) = (here.x.floor() as i32, here.y.floor() as i32, here.z.floor() as i32);
        let (min_x, max_x) = ((here.x - 3.0).floor() as i32, (here.x + 3.0).floor() as i32);
        let (min_y, max_y) = ((here.y - 6.0).floor() as i32, (here.y + 6.0).floor() as i32);
        let (min_z, max_z) = ((here.z - 3.0).floor() as i32, (here.z + 3.0).floor() as i32);
        let is_air = |cell: (i32, i32, i32)| {
            mob.block_state_at(cell).is_some_and(super::turtle_egg::is_air)
        };
        let is_perch = |cell: (i32, i32, i32)| {
            mob.block_state_at(cell).is_some_and(|state| {
                lodestone_data::tool::block_tag_contains("minecraft:leaves", state.block())
                    || lodestone_data::tool::block_tag_contains("minecraft:logs", state.block())
            })
        };
        for z in min_z..=max_z {
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    if (x, y, z) == (cx, cy, cz) {
                        continue;
                    }
                    if is_perch((x, y - 1, z)) && is_air((x, y, z)) && is_air((x, y + 1, z)) {
                        return Some(Vec3::new(f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5));
                    }
                }
            }
        }
        None
    }
}

impl Goal for PerchWanderGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if mob.no_action_time() >= 100 || mob.next_i32(reduced_tick_delay(120)) != 0 {
            return false;
        }
        let swimming = mob.in_water().then(|| mob.random_stroll_target()).flatten();
        let perch = if mob.next_f32() >= 0.001 { Self::perch(mob) } else { None };
        self.target = perch.or(swimming).or_else(|| mob.air_wander_position(None));
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.navigation_done()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(target) = self.target {
            mob.move_to(target, self.speed);
        }
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.target = None;
    }
}

/// Trails the nearest mob of another species, keeping within 3 blocks.
#[derive(Debug)]
pub struct TrailMobGoal {
    speed: f64,
    target: Option<Vec3>,
    recalc: i32,
}

impl TrailMobGoal {
    const STOP_DISTANCE: f64 = 3.0;

    /// A follower at `speed`.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self { speed, target: None, recalc: 0 }
    }
}

fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)
}

impl Goal for TrailMobGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn target_class(&self) -> Option<TargetClass> {
        Some(TargetClass::OtherSpecies)
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.target = mob.nearest_of_class(TargetClass::OtherSpecies);
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.target = mob.nearest_of_class(TargetClass::OtherSpecies);
        self.target.is_some_and(|t| {
            !mob.navigation_done() && distance_sqr(mob.position(), t) > Self::STOP_DISTANCE * Self::STOP_DISTANCE
        })
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.recalc = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.target = None;
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = self.target else { return };
        mob.look_at(target);
        self.recalc -= 1;
        if self.recalc > 0 {
            return;
        }
        self.recalc = reduced_tick_delay(10);
        let gap = distance_sqr(mob.position(), target);
        if gap > Self::STOP_DISTANCE * Self::STOP_DISTANCE {
            mob.move_to(target, self.speed);
        } else {
            mob.stop_navigation();
            if gap <= Self::STOP_DISTANCE {
                let here = mob.position();
                let away = Vec3::new(2.0 * here.x - target.x, here.y, 2.0 * here.z - target.z);
                mob.move_to(away, self.speed);
            }
        }
    }
}
