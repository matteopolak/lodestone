//! Goal sets for the farm animals: cow, mooshroom, sheep, pig, chicken.
//!
//! # What it is
//!
//! One [`Registration`] table per species, transcribed from
//! `.cache/mc/26.2/src/net/minecraft/world/entity/animal/`. Owned by issue
//! [#228]; extend it here and nothing else in the tree changes.
//!
//! # Why this family is where the roster first becomes visible
//!
//! Before this module, `MobSim::spawn_species` installed `RandomStrollGoal` and
//! `RandomLookAroundGoal` on a cow and nothing else. `FloatGoal`, `PanicGoal`,
//! `BreedGoal`, `TemptGoal` and `FollowParentGoal` were **fully implemented, fully
//! unit-tested, fully fed with real perception by `MobSim::tick` — and installed
//! by nothing but tests.** Every call site outside `#[cfg(test)]` was zero. That
//! is the island shape one layer up from the one issue #441 fixed: perception was
//! no longer starved, but nothing put the goals that read it on a real mob.
//!
//! So the cow and pig tables below are the first production installation of five
//! goals, and `crates/lodestone-server/tests/mob_roster.rs` is the gate that says
//! so behaviourally rather than by counting goals.
//!
//! # Known gaps, all disclosed in the tables
//!
//! * **Sheep grazing** (`Sheep.eatBlockGoal`) is [`Coverage::Missing`]. Its
//!   blocking dependency is *not* missing — `random_tick.rs` models grass→dirt
//!   and runs in the production tick loop — so this is issue #238's to build, not
//!   to unblock.
//! * **A pig's carrot-on-a-stick tempt** is [`Coverage::Missing`] separately from
//!   its food tempt, because it is a distinct vanilla registration with a
//!   distinct item and the server's `tempt_food` feed covers only the food tag.
//!
//! [#228]: https://github.com/matteopolak/lodestone/issues/228

use crate::ai::goal::Goal;
use crate::ai::goals::{FollowParentGoal, PanicGoal, TemptGoal};

use super::{
    Registration, Selector, SpeciesContext, breed_1_0, float_goal, look_at_player_6,
    random_look_around, stroll,
};

/// Every species this family claims. Iterated by `roster`'s invariant gates.
pub const SPECIES: &[&str] = &["cow", "mooshroom", "sheep", "pig", "chicken"];

/// Resolves a species path to its table, or `None` if this family does not claim
/// it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        // `MushroomCow` declares no `registerGoals` of its own
        // (`animal/cow/MushroomCow.java`), so a mooshroom inherits
        // `AbstractCow`'s verbatim — the same reason they share `cow_food`.
        "cow" | "mooshroom" => Some(COW),
        "sheep" => Some(SHEEP),
        "pig" => Some(PIG),
        "chicken" => Some(CHICKEN),
        _ => None,
    }
}

/// `animal/cow/AbstractCow.java:40-48`.
///
/// The only table in the roster with **no** gaps: all eight of vanilla's cow
/// registrations have an equivalent here.
pub const COW: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_2_0),
    Registration::goal(2, "BreedGoal", breed_1_0),
    Registration::goal(3, "TemptGoal(COW_FOOD)", tempt_1_25),
    Registration::goal(4, "FollowParentGoal", follow_parent_1_25),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(7, "RandomLookAroundGoal", random_look_around),
];

/// `animal/sheep/Sheep.java:74-84`. Note `:75` assigns `this.eatBlockGoal` before
/// the first `addGoal`, so the registrations are `:76-84`.
pub const SHEEP: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_1_25),
    Registration::goal(2, "BreedGoal", breed_1_0),
    Registration::goal(3, "TemptGoal(SHEEP_FOOD)", tempt_1_1),
    Registration::goal(4, "FollowParentGoal", follow_parent_1_1),
    // `this.eatBlockGoal` — grazing, issue #238. Unlike most gaps in this roster
    // its dependency already exists: `crates/lodestone-server/src/random_tick.rs`
    // models grass→dirt and runs in the production tick loop, so #228's own
    // "blocked on random ticks" note is stale.
    Registration::missing(Selector::Goal, 5, "EatBlockGoal"),
    Registration::goal(6, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(7, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
];

/// `animal/pig/Pig.java:80-89`.
///
/// A pig is the one species here with **two** `TemptGoal` registrations at the
/// same priority (`:84-85`), for carrot-on-a-stick and for `PIG_FOOD`.
pub const PIG: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_1_25),
    // Vanilla puts a pig's `BreedGoal` at 3, not 2 — nothing occupies 2.
    Registration::goal(3, "BreedGoal", breed_1_0),
    // `TemptGoal(this, 1.2, i -> i.is(Items.CARROT_ON_A_STICK), false)`. A
    // separate registration from the food one below, and a separate item: the
    // server's `tempt_food` feed resolves the `pig_food` **tag** only, so
    // steering a pig with a carrot on a stick is not modelled. It is also the
    // riding-control item, so it belongs with saddles rather than with tempting.
    Registration::missing(Selector::Goal, 4, "TemptGoal(CARROT_ON_A_STICK)"),
    Registration::goal(4, "TemptGoal(PIG_FOOD)", tempt_1_2),
    Registration::goal(5, "FollowParentGoal", follow_parent_1_1),
    Registration::goal(6, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(7, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
];

/// `animal/chicken/Chicken.java:85-93`.
pub const CHICKEN: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_1_4),
    Registration::goal(2, "BreedGoal", breed_1_0),
    Registration::goal(3, "TemptGoal(CHICKEN_FOOD)", tempt_1_0),
    Registration::goal(4, "FollowParentGoal", follow_parent_1_1),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(7, "RandomLookAroundGoal", random_look_around),
];

