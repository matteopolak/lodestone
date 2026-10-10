//! A mob levelling a spear at its target: close in, charge, then back off.

use lodestone_model::Vec3;

use super::goal::{Flag, FlagSet, Goal, reduced_tick_delay};
use super::kinetic::{KineticSpear, kinetic_spear};
use super::mob::MobController;

/// Longest the mob keeps backing off before the goal ends, in ticks.
fn max_fleeing_time() -> i32 {
    reduced_tick_delay(100)
}

#[derive(Debug)]
struct State {
    engage_time: i32,
    fleeing_time: i32,
    away: Option<Vec3>,
    done: bool,
}

/// Charges at the current target with a spear in hand.
#[derive(Debug)]
pub struct SpearUseGoal {
    charge_speed: f64,
    reposition_speed: f64,
    approach_sq: f64,
    in_range_sq: f64,
    state: Option<State>,
}

impl SpearUseGoal {
    /// A goal that approaches within `approach` blocks, charges at
    /// `charge_speed` and counts `in_range` blocks as a touch.
    #[must_use]
    pub fn new(charge_speed: f64, reposition_speed: f64, approach: f64, in_range: f64) -> Self {
        Self {
            charge_speed,
            reposition_speed,
            approach_sq: approach * approach,
            in_range_sq: in_range * in_range,
            state: None,
        }
    }

    fn spear(mob: &dyn MobController) -> Option<KineticSpear> {
        mob.main_hand_item().and_then(kinetic_spear)
    }

    fn able(mob: &dyn MobController) -> bool {
        mob.attack_target().is_some() && Self::spear(mob).is_some()
    }
}

fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)
}

impl Goal for SpearUseGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        Self::able(mob) && !mob.is_using_item()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.state.as_ref().is_some_and(|s| !s.done) && Self::able(mob)
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.state = Some(State { engage_time: -1, fleeing_time: -1, away: None, done: false });
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.stop_navigation();
        mob.stop_using_item();
        self.state = None;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let (Some(target), Some(spear), Some(state)) = (mob.attack_target(), Self::spear(mob), self.state.as_mut())
        else {
            return;
        };
        let d2 = distance_sqr(mob.position(), target);
        mob.look_at(target);
        if state.engage_time < 0 {
            if d2 > self.approach_sq {
                mob.move_to(target, self.reposition_speed);
                return;
            }
            state.engage_time = reduced_tick_delay(spear.use_duration());
            mob.start_using_item();
        }
        if state.engage_time > 0 {
            state.engage_time -= 1;
            if state.engage_time == 0 {
                mob.stop_using_item();
                let d = d2.sqrt();
                state.away = mob.position_away(target, (9.0 - d).max(0.0), (11.0 - d).max(1.0));
                state.fleeing_time = 1;
            }
        }
        if state.fleeing_time > 0 {
            state.fleeing_time += 1;
            if state.fleeing_time > max_fleeing_time() {
                state.done = true;
                return;
            }
        }
        if let Some(away) = state.away {
            mob.move_to(away, self.reposition_speed);
            if mob.navigation_done() {
                if state.fleeing_time > 0 {
                    state.done = true;
                    return;
                }
                state.away = None;
            }
        } else {
            mob.move_to(target, self.charge_speed);
            if d2 < self.in_range_sq || mob.navigation_done() {
                let d = d2.sqrt();
                state.away = mob.position_away(target, (6.0 - d).max(0.0), (7.0 - d).max(1.0));
            }
        }
    }
}
