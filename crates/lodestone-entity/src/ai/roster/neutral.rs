//! Goal sets for the neutral mobs that hold a grudge: enderman, zombified
//! piglin, bee, wolf.
//!
//! # What it is
//!
//! One [`Registration`] table per species, transcribed from that species'
//! `registerGoals()` in `.cache/mc/26.2/src/net/minecraft/world/entity/`.
//!
//! **Read the next section before adding behaviour here.** The four headline
//! mechanisms this family exists to cover — enderman teleport-on-stare,
//! zombified-piglin group aggro, bee sting-then-die, wolf pack aggro — are
//! **not** in these tables as
//! [`Coverage::Modelled`] rows, and that is a finding rather than an omission.
//! Each of the four waits on a primitive that did not exist on
//! [`MobController`](super::MobController) when the tables were written; all
//! five have since landed, but a primitive is necessary, not
//! sufficient — each mechanism still needs its goal and its host census — so
//! each is a [`Coverage::Missing`] row naming what it still waits on.
//!
//! # Why the four headline mechanisms are `Missing`
//!
//! A roster entry can only be as good as the goals it can build, and a goal can
//! only ask [`MobController`](super::MobController) questions it has methods
//! for. Measured against the trait, **five primitives were absent** and between
//! them accounted for every one of the four. All five have landed;
//! the table is kept as the record of what each needed, with the landed state
//! marked:
//!
//! | absent primitive | blocks | landed as |
//! |---|---|---|
//! | the anger timer + anger target (a *grudge*) | all four | host-side `SimMob::anger` + `MobController::angry_target` (the deadline stays on the host) |
//! | "is that player looking at me" (a gaze test) | enderman freeze + stare | `MobController::is_being_stared_at` + free `is_in_view_cone` geometry, consumed by both `EndermanFreezeWhenLookedAt` and `EndermanLookForPlayerGoal` below (both `Coverage::Modelled`). **The host feed has landed**: `lodestone_server::mobs::MobSim::tick_with_terrain` computes it every tick from each connected player's real position and view direction (`PerceivedPlayer::perception.view_direction`), so `is_being_stared_at` is a live boolean in the running game, not a permanent `false` — see this table's own doc on the enderman row |
//! | relocating a mob instantly (a teleport) | enderman teleport | `MobController::teleport_to`, host-commanded via `SimMob::teleport_to` |
//! | a mob damaging **itself** | bee sting-then-die | `MobController::damage_self`, drained by `MobSim::tick` through the damage pipeline |
//! | an owner relationship | wolf tame half | `MobController::owner_position`/`is_tame`/`is_ordered_to_sit` + host-side `SimMob::owner`. **No longer blocked**: `lodestone_server::mobs::PerceivedPlayer` puts a `PlayerIdentity` (account uuid + runtime entity id) at the perception seam, so a mob can be owned *by a player*, and the wolf's `SitWhenOrderedToGoal` and `FollowOwnerGoal` rows below are `Modelled` |
//!
//! Group propagation needs a sixth thing that is not a trait method at all: a
//! same-species census, which the seam deliberately does not expose (a goal sees
//! `Option<Vec3>` answers, never a population). `MobSim::feed_perception` is
//! where that resolution happens for every other question, and it is where
//! `alertOthers` belongs.
//!
//! So the honest state of this family is: **the tables are complete and cited; the
//! mechanisms are blocked on the seam, in five specific, named places — all of
//! which now have a landed primitive.** A primitive is not a
//! mechanism: each of the four still needs its goal (a bee sting hook, a tame
//! interaction) and its census (`alertOthers`) before it can be a
//! [`Coverage::Modelled`] row. Writing one of those *here* before its goal
//! exists would produce a type with no possible consumer — the island this
//! repo's dominant defect class is named after — so this module does not
//! contain most of them.
//!
//! **The enderman's stare is now the exception that closed, not the one still
//! open.** Both halves are real [`Coverage::Modelled`] rows below, driven off
//! the real [`MobController::is_being_stared_at`](super::MobController::is_being_stared_at)
//! seam and reachable through the real roster: `EndermanFreezeWhenLookedAt`
//! (goal priority 1) pins the head and stops the navigation while stared at,
//! and `EndermanLookForPlayerGoal` (target priority 1) is what actually turns
//! a stare into an attack target — see its own doc comment for the port and
//! for the identity/line-of-sight/landing-check narrowings the position-only
//! seam forces. This used to be the honest intermediate state
//! `nearest_player`/`temptation` sat in before their own feed lines landed —
//! a goal with no possible input, `can_use` reading a permanent `false` — but
//! that state ended once `MobSim::tick_with_terrain` started computing
//! `is_being_stared_at` from a real per-player view vector each tick; see this
//! table's own doc on the enderman row for the citation.
//!
//! # The trap that decided the target rows
//!
//! Three of these species register a player-targeting goal whose last argument is
//! an **anger predicate**: `NearestAttackableTargetGoal<>(this, Player.class, 10,
//! true, false, this::isAngryAt)` (`ZombifiedPiglin.addBehaviourGoals`,
//! `Wolf.registerGoals`, and via `Bee.BeeBecomeAngryTargetGoal`
//! in `Bee.registerGoals`, whose `Bee.BeeBecomeAngryTargetGoal.beeCanTarget` is
//! `isAngry() && !hasStung()`).
//!
//! **That predicate is the entire difference between a neutral mob and a hostile
//! one.** Our [`NearestAttackableTargetGoal`](super::nearest_attackable_target)
//! takes no predicate, so registering it for these species would make a
//! zombified piglin, a wolf and a bee attack players **on sight** — strictly
//! worse than the mob doing nothing, and worse than vanilla in a way a priority
//! gate cannot see. Every such row is therefore [`Coverage::Missing`].
//!
//! This was written down while `NavigatingMob::find_nearest_target` still
//! returned `self.attack_target` instead of searching, so no
//! `NearestAttackableTargetGoal` could fire in production. That self-loop is
//! fixed now, which makes the anger predicate load-bearing rather than
//! theoretical: a `Modelled` row here would silently turn three neutral
//! species hostile. Do not "upgrade" these rows without an anger predicate to
//! gate them.
//!
//! # What does still reach behaviour
//!
//! Retaliation does, and it is not a consolation prize: `HurtByTargetGoal::start`
//! calls `set_attack_target`, which is what
//! `MeleeAttackGoal::can_use` reads, and `last_hurt_by` is really fed
//! by `MobSim`. So **hurt → retaliate → close → strike** is a live
//! chain through the real seam for all four species, and it is the correct
//! neutral-mob behaviour: these mobs fight back rather than hunt. The gates below
//! drive exactly that, plus the flag contention that makes a wolf *flee before it
//! fights* while a zombified piglin fights immediately — a real per-species
//! difference, since the piglin's table carries no panic goal.
//!
//! One measured caveat on those behavioural gates, recorded because the opposite
//! was believed first: they are sensitive to **which rows a species has**, not to
//! the rows' priority *numbers*. Two deliberate priority mutations left them green
//! (see [`tests::a_hurt_wolf_flees_before_it_fights_because_panic_is_uninterruptable`]).
//! The priority guard is the multiset gate against the jar.
//!
//! # How to change it
//!
//! Add a species path to [`SPECIES`] and an arm to [`lookup`]. `SPECIES` is
//! iterated by `roster`'s invariant gates, so a species missing from it is a
//! species nothing checks.
//!
//! **Do not claim `llama`** (nor `panda`/`polar_bear` without checking). The
//! stub this module replaced listed them as in scope, but `llama` is load-bearing
//! as the *rosterless* control in at least four existing gates —
//! `super::tests::unknown_species_falls_back_and_known_species_differ`,
//! `hostile_melee`'s fallback control, and
//! `lodestone-server/tests/mob_roster.rs` — every one of which asserts
//! `is_fallback(registrations_for("llama"))`. Claiming it turns those green
//! controls red. Pick a different rosterless species for a control first.
//!
//! ## Gotchas
//!
//! * **This family is not uniform, and the exception is the interesting one.**
//!   `ZombifiedPiglin` declares **no** `registerGoals`; it overrides
//!   `ZombifiedPiglin.addBehaviourGoals`, the hook `Zombie.registerGoals` calls.
//!   Its table is therefore Zombie's own three rows **plus**
//!   its six, and because its override does not call `super`, Zombie's
//!   `MoveThroughVillageGoal` is *dropped* and `SpearUseGoal` moves from priority
//!   2 to 1. Transcribing only the method that carries the species' name would
//!   miss three rows and mis-number two.
//! * **`adjustedTickDelay` halves every delay it wraps.**
//!   `Goal.adjustedTickDelay` is `requiresUpdateEveryTick() ? ticks :
//!   reducedTickDelay(ticks)` and `Goal.reducedTickDelay`
//!   is `Mth.positiveCeilDiv(ticks, 2)`. Neither
//!   `NearestAttackableTargetGoal`, `TargetGoal` nor
//!   `EnderMan.EndermanLookForPlayerGoal` overrides `requiresUpdateEveryTick`, so
//!   the enderman's aggro delay is **3** ticks, not the literal 5, and its
//!   teleport-towards delay is **15**, not 30. Any future enderman work that
//!   transcribes the literal will be off by a factor of two.
//! * **The grudge is an absolute timestamp in 26.2, not a countdown.**
//!   `NeutralMob.setTimeToRemainAngry(remaining)` stores
//!   `level.getGameTime() + remaining` and `NeutralMob.isAngry()`
//!   compares against the clock. A countdown field is the pre-26.2
//!   model; porting it would drift against a paused or stepped tick loop.
//! * **The alert box is a box, not a sphere, and its vertical extent is a flat
//!   10.** `HurtByTargetGoal.alertOthers` inflates by
//!   `(followRange, 10.0, followRange)` (`HurtByTargetGoal.ALERT_RANGE_Y`, the
//!   flat `10`), and the piglin's own `ZombifiedPiglin.alertOthers` uses the same
//!   shape. Collapsing it to one radius is wrong in
//!   the corners in both directions.

