//! Goal sets for the melee monsters: the zombie family, the skeleton family's
//! melee fallback, spiders and creepers.
//!
//! # What it is
//!
//! One [`Registration`] table per species, transcribed from that species'
//! `registerGoals()` in `.cache/mc/26.2/src/net/minecraft/world/entity/`. Owned
//! by issue [#226]; extend it here and nothing else in the tree changes.
//!
//! # How to change it
//!
//! Add or edit a table, then add the species path to [`SPECIES`] and an arm to
//! [`lookup`]. `SPECIES` is not decoration — `roster`'s own gates iterate it, so
//! a species missing from it is a species nothing checks, and
//! `every_advertised_species_resolves_to_a_real_table` fails if the two
//! disagree.
//!
//! Every table below is gated against the jar by
//! [`super::tests`](super#tests)'s multiset check, which compares the *whole*
//! table — including [`Coverage::Missing`] rows — against a hand-transcribed
//! copy of the cited `addGoal` block. Change a priority here and that gate fails
//! until the citation is re-read, which is the point.
//!
//! # Known gaps, all disclosed in the tables
//!
//! * **No ranged goals exist in this repo**, so the skeleton family gets the
//!   melee half of `reassessWeaponGoal()` unconditionally rather than swapping to
//!   a bow. [`GoalSelector::remove`](crate::ai::GoalSelector::remove) now exists
//!   for that swap; the bow goal itself is issue #227's.
//! * **Nothing in the sim is a villager, iron golem, turtle or armadillo**, so
//!   every target registration naming one is [`Coverage::Missing`] rather than a
//!   goal that would search for an entity class that cannot be spawned.
//! * **`HurtByTargetGoal.setAlertOthers` is not modelled** — our
//!   `HurtByTargetGoal` retaliates but never propagates anger to nearby mobs.
//!   That needs the sim-side census issue #233 owns.
//!
//! [#226]: https://github.com/matteopolak/lodestone/issues/226

use crate::ai::goal::Goal;
use crate::ai::goals::{MeleeAttackGoal, RandomStrollGoal};

use super::{
    Registration, Selector, SpeciesContext, avoid_entity, float_goal, hurt_by_target,
    look_at_player_8, melee_attack, nearest_attackable_target, random_look_around, stroll, swell,
};

/// Every species this family claims. Iterated by `roster`'s invariant gates.
pub const SPECIES: &[&str] = &[
    "zombie",
    "husk",
    "creeper",
    "spider",
    "cave_spider",
    "skeleton",
    "stray",
    "bogged",
    "wither_skeleton",
];

/// Resolves a species path to its table, or `None` if this family does not claim
/// it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        // `Husk` declares no `registerGoals` of its own
        // (`monster/zombie/Husk.java`), so it inherits `Zombie`'s verbatim.
        "zombie" | "husk" => Some(ZOMBIE),
        "creeper" => Some(CREEPER),
        // `CaveSpider` likewise inherits `Spider`'s.
        "spider" | "cave_spider" => Some(SPIDER),
        // `Skeleton`, `Stray` and `Bogged` declare no `registerGoals`.
        // `WitherSkeleton` *does* (`monster/skeleton/WitherSkeleton.java:38-41`):
        // it adds `targetSelector.addGoal(3, NearestAttackableTargetGoal(AbstractPiglin))`
        // and then calls `super.registerGoals()`. That single extra row would be
        // `Coverage::Missing` either way — no piglin can exist in this sim — so
        // it contributes nothing to `goals_for` and the wither shares the base
        // table here. It is therefore the one species in this family whose table
        // is *not* a complete transcription; #226 should split it out when
        // piglins exist.
        "skeleton" | "stray" | "bogged" | "wither_skeleton" => Some(SKELETON),
        _ => None,
    }
}

/// `monster/Creeper.java:65-74`.
///
/// The reference shape for this family: a creeper's swell and detonation already
/// reach a real client (`crates/protocol/v770/tests/server_creeper_metadata_and_explode.rs`),
/// so this is the one table whose end-to-end path was proven before the roster
/// existed.
///
/// Note what vanilla's numbers buy over the hand-written baseline this replaced.
/// That baseline numbered its own goals 0/1/2 and had to register `SwellGoal` at
/// **-1** to get "swell preempts melee", with a comment explaining the private
/// scale. Vanilla's own numbers — swell 2, melee 4 — express the same precedence
/// directly and can be checked against the jar.
pub const CREEPER: &[Registration] = &[
    Registration::goal(1, "FloatGoal", float_goal),
    Registration::goal(2, "SwellGoal", swell),
    // `AvoidEntityGoal<>(this, Ocelot.class, 6.0F, 1.0, 1.2)`. Vanilla's last two
    // arguments are walk and *sprint* speed modifiers; our `AvoidEntityGoal` has
    // a single speed, so it takes the walk tier (1.0) and the panic-sprint tier
    // is not modelled.
    Registration::goal(3, "AvoidEntityGoal(Ocelot)", avoid_entity),
    // Our `AvoidEntityGoal` is class-agnostic and the server's `avoided_species`
    // feed already reports both ocelot and cat for a creeper.
    Registration::covered(
        Selector::Goal,
        3,
        "AvoidEntityGoal(Cat)",
        "AvoidEntityGoal(Ocelot)",
    ),
    Registration::goal(4, "MeleeAttackGoal", melee_attack),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll_0_8),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(6, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::target(2, "HurtByTargetGoal", hurt_by_target),
];

