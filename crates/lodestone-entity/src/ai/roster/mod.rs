//! Per-species goal sets — which [`Goal`]s a species gets, at which priorities.
//!
//! # What it is
//!
//! A pure, world-free lookup from a species path (`"cow"`, `"creeper"`) to the
//! list of goal registrations vanilla's own goal-registration method performs for it, so
//! `lodestone_server::mobs::MobSim::spawn_species` can install a real per-species
//! brain with one loop instead of a hand-written match. Entry point:
//! [`goals_for`].
//!
//! # Why the table is not just a `match` returning boxed goals
//!
//! A roster entry is a `&'static [Registration]`, and a [`Registration`] carries
//! a **descriptive name** (`data/goal-names.tsv` maps each to the reference
//! registration it stands for; a test rejects class-style or unmapped names)
//! alongside its priority, plus a [`Coverage`] saying
//! whether this repo builds an equivalent, lacks one, or already covers it with a
//! sibling row. Three things fall out of that shape, and all three were the
//! point:
//!
//! * **The expected value in a test originates outside the code under test.** A
//!   gate can assert that a species' table — every row, whatever its coverage —
//!   equals the exact multiset of goal registrations vanilla's own species
//!   class performs.
//!   Asserting only the goals we build would be satisfied by any subset,
//!   including a wrong one, and asserting them against numbers copied out of
//!   this file is the closed loop CLAUDE.md's evidence standards are about.
//! * **An omission cannot go quiet.** Vanilla registers seven goals on a spider
//!   and this repo implements six of them. [`Coverage::Missing`] says which one,
//!   in the table, at its real vanilla priority — so implementing
//!   `PounceGoal` later is a one-row change the multiset gate already
//!   covers, rather than a discovery.
//! * **The two priority namespaces stay legible.** [`Selector`] records whether
//!   vanilla put a registration on `goalSelector` or `targetSelector`, so the
//!   jar's numbers are transcribed verbatim rather than shifted by an offset
//!   convention. See [`MobAi`](super::MobAi) for why they can share one
//!   `GoalSelector` at all, and [`goals_for`] for the ordering that makes it
//!   equivalent.
//!
//! # How to add a species
//!
//! Edit **one** file: the family module your species belongs to
//! ([`hostile_melee`], [`ranged`], [`passive`], [`neutral`], [`specialist`]).
//! Add an arm to its `lookup` returning your table. Nothing else in the tree
//! changes — not this file, not `mobs.rs`, not a registration list. All five
//! family modules are already declared and already consulted by [`FAMILIES`],
//! precisely so that five parallel roster units never contend on a shared file.
//!
//! If your species needs a goal type that does not exist yet, add it to
//! [`goals`](super::goals) and register it with [`Registration::goal`]; if it
//! needs one you are not implementing, use [`Registration::missing`] and say why
//! in the comment. Do not omit the row — an absent row is indistinguishable from
//! a vanilla registration nobody noticed, which is exactly the state this module
//! replaced.
//!
//! # What it deliberately does not carry
//!
//! **Not `MobCategory`, and not hostility.** There are two independent
//! `MobCategory` types in this workspace ([`crate::spawn::MobCategory`], 8
//! variants, and `lodestone_server::mob_spawn::MobCategory`, 7 variants and a
//! different `check_despawn` signature), the server uses its own, and unifying
//! them is out of scope here. This table is keyed on the species **path string**
//! and returns goals only, so it takes no side in that fork and needs no import
//! from either. Spawn category and despawn persistence stay where they are, in
//! `mobs.rs`.
//!
//! **Not perception data.** "Which species does a spider flee" and "which items
//! tempt a pig" answer *what the mob can see*, are fed to
//! [`MobController`](super::MobController) by the server's own census, and
//! already live in `mobs.rs` next to that feed (`avoided_species`, `tempt_food`).
//! The roster only decides that a spider gets an `FleeEntityGoal` at all.
//!
//! # Every registration table is a `static`, never a `const`
//!
//! Several gates here identify a table by comparing `as_ptr()` — "the horse, the
//! donkey and the mule share one table", "the elder guardian has its own despite
//! identical rows". That comparison is only meaningful against an item with a
//! single address, and a `const` does not have one: the language re-promotes it
//! at every use site, and whether two promotions are deduplicated depends on the
//! codegen backend and on whether the uses land in the same compilation unit.
//! [`FALLBACK`] carries the measurement — as a `const` it came back at two
//! different addresses under this workspace's Cranelift debug profile, and
//! `is_fallback` reported a real unclaimed species as claimed.
//!
//! The other tables were left as `const` when that was fixed, which made them
//! latent instances of the same bug rather than safe ones: their gates pass only
//! because the promotions happen to be folded today. They are all `static` now,
//! so the property the pointer comparisons need holds by construction. Declare
//! any new table the same way.

use super::goal::Goal;
use super::goals::{
    FleeEntityGoal, MateGoal, StayAfloatGoal, RetaliateGoal, PounceGoal, WatchPlayerGoal,
    MeleeStrikeGoal, ReturnToHomeAreaGoal, NearestTargetGoal, DefendOwnerGoal,
    AssistOwnerGoal, IdleGlanceGoal, WanderGoal, ResetUniversalAngerGoal,
    SitOnCommandGoal, FuseGoal,
};
use super::target_class::TargetClass;

