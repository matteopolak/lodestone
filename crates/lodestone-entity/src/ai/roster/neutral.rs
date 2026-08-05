//! Goal sets for the neutral mobs that hold a grudge: enderman, zombified
//! piglin, bee, wolf.
//!
//! # What it is
//!
//! One [`Registration`] table per species, transcribed from that species'
//! `registerGoals()` in `.cache/mc/26.2/src/net/minecraft/world/entity/`. Owned
//! by issue [#233].
//!
//! **Read the next section before adding behaviour here.** The four mechanisms
//! #233 names in its title — enderman teleport-on-stare, zombified-piglin group
//! aggro, bee sting-then-die, wolf pack aggro — are **not** in these tables as
//! [`Coverage::Modelled`] rows, and that is a finding rather than an omission.
//! Each of the four waits on a primitive that did not exist on
//! [`MobController`](super::MobController) when the tables were written; all
//! five have since landed (issue #458), but a primitive is necessary, not
//! sufficient — each mechanism still needs its goal and its host census — so
//! each is a [`Coverage::Missing`] row naming what it still waits on.
//!
//! # Why the four headline mechanisms are `Missing`
//!
//! A roster entry can only be as good as the goals it can build, and a goal can
//! only ask [`MobController`](super::MobController) questions it has methods
//! for. Measured against the trait, **five primitives were absent** and between
//! them accounted for every one of the four. All five have landed (issue #458);
//! the table is kept as the record of what each needed, with the landed state
//! marked:
//!
//! | absent primitive | blocks | landed as |
//! |---|---|---|
//! | the anger timer + anger target (a *grudge*) | all four | host-side `SimMob::anger` + `MobController::angry_target` (the deadline stays on the host) |
//! | "is that player looking at me" (a gaze test) | enderman freeze + stare | `MobController::is_being_stared_at` + free `is_in_view_cone` geometry; the **feed is blocked** — `PlayerPerception` carries no view vector |
//! | relocating a mob instantly (a teleport) | enderman teleport | `MobController::teleport_to`, host-commanded via `SimMob::teleport_to` |
//! | a mob damaging **itself** | bee sting-then-die | `MobController::damage_self`, drained by `MobSim::tick` through the damage pipeline |
//! | an owner relationship | wolf tame half | `MobController::owner_position` + host-side `SimMob::owner_id`; the **player** half is blocked — `PlayerPerception` carries no player identity to own *by* |
//!
//! Group propagation needs a sixth thing that is not a trait method at all: a
//! same-species census, which the seam deliberately does not expose (a goal sees
//! `Option<Vec3>` answers, never a population). `MobSim::feed_perception` is
//! where that resolution happens for every other question, and it is where
//! `alertOthers` belongs.
//!
//! So the honest state of #233 is: **the tables are complete and cited; the
//! mechanisms are blocked on the seam, in five specific, named places — all of
//! which now have a landed primitive (issue #458).** A primitive is not a
//! mechanism: each of the four still needs its goal (the enderman's
//! `EndermanFreezeWhenLookedAt`/`EndermanLookForPlayerGoal`, a bee sting hook,
//! a tame interaction) and its census (`alertOthers`) before it can be a
//! [`Coverage::Modelled`] row. Writing one of those *here* before its goal
//! exists would produce a type with no possible consumer — the island this
//! repo's dominant defect class is named after — so this module does not
//! contain one.
//!
//! # The trap that decided the target rows
//!
//! Three of these species register a player-targeting goal whose last argument is
//! an **anger predicate**: `NearestAttackableTargetGoal<>(this, Player.class, 10,
//! true, false, this::isAngryAt)` (`monster/zombie/ZombifiedPiglin.java:76`,
//! `animal/wolf/Wolf.java:144`, and via `Bee.BeeBecomeAngryTargetGoal`
//! `animal/bee/Bee.java:193`, whose `beeCanTarget` is `isAngry() && !hasStung()`,
//! `:735-738`).
//!
//! **That predicate is the entire difference between a neutral mob and a hostile
//! one.** Our [`NearestAttackableTargetGoal`](super::nearest_attackable_target)
//! takes no predicate, so registering it for these species would make a
//! zombified piglin, a wolf and a bee attack players **on sight** — strictly
//! worse than the mob doing nothing, and worse than vanilla in a way a priority
//! gate cannot see. Every such row is therefore [`Coverage::Missing`].
//!
//! This is currently latent rather than active, which is exactly why it is
//! written down: `NavigatingMob::find_nearest_target` returns `self.attack_target`
//! (`ai/navigating_mob.rs:904-906`) instead of searching, so *no*
//! `NearestAttackableTargetGoal` fires in production today. The day that
//! self-loop is fixed — the obvious next repair — a `Modelled` row here would
//! silently turn three neutral species hostile. Do not "upgrade" these rows
//! without an anger predicate to gate them.
//!
//! # What does still reach behaviour
//!
//! Retaliation does, and it is not a consolation prize: `HurtByTargetGoal::start`
//! calls `set_attack_target` (`ai/goals.rs:564-566`), which is what
//! `MeleeAttackGoal::can_use` reads (`:257-260`), and `last_hurt_by` is really fed
//! by `MobSim` (issue #441). So **hurt → retaliate → close → strike** is a live
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
//!   `addBehaviourGoals` (`:71-78`), the hook `Zombie.registerGoals` calls at
//!   `Zombie.java:116`. Its table is therefore Zombie's own three rows **plus**
//!   its six, and because its override does not call `super`, Zombie's
//!   `MoveThroughVillageGoal` is *dropped* and `SpearUseGoal` moves from priority
//!   2 to 1. Transcribing only the method that carries the species' name would
//!   miss three rows and mis-number two.
//! * **`adjustedTickDelay` halves every delay it wraps.**
//!   `Goal.adjustedTickDelay` is `requiresUpdateEveryTick() ? ticks :
//!   reducedTickDelay(ticks)` (`ai/goal/Goal.java:49-51`) and `reducedTickDelay`
//!   is `Mth.positiveCeilDiv(ticks, 2)` (`:53-55`). Neither
//!   `NearestAttackableTargetGoal`, `TargetGoal` nor
//!   `EnderMan.EndermanLookForPlayerGoal` overrides `requiresUpdateEveryTick`, so
//!   the enderman's aggro delay is **3** ticks, not the literal 5, and its
//!   teleport-towards delay is **15**, not 30. Any future enderman work that
//!   transcribes the literal will be off by a factor of two.
//! * **The grudge is an absolute timestamp in 26.2, not a countdown.**
//!   `NeutralMob.setTimeToRemainAngry(remaining)` stores
//!   `level.getGameTime() + remaining` (`NeutralMob.java:20-22`) and `isAngry()`
//!   compares against the clock (`:112-120`). A countdown field is the pre-26.2
//!   model; porting it would drift against a paused or stepped tick loop.
//! * **The alert box is a box, not a sphere, and its vertical extent is a flat
//!   10.** `HurtByTargetGoal.alertOthers` inflates by
//!   `(followRange, 10.0, followRange)` (`ai/goal/target/HurtByTargetGoal.java:74`,
//!   `ALERT_RANGE_Y = 10` `:20`), and the piglin's own `alertOthers` uses the same
//!   shape (`ZombifiedPiglin.java:141`). Collapsing it to one radius is wrong in
//!   the corners in both directions.
//!
//! [#233]: https://github.com/matteopolak/lodestone/issues/233