// -- builders, one per distinct jar speed multiplier -------------------------
//
// Vanilla's speed arguments are multipliers on the mob's own MOVEMENT_SPEED, so
// each of these is `ctx.speed * <the jar's factor>` and the factor stays visible
// next to the citation. `Registration.build` must be a plain `fn` item, so a
// parameterised closure is not an option.

/// `PanicGoal(this, 2.0)` — cow (`animal/cow/AbstractCow.java:42`). The fastest
/// panic in this family.
fn panic_2_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 2.0))
}

/// `PanicGoal(this, 1.25)` — sheep (`animal/sheep/Sheep.java:77`) and pig
/// (`animal/pig/Pig.java:82`).
fn panic_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.25))
}

/// `PanicGoal(this, 1.4)` — chicken (`animal/chicken/Chicken.java:87`).
fn panic_1_4(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.4))
}

/// `TemptGoal(this, 1.25, …)` — cow (`animal/cow/AbstractCow.java:44`).
fn tempt_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.25))
}

/// `TemptGoal(this, 1.1, …)` — sheep (`animal/sheep/Sheep.java:79`).
fn tempt_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.1))
}

/// `TemptGoal(this, 1.2, …)` — pig (`animal/pig/Pig.java:85`).
fn tempt_1_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.2))
}

/// `TemptGoal(this, 1.0, …)` — chicken (`animal/chicken/Chicken.java:89`).
fn tempt_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed))
}

/// `FollowParentGoal(this, 1.25)` — cow (`animal/cow/AbstractCow.java:45`).
fn follow_parent_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowParentGoal::new(ctx.speed * 1.25))
}