use crate::ai::goal::Goal;
use crate::ai::goals::{
    EndermanFreezeWhenLookedAt, EndermanLookForPlayerGoal, FollowOwnerGoal, FollowParentGoal,
    PanicGoal, TemptGoal,
};

use super::{
    Registration, Selector, SpeciesContext, breed_1_0, float_goal, hurt_by_target,
    look_at_player_8, melee_attack, random_look_around, sit_when_ordered, stroll,
};

/// Every species this family claims. Iterated by `roster`'s invariant gates.
///
/// Four, deliberately. `llama`, `panda` and `polar_bear` are neutral mobs and
/// belong here eventually, but `llama` is the rosterless control in several
/// existing gates — see this module's "How to change it".
pub const SPECIES: &[&str] = &["enderman", "zombified_piglin", "bee", "wolf"];

/// Resolves a species path to its table, or `None` if this family does not claim
/// it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        "enderman" => Some(ENDERMAN),
        "zombified_piglin" => Some(ZOMBIFIED_PIGLIN),
        "bee" => Some(BEE),
        "wolf" => Some(WOLF),
        _ => None,
    }
}

/// `EnderMan.registerGoals`.
///
/// Attributes from `EnderMan.createAttributes` — `MAX_HEALTH 40.0`, `MOVEMENT_SPEED 0.3F`,
/// `ATTACK_DAMAGE 7.0`, `FOLLOW_RANGE 64.0`.
///
/// The stare is two goals, not one, and both are [`Coverage::Modelled`] below.
/// `EndermanFreezeWhenLookedAt` at goal priority 1 stops the navigation while
/// a player within 16 blocks (`distanceToSqr <= 256.0`, in
/// `EnderMan.EndermanFreezeWhenLookedAt.canUse`) is staring.
/// `EndermanLookForPlayerGoal` at target priority 1 does the aggro/teleport —
/// see its own doc comment for the port and its disclosed narrowings. Both
/// route through `EnderMan.isBeingStaredBy`, which is
/// `LivingEntity.PLAYER_NOT_WEARING_DISGUISE_ITEM` (a carved
/// pumpkin, via `ItemTags.GAZE_DISGUISE_EQUIPMENT`, defeats the stare — **not
/// modelled**, `PlayerPerception` carries no armour-slot data) **and**
/// `isLookingAtMe(player, 0.025, true, false, getEyeY())`.
///
/// That gaze test has a real geometric definition worth citing rather than
/// approximating (`LivingEntity.isLookingAtMe`): with the player's normalised
/// view vector `look` and the normalised offset `dir` from player eyes to the
/// enderman, a stare is `look.dot(dir) > 1.0 - coneSize / dist` plus line of
/// sight. `coneSize` is `0.025` and `adjustForDistance` is `true`, so the
/// tolerance is **divided by distance** — the required precision *increases*
/// the further away the player stands (the same offset that reads as a stare
/// up close reads as a near-miss at range), which is the opposite of the
/// fixed-angle cone an approximation reaches for first. See
/// [`is_in_view_cone`](crate::ai::mob::is_in_view_cone)'s own doc comment for
/// the worked example.
pub const ENDERMAN: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    // `EnderMan.EndermanFreezeWhenLookedAt`, flags `{JUMP, MOVE}`.
    // Built on `MobController::is_being_stared_at` — see
    // `EndermanFreezeWhenLookedAt`'s own doc comment for the port. The
    // host feed that computes the boolean from a real player view vector has
    // not landed (`lodestone_server::mobs::PlayerPerception` carries none
    // yet), so `can_use` reads a permanent `false` in the running game until
    // it does; the goal itself is real and exercised against the seam by this
    // module's own tests.
    Registration::goal(1, "EnderMan.EndermanFreezeWhenLookedAt", freeze_when_looked_at),
    Registration::goal(2, "MeleeAttackGoal", melee_attack),
    // `WaterAvoidingRandomStrollGoal(this, 1.0, 0.0F)`. The third argument is
    // vanilla's probability of preferring a dry destination; ours has no such
    // parameter (see `super::stroll`), so only the 1.0 speed factor transcribes.
    Registration::goal(7, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // Carrying and placing blocks — `DATA_CARRY_STATE`
    // (`EnderMan.setCarriedBlock`/`EnderMan.getCarriedBlock`) plus a
    // block-placement path the AI seam has no access to.
    Registration::missing(Selector::Goal, 10, "EnderMan.EndermanLeaveBlockGoal"),
    Registration::missing(Selector::Goal, 11, "EnderMan.EndermanTakeBlockGoal"),
    // `EnderMan.EndermanLookForPlayerGoal(this, this::isAngryAt)` — the
    // teleport-on-stare goal. `Coverage::Modelled` now: see
    // `EndermanLookForPlayerGoal`'s own doc comment for the port and its
    // disclosed narrowings (no per-player identity, no line of sight, no
    // landing check on the teleport).
    Registration::target(1, "EnderMan.EndermanLookForPlayerGoal", look_for_player),
    Registration::target(2, "HurtByTargetGoal", hurt_by_target),
    // No endermite can exist in this sim.
    Registration::missing(
        Selector::Target,
        3,
        "NearestAttackableTargetGoal(Endermite)",
    ),
    // `ResetUniversalAngerTargetGoal<>(this, false)` — clears anger when the
    // `universalAnger` gamerule turns it off. No anger state, no gamerule.
    Registration::missing(Selector::Target, 4, "ResetUniversalAngerTargetGoal"),
];