use crate::ai::goal::Goal;
use crate::ai::goals::{FollowParentGoal, PanicGoal, TemptGoal};

use super::{
    Registration, Selector, SpeciesContext, breed_1_0, float_goal, hurt_by_target,
    look_at_player_8, melee_attack, random_look_around, stroll,
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

/// `monster/EnderMan.java:93-106`.
///
/// Attributes `:113-118` — `MAX_HEALTH 40.0`, `MOVEMENT_SPEED 0.3F`,
/// `ATTACK_DAMAGE 7.0`, `FOLLOW_RANGE 64.0`.
///
/// The stare is two goals, not one, and both are `Missing`:
/// `EndermanFreezeWhenLookedAt` at goal priority 1 stops the navigation while a
/// player within 16 blocks (`distanceToSqr <= 256.0`, `:414-415`) is staring, and
/// `EndermanLookForPlayerGoal` at target priority 1 does the teleporting. Both
/// route through `isBeingStaredBy` (`:209-211`), which is
/// `PLAYER_NOT_WEARING_DISGUISE_ITEM` (`LivingEntity.java:212-215` — a carved
/// pumpkin, via `ItemTags.GAZE_DISGUISE_EQUIPMENT`, defeats the stare) **and**
/// `isLookingAtMe(player, 0.025, true, false, getEyeY())`.
///
/// That gaze test has a real geometric definition worth citing rather than
/// approximating (`LivingEntity.java:1756-1775`): with the player's normalised
/// view vector `look` and the normalised offset `dir` from player eyes to the
/// enderman, a stare is `look.dot(dir) > 1.0 - coneSize / dist` plus line of
/// sight. `coneSize` is `0.025` and `adjustForDistance` is `true`, so the
/// tolerance is **divided by distance** — the acceptance cone widens the further
/// away the player is, which is the opposite of the fixed-angle cone an
/// approximation reaches for first.
pub const ENDERMAN: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    // `EnderMan.EndermanFreezeWhenLookedAt` (`:401-430`), flags `{JUMP, MOVE}`.
    // Needs the gaze test above; no raycast or view-vector primitive exists on
    // the seam.
    Registration::missing(Selector::Goal, 1, "EnderMan.EndermanFreezeWhenLookedAt"),
    Registration::goal(2, "MeleeAttackGoal", melee_attack),
    // `WaterAvoidingRandomStrollGoal(this, 1.0, 0.0F)`. The third argument is
    // vanilla's probability of preferring a dry destination; ours has no such
    // parameter (see `super::stroll`), so only the 1.0 speed factor transcribes.
    Registration::goal(7, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // Carrying and placing blocks — `DATA_CARRY_STATE` (`:343-349`) plus a
    // block-placement path the AI seam has no access to.
    Registration::missing(Selector::Goal, 10, "EnderMan.EndermanLeaveBlockGoal"),
    Registration::missing(Selector::Goal, 11, "EnderMan.EndermanTakeBlockGoal"),
    // `EnderMan.EndermanLookForPlayerGoal(this, this::isAngryAt)` (`:484-574`) —
    // the teleport-on-stare goal, needing the gaze test, a teleport primitive and
    // the anger predicate all three.
    Registration::missing(Selector::Target, 1, "EnderMan.EndermanLookForPlayerGoal"),
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

/// `monster/zombie/Zombie.java:113-115` **plus**
/// `monster/zombie/ZombifiedPiglin.java:72-77`.
///
/// The non-uniform species in this family. `ZombifiedPiglin` declares no
/// `registerGoals`; it overrides `addBehaviourGoals` (`:71-78`), which
/// `Zombie.registerGoals` calls at `Zombie.java:116` after adding three rows of
/// its own (`:113-115`). Those three are inherited verbatim and open this table.
/// The override does **not** call `super.addBehaviourGoals()`, so Zombie's
/// `MoveThroughVillageGoal` (`Zombie.java:122`) is absent here — correctly, not
/// by oversight — and `SpearUseGoal`/`ZombieAttackGoal` sit at 1/2 rather than
/// Zombie's 2/3.
///
/// Attributes `:80-85` — `Zombie.createAttributes()` plus
/// `SPAWN_REINFORCEMENTS_CHANCE 0.0`, `MOVEMENT_SPEED 0.23F`,
/// `ATTACK_DAMAGE 5.0`. `FOLLOW_RANGE 35.0` is inherited (`Zombie.java:133`) and
/// is what sizes the alert box below.
///
/// Group aggro is `Missing` in two halves. The propagation itself is
/// `alertOthers` (`:139-149`): every other zombified piglin in a box of
/// `(FOLLOW_RANGE, 10.0, FOLLOW_RANGE)` = **±35 XZ, ±10 Y** that has no target
/// yet and is not allied to the victim's attacker has `setTarget` called on it.
/// It is throttled by `maybeAlertOthers` (`:127-137`) on `ALERT_INTERVAL =
/// TimeUtil.rangeOfSeconds(4, 6)` = **[80, 120] ticks** and requires line of
/// sight to the target. Note it is driven from `customServerAiStep` (`:112`),
/// **not** from a goal — so it is not a roster row at all, and modelling it means
/// a census in `MobSim::tick`, not a `Registration` here.
pub const ZOMBIFIED_PIGLIN: &[Registration] = &[
    // Inherited from `Zombie.registerGoals` (`Zombie.java:113-115`).
    Registration::missing(Selector::Goal, 4, "Zombie.ZombieAttackTurtleEggGoal"),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
    // `ZombifiedPiglin.addBehaviourGoals` (`:72-77`) from here down.
    // `SpearUseGoal<>(this, 1.0, 1.0, 10.0F, 2.0F)` — a ranged goal, so #227's.
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

/// `animal/bee/Bee.java:174-195`.
///
/// Seventeen registrations, of which this repo builds five. Note that `:181`,
/// `:185` and `:187` are field assignments rather than `addGoal` calls — a
/// line-count transcription of that block gets twenty rows and three of them are
/// not registrations.
///
/// Attributes `:528-533` — `Animal.createAnimalAttributes()` plus
/// `MAX_HEALTH 10.0`, `FLYING_SPEED 0.6F`, `MOVEMENT_SPEED 0.3F`,
/// `ATTACK_DAMAGE 2.0`.
///
/// **Sting-then-die is not "die on stinging", and the difference is the whole
/// mechanism.** `doHurtTarget` (`:224-249`) deals `(int)ATTACK_DAMAGE` = 2,
/// applies poison for 0/10/18 seconds by difficulty (`:231-240`), then sets
/// `hasStung` (`:243`) and calls `stopBeingAngry()` (`:244`) — the bee survives
/// the sting. Death comes later and stochastically, in `customServerAiStep`
/// (`:374-379`): once `hasStung`, `timeSinceSting++` each tick and every fifth
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
    // `Bee.BeeAttackGoal(this, 1.4F, true)` (`:698-712`) extends
    // `MeleeAttackGoal`, but its `canUse` adds `isAngry() && !hasStung()`
    // (`:705`). Registering a bare melee goal at priority 0 would make every bee
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
    // `Bee.BeeHurtByOtherGoal(this).setAlertOthers()` (`:999-1006`) extends
    // `HurtByTargetGoal`; it overrides only `canContinueToUse`, adding
    // `isAngry()`. `canUse` — the retaliation trigger — is inherited unchanged,
    // so ours is a faithful stand-in for the part that fires.
    Registration::target(1, "Bee.BeeHurtByOtherGoal", hurt_by_target),
    // `Bee.BeeBecomeAngryTargetGoal` (`:714-739`) — gated on
    // `isAngry() && !hasStung()`; see the module doc's neutrality trap.
    Registration::missing(Selector::Target, 2, "Bee.BeeBecomeAngryTargetGoal"),
    Registration::missing(Selector::Target, 3, "ResetUniversalAngerTargetGoal"),
];

/// `animal/wolf/Wolf.java:128-149`.
///
/// Attributes `:216-217` — `Animal.createAnimalAttributes()` plus
/// `MOVEMENT_SPEED 0.3F`, `MAX_HEALTH 8.0`, `ATTACK_DAMAGE 4.0`;
/// `applyTamingSideEffects` (`:430-438`) raises `MAX_HEALTH` to `40.0` on taming
/// and drops it back to `8.0`. `FOLLOW_RANGE` is the `Mob.createMobAttributes`
/// default `16.0` (`Mob.java:167`), which sizes the pack-alert box.
///
/// **Eight of the twenty rows need an owner**, and no ownership model exists
/// tree-wide — not on `MobController`, not on `SimMob`, and not even
/// expressibly, because `PlayerPerception` carries a position and a held item but
/// no player identity to be owned *by*. `SitWhenOrderedToGoal`, `FollowOwnerGoal`,
/// `BegGoal`, `OwnerHurtByTargetGoal`, `OwnerHurtTargetGoal` and both
/// `NonTameRandomTargetGoal`s are all `Missing` for that one reason.
///
/// Pack aggro is `HurtByTargetGoal(this).setAlertOthers()` at target priority 3,
/// and the ownership gap reaches into it too:
/// `HurtByTargetGoal.alertOthers` filters same-class neighbours in a
/// `(16.0, 10.0, 16.0)` box, and for a `TamableAnimal` it additionally requires
/// **`tamable.getOwner() == other.getOwner()`**
/// (`ai/goal/target/HurtByTargetGoal.java:88`) — a wolf pack only rallies for
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
    Registration::missing(Selector::Goal, 2, "SitWhenOrderedToGoal"),
    // `Wolf.WolfAvoidEntityGoal<>(this, Llama.class, 24.0F, 1.5, 1.5)`
    // (`:666-696`). Not merely an `AvoidEntityGoal` with a 24-block radius: its
    // `canUse` requires `!wolf.isTame()` (`:678`) and rolls against the llama's
    // strength (`:681-683`). Without the tame flag, ours would make a *tamed*
    // wolf flee too, so this waits on the owner model rather than shipping half.
    Registration::missing(Selector::Goal, 3, "Wolf.WolfAvoidEntityGoal(Llama)"),
    Registration::missing(Selector::Goal, 4, "LeapAtTargetGoal"),
    // `MeleeAttackGoal(this, 1.0, true)`.
    Registration::goal(5, "MeleeAttackGoal", melee_attack),
    Registration::missing(Selector::Goal, 6, "FollowOwnerGoal"),
    Registration::goal(7, "BreedGoal", breed_1_0),
    Registration::goal(8, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::missing(Selector::Goal, 9, "BegGoal"),
    Registration::goal(10, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(10, "RandomLookAroundGoal", random_look_around),
    Registration::missing(Selector::Target, 1, "OwnerHurtByTargetGoal"),
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

/// `TamableAnimal.TamableAnimalPanicGoal(1.5, …)` — the wolf's panic speed
/// factor (`animal/wolf/Wolf.java:130`). Vanilla's own `PanicGoal` speed argument
/// is a `MOVEMENT_SPEED` multiplier.
fn panic_1_5(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.5))
}

/// `TemptGoal(this, 1.25, BEE_FOOD, false)` (`animal/bee/Bee.java:178`).
fn tempt_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.25))
}

