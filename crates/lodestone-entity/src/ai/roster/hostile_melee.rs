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
//! # The family is not uniform, and three of its four branches prove it
//!
//! "Inherits its parent's `registerGoals`" is a claim about the jar, and it is
//! false in three different ways here. Each was checked per class rather than
//! assumed from the family:
//!
//! | class | declares | consequence |
//! |---|---|---|
//! | `Husk`, `ZombieVillager` | nothing | share [`ZOMBIE`] verbatim |
//! | `Drowned` | **`addBehaviourGoals`**, not `registerGoals` (`:91-103`) | keeps `Zombie`'s *three* base rows and replaces all nine others — [`DROWNED`] |
//! | `CaveSpider` | nothing | shares [`SPIDER`] |
//! | `Skeleton`, `Stray`, `Bogged`, `Parched` | nothing | share [`SKELETON`] |
//! | `WitherSkeleton` | `registerGoals` (`:38-41`) | one extra target row *before* `super` — [`WITHER_SKELETON`] |
//!
//! The `Drowned` case is the one a family-shaped assumption gets wrong.
//! `Zombie.registerGoals` calls `this.addBehaviourGoals()` at `:116`, so the
//! override is a *partial* replacement: a drowned still gets the turtle-egg goal
//! and both priority-8 look goals, but none of `SpearUseGoal`,
//! `ZombieAttackGoal`, `MoveThroughVillageGoal` or the water-avoiding stroll.
//! Transcribing `Zombie`'s whole table for it would give it four goals vanilla
//! does not register and omit six it does.
//!
//! `Parched` (`monster/skeleton/Parched.java:17`) is a 26.2 skeleton variant that
//! was not in issue #226's list at all; it declares no `registerGoals`, so it
//! shares the base table.
//!
//! # Known gaps, all disclosed in the tables
//!
//! * **The skeleton family shoots. Only the wither skeleton punches.** This entry
//!   used to say no ranged goals existed here, so the family took the melee half
//!   of `reassessWeaponGoal()` unconditionally. They exist now
//!   ([`super::ranged`]), and `48062b7` *replaced* [`SKELETON`]'s priority-4 row
//!   with `RangedBowAttackGoal` rather than adding to it — both candidates claim
//!   MOVE and vanilla removes both before re-adding exactly one (`:132-148`), so
//!   a second row would make the winner registration-order dependent.
//!   `AbstractSkeleton.populateDefaultEquipmentSlots` puts a `BOW` in the main
//!   hand **unconditionally** (`:111` — no random roll, no difficulty gate), so
//!   `reassessWeaponGoal`'s `is(Items.BOW)` test at `:137` holds for every
//!   normally-spawned skeleton and the melee `else` at `:146` never runs.
//!   **The boundary is the *equipment* override, not the goal method**:
//!   [`WITHER_SKELETON`] genuinely keeps melee, because
//!   `WitherSkeleton.java:74` overrides that method with a `STONE_SWORD` and so
//!   fails the bow test — it does not override `reassessWeaponGoal` at all, only
//!   calls it (`:88`). `Skeleton`, `Stray`, `Bogged` and `Parched` override
//!   neither. So [`melee_attack_1_2`] survives with exactly one reachable caller,
//!   and "skeletons shoot" is *not* the whole rule.
//!   A drowned still never throws its trident: [`super::ranged`] has a
//!   `trident_attack` builder, but [`DROWNED`]'s row is still
//!   [`Coverage::Missing`].
//! * **Nothing in the sim is a villager, iron golem, turtle, armadillo, axolotl or
//!   piglin**, so every target registration naming one is [`Coverage::Missing`]
//!   rather than a goal that would search for an entity class that cannot be
//!   spawned.
//! * **`HurtByTargetGoal.setAlertOthers` is not modelled** — our
//!   `HurtByTargetGoal` retaliates but never propagates anger to nearby mobs.
//!   That needs the sim-side census issue #233 owns.
//! * **No water-aware navigation exists**, so all five of the drowned's
//!   amphibious goals (`DrownedGoToWaterGoal`, `DrownedGoToBeachGoal`,
//!   `DrownedSwimUpGoal`, and the two that gate on `okTarget`) are `Missing`. A
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
//! forever. That was measured and true when written. It is not true now: issue
//! #455 (`23b3dd2`) made `find_nearest_target` read the `nearest_player` the
//! server's perception feed populates, cut by vanilla's `FOLLOW_RANGE` (a
//! `distanceToSqr` against `max(range, 2.0)`), and `mobs.rs` now passes the
//! per-species attribute through. A hosted zombie acquires a player it was never
//! told about and walks at it.
//!
//! **`set_attack_target` still has no production caller** — every one is a test,
//! a bench or `SimMob`'s wrapper — and that is why the gates below hand the
//! target over directly rather than waiting for acquisition. It keeps a failure
//! in this file attributable to a **table row**;
//! `crates/lodestone-entity/tests/target_acquisition.rs` is what proves
//! acquisition itself.
//!
//! What is still missing is **line of sight**: vanilla's is an eye-to-eye
//! `level.clip` ray (`TargetingConditions.java:90`), which #456's local block
//! cues structurally cannot answer, so it is unimplemented. That errs
//! *permissive* — a mob can acquire through a wall.
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
        // `Husk` (`monster/zombie/Husk.java:31`) and `ZombieVillager`
        // (`monster/zombie/ZombieVillager.java:61`) both extend `Zombie` and
        // declare neither `registerGoals` nor `addBehaviourGoals`, so they
        // inherit the whole table verbatim. Checked per class, not inferred from
        // the family — their sibling `Drowned` does override, one method down.
        "zombie" | "husk" | "zombie_villager" => Some(ZOMBIE),
        "drowned" => Some(DROWNED),
        "creeper" => Some(CREEPER),
        // `CaveSpider` (`monster/spider/CaveSpider.java:20`) likewise inherits
        // `Spider`'s; its only overrides are attributes and a poison effect on
        // hit.
        "spider" | "cave_spider" => Some(SPIDER),
        // `Skeleton`, `Stray`, `Bogged` and `Parched` all extend
        // `AbstractSkeleton` and declare no `registerGoals`.
        "skeleton" | "stray" | "bogged" | "parched" => Some(SKELETON),
        // `WitherSkeleton` *does* declare one
        // (`monster/skeleton/WitherSkeleton.java:38-41`), so it gets its own
        // table rather than sharing the base one.
        "wither_skeleton" => Some(WITHER_SKELETON),
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
    // `Zombie.ZombieAttackTurtleEggGoal(this, 1.0, 3)` — a `RemoveBlockGoal`
    // subclass: a 24-block spiral search (vertical range 3) for a `turtle_egg`,
    // then break-progress and a destroy intent. Neither the candidate search
    // nor the mutation exists on this seam (`docs/mob-block-perception.md`), and
    // no turtle can spawn in this sim regardless.
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

