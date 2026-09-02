//! Goal sets for the melee monsters: the zombie family, the skeleton family's
//! melee fallback, spiders and creepers.
//!
//! # What it is
//!
//! One [`Registration`] table per species, transcribed from that species'
//! own goal registration in the 26.2 decompiled sources. This
//! is the melee-family roster; extend it here and nothing else in the tree
//! changes.
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
//! copy of vanilla's own goal-registration block. Change a priority here and that gate fails
//! until the citation is re-read, which is the point.
//!
//! # The family is not uniform, and three of its four branches prove it
//!
//! "Inherits its parent's goal registration" is a claim about the jar, and it is
//! false in three different ways here. Each was checked per class rather than
//! assumed from the family:
//!
//! | class | declares | consequence |
//! |---|---|---|
//! | `Husk`, `ZombieVillager` | nothing | share [`ZOMBIE`] verbatim |
//! | `Drowned` | **its own behaviour-goals helper**, not the main registration method | keeps `Zombie`'s *three* base rows and replaces all nine others — [`DROWNED`] |
//! | `CaveSpider` | nothing | shares [`SPIDER`] |
//! | `Skeleton`, `Stray`, `Bogged`, `Parched` | nothing | share [`SKELETON`] |
//! | `WitherSkeleton` | its own goal registration | one extra target row *before* `super` — [`WITHER_SKELETON`] |
//!
//! The `Drowned` case is the one a family-shaped assumption gets wrong.
//! Vanilla's own zombie goal registration calls its own behaviour-goals
//! helper, so the
//! override is a *partial* replacement: a drowned still gets the turtle-egg goal
//! and both priority-8 look goals, but none of the spear-use goal, the
//! zombie-attack goal, the move-through-village goal or the water-avoiding stroll.
//! Transcribing the zombie's whole table for it would give it four goals vanilla
//! does not register and omit six it does.
//!
//! `Parched` is a 26.2 skeleton variant that was not covered when this table was
//! first transcribed; it declares no goal registration of its own, so it shares the base
//! table.
//!
//! # Known gaps, all disclosed in the tables
//!
//! * **The skeleton family shoots. Only the wither skeleton punches.** This entry
//!   used to say no ranged goals existed here, so the family took the melee half
//!   of vanilla's own weapon-reassessment step unconditionally. They exist now
//!   ([`super::ranged`]), and a later fix *replaced* [`SKELETON`]'s priority-4 row
//!   with `RangedBowAttackGoal` rather than adding to it — both candidates claim
//!   MOVE and vanilla's own weapon-reassessment step removes both before
//!   re-adding exactly one, so a second row would make the winner
//!   registration-order dependent.
//!   Vanilla's own default-equipment assignment puts a `BOW` in the main
//!   hand **unconditionally** — no random roll, no difficulty gate — so
//!   the weapon-reassessment step's own bow test holds for every
//!   normally-spawned skeleton and the melee `else` branch never runs.
//!   **The boundary is the *equipment* override, not the goal method**:
//!   [`WITHER_SKELETON`] genuinely keeps melee, because
//!   vanilla's own wither-skeleton equipment assignment overrides that method with a
//!   `STONE_SWORD` and so fails the bow test — it does not override
//!   the weapon-reassessment step at all, only calls it from
//!   its own spawn-finalization step. `Skeleton`, `Stray`, `Bogged` and `Parched`
//!   override neither. So [`melee_attack_1_2`] survives with exactly one reachable caller,
//!   and "skeletons shoot" is *not* the whole rule.
//!   A drowned now throws its trident when it spawned holding one
//!   ([`crate::spawn_equipment`]'s ~6.25% roll): [`DROWNED`]'s trident row
//!   registers [`super::ranged::trident_attack`] unconditionally, exactly as
//!   vanilla's own behaviour-goals helper does, and the goal's own `can_use` gates on
//!   [`crate::ai::MobController::main_hand_item`] rather than on
//!   registration.
//! * **Nothing in the sim is a villager, iron golem, turtle, armadillo, axolotl or
//!   piglin**, so every target registration naming one is [`Coverage::Missing`]
//!   rather than a goal that would search for an entity class that cannot be
//!   spawned.
//! * **Vanilla's own same-owner alert-others flag is not modelled** — our
//!   `HurtByTargetGoal` retaliates but never propagates anger to nearby mobs.
//!   That needs a sim-side census of nearby same-species mobs this repo does
//!   not have yet.
//! * **No water-aware navigation exists**, so all five of the drowned's
//!   amphibious goals (vanilla's own go-to-water, go-to-beach and swim-up
//!   goals, and the two that gate on a valid target) are `Missing`. A
//!   drowned on land behaves like a slow zombie, which is what vanilla's own
//!   land branch does anyway.
//!
//! # What these tables cannot fix, and where the behaviour actually stops
//!
//! Every melee row below depends on the mob having an attack target, and this
//! section **used to say nothing in production ever gives one** —
//! `NavigatingMob::find_nearest_target` returned the `self.attack_target` its own
//! caller writes, so `NearestAttackableTargetGoal::can_use` asked for a target,
//! got back the target it was supposed to be finding, and returned `false`
//! forever. That was measured and true when written. It is not true now: a
//! later fix (`23b3dd2`) made `find_nearest_target` read the `nearest_player`
//! the server's perception feed populates, cut by vanilla's `FOLLOW_RANGE` (a
//! squared-distance test against `max(range, 2.0)`), and `mobs.rs` now passes the
//! per-species attribute through. A hosted zombie acquires a player it was never
//! told about and walks at it.
//!
//! **`set_attack_target` is now written in production**, but only by the goal
//! that reads `find_nearest_target` in the first place —
//! `NearestAttackableTargetGoal` and `HurtByTargetGoal` in
//! [`crate::ai::goals`] are the only production writers. The gates below still
//! hand the target over directly rather than waiting for acquisition, because
//! that keeps a failure in this file attributable to a **table row** rather than
//! to acquisition; `crates/lodestone-entity/tests/target_acquisition.rs` is what
//! proves acquisition itself.
//!
//! What is still missing is **line of sight**: vanilla's is an eye-to-eye ray
//! cast (vanilla's own targeting-condition test calling its own
//! has-line-of-sight check, which
//! resolves to a world raycast), which this repo's local `BlockCues` lookups
//! structurally cannot answer, so it is unimplemented. That errs *permissive* —
//! a mob can acquire through a wall.

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
    "zombie_villager",
    "drowned",
    "creeper",
    "spider",
    "cave_spider",
    "skeleton",
    "stray",
    "bogged",
    "parched",
    "wither_skeleton",
];

