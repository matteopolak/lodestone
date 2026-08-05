//! Goal sets for the farm animals: cow, mooshroom, sheep, pig, chicken, rabbit.
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
//! * **A pig's carrot-on-a-stick tempt** is [`Coverage::Missing`] separately from
//!   its food tempt, because it is a distinct vanilla registration with a
//!   distinct item and the server's `tempt_food` feed covers only the food tag.
//! * **A rabbit's `AvoidEntityGoal` is modelled but inert**, because the server's
//!   `avoided_species` feed has no rabbit arm — see [`rabbit_avoid_player`].
//! * **Two rabbit rows are unmodelled for block-perception-adjacent reasons**, next.
//!
//! # Block perception: the sheep's row is closed, two rabbit rows are not
//!
//! Sheep grazing (`Sheep.eatBlockGoal`, issue [#238]) used to be
//! [`Coverage::Missing`] because [`MobController`](crate::ai::MobController)
//! could not read a block at all — a goal that eats grass could not ask whether
//! there was grass. That seam landed as issue [#456] (`bdf7120`, `b50255a`):
//! `PathWorld::block_cues` answers block *identity* on the world seam,
//! `MobController::block_cues_at_feet`/`_below` are overridden on
//! [`NavigatingMob`](crate::ai::navigating_mob::NavigatingMob) from the
//! [`PathWorld`](crate::pathfinding::PathWorld) it already borrows, and the goal
//! reports each eat back as an `ate(EatenBlock)` intent for the host to apply.
//! [`eat_block`] is installed by the [`SHEEP`] table below.
//!
//! **What that achieves, and what it does not.** The goal is installed on a real
//! mob and reads the seam. **A sheep in a running game still does not graze**:
//! the host half is `ChunkWorld::block_cues` — the classification, which
//! `base_path_type` deliberately erases, since `grass_block`, `dirt` and `stone`
//! are one `Blocked` — plus a `pending_grazes` handoff drained where mutable
//! chunk access lives. `MobSim` borrows the world immutably, so the mutation must
//! take the `pending_detonations` route through the tick driver rather than
//! happening in `MobSim::tick`. Until that lands the cue feed answers
//! `BlockCues::NONE`, which leaves this row inert rather than wrong. Wool
//! regrowth — `Sheep.ate()`'s `setSheared(false)` plus `ageUp(60)`
//! (`animal/sheep/Sheep.java:292-297`) — is entity metadata on the wire and is
//! still to come. `docs/mob-block-perception.md` is the doc.
//!
//! **A generalisation not to inherit.** #456's body grouped seven `Missing` rows
//! across two families as one seam capability. Measured against the jar it closes
//! **one**: a rabbit's `ClimbOnTopOfPowderSnowGoal` needs powder-snow physics
//! nothing here models, its `RaidGardenGoal` needs a host-computed candidate
//! block position (`MoveToBlockGoal`'s spiral) plus a block-state *property*, and
//! [`hostile_melee`](super::hostile_melee)'s `RestrictSunGoal` reads no block at
//! all. Anyone planning off the original table would expect the rest to be free.
//!
//! **A stale claim not to inherit either.** #228 says grazing is blocked on
//! random ticks, and the epic's plan then corrects that to "unblocked, because
//! `random_tick.rs` exists and runs in the production tick loop". The correction
//! is true and it was *not sufficient*: `random_tick.rs` being real makes a
//! grass→dirt **world mutation** available, which was never the binding
//! constraint — the seam above was.
//!
//! # What consumes these tables — and the honest limit on it
//!
//! [`goals_for`](super::goals_for) is called by `MobSim::spawn_species`, so every
//! table here reaches a real `GoalSelector` on a real mob. But
//! **`seed_demo_mobs` — the only production path that creates a client-visible
//! mob — spawns `minecraft:zombie` and nothing else** (`mobs.rs:2500-2526`, a
//! hardcoded ring). Every species in this file is therefore reachable from tests
//! and from a caller that names it, and from **no** running game. That is unit
//! A4's job in issue #225's plan, and it is not done; until it is, the gate
//! below driving a real [`NavigatingMob`](crate::ai::navigating_mob::NavigatingMob)
//! is the strongest available evidence that these tables do something, and
//! "a player can see a cow" is not yet true.
//!
//! [#228]: https://github.com/matteopolak/lodestone/issues/228
//! [#238]: https://github.com/matteopolak/lodestone/issues/238
//! [#456]: https://github.com/matteopolak/lodestone/issues/456