/// `Zombie.registerGoals`'s three rows **plus**
/// `ZombifiedPiglin.addBehaviourGoals`'s six.
///
/// The non-uniform species in this family. `ZombifiedPiglin` declares no
/// `registerGoals`; it overrides `ZombifiedPiglin.addBehaviourGoals`, which
/// `Zombie.registerGoals` calls after adding three rows of
/// its own. Those three are inherited verbatim and open this table.
/// The override does **not** call `super.addBehaviourGoals()`, so Zombie's
/// `MoveThroughVillageGoal` is absent here — correctly, not
/// by oversight — and `SpearUseGoal`/`ZombieAttackGoal` sit at 1/2 rather than
/// Zombie's 2/3.
///
/// Attributes from `Zombie.createAttributes()` plus
/// `SPAWN_REINFORCEMENTS_CHANCE 0.0`, `MOVEMENT_SPEED 0.23F`,
/// `ATTACK_DAMAGE 5.0`. `FOLLOW_RANGE 35.0` is inherited from `Zombie.createAttributes` and
/// is what sizes the alert box below.
///
/// Group aggro is `Missing` in two halves. The propagation itself is
/// `ZombifiedPiglin.alertOthers`: every other zombified piglin in a box of
/// `(FOLLOW_RANGE, 10.0, FOLLOW_RANGE)` = **±35 XZ, ±10 Y** that has no target
/// yet and is not allied to the victim's attacker has `setTarget` called on it.
/// It is throttled by `ZombifiedPiglin.maybeAlertOthers` on `ALERT_INTERVAL =
/// TimeUtil.rangeOfSeconds(4, 6)` = **[80, 120] ticks** and requires line of
/// sight to the target. Note it is driven from `ZombifiedPiglin.customServerAiStep`,
/// **not** from a goal — so it is not a roster row at all, and modelling it means
/// a census in `MobSim::tick`, not a `Registration` here.
pub const ZOMBIFIED_PIGLIN: &[Registration] = &[
    // Inherited from `Zombie.registerGoals`.
    Registration::missing(Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // `ZombifiedPiglin.addBehaviourGoals` from here down.
    // `SpearUseGoal<>(this, 1.0, 1.0, 10.0F, 2.0F)` — a ranged goal, so it
    // belongs to the ranged-attack family, not this one.
    Registration::missing(Selector::Goal, 1, "SpearUseGoal"),
    // `ZombieAttackGoal(this, 1.0, false)` extends `MeleeAttackGoal`, adding only
    // the raised-arms metadata flag while it runs.
    Registration::goal(2, "ZombieAttackGoal", melee_attack),
    Registration::goal(7, "WaterAvoidingRandomStrollGoal", stroll),
    // `HurtByTargetGoal(this).setAlertOthers()`. Retaliation is modelled; the
    // same-type alert is not — see this table's doc comment.
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    // The anger predicate is what makes a piglin neutral; see the module doc.
    Registration::missing(
        Selector::Target,
        2,
        "NearestAttackableTargetGoal(Player,isAngryAt)",
    ),
    Registration::missing(Selector::Target, 3, "ResetUniversalAngerTargetGoal"),
];

/// `Bee.registerGoals`.
///
/// Seventeen registrations, of which this repo builds five. Note that the
/// `beePollinateGoal`, `goToHiveGoal` and `goToKnownFlowerGoal` field
/// assignments inside `Bee.registerGoals` are field assignments rather than
/// `addGoal` calls — a line-count transcription of that block gets twenty
/// rows and three of them are not registrations.
///
/// Attributes from `Bee.createAttributes()` — `Animal.createAnimalAttributes()` plus
/// `MAX_HEALTH 10.0`, `FLYING_SPEED 0.6F`, `MOVEMENT_SPEED 0.3F`,
/// `ATTACK_DAMAGE 2.0`.
///
/// **Sting-then-die is not "die on stinging", and the difference is the whole
/// mechanism.** `Bee.doHurtTarget` deals `(int)ATTACK_DAMAGE` = 2,
/// applies poison for 0/10/18 seconds by difficulty, then sets
/// `hasStung` and calls `stopBeingAngry()` — the bee survives
/// the sting. Death comes later and stochastically, in `Bee.customServerAiStep`:
/// once `hasStung`, `timeSinceSting++` each tick and every fifth
/// tick the bee kills itself with probability
/// `1 / clamp(1200 - timeSinceSting, 1, 1200)`. The clamp is what bounds it — at
/// `timeSinceSting == 1200` the divisor is 1 and `nextInt(1) == 0` always, and
/// 1200 is a multiple of 5, so **a stung bee is certainly dead by 1200 ticks
/// after the sting and certainly alive one tick after it.**
///
/// That shape is a drain-flag, not a goal: the closest precedent in this repo is
/// the creeper fuse (`SwellGoal` sets a direction, `NavigatingMob::advance`
/// integrates it, `take_detonated` drains it, `MobSim::tick` resolves it). A bee
/// needs the same three pieces plus access to its own health, which lives on
/// `SimMob`.
pub const BEE: &[Registration] = &[
    // `Bee.BeeAttackGoal(this, 1.4F, true)` extends
    // `MeleeAttackGoal`, but its `Bee.BeeAttackGoal.canUse` adds `isAngry() && !hasStung()`.
    // Registering a bare melee goal at priority 0 would make every bee
    // attack on sight and keep attacking after it had stung — two wrongs, both
    // invisible to a priority gate.
    Registration::missing(Selector::Goal, 0, "Bee.BeeAttackGoal"),
    Registration::missing(Selector::Goal, 1, "Bee.BeeEnterHiveGoal"),
    Registration::goal(2, "BreedGoal", breed_1_0),
    // `TemptGoal(this, 1.25, i -> i.is(ItemTags.BEE_FOOD), false)`. The goal is
    // real; whether a held item tempts a bee is perception, and `mobs.rs`'s
    // interim `tempt_food` has no `bee` arm yet — B2 owns the generated tag
    // table that fixes that for every species at once.
    Registration::goal(3, "TemptGoal(BEE_FOOD)", tempt_1_25),
    Registration::missing(Selector::Goal, 3, "Bee.ValidateHiveGoal"),
    Registration::missing(Selector::Goal, 3, "Bee.ValidateFlowerGoal"),
    Registration::missing(Selector::Goal, 4, "Bee.BeePollinateGoal"),
    Registration::goal(5, "FollowParentGoal", follow_parent_1_25),
    Registration::missing(Selector::Goal, 5, "Bee.BeeLocateHiveGoal"),
    Registration::missing(Selector::Goal, 5, "Bee.BeeGoToHiveGoal"),
    Registration::missing(Selector::Goal, 6, "Bee.BeeGoToKnownFlowerGoal"),
    Registration::missing(Selector::Goal, 7, "Bee.BeeGrowCropGoal"),
    // `Bee.BeeWanderGoal` picks a destination in flight and hands it to a flying
    // navigation. Our `RandomStrollGoal` drives the ground A*, so it is not an
    // equivalent — a bee is not a mob that walks somewhere slowly.
    Registration::missing(Selector::Goal, 8, "Bee.BeeWanderGoal"),
    Registration::goal(9, "FloatGoal", float_goal),
    // `Bee.BeeHurtByOtherGoal(this).setAlertOthers()` extends
    // `HurtByTargetGoal`; it overrides only `canContinueToUse`, adding
    // `isAngry()`. `canUse` — the retaliation trigger — is inherited unchanged,
    // so ours is a faithful stand-in for the part that fires.
    Registration::target(1, "Bee.BeeHurtByOtherGoal", hurt_by_target),
    // `Bee.BeeBecomeAngryTargetGoal` — gated on
    // `isAngry() && !hasStung()`; see the module doc's neutrality trap.
    Registration::missing(Selector::Target, 2, "Bee.BeeBecomeAngryTargetGoal"),
    Registration::missing(Selector::Target, 3, "ResetUniversalAngerTargetGoal"),
];

/// `Wolf.registerGoals`.
///
/// Attributes from `Wolf.createAttributes()` — `Animal.createAnimalAttributes()` plus
/// `MOVEMENT_SPEED 0.3F`, `MAX_HEALTH 8.0`, `ATTACK_DAMAGE 4.0`;
/// `Wolf.applyTamingSideEffects` raises `MAX_HEALTH` to `40.0` on taming
/// and drops it back to `8.0`. `FOLLOW_RANGE` is the `Mob.createMobAttributes`
/// default `16.0`, which sizes the pack-alert box.
///
/// **Eight of the twenty rows needed an owner**, and until `lodestone_server`
/// grew one (`PlayerIdentity` at the perception seam, plus `MobController::
/// owner_position`/`is_tame`/`is_ordered_to_sit`) all eight were `Missing` for
/// that single reason. `SitWhenOrderedToGoal` and `FollowOwnerGoal` are now
/// built on it and appear below as real rows — the ownership half of the gap
/// is closed for those two.
///
/// The other six are still `Missing`, but **not for the reason above anymore**,
/// and re-reading this comment instead of the table is exactly the kind of
/// staleness this repo's evidence standards warn about:
///
/// * `OwnerHurtByTargetGoal`/`OwnerHurtTargetGoal` read `owner.getLastHurtByMob()`
///   and `owner.getLastHurtMob()` (`ai/goal/target/OwnerHurtByTargetGoal.java`,
///   `OwnerHurtTargetGoal.java`) — **who last hurt the owner** and **who the
///   owner last hurt**. Ownership can name the owner now; nothing produces
///   either fact. The player half needs the attacker's `PlayerIdentity` threaded
///   through `MobSim::attack`'s caller (`crate::server::apply_attack`, which
///   today passes only a `Vec3` for knockback), and the "a mob hurt the owner"
///   half has no producer at all — player damage is not resolved through
///   `MobSim`. Both rows stay `Missing` at their own table entries below, with
///   this reasoning repeated there rather than assumed from this paragraph.
/// * `BegGoal` and `Wolf.WolfAvoidEntityGoal(Llama)` both gate on `isTame()`
///   (`ai/goal/BegGoal.java`, `Wolf.WolfAvoidEntityGoal.canUse`), which is answerable
///   today — but neither has a goal *type* in this crate yet (begging for food,
///   and fleeing a llama with a strength-gated roll), so they are `Missing` for
///   an ordinary "we have no such goal" reason now, not an ownership one.
/// * Both `NonTameRandomTargetGoal`s gate on `!isTame()` and otherwise pick a
///   random same-class target — also answerable, also no goal type here yet.
///
/// Pack aggro is `HurtByTargetGoal(this).setAlertOthers()` at target priority 3,
/// and the ownership gap reaches into it too:
/// `HurtByTargetGoal.alertOthers` filters same-class neighbours in a
/// `(16.0, 10.0, 16.0)` box, and for a `TamableAnimal` it additionally requires
/// **`tamable.getOwner() == other.getOwner()`**
/// (in `HurtByTargetGoal.alertOthers`) — a wolf pack only rallies for
/// wolves sharing its owner. So a correct wolf pack alert needs the owner model
/// even though the alert itself is not about taming. Retaliation, the half that
/// does not, is modelled.
pub const WOLF: &[Registration] = &[
    Registration::goal(1, "FloatGoal", float_goal),
    // `TamableAnimal.TamableAnimalPanicGoal(1.5, DamageTypeTags.PANIC_ENVIRONMENTAL_CAUSES)`
    // extends `PanicGoal`, narrowing it to environmental damage types. Our
    // `PanicGoal` reads `is_panicking()`, which `MobSim` sets from *any* damage
    // (`SimMob::apply_damage` → `note_hurt`), so ours panics on a strict superset
    // of vanilla's causes. A disclosed over-eagerness, not a missing goal — and
    // the priority is what matters here, see the gates below.
    Registration::goal(1, "TamableAnimal.TamableAnimalPanicGoal", panic_1_5),
    Registration::goal(2, "SitWhenOrderedToGoal", sit_when_ordered),
    // `Wolf.WolfAvoidEntityGoal<>(this, Llama.class, 24.0F, 1.5, 1.5)`.
    // Not merely an `AvoidEntityGoal` with a 24-block radius: its
    // `canUse` requires `!wolf.isTame()` (answerable now through
    // `MobController::is_tame`) and rolls against the llama's strength
    // in `Wolf.WolfAvoidEntityGoal.avoidLlama`. The remaining gap is a goal
    // *type*: no llama-strength roll exists in this crate, not the tame flag.
    Registration::missing(Selector::Goal, 3, "Wolf.WolfAvoidEntityGoal(Llama)"),
    Registration::missing(Selector::Goal, 4, "LeapAtTargetGoal"),
    // `MeleeAttackGoal(this, 1.0, true)`.
    Registration::goal(5, "MeleeAttackGoal", melee_attack),
    // `FollowOwnerGoal(this, 1.0, 10.0F, 2.0F)` (`animal/wolf/Wolf.java`). The
    // two distances are the wolf's own — a cat's are `(10, 5)` and a parrot's
    // `(5, 1)`, so they are constructor arguments rather than constants.
    Registration::goal(6, "FollowOwnerGoal", follow_owner_10_2),
    Registration::goal(7, "BreedGoal", breed_1_0),
    Registration::goal(8, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::missing(Selector::Goal, 9, "BegGoal"),
    Registration::goal(10, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(10, "RandomLookAroundGoal", random_look_around),
    // `OwnerHurtByTargetGoal` (`ai/goal/target/OwnerHurtByTargetGoal.java`):
    // targets whoever `owner.getLastHurtByMob()` names. Ownership can resolve
    // the owner now; nothing produces "who last hurt the owner" — a player's
    // incoming damage is not resolved through `MobSim` at all, so there is no
    // call site here to read it from. See this table's own doc for the fuller
    // account.
    Registration::missing(Selector::Target, 1, "OwnerHurtByTargetGoal"),
    // `OwnerHurtTargetGoal` (`ai/goal/target/OwnerHurtTargetGoal.java`): targets
    // whoever `owner.getLastHurtMob()` names, i.e. joins a fight the owner
    // started. `MobSim::attack` — this crate's own melee-resolution entry point
    // — already takes the attacker's `Vec3` for knockback, but not their
    // `PlayerIdentity`, so a wolf here cannot tell "the owner just hit this"
    // from "some other player did". Threading identity through needs a change
    // at `crate::server::apply_attack`'s call site, which this unit does not
    // own; see this table's own doc for the fuller account.
    Registration::missing(Selector::Target, 2, "OwnerHurtTargetGoal"),
    // `.setAlertOthers()` — the pack half is `Missing`; see this table's doc.
    Registration::target(3, "HurtByTargetGoal", hurt_by_target),
    Registration::missing(
        Selector::Target,
        4,
        "NearestAttackableTargetGoal(Player,isAngryAt)",
    ),
    Registration::missing(Selector::Target, 5, "NonTameRandomTargetGoal(Animal)"),
    Registration::missing(Selector::Target, 6, "NonTameRandomTargetGoal(Turtle)"),
    Registration::missing(
        Selector::Target,
        7,
        "NearestAttackableTargetGoal(AbstractSkeleton)",
    ),
    Registration::missing(Selector::Target, 8, "ResetUniversalAngerTargetGoal"),
];

// -- local builders ----------------------------------------------------------
//
// The factors live here rather than in `super` because no other family
// registers these three goals at these multipliers.

/// `EnderMan.EndermanFreezeWhenLookedAt(this)` takes no constructor arguments.
fn freeze_when_looked_at(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(EndermanFreezeWhenLookedAt::new())
}

/// `EnderMan.EndermanLookForPlayerGoal(this, this::isAngryAt)` takes no
/// per-species argument this port carries — see the goal's own doc comment.
fn look_for_player(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(EndermanLookForPlayerGoal::new())
}

/// `TamableAnimal.TamableAnimalPanicGoal(1.5, …)` — the wolf's panic speed
/// factor, from `Wolf.registerGoals`. Vanilla's own `PanicGoal` speed argument
/// is a `MOVEMENT_SPEED` multiplier.
fn panic_1_5(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.5))
}

/// `TemptGoal(this, 1.25, BEE_FOOD, false)`, from `Bee.registerGoals`.
fn tempt_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.25))
}

