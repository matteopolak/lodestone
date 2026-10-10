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
//! pufferfish puff are `Missing` rows: flock following needs a group feed and
//! the puff needs a contact-damage hook.

use crate::ai::goal::Goal;
use crate::ai::goals::{AvoidEntityGoal, DriftFleeGoal, DriftGoal, PanicGoal, RandomStrollGoal};

use super::{Registration, Selector, SpeciesContext};

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
    Registration::missing(Selector::Goal, 5, "follow flock leader"),
];

/// The pufferfish: the base set plus the puff goal at priority 1.
pub static PUFFERFISH: &[Registration] = &[
    Registration::goal(0, "panic", panic),
    Registration::goal(2, "avoid player", avoid_player),
    Registration::goal(4, "swim stroll", swim_stroll),
    Registration::missing(Selector::Goal, 1, "puff"),
];

/// Squid and glow squid: pulsed drifting, and an escape from whoever hurt them.
pub static SQUID: &[Registration] = &[
    Registration::goal(0, "drift", drift),
    Registration::goal(1, "drift flee", drift_flee),
];

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
    Box::new(RandomStrollGoal::new(ctx.speed).with_interval(40))
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