use crate::ai::goal::Goal;
use crate::ai::goals::{
    AvoidEntityGoal, BreedGoal, EatBlockGoal, FollowParentGoal, LookAtPlayerGoal, PanicGoal,
    RandomStrollGoal, TemptGoal,
};

use super::{
    LOOK_PROBABILITY, Registration, Selector, SpeciesContext, breed_1_0, float_goal,
    look_at_player_6, random_look_around, stroll,
};

/// Every species this family claims. Iterated by `roster`'s invariant gates.
pub const SPECIES: &[&str] = &["cow", "mooshroom", "sheep", "pig", "chicken", "rabbit"];

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
        "rabbit" => Some(RABBIT),
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
    // The seam gap this row waited on is closed (#456, `bdf7120`): the goal reads
    // the block below through `MobController::block_cues_below`. Grazing still
    // needs the host's drain of `take_new_eaten` to see grass turn to dirt — see
    // `docs/mob-block-perception.md`.
    Registration::goal(5, "EatBlockGoal", eat_block),
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

/// `animal/rabbit/Rabbit.java:119-130`.
///
/// The odd one out of this family in five ways, every one a jar fact rather than
/// a transcription choice. They are listed because four of the five are exactly
/// the shape of thing that gets "fixed" into symmetry by a later reader:
///
/// * **No `FollowParentGoal`.** Every other species here registers one; a rabbit
///   does not — `:119-130` has no such line — so there is no row for it. Do not
///   add one for consistency with its siblings.
/// * **Three registrations share priority 1** (`FloatGoal`,
///   `ClimbOnTopOfPowderSnowGoal`, `RabbitPanicGoal`), where every other species
///   here has exactly one goal per priority.
/// * **Its look goal is at priority 11**, not 6 or 7, and at **`10.0F`** rather
///   than the `6.0F` every other farm animal uses (`:130`).
/// * **It is the only species in this family that flees anything**, and it
///   registers three `AvoidEntityGoal`s to do it.
/// * **Its breed and stroll speeds are not the family's** — `0.8` and `0.6`
///   against everyone else's `1.0`, so neither shared builder applies.
///
/// The killer-bunny variant installs a `MeleeAttackGoal(1.4, true)` and two
/// target goals from `setRabbitType` (`:371-374`), **not** from `registerGoals`.
/// They are deliberately absent here: this table is the `registerGoals`
/// transcription that the multiset gate cites, and a conditional runtime
/// installation is a different mechanism — the one
/// [`GoalSelector::remove`](crate::ai::goal::GoalSelector::remove) exists for.
/// Adding them as rows would make the cited line range a lie.
pub const RABBIT: &[Registration] = &[
    Registration::goal(1, "FloatGoal", float_goal),
    // `ClimbOnTopOfPowderSnowGoal(this, this.level())` (`:121`) — it has to read
    // the block the mob is standing in, and `MobController` exposes no block
    // access whatsoever (see this module's header). Not a missing goal so much
    // as the missing seam capability every unmodelled row in the roster shares.
    Registration::missing(Selector::Goal, 1, "ClimbOnTopOfPowderSnowGoal"),
    Registration::goal(1, "Rabbit.RabbitPanicGoal", panic_2_2),
    Registration::goal(2, "BreedGoal", breed_0_8),
    Registration::goal(3, "TemptGoal(RABBIT_FOOD)", tempt_1_0),
    // The three `RabbitAvoidEntityGoal`s (`:125-127`) differ from the creeper's
    // pair in a way worth being explicit about: the creeper's Ocelot and Cat
    // registrations share one radius (`6.0F`), so one class-agnostic goal of
    // ours reproduces both exactly. A rabbit's three do **not** — Player
    // `8.0F`, Wolf `10.0F`, Monster `4.0F`. One instance therefore cannot carry
    // all three radii, and this row takes the Player figure.
    //
    // So the two `CoveredBy` rows below are coverage of the *behaviour* (the
    // rabbit flees what the server's feed reports as a threat) at the **wrong
    // radius**: a wolf is fled from 2 blocks later than vanilla, a monster 4
    // blocks earlier. A disclosed approximation, and the honest alternative to
    // three goals fighting over MOVE at equal priority.
    Registration::goal(
        4,
        "Rabbit.RabbitAvoidEntityGoal(Player)",
        rabbit_avoid_player,
    ),
    Registration::covered(
        Selector::Goal,
        4,
        "Rabbit.RabbitAvoidEntityGoal(Wolf)",
        "Rabbit.RabbitAvoidEntityGoal(Player)",
    ),
    Registration::covered(
        Selector::Goal,
        4,
        "Rabbit.RabbitAvoidEntityGoal(Monster)",
        "Rabbit.RabbitAvoidEntityGoal(Player)",
    ),
    // `Rabbit.RaidGardenGoal(this)` (`:128`) — a `MoveToBlockGoal` that hunts
    // carrot crops and eats them. Same missing block access as the powder-snow
    // row, plus a block mutation; see the header's `EatBlockGoal` note, which is
    // the same feature under a different name.
    Registration::missing(Selector::Goal, 5, "Rabbit.RaidGardenGoal"),
    Registration::goal(6, "WaterAvoidingRandomStrollGoal", stroll_0_6),
    Registration::goal(11, "LookAtPlayerGoal(Player)", look_at_player_10),
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

/// `Rabbit.RabbitPanicGoal(this, 2.2)` — rabbit
/// (`animal/rabbit/Rabbit.java:122`). The fastest panic in the family; a cow's
/// `2.0` is next.
///
/// `RabbitPanicGoal` (`:578`) is a `PanicGoal` subclass whose only addition is
/// setting the jump control while fleeing, so the speed argument is the whole of
/// what our `PanicGoal` models and the subclass is not a separate gap.
fn panic_2_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 2.2))
}