/// `monster/zombie/Zombie.java:113-115` plus `monster/zombie/Drowned.java:91-103`.
///
/// **The one species in this family whose parent's table is only partly
/// inherited.** `Drowned extends Zombie` (`Drowned.java:67`) and overrides
/// `addBehaviourGoals` — *not* `registerGoals`. Since `Zombie.registerGoals`
/// calls `this.addBehaviourGoals()` at `:116`, a drowned keeps exactly the three
/// rows `Zombie.registerGoals` adds itself (turtle-egg at 4, and both look goals
/// at 8) and replaces the other nine wholesale. Reading "Drowned inherits
/// Zombie's goals" off the class hierarchy would give it `SpearUseGoal`,
/// `ZombieAttackGoal`, `MoveThroughVillageGoal` and a water-avoiding stroll that
/// vanilla never registers on it, and lose all six goals that make it amphibious.
///
/// Of the twelve rows only four are modelled, and that is the honest figure: five
/// of the eight unmodelled ones are the amphibious navigation and the trident,
/// neither of which exists in this repo.
pub const DROWNED: &[Registration] = &[
    // -- inherited from `Zombie.registerGoals` (`Zombie.java:113-115`) --------
    Registration::missing(Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // -- `Drowned.addBehaviourGoals` (`Drowned.java:92-103`) ------------------
    // `Drowned.DrownedGoToWaterGoal(this, 1.0)` — seeks a water column to
    // submerge in. Needs the water-aware navigation `AmphibiousPathNavigation`
    // provides (`Drowned.java:86-88`) and this repo's `PathWorld` does not.
    Registration::missing(Selector::Goal, 1, "Drowned.DrownedGoToWaterGoal"),
    // `Drowned.DrownedTridentAttackGoal(this, 1.0, 40, 10.0F)` extends
    // `RangedAttackGoal` (`Drowned.java:531`) — ranged, so issue #227's, not
    // this unit's. Note it shares priority 2 with the melee goal below: vanilla
    // gates them on the held item rather than on precedence.
    Registration::missing(Selector::Goal, 2, "Drowned.DrownedTridentAttackGoal"),
    // `Drowned.DrownedAttackGoal(this, 1.0, false)` extends `ZombieAttackGoal`
    // (`Drowned.java:323`), adding only the `okTarget` check — vanilla's rule
    // that a drowned in water will chase anything but on land only chases a
    // target that is itself in water (`Drowned.java:223`). Not modelled: our
    // melee goal chases whatever target it is given, which on land makes a
    // drowned slightly more aggressive than vanilla's.
    Registration::goal(2, "Drowned.DrownedAttackGoal", melee_attack),
    // `Drowned.DrownedGoToBeachGoal(this, 1.0)` extends `MoveToBlockGoal`
    // (`Drowned.java:342`) — leaves the water at night to hunt. No sun/time
    // query on the AI seam and no water to leave.
    Registration::missing(Selector::Goal, 5, "Drowned.DrownedGoToBeachGoal"),
    // `Drowned.DrownedSwimUpGoal(this, 1.0, seaLevel)` (`Drowned.java:482`) —
    // rises toward the surface. Needs a sea-level query and vertical swimming.
    Registration::missing(Selector::Goal, 6, "Drowned.DrownedSwimUpGoal"),
    // `RandomStrollGoal(this, 1.0)` — the plain stroll, **not** the
    // water-avoiding subclass every other species in this family registers
    // (contrast `Zombie.java:123`). So this is the one stroll row in the roster
    // where our `RandomStrollGoal` is an exact match rather than a disclosed
    // simplification: a drowned is happy to wander into water.
    Registration::goal(7, "RandomStrollGoal", stroll),
    // `HurtByTargetGoal(this, Drowned.class).setAlertOthers(ZombifiedPiglin.class)`.
    // The second constructor argument is the *ignore* list — a drowned does not
    // retaliate against other drowned. Not modelled; ours has no class filter,
    // and nothing yet makes one drowned hurt another.
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    // `NearestAttackableTargetGoal<>(this, Player.class, 10, true, false, okTarget)`
    // — same `okTarget` water rule as the melee goal, unmodelled the same way.
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(AbstractVillager)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    // Vanilla's drowned hunt axolotls; nothing in this sim is one.
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(Axolotl)"),
    Registration::missing(Selector::Target, 5, "NearestAttackableTargetGoal(Turtle)"),
];

/// `monster/skeleton/AbstractSkeleton.java:76-86`, plus the priority-4 weapon
/// goal that `reassessWeaponGoal()` installs at `:144`/`:146` rather than in
/// `registerGoals`.
///
/// That priority-4 slot is the reason [`GoalSelector::remove`] exists: vanilla
/// removes *both* candidate goals and re-adds exactly one every time the
/// skeleton's held item changes (`:132-148`).
///
/// **Which one it re-adds is not a coin toss.** `populateDefaultEquipmentSlots`
/// puts a `BOW` in the main hand *unconditionally* (`:109-112` — no random roll,
/// no difficulty gate), so `usedWeapon.is(Items.BOW)` at `:137` is true for every
/// normally-spawned skeleton and the `else` at `:146` **never runs**. This table
/// therefore carries the bow half, which is the only branch the game reaches.
/// It used to carry the melee half, modelling a state a skeleton is never in
/// (#226) — and a *second* priority-4 row would have been worse than either,
/// since both goals claim MOVE and the winner would be registration-order
/// dependent.
///
/// [`WITHER_SKELETON`] is the exception, and the boundary is the **equipment**
/// override, not the goal method: `WitherSkeleton.java:74-76` overrides
/// `populateDefaultEquipmentSlots` to hand out a `STONE_SWORD`, and the
/// *inherited* `reassessWeaponGoal` then takes the `else`. It does not override
/// `reassessWeaponGoal` itself — only calls it (`:88`). `Skeleton`, `Stray`,
/// `Bogged` and `Parched` override neither method, so all four inherit the bow
/// and share this table.
///
/// One known simplification inside the shared row: `Bogged` and `Parched` *do*
/// override the interval, to `70` below Hard against `AbstractSkeleton`'s `40`
/// (`Bogged.java:117-124`, `Parched.java:57-64`, `AbstractSkeleton.java:151-157`).
/// All four get `40` here, because the interval is an argument to the shared
/// builder rather than a row identity, and nothing in this repo carries a world
/// difficulty for the Hard half either. Splitting it needs a per-species field on
/// [`SpeciesContext`], not a fourth table.
///
/// [`GoalSelector::remove`]: crate::ai::GoalSelector::remove
pub const SKELETON: &[Registration] = &[
    // `RestrictSunGoal(this)` and `FleeSunGoal(this, 1.0)` — two different
    // mechanisms, both absent. `RestrictSunGoal` reads no block: its gate is a
    // daytime query plus an empty HEAD slot, and its *effect* is
    // `GroundPathNavigation.setAvoidSun(true)` — a sky-light penalty in the
    // path evaluator, a pathfinder feature. `FleeSunGoal` needs a host-computed
    // shaded position (`FleeSunGoal.java:64-73` probes ten spots). So a
    // skeleton does not seek shade. Daylight *burning* is separately #226's.
    Registration::missing(Selector::Goal, 2, "RestrictSunGoal"),
    Registration::missing(Selector::Goal, 3, "FleeSunGoal"),
    Registration::goal(3, "AvoidEntityGoal(Wolf)", avoid_entity),
    // `reassessWeaponGoal()` at `:144`: the bow branch, the only one a
    // normally-spawned skeleton takes. Vanilla's `bowGoal` field is
    // `new RangedBowAttackGoal<>(this, 1.0, 20, 15.0F)` (`:55`), with the
    // interval overwritten per difficulty at `:138-143`. The `else` at `:146`
    // installs `meleeGoal` and belongs to `WITHER_SKELETON` alone.
    Registration::goal(4, "RangedBowAttackGoal", super::ranged::bow_attack),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(6, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal(Player)", nearest_attackable_target),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(IronGolem)"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(Turtle)"),
];

/// `monster/skeleton/WitherSkeleton.java:38-41` — one extra target registration,
/// then everything in [`SKELETON`].
///
/// `WitherSkeleton` is the only class in this family that declares
/// `registerGoals`, and it adds its row **before** calling `super.registerGoals()`
/// at `:40`, which is why the piglin row comes first here. Vanilla's ordering is
/// observable only among rows of equal priority, and this row shares priority 3
/// with two others, so transcribing the order matters even though all three are
/// unmodelled.
///
/// This table used to be [`SKELETON`], shared, with a comment conceding it was
/// "knowingly not a complete transcription" because the extra row is `Missing`
/// either way. Splitting it costs eleven duplicated lines and buys a table that a
/// multiset gate can actually check against the jar — and
/// `wither_skeleton_is_the_base_table_plus_the_piglin_row` pins the duplication so
/// the two cannot drift.
pub const WITHER_SKELETON: &[Registration] = &[
    // `NearestAttackableTargetGoal<>(this, AbstractPiglin.class, true)`
    // (`WitherSkeleton.java:39`). No piglin can exist in this sim.
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal(AbstractPiglin)"),
    // -- `super.registerGoals()` (`AbstractSkeleton.java:77-86`) + `:146` -----
    Registration::missing(Selector::Goal, 2, "RestrictSunGoal"),
    Registration::missing(Selector::Goal, 3, "FleeSunGoal"),
    Registration::goal(3, "AvoidEntityGoal(Wolf)", avoid_entity),
    // The `else` half of `reassessWeaponGoal` (`:146`) — the one branch of this
    // family that is really melee, because `WitherSkeleton.java:74-76` overrides
    // `populateDefaultEquipmentSlots` with a `STONE_SWORD` and so fails the
    // `is(Items.BOW)` test at `:137`. [`SKELETON`] takes `:144` instead.
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

/// `MeleeAttackGoal(this, 1.2, false)` — `AbstractSkeleton`'s `meleeGoal` field
/// (`monster/skeleton/AbstractSkeleton.java:56`), faster than the 1.0 every other
/// species in this family uses.
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
    /// every one of the twelve species in [`SPECIES`] is pinned to a cited
    /// `.java` line. The expectations here were transcribed from the jar, not from
    /// the tables above — copying them from `DROWNED` would be satisfied by any
    /// table, right or wrong.
    #[test]
    fn drowned_and_wither_skeleton_match_the_jars_addgoal_block() {
        type Row = (Selector, i32, &'static str);
        let cases: &[(&str, &str, &[Row])] = &[
            (
                "drowned",
                "monster/zombie/Zombie.java:113-115 (registerGoals) + \
                 monster/zombie/Drowned.java:92-103 (addBehaviourGoals override)",
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
                "monster/skeleton/WitherSkeleton.java:38-41 + \
                 monster/skeleton/AbstractSkeleton.java:77-86 + reassessWeaponGoal :146",
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

        for &(species, cite, want) in cases {
            let got: Vec<Row> = registrations_for(species)
                .iter()
                .map(|r| (r.selector, r.priority, r.vanilla))
                .collect();
            assert_eq!(
                got,
                want.to_vec(),
                "{species}'s table does not match {cite} — re-read the jar before \
                 editing either side of this"
            );
        }
    }

    /// Which classes declare `registerGoals`/`addBehaviourGoals` is a claim about
    /// the jar, and it is what decides whether two species share a table. It was
    /// wrong for two of the four branches of this family before this unit, so it
    /// is gated rather than commented.
    ///
    /// `/usr/bin/grep -n "registerGoals\|addGoal\|addBehaviourGoals"` over each
    /// class: zero hits for `Husk`, `ZombieVillager`, `CaveSpider`, `Skeleton`,
    /// `Stray`, `Bogged` and `Parched`; `Drowned.java:91` overrides
    /// `addBehaviourGoals`; `WitherSkeleton.java:38` overrides `registerGoals`.
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
                "{child} extends AbstractSkeleton and declares no registerGoals"
            );
        }

        // And the two that must NOT share, which is the half a family-shaped
        // assumption gets wrong. Without these the test above is satisfied by
        // "everything in the family shares one table".
        assert!(
            !same("zombie", "drowned"),
            "Drowned overrides addBehaviourGoals (Drowned.java:91), so it keeps \
             only Zombie's three registerGoals rows — sharing zombie's table \
             would give it four goals vanilla never registers and lose six it does"
        );
        assert!(
            !same("skeleton", "wither_skeleton"),
            "WitherSkeleton declares registerGoals (WitherSkeleton.java:38)"
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
    /// than filtered away. `reassessWeaponGoal` picks its branch from the held
    /// item, so the skeleton's unconditional `BOW`
    /// (`AbstractSkeleton.java:109-112`) reaches `:144` and the wither's
    /// `STONE_SWORD` (`WitherSkeleton.java:74-76`) reaches `:146`. A gate that
    /// merely skipped priority 4 would still pass with both tables carrying the
    /// *same* goal — which is precisely the state #226 found.
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
                 candidates and re-adds one (`AbstractSkeleton.java:132-148`), so a \
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
            "WitherSkeleton.java:40 calls super.registerGoals(), so every row other \
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
    /// The interesting row is the wither skeleton's melee. Vanilla's `meleeGoal`
    /// field is `new MeleeAttackGoal(this, 1.2, false)`
    /// (`monster/skeleton/AbstractSkeleton.java:56`) — every other melee row in
    /// this family is `1.0` — so at the skeleton family's `MOVEMENT_SPEED 0.25`
    /// (`:90`) the two hypotheses are `0.30` and `0.25`, and the assertion is
    /// written to fail on the wrong one.
    ///
    /// The plain skeleton's priority-4 row is the *bow* goal, whose multiplier is
    /// `1.0` (`:55`), so it cannot host that discriminator — 1.0 × 0.25 is its
    /// bare movement speed. It is still pinned to the value, just without the
    /// inequality; the wither carries the inequality.
    #[test]
    fn transcribed_speed_multipliers_land_on_the_jars_value() {
        // (species, movement_speed, vanilla row, jar factor)
        let cases: &[(&str, f64, &str, f64)] = &[
            // `AbstractSkeleton.java:55` — the bowGoal's 1.0, at `:90`'s
            // MOVEMENT_SPEED 0.25.
            ("skeleton", 0.25, "RangedBowAttackGoal", 1.0),
            // `AbstractSkeleton.java:56` — the meleeGoal's 1.2, at the same 0.25.
            // Only the wither ever installs it.
            ("wither_skeleton", 0.25, "MeleeAttackGoal", 1.2),
            // `Creeper.java:69` — 1.0, at 0.25.
            ("creeper", 0.25, "MeleeAttackGoal", 1.0),
            // `Zombie.java:121` — `ZombieAttackGoal(this, 1.0, false)`, at
            // `Zombie.java:134`'s MOVEMENT_SPEED 0.23.
            ("zombie", 0.23, "ZombieAttackGoal", 1.0),
            // `Drowned.java:94` — 1.0. `Drowned.createAttributes` is
            // `Zombie.createAttributes().add(STEP_HEIGHT, 1.0)`
            // (`Drowned.java:81-83`), so the speed is the zombie's 0.23.
            ("drowned", 0.23, "Drowned.DrownedAttackGoal", 1.0),
            // `Creeper.java:70` and `Spider.java:62` — the 0.8 strolls.
            ("creeper", 0.25, "WaterAvoidingRandomStrollGoal", 0.8),
            ("spider", 0.3, "WaterAvoidingRandomStrollGoal", 0.8),
            // `Zombie.java:123` — 1.0.
            ("zombie", 0.23, "WaterAvoidingRandomStrollGoal", 1.0),
            // `Drowned.java:97` — the *plain* `RandomStrollGoal(this, 1.0)`.
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
             is the 1.0 hypothesis, and AbstractSkeleton.java:56 is 1.2"
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
    /// observe cannot tell whether the roster installed it — the closed loop that
    /// hid issue #441's island. `NavigatingMob` is the only production
    /// implementor of `MobController`, so the goals run against the same
    /// perception the running game gives them.
    ///
    /// The attack target is set explicitly so this gate stays about the *roster*
    /// rather than about acquisition: `find_nearest_target` reads the perception
    /// feed as of #455, and `tests/target_acquisition.rs` is what proves that.
    /// Handing the target over directly keeps this file's failures attributable
    /// to a table row.
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
    /// (`monster/Creeper.java:66`, `:69`), and both claim MOVE, so the *only*
    /// reason the fuse ever climbs is that 2 outranks 4. Transcribe the swell at
    /// any number above 4 and melee holds MOVE and the creeper never primes,
    /// which is the control A3 ran and this gate inherits.
    #[test]
    fn a_creeper_swells_because_vanillas_priority_2_outranks_melees_4() {
        // Inside `SwellGoal`'s 9.0 squared proximity (`ai/goal/SwellGoal.java:20`).
        let close = Vec3::new(2.0, 0.0, 0.5);
        let creeper = run("creeper", 0.25, close, 40);
        let zombie = run("zombie", 0.23, close, 40);

        assert!(
            creeper.swell > 0,
            "a creeper's fuse must climb with a target 1.5 blocks away. If this \
             fails, check that Creeper.java:66's priority 2 is still below :69's \
             4 in CREEPER — a MeleeAttackGoal holding MOVE prevents the swell"
        );
        assert_eq!(
            zombie.swell, 0,
            "a zombie has no SwellGoal (Zombie.java:112-128 registers none), so \
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
    /// (`Drowned.java:94`), not the zombie's 3, and it is the only modelled MOVE
    /// goal in its table besides the priority-7 stroll.
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
    /// gate for #226's weapon-branch fix.
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
    /// The bow draws for `BOW_FULL_DRAW_TICKS` = 20 (vanilla's
    /// `getTicksUsingItem() >= 20`) and then waits out `getAttackInterval()` = 40,
    /// the value `reassessWeaponGoal` installs below Hard
    /// (`AbstractSkeleton.java:139-143`, `:151-157`) — a 60-tick cycle whose first
    /// release lands on tick 21, so ticks 21, 81, … 741 give **13** releases in
    /// 800. A wrong interval or a missing draw phase lands somewhere else.
    ///
    /// The **target is 30 blocks out**, which is what makes the distance assertion
    /// mean anything. At 6 blocks a skeleton legitimately walks *in* — the goal
    /// only parks once `seeTime` reaches 20 (`RangedBowAttackGoal.java:86-92`), and
    /// 20 ticks at 0.25 blocks/tick is 5 blocks, so it arrives at 1.25 and a
    /// "did not close" bound would fail on correct code. Measured, and the reason
    /// this gate is shaped the way it is. From 30 the binding constraint is instead
    /// the bow's own `attackRadius` of `15.0F` (`AbstractSkeleton.java:55`), so the
    /// hold distance itself becomes the prediction.
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
             priority-4 row is not the bow goal — which is #226's original defect",
            skeleton.launches, skeleton.attacks, skeleton.gap
        );
        assert_eq!(
            skeleton.attacks, 0,
            "and it must never swing: {} melee attacks means a MeleeAttackGoal is \
             still in the table, modelling a branch `reassessWeaponGoal` never \
             takes for a bow-holding skeleton (AbstractSkeleton.java:137, :146)",
            skeleton.attacks
        );
        // The bow's radius is 15.0 and it stops navigating on crossing it, so it
        // parks just inside. Bracket that rather than predict a float: the melee
        // hypothesis settles at `SpeciesContext::attack_reach` = 2.0, which is
        // nowhere near this window, and "never moved" (30.0) is excluded too.
        assert!(
            (12.0..17.0).contains(&skeleton.gap.1),
            "and it must stop at bow range, not melee range: the goal holds \
             position on crossing its 15.0 attackRadius \
             (AbstractSkeleton.java:55, RangedBowAttackGoal.java:86-92), so from \
             {} blocks it must settle near 15. Ended at {} — ~2 means a melee goal \
             walked it to contact, ~30 means nothing claimed MOVE at all",
            skeleton.gap.0,
            skeleton.gap.1
        );

        // -- the wither skeleton punches, from the same everything -----------
        assert!(
            wither.attacks > 0,
            "a wither skeleton keeps the melee branch (a STONE_SWORD fails \
             `is(Items.BOW)`, WitherSkeleton.java:74-76), so it must reach attack: \
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