pub mod aerial;
pub mod amphibious;
pub mod aquatic;
pub mod equine;
pub mod hostile_melee;
pub mod neutral;
pub mod passive;
#[cfg(test)]
pub mod probe;
pub mod ranged;
pub mod specialist;

/// Which of a vanilla mob's two goal selectors a registration was made on.
///
/// Vanilla's own mob base type owns a goal selector and a target selector with
/// **independent**
/// priority numbering, so a creeper's `StayAfloatGoal` at goal-priority 1 and its
/// `NearestTargetGoal` at target-priority 1 are not competing
/// (vanilla's own creeper registration puts them at goal-priority 1 and
/// target-priority 1 respectively). Recording which one a number came from
/// is what lets the jar's numbers be copied verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    /// Vanilla's own goal selector — movement, looking, jumping, attacking.
    Goal,
    /// Vanilla's own target selector — goals that pick *who* to attack.
    Target,
}

/// The per-mob numbers a goal constructor needs, resolved by the caller.
///
/// Vanilla's own goal registrations take speed arguments that are
/// **multipliers** on the mob's
/// `MOVEMENT_SPEED` attribute, not absolute speeds: a panic-goal registration
/// of `2.0` on a
/// cow means "twice this cow's walking
/// speed". Our goals take an absolute blocks-per-tick figure, so every `build`
/// below multiplies [`speed`](Self::speed) by the jar's own factor. Keeping the
/// factor visible at the call site is the point — a flattened absolute number
/// could not be checked against the jar.
#[derive(Debug, Clone, Copy)]
pub struct SpeciesContext {
    /// The mob's `minecraft:movement_speed` attribute value, in blocks per tick
    /// — what `MobSim` already calls `step_per_tick`.
    pub speed: f64,
    /// How close a [`MeleeStrikeGoal`] must be to connect, in blocks.
    ///
    /// Vanilla derives this from the attacker's and target's bounding boxes
    /// (vanilla's own attack-bounding-box getter); nothing here models the target's box at
    /// goal-construction time, so the caller passes the flat figure it was
    /// already using. A disclosed approximation, not a guess about vanilla.
    pub attack_reach: f64,
}

impl SpeciesContext {
    /// The context for a mob of the given speed, with the melee reach
    /// `MobSim::spawn_species` has always used.
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self {
            speed,
            attack_reach: 2.0,
        }
    }
}

/// Builds one goal instance for a mob described by `ctx`.
pub type Build = fn(&SpeciesContext) -> Box<dyn Goal>;

/// What this repo does about one vanilla registration.
///
/// The distinction between the two non-building variants is the reason this is
/// an enum rather than an `Option<Build>`. "We have no such goal" and "our
/// existing goal already does this one's job" are both *not building anything
/// here*, and collapsing them loses the only information that says whether the
/// species' behaviour is actually incomplete.
#[derive(Debug, Clone, Copy)]
pub enum Coverage {
    /// We build our own equivalent of this registration.
    Modelled(Build),
    /// Vanilla registers it and this repo has no equivalent goal type. The
    /// species' behaviour is genuinely missing this piece.
    Missing,
    /// Vanilla registers it separately, but one of *our* goals in the same table
    /// already covers it — the string names the sibling row.
    ///
    /// This happens because several of our goals are class-agnostic where
    /// vanilla's are generic over a target class. A creeper gets two
    /// `FleeEntityGoal` registrations, one for `Ocelot` and one for `Cat`
    /// (vanilla's own creeper registration); our `FleeEntityGoal` has no class
    /// parameter at all and flees whatever
    /// [`MobController::avoid_threat`](super::MobController::avoid_threat)
    /// reports, which the server's own `avoided_species` feed already resolves
    /// to *both* classes. So one instance of ours is the whole behaviour, and
    /// building a second would give the creeper two goals fighting over MOVE at
    /// equal priority.
    CoveredBy(&'static str),
}

/// One vanilla `addGoal` call, transcribed.
#[derive(Debug, Clone, Copy)]
pub struct Registration {
    /// Which selector vanilla registered it on.
    pub selector: Selector,
    /// Vanilla's own priority number, unshifted (lower = higher precedence).
    pub priority: i32,
    /// The vanilla class name exactly as it appears in the cited `addGoal` line,
    /// e.g. `"wander_dry"`. This is the join key a gate uses
    /// to compare a table against the jar, so it must match the jar's spelling,
    /// not ours.
    pub name: &'static str,
    /// What this repo does about it.
    pub coverage: Coverage,
}

impl Registration {
    /// A `goalSelector` registration we implement.
    #[must_use]
    pub const fn goal(priority: i32, name: &'static str, build: Build) -> Self {
        Self {
            selector: Selector::Goal,
            priority,
            name,
            coverage: Coverage::Modelled(build),
        }
    }

    /// A `targetSelector` registration we implement.
    #[must_use]
    pub const fn target(priority: i32, name: &'static str, build: Build) -> Self {
        Self {
            selector: Selector::Target,
            priority,
            name,
            coverage: Coverage::Modelled(build),
        }
    }

    /// A vanilla registration this repo has no equivalent goal for.
    #[must_use]
    pub const fn missing(selector: Selector, priority: i32, name: &'static str) -> Self {
        Self {
            selector,
            priority,
            name,
            coverage: Coverage::Missing,
        }
    }