/// `BreedGoal(this, 0.8)` — rabbit (`animal/rabbit/Rabbit.java:123`). The only
/// species in this family whose breed speed is not `1.0`, which is why it cannot
/// use the shared [`breed_1_0`].
fn breed_0_8(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(BreedGoal::new(ctx.speed * 0.8))
}

/// `WaterAvoidingRandomStrollGoal(this, 0.6)` — rabbit
/// (`animal/rabbit/Rabbit.java:129`), against the `1.0` every other farm animal
/// registers, so the shared [`stroll`] would be wrong by a factor of 1.67.
fn stroll_0_6(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RandomStrollGoal::new(ctx.speed * 0.6))
}

/// `LookAtPlayerGoal(this, Player.class, 10.0F)` — rabbit
/// (`animal/rabbit/Rabbit.java:130`), the only non-`6.0F` look distance in this
/// family, so [`look_at_player_6`] does not apply.
fn look_at_player_10(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LookAtPlayerGoal::new(10.0, LOOK_PROBABILITY))
}

/// `Rabbit.RabbitAvoidEntityGoal<>(this, Player.class, 8.0F, 2.2, 2.2)` — rabbit
/// (`animal/rabbit/Rabbit.java:125`).
///
/// Vanilla's fourth and fifth arguments are the walk and sprint tiers, and a
/// rabbit is the one registration in the roster where **they are equal**
/// (`2.2, 2.2`), so the shared builder's "take the walk tier, sprint is not
/// modelled" caveat costs nothing here.
///
/// **This goal is inert for a rabbit in production today**, and that is not a
/// defect in this row. Our `AvoidEntityGoal` reads
/// [`MobController::avoid_threat`](crate::ai::MobController::avoid_threat),
/// which `MobSim` feeds from its own `avoided_species` table — and that table
/// has arms for creeper, the skeletons and the spiders only. Until it gains a
/// `"rabbit" => &["player", "wolf", "monster"]` arm the rabbit sees no threats
/// and this goal never starts. The roster deliberately does not carry perception
/// data (see the module header of [`super`]), so the fix belongs there, not here.
fn rabbit_avoid_player(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AvoidEntityGoal::new(8.0, ctx.speed * 2.2))
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

/// `EatBlockGoal(this)` — sheep only (`animal/sheep/Sheep.java`), no arguments.
/// Its predicate reads the block at and below the mob through
/// `MobController::block_cues_*` (#456); a host whose `PathWorld` does not
/// classify blocks leaves it inert rather than wrong.
fn eat_block(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(EatBlockGoal::new())
}