/// Resolves a species path to its table, or `None` if this family does not claim
/// it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        // `Husk` and `ZombieVillager` both extend `Zombie` and declare neither
        // `registerGoals` nor `addBehaviourGoals`, so they inherit the whole
        // table verbatim. Checked per class, not inferred from the family —
        // their sibling `Drowned` does override, one method down.
        "zombie" | "husk" | "zombie_villager" => Some(ZOMBIE),
        "drowned" => Some(DROWNED),
        "creeper" => Some(CREEPER),
        // `CaveSpider` likewise inherits `Spider`'s; its only overrides are
        // attributes and a poison effect on hit.
        "spider" | "cave_spider" => Some(SPIDER),
        // `Skeleton`, `Stray`, `Bogged` and `Parched` all extend
        // the abstract skeleton base and declare no goal registration of their own.
        "skeleton" | "stray" | "bogged" | "parched" => Some(SKELETON),
        // `WitherSkeleton` *does* declare one of its own,
        // so it gets its own table rather than sharing the base one.
        "wither_skeleton" => Some(WITHER_SKELETON),
        _ => None,
    }
}

/// Vanilla's own creeper goal registration.
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
pub static CREEPER: &[Registration] = &[
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

/// Vanilla's own spider goal registration.
pub static SPIDER: &[Registration] = &[
    Registration::goal(1, "FloatGoal", float_goal),
    // `AvoidEntityGoal<>(this, Armadillo.class, 6.0F, 1.0, 1.2, e -> !e.isScared())`.
    // The `isScared` filter is not modelled — see `mobs.rs`'s `avoided_species`,
    // which discloses it can only make a spider flee slightly more often.
    Registration::goal(2, "AvoidEntityGoal(Armadillo)", avoid_entity),
    // `LeapAtTargetGoal(this, 0.4F)` — no equivalent goal exists; a spider will
    // walk into melee range instead of pouncing.
    Registration::missing(Selector::Goal, 3, "LeapAtTargetGoal"),
    // Vanilla's own spider attack goal extends `MeleeAttackGoal`; its only
    // addition is
    // refusing to attack while the spider has a passenger, which this sim has no
    // notion of.
    Registration::goal(4, "Spider.SpiderAttackGoal", melee_attack),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll_0_8),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(6, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    // Vanilla's own spider target goal extends
    // `NearestAttackableTargetGoal`, adding only a daylight brightness penalty to
    // the search radius.
    Registration::target(2, "Spider.SpiderTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "Spider.SpiderTargetGoal(IronGolem)"),
];

/// Vanilla's own zombie goal registration plus its own behaviour-goals
/// helper, which
/// the main registration calls — the registrations are split across two methods and
/// both halves belong to this table.
///
/// A zombie gets **no** `FloatGoal`, which is not an omission: vanilla does not
/// register one, because zombies sink and walk along the bottom.
pub static ZOMBIE: &[Registration] = &[
    // Vanilla's own zombie turtle-egg-attack goal — a `RemoveBlockGoal`
    // subclass: a 24-block spiral search (vertical range 3) for a `turtle_egg`,
    // then break-progress and a destroy intent. Neither the candidate search
    // nor the mutation exists on this seam (`docs/mob-block-perception.md`), and
    // no turtle can spawn in this sim regardless.
    Registration::missing(Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // `SpearUseGoal<>(this, 1.0, 1.0, 10.0F, 2.0F)` — new in 26.2, and a ranged
    // goal, so it belongs to the ranged-attack roster (`super::ranged`) rather
    // than here.
    Registration::missing(Selector::Goal, 2, "SpearUseGoal"),
    // `ZombieAttackGoal(this, 1.0, false)` extends `MeleeAttackGoal`, adding only
    // the raised-arms metadata flag while it runs.
    Registration::goal(3, "ZombieAttackGoal", melee_attack),
    // `MoveThroughVillageGoal(this, 1.0, true, 4, this::canBreakDoors)` — needs
    // village POI data that does not exist here.
    Registration::missing(Selector::Goal, 6, "MoveThroughVillageGoal"),
    Registration::goal(7, "WaterAvoidingRandomStrollGoal", stroll),
    // Vanilla's own registration chains an alert-others flag naming the
    // zombified piglin as the excluded species. The
    // retaliation is modelled; the alert propagation to nearby mobs is not.
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(AbstractVillager)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    Registration::missing(Selector::Target, 5, "NearestAttackableTargetGoal(Turtle)"),
];

/// Vanilla's own zombie goal registration plus the drowned's own
/// behaviour-goals helper.
///
/// **The one species in this family whose parent's table is only partly
/// inherited.** The drowned extends the zombie and overrides its own
/// behaviour-goals helper —
/// *not* the main registration method. Since vanilla's own zombie registration calls
/// its own behaviour-goals helper, a drowned keeps exactly the three
/// rows the zombie's own registration adds itself (turtle-egg at 4, and both look goals
/// at 8) and replaces the other nine wholesale. Reading "Drowned inherits
/// Zombie's goals" off the class hierarchy would give it `SpearUseGoal`,
/// `ZombieAttackGoal`, `MoveThroughVillageGoal` and a water-avoiding stroll that
/// vanilla never registers on it, and lose all six goals that make it amphibious.
///
/// Of the fifteen rows seven are now modelled, with the trident row the
/// newest addition: it joins the melee row as real now that
/// [`crate::spawn_equipment`] can say which drowned are holding one. Four of
/// the eight still-`Missing` rows are the amphibious navigation, which this
/// repo has no water-aware pathing for at all.
pub static DROWNED: &[Registration] = &[
    // -- inherited from vanilla's own zombie registration ---------------------
    Registration::missing(Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // -- the drowned's own behaviour-goals helper -----------------------------
    // Vanilla's own go-to-water goal — seeks a water column to
    // submerge in. Needs the water-aware navigation the drowned's own
    // navigation type
    // provides and this repo's `PathWorld` does
    // not.
    Registration::missing(Selector::Goal, 1, "Drowned.DrownedGoToWaterGoal"),
    // Vanilla's own drowned trident-attack goal extends
    // `RangedAttackGoal` — its builder lives in the ranged-attack roster
    // (`super::ranged::trident_attack`), registered from here since the
    // drowned's melee half keeps this file. It shares priority 2 with the
    // melee goal below: vanilla registers both unconditionally and gates them
    // at runtime on the held item (`RangedAttackGoal::can_use`'s
    // `requires_main_hand` conjunct), not on precedence — a drowned that never
    // rolled a trident (`crate::spawn_equipment`, ~93.75% of spawns) simply
    // never has this goal's `can_use` return true.
    Registration::goal(2, "Drowned.DrownedTridentAttackGoal", super::ranged::trident_attack),
    // Vanilla's own drowned attack goal extends `ZombieAttackGoal`,
    // adding only a valid-target check — vanilla's rule that a drowned in water
    // will chase anything but on land only chases a target that is itself in
    // water. Not modelled: our melee goal chases whatever
    // target it is given, which on land makes a drowned slightly more
    // aggressive than vanilla's.
    Registration::goal(2, "Drowned.DrownedAttackGoal", melee_attack),
    // Vanilla's own go-to-beach goal extends `MoveToBlockGoal` —
    // leaves the water at night to hunt. No sun/time query on the AI seam and
    // no water to leave.
    Registration::missing(Selector::Goal, 5, "Drowned.DrownedGoToBeachGoal"),
    // Vanilla's own swim-up goal — rises toward the
    // surface. Needs a sea-level query and vertical swimming.
    Registration::missing(Selector::Goal, 6, "Drowned.DrownedSwimUpGoal"),
    // Vanilla's own plain stroll goal at speed `1.0` — the plain stroll, **not** the
    // water-avoiding subclass every other species in this family registers
    // (contrast the zombie's own behaviour-goals helper). So this is the one stroll row in
    // the roster where our `RandomStrollGoal` is an exact match rather than a
    // disclosed simplification: a drowned is happy to wander into water.
    Registration::goal(7, "RandomStrollGoal", stroll),
    // Vanilla's own registration chains an alert-others flag naming the
    // zombified piglin, with an ignore list naming the drowned itself — a
    // drowned does not
    // retaliate against other drowned. Not modelled; ours has no class filter,
    // and nothing yet makes one drowned hurt another.
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    // Vanilla's own targeting-goal registration adds the same valid-target
    // water rule as the melee goal, unmodelled the same way.
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(AbstractVillager)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    // Vanilla's drowned hunt axolotls; nothing in this sim is one.
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(Axolotl)"),
    Registration::missing(Selector::Target, 5, "NearestAttackableTargetGoal(Turtle)"),
];

/// Vanilla's own abstract-skeleton goal registration, plus the priority-4
/// weapon goal that
/// its own weapon-reassessment step installs rather than in the main
/// registration.
///
/// That priority-4 slot is the reason [`GoalSelector::remove`] exists: vanilla's
/// own weapon-reassessment step removes *both* candidate goals and
/// re-adds exactly one every time the skeleton's held item changes.
///
/// **Which one it re-adds is not a coin toss.** Vanilla's own default-equipment
/// assignment
/// puts a `BOW` in the main hand *unconditionally* — no random roll, no
/// difficulty gate — so the bow test is true for every
/// normally-spawned skeleton and the `else` branch **never runs**. This table
/// therefore carries the bow half, which is the only branch the game reaches.
/// It used to carry the melee half, modelling a state a skeleton is never in —
/// and a *second* priority-4 row would have been worse than either, since both
/// goals claim MOVE and the winner would be registration-order dependent.
///
/// [`WITHER_SKELETON`] is the exception, and the boundary is the **equipment**
/// override, not the goal method: vanilla's own wither-skeleton equipment
/// assignment
/// overrides that method to hand out a `STONE_SWORD`, and the *inherited*
/// weapon-reassessment step then takes the `else`. It does not override
/// the weapon-reassessment step itself — only calls it, from its own
/// spawn-finalization step.
/// `Skeleton`, `Stray`, `Bogged` and `Parched` override neither method, so all
/// four inherit the bow and share this table.
///
/// One known simplification inside the shared row: `Bogged` and `Parched` *do*
/// override the interval, to `70` below Hard against the abstract skeleton's
/// own `40`
/// (each species' own attack-interval override). All four get `40` here, because the
/// interval is an argument to the shared builder rather than a row identity, and
/// nothing in this repo carries a world difficulty for the Hard half either.
/// Splitting it needs a per-species field on [`SpeciesContext`], not a fourth
/// table.
///
/// [`GoalSelector::remove`]: crate::ai::GoalSelector::remove
pub static SKELETON: &[Registration] = &[
    // Vanilla's own restrict-sun and flee-sun goals — two different
    // mechanisms, both absent. The restrict-sun goal reads no block: its gate is a
    // daytime query plus an empty HEAD slot, and its *effect* is
    // vanilla's own avoid-sun pathfinding flag — a sky-light penalty in the
    // path evaluator, a pathfinder feature. The flee-sun goal needs a host-computed
    // shaded position (its own hide-position search probes ten spots). So a
    // skeleton does not seek shade. Daylight *burning* is modelled separately,
    // not by this table.
    Registration::missing(Selector::Goal, 2, "RestrictSunGoal"),
    Registration::missing(Selector::Goal, 3, "FleeSunGoal"),
    Registration::goal(3, "AvoidEntityGoal(Wolf)", avoid_entity),
    // Vanilla's own weapon-reassessment step's bow branch, the only one a
    // normally-spawned skeleton takes. Vanilla's own bow-goal field is
    // constructed at speed `1.0`, interval `20`, distance `15.0F`, with the
    // interval
    // overwritten per difficulty. The `else` branch installs the melee goal and
    // belongs to `WITHER_SKELETON` alone.
    Registration::goal(4, "RangedBowAttackGoal", super::ranged::bow_attack),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(6, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(Turtle)"),
];

/// Vanilla's own wither-skeleton goal registration — one extra target
/// registration, then
/// everything in [`SKELETON`].
///
/// `WitherSkeleton` is the only class in this family that declares
/// its own goal registration, and it adds its row **before** calling
/// the base registration, which is why the piglin row comes first here.
/// Vanilla's ordering is observable only among rows of equal priority, and this
/// row shares priority 3
/// with two others, so transcribing the order matters even though all three are
/// unmodelled.
///
/// This table used to be [`SKELETON`], shared, with a comment conceding it was
/// "knowingly not a complete transcription" because the extra row is `Missing`
/// either way. Splitting it costs eleven duplicated lines and buys a table that a
/// multiset gate can actually check against the jar — and
/// `wither_skeleton_is_the_base_table_plus_the_piglin_row` pins the duplication so
/// the two cannot drift.
pub static WITHER_SKELETON: &[Registration] = &[
    // Vanilla's own wither-skeleton registration adds a targeting goal for
    // the abstract-piglin class. No piglin can exist in this sim.
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(AbstractPiglin)"),
    // -- vanilla's own base registration plus the
    // -- weapon `else` branch --------------------------------------------
    Registration::missing(Selector::Goal, 2, "RestrictSunGoal"),
    Registration::missing(Selector::Goal, 3, "FleeSunGoal"),
    Registration::goal(3, "AvoidEntityGoal(Wolf)", avoid_entity),
    // The `else` half of vanilla's own weapon-reassessment step — the one
    // branch of this family
    // that is really melee, because vanilla's own wither-skeleton equipment
    // assignment
    // overrides with a `STONE_SWORD` and so fails the bow test.
    // [`SKELETON`] takes the bow branch instead.
    Registration::goal(4, "MeleeAttackGoal", melee_attack_1_2),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(6, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(Turtle)"),
];

/// `WaterAvoidingRandomStrollGoal(this, 0.8)` — the creeper's own registration
/// and the spider's own registration.
fn stroll_0_8(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RandomStrollGoal::new(ctx.speed * 0.8))
}

/// `MeleeAttackGoal(this, 1.2, false)` — `AbstractSkeleton`'s `meleeGoal` field,
/// faster than the 1.0 every other species in this family uses.
///
/// Declared on `AbstractSkeleton` but reachable only by [`WITHER_SKELETON`]: the
/// `else` branch that installs it needs a non-bow main hand, and only the wither
/// overrides `populateDefaultEquipmentSlots` to have one.
fn melee_attack_1_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MeleeAttackGoal::new(ctx.speed * 1.2, ctx.attack_reach))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lodestone_model::Vec3;

    use super::super::probe::SpeedProbe;
    use super::super::{Coverage, goals_for, is_fallback, registrations_for};
    use super::*;
    use crate::ai::goal::GoalSelector;
    use crate::ai::mob::MobController;
    use crate::ai::navigating_mob::NavigatingMob;
    use crate::pathfinding::{Aabb, MobShape, PathType, PathWorld};

    /// The two tables `super::super::tests::every_table_matches_the_jars_addgoal_block`
    /// does **not** transcribe, checked the same way: row for row against the jar,
    /// in jar order, including the rows this repo does not implement.
    ///
    /// That gate covers creeper, spider, zombie and skeleton; between it, this
    /// test and [`inheritance_matches_which_classes_declare_register_goals`] below,
    /// every one of the twelve species in [`SPECIES`] is pinned to vanilla's
    /// own goal registration. The expectations here were transcribed from the jar, not from
    /// the tables above — copying them from `DROWNED` would be satisfied by any
    /// table, right or wrong.
    #[test]
    fn drowned_and_wither_skeleton_match_the_jars_addgoal_block() {
        type Row = (Selector, i32, &'static str);
        let cases: &[(&str, &[Row])] = &[
            (
                "drowned",
                &[
                    (Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
                    (Selector::Goal, 8, "LookAtPlayerGoal(Player)"),
                    (Selector::Goal, 8, "RandomLookAroundGoal"),
                    (Selector::Goal, 1, "Drowned.DrownedGoToWaterGoal"),
                    (Selector::Goal, 2, "Drowned.DrownedTridentAttackGoal"),
                    (Selector::Goal, 2, "Drowned.DrownedAttackGoal"),
                    (Selector::Goal, 5, "Drowned.DrownedGoToBeachGoal"),
                    (Selector::Goal, 6, "Drowned.DrownedSwimUpGoal"),
                    (Selector::Goal, 7, "RandomStrollGoal"),
                    (Selector::Target, 1, "HurtByTargetGoal"),
                    (Selector::Target, 2, "NearestAttackableTargetGoal(Player)"),
                    (
                        Selector::Target,
                        3,
                        "NearestAttackableTargetGoal(AbstractVillager)",
                    ),
                    (Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
                    (Selector::Target, 3, "NearestAttackableTargetGoal(Axolotl)"),
                    (Selector::Target, 5, "NearestAttackableTargetGoal(Turtle)"),
                ],
            ),
            (
                "wither_skeleton",
                &[
                    (
                        Selector::Target,
                        3,
                        "NearestAttackableTargetGoal(AbstractPiglin)",
                    ),
                    (Selector::Goal, 2, "RestrictSunGoal"),
                    (Selector::Goal, 3, "FleeSunGoal"),
                    (Selector::Goal, 3, "AvoidEntityGoal(Wolf)"),
                    (Selector::Goal, 4, "MeleeAttackGoal"),
                    (Selector::Goal, 5, "WaterAvoidingRandomStrollGoal"),
                    (Selector::Goal, 6, "LookAtPlayerGoal(Player)"),
                    (Selector::Goal, 6, "RandomLookAroundGoal"),
                    (Selector::Target, 1, "HurtByTargetGoal"),
                    (Selector::Target, 2, "NearestAttackableTargetGoal(Player)"),
                    (Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
                    (Selector::Target, 3, "NearestAttackableTargetGoal(Turtle)"),
                ],
            ),
        ];

        for &(species, want) in cases {
            let got: Vec<Row> = registrations_for(species)
                .iter()
                .map(|r| (r.selector, r.priority, r.vanilla))
                .collect();
            assert_eq!(
                got,
                want.to_vec(),
                "{species}'s table does not match vanilla's own goal \
                 registration — re-read the jar before editing either side of this"
            );
        }
    }

    /// Which classes declare `registerGoals`/`addBehaviourGoals` is a claim about
    /// the jar, and it is what decides whether two species share a table. It was
    /// wrong for two of the four branches of this family before this unit, so it
    /// is gated rather than commented.
    ///
    /// A grep for vanilla's own goal-registration methods over each
    /// class: zero hits for `Husk`, `ZombieVillager`, `CaveSpider`, `Skeleton`,
    /// `Stray`, `Bogged` and `Parched`; `Drowned` overrides its own
    /// behaviour-goals helper; `WitherSkeleton` overrides the main registration.
    #[test]
    fn inheritance_matches_which_classes_declare_register_goals() {
        let same = |a: &str, b: &str| {
            let (ta, tb) = (registrations_for(a), registrations_for(b));
            std::ptr::eq(ta.as_ptr(), tb.as_ptr()) && ta.len() == tb.len()
        };

        for child in ["husk", "zombie_villager"] {
            assert!(
                same("zombie", child),
                "{child} declares no goal method of its own, so it must share \
                 zombie's table"
            );
        }
        assert!(same("spider", "cave_spider"));
        for child in ["stray", "bogged", "parched"] {
            assert!(
                same("skeleton", child),
                "{child} extends the abstract skeleton and declares no goal \
                 registration of its own"
            );
        }

        // And the two that must NOT share, which is the half a family-shaped
        // assumption gets wrong. Without these the test above is satisfied by
        // "everything in the family shares one table".
        assert!(
            !same("zombie", "drowned"),
            "Drowned overrides its own behaviour-goals helper, so it keeps \
             only Zombie's three registration rows — sharing zombie's table \
             would give it four goals vanilla never registers and lose six it does"
        );
        assert!(
            !same("skeleton", "wither_skeleton"),
            "WitherSkeleton declares its own goal registration"
        );
        // Every species this family claims must resolve to a real table, or the
        // assertions above could be comparing two fallbacks.
        for s in SPECIES {
            assert!(!is_fallback(registrations_for(s)), "{s} took the fallback");
        }
    }

    /// [`WITHER_SKELETON`] duplicates eleven rows of [`SKELETON`] by hand, so pin
    /// the relationship the duplication represents: the piglin row, then the base
    /// table with exactly **one** divergence. Editing one table and not the other
    /// fails here rather than silently giving the wither a stale set.
    ///
    /// That divergence is the priority-4 weapon row, and it is *asserted* rather
    /// than filtered away. Vanilla's own weapon-reassessment step picks its
    /// branch from the held
    /// item, so the skeleton's unconditional `BOW`
    /// (vanilla's own default-equipment assignment) reaches the bow branch
    /// and the wither's `STONE_SWORD`
    /// (vanilla's own wither-skeleton equipment assignment) reaches the melee
    /// branch. A gate that merely skipped priority 4 would still pass with both
    /// tables carrying the *same* goal — which is precisely the bug the
    /// pre-split shared table produced.
    #[test]
    fn wither_skeleton_is_the_base_table_plus_the_piglin_row_and_the_weapon_swap() {
        assert_eq!(
            WITHER_SKELETON[0].vanilla,
            "NearestAttackableTargetGoal(AbstractPiglin)"
        );

        // The weapon row: one slot, and a different occupant on each side.
        let weapon = |t: &[Registration]| {
            let rows: Vec<&str> = t
                .iter()
                .filter(|r| r.selector == Selector::Goal && r.priority == 4)
                .map(|r| r.vanilla)
                .collect();
            assert_eq!(
                rows.len(),
                1,
                "exactly one priority-4 goal row is allowed: vanilla removes both \
                 candidates and re-adds one (its own weapon-reassessment step), so a \
                 second row here would put two MOVE claimants in one slot and make \
                 the winner registration-order dependent"
            );
            rows[0]
        };
        assert_eq!(
            weapon(SKELETON),
            "RangedBowAttackGoal",
            "every normally-spawned skeleton holds a bow"
        );
        assert_eq!(
            weapon(&WITHER_SKELETON[1..]),
            "MeleeAttackGoal",
            "the wither skeleton is handed a stone sword instead"
        );

        // Everything else, row for row and in jar order.
        let without_weapon = |t: &[Registration]| {
            t.iter()
                .filter(|r| !(r.selector == Selector::Goal && r.priority == 4))
                .map(|r| (r.selector, r.priority, r.vanilla))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            without_weapon(&WITHER_SKELETON[1..]),
            without_weapon(SKELETON),
            "WitherSkeleton's own registration calls the base registration, so every row other \
             than the weapon row must be AbstractSkeleton's table verbatim"
        );
    }

    // -- speeds: the value, not the sign -------------------------------------

    /// Builds the row named `vanilla` from `species`' table and returns the speed
    /// it asks the mob to move at.
    fn speed_of(species: &str, vanilla: &str, ctx: &SpeciesContext) -> f64 {
        let row = registrations_for(species)
            .iter()
            .find(|r| r.vanilla == vanilla)
            .unwrap_or_else(|| panic!("{species} has no {vanilla} row"));
        assert!(
            matches!(row.coverage, Coverage::Modelled(_)),
            "{species}'s {vanilla} is not modelled, so it has no speed to read"
        );
        let mut goal = row.build().expect("modelled")(ctx);
        let mut probe = SpeedProbe::new();
        assert!(
            goal.can_use(&mut probe),
            "{species}'s {vanilla} did not become eligible against the probe, so \
             no move_to was recorded and this gate would measure nothing"
        );
        goal.start(&mut probe);
        goal.tick(&mut probe);
        probe
            .first_speed()
            .unwrap_or_else(|| panic!("{species}'s {vanilla} performed no move_to"))
    }

    /// A priority multiset cannot see a wrong speed multiplier, so predict the
    /// value each row must produce and require the measurement to land on it
    /// rather than merely move in the right direction.
    ///
    /// The interesting row is the wither skeleton's melee. Vanilla's own
    /// melee-goal
    /// field is constructed at `1.2` — every other melee row
    /// in this family is `1.0` — so at the skeleton family's
    /// own movement-speed attribute of `0.25` the two
    /// hypotheses are `0.30` and `0.25`, and the assertion is written to fail on
    /// the wrong one.
    ///
    /// The plain skeleton's priority-4 row is the *bow* goal, whose multiplier
    /// is `1.0` (vanilla's own bow-goal field), so it cannot host that
    /// discriminator — 1.0 × 0.25 is its bare movement speed. It is still
    /// pinned to the value, just without the inequality; the wither carries the
    /// inequality.
    #[test]
    fn transcribed_speed_multipliers_land_on_the_jars_value() {
        // (species, movement_speed, vanilla row, jar factor)
        let cases: &[(&str, f64, &str, f64)] = &[
            // Vanilla's own bow-goal field — 1.0, at
            // the abstract skeleton's own MOVEMENT_SPEED 0.25.
            ("skeleton", 0.25, "RangedBowAttackGoal", 1.0),
            // Vanilla's own melee-goal field — 1.2, at the same 0.25.
            // Only the wither ever installs it.
            ("wither_skeleton", 0.25, "MeleeAttackGoal", 1.2),
            // The creeper's own registration — 1.0, at 0.25.
            ("creeper", 0.25, "MeleeAttackGoal", 1.0),
            // The zombie's own behaviour-goals helper — its own attack goal at
            // speed `1.0`,
            // at the zombie's own MOVEMENT_SPEED 0.23.
            ("zombie", 0.23, "ZombieAttackGoal", 1.0),
            // The drowned's own behaviour-goals helper — 1.0. The drowned's own
            // attribute builder is
            // the zombie's plus `STEP_HEIGHT 1.0`, so the speed is
            // the zombie's 0.23.
            ("drowned", 0.23, "Drowned.DrownedAttackGoal", 1.0),
            // The creeper's own registration and the spider's own registration
            // — the 0.8
            // strolls.
            ("creeper", 0.25, "WaterAvoidingRandomStrollGoal", 0.8),
            ("spider", 0.3, "WaterAvoidingRandomStrollGoal", 0.8),
            // The zombie's own behaviour-goals helper — 1.0.
            ("zombie", 0.23, "WaterAvoidingRandomStrollGoal", 1.0),
            // The drowned's own behaviour-goals helper — the *plain* stroll goal at speed `1.0`.
            ("drowned", 0.23, "RandomStrollGoal", 1.0),
        ];

        for &(species, movement_speed, vanilla, factor) in cases {
            let ctx = SpeciesContext::new(movement_speed);
            let got = speed_of(species, vanilla, &ctx);
            let want = movement_speed * factor;
            assert!(
                (got - want).abs() < 1e-12,
                "{species}'s {vanilla} must move at {movement_speed} × {factor} = \
                 {want}, measured {got}. A wrong factor here is invisible to every \
                 priority assertion in this file"
            );
        }

        // The discriminating control, stated as an inequality against the
        // hypothesis a copy-paste from the creeper's row would produce. It lives on
        // the wither skeleton because the plain skeleton's row is now the bow goal,
        // whose 1.0 *is* the bare movement speed.
        let unshifted = SpeciesContext::new(0.25).speed;
        let wither = speed_of(
            "wither_skeleton",
            "MeleeAttackGoal",
            &SpeciesContext::new(0.25),
        );
        assert!(
            (wither - unshifted).abs() > 0.04,
            "a wither skeleton's melee must not be its bare movement_speed — that \
             is the 1.0 hypothesis, and vanilla's own melee-goal field is 1.2"
        );
    }

    // -- behaviour, through the production controller -------------------------

    /// Flat ground below `y = 0`. The narrowest world that lets a real
    /// `NavigatingMob` path, which is the point: the subject under test is the
    /// roster, not the pathfinder.
    struct Flat {
        walls: HashSet<(i32, i32, i32)>,
    }

    impl Flat {
        fn new() -> Self {
            Self {
                walls: HashSet::new(),
            }
        }
        fn solid(&self, x: i32, y: i32, z: i32) -> bool {
            y <= -1 || self.walls.contains(&(x, y, z))
        }
    }

    impl PathWorld for Flat {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
            if self.solid(x, y, z) {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
            if self.solid(x, y, z) { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            let (x0, x1) = (aabb.min_x.floor() as i32, (aabb.max_x - 1e-7).floor() as i32);
            let (y0, y1) = (aabb.min_y.floor() as i32, (aabb.max_y - 1e-7).floor() as i32);
            let (z0, z1) = (aabb.min_z.floor() as i32, (aabb.max_z - 1e-7).floor() as i32);
            (x0..=x1).any(|x| (y0..=y1).any(|y| (z0..=z1).any(|z| self.solid(x, y, z))))
        }
        fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
            false
        }
    }

    /// One outcome of ticking a real [`NavigatingMob`] whose goal selector was
    /// filled **only** by [`goals_for`].
    struct Outcome {
        /// Horizontal distance from the target at the start and at the end.
        gap: (f64, f64),
        /// How many times a goal reached [`MobController::attack`].
        attacks: usize,
        /// How many times a goal reached
        /// [`MobController::launch_projectile`](crate::ai::mob::MobController::launch_projectile).
        launches: usize,
        /// Peak fuse counter — non-zero only if `SwellGoal` ran.
        swell: i32,
    }

    /// Spawns `species` at the origin with a target `at`, installs whatever the
    /// roster says it gets, and ticks.
    ///
    /// **No `add` call of its own.** A gate that installs the goal it is about to
    /// observe cannot tell whether the roster installed it — the closed loop
    /// that hid a real island: a hostile mob's target acquisition was wired but
    /// unreachable, because the perception feed was written and never read.
    /// `NavigatingMob` is the only production implementor of `MobController`, so
    /// the goals run against the same perception the running game gives them.
    ///
    /// The attack target is set explicitly so this gate stays about the *roster*
    /// rather than about acquisition: `NavigatingMob::find_nearest_target` reads
    /// the perception feed, and `tests/target_acquisition.rs` is what proves
    /// that. Handing the target over directly keeps this file's failures
    /// attributable to a table row.
    fn run(species: &str, movement_speed: f64, at: Vec3, ticks: usize) -> Outcome {
        let world = Flat::new();
        let ctx = SpeciesContext::new(movement_speed);
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, 0.0, 0.5),
            movement_speed,
            // Vanilla's `floor(followRange * 16)` at the zombie's 35.0.
            560,
            0,
        );
        mob.set_attack_target(Some(at));

        let mut ai = GoalSelector::new();
        for (priority, goal) in goals_for(species, &ctx) {
            ai.add(priority, goal);
        }

        let horizontal = |p: Vec3| ((at.x - p.x).powi(2) + (at.z - p.z).powi(2)).sqrt();
        let before = horizontal(mob.position());
        let mut swell = 0;
        for _ in 0..ticks {
            mob.tick(&mut ai);
            swell = swell.max(mob.swell());
        }
        Outcome {
            gap: (before, horizontal(mob.position())),
            attacks: mob.attacks().len(),
            launches: mob.launches().len(),
            swell,
        }
    }

    /// The headline behavioural gate: a zombie whose goals came only from the
    /// roster closes on its target and hits it, and a species the roster does not
    /// claim — with the identical world, target and speed — does neither.
    ///
    /// The control is the "empty the species' entry" control expressed without
    /// editing the table: a llama is a real 26.2 species no family claims, so it
    /// takes [`FALLBACK`](super::super::FALLBACK) (stroll + look) by construction.
    /// It shares every other input, so "the zombie closed" cannot be satisfied by
    /// a lucky stroll.
    #[test]
    fn a_zombie_closes_and_attacks_and_a_rosterless_species_does_not() {
        let target = Vec3::new(6.5, 0.0, 0.5);
        let zombie = run("zombie", 0.23, target, 400);
        let llama = run("llama", 0.23, target, 400);

        assert!(
            zombie.attacks > 0,
            "a zombie built from the roster must reach MeleeAttackGoal's attack: \
             gap {:?}, attacks {}. Nothing in this test adds a goal, so a failure \
             means ZombieAttackGoal is not reaching the goal selector",
            zombie.gap,
            zombie.attacks
        );
        assert!(
            zombie.gap.1 < zombie.gap.0 - 3.0,
            "it must also have travelled: {:?}",
            zombie.gap
        );
        assert_eq!(
            llama.attacks, 0,
            "a llama gets FALLBACK, which has no melee goal, so it must never \
             attack — {} attacks means every species is getting the same table",
            llama.attacks
        );
        assert!(
            llama.gap.1 > llama.gap.0 - 3.0,
            "and it must not close on the target either, or the zombie's \
             assertion above is satisfied by strolling: {:?}",
            llama.gap
        );
    }

    /// A creeper swells and a zombie does not, from the same target and the same
    /// production path — the priority-order-sensitive gate.
    ///
    /// `SwellGoal` is at goal-priority 2 and `MeleeAttackGoal` at 4
    /// (vanilla's own creeper registration), and both claim MOVE, so the *only*
    /// reason the fuse ever climbs is that 2 outranks 4. Transcribe the swell at
    /// any number above 4 and melee holds MOVE and the creeper never primes,
    /// which is the control A3 ran and this gate inherits.
    #[test]
    fn a_creeper_swells_because_vanillas_priority_2_outranks_melees_4() {
        // Inside vanilla's own eligibility check's 9.0 squared proximity.
        let close = Vec3::new(2.0, 0.0, 0.5);
        let creeper = run("creeper", 0.25, close, 40);
        let zombie = run("zombie", 0.23, close, 40);

        assert!(
            creeper.swell > 0,
            "a creeper's fuse must climb with a target 1.5 blocks away. If this \
             fails, check that the creeper's own swell-goal priority 2 is still \
             below its own melee-goal priority 4 in CREEPER — a MeleeAttackGoal \
             holding MOVE prevents the swell"
        );
        assert_eq!(
            zombie.swell, 0,
            "a zombie has no SwellGoal (vanilla's own zombie registration \
             registers none), so \
             its fuse must stay at 0 — if it climbs, every species is getting the \
             same table"
        );
    }

    /// A drowned attacks through **its own** table, not the fallback.
    ///
    /// This is the island check for the species this unit added. Before it,
    /// `drowned` was in no family, so `registrations_for` returned `FALLBACK` and
    /// a drowned in the running game could only stroll and look — the same
    /// observable as the llama control above. Its melee row is at goal-priority 2
    /// (the drowned's own behaviour-goals helper), not the zombie's 3, and it is the only
    /// modelled MOVE goal in its table besides the priority-7 stroll.
    #[test]
    fn a_drowned_attacks_from_its_own_table_rather_than_the_fallback() {
        let target = Vec3::new(6.5, 0.0, 0.5);
        let drowned = run("drowned", 0.23, target, 400);
        assert!(
            !is_fallback(registrations_for("drowned")),
            "precondition: drowned must have a table of its own"
        );
        assert!(
            drowned.attacks > 0,
            "a drowned must reach its DrownedAttackGoal: gap {:?}, attacks {}",
            drowned.gap,
            drowned.attacks
        );
        assert!(
            drowned.gap.1 < drowned.gap.0 - 3.0,
            "and close the distance: {:?}",
            drowned.gap
        );
    }

    /// A skeleton **shoots** and a wither skeleton **punches** — the behavioural
    /// gate for the skeleton weapon-branch fix (`48062b7`).
    ///
    /// A priority multiset cannot see this fix: the slot is 4 on both sides either
    /// way and only the occupant changes. So assert the observable instead, and
    /// assert it in *both* directions from the same world, target, speed and
    /// family — the two runs differ in exactly one table row.
    ///
    /// Under the pre-fix table (`MeleeAttackGoal` in [`SKELETON`]) the skeleton's
    /// three assertions all invert: it records attacks and no launches, and it
    /// closes to contact. That is the control, and it was run.
    ///
    /// The launch count is predicted rather than merely required to be positive.
    /// The bow draws for vanilla's own full-draw duration of 20 ticks (vanilla's
    /// own ticks-using-item threshold) and then waits out vanilla's own attack
    /// interval of 40,
    /// the value vanilla's own weapon-reassessment step installs below Hard via
    /// its own attack-interval getters — a 60-tick cycle whose first
    /// release lands on tick 21, so ticks 21, 81, … 741 give **13** releases in
    /// 800. A wrong interval or a missing draw phase lands somewhere else.
    ///
    /// The **target is 30 blocks out**, which is what makes the distance assertion
    /// mean anything. At 6 blocks a skeleton legitimately walks *in* — the goal
    /// only parks once its own see-time counter reaches 20 (vanilla's own
    /// per-tick update), and 20
    /// ticks at 0.25 blocks/tick is 5 blocks, so it arrives at 1.25 and a
    /// "did not close" bound would fail on correct code. Measured, and the reason
    /// this gate is shaped the way it is. From 30 the binding constraint is
    /// instead the bow's own attack radius of `15.0F` (vanilla's own
    /// bow-goal field), so the hold distance itself becomes the prediction.
    #[test]
    fn a_skeleton_shoots_from_range_and_a_wither_skeleton_closes_and_punches() {
        // 30 blocks out — beyond the bow's 15.0 radius, inside the 35.0 follow
        // range `run` gives the navigator.
        let target = Vec3::new(30.5, 0.0, 0.5);
        let skeleton = run("skeleton", 0.25, target, 800);
        let wither = run("wither_skeleton", 0.25, target, 800);

        // -- the skeleton shoots --------------------------------------------
        assert_eq!(
            skeleton.launches, 13,
            "a skeleton built only from the roster must release 13 arrows in 800 \
             ticks (a 20-tick draw plus a 40-tick interval, first release on tick \
             21). Measured {} launches, {} attacks, gap {:?}. Zero means the \
             priority-4 row is not the bow goal — an original defect this gate \
             caught",
            skeleton.launches, skeleton.attacks, skeleton.gap
        );
        assert_eq!(
            skeleton.attacks, 0,
            "and it must never swing: {} melee attacks means a MeleeAttackGoal is \
             still in the table, modelling a branch vanilla's own \
             weapon-reassessment step never \
             takes for a bow-holding skeleton",
            skeleton.attacks
        );
        // The bow's radius is 15.0 and it stops navigating on crossing it, so it
        // parks just inside. Bracket that rather than predict a float: the melee
        // hypothesis settles at `SpeciesContext::attack_reach` = 2.0, which is
        // nowhere near this window, and "never moved" (30.0) is excluded too.
        assert!(
            (12.0..17.0).contains(&skeleton.gap.1),
            "and it must stop at bow range, not melee range: the goal holds \
             position on crossing its 15.0 attack radius \
             (vanilla's own bow-goal attack radius and its own per-tick park \
             behaviour), so from \
             {} blocks it must settle near 15. Ended at {} — ~2 means a melee goal \
             walked it to contact, ~30 means nothing claimed MOVE at all",
            skeleton.gap.0,
            skeleton.gap.1
        );

        // -- the wither skeleton punches, from the same everything -----------
        assert!(
            wither.attacks > 0,
            "a wither skeleton keeps the melee branch (a STONE_SWORD fails \
             vanilla's own bow test), so it must reach attack: \
             gap {:?}, attacks {}",
            wither.gap,
            wither.attacks
        );
        assert_eq!(
            wither.launches, 0,
            "and must never shoot — {} launches means the bow row leaked into \
             WITHER_SKELETON",
            wither.launches
        );
        assert!(
            wither.gap.1 < 4.0,
            "and it must close all the way to contact, which the skeleton does not: \
             {:?}. If both species settle at the same distance they are sharing one \
             table",
            wither.gap
        );
    }
}