    /// A vanilla registration already covered by the sibling row `by`.
    #[must_use]
    pub const fn covered(
        selector: Selector,
        priority: i32,
        name: &'static str,
        by: &'static str,
    ) -> Self {
        Self {
            selector,
            priority,
            name,
            coverage: Coverage::CoveredBy(by),
        }
    }

    /// The constructor for our equivalent, if this row builds one.
    #[must_use]
    pub const fn build(&self) -> Option<Build> {
        match self.coverage {
            Coverage::Modelled(b) => Some(b),
            Coverage::Missing | Coverage::CoveredBy(_) => None,
        }
    }
}

// -- shared builders ---------------------------------------------------------
//
// Every family needs these, and a `fn` item is the only thing that can live in a
// `const` table. Each multiplies `ctx.speed` by the jar's own factor, so the
// factor is auditable at the registration site.

/// `StayAfloatGoal` takes no arguments in vanilla.
pub fn float_goal(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(StayAfloatGoal)
}

/// `IdleGlanceGoal` takes no arguments.
pub fn random_look_around(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(IdleGlanceGoal::new())
}

/// Vanilla's default `WatchPlayerGoal` probability: its own three-argument
/// constructor forwards `0.02F`
/// to its own four-argument constructor, and
/// every registration in this roster uses that three-argument form.
const LOOK_PROBABILITY: f32 = 0.02;

/// The look distance `8.0` — every hostile registration in
/// the roster uses it (the creeper, spider, abstract skeleton and zombie
/// families all register it this way).
///
/// There are two of these rather than one parameterised builder because a
/// [`Registration`] table is a `const`, so `build` must be a plain `fn` item — a
/// closure capturing the distance is not a function pointer. Two named constants
/// also read better against the jar than one call with a magic argument.
pub fn look_at_player_8(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WatchPlayerGoal::new(8.0, LOOK_PROBABILITY))
}

/// The look distance `3.0` — the vindicator's registration.
pub fn look_at_player_3(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WatchPlayerGoal::new(3.0, LOOK_PROBABILITY))
}

/// The look distance `6.0` — every farm-animal registration
/// uses it (the cow, sheep, pig and chicken families all register it this
/// way).
pub fn look_at_player_6(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WatchPlayerGoal::new(6.0, LOOK_PROBABILITY))
}

/// The stroll speed factor `1.0` — the most common registration in
/// the roster (the zombie, abstract skeleton, cow, sheep, pig and chicken
/// families all register it).
///
/// Our `WanderGoal` is the plain stroll; vanilla's water-avoiding subclass
/// only biases the candidate position away from water, which the A\* the goal
/// drives does not model. A disclosed simplification shared by every species that
/// registers it, which is why it is not a per-family `Coverage::Missing`.
pub fn stroll(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WanderGoal::new(ctx.speed))
}

/// The avoid radius `6.0` and walk-speed modifier `1.0` — every registration in the
/// roster uses the same figures
/// (the creeper, spider and abstract skeleton families all register it this
/// way).
///
/// Vanilla's fourth and fifth arguments are separate *walk* and *sprint* speed
/// modifiers, switching to the sprint tier once the threat is very close; our
/// `FleeEntityGoal` has one speed, so it takes the walk tier and the sprint tier
/// is not modelled. The `6.0` radius is the same figure `mobs.rs`'s `AVOID_RANGE`
/// already cites for the perception feed.
pub fn avoid_entity(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeEntityGoal::new(6.0, ctx.speed))
}

/// The melee speed factor `1.0` — the creeper's own registration, and
/// via subclasses the zombie's own attack-goal registration
/// and the spider's own attack-goal registration (which passes `1.0` up to
/// `MeleeStrikeGoal`).
///
/// The skeleton's is `1.2` and has its own builder in
/// [`hostile_melee`](hostile_melee).
pub fn melee_attack(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MeleeStrikeGoal::new(ctx.speed, ctx.attack_reach))
}

/// `FuseGoal(this)` — creeper only. Takes no
/// arguments.
pub fn swell(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FuseGoal::new())
}

/// `RetaliateGoal(this)` — a target-selector goal, no arguments.
///
/// Several registrations chain a same-owner alert-others flag (the zombie
/// and zombified-piglin families both do); that
/// propagation is not modelled anywhere yet.
pub fn hurt_by_target(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RetaliateGoal::new())
}

/// `DefendOwnerGoal(this)` — a target-selector goal, no arguments.
/// Retaliates against whoever last hurt this mob's owner. See
/// [`DefendOwnerGoal`]'s own doc comment.
pub fn owner_hurt_by_target(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(DefendOwnerGoal::new())
}

/// `AssistOwnerGoal(this)` — a target-selector goal, no arguments. Joins
/// whatever fight this mob's owner just started. See
/// [`AssistOwnerGoal`]'s own doc comment.
pub fn owner_hurt_target(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AssistOwnerGoal::new())
}

/// Vanilla's own player-targeting target-selector
/// goal.
///
/// Ours is not generic over a target class: it asks
/// [`MobController::find_nearest_target`](super::MobController::find_nearest_target),
/// which the server resolves to the nearest player. So it stands in for the
/// player-targeted registration only, and every registration naming another class
/// is a [`Coverage::Missing`] row.
pub fn nearest_attackable_target(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::new())
}

/// Hunts the nearest villager or wandering trader without needing to see it.
pub fn target_villager_unseen(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::Villager, false))
}