/// `FollowOwnerGoal(this, 1.0, 10.0F, 2.0F)` — the wolf's follow distances.
fn follow_owner_10_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowOwnerGoal::new(ctx.speed, 10.0, 2.0))
}

/// `FollowParentGoal(this, 1.25)`, from `Bee.registerGoals`.
fn follow_parent_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowParentGoal::new(ctx.speed * 1.25))
}

#[cfg(test)]
mod tests {
    use lodestone_model::Vec3;

    use super::super::probe::SpeedProbe;
    use super::super::{Coverage, goals_for, is_fallback, registrations_for};
    use super::*;
    use crate::ai::goal::GoalSelector;
    use crate::ai::navigating_mob::NavigatingMob;
    use crate::pathfinding::{Aabb, MobShape, PathType, PathWorld};

    /// `MOVEMENT_SPEED` for each species, from the jar. These are the values
    /// `SpeciesContext` *will* carry once `attribute.rs`'s `type_spec` gains arms
    /// for these four species; it has none today, so production currently hands
    /// them the generic registry default instead. That gap is real and is
    /// reported with this unit — it does not change what the multiplier below
    /// must be, which is what these gates measure.
    const ENDERMAN_SPEED: f64 = 0.3; // `EnderMan.createAttributes`
    const PIGLIN_SPEED: f64 = 0.23; // `ZombifiedPiglin.createAttributes`
    const BEE_SPEED: f64 = 0.3; // `Bee.createAttributes`
    const WOLF_SPEED: f64 = 0.3; // `Wolf.createAttributes`

    // -- table transcription --------------------------------------------------

