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
//! # Known gaps, all disclosed as `Missing` rows
//!
//! * **Vanilla's own run-around-like-crazy goal and its own mount-panic goal** both
//!   require an existing passenger to run at all (vanilla's own eligibility
//!   check reads whether the mob is a ridden vehicle), and this table has no way to express "only
//!   while ridden". The tame roll they gate is already ported directly as
//!   [`MobSim::attempt_horse_tame`](crate) — called once per empty-handed
//!   mount attempt from `interact_horse` rather than as a recurring goal —
//!   see that method's own doc for the one disclosed pacing difference now
//!   that a real passenger model exists.
//! * **`TemptGoal(HORSE_TEMPT_ITEMS)`** needs a horse-specific tempt-item
//!   feed the server's `tempt_food` table does not carry — the same
//!   disclosed gap as the pig's carrot-on-a-stick row in
//!   [`passive::PIG`](super::passive::PIG). Note this is a **different** item
//!   set from `species::horse_temper_gain`'s feed table (that one drives
//!   `handleEating`'s temper counter on interact, not idle wander-toward).
//! * **`RandomStandGoal`** (rearing) needs a client-visible standing pose
//!   this crate does not model.

use crate::ai::goal::Goal;
use crate::ai::goals::{BreedGoal, FollowParentGoal, LookAtPlayerGoal, RandomStrollGoal};

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
    Registration::missing(Selector::Goal, 1, "RunAroundLikeCrazyGoal"),
    Registration::goal(2, "BreedGoal", breed_1_0),
    Registration::goal(4, "FollowParentGoal", follow_parent_1_0),
    Registration::goal(6, "WaterAvoidingRandomStrollGoal", stroll_0_7),
    Registration::goal(7, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    Registration::missing(Selector::Goal, 9, "RandomStandGoal"),
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::missing(Selector::Goal, 1, "AbstractHorse.MountPanicGoal"),
    Registration::missing(Selector::Goal, 3, "TemptGoal(HORSE_TEMPT_ITEMS)"),
];

/// Vanilla's own breed-goal registration: speed multiplier `1.0`, mate class
/// the abstract horse type.
fn breed_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(BreedGoal::new(ctx.speed))
}

/// Vanilla's own follow-parent-goal registration: speed multiplier `1.0`.
fn follow_parent_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowParentGoal::new(ctx.speed))
}

/// Vanilla's own water-avoiding-random-stroll-goal registration at speed
/// multiplier `0.7` — slower than every farm animal
/// in [`passive`](super::passive), whose shared [`super::passive`] strollers
/// are all `1.0`.
fn stroll_0_7(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RandomStrollGoal::new(ctx.speed * 0.7))
}

/// Vanilla's own look-at-player-goal registration: look distance `6.0F`,
/// target class the player.
fn look_at_player_6(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LookAtPlayerGoal::new(6.0, LOOK_PROBABILITY))
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