/// Hunts the nearest iron golem it can see.
pub fn target_iron_golem(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::IronGolem, true))
}

/// Hunts the nearest baby turtle out of water that it can see.
pub fn target_baby_turtle(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::BabyLandTurtle, true))
}

/// Hunts the nearest axolotl it can see.
pub fn target_axolotl(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::Axolotl, true))
}

/// Hunts the nearest piglin or piglin brute it can see.
pub fn target_piglin(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::Piglin, true))
}

/// Hunts the nearest endermite it can see.
pub fn target_endermite(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::Endermite, true))
}

/// Hunts the nearest skeleton variant without needing to see it.
pub fn target_skeleton_unseen(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::Skeleton, false))
}

/// Hunts the nearest hostile mob it can see.
pub fn target_hostile(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::Hostile, true))
}

/// An untamed hunter's random pick among sheep, rabbits and foxes.
pub fn untamed_target_prey(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::WolfPrey, false).untamed_only())
}

/// An untamed hunter's random pick of a baby turtle out of water.
pub fn untamed_target_baby_turtle(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::BabyLandTurtle, false).untamed_only())
}

/// An untamed hunter's random pick of a rabbit.
pub fn untamed_target_rabbit(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(NearestTargetGoal::of_class(TargetClass::Rabbit, false).untamed_only())
}

/// Watches the nearest other guardian: distance `12.0`, chance `0.01`.
pub fn look_at_guardian(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WatchPlayerGoal::of_class(12.0, 0.01, TargetClass::Guardian))
}

/// Walks back inside the home radius at the mob's full speed.
pub fn move_towards_restriction(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(ReturnToHomeAreaGoal::new(ctx.speed))
}

/// The guardian's form of the same goal, which also holds the LOOK flag.
pub fn guardian_move_towards_restriction(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(ReturnToHomeAreaGoal::new(ctx.speed).claiming_look())
}

/// Universal-anger reset that also alerts the surrounding group.
pub fn reset_universal_anger_alerting(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(ResetUniversalAngerGoal::new(true))
}

/// Universal-anger reset for the mob alone.
pub fn reset_universal_anger_solo(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(ResetUniversalAngerGoal::new(false))
}

/// A pounce with vertical velocity `0.4` (spider, wolf).
pub fn leap_0_4(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PounceGoal::new(0.4))
}

/// A pounce with vertical velocity `0.3` (cat).
pub fn leap_0_3(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PounceGoal::new(0.3))
}

/// The breed speed factor `1.0` — every farm animal registers it at exactly that value
/// (the cow, sheep, pig and chicken families), only the priority
/// differs.
pub fn breed_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MateGoal::new(ctx.speed))
}

/// `SitOnCommandGoal(this)` — the wolf, the cat and the parrot all register
/// this at goal priority 2, and all three with no constructor arguments.
/// One shared builder because the goal itself
/// carries every per-species difference already — see its own doc comment.
pub fn sit_when_ordered(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(SitOnCommandGoal)
}

/// The registrations every species without a family entry gets: wander and look
/// around, which is what `spawn_species` gave *every* species before this module
/// existed.
///
/// Not an empty slice. An unknown mob that stands perfectly still is a worse
/// answer than one that wanders — vanilla has no species with an empty
/// goal registration — and the fallback being explicit here rather than a
/// leftover branch in `mobs.rs` is the whole point of the seam. The two goals
/// carry no vanilla citation because they are not a transcription of any one
/// species; `vanilla` is `"—"` so a multiset gate can never mistake the fallback
/// for a real table.
///
/// **A `static`, not a `const`.** [`is_fallback`] identifies the fallback table
/// by comparing `table.as_ptr()` against this item's own address, and a `const`
/// item has no single address — the language re-promotes (and, depending on
/// the codegen backend, may or may not deduplicate) a fresh instance at every
/// use site. Measured: under this workspace's Cranelift debug profile, the
/// promoted instance `registrations_for`'s fallback arm returns and the one
/// `FALLBACK.as_ptr()` names directly in a *different* compilation unit came
/// back at two different addresses, so `is_fallback(registrations_for("llama"))`
/// — llama being a real 26.2 species no family claims — read as `false`.
/// `static` has exactly one address for the item's whole `'static` lifetime,
/// which is the property `is_fallback`'s pointer comparison actually needs.
pub static FALLBACK: &[Registration] = &[
    Registration::goal(5, "—", |ctx| {
        Box::new(WanderGoal::new(ctx.speed))
    }),
    Registration::goal(6, "—", random_look_around),
];

/// A family module's species lookup.
pub type FamilyLookup = fn(&str) -> Option<&'static [Registration]>;

/// Every family module, consulted in order by [`registrations_for`].
///
/// Fixed at five entries by design: the five roster units
/// (hostile melee, ranged, passive, specialists,
/// neutral) each own exactly one of the modules below, and this array is already
/// complete, so none of them has to edit this file. A species must appear in at
/// most one family — the first match wins, and
/// `no_species_is_claimed_by_two_families` fails if two claim the same one, which
/// is the failure mode of five people adding arms in parallel.
pub const FAMILIES: [FamilyLookup; 9] = [
    hostile_melee::lookup,
    ranged::lookup,
    passive::lookup,
    neutral::lookup,
    specialist::lookup,
    equine::lookup,
    aquatic::lookup,
    aerial::lookup,
    amphibious::lookup,
];