    /// Every row of all four tables, against the cited `addGoal` block, in jar
    /// order, including the rows this repo does not implement.
    ///
    /// `super::super::tests::every_table_matches_the_jars_addgoal_block` covers
    /// the hostile and passive families; this is the same gate for this one. The
    /// expectations were transcribed from the jar, not from the tables above —
    /// copying them from `WOLF` would be satisfied by any table, right or wrong.
    /// Re-read the citation before changing either side.
    #[test]
    fn every_table_matches_the_jars_addgoal_block() {
        type Row = (Selector, i32, &'static str);
        let cases: &[(&str, &str, &[Row])] = &[
            (
                "enderman",
                "monster/EnderMan.java:93-106",
                &[
                    (Selector::Goal, 0, "FloatGoal"),
                    (Selector::Goal, 1, "EnderMan.EndermanFreezeWhenLookedAt"),
                    (Selector::Goal, 2, "MeleeAttackGoal"),
                    (Selector::Goal, 7, "WaterAvoidingRandomStrollGoal"),
                    (Selector::Goal, 8, "LookAtPlayerGoal(Player)"),
                    (Selector::Goal, 8, "RandomLookAroundGoal"),
                    (Selector::Goal, 10, "EnderMan.EndermanLeaveBlockGoal"),
                    (Selector::Goal, 11, "EnderMan.EndermanTakeBlockGoal"),
                    (Selector::Target, 1, "EnderMan.EndermanLookForPlayerGoal"),
                    (Selector::Target, 2, "HurtByTargetGoal"),
                    (Selector::Target, 3, "NearestAttackableTargetGoal(Endermite)"),
                    (Selector::Target, 4, "ResetUniversalAngerTargetGoal"),
                ],
            ),
            (
                "zombified_piglin",
                "monster/zombie/Zombie.java:113-115 (inherited registerGoals) + \
                 monster/zombie/ZombifiedPiglin.java:72-77 (addBehaviourGoals)",
                &[
                    (Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
                    (Selector::Goal, 8, "LookAtPlayerGoal(Player)"),
                    (Selector::Goal, 8, "RandomLookAroundGoal"),
                    (Selector::Goal, 1, "SpearUseGoal"),
                    (Selector::Goal, 2, "ZombieAttackGoal"),
                    (Selector::Goal, 7, "WaterAvoidingRandomStrollGoal"),
                    (Selector::Target, 1, "HurtByTargetGoal"),
                    (
                        Selector::Target,
                        2,
                        "NearestAttackableTargetGoal(Player,isAngryAt)",
                    ),
                    (Selector::Target, 3, "ResetUniversalAngerTargetGoal"),
                ],
            ),
            (
                "bee",
                "animal/bee/Bee.java:174-195",
                &[
                    (Selector::Goal, 0, "Bee.BeeAttackGoal"),
                    (Selector::Goal, 1, "Bee.BeeEnterHiveGoal"),
                    (Selector::Goal, 2, "BreedGoal"),
                    (Selector::Goal, 3, "TemptGoal(BEE_FOOD)"),
                    (Selector::Goal, 3, "Bee.ValidateHiveGoal"),
                    (Selector::Goal, 3, "Bee.ValidateFlowerGoal"),
                    (Selector::Goal, 4, "Bee.BeePollinateGoal"),
                    (Selector::Goal, 5, "FollowParentGoal"),
                    (Selector::Goal, 5, "Bee.BeeLocateHiveGoal"),
                    (Selector::Goal, 5, "Bee.BeeGoToHiveGoal"),
                    (Selector::Goal, 6, "Bee.BeeGoToKnownFlowerGoal"),
                    (Selector::Goal, 7, "Bee.BeeGrowCropGoal"),
                    (Selector::Goal, 8, "Bee.BeeWanderGoal"),
                    (Selector::Goal, 9, "FloatGoal"),
                    (Selector::Target, 1, "Bee.BeeHurtByOtherGoal"),
                    (Selector::Target, 2, "Bee.BeeBecomeAngryTargetGoal"),
                    (Selector::Target, 3, "ResetUniversalAngerTargetGoal"),
                ],
            ),
            (
                "wolf",
                "animal/wolf/Wolf.java:128-149",
                &[
                    (Selector::Goal, 1, "FloatGoal"),
                    (Selector::Goal, 1, "TamableAnimal.TamableAnimalPanicGoal"),
                    (Selector::Goal, 2, "SitWhenOrderedToGoal"),
                    (Selector::Goal, 3, "Wolf.WolfAvoidEntityGoal(Llama)"),
                    (Selector::Goal, 4, "LeapAtTargetGoal"),
                    (Selector::Goal, 5, "MeleeAttackGoal"),
                    (Selector::Goal, 6, "FollowOwnerGoal"),
                    (Selector::Goal, 7, "BreedGoal"),
                    (Selector::Goal, 8, "WaterAvoidingRandomStrollGoal"),
                    (Selector::Goal, 9, "BegGoal"),
                    (Selector::Goal, 10, "LookAtPlayerGoal(Player)"),
                    (Selector::Goal, 10, "RandomLookAroundGoal"),
                    (Selector::Target, 1, "OwnerHurtByTargetGoal"),
                    (Selector::Target, 2, "OwnerHurtTargetGoal"),
                    (Selector::Target, 3, "HurtByTargetGoal"),
                    (
                        Selector::Target,
                        4,
                        "NearestAttackableTargetGoal(Player,isAngryAt)",
                    ),
                    (Selector::Target, 5, "NonTameRandomTargetGoal(Animal)"),
                    (Selector::Target, 6, "NonTameRandomTargetGoal(Turtle)"),
                    (
                        Selector::Target,
                        7,
                        "NearestAttackableTargetGoal(AbstractSkeleton)",
                    ),
                    (Selector::Target, 8, "ResetUniversalAngerTargetGoal"),
                ],
            ),
        ];

        let mut rows_checked = 0usize;
        for &(species, cite, want) in cases {
            let table = registrations_for(species);
            let got: Vec<Row> = table
                .iter()
                .map(|r| (r.selector, r.priority, r.vanilla))
                .collect();
            assert_eq!(
                got,
                want.to_vec(),
                "{species}'s table does not match {cite} — re-read the jar before \
                 editing either side of this"
            );
            rows_checked += got.len();
        }
        // A control on the gate itself: if `cases` were emptied or a table went
        // to zero rows, every assertion above would pass vacuously.
        assert_eq!(
            rows_checked, 58,
            "expected 12 enderman + 9 zombified piglin + 17 bee + 20 wolf rows"
        );
    }

    /// The piglin's inheritance is a claim about the jar, so it is asserted as
    /// one: it must **not** share `zombie`'s table (it overrides
    /// `addBehaviourGoals`), it must carry Zombie's three `registerGoals` rows,
    /// and it must **not** carry the `MoveThroughVillageGoal` its parent's
    /// `addBehaviourGoals` registers at priority 6.
    ///
    /// This is the assertion that would have caught transcribing only the method
    /// named after the species.
    #[test]
    fn the_piglin_inherits_three_rows_and_drops_one_from_its_parent() {
        let zombie = registrations_for("zombie");
        let piglin = registrations_for("zombified_piglin");
        assert!(
            !std::ptr::eq(zombie.as_ptr(), piglin.as_ptr()),
            "a zombified piglin overrides addBehaviourGoals \
             (ZombifiedPiglin.java:71-78), so it must not share Zombie's table"
        );

        let names = |t: &[Registration]| -> Vec<&'static str> {
            t.iter().map(|r| r.vanilla).collect()
        };
        let piglin_names = names(piglin);

        // Inherited from `Zombie.registerGoals`.
        for inherited in [
            "Zombie.ZombieAttackTurtleEggGoal",
            "LookAtPlayerGoal(Player)",
            "RandomLookAroundGoal",
        ] {
            assert!(
                piglin_names.contains(&inherited),
                "{inherited} is registered by Zombie.registerGoals, which the \
                 piglin does not override, so it must appear in the piglin's table"
            );
        }

        // Dropped: the override does not call `super.addBehaviourGoals()`.
        assert!(
            !piglin_names.contains(&"MoveThroughVillageGoal"),
            "ZombifiedPiglin.addBehaviourGoals does not call super, so Zombie's \
             MoveThroughVillageGoal must NOT appear"
        );
        assert!(
            names(zombie).contains(&"MoveThroughVillageGoal"),
            "precondition: Zombie's own table must carry the row the piglin drops, \
             or the assertion above proves nothing"
        );

        // And the two rows the override renumbers.
        let at = |t: &[Registration], name: &str| -> Option<i32> {
            t.iter().find(|r| r.vanilla == name).map(|r| r.priority)
        };
        assert_eq!(at(piglin, "SpearUseGoal"), Some(1));
        assert_eq!(at(zombie, "SpearUseGoal"), Some(2));
        assert_eq!(at(piglin, "ZombieAttackGoal"), Some(2));
        assert_eq!(at(zombie, "ZombieAttackGoal"), Some(3));
    }

    /// The still-blocked headline mechanisms named in this module's own doc
    /// comment must be present as `Coverage::Missing` rows at their real
    /// vanilla priorities — not absent, and
    /// not silently `Modelled` by a goal that does something else.
    ///
    /// This is the gate that fails if someone "implements" the enderman
    /// teleport by pointing the row at a plain goal, or drops a row because
    /// nothing builds it. The enderman *freeze* half is no longer in this
    /// list — primitive 2 landed it as a real `Coverage::Modelled` row, and
    /// [`the_enderman_freeze_row_is_modelled_and_built_from_the_seam`] is its
    /// own positive gate.
    #[test]
    fn the_four_blocked_mechanisms_are_recorded_as_missing_not_omitted() {
        let blocked: &[(&str, i32, Selector, &str)] = &[
            ("bee", 0, Selector::Goal, "Bee.BeeAttackGoal"),
            ("wolf", 1, Selector::Target, "OwnerHurtByTargetGoal"),
        ];
        for &(species, priority, selector, vanilla) in blocked {
            let row = registrations_for(species)
                .iter()
                .find(|r| r.vanilla == vanilla)
                .unwrap_or_else(|| panic!("{species} has no row for {vanilla}"));
            assert_eq!(row.priority, priority, "{vanilla} is at the wrong priority");
            assert_eq!(row.selector, selector, "{vanilla} is on the wrong selector");
            assert!(
                matches!(row.coverage, Coverage::Missing),
                "{vanilla} claims coverage other than Missing; the primitive it \
                 needs does not exist on MobController, so anything built here is \
                 doing something else"
            );
        }
    }

    /// The enderman's freeze row is a real, working goal built from the seam —
    /// not merely a row whose `coverage` field says `Modelled` while its build
    /// function does something else. Drives the row's own `build()` against a
    /// real [`NavigatingMob`], the production `MobController` (never
    /// `ScriptMob` or [`super::probe`]'s double, which override perception
    /// wholesale), with a discriminating pair: identical position and target,
    /// only `is_being_stared_at` differs.
    #[test]
    fn the_enderman_freeze_row_is_modelled_and_built_from_the_seam() {
        use crate::ai::mob::MobController;

        let row = registrations_for("enderman")
            .iter()
            .find(|r| r.vanilla == "EnderMan.EndermanFreezeWhenLookedAt")
            .expect("enderman has a freeze row");
        assert_eq!(row.selector, Selector::Goal);
        assert_eq!(row.priority, 1);
        assert!(
            matches!(row.coverage, Coverage::Modelled(_)),
            "EndermanFreezeWhenLookedAt is a real goal now; this row must say so"
        );
        let build = row.build().expect("a Modelled row must build something");

        let world = Flat::dry();
        let ctx = SpeciesContext::new(ENDERMAN_SPEED);
        let target = Vec3::new(6.0, 0.0, 0.0); // distSqr 36 <= 256

        let mut watched = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.0, 0.0, 0.0),
            ENDERMAN_SPEED,
            560,
            1,
        );
        watched.set_attack_target(Some(target));
        watched.set_stared_at(true);
        assert!(
            build(&ctx).can_use(&mut watched),
            "the real row's goal must accept a stared-at target within range"
        );