/// `monster/spider/Spider.java:57-67`.
pub const SPIDER: &[Registration] = &[
    Registration::goal(1, "FloatGoal", float_goal),
    // `AvoidEntityGoal<>(this, Armadillo.class, 6.0F, 1.0, 1.2, e -> !e.isScared())`.
    // The `isScared` filter is not modelled — see `mobs.rs`'s `avoided_species`,
    // which discloses it can only make a spider flee slightly more often.
    Registration::goal(2, "AvoidEntityGoal(Armadillo)", avoid_entity),
    // `LeapAtTargetGoal(this, 0.4F)` — no equivalent goal exists; a spider will
    // walk into melee range instead of pouncing.
    Registration::missing(Selector::Goal, 3, "LeapAtTargetGoal"),
    // `Spider.SpiderAttackGoal` extends `MeleeAttackGoal`; its only addition is
    // refusing to attack while the spider has a passenger, which this sim has no
    // notion of.
    Registration::goal(4, "Spider.SpiderAttackGoal", melee_attack),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll_0_8),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(6, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    // `Spider.SpiderTargetGoal<>(this, Player.class)` extends
    // `NearestAttackableTargetGoal`, adding only a daylight brightness penalty to
    // the search radius.
    Registration::target(2, "Spider.SpiderTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "Spider.SpiderTargetGoal(IronGolem)"),
];

/// `monster/zombie/Zombie.java:112-116` plus `addBehaviourGoals` at `:119-130`,
/// which `registerGoals` calls at `:116` — the registrations are split across two
/// methods and both halves belong to this table.
///
/// A zombie gets **no** `FloatGoal`, which is not an omission: vanilla does not
/// register one, because zombies sink and walk along the bottom.
pub const ZOMBIE: &[Registration] = &[
    // `Zombie.ZombieAttackTurtleEggGoal(this, 1.0, 3)` — a block-breaking goal
    // with no turtle eggs to break.
    Registration::missing(Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // `SpearUseGoal<>(this, 1.0, 1.0, 10.0F, 2.0F)` — new in 26.2, and a ranged
    // goal, so it belongs to #227 rather than here.
    Registration::missing(Selector::Goal, 2, "SpearUseGoal"),
    // `ZombieAttackGoal(this, 1.0, false)` extends `MeleeAttackGoal`, adding only
    // the raised-arms metadata flag while it runs.
    Registration::goal(3, "ZombieAttackGoal", melee_attack),
    // `MoveThroughVillageGoal(this, 1.0, true, 4, this::canBreakDoors)` — needs
    // village POI data that does not exist here.
    Registration::missing(Selector::Goal, 6, "MoveThroughVillageGoal"),
    Registration::goal(7, "WaterAvoidingRandomStrollGoal", stroll),
    // `HurtByTargetGoal(this).setAlertOthers(ZombifiedPiglin.class)`. The
    // retaliation is modelled; the alert propagation is not (#233).
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(AbstractVillager)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    Registration::missing(Selector::Target, 5, "NearestAttackableTargetGoal(Turtle)"),
];

/// `monster/skeleton/AbstractSkeleton.java:76-86`, plus the priority-4 weapon
/// goal that `reassessWeaponGoal()` installs at `:144`/`:146` rather than in
/// `registerGoals`.
///
/// That priority-4 slot is the reason [`GoalSelector::remove`] exists: vanilla
/// removes *both* candidate goals and re-adds exactly one every time the
/// skeleton's held item changes (`:132-146`). This table installs the melee half
/// unconditionally, because `RangedBowAttackGoal` has no equivalent here and a
/// skeleton that never attacks is worse than one that punches. Issue #227 turns
/// this row into the real swap.
///
/// [`GoalSelector::remove`]: crate::ai::GoalSelector::remove
pub const SKELETON: &[Registration] = &[
    // `RestrictSunGoal(this)` and `FleeSunGoal(this, 1.0)` both need a
    // sky-light-and-daytime query the AI seam does not expose, so a skeleton
    // does not seek shade. Daylight *burning* is separately #226's.
    Registration::missing(Selector::Goal, 2, "RestrictSunGoal"),
    Registration::missing(Selector::Goal, 3, "FleeSunGoal"),
    Registration::goal(3, "AvoidEntityGoal(Wolf)", avoid_entity),
    // `reassessWeaponGoal()` at `:146`: the non-bow branch. Vanilla's
    // `meleeGoal` field is `new MeleeAttackGoal(this, 1.2, false)` (`:56`), and
    // the bow branch at `:144` installs `bowGoal`,
    // `new RangedBowAttackGoal<>(this, 1.0, 20, 15.0F)` (`:55`).
    Registration::goal(4, "MeleeAttackGoal", melee_attack_1_2),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(6, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(Turtle)"),
];

/// `WaterAvoidingRandomStrollGoal(this, 0.8)` — creeper (`Creeper.java:70`) and
/// spider (`Spider.java:62`).
fn stroll_0_8(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RandomStrollGoal::new(ctx.speed * 0.8))
}

/// `MeleeAttackGoal(this, 1.2, false)` — the skeleton's melee goal
/// (`monster/skeleton/AbstractSkeleton.java:56`), faster than the 1.0 every other
/// species in this family uses.
fn melee_attack_1_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MeleeAttackGoal::new(ctx.speed * 1.2, ctx.attack_reach))
}
