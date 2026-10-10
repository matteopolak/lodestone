//! Goal sets for water animals: the bony fish (cod, salmon, tropical fish,
//! pufferfish) and the squids.
//!
//! # What it is
//!
//! The registrations of the shared fish base (panic, flee players, wander the
//! water) plus the two species extras, in the reference's priority numbers.
//!
//! # How it works
//!
//! A swimming mob's stroll target search lands in water (see
//! [`NavMode`](crate::pathfinding::NavMode)); the interval is 40, reduced to 20 goal ticks. Schooling and the
//! puff are goals that only read and ask: a follower swims after the leader the
//! host assigns, and the puff goal asks the host to inflate while something
//! scary is near. The host owns both the school links and the puff state.

use crate::ai::goal::{FlagSet, Goal, reduced_tick_delay};
use crate::ai::mob::{MobController, distance_sqr};
use crate::ai::goals::{AvoidEntityGoal, DriftFleeGoal, DriftGoal, PanicGoal, RandomStrollGoal};

use super::{Registration, SpeciesContext};

/// Resolves the fish species this module owns.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        "cod" | "salmon" | "tropical_fish" => Some(SCHOOLING_FISH),
        "pufferfish" => Some(PUFFERFISH),
        "squid" | "glow_squid" => Some(SQUID),
        _ => None,
    }
}

/// A schooling fish: the base set plus following a flock leader at priority 5.
pub static SCHOOLING_FISH: &[Registration] = &[
    Registration::goal(0, "panic", panic),
    Registration::goal(2, "avoid player", avoid_player),
    Registration::goal(4, "swim stroll", swim_stroll),
    Registration::goal(5, "follow flock leader", follow_flock_leader),
];

/// The pufferfish: the base set plus the puff goal at priority 1.
pub static PUFFERFISH: &[Registration] = &[
    Registration::goal(0, "panic", panic),
    Registration::goal(2, "avoid player", avoid_player),
    Registration::goal(4, "swim stroll", swim_stroll),
    Registration::goal(1, "puff", puff),
];

/// Squid and glow squid: pulsed drifting, and an escape from whoever hurt them.
pub static SQUID: &[Registration] = &[
    Registration::goal(0, "drift", drift),
    Registration::goal(1, "drift flee", drift_flee),
];

fn follow_flock_leader(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowFlockLeaderGoal { speed: ctx.speed, until_path: 0 })
}

/// Swims after the school leader while within 11 blocks of it, repathing every
/// ten goal ticks; a leader with followers of its own never follows.
#[derive(Debug)]
struct FollowFlockLeaderGoal {
    speed: f64,
    until_path: i32,
}

impl Goal for FollowFlockLeaderGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.has_flock_followers() && mob.flock_leader().is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.flock_leader().is_some_and(|leader| distance_sqr(leader, mob.position()) <= 121.0)
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.until_path = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.leave_flock();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.until_path -= 1;
        if self.until_path <= 0 {
            self.until_path = reduced_tick_delay(10);
            if let Some(leader) = mob.flock_leader() {
                mob.move_to(leader, self.speed);
            }
        }
    }
}

fn puff(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PuffGoal)
}

/// Inflates while something scary is within 2 blocks.
#[derive(Debug)]
struct PuffGoal;

impl Goal for PuffGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.scary_near()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_inflating(true);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_inflating(false);
    }
}

fn drift(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(DriftGoal::new())
}

fn drift_flee(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(DriftFleeGoal::new())
}

fn panic(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.25))
}

/// Flee players within 8 blocks at 1.6 (1.4 when sprinting away); the goal
/// has a single speed, so it uses the close-range figure.
fn avoid_player(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AvoidEntityGoal::new(8.0, ctx.speed * 1.4))
}

fn swim_stroll(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FishStrollGoal(RandomStrollGoal::new(ctx.speed).with_interval(40)))
}

/// The swim stroll, which a school follower never starts.
#[derive(Debug)]
struct FishStrollGoal(RandomStrollGoal);

impl Goal for FishStrollGoal {
    fn flags(&self) -> FlagSet {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.flock_leader().is_none() && self.0.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.0.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.0.start(mob);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.0.stop(mob);
    }
}

#[cfg(test)]
mod tests {
    use crate::ai::roster::registrations_for;

    #[test]
    fn every_fish_gets_the_base_priorities() {
        for species in ["cod", "salmon", "tropical_fish", "pufferfish"] {
            let priorities: Vec<i32> = registrations_for(species).iter().map(|r| r.priority).collect();
            assert!(priorities.starts_with(&[0, 2, 4]), "{species}: {priorities:?}");
        }
    }

    #[test]
    fn a_squid_has_no_path_goals() {
        assert_eq!(registrations_for("squid").len(), 2);
        assert!(std::ptr::eq(registrations_for("squid"), registrations_for("glow_squid")));
    }
}