/// The full registration table for `species`, or [`FALLBACK`] if no family
/// claims it.
///
/// `species` is a resource-key **path** (`"cow"`, not `"minecraft:cow"`), which
/// is how the server's own species-keyed perception tables are keyed too.
#[must_use]
pub fn registrations_for(species: &str) -> &'static [Registration] {
    for family in FAMILIES {
        if let Some(table) = family(species) {
            return table;
        }
    }
    FALLBACK
}

/// The goals to install on a freshly spawned `species`, in the order they must be
/// added.
///
/// **Target-selector registrations come first**, then goal-selector ones, each
/// group in the table's own order. That ordering is what makes one
/// `GoalSelector` equivalent to vanilla's two: vanilla ticks `targetSelector`
/// before `goalSelector` so a target acquired this tick is visible to a movement
/// goal in the same tick, and `GoalSelector`'s `update`/`tick_running` iterate in
/// insertion order. See [`MobAi`](super::MobAi) for the full argument and the
/// invariant that keeps the two priority namespaces from colliding.
///
/// Priorities are vanilla's own numbers, unshifted. Unmodelled registrations are
/// skipped.
///
/// # Brain-driven species take the early return
///
/// Roughly 20 concrete 26.2 mobs have **no goal registration at all** — a warden's
/// own class contains no goal registration anywhere — because their AI is
/// a [`Brain`](crate::brain::Brain) instead. Before this, those species fell
/// through every family lookup to [`FALLBACK`] and got two generic stroll/look
/// goals: plausible on screen, and not the mob's behaviour in any sense.
///
/// They now get a single [`BrainGoal`](crate::brain::BrainGoal) and **nothing
/// else**. The `return` is not an optimisation; installing the fallback stroll
/// alongside a brain would put two independent writers on movement, and the brain
/// would lose arbitration on the ticks it happened to be between walk targets.
/// This is also the join that makes the Brain system reachable at all: no host
/// learns a new call, because `goals_for` is already the function
/// `MobSim::spawn_species` calls.
#[must_use]
pub fn goals_for(species: &str, ctx: &SpeciesContext) -> Vec<(i32, Box<dyn Goal>)> {
    if let Some(brain) = crate::brain::brain_for(species) {
        let mut out: Vec<(i32, Box<dyn Goal>)> = vec![(BRAIN_PRIORITY, Box::new(brain))];
        // A villager opens a door on its path and shuts it behind itself; the
        // goal takes no flags, so it runs beside the brain.
        if species == "villager" {
            out.push((BRAIN_PRIORITY, Box::new(crate::ai::door::OpenDoorGoal::new(true))));
        }
        return out;
    }
    let table = registrations_for(species);
    let mut out = Vec::new();
    for want in [Selector::Target, Selector::Goal] {
        for r in table.iter().filter(|r| r.selector == want) {
            if let Some(build) = r.build() {
                out.push((r.priority, build(ctx)));
            }
        }
    }
    out
}

/// The priority a [`BrainGoal`](crate::brain::BrainGoal) is installed at.
///
/// `0`, i.e. the highest, and it is the only goal a brain mob gets — so the number
/// is about *insulation* rather than arbitration. If a later species needs a real
/// goal alongside its brain (vanilla does this: a `Villager` still registers
/// `StayAfloatGoal` and a trade-with-player goal on its goal selector), that goal takes
/// its own vanilla priority and loses `MOVE`/`LOOK` to the brain, which is the
/// vanilla outcome.
pub const BRAIN_PRIORITY: i32 = 0;