#[cfg(test)]
mod tests {
    use lodestone_model::Vec3;

    use super::*;
    use crate::ai::goal::GoalSelector;
    use crate::ai::navigating_mob::NavigatingMob;
    use crate::ai::roster::Coverage;
    use crate::pathfinding::{Aabb, BlockCues, MobShape, PathType, PathWorld};

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
            // A rabbit shares not one multiplier with its siblings except the
            // tempt `1.0`, which makes it the strongest row in this table: four
            // of its five figures are unique in the family, so a builder copied
            // from a neighbour fails here rather than passing by coincidence.
            ("rabbit", "Rabbit.RabbitPanicGoal", 2.2, "Rabbit.java:122"),
            ("rabbit", "BreedGoal", 0.8, "Rabbit.java:123"),
            ("rabbit", "TemptGoal(RABBIT_FOOD)", 1.0, "Rabbit.java:124"),
            (
                "rabbit",
                "Rabbit.RabbitAvoidEntityGoal(Player)",
                2.2,
                "Rabbit.java:125",
            ),
            ("rabbit", "WaterAvoidingRandomStrollGoal", 0.6, "Rabbit.java:129"),
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

    /// A rabbit's table against the exact multiset of `addGoal` calls at
    /// `animal/rabbit/Rabbit.java:119-130`.
    ///
    /// The family's other four species are covered by the equivalent gate in
    /// [`super`], which is unit A3's file; this one lives here so that adding a
    /// species to this family never requires an edit outside it. The expectation
    /// is transcribed from the jar in jar order, **including the two
    /// registrations this repo does not implement** — an expectation listing only
    /// what we build would be satisfied by any subset, including a wrong one.
    #[test]
    fn a_rabbits_table_matches_the_jars_addgoal_block() {
        let want: Vec<(Selector, i32, &str)> = vec![
            (Selector::Goal, 1, "FloatGoal"),
            (Selector::Goal, 1, "ClimbOnTopOfPowderSnowGoal"),
            (Selector::Goal, 1, "Rabbit.RabbitPanicGoal"),
            (Selector::Goal, 2, "BreedGoal"),
            (Selector::Goal, 3, "TemptGoal(RABBIT_FOOD)"),
            (Selector::Goal, 4, "Rabbit.RabbitAvoidEntityGoal(Player)"),
            (Selector::Goal, 4, "Rabbit.RabbitAvoidEntityGoal(Wolf)"),
            (Selector::Goal, 4, "Rabbit.RabbitAvoidEntityGoal(Monster)"),
            (Selector::Goal, 5, "Rabbit.RaidGardenGoal"),
            (Selector::Goal, 6, "WaterAvoidingRandomStrollGoal"),
            (Selector::Goal, 11, "LookAtPlayerGoal(Player)"),
        ];
        let got: Vec<(Selector, i32, &str)> = super::super::registrations_for("rabbit")
            .iter()
            .map(|r| (r.selector, r.priority, r.vanilla))
            .collect();
        assert_eq!(
            got, want,
            "the rabbit table does not match `animal/rabbit/Rabbit.java:119-130` \
             — re-read the jar before editing either side of this"
        );

        // Three jar facts a later reader is most likely to "correct" into
        // symmetry with the rest of the family. Asserted rather than left to the
        // comment on the table, because a comment cannot fail.
        assert!(
            !RABBIT.iter().any(|r| r.vanilla == "FollowParentGoal"),
            "`Rabbit.java:119-130` registers no FollowParentGoal — every other \
             species in this family does, and adding one here for consistency is \
             exactly what this assertion exists to reject"
        );
        assert!(
            RABBIT.iter().any(|r| r.priority == 11),
            "a rabbit's LookAtPlayerGoal is at priority 11 (`Rabbit.java:130`), \
             not 6 or 7 like its siblings'"
        );
        assert_eq!(
            RABBIT.iter().filter(|r| r.priority == 1).count(),
            3,
            "`Rabbit.java:120-122` puts three registrations at priority 1"
        );
    }