        let mut unwatched = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.0, 0.0, 0.0),
            ENDERMAN_SPEED,
            560,
            1,
        );
        unwatched.set_attack_target(Some(target));
        unwatched.set_stared_at(false);
        assert!(
            !build(&ctx).can_use(&mut unwatched),
            "the identical position and target with is_being_stared_at() false \
             must NOT make the real row's goal eligible — if this fires, the \
             row's build function has degenerated into a distance check"
        );
    }

    /// The freeze goal reaching *behaviour*, not just eligibility: the same
    /// discriminating pair, driven through the whole real roster
    /// (`goals_for("enderman", ..)`, exactly what `MobSim::spawn_species`
    /// installs) and ticked, so this is the production wiring end to end —
    /// `EndermanFreezeWhenLookedAt` (goal priority 1) must actually preempt
    /// `MeleeAttackGoal` (priority 2) on their shared MOVE flag, the same
    /// ordinary `GoalSelector` preemption the wolf panic-vs-melee test above
    /// already relies on.
    ///
    /// If this test only varied the mobs' *position* it could not distinguish
    /// a real gaze test from a plain distance check — the wrong
    /// implementation someone could plausibly ship instead — so both mobs
    /// start at the identical position and are given the identical target;
    /// `is_being_stared_at` is the only input that differs.
    #[test]
    fn a_stared_at_enderman_freezes_while_an_unwatched_one_at_the_same_spot_closes_in() {
        use crate::ai::mob::MobController;

        let world = Flat::dry();
        let ctx = SpeciesContext::new(ENDERMAN_SPEED);
        let start = Vec3::new(0.5, 0.0, 0.5);
        // 6 blocks: inside the freeze goal's 16-block range, well outside
        // MeleeAttackGoal's reach, so 60 ticks of unobstructed closing is
        // visible in the ending position.
        let target = Vec3::new(6.5, 0.0, 0.5);

        let run = |stared_at: bool| {
            let mut mob = NavigatingMob::new(
                &world,
                MobShape::land(0.6, 1.95),
                start,
                ENDERMAN_SPEED,
                560,
                0xB4_2333,
            );
            let mut ai = GoalSelector::new();
            for (priority, goal) in goals_for("enderman", &ctx) {
                ai.add(priority, goal);
            }
            mob.set_attack_target(Some(target));
            mob.set_stared_at(stared_at);
            for _ in 0..60 {
                mob.tick(&mut ai);
            }
            mob.position()
        };

        let frozen = run(true);
        let closing = run(false);

        assert_eq!(
            frozen, start,
            "a stared-at enderman must not move from its start position: \
             EndermanFreezeWhenLookedAt (goal priority 1) should hold MOVE and \
             call stop_navigation, preempting MeleeAttackGoal (priority 2). \
             Ended at {frozen:?} instead of {start:?}"
        );
        let gap_closed = ((start.x - closing.x).powi(2) + (start.z - closing.z).powi(2)).sqrt();
        assert!(
            gap_closed > 1.0,
            "with the identical start position and target but \
             is_being_stared_at() false, MeleeAttackGoal should win MOVE and \
             close the gap; the enderman only moved {gap_closed} blocks in 60 \
             ticks (ended at {closing:?})"
        );
    }

    /// The look-for-player row is a real, working goal built from the seam —
    /// the same shape as the freeze row's own gate above, with the same
    /// discriminating pair: identical candidate, only `is_being_stared_at`
    /// differs (a live grudge is the other way in, not exercised here).
    #[test]
    fn the_enderman_look_for_player_row_is_modelled_and_built_from_the_seam() {
        let row = registrations_for("enderman")
            .iter()
            .find(|r| r.vanilla == "EnderMan.EndermanLookForPlayerGoal")
            .expect("enderman has a look-for-player row");
        assert_eq!(row.selector, Selector::Target);
        assert_eq!(row.priority, 1);
        assert!(
            matches!(row.coverage, Coverage::Modelled(_)),
            "EndermanLookForPlayerGoal is a real goal now; this row must say so"
        );
        let build = row.build().expect("a Modelled row must build something");

        let world = Flat::dry();
        let ctx = SpeciesContext::new(ENDERMAN_SPEED);

        let mut watched = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.0, 0.0, 0.0),
            ENDERMAN_SPEED,
            560,
            1,
        );
        watched.set_nearest_player(Some(Vec3::new(5.0, 0.0, 0.0)));
        watched.set_stared_at(true);
        assert!(
            build(&ctx).can_use(&mut watched),
            "a stared-at nearby player must make the real row's goal eligible"
        );

        let mut unwatched = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.0, 0.0, 0.0),
            ENDERMAN_SPEED,
            560,
            1,
        );
        unwatched.set_nearest_player(Some(Vec3::new(5.0, 0.0, 0.0)));
        unwatched.set_stared_at(false);
        assert!(
            !build(&ctx).can_use(&mut unwatched),
            "the identical nearby player with is_being_stared_at() false and no \
             live grudge must NOT make the real row's goal eligible — if this \
             fires, the row's build function has degenerated into a proximity \
             check"
        );
    }

    /// Predicts the exact tick the stare turns into an attack target — vanilla's
    /// `aggroTime = adjustedTickDelay(5)`, counted down once per `tick()` — not
    /// merely "eventually acquires one". Driven directly against the real
    /// [`EndermanLookForPlayerGoal`] (not through a `GoalSelector`, so nothing
    /// else perturbs `attack_target` or moves the mob), matching this module's
    /// own `the_enderman_freeze_row_is_modelled_and_built_from_the_seam` style.
    #[test]
    fn an_endermans_stare_provokes_an_attack_after_exactly_the_aggro_delay() {
        use crate::ai::mob::MobController;

        let world = Flat::dry();
        let candidate = Vec3::new(5.0, 0.0, 0.0);
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.0, 0.0, 0.0),
            ENDERMAN_SPEED,
            560,
            2,
        );
        mob.set_nearest_player(Some(candidate));
        mob.set_stared_at(true);

        let mut goal = EndermanLookForPlayerGoal::new();
        assert!(goal.can_use(&mut mob), "the stare must make the goal eligible");
        goal.start(&mut mob);

        for n in 1..=4 {
            goal.tick(&mut mob);
            assert!(
                mob.attack_target().is_none(),
                "aggro_time is 5; tick {n} of 4 must not yet promote a target \
                 (got {:?})",
                mob.attack_target()
            );
        }
        goal.tick(&mut mob);
        assert_eq!(
            mob.attack_target(),
            Some(candidate),
            "the fifth tick must promote the pending candidate to a real target"
        );
    }

    /// Predicts the exact landing point of `EnderMan::teleportTowards`, not
    /// merely "the enderman moved" or "moved closer" — both the *magnitude*
    /// species of vacuous test this repo's evidence standards name explicitly.
    /// `teleport_time` must reach vanilla's `adjustedTickDelay(30)` while the
    /// target stays farther than 16 blocks (`distanceToSqr > 256.0`) and the
    /// enderman is *not* currently stared at (the sibling branch, exercised
    /// separately below); the landing point is 16 blocks from the target along
    /// the target-to-enderman direction, computed independently of the goal's
    /// own implementation from the same inputs.
    #[test]
    fn a_far_untended_target_is_closed_by_a_predicted_teleport_not_a_walk() {
        use crate::ai::mob::MobController;

        let world = Flat::dry();
        let start = Vec3::new(0.0, 0.0, 0.0);
        let far_target = Vec3::new(50.0, 0.0, 0.0); // distSqr 2500 > 256
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            start,
            ENDERMAN_SPEED,
            560,
            3,
        );
        mob.set_follow_range(64.0); // EnderMan.createAttributes' real FOLLOW_RANGE
        mob.set_nearest_player(Some(far_target));
        // Acquisition needs the stare (or a live grudge, exercised elsewhere);
        // once a target is *held*, vanilla's `continueAggroTargetConditions`
        // ignores the stare, which is exactly what the phase below exercises.
        mob.set_stared_at(true);

        let mut goal = EndermanLookForPlayerGoal::new();
        assert!(goal.can_use(&mut mob), "the far candidate must be eligible");
        goal.start(&mut mob);
        for _ in 0..5 {
            goal.tick(&mut mob);
        }
        assert_eq!(
            mob.attack_target(),
            Some(far_target),
            "precondition: the aggro delay must have promoted the far target"
        );

        // The player looks away: the far-teleport branch is gated on *not*
        // being stared at (the sibling blink branch is the stared-at one).
        mob.set_stared_at(false);

        // Vanilla's gate is a Java post-increment, `teleportTime++ >= 30`:
        // the comparison reads the value *before* incrementing, so the first
        // tick that reads 30 is the 31st call (ticks 1..=30 leave
        // teleport_time at 1..=30 without ever comparing a value >= 30 — the
        // comparison on tick 30 sees the pre-increment 29). 30 ticks must
        // therefore not teleport yet.
        for n in 1..=30 {
            goal.tick(&mut mob);
            assert_eq!(
                mob.position(),
                start,
                "tick {n} of 30 must not teleport yet"
            );
        }
        // The 31st tick crosses the gate.
        goal.tick(&mut mob);

        // Independently derived expectation: `EnderMan::teleportTowards`
        // lands 16 blocks from the target along (enderman - target),
        // normalised. Here that direction is exactly -X, so the landing point
        // is target.x - 16.
        let expected = Vec3::new(far_target.x - 16.0, 0.0, 0.0);
        let got = mob.position();
        assert!(
            (got.x - expected.x).abs() < 1.0e-9 && got.y == expected.y && got.z == expected.z,
            "expected the teleport-towards landing point {expected:?}, got {got:?}"
        );
    }

    /// The sibling branch: a target *closer* than 16 blocks while actively
    /// stared at must trigger vanilla's evasive blink
    /// (`EnderMan::teleport`, ±32 blocks XZ and `nextInt(64) - 32` on Y) —
    /// and, as the control, a target beyond 16 blocks while stared at must
    /// hold still (the far branch is gated on *not* being stared at).
    #[test]
    fn a_close_stared_at_target_triggers_the_evasive_blink_and_a_far_one_does_not() {
        use crate::ai::mob::MobController;

        let world = Flat::dry();
        let start = Vec3::new(0.0, 0.0, 0.0);
        let close_target = Vec3::new(2.0, 0.0, 0.0); // distSqr 4 < 16
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            start,
            ENDERMAN_SPEED,
            560,
            4,
        );
        mob.set_follow_range(64.0);
        mob.set_nearest_player(Some(close_target));
        mob.set_stared_at(true);

        let mut goal = EndermanLookForPlayerGoal::new();
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        for _ in 0..5 {
            goal.tick(&mut mob);
        }
        assert_eq!(mob.attack_target(), Some(close_target));

        goal.tick(&mut mob);
        assert_ne!(
            mob.position(),
            start,
            "a close, stared-at target must trigger the evasive blink \
             (EnderMan::teleport), but the position never changed"
        );

        // Control: the same close distance, but not stared at — the blink is
        // gated on `isBeingStaredBy`, so this must hold still (it takes the
        // far/near-walk branch instead, which does not fire under 256 sqr
        // distance either).
        let mut control = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            start,
            ENDERMAN_SPEED,
            560,
            5,
        );
        control.set_follow_range(64.0);
        control.set_nearest_player(Some(close_target));
        // Acquire exactly like the subject (the stare gates acquisition
        // itself), then look away only for the tick under test.
        control.set_stared_at(true);
        let mut control_goal = EndermanLookForPlayerGoal::new();
        assert!(control_goal.can_use(&mut control));
        control_goal.start(&mut control);
        for _ in 0..5 {
            control_goal.tick(&mut control);
        }
        assert_eq!(control.attack_target(), Some(close_target));
        control.set_stared_at(false);
        control_goal.tick(&mut control);
        assert_eq!(
            control.position(),
            start,
            "control: not stared at and within 16 blocks must trigger neither \
             the blink nor the far-teleport branch"
        );
    }

    /// Every player-targeting row on these species carries vanilla's anger
    /// predicate, and ours has none — so none of them may be `Modelled`.
    ///
    /// The module doc explains why this matters more than it looks: the rows are
    /// inert today only because `find_nearest_target` is a self-loop, so a
    /// `Modelled` row here is a latent "neutral mobs become hostile" bug rather
    /// than an active one.
    #[test]
    fn no_anger_gated_target_row_is_modelled() {
        let mut checked = 0usize;
        for species in SPECIES {
            for r in registrations_for(species) {
                if r.vanilla.contains("isAngryAt") || r.vanilla.contains("BecomeAngry") {
                    assert!(
                        matches!(r.coverage, Coverage::Missing),
                        "{species}'s {} is gated on isAngryAt in the jar; \
                         modelling it with our predicate-free \
                         NearestAttackableTargetGoal makes a neutral mob hostile",
                        r.vanilla
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked, 3,
            "expected the zombified piglin, wolf and bee anger-gated target rows"
        );
    }

    /// This family must not accidentally claim a species another family owns, and
    /// must not claim the rosterless controls other gates depend on.
    #[test]
    fn the_family_claims_exactly_four_species_and_leaves_the_controls_alone() {
        assert_eq!(SPECIES.len(), 4);
        for s in SPECIES {
            assert!(
                lookup(s).is_some(),
                "{s} is advertised in SPECIES but lookup returns None"
            );
            assert!(!is_fallback(registrations_for(s)));
        }
        // The rosterless controls in `super::tests` and `mob_roster.rs`.
        for control in ["llama", "panda", "polar_bear"] {
            assert!(
                lookup(control).is_none(),
                "{control} is a rosterless control in other gates; claiming it \
                 here turns those controls green-for-the-wrong-reason or red"
            );
        }
    }

    // -- speed magnitudes -----------------------------------------------------

    /// Drives one row's goal against a [`SpeedProbe`] and returns the speed it
    /// asked the mob to move at.
    fn speed_of(species: &str, vanilla: &str, ctx: &SpeciesContext) -> f64 {
        let row = registrations_for(species)
            .iter()
            .find(|r| r.vanilla == vanilla)
            .unwrap_or_else(|| panic!("{species} has no row named {vanilla}"));
        let build = row
            .build()
            .unwrap_or_else(|| panic!("{species}'s {vanilla} builds nothing"));
        let mut goal = build(ctx);
        let mut probe = SpeedProbe::new();
        assert!(
            goal.can_use(&mut probe),
            "{species}'s {vanilla} refused a fully permissive probe, so no speed \
             was recorded"
        );
        goal.start(&mut probe);
        goal.tick(&mut probe);
        probe.first_speed().unwrap_or_else(|| {
            panic!("{species}'s {vanilla} never called move_to, so its speed is unmeasured")
        })
    }

    /// A priority multiset cannot see a wrong speed: the wolf's panic goal built
    /// with `1.0` instead of the jar's `1.5` sits at the right priority, under the
    /// right class name, and still flees — direction preserved, magnitude wrong.
    ///
    /// So each row below predicts the **value** and is required to land on it,
    /// and the wrong hypothesis (the multiplier dropped, i.e. the bare
    /// `MOVEMENT_SPEED`) is computed too and required *not* to match. Without the
    /// second half, a builder that ignored `ctx` entirely would pass whenever the
    /// factor happened to be 1.0.
    #[test]
    fn transcribed_speed_multipliers_land_on_the_jars_value() {
        // (species, base speed, vanilla row, jar factor)
        let cases: &[(&str, f64, &str, f64)] = &[
            // `Wolf.registerGoals` — the one non-unit factor in this family.
            ("wolf", WOLF_SPEED, "TamableAnimal.TamableAnimalPanicGoal", 1.5),
            // `Bee.registerGoals`'s `TemptGoal` and `FollowParentGoal` rows.
            ("bee", BEE_SPEED, "TemptGoal(BEE_FOOD)", 1.25),
            ("bee", BEE_SPEED, "FollowParentGoal", 1.25),
            // Unit factors, still asserted: a builder that multiplied by the
            // wrong constant would show up here.
            ("wolf", WOLF_SPEED, "MeleeAttackGoal", 1.0),
            ("wolf", WOLF_SPEED, "WaterAvoidingRandomStrollGoal", 1.0),
            ("wolf", WOLF_SPEED, "BreedGoal", 1.0),
            ("enderman", ENDERMAN_SPEED, "MeleeAttackGoal", 1.0),
            ("enderman", ENDERMAN_SPEED, "WaterAvoidingRandomStrollGoal", 1.0),
            ("zombified_piglin", PIGLIN_SPEED, "ZombieAttackGoal", 1.0),
            (
                "zombified_piglin",
                PIGLIN_SPEED,
                "WaterAvoidingRandomStrollGoal",
                1.0,
            ),
        ];

        let mut non_unit_factors = 0usize;
        for &(species, base, vanilla, factor) in cases {
            let ctx = SpeciesContext::new(base);
            let measured = speed_of(species, vanilla, &ctx);
            let correct = base * factor;
            assert!(
                (measured - correct).abs() < 1e-12,
                "{species}'s {vanilla} moves at {measured}; the jar's factor \
                 {factor} on MOVEMENT_SPEED {base} predicts {correct}"
            );
            if (factor - 1.0).abs() > 1e-12 {
                non_unit_factors += 1;
                // The suspected-wrong hypothesis: the multiplier was dropped.
                assert!(
                    (measured - base).abs() > 1e-12,
                    "{species}'s {vanilla} landed on the bare MOVEMENT_SPEED \
                     {base}, which is the hypothesis where the jar's {factor} was \
                     dropped — the two must be distinguishable"
                );
            }
        }
        assert_eq!(
            non_unit_factors, 3,
            "the wolf panic 1.5 and the bee's two 1.25s are the rows that make \
             this gate more than a tautology; if they stop being counted, the \
             wrong-hypothesis half of this test has stopped running"
        );
    }

    // -- behaviour, through a real `NavigatingMob` -----------------------------

    /// Flat ground at `y <= -1`, optionally flooded up to `water_top`.
    ///
    /// `is_water` is deliberately **not** overridden, so the fixture cannot fake
    /// the answer `FloatGoal` depends on — it must fall out of `base_path_type`
    /// the way the real `ChunkWorld` makes it.
    struct Flat {
        water_top: i32,
    }

    impl Flat {
        const fn dry() -> Self {
            Self { water_top: i32::MIN }
        }
        const fn flooded() -> Self {
            Self { water_top: 2 }
        }
    }

    impl PathWorld for Flat {
        fn min_y(&self) -> i32 {
            -64
        }

        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if y <= -1 {
                PathType::Blocked
            } else if y <= self.water_top {
                PathType::Water
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

    /// What a run of the scheduler produced.
    struct Outcome {
        /// Distance from the attacker at the end of each tick.
        gaps: Vec<f64>,
        /// Cumulative melee strikes at the end of each tick, so a phase boundary
        /// can be asserted rather than only the total.
        attacks_by_tick: Vec<usize>,
        /// Whether the mob ever asked to jump (what `FloatGoal` does).
        jumped: bool,
    }

    impl Outcome {
        /// Total strikes over the whole run.
        fn attacks(&self) -> usize {
            self.attacks_by_tick.last().copied().unwrap_or(0)
        }
    }

    /// Builds a mob for `species` from the roster **only** — nothing is added by
    /// hand — hurts it from `attacker`, and ticks the real scheduler.
    ///
    /// The mob is a `NavigatingMob`, the production `MobController`. It is
    /// emphatically not `ScriptMob`, which overrides every perception method —
    /// a substitution that once let a whole roster's goals stay green while
    /// production built nothing.
    fn run_hurt(
        world: &Flat,
        species: &str,
        speed: f64,
        attacker: Vec3,
        ticks: usize,
    ) -> Outcome {
        let mut mob = NavigatingMob::new(
            world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, 0.0, 0.5),
            speed,
            560,
            0xB4_2333,
        );

        let mut ai = GoalSelector::new();
        let ctx = SpeciesContext::new(speed);
        let installed = goals_for(species, &ctx);
        assert!(
            !installed.is_empty(),
            "{species} installed no goals, so this run measures nothing"
        );
        for (priority, goal) in installed {
            ai.add(priority, goal);
        }

        // The one input: something hurt us from `attacker`. `MobSim` does exactly
        // this in its melee resolution and in `MobSim::attack`.
        mob.note_hurt(Some(attacker));

        let mut gaps = Vec::with_capacity(ticks);
        let mut attacks_by_tick = Vec::with_capacity(ticks);
        let mut jumped = false;
        for _ in 0..ticks {
            mob.tick(&mut ai);
            jumped |= mob.is_jumping();
            let p = mob.position();
            gaps.push(((p.x - attacker.x).powi(2) + (p.z - attacker.z).powi(2)).sqrt());
            attacks_by_tick.push(mob.attacks().len());
        }
        Outcome {
            gaps,
            attacks_by_tick,
            jumped,
        }
    }

    /// A hurt wolf must **run away first and fight afterwards**, because its
    /// table carries a panic goal (`Wolf.registerGoals`'s
    /// `TamableAnimal.TamableAnimalPanicGoal` row) alongside its
    /// melee goal (the `MeleeAttackGoal` row) and both claim MOVE.
    ///
    /// This is behavioural: it asserts where the wolf *is* and whether it
    /// *struck*, never a `can_use` return value. The two phases come from cited
    /// constants rather than from watching the output:
    ///
    /// * `is_panicking()` is `damage_ticks > 0`, and `note_hurt` sets it to
    ///   [`crate::ai::navigating_mob::PANIC_DAMAGE_TICKS`] = 40, vanilla's own
    ///   figure via `LivingEntity.getLastDamageSource`, which `PanicGoal.shouldPanic`
    ///   reads. So panic owns MOVE for the first 40
    ///   ticks.
    /// * `HurtByTargetGoal::start` sets the attack target from `last_hurt_by`,
    ///   which persists [`crate::ai::navigating_mob::LAST_HURT_BY_TICKS`] = 100,
    ///   vanilla's own figure via `LivingEntity.baseTick` — long enough to still
    ///   be hunting when panic ends.
    ///
    /// # What this gate does and does not detect — measured, not assumed
    ///
    /// It detects the **presence and identity** of the rows: deleting the wolf's
    /// `lookup` arm so it falls through to `FALLBACK` fails this test, and so does
    /// the `FloatGoal` gate below. Verified by running that mutation.
    ///
    /// It does **not** detect a wrong *priority number* on this pair, and that
    /// was verified too: transcribing the panic row at 6 instead of 1 — below
    /// melee's 5 — leaves this test **green**. The reason is that
    /// `PanicGoal::is_interruptable()` is `false` and
    /// panic precedes melee in table order, so once panic holds MOVE no priority
    /// can dislodge it; the priority number is simply not load-bearing here. A
    /// second mutation (the piglin's melee at 9, below its stroll at 7) is
    /// likewise invisible, because `RandomStrollGoal`'s interval roll means melee
    /// re-takes MOVE the next tick.
    ///
    /// **So the priority guard for this family is
    /// [`every_table_matches_the_jars_addgoal_block`], not this test** — that gate
    /// caught both mutations immediately and named the row. Do not add a comment
    /// here claiming otherwise; it was believed and measured false.
    #[test]
    fn a_hurt_wolf_flees_before_it_fights_because_panic_is_uninterruptable() {
        let world = Flat::dry();
        let attacker = Vec3::new(2.5, 0.0, 0.5);
        let out = run_hurt(&world, "wolf", WOLF_SPEED, attacker, 240);

        let start_gap = 2.0; // |(0.5,0.5) - (2.5,0.5)|
        // Phase 1: while panic holds MOVE the wolf must end up *further* away
        // than it started, and must not have struck.
        let panic_gap = out.gaps[39];
        assert!(
            panic_gap > start_gap + 1.0,
            "a wolf hurt from {start_gap} blocks away should flee while panicking; \
             after 40 ticks it is {panic_gap} blocks away. If this is ~0, melee \
             took MOVE and PanicGoal's priority 1 is not being honoured"
        );
        assert_eq!(
            out.attacks_by_tick[39], 0,
            "the wolf struck while still panicking; PanicGoal at priority 1 is \
             uninterruptable and owns MOVE, so MeleeAttackGoal at 5 must not have \
             been able to close to reach yet"
        );

        // Phase 2: once panic expires, retaliation drives it back into reach.
        assert!(
            out.attacks() > 0,
            "after panic expires the wolf must return and strike: \
             HurtByTargetGoal (target 3) set the attack target and MeleeAttackGoal \
             (goal 5) should then own MOVE. It never struck in 240 ticks"
        );
        let final_gap = *out.gaps.last().expect("240 ticks recorded");
        assert!(
            final_gap < panic_gap,
            "the wolf ended {final_gap} blocks out having fled to {panic_gap}; it \
             should have closed again once melee took MOVE"
        );
    }

    /// The same scenario for a zombified piglin, whose table has **no** panic
    /// goal — `Zombie.registerGoals`/`addBehaviourGoals` register none — so it
    /// must close immediately instead of fleeing.
    ///
    /// This is the species-separating half. Both mobs run the same harness with
    /// the same single input, and the only thing that differs is the table the
    /// roster handed them, so a difference in outcome can only come from the
    /// roster.
    #[test]
    fn a_hurt_zombified_piglin_fights_at_once_because_its_table_has_no_panic_goal() {
        // Precondition: the claim about the jar this test rests on.
        assert!(
            !registrations_for("zombified_piglin")
                .iter()
                .any(|r| r.vanilla.contains("PanicGoal")),
            "precondition: a zombified piglin registers no panic goal"
        );
        assert!(
            registrations_for("wolf")
                .iter()
                .any(|r| r.vanilla.contains("PanicGoal")),
            "precondition: a wolf does, or the contrast below is vacuous"
        );

        let world = Flat::dry();
        let attacker = Vec3::new(2.5, 0.0, 0.5);
        let out = run_hurt(&world, "zombified_piglin", PIGLIN_SPEED, attacker, 240);

        let start_gap = 2.0;
        assert!(
            out.gaps[39] <= start_gap,
            "a zombified piglin has no panic goal, so 40 ticks after being hurt it \
             should be no further away than it started; it is at {}",
            out.gaps[39]
        );
        assert!(
            out.attacks() > 0,
            "a hurt zombified piglin must retaliate: HurtByTargetGoal at target 1 \
             feeds ZombieAttackGoal at goal 2"
        );
    }

    /// `FloatGoal` is a real per-species difference, not boilerplate: the
    /// enderman, bee and wolf register one (in `EnderMan.registerGoals`,
    /// `Bee.registerGoals` and
    /// `Wolf.registerGoals`) and the zombified piglin does not, because it inherits
    /// `Zombie`'s table and zombies sink and walk along the bottom.
    ///
    /// Asserted as behaviour — does the mob try to jump in water — with the
    /// piglin as the negative control in the same water.
    #[test]
    fn only_the_species_that_register_floatgoal_swim() {
        let flooded = Flat::flooded();
        for (species, speed) in [
            ("enderman", ENDERMAN_SPEED),
            ("bee", BEE_SPEED),
            ("wolf", WOLF_SPEED),
        ] {
            let out = run_hurt(&flooded, species, speed, Vec3::new(6.5, 0.0, 0.5), 40);
            assert!(
                out.jumped,
                "{species} registers FloatGoal, so a real NavigatingMob standing \
                 in water must ask to jump"
            );
        }

        let out = run_hurt(
            &flooded,
            "zombified_piglin",
            PIGLIN_SPEED,
            Vec3::new(6.5, 0.0, 0.5),
            40,
        );
        assert!(
            !out.jumped,
            "a zombified piglin inherits Zombie's table, which registers no \
             FloatGoal, so it must not swim. If this fires, the piglin table has \
             picked up a FloatGoal it should not have"
        );

        // And the control that proves the water is what did it.
        let dry = Flat::dry();
        let out = run_hurt(&dry, "wolf", WOLF_SPEED, Vec3::new(6.5, 0.0, 0.5), 40);
        assert!(
            !out.jumped,
            "a wolf on dry land must not jump; if it does, `jumped` is measuring \
             something other than FloatGoal"
        );
    }
}