/// `FollowParentGoal(this, 1.25)` (`animal/bee/Bee.java:183`).
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
    const ENDERMAN_SPEED: f64 = 0.3; // `monster/EnderMan.java:116`
    const PIGLIN_SPEED: f64 = 0.23; // `monster/zombie/ZombifiedPiglin.java:83`
    const BEE_SPEED: f64 = 0.3; // `animal/bee/Bee.java:532`
    const WOLF_SPEED: f64 = 0.3; // `animal/wolf/Wolf.java:217`

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

        // Inherited from `Zombie.registerGoals` (`Zombie.java:113-115`).
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

    /// The four headline mechanisms of issue #233 must be present as
    /// `Coverage::Missing` rows at their real vanilla priorities — not absent, and
    /// not silently `Modelled` by a goal that does something else.
    ///
    /// This is the gate that fails if someone "implements" the enderman stare by
    /// pointing the row at a plain melee goal, or drops a row because nothing
    /// builds it.
    #[test]
    fn the_four_blocked_mechanisms_are_recorded_as_missing_not_omitted() {
        let blocked: &[(&str, i32, Selector, &str)] = &[
            (
                "enderman",
                1,
                Selector::Goal,
                "EnderMan.EndermanFreezeWhenLookedAt",
            ),
            (
                "enderman",
                1,
                Selector::Target,
                "EnderMan.EndermanLookForPlayerGoal",
            ),
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
            // `animal/wolf/Wolf.java:130` — the one non-unit factor in this family.
            ("wolf", WOLF_SPEED, "TamableAnimal.TamableAnimalPanicGoal", 1.5),
            // `animal/bee/Bee.java:178` and `:183`.
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
    /// emphatically not `ScriptMob`, which overrides every perception method and
    /// is how issue #441's island stayed green for eight goals at once.
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
    /// table carries a panic goal (`animal/wolf/Wolf.java:130`) alongside its
    /// melee goal (`:134`) and both claim MOVE.
    ///
    /// This is behavioural: it asserts where the wolf *is* and whether it
    /// *struck*, never a `can_use` return value. The two phases come from cited
    /// constants rather than from watching the output:
    ///
    /// * `is_panicking()` is `damage_ticks > 0`, and `note_hurt` sets it to
    ///   `PANIC_DAMAGE_TICKS = 40` (`ai/navigating_mob.rs:82`,
    ///   `LivingEntity.java:1420-1421`). So panic owns MOVE for the first 40
    ///   ticks.
    /// * `HurtByTargetGoal::start` sets the attack target from `last_hurt_by`,
    ///   which persists `LAST_HURT_BY_TICKS = 100` (`:68`,
    ///   `LivingEntity.java:493`) — long enough to still be hunting when panic
    ///   ends.
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
    /// `PanicGoal::is_interruptable()` is `false` (`ai/goals.rs:456-458`) and
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
    /// enderman, bee and wolf register one (`EnderMan.java:94`, `Bee.java:191`,
    /// `Wolf.java:129`) and the zombified piglin does not, because it inherits
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