    // -- the behavioural gate: a real `NavigatingMob`, not a fake -------------
    //
    // Everything above is structural or reads an argument back off a probe. Both
    // are necessary and neither can see the failure that matters: a table whose
    // goals are installed on a mob that cannot act on them. `SpeedProbe` and
    // `ScriptMob` override every perception method, so a goal's `can_use` is
    // true against them whatever production does — which is precisely how issue
    // #441's island stayed green. The gate below installs a table into a real
    // `GoalSelector` on a real `NavigatingMob` over a real `PathWorld`, feeds it
    // only what `MobSim::tick` feeds, and measures where the mob ends up.

    /// Flat ground at `y <= -1` with air above — the smallest world a real A\*
    /// search can cross.
    struct Flat;

    impl PathWorld for Flat {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if y <= -1 {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
            if y <= -1 { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            aabb.min_y < 0.0
        }
    }

    /// The mob's `movement_speed`, and the one figure the two runs share.
    const WALK: f64 = 0.3;
    /// Where the tempting player stands, on flat ground 12 blocks along +X.
    const PLAYER: Vec3 = Vec3::new(12.5, 0.0, 0.5);

    /// Installs `species`' table — or nothing, when `species` is `None` — onto a
    /// real [`NavigatingMob`], ticks it `ticks` times with a player holding food
    /// standing [`PLAYER`] away, and returns the horizontal gap before and after.
    ///
    /// The only thing that varies between the measurement and its controls is
    /// **which registration table is installed**. Same world, same mob, same
    /// speed, same perception feed, same tick count — so a difference in outcome
    /// can only come from the roster.
    fn approach_gap(species: Option<&str>, ticks: usize) -> (f64, f64) {
        let world = Flat;
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.4, 0.5),
            Vec3::new(0.5, 0.0, 0.5),
            WALK,
            560,
            0,
        );
        let mut ai = GoalSelector::new();
        if let Some(s) = species {
            for (p, g) in super::super::goals_for(s, &SpeciesContext::new(WALK)) {
                ai.add(p, g);
            }
        }