/// Whether `table` is the shared [`FALLBACK`] rather than a species' own entry.
///
/// Compares the backing pointer, not the contents: a family could legitimately
/// return a table that happens to *equal* the fallback, and the question here is
/// always "did any family claim this species".
#[must_use]
pub fn is_fallback(table: &[Registration]) -> bool {
    std::ptr::eq(table.as_ptr(), FALLBACK.as_ptr()) && table.len() == FALLBACK.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::goal::{Flag, FlagSet};

    /// Every species any family claims. Used by the invariant gates below, which
    /// must cover species added later without being edited.
    fn all_claimed_species() -> Vec<&'static str> {
        let mut v = Vec::new();
        v.extend(hostile_melee::SPECIES);
        v.extend(ranged::SPECIES);
        v.extend(passive::SPECIES);
        v.extend(neutral::SPECIES);
        v.extend(specialist::SPECIES);
        v
    }

    /// Each family's advertised `SPECIES` list must actually resolve through
    /// `registrations_for`, or the invariant gates below silently measure
    /// nothing. This is the precondition control for every test in this module.
    #[test]
    fn every_advertised_species_resolves_to_a_real_table() {
        let claimed = all_claimed_species();
        assert!(
            !claimed.is_empty(),
            "no family claims any species, so every gate below is vacuous"
        );
        for s in claimed {
            let table = registrations_for(s);
            assert!(
                !is_fallback(table),
                "{s} is advertised by a family but resolves to FALLBACK — its \
                 `lookup` arm and its SPECIES list disagree"
            );
            assert!(!table.is_empty(), "{s}'s table is empty");
        }
    }

    /// `data/goal-names.tsv`: `name<TAB>reference registration`, or `-` for a goal the reference has no counterpart to.
    const GOAL_NAMES: &str = include_str!("../../../data/goal-names.tsv");

    /// Whether `name` is lowercase snake_case with an optional `owner.` prefix
    /// and an optional `(qualifier)` suffix.
    fn is_descriptive(name: &str) -> bool {
        let word = |w: &str| {
            !w.is_empty()
                && w.starts_with(|c: char| c.is_ascii_lowercase())
                && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        let (head, qualifier) = match name.split_once('(') {
            Some((h, q)) => (h, q.strip_suffix(')')),
            None => (name, Some("x")),
        };
        let Some(qualifier) = qualifier else { return false };
        let qualifier_ok = qualifier == "x" || qualifier.split(',').all(word);
        let mut parts = head.split('.');
        let (first, second, third) = (parts.next(), parts.next(), parts.next());
        third.is_none() && first.is_some_and(word) && second.is_none_or(word) && qualifier_ok
    }

    /// Control: the check rejects the reference's class-name style.
    #[test]
    fn the_name_shape_check_rejects_class_style_names() {
        for bad in ["WaterAvoidingRandomStrollGoal", "Ghast.FaceTargetGoal", "LureGoal(PIG_FOOD)", "flee_entity(Wolf)", "a.b.c", ""] {
            assert!(!is_descriptive(bad), "{bad} must be rejected");
        }
        for good in ["wander_dry", "ghast.face_target", "lure(pig_food)", "nearest_target(player,angry_at)"] {
            assert!(is_descriptive(good), "{good} must be accepted");
        }
    }

    /// Every registration in every table has a descriptive name, an entry in
    /// the goal-name file, and the file has no entry no table uses. A new
    /// class-style name fails the first check; a new descriptive name that was
    /// not mapped to its reference registration fails the second.
    #[test]
    fn every_registration_name_is_descriptive_and_mapped_to_its_reference() {
        let mut mapped = std::collections::BTreeMap::new();
        for line in GOAL_NAMES.lines().filter(|l| !l.is_empty()) {
            let (name, reference) = line.split_once('\t').expect("name<TAB>reference");
            assert!(is_descriptive(name), "goal-names.tsv entry {name} is not descriptive");
            assert!(mapped.insert(name, reference).is_none(), "{name} is mapped twice");
        }
        let references: Vec<_> = mapped.values().filter(|r| **r != "-").collect();
        let distinct: std::collections::BTreeSet<_> = references.iter().collect();
        assert_eq!(distinct.len(), references.len(), "two names map to one reference registration");
        let mut used = std::collections::BTreeSet::new();
        let every_species = all_claimed_species()
            .into_iter()
            .chain(equine::SPECIES.iter().copied())
            .chain(["cod", "pufferfish", "squid", "bat", "phantom", "turtle"]);
        for species in every_species {
            for r in registrations_for(species) {
                assert!(is_descriptive(r.name), "{species}: {} is not a descriptive name", r.name);
                assert!(mapped.contains_key(r.name), "{species}: {} is missing from goal-names.tsv", r.name);
                used.insert(r.name);
            }
        }
        let unused: Vec<_> = mapped.keys().filter(|k| !used.contains(**k)).collect();
        assert!(unused.is_empty(), "goal-names.tsv entries no table uses: {unused:?}");
    }

    /// The invariant that lets vanilla's two priority namespaces share one
    /// `GoalSelector`: a target-selector goal claims exactly `{TARGET}`, and no
    /// goal-selector goal claims TARGET at all. With that true, a priority
    /// number from one namespace can never be compared against one from the
    /// other, because `GoalSelector` only compares goals contending for a shared
    /// flag.
    ///
    /// If this fails, the merge is no longer sound and `MobAi` is the fix — do
    /// not paper over it with a priority offset.
    #[test]
    fn target_and_goal_namespaces_cannot_contend() {
        let ctx = SpeciesContext::new(0.25);
        let mut checked = 0usize;
        for species in all_claimed_species() {
            for r in registrations_for(species) {
                let Some(build) = r.build() else { continue };
                let flags = build(&ctx).flags();
                checked += 1;
                match r.selector {
                    // A target-selector goal may claim no flag at all (the
                    // universal-anger reset does not), but never a goal-side one.
                    Selector::Target => assert!(
                        ![Flag::Move, Flag::Look, Flag::Jump].iter().any(|&f| flags.contains(f)),
                        "{species}'s {} is on the target selector but claims \
                         flags other than TARGET; vanilla's two priority \
                         namespaces would now collide",
                        r.name
                    ),
                    Selector::Goal => assert!(
                        !flags.contains(Flag::Target),
                        "{species}'s {} is on the goal selector but claims \
                         TARGET; its priority number is in the wrong namespace",
                        r.name
                    ),
                }
            }
        }
        assert!(checked > 0, "no goals were checked");
    }

    /// A species in two families means the first-match order silently decides
    /// which roster wins — the exact failure of five units adding arms in
    /// parallel.
    #[test]
    fn no_species_is_claimed_by_two_families() {
        let mut seen: Vec<&str> = Vec::new();
        for s in all_claimed_species() {
            assert!(
                !seen.contains(&s),
                "{s} is claimed by two family modules; only the first in \
                 FAMILIES would ever be used"
            );
            seen.push(s);
        }
        // And the claim is exclusive per family, not just per name: exactly one
        // family may answer for each species.
        for s in seen {
            let answers = FAMILIES.iter().filter(|f| f(s).is_some()).count();
            assert_eq!(answers, 1, "{s} is answered by {answers} families");
        }
    }

    /// An unknown species must fall back explicitly, and two different species
    /// must not share a table — an assertion that passed for both would mean the
    /// lookup is ignoring its key.
    #[test]
    fn unknown_species_falls_back_and_known_species_differ() {
        let ctx = SpeciesContext::new(0.25);
        assert!(is_fallback(registrations_for("llama")));
        assert!(is_fallback(registrations_for("")));
        assert!(
            is_fallback(registrations_for("minecraft:cow")),
            "the table is keyed on a resource-key *path*; a full key must not \
             match, or a caller passing the wrong form would get a table by luck"
        );

        let creeper = goals_for("creeper", &ctx);
        let cow = goals_for("cow", &ctx);
        let fallback = goals_for("llama", &ctx);
        assert!(!creeper.is_empty());
        assert!(!cow.is_empty());
        assert_eq!(fallback.len(), FALLBACK.len());
        // Compare by (priority, flags) rather than by length alone: two sets of
        // equal size are not the same set.
        let shape = |v: Vec<(i32, Box<dyn Goal>)>| {
            v.into_iter()
                .map(|(p, g)| (p, format!("{:?}", g.flags())))
                .collect::<Vec<_>>()
        };
        assert_ne!(
            shape(creeper),
            shape(cow),
            "a creeper and a cow must not get the same goal set"
        );
    }

    /// The gate this unit needs: a species' table must
    /// equal the exact multiset of goal registrations vanilla's own species
    /// class performs.
    ///
    /// The expected values below are transcribed **from the jar**, in jar order,
    /// including the registrations this repo does not implement. That is what
    /// makes the gate non-vacuous: an expectation copied out of the tables in
    /// this crate would be satisfied by any table, correct or not, and an
    /// expectation listing only the goals we build would be satisfied by any
    /// subset. Re-read the citation before changing a line here.
    ///
    /// Deliberately *not* covered: `wither_skeleton`, which shares the base
    /// skeleton table while vanilla gives it one extra target registration.
    /// That row can only ever be
    /// `Missing` today — no piglin can exist in this sim — so it changes no
    /// behaviour, but the table is knowingly not a complete transcription and
    /// asserting it here would be a lie.
    #[test]
    fn every_table_matches_the_jars_addgoal_block() {
        // (species, [(selector, priority, vanilla name)]) — jar order.
        type Row = (Selector, i32, &'static str);
        let cases: &[(&str, &[Row])] = &[
            (
                "creeper",
                &[
                    (Selector::Goal, 1, "stay_afloat"),
                    (Selector::Goal, 2, "fuse"),
                    (Selector::Goal, 3, "flee_entity(ocelot)"),
                    (Selector::Goal, 3, "flee_entity(cat)"),
                    (Selector::Goal, 4, "melee_strike"),
                    (Selector::Goal, 5, "wander_dry"),
                    (Selector::Goal, 6, "watch_player(player)"),
                    (Selector::Goal, 6, "idle_glance"),
                    (Selector::Target, 1, "nearest_target(player)"),
                    (Selector::Target, 2, "retaliate"),
                ],
            ),
            (
                "spider",
                &[
                    (Selector::Goal, 1, "stay_afloat"),
                    (Selector::Goal, 2, "flee_entity(armadillo)"),
                    (Selector::Goal, 3, "pounce"),
                    (Selector::Goal, 4, "spider.melee_attack"),
                    (Selector::Goal, 5, "wander_dry"),
                    (Selector::Goal, 6, "watch_player(player)"),
                    (Selector::Goal, 6, "idle_glance"),
                    (Selector::Target, 1, "retaliate"),
                    (Selector::Target, 2, "spider.target(player)"),
                    (Selector::Target, 3, "spider.target(iron_golem)"),
                ],
            ),
            (
                "zombie",
                &[
                    (Selector::Goal, 1, "zombie.break_door"),
                    (Selector::Goal, 4, "zombie.break_turtle_egg"),
                    (Selector::Goal, 8, "watch_player(player)"),
                    (Selector::Goal, 8, "idle_glance"),
                    (Selector::Goal, 2, "spear_lunge"),
                    (Selector::Goal, 3, "zombie.melee_attack"),
                    (Selector::Goal, 6, "patrol_village"),
                    (Selector::Goal, 7, "wander_dry"),
                    (Selector::Target, 1, "retaliate"),
                    (Selector::Target, 2, "nearest_target(player)"),
                    (
                        Selector::Target,
                        3,
                        "nearest_target(villager)",
                    ),
                    (Selector::Target, 3, "nearest_target(iron_golem)"),
                    (Selector::Target, 5, "nearest_target(turtle)"),
                ],
            ),
            (
                "skeleton",
                // The bow registration inside vanilla's own weapon-reassessment
                // step, not the melee one: vanilla's own default-equipment
                // assignment hands out a
                // bow unconditionally, so that reassessment takes
                // the bow branch for every normally-spawned skeleton and the melee
                // fallback is unreachable outside the wither skeleton.
                &[
                    (Selector::Goal, 2, "avoid_sunlight"),
                    (Selector::Goal, 3, "seek_shade"),
                    (Selector::Goal, 3, "flee_entity(wolf)"),
                    (Selector::Goal, 4, "bow_strike"),
                    (Selector::Goal, 5, "wander_dry"),
                    (Selector::Goal, 6, "watch_player(player)"),
                    (Selector::Goal, 6, "idle_glance"),
                    (Selector::Target, 1, "retaliate"),
                    (Selector::Target, 2, "nearest_target(player)"),
                    (Selector::Target, 3, "nearest_target(iron_golem)"),
                    (Selector::Target, 3, "nearest_target(turtle)"),
                ],
            ),
            (
                "cow",
                &[
                    (Selector::Goal, 0, "stay_afloat"),
                    (Selector::Goal, 1, "flee_in_panic"),
                    (Selector::Goal, 2, "mate"),
                    (Selector::Goal, 3, "lure(cow_food)"),
                    (Selector::Goal, 4, "trail_parent"),
                    (Selector::Goal, 5, "wander_dry"),
                    (Selector::Goal, 6, "watch_player(player)"),
                    (Selector::Goal, 7, "idle_glance"),
                ],
            ),
            (
                "sheep",
                &[
                    (Selector::Goal, 0, "stay_afloat"),
                    (Selector::Goal, 1, "flee_in_panic"),
                    (Selector::Goal, 2, "mate"),
                    (Selector::Goal, 3, "lure(sheep_food)"),
                    (Selector::Goal, 4, "trail_parent"),
                    (Selector::Goal, 5, "graze"),
                    (Selector::Goal, 6, "wander_dry"),
                    (Selector::Goal, 7, "watch_player(player)"),
                    (Selector::Goal, 8, "idle_glance"),
                ],
            ),
            (
                "pig",
                &[
                    (Selector::Goal, 0, "stay_afloat"),
                    (Selector::Goal, 1, "flee_in_panic"),
                    (Selector::Goal, 3, "mate"),
                    (Selector::Goal, 4, "lure(carrot_on_a_stick)"),
                    (Selector::Goal, 4, "lure(pig_food)"),
                    (Selector::Goal, 5, "trail_parent"),
                    (Selector::Goal, 6, "wander_dry"),
                    (Selector::Goal, 7, "watch_player(player)"),
                    (Selector::Goal, 8, "idle_glance"),
                ],
            ),
            (
                "chicken",
                &[
                    (Selector::Goal, 0, "stay_afloat"),
                    (Selector::Goal, 1, "flee_in_panic"),
                    (Selector::Goal, 2, "mate"),
                    (Selector::Goal, 3, "lure(chicken_food)"),
                    (Selector::Goal, 4, "trail_parent"),
                    (Selector::Goal, 5, "wander_dry"),
                    (Selector::Goal, 6, "watch_player(player)"),
                    (Selector::Goal, 7, "idle_glance"),
                ],
            ),
            (
                "horse",
                // Vanilla's own abstract-horse goal registration, then its own
                // trailing shared-behaviour-goals call.
                &[
                    (Selector::Goal, 1, "horse.buck_off_rider"),
                    (Selector::Goal, 2, "mate"),
                    (Selector::Goal, 4, "trail_parent"),
                    (Selector::Goal, 6, "wander_dry"),
                    (Selector::Goal, 7, "watch_player(player)"),
                    (Selector::Goal, 8, "idle_glance"),
                    (Selector::Goal, 9, "rear_up"),
                    (Selector::Goal, 0, "stay_afloat"),
                    (Selector::Goal, 1, "horse.mount_panic"),
                    (Selector::Goal, 3, "lure(horse_items)"),
                ],
            ),
        ];

        for &(species, want) in cases {
            let table = registrations_for(species);
            let got: Vec<Row> = table
                .iter()
                .map(|r| (r.selector, r.priority, r.name))
                .collect();
            assert_eq!(
                got,
                want.to_vec(),
                "{species}'s table does not match vanilla's own goal \
                 registration — re-read the jar before editing either side of this"
            );
        }

        // Species that share a table must actually share it, since the jar
        // reason they do (no goal-registration override) is a claim about the jar.
        for (a, b) in [
            ("cow", "mooshroom"),
            ("zombie", "husk"),
            ("spider", "cave_spider"),
            ("horse", "donkey"),
            ("horse", "mule"),
        ] {
            let (ta, tb) = (registrations_for(a), registrations_for(b));
            assert!(
                std::ptr::eq(ta.as_ptr(), tb.as_ptr()) && ta.len() == tb.len(),
                "{b} must share {a}'s table — it declares no goal registration of \
                 its own"
            );
        }
    }

    /// Target-selector goals must be installed before goal-selector ones, since
    /// insertion order is what reproduces vanilla's "tick targets first".
    #[test]
    fn target_goals_are_installed_first() {
        let ctx = SpeciesContext::new(0.25);
        // A creeper has both kinds (vanilla's own creeper registration).
        let table = registrations_for("creeper");
        let target_count = table
            .iter()
            .filter(|r| r.selector == Selector::Target && r.build().is_some())
            .count();
        assert!(target_count > 0, "precondition: creeper has target goals");

        let installed = goals_for("creeper", &ctx);
        let target_flags = FlagSet::of(&[Flag::Target]);
        for (i, (_, goal)) in installed.iter().enumerate() {
            let is_target = goal.flags() == target_flags;
            assert_eq!(
                is_target,
                i < target_count,
                "goal {i} of the creeper's installed set is on the wrong side \
                 of the target/goal boundary at {target_count}"
            );
        }
    }
}