/// `FollowParentGoal(this, 1.1)` — sheep (`Sheep.java:80`), pig (`Pig.java:86`)
/// and chicken (`Chicken.java:90`).
fn follow_parent_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowParentGoal::new(ctx.speed * 1.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::roster::Coverage;

    /// A cow's table is the roster's completeness benchmark: vanilla registers
    /// eight goals and every one has an equivalent here. If a gap ever appears in
    /// it, that is a regression in this crate's goal coverage, not a
    /// transcription choice.
    #[test]
    fn a_cow_has_no_unmodelled_registrations() {
        for r in COW {
            assert!(
                matches!(r.coverage, Coverage::Modelled(_)),
                "cow's {} is no longer modelled: {:?}",
                r.vanilla,
                r.coverage
            );
        }
        assert_eq!(COW.len(), 8, "`AbstractCow.java:41-48` registers 8 goals");
    }

    /// The speed each goal actually asks the mob to move at, measured, against
    /// the value predicted from the jar's own multiplier.
    ///
    /// This is the gate a priority-multiset check cannot replace. A cow's
    /// `TemptGoal` built with sheep's `1.1` instead of cow's `1.25` sits at the
    /// right priority, under the right vanilla name, and still moves the cow
    /// toward the player — so every structural assertion and every
    /// direction-of-movement assertion passes. Only predicting `0.2 × 1.25 =
    /// 0.25` and requiring the measurement to land there separates them.
    ///
    /// `BASE` is deliberately not `1.0`: with a base of 1.0 a dropped
    /// multiplication is invisible for the `1.0` multipliers and a swapped
    /// multiplier still shows, so the test would be weaker in exactly the case it
    /// exists for.
    #[test]
    fn every_speed_matches_the_jars_multiplier() {
        use crate::ai::roster::probe::SpeedProbe;

        const BASE: f64 = 0.2;
        let ctx = SpeciesContext::new(BASE);

        // (species, vanilla name, jar multiplier, cited line). Transcribed from
        // the `.java` files, not from the tables above — the whole point is that
        // the expected value originates outside the code under test.
        let expected: &[(&str, &str, f64, &str)] = &[
            ("cow", "PanicGoal", 2.0, "AbstractCow.java:42"),
            ("cow", "BreedGoal", 1.0, "AbstractCow.java:43"),
            ("cow", "TemptGoal(COW_FOOD)", 1.25, "AbstractCow.java:44"),
            ("cow", "FollowParentGoal", 1.25, "AbstractCow.java:45"),
            ("cow", "WaterAvoidingRandomStrollGoal", 1.0, "AbstractCow.java:46"),
            ("sheep", "PanicGoal", 1.25, "Sheep.java:77"),
            ("sheep", "BreedGoal", 1.0, "Sheep.java:78"),
            ("sheep", "TemptGoal(SHEEP_FOOD)", 1.1, "Sheep.java:79"),
            ("sheep", "FollowParentGoal", 1.1, "Sheep.java:80"),
            ("sheep", "WaterAvoidingRandomStrollGoal", 1.0, "Sheep.java:82"),
            ("pig", "PanicGoal", 1.25, "Pig.java:82"),
            ("pig", "BreedGoal", 1.0, "Pig.java:83"),
            ("pig", "TemptGoal(PIG_FOOD)", 1.2, "Pig.java:85"),
            ("pig", "FollowParentGoal", 1.1, "Pig.java:86"),
            ("pig", "WaterAvoidingRandomStrollGoal", 1.0, "Pig.java:87"),
            ("chicken", "PanicGoal", 1.4, "Chicken.java:87"),
            ("chicken", "BreedGoal", 1.0, "Chicken.java:88"),
            ("chicken", "TemptGoal(CHICKEN_FOOD)", 1.0, "Chicken.java:89"),
            ("chicken", "FollowParentGoal", 1.1, "Chicken.java:90"),
            ("chicken", "WaterAvoidingRandomStrollGoal", 1.0, "Chicken.java:91"),
        ];

        for &(species, vanilla, multiplier, cite) in expected {
            let table = super::super::registrations_for(species);
            let row = table
                .iter()
                .find(|r| r.vanilla == vanilla)
                .unwrap_or_else(|| panic!("{species} has no {vanilla} row"));
            let build = row
                .build()
                .unwrap_or_else(|| panic!("{species}'s {vanilla} builds nothing"));

            let mut probe = SpeedProbe::new();
            let mut goal = build(&ctx);
            assert!(
                goal.can_use(&mut probe),
                "{species}'s {vanilla} could not start against a permissive \
                 probe, so no speed can be read from it"
            );
            goal.start(&mut probe);
            goal.tick(&mut probe);

            let measured = probe.first_speed().unwrap_or_else(|| {
                panic!("{species}'s {vanilla} never called move_to, so its speed argument is unobservable")
            });
            let want = BASE * multiplier;
            assert!(
                (measured - want).abs() < 1e-9,
                "{species}'s {vanilla} moves at {measured}, but {cite} says \
                 {multiplier} × the mob's speed = {want}"
            );
        }
    }

    /// The control for the gate above: it must be *able* to fail. Feed it the
    /// wrong species' multiplier and the same assertion has to reject it —
    /// otherwise the tolerance is loose enough to accept anything.
    ///
    /// Cow panic is `2.0` and sheep panic is `1.25` (`AbstractCow.java:42` vs
    /// `Sheep.java:77`), so the two are 0.15 blocks/tick apart at this base and
    /// the 1e-9 tolerance cannot straddle them.
    #[test]
    fn the_speed_gate_rejects_a_wrong_multiplier() {
        use crate::ai::roster::probe::SpeedProbe;

        const BASE: f64 = 0.2;
        let ctx = SpeciesContext::new(BASE);
        let cow_panic = COW
            .iter()
            .find(|r| r.vanilla == "PanicGoal")
            .and_then(super::Registration::build)
            .expect("cow has a modelled PanicGoal");

        let mut probe = SpeedProbe::new();
        let mut goal = cow_panic(&ctx);
        assert!(goal.can_use(&mut probe));
        goal.start(&mut probe);
        let measured = probe.first_speed().expect("panic moves the mob");

        assert!(
            (measured - BASE * 2.0).abs() < 1e-9,
            "cow panic must be 2.0x"
        );
        assert!(
            (measured - BASE * 1.25).abs() >= 1e-9,
            "the gate above would accept sheep's 1.25 for a cow, so it is not \
             measuring the multiplier at all"
        );
    }
}