        let gap = |p: Vec3| ((p.x - PLAYER.x).powi(2) + (p.z - PLAYER.z).powi(2)).sqrt();
        let before = gap(mob.position());
        for _ in 0..ticks {
            // Exactly the perception `MobSim::tick` feeds when a player holds an
            // item in this species' food tag, and the only input this mob gets.
            mob.set_temptation(Some(PLAYER));
            mob.tick(&mut ai);
        }
        (before, gap(mob.position()))
    }

    /// A rabbit built from [`RABBIT`] and driven through the production
    /// `NavigatingMob` + `GoalSelector` path walks to a player holding a carrot.
    ///
    /// Two controls run inside the test, so neither can be skipped or drift out
    /// of date, and both are *differences in the table alone*:
    ///
    /// * **An empty roster entry** — the shape of "this species' table was never
    ///   filled in". The gap must not change at all.
    /// * **The [`FALLBACK`](super::super::FALLBACK) table**, which any unclaimed
    ///   species gets: a stroll and a look, no `TemptGoal`. It receives the
    ///   *identical* temptation feed, so it separates "the roster's tempt row
    ///   moved the rabbit" from "any goal set moves a mob about and 200 ticks is
    ///   long enough to arrive by accident".
    #[test]
    fn a_rabbit_walks_to_a_player_holding_food_and_a_tableless_one_does_not() {
        const TICKS: usize = 200;

        let (before, after) = approach_gap(Some("rabbit"), TICKS);
        assert!(
            (before - 12.0).abs() < 1e-9,
            "precondition: the gap must start at 12 blocks, got {before}"
        );

        // `TemptGoal` stops navigating inside 2.5 blocks (vanilla's stop
        // distance), so a rabbit that genuinely followed ends just inside that,
        // and one walk-step of slack covers the tick it crosses the line on.
        // This is a predicted *value*, not a direction: "it got closer" would be
        // satisfied by a single accidental step.
        assert!(
            after < 2.5 + WALK,
            "a rabbit fed a temptation 12 blocks away ended {after} blocks from \
             it; `TemptGoal`'s stop distance is 2.5, so it never followed"
        );

        let (c_before, c_after) = approach_gap(None, TICKS);
        assert!(
            (c_before - c_after).abs() < 1e-9,
            "control: a mob with an empty roster entry moved from {c_before} to \
             {c_after}. Something other than an installed goal is moving it, so \
             the measurement above is not attributable to the table"
        );

        let (f_before, f_after) = approach_gap(Some("llama"), TICKS);
        assert!(
            (f_before - 12.0).abs() < 1e-9,
            "precondition: the fallback control starts at the same gap"
        );
        assert!(
            f_after > 2.5 + WALK,
            "control: the FALLBACK table — a stroll and a look, no TemptGoal — \
             also reached {f_after} blocks from the player. Then the rabbit's \
             approach is not evidence about its TemptGoal row, and this gate is \
             measuring nothing more than that mobs wander"
        );
    }

    /// [`Flat`], with the floor classified as `minecraft:grass_block`.
    ///
    /// A separate world rather than a cue arm on [`Flat`] on purpose: `Flat`
    /// answers [`BlockCues::NONE`], which is what keeps the tempt gate above free
    /// of a sheep that stops to graze mid-approach. Note the cue is the *only*
    /// difference — a host that classifies nothing leaves [`eat_block`] inert
    /// rather than wrong, which is exactly the state production is in until
    /// `ChunkWorld::block_cues` lands.
    struct Grass;

    impl PathWorld for Grass {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if y <= -1 {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
            if y <= -1 { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            aabb.min_y < 0.0
        }
        fn block_cues(&self, _x: i32, y: i32, _z: i32) -> BlockCues {
            if y <= -1 {
                BlockCues { grass_block: true, ..BlockCues::NONE }
            } else {
                BlockCues::NONE
            }
        }
    }

    /// Ticks a baby `species` on [`Grass`] with **no `add` call of this test's
    /// own** and returns how many eat intents reached the host.
    ///
    /// A baby because [`EatBlockGoal::BABY_INTERVAL`] is 25 ticks against an
    /// adult's 500, so reachability is observable in a short run. The world never
    /// mutates, so nothing depletes the supply — the failure mode that made the
    /// seam's first interval measurement read grass scarcity instead of the eat
    /// interval.
    fn grazes(species: &str, ticks: usize) -> usize {
        let world = Grass;
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.9, 1.3),
            Vec3::new(0.5, 0.0, 0.5),
            WALK,
            256,
            0,
        );
        mob.set_age(crate::ai::navigating_mob::BABY_START_AGE);

        let mut ai = GoalSelector::new();
        for (p, g) in super::super::goals_for(species, &SpeciesContext::new(WALK)) {
            ai.add(p, g);
        }

        let mut eaten = 0;
        for _ in 0..ticks {
            mob.tick(&mut ai);
            eaten += mob.take_new_eaten().len();
        }
        eaten
    }

    /// The [`SHEEP`] table installs an `EatBlockGoal` that a real
    /// [`NavigatingMob`] can actually reach.
    ///
    /// **What this asserts is installation and reachability, not grazing.** The
    /// cue feed here belongs to this test's [`Grass`] world; production's
    /// `ChunkWorld` does not classify blocks yet and the host does not drain
    /// `take_new_eaten`, so a sheep in a running game grazes nothing. An
    /// eat-*count* prediction would therefore be measuring the absent host half
    /// rather than this row — `crates/lodestone-entity/tests/block_perception.rs`
    /// is where the 444-vs-286 interval calibration lives.
    ///
    /// The control is a difference in the table alone: a cow stands on the same
    /// grass, gets the same feed and the same ticks, and its table has no grazing
    /// row — so a blanket install, or a goal reachable from any passive table,
    /// fails here rather than passing as a sheep.
    #[test]
    fn the_sheeps_table_installs_a_reachable_eat_block_goal() {
        const TICKS: usize = 2_000;

        let sheep = grazes("sheep", TICKS);
        assert!(
            sheep > 0,
            "a baby sheep built only from the roster ate nothing in {TICKS} ticks \
             on classified grass. Either SHEEP no longer carries the EatBlockGoal \
             row, or goals_for does not reach it: with BABY_INTERVAL = {} the \
             chance of a genuinely installed goal never firing is vanishing",
            EatBlockGoal::BABY_INTERVAL
        );

        let cow = grazes("cow", TICKS);
        assert_eq!(
            cow, 0,
            "control: a cow on the same grass, ticked the same {TICKS} times, ate \
             {cow} times. `AbstractCow.java:41-48` registers no EatBlockGoal, so \
             something installs grazing regardless of the table and the sheep \
             measurement above is not attributable to its row"
        );
    }
}
