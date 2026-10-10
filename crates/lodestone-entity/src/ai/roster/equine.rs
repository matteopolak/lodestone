//! Goal set for the horse family — horse, donkey, mule — one table shared by
//! all three, transcribed from vanilla's own abstract-horse goal
//! registration in the 26.2 decompiled sources.
//!
//! # What it is
//!
//! Before this module, `"horse"`/`"donkey"`/`"mule"` had **zero rows in any
//! roster family** — every family's `lookup` returned `None` for all three,
//! so a spawned one fell all the way through to [`FALLBACK`](super::FALLBACK):
//! a stroll and a look goal, not a horse's actual behaviour. That gap existed
//! despite taming (`species::TameMechanism::Temper`), the temper/food tables
//! and `MobSim::attempt_horse_tame` all being real and tested — the AI half
//! of the species was simply never installed.
//!
//! # Why one table for three species
//!
//! The horse, donkey and mule, plus the abstract chested-horse type between
//! the latter two, all decline to override goal registration, so every one of
//! them runs the abstract horse's own registration verbatim.
//! `skeleton_horse` and `zombie_horse` are deliberately **not** claimed here:
//! nothing in this crate has verified whether either overrides it, and
//! guessing "no override" the way the shared-table gate below checks for
//! horse/donkey/mule would be an unverified claim baked into a citation.
//!
//! # Known gap, disclosed as a `Missing` row
//!
//! Run-around-like-crazy needs a rider-eject notification from the sim to the
//! connection, which does not exist. The tame roll it gates runs once per
//! empty-handed mount attempt as
//! [`MobSim::attempt_horse_tame`](crate), and an untamed horse cannot be
//! mounted.

use crate::ai::goal::{FlagSet, Goal};
use crate::ai::goals::{MateGoal, TrailParentGoal, WatchPlayerGoal, FleeInPanicGoal, WanderGoal, LureGoal};
use crate::ai::mob::MobController;

use super::{LOOK_PROBABILITY, Registration, Selector, SpeciesContext, float_goal, random_look_around};

/// Every species this family claims. Iterated by `roster`'s invariant gates.
pub const SPECIES: &[&str] = &["horse", "donkey", "mule"];

/// Resolves a species path to its table, or `None` if this family does not
/// claim it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        "horse" | "donkey" | "mule" => Some(HORSE_FAMILY),
        _ => None,
    }
}

/// Vanilla's own abstract-horse goal registration, in the
/// method's own call order: its own six goal registrations (including the
/// rearing-gated ninth, which is unconditionally `true` —
/// vanilla's own rearing-eligibility check returns `true` and neither the
/// donkey nor the mule override it), then a shared three-goal helper's own
/// registrations, called last from inside the main registration method.
pub static HORSE_FAMILY: &[Registration] = &[
    Registration::missing(Selector::Goal, 1, "horse.buck_off_rider"),
    Registration::goal(2, "mate", breed_1_0),
    Registration::goal(4, "trail_parent", follow_parent_1_0),
    Registration::goal(6, "wander_dry", stroll_0_7),
    Registration::goal(7, "watch_player(player)", look_at_player_6),
    Registration::goal(8, "idle_glance", random_look_around),
    Registration::goal(9, "rear_up", random_stand),
    Registration::goal(0, "stay_afloat", float_goal),
    Registration::goal(1, "horse.mount_panic", mount_panic),
    Registration::goal(3, "lure(horse_items)", tempt),
];

/// Panic at 1.2, only ever running unridden: a ridden mount's goals do not
/// tick, because its rider steers it.
fn mount_panic(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 1.2))
}

fn tempt(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LureGoal::new(ctx.speed * 1.25))
}

fn random_stand(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RearUpGoal::new())
}

/// Rears up now and then: a counter starts 80 ticks below zero and rises one a
/// tick; once positive, each tick has `counter / 1000` odds of ending the wait,
/// and a tenth of those rear.
#[derive(Debug)]
struct RearUpGoal {
    next_stand: i32,
}

impl RearUpGoal {
    const INTERVAL: i32 = 80;

    fn new() -> Self {
        Self { next_stand: -Self::INTERVAL }
    }
}

impl Goal for RearUpGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.next_stand += 1;
        if self.next_stand > 0 && mob.next_i32(1000) < self.next_stand {
            self.next_stand = -Self::INTERVAL;
            mob.next_i32(10) == 0
        } else {
            false
        }
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        false
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.rear_up();
    }
}

/// Vanilla's own breed-goal registration: speed multiplier `1.0`, mate class
/// the abstract horse type.
fn breed_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MateGoal::new(ctx.speed))
}

/// Vanilla's own follow-parent-goal registration: speed multiplier `1.0`.
fn follow_parent_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TrailParentGoal::new(ctx.speed))
}

/// Vanilla's own water-avoiding-random-stroll-goal registration at speed
/// multiplier `0.7` — slower than every farm animal
/// in [`passive`](super::passive), whose shared [`super::passive`] strollers
/// are all `1.0`.
fn stroll_0_7(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WanderGoal::new(ctx.speed * 0.7))
}

/// Vanilla's own look-at-player-goal registration: look distance `6.0F`,
/// target class the player.
fn look_at_player_6(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WatchPlayerGoal::new(6.0, LOOK_PROBABILITY))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::roster::registrations_for;

    #[test]
    fn horse_donkey_and_mule_all_share_the_same_table() {
        let (h, d, m) = (
            registrations_for("horse"),
            registrations_for("donkey"),
            registrations_for("mule"),
        );
        assert!(std::ptr::eq(h.as_ptr(), d.as_ptr()) && h.len() == d.len());
        assert!(std::ptr::eq(h.as_ptr(), m.as_ptr()) && h.len() == m.len());
    }

    #[test]
    fn a_species_this_family_does_not_claim_is_not_shadowed() {
        assert!(lookup("zombie_horse").is_none());
        assert!(lookup("skeleton_horse").is_none());
        assert!(lookup("cow").is_none());
    }

    #[test]
    fn the_horse_family_reaches_a_real_goal_selector_and_is_not_the_fallback() {
        let ctx = SpeciesContext {
            speed: 1.0,
            attack_reach: 0.0,
        };
        let goals = super::super::goals_for("horse", &ctx);
        assert!(
            !goals.is_empty(),
            "a horse must get real behaviour, not the empty set"
        );
        assert!(
            !super::super::is_fallback(registrations_for("horse")),
            "a horse must not fall through to the generic stroll/look pair"
        );
    }
}
