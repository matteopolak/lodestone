//! Which species run on a [`Brain`], and the behaviour set they get.
//!
//! The [`ai::roster`](crate::ai::roster) analogue for the *other* AI
//! architecture. `ai::roster::goals_for` answers "what goals does a cow get";
//! this answers "does this species have a brain at all, and what is in it".
//!
//! # The species list is a jar claim, so it is gated against the jar
//!
//! [`BRAIN_SPECIES`] is a hand-transcribed name list, and `CLAUDE.md` is blunt
//! about those: five have been wrong recently in this repo, the worst a tag set
//! 1-of-4 correct *with a false positive*. So this list is not merely asserted —
//! `tests/brain_census.rs` re-derives it from the decompiled 26.2 sources
//! (every `world/entity/**.java` declaring `brainProvider`/`makeBrain`, joined to
//! its `EntityType.register` id) and fails on any drift in **either** direction,
//! with `LODESTONE_REGEN=1` to refresh. The gate is `#[ignore]`d because it needs
//! `.cache/mc/26.2/src`, which is not repo state.
//!
//! A name missing from this list is not a cosmetic defect: it is a species that
//! silently gets [`FALLBACK`](crate::ai::roster::FALLBACK) stroll-and-look goals
//! instead of a brain, which is exactly the failure mode that looks fine on
//! screen and is wrong.
//!
//! # Scope: this is the plumbing, not the behaviour packages
//!
//! **This doc used to say every brain species gets the identical generic
//! CORE+IDLE [`scaffold`], full stop.** That is no longer true: six species —
//! goat, camel, armadillo, frog, sniffer, allay, the `AnimalPanic`-family
//! half of the passive-roster brain species — now also flee a recent
//! attacker via [`scaffold_with_panic`], each at its own jar-cited speed
//! multiplier. Everything else about the scaffold claim
//! still holds: this is still the composition, not the full per-species
//! behaviour package (a villager's profession schedule and trading
//! availability, a warden's vibration sensor and anger, a piglin's barter and
//! hoglin hunt, are separate units layered on this one and need machinery
//! this crate does not have yet — the warden's needs vanilla's
//! world-event/vibration registry, which is **absent** here
//! (`lodestone_ecs::GameEvent` is the client plugin bus and `GAME_EVENT` is
//! vanilla's weather packet; neither is the vibration listener)).
//!
//! So a camel and a warden no longer behave *identically* — the camel now
//! flees a hit at 4× speed, the warden still just wanders and watches
//! players — but both are still a floor rather than a finish, and both
//! reached zero pixels before the brain was wired to production at all.

use super::activity::Activity;
use super::behavior::{Behavior, BehaviorControl, Leaf};
use super::behaviors::{
    AvoidTarget, CopyMemoryWithExpiry, LookAtTargetSink, MoveToTargetSink, Panic, PrepareRam,
    RamTarget, RandomStroll, SetPlayerLookTarget, WalkToPoi,
};
use super::driver::BrainGoal;
use super::gate::GateBehavior;
use super::memory::{MemoryModuleType, MemoryStatus};
use super::sensor::{
    HurtBySensor, NearestHostileSensor, NearestPlayerSensor, NearestVisibleLivingEntitiesSensor,
    NearestVisibleZombifiedSensor, VillagerPoiSensor,
};
use super::Brain;

/// The walk-target speed **modifier** the scaffold's stroll writes.
///
/// This is vanilla's `speedModifier` unit — the `0.6F`-ish float a brain
/// behaviour hands to `WalkTarget` — not blocks per tick. Worth stating plainly
/// because the two are easy to confuse and
/// [`SpeciesContext::speed`](crate::ai::SpeciesContext::speed) is the *other* one:
/// it is the `movement_speed` attribute in blocks/tick, and feeding it here would
/// be a unit error.
///
/// It also does not currently change how fast the mob moves.
/// [`NavigatingMob::advance`](crate::ai::NavigatingMob) is a kinematic follower
/// that steps at its own `step_per_tick` and ignores the speed the navigator was
/// started with, so this value reaches
/// [`MobController::move_to`](crate::ai::MobController::move_to) and stops there.
/// Stated so nobody later "fixes" a speed bug by editing this constant.
pub const SCAFFOLD_STROLL_SPEED: f32 = 1.0;

/// How far the scaffold's look behaviour will track a player, in blocks. Vanilla's
/// brain mobs use `8.0F` for the generic `SetEntityLookTarget` player row, the
/// same figure the goal system's `LookAtPlayerGoal(Player.class, 8.0F)` uses.
pub const SCAFFOLD_LOOK_DISTANCE: f32 = 8.0;

/// The resource-key **paths** of every concrete 26.2 mob that runs on a
/// [`Brain`] rather than a [`GoalSelector`](crate::ai::GoalSelector).
///
/// Transcribed from the crate census in [`super`]'s module doc; gated against the
/// decompiled sources by `tests/brain_census.rs` — see this module's doc for why
/// the gate exists rather than trust.
///
/// Sorted, so a diff against the jar-derived list is a plain set comparison.
pub const BRAIN_SPECIES: &[&str] = &[
    "allay",
    "armadillo",
    "axolotl",
    "breeze",
    "camel",
    "copper_golem",
    "creaking",
    "frog",
    "goat",
    "happy_ghast",
    "hoglin",
    "nautilus",
    "piglin",
    "piglin_brute",
    "sniffer",
    "tadpole",
    "villager",
    "warden",
    "zoglin",
    "zombie_nautilus",
];

/// Whether `species` (a resource-key path, `"villager"` not
/// `"minecraft:villager"`) runs on a brain.
#[must_use]
pub fn is_brain_species(species: &str) -> bool {
    BRAIN_SPECIES.contains(&species)
}

fn leaf<B: Behavior + 'static>(b: B) -> Box<dyn BehaviorControl> {
    Box::new(Leaf::new(b))
}

/// The universal CORE + IDLE scaffold every vanilla brain shares: the move and
/// look **sinks** in `CORE`, and a run-one gate of look-at-player / stroll in
/// `IDLE`.
///
/// The stroll ⇄ move-sink pair is the architecture in miniature — two behaviours
/// that never name each other, coordinating entirely through the `WALK_TARGET`
/// memory. `CORE` holds the sinks because they must keep running across an
/// activity switch; `IDLE` holds the deciders.
#[must_use]
pub fn scaffold(stroll_speed: f32, look_distance: f32) -> Brain {
    let mut brain = Brain::new();
    brain.add_sensor(Box::new(NearestPlayerSensor));
    brain.add_activity(
        Activity::CORE,
        vec![
            (0, leaf(LookAtTargetSink::default())),
            (1, leaf(MoveToTargetSink::new())),
        ],
        Vec::new(),
        Vec::new(),
    );
    brain.add_activity(
        Activity::IDLE,
        vec![(
            5,
            Box::new(GateBehavior::run_one(
                "idle_gate",
                vec![
                    leaf(SetPlayerLookTarget::new(look_distance)),
                    leaf(RandomStroll::new(stroll_speed)),
                ],
            )),
        )],
        Vec::new(),
        Vec::new(),
    );
    brain
}

/// [`scaffold`] plus a [`Panic`] behaviour in `CORE` — `AnimalPanic`'s own
/// composition, ported for the six brain species that register it (goat,
/// camel, armadillo, frog, sniffer, allay; axolotl is deliberately absent —
/// vanilla gives it play-dead-on-low-health instead, a different mechanism
/// this does not build).
///
/// `panic_speed_multiplier` is each species' own jar constant — see
/// [`brain_for`]'s per-species table, not a figure this function invents.
///
/// **Priority `-1`, ahead of [`LookAtTargetSink`]'s `0` and
/// [`MoveToTargetSink`]'s `1`.** [`Brain::add_activity`] sorts behaviours
/// within an activity by priority and ticks them in that order every frame,
/// so `Panic` must write a fresh `WALK_TARGET` *before* `MoveToTargetSink`
/// reads it in the same tick — running it after would leave the mob stepping
/// toward last tick's fleeing point one tick late, every tick, for the whole
/// panic.
#[must_use]
pub fn scaffold_with_panic(stroll_speed: f32, look_distance: f32, panic_speed_multiplier: f32) -> Brain {
    let mut brain = scaffold(stroll_speed, look_distance);
    brain.add_sensor(Box::new(HurtBySensor));
    brain.add_activity(
        Activity::CORE,
        vec![(-1, leaf(Panic::new(panic_speed_multiplier)))],
        Vec::new(),
        Vec::new(),
    );
    brain
}

/// `Villager`'s own panic: an **Activity-swap** shaped differently from the
/// six [`PANIC_SPEED_MULTIPLIER`] species' in-place [`Panic`] behaviour.
///
/// Vanilla's `VillagerPanicTrigger` is an imperative `Behavior` that reaches
/// into its own entity's `Brain` and calls `setActiveActivityIfPossible
/// (Activity.PANIC)` directly — a seam this crate's [`Behavior`] trait
/// deliberately does not expose (see [`Brain::add_activity_any_of`]'s own doc).
/// The declarative equivalent, wired through the same
/// `set_active_activity_to_first_valid` candidate list every other brain
/// species' `updateActivity` already uses: offer `PANIC` **before** `IDLE`,
/// gated on "hurt OR a hostile is nearby"
/// (`VillagerPanicTrigger.isHurt`/`hasHostile`) via [`Brain::add_activity_any_of`].
///
/// **What is ported and what is not**, against
/// `VillagerGoalPackages.getPanicPackage`: the flee-and-look shape is real
/// (reusing [`Panic`], the same `AnimalPanic`-style fleeing this repo already
/// has, rather than porting `SetWalkTargetAwayFrom`'s *directed* flee — a
/// villager here flees to a random nearby spot instead of one chosen to
/// increase distance from the threat specifically) and
/// `VillagerCalmDown`/village-bound stroll are not ported (this repo has no
/// village-bounds concept). **`VillagerPanicTrigger.tick`'s
/// `spawnGolemIfNeeded` (golem-summon-on-hurt) is landed, but not here** —
/// this doc used to say it was a separate, unbuilt unit, and that went stale
/// the moment it shipped: `MobSim::tick_golem_summon`
/// (`lodestone_server::mobs`) recomputes "is this villager hurt or does it
/// see a hostile" directly against `self.mobs` on the same 100-tick cadence,
/// because it needs two things no single mob's `BrainMob` seam can give it —
/// other villagers' own state (the agreement count) and the power to create
/// a new entity — see that function's own doc for why it lives on the host
/// rather than as a `Brain` behaviour, and its three disclosed cuts from the
/// jar original.
/// `Villager.SPEED_MODIFIER` — the one figure every non-panic villager
/// package (`WORK`, `REST`, `MEET`, `IDLE`) is built with
/// (`ActivityData.create(Activity.WORK, VillagerGoalPackages.getWorkPackage(profession, 0.5F), …)`
/// and its three siblings, all literal `0.5F`).
pub const VILLAGER_SPEED_MODIFIER: f32 = 0.5;

/// `data/minecraft/timeline/villager_schedule.json`'s
/// `minecraft:gameplay/villager_activity` track (the non-baby schedule;
/// `baby_villager_activity`'s `PLAY`/`REST` pairing is not ported — this
/// crate has no separate baby-villager brain). Read directly off the shipped
/// JSON, not transcribed from a Java constant, since 26.2 moved the vanilla
/// schedule tables into data.
const VILLAGER_SCHEDULE: &[(i32, Activity)] = &[
    (10, Activity::IDLE),
    (2000, Activity::WORK),
    (9000, Activity::MEET),
    (11000, Activity::IDLE),
    (12000, Activity::REST),
];

/// A villager's brain: the [`scaffold`] plus a real WORK/MEET/REST/IDLE
/// day schedule (issue #231's remaining half — professions/POI claiming
/// itself is `crate::mobs::villager` in the server crate, out of this
/// crate's scope) and the villager's own Activity-swap panic.
///
/// # What WORK/MEET/REST actually do, against `VillagerGoalPackages`
///
/// Each is a single [`WalkToPoi`] behaviour reading the position a host
/// claimed into [`MemoryModuleType::JOB_SITE`]/`HOME`/`MEETING_POINT`
/// (fed every tick by [`VillagerPoiSensor`], since this crate's `BrainMob`
/// seam has no live claim-ledger reference for a behaviour to call back
/// into, unlike vanilla's `AcquirePoi`/`YieldJobSite` which write the
/// memory themselves) — same close-enough distances as vanilla's own
/// `SetWalkTargetFromBlockMemory` calls (`9`/`1`/`6` blocks). **Not ported**:
/// `WorkAtPoi`/`WorkAtComposter` (the actual profession-specific work
/// animation/particle), `ShowTradesToPlayer`/`SetLookAndInteract`
/// (villager-initiated trade UI), `SleepInBed` (the sleep pose and bed
/// occupancy flag), `SocializeAtBell`/`StrollAroundPoi` (wandering near the
/// claim rather than beelining to its exact centre), and `GiveGiftToHero`.
/// A villager under this port reaches its claimed workstation, bed or bell
/// and stands there — the day/night activity switch and the walk are real;
/// what a villager *does* once arrived is not.
///
/// **`WORK` requires [`MemoryModuleType::JOB_SITE`] present and `MEET`
/// requires [`MemoryModuleType::MEETING_POINT`] present**, exactly
/// `Villager.java`'s own `ImmutableSet.of(Pair.of(…, VALUE_PRESENT))` —
/// an unemployed villager or one that never claimed a bell simply never
/// becomes eligible for that activity and the schedule falls back to
/// `IDLE` instead (`Brain::set_active_activity_if_possible`'s own
/// fallback). `REST` carries no such requirement in vanilla either (a
/// homeless villager still "rests", just with nowhere to walk to — see
/// `getRestPackage`'s own `RunOne` fallback for `HOME` absent, not ported
/// here), so it is registered with an empty condition list.
///
/// See [`brain_for`]'s own doc for why [`Activity::PANIC`] is a
/// candidate-list swap rather than part of this schedule.
#[must_use]
pub fn villager_brain() -> Brain {
    let mut brain = scaffold(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE);
    brain.add_sensor(Box::new(HurtBySensor));
    brain.add_sensor(Box::new(NearestHostileSensor));
    brain.add_sensor(Box::new(VillagerPoiSensor));
    // `AnimalPanic(0.5F)` is `VillagerGoalPackages.getPanicPackage`'s own
    // `speedModifier` argument passed from `Villager.java:164`
    // (`getPanicPackage(0.5F)`), distinct from every `PANIC_SPEED_MULTIPLIER`
    // row below (none of which is a villager).
    brain.add_activity_any_of(
        Activity::PANIC,
        vec![(-1, leaf(Panic::new(0.5)))],
        vec![
            (MemoryModuleType::HURT_BY, MemoryStatus::ValuePresent),
            (MemoryModuleType::NEAREST_HOSTILE, MemoryStatus::ValuePresent),
        ],
        Vec::new(),
    );
    brain.add_activity(
        Activity::WORK,
        vec![(
            2,
            leaf(WalkToPoi::new(
                MemoryModuleType::JOB_SITE,
                VILLAGER_SPEED_MODIFIER,
                9,
            )),
        )],
        vec![(MemoryModuleType::JOB_SITE, MemoryStatus::ValuePresent)],
        Vec::new(),
    );
    brain.add_activity(
        Activity::MEET,
        vec![(
            2,
            leaf(WalkToPoi::new(
                MemoryModuleType::MEETING_POINT,
                VILLAGER_SPEED_MODIFIER,
                6,
            )),
        )],
        vec![(MemoryModuleType::MEETING_POINT, MemoryStatus::ValuePresent)],
        Vec::new(),
    );
    brain.add_activity(
        Activity::REST,
        vec![(
            2,
            leaf(WalkToPoi::new(MemoryModuleType::HOME, VILLAGER_SPEED_MODIFIER, 1)),
        )],
        Vec::new(),
        Vec::new(),
    );
    brain.set_schedule(VILLAGER_SCHEDULE.to_vec());
    brain
}

/// A goat's brain: [`scaffold_with_panic`] (`AnimalPanic(2.0F)`, `GoatAi`'s
/// own figure — see [`PANIC_SPEED_MULTIPLIER`]'s `"goat"` row) plus the
/// ram-attack pair (issue #230's genuine ask; the long jump is a separate,
/// arc-shaped unit not built here — see
/// [`super::behaviors::PrepareRam`]/[`super::behaviors::RamTarget`]'s own
/// docs for exactly what each does and does not port).
///
/// # Why `RAM`'s eligibility condition adds a memory vanilla's own table does not require
///
/// `GoatAi.initRamActivity`'s own `ImmutableSet` requires only
/// `RAM_COOLDOWN_TICKS` absent (plus `TEMPTING_PLAYER`/`BREED_TARGET` absent,
/// neither of which this crate models). Requiring only that would make `RAM`
/// "eligible" — and therefore active, since it precedes `IDLE` in
/// [`brain_for`]'s candidate list — even with nothing nearby to ram, which
/// would suppress `IDLE`'s stroll/look behaviours for the whole
/// [`PrepareRam`](super::behaviors::PrepareRam) timeout (160 ticks) every
/// time the cooldown lapses with no target around. That is a genuine vanilla
/// quirk (`PrepareRamNearestTarget`'s own fail path sets a short cooldown and
/// retries), not a bug this port needs to reproduce: requiring
/// `NEAREST_VISIBLE_LIVING_ENTITIES` present too keeps a goat with nothing
/// nearby idling normally instead of periodically freezing.
#[must_use]
pub fn goat_brain() -> Brain {
    let mut brain = scaffold_with_panic(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE, 2.0);
    brain.add_sensor(Box::new(NearestVisibleLivingEntitiesSensor));
    brain.add_activity(
        Activity::RAM,
        vec![
            // `GoatAi.initRamActivity`'s own priorities: `RamTarget` at 0,
            // `PrepareRamNearestTarget` at 1.
            (0, leaf(RamTarget::new(3.0, 600, 6000))),
            (1, leaf(PrepareRam::new(4.0, 7.0, 1.25, 20, 600, 6000))),
        ],
        vec![
            (MemoryModuleType::RAM_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
            (
                MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES,
                MemoryStatus::ValuePresent,
            ),
        ],
        Vec::new(),
    );
    brain
}

/// `PiglinAi.avoidZombified`'s own duration range —
/// `TimeUtil.rangeOfSeconds(5, 7)` = 100–140 ticks.
const AVOID_ZOMBIFIED_DURATION: (i64, i64) = (100, 140);

/// A piglin's brain: the [`scaffold`] plus the one slice of `PiglinAi` this
/// crate has machinery for — an adult piglin flees a nearby visible
/// zombified piglin (`PiglinAi.avoidZombified`/`initRetreatActivity`).
///
/// # What this is, and what it deliberately is not
///
/// Real vanilla `PiglinAi` also barters (`ADMIRE_ITEM`), hunts hoglins and
/// fights players not wearing gold armour (`FIGHT`, beyond a generic attack
/// target), celebrates a kill (`CELEBRATE`) and lets a baby ride a hoglin
/// (`RIDE`) — all fed by `PiglinSpecificSensor`, a single sensor producing
/// nine memory values (a repellent-block search, wanted-item detection,
/// gold-armour detection, a live hoglin/piglin population census) this crate
/// has no seam for. `super`'s own module doc already discloses "a piglin's
/// barter and hoglin hunt… need machinery this crate does not have yet";
/// this lands exactly one slice of it. `AVOID` is the whole slice landed
/// here: it is the one piglin-specific behaviour namable with a single new
/// host primitive ([`BrainMob::nearest_visible_zombified`](super::mob::BrainMob::nearest_visible_zombified))
/// rather than the nine-memory sensor the rest of the package would share.
///
/// **A piglin has no work/rest/bed schedule and no golem-summon mechanism —
/// neither applies, and building either would be fiction, not a gap.**
/// `PiglinAi.java` registers no `Schedule` at all (`Piglin`/`AbstractPiglin`
/// are hostile mobs, not villagers, and share none of `Villager`'s
/// `WORK`/`MEET`/`REST` machinery), and `spawnGolemIfNeeded` lives only on
/// `VillagerPanicTrigger` — nothing in `PiglinAi.java` calls anything
/// resembling it. `babyAvoidNemesis` (a baby piglin fleeing a nearby hoglin)
/// is the other half of `initCoreActivity`'s avoid pair and is **not**
/// ported here either: it needs a `NEAREST_VISIBLE_NEMESIS` sensor this
/// crate also has no host primitive for yet, a separate gap from
/// `avoidZombified`'s.
///
/// `piglin_brute` deliberately does **not** get this brain — see
/// [`brain_for`]'s own piglin arm for why.
///
/// # Disclosed simplification
///
/// [`AvoidTarget`]'s own doc discloses the flee-direction simplification
/// shared with [`Panic`].
#[must_use]
pub fn piglin_brain() -> Brain {
    let mut brain = scaffold(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE);
    brain.add_sensor(Box::new(NearestVisibleZombifiedSensor));
    // `PiglinAi.initCoreActivity`'s own `avoidZombified()` row — always
    // ticking, refreshing `AVOID_TARGET` from `NEAREST_VISIBLE_ZOMBIFIED`
    // every tick a zombified piglin is visible.
    brain.add_activity(
        Activity::CORE,
        vec![(
            0,
            leaf(CopyMemoryWithExpiry::new(
                MemoryModuleType::NEAREST_VISIBLE_ZOMBIFIED,
                MemoryModuleType::AVOID_TARGET,
                AVOID_ZOMBIFIED_DURATION.0,
                AVOID_ZOMBIFIED_DURATION.1,
            )),
        )],
        Vec::new(),
        Vec::new(),
    );
    brain.add_activity(
        Activity::AVOID,
        // `PiglinAi.initRetreatActivity`'s own priority 1 for the walk-away
        // behaviour (priority 0's `SetEntityLookTargetSometimes` and
        // priority 3's `EraseMemoryIf` early-exit are both disclosed as not
        // ported, on `AvoidTarget`'s own doc).
        vec![(1, leaf(AvoidTarget::new(1.0)))],
        vec![(MemoryModuleType::AVOID_TARGET, MemoryStatus::ValuePresent)],
        Vec::new(),
    );
    brain
}

/// `AnimalPanic`'s own speed multiplier, one row per species that registers
/// it — `new AnimalPanic(speedMultiplier)` (or, for the sniffer, the
/// anonymous subclass's identical constructor argument) in each species' own
/// `registerGoals`/`*Ai.initCoreActivity`.
const PANIC_SPEED_MULTIPLIER: &[(&str, f32)] = &[
    ("allay", 2.5),
    ("armadillo", 2.0),
    ("camel", 4.0),
    ("frog", 2.0),
    ("goat", 2.0),
    ("sniffer", 2.0),
];

/// The [`BrainGoal`] a brain-driven species gets, or `None` for a species that
/// runs on the goal system.
///
/// This is the function [`ai::roster::goals_for`](crate::ai::roster::goals_for)
/// consults, which is how a brain reaches a real mob in a real world: the server's
/// `spawn_species` already installs whatever `goals_for` returns, so nothing on
/// the host side has to know brains exist.
#[must_use]
pub fn brain_for(species: &str) -> Option<BrainGoal> {
    if !is_brain_species(species) {
        return None;
    }
    // The villager is the one species whose panic is an Activity-swap rather
    // than an in-place `Panic` behaviour, so it needs both a different brain
    // ([`villager_brain`]) and a candidate list that actually offers `PANIC`
    // — `BrainGoal::idle` only ever offers `IDLE`, which would build the
    // activity and then never let it become active.
    //
    // **Deliberately just `[PANIC]`, not `[PANIC, IDLE]`.**
    // `BrainGoal::tick` runs this candidate check unconditionally every
    // tick, and `set_active_activity_to_first_valid` stops at the first
    // eligible candidate — `IDLE` has no requirements at all
    // (`villager_brain`'s scaffold registers it with an empty condition
    // list), so it is *always* eligible. Leaving it in this list would mean
    // every tick that is not itself the (throttled, every-20-ticks) schedule
    // check would force the villager back to `IDLE`, fighting
    // `update_activity_from_schedule` and making `WORK`/`MEET`/`REST` flicker
    // on for one tick in twenty and off for the rest. Dropping `IDLE` here
    // is safe precisely because `villager_brain` carries a real schedule
    // (`Brain::has_schedule`) whose own fallback is already `IDLE` — see
    // `BrainGoal::tick`'s own doc for the split this candidate list and the
    // schedule now share.
    if species == "villager" {
        return Some(BrainGoal::new(villager_brain(), vec![Activity::PANIC]));
    }
    // Same shape as the villager special-case above: a goat needs both a
    // brain with an extra activity (`RAM`) and a candidate list that offers
    // it — `BrainGoal::idle` only ever offers `IDLE`.
    if species == "goat" {
        return Some(BrainGoal::new(goat_brain(), vec![Activity::RAM, Activity::IDLE]));
    }
    // Same shape again: `piglin_brain` needs both an extra activity (`AVOID`)
    // and a candidate list that offers it. `piglin_brute` is deliberately
    // excluded — `PiglinBruteAi.updateActivity` offers only
    // `[Activity.FIGHT, Activity.IDLE]` in the jar, so a brute never flees a
    // zombified piglin and falls through to the plain [`scaffold`] below like
    // every other brain species this crate has no dedicated package for.
    if species == "piglin" {
        return Some(BrainGoal::new(piglin_brain(), vec![Activity::AVOID, Activity::IDLE]));
    }
    let brain = match PANIC_SPEED_MULTIPLIER.iter().find(|&&(s, _)| s == species) {
        Some(&(_, speed)) => scaffold_with_panic(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE, speed),
        None => scaffold(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE),
    };
    Some(BrainGoal::idle(brain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn species_list_is_sorted_and_has_no_duplicates() {
        // Not cosmetic: the jar-drift gate compares sorted sets, and a duplicate
        // would make `BRAIN_SPECIES.len()` disagree with the set size while every
        // `contains` check still passed.
        let mut sorted = BRAIN_SPECIES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), BRAIN_SPECIES, "list must be sorted+unique");
        assert_eq!(BRAIN_SPECIES.len(), 20, "the crate census counts 20");
    }

    #[test]
    fn brain_species_get_a_brain_and_goal_species_do_not() {
        assert!(brain_for("villager").is_some());
        assert!(brain_for("warden").is_some());
        assert!(brain_for("camel").is_some());
        // The negative half, and it is the one that matters: a goal-system mob
        // must not acquire a brain, or it would get the scaffold's stroll *and*
        // its roster's stroll, both writing movement.
        assert!(brain_for("zombie").is_none());
        assert!(brain_for("cow").is_none());
        assert!(brain_for("creeper").is_none());
        // Near-misses that are genuinely goal-driven despite the name: a
        // zombified piglin is a `Zombie` subclass and a zombie villager is a
        // `Zombie`, so neither has a brain even though "piglin" and "villager"
        // are substrings. A `contains`-style match would get both wrong.
        assert!(brain_for("zombified_piglin").is_none());
        assert!(brain_for("zombie_villager").is_none());
    }

    /// Named for exactly what it checks. An earlier draft called this
    /// `..._and_the_sensor` while asserting nothing about sensors — a small
    /// instance of the same disease as the rest of this issue, so it is worth not
    /// shipping.
    #[test]
    fn the_scaffold_starts_with_core_and_idle_active() {
        let brain = scaffold(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE);
        assert!(brain.is_active(Activity::CORE), "CORE is always active");
        assert!(brain.is_active(Activity::IDLE), "IDLE is the default");
    }

    #[test]
    fn every_panic_species_is_a_real_brain_species_and_the_table_is_sorted_and_unique() {
        // The multiset gate this crate always wants for a hand-transcribed
        // list: not just "every name resolves", but "no duplicate could hide
        // a wrong figure behind a right one".
        let mut names: Vec<&str> = PANIC_SPEED_MULTIPLIER.iter().map(|&(s, _)| s).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, {
            names.sort_unstable();
            names
        });
        for &(species, _) in PANIC_SPEED_MULTIPLIER {
            assert!(
                is_brain_species(species),
                "{species} is in PANIC_SPEED_MULTIPLIER but not BRAIN_SPECIES"
            );
        }
        // Axolotl is the named exclusion — vanilla gives it play-dead instead
        // of AnimalPanic, so it must never silently pick this table up.
        assert!(
            !PANIC_SPEED_MULTIPLIER.iter().any(|&(s, _)| s == "axolotl"),
            "axolotl must not be in the AnimalPanic table"
        );
    }

    /// A minimal [`BrainMob`] double, local to this module because
    /// `brain::mod`'s own `TestMob` is private to its file. Only implements
    /// what [`Panic`]/[`super::sensor::HurtBySensor`]/[`MoveToTargetSink`]
    /// actually call.
    struct PanicTestMob {
        pos: lodestone_model::Vec3,
        time: i64,
        hurt_by: Option<lodestone_model::Vec3>,
        nav_done: bool,
        nearby: Vec<super::super::mob::NearbyBrainEntity>,
        zombified: Option<lodestone_model::Vec3>,
    }

    impl PanicTestMob {
        fn new() -> Self {
            Self {
                pos: lodestone_model::Vec3::default(),
                time: 0,
                hurt_by: None,
                nav_done: true,
                nearby: Vec::new(),
                zombified: None,
            }
        }
    }

    impl super::super::mob::BrainMob for PanicTestMob {
        fn next_i32(&mut self, _bound: i32) -> i32 {
            0
        }
        fn next_f32(&mut self) -> f32 {
            0.5
        }
        fn game_time(&self) -> i64 {
            self.time
        }
        fn position(&self) -> lodestone_model::Vec3 {
            self.pos
        }
        fn move_to(&mut self, _target: lodestone_model::Vec3, _speed: f32) -> bool {
            self.nav_done = false;
            true
        }
        fn navigation_done(&self) -> bool {
            self.nav_done
        }
        fn stop_navigation(&mut self) {
            self.nav_done = true;
        }
        fn look_at(&mut self, _target: lodestone_model::Vec3) {}
        fn random_land_pos(&mut self, max_xz: i32, _max_y: i32) -> Option<lodestone_model::Vec3> {
            Some(lodestone_model::Vec3::new(self.pos.x + f64::from(max_xz), self.pos.y, self.pos.z))
        }
        fn last_hurt_by(&self) -> Option<lodestone_model::Vec3> {
            self.hurt_by
        }
        fn nearby_entities(&self) -> Vec<super::super::mob::NearbyBrainEntity> {
            self.nearby.clone()
        }
        fn nearest_visible_zombified(&self) -> Option<lodestone_model::Vec3> {
            self.zombified
        }
    }

    /// **The discriminating gate for this whole slice.** A hurt species from
    /// [`PANIC_SPEED_MULTIPLIER`] must actually run `panic` — not merely have
    /// a `HURT_BY`-shaped memory slot registered — while a brain species
    /// outside that table (the warden, which has no `AnimalPanic` in the jar
    /// at all) must never run it however hard it is hit. Ticked through
    /// [`brain_for`]/[`Brain::tick`], the same production path
    /// `MobSim::spawn_species` uses, so this measures the roster wiring
    /// itself rather than a `Panic` constructed by hand.
    #[test]
    fn a_hurt_panic_species_runs_panic_and_a_hurt_non_panic_species_never_does() {
        let mut goat = brain_for("goat").expect("goat is a brain species");
        let mut mob = PanicTestMob::new();
        mob.hurt_by = Some(lodestone_model::Vec3::new(5.0, 0.0, 0.0));
        mob.time = 1;
        goat.brain_mut().tick(&mut mob);
        assert!(
            goat.brain().running_behavior_names().contains(&"panic"),
            "a hurt goat did not run its panic behaviour: {:?}",
            goat.brain().running_behavior_names()
        );

        let mut warden = brain_for("warden").expect("warden is a brain species");
        let mut mob2 = PanicTestMob::new();
        mob2.hurt_by = Some(lodestone_model::Vec3::new(5.0, 0.0, 0.0));
        mob2.time = 1;
        warden.brain_mut().tick(&mut mob2);
        assert!(
            !warden.brain().running_behavior_names().contains(&"panic"),
            "the warden ran a panic behaviour it was never given: {:?}",
            warden.brain().running_behavior_names()
        );
    }

    /// The villager's Activity-swap panic, driven through the exact sequence
    /// [`super::driver::BrainGoal::tick`] runs each tick
    /// (`set_active_activity_to_first_valid` then `brain.tick`), since a plain
    /// `Brain::tick` alone — what the goat test above uses — never
    /// re-evaluates which non-core activity is active and so cannot show this
    /// species' mechanism working at all: its `Panic` lives inside
    /// `Activity::PANIC`, not `Activity::CORE`.
    ///
    /// Three arms: hurt alone is enough, a nearby hostile alone is enough
    /// (proving the OR, not just one disjunct), and neither leaves the
    /// villager idly stroll-and-looking with no panic running at all.
    ///
    /// Each arm drives the sequence **twice**: the first
    /// `set_active_activity_to_first_valid` + `tick` runs against whatever
    /// memory existed *before* this tick's sensors wrote anything (there is
    /// none yet, on tick one), and only the second call sees `HURT_BY`/
    /// `NEAREST_HOSTILE` as written by the first tick's sensor pass — the same
    /// one-tick lag `BrainGoal::tick`'s real production sequence has, and
    /// exactly why a plain single call is not enough for a species whose
    /// panic lives behind an activity switch rather than in `CORE`.
    #[test]
    fn a_villager_panics_via_activity_swap_when_hurt_or_a_hostile_is_near() {
        let candidates = [Activity::PANIC, Activity::IDLE];
        let drive = |brain: &mut Brain, mob: &mut PanicTestMob| {
            brain.set_active_activity_to_first_valid(&candidates);
            brain.tick(mob);
            mob.time += 1;
            brain.set_active_activity_to_first_valid(&candidates);
            brain.tick(mob);
        };

        let mut hurt_brain = villager_brain();
        let mut hurt_mob = PanicTestMob::new();
        hurt_mob.hurt_by = Some(lodestone_model::Vec3::new(5.0, 0.0, 0.0));
        hurt_mob.time = 1;
        drive(&mut hurt_brain, &mut hurt_mob);
        assert!(hurt_brain.is_active(Activity::PANIC), "HURT_BY alone must swap to PANIC");
        assert!(
            hurt_brain.running_behavior_names().contains(&"panic"),
            "and the panic behaviour itself must be running: {:?}",
            hurt_brain.running_behavior_names()
        );

        let mut hostile_brain = villager_brain();
        let mut hostile_mob = PanicTestMob::new();
        hostile_mob.time = 1;
        hostile_mob.nearby = vec![super::super::mob::NearbyBrainEntity {
            id: 42,
            position: lodestone_model::Vec3::new(3.0, 0.0, 0.0),
            hostile: true,
        }];
        drive(&mut hostile_brain, &mut hostile_mob);
        assert!(
            hostile_brain.is_active(Activity::PANIC),
            "a nearby hostile alone must swap to PANIC too, proving the OR"
        );

        let mut calm_brain = villager_brain();
        let mut calm_mob = PanicTestMob::new();
        calm_mob.time = 1;
        drive(&mut calm_brain, &mut calm_mob);
        assert!(calm_brain.is_active(Activity::IDLE), "neither condition must leave IDLE active");
        assert!(
            !calm_brain.running_behavior_names().contains(&"panic"),
            "and panic must not be running: {:?}",
            calm_brain.running_behavior_names()
        );
    }

    /// **The magnitude half**: each panic species carries its own jar
    /// constant, not one figure copy-pasted onto all six. Distinguishing
    /// camel's `4.0` from goat's `2.0` is what a presence-only check cannot
    /// do — a `Panic::new(2.0)` wired onto every species would pass every
    /// assertion above and still be wrong for camel, armadillo and allay.
    #[test]
    fn each_panic_species_carries_its_own_jar_speed_multiplier() {
        let lookup = |species: &str| {
            PANIC_SPEED_MULTIPLIER
                .iter()
                .find(|&&(s, _)| s == species)
                .map(|&(_, speed)| speed)
        };
        assert_eq!(lookup("camel"), Some(4.0), "Camel.CamelPanic(4.0F)");
        assert_eq!(lookup("allay"), Some(2.5), "Allay's AnimalPanic(2.5F)");
        assert_eq!(lookup("goat"), Some(2.0), "AnimalPanic(2.0F) in GoatAi");
        assert_eq!(lookup("frog"), Some(2.0), "AnimalPanic(2.0F) in FrogAi");
        assert_eq!(lookup("sniffer"), Some(2.0), "AnimalPanic<Sniffer>(2.0F) in SnifferAi");
        assert_eq!(lookup("armadillo"), Some(2.0), "ArmadilloAi.ArmadilloPanic(2.0F)");
        assert_eq!(lookup("axolotl"), None, "axolotl has no AnimalPanic in the jar at all");
        // The two must genuinely differ, or the presence check above could
        // pass with every species sharing one hardcoded constant.
        assert_ne!(lookup("camel"), lookup("goat"));
    }

    /// A [`BrainMob`](super::super::mob::BrainMob) double whose `move_to`
    /// actually relocates the mob (unlike [`PanicTestMob`], which only flags
    /// navigation in progress) — needed so [`PrepareRam`]/[`RamTarget`]'s own
    /// "did we arrive" checks can resolve `true` inside a hermetic test, and
    /// so it can record what [`super::super::mob::BrainMob::attack`] was
    /// called with.
    struct RamTestMob {
        pos: lodestone_model::Vec3,
        time: i64,
        nearby: Vec<super::super::mob::NearbyBrainEntity>,
        attacks: Vec<lodestone_model::Vec3>,
    }

    impl super::super::mob::BrainMob for RamTestMob {
        fn next_i32(&mut self, bound: i32) -> i32 {
            bound.saturating_sub(1).max(0)
        }
        fn next_f32(&mut self) -> f32 {
            0.5
        }
        fn game_time(&self) -> i64 {
            self.time
        }
        fn position(&self) -> lodestone_model::Vec3 {
            self.pos
        }
        fn move_to(&mut self, target: lodestone_model::Vec3, _speed: f32) -> bool {
            self.pos = target;
            true
        }
        fn navigation_done(&self) -> bool {
            true
        }
        fn stop_navigation(&mut self) {}
        fn look_at(&mut self, _target: lodestone_model::Vec3) {}
        fn random_land_pos(&mut self, max_xz: i32, _max_y: i32) -> Option<lodestone_model::Vec3> {
            Some(lodestone_model::Vec3::new(self.pos.x + f64::from(max_xz), self.pos.y, self.pos.z))
        }
        fn nearby_entities(&self) -> Vec<super::super::mob::NearbyBrainEntity> {
            self.nearby.clone()
        }
        fn attack(&mut self, target: lodestone_model::Vec3) {
            self.attacks.push(target);
        }
    }

    /// End-to-end through [`brain_for("goat")`], the exact production path
    /// `MobSim::spawn_species` uses (per [`goals_for`](crate::ai::roster::goals_for)'s
    /// own doc: "no host learns a new call"): a goat with a living entity 5
    /// blocks away must back away, prepare, charge, and land a
    /// [`BrainMob::attack`] on it — proving the whole ram pair is wired, not
    /// merely unit-correct in isolation. Driven with the villager test's own
    /// `set_active_activity_to_first_valid` + `tick` sequence, since `RAM`
    /// (like `PANIC` there) is a non-core activity `BrainGoal::tick` — not a
    /// plain `Brain::tick` — re-evaluates every call.
    #[test]
    fn a_goat_with_a_nearby_target_charges_and_lands_a_hit() {
        let candidates = [Activity::RAM, Activity::IDLE];
        let mut goat = brain_for("goat").expect("goat is a brain species");
        let target_id = 77;
        let mut mob = RamTestMob {
            pos: lodestone_model::Vec3::new(0.0, 0.0, 0.0),
            time: 0,
            nearby: vec![super::super::mob::NearbyBrainEntity {
                id: target_id,
                position: lodestone_model::Vec3::new(5.0, 0.0, 0.0),
                hostile: false,
            }],
            attacks: Vec::new(),
        };

        // Generous: prepare (20-tick wait once arrived, plus a few ticks of
        // one-tick lag between a producer and the CORE sink that consumes it)
        // plus the charge itself, well inside RamTarget's own 200-tick cap.
        for _ in 0..80 {
            goat.brain_mut()
                .set_active_activity_to_first_valid(&candidates);
            goat.brain_mut().tick(&mut mob);
            mob.time += 1;
            if !mob.attacks.is_empty() {
                break;
            }
        }

        assert_eq!(
            mob.attacks,
            vec![lodestone_model::Vec3::new(5.0, 0.0, 0.0)],
            "the goat must land exactly one attack on the target's position; got {:?}",
            mob.attacks
        );
    }

    /// The negative control: no nearby entity means `RAM` never has anything
    /// to do, so the goat must fall back to `IDLE` and never call
    /// [`BrainMob::attack`] — a goat alone in the world must not spontaneously
    /// ram nothing.
    #[test]
    fn a_goat_with_nothing_nearby_never_attacks_and_falls_back_to_idle() {
        let candidates = [Activity::RAM, Activity::IDLE];
        let mut goat = brain_for("goat").expect("goat is a brain species");
        let mut mob = RamTestMob {
            pos: lodestone_model::Vec3::new(0.0, 0.0, 0.0),
            time: 0,
            nearby: Vec::new(),
            attacks: Vec::new(),
        };

        for _ in 0..40 {
            goat.brain_mut()
                .set_active_activity_to_first_valid(&candidates);
            goat.brain_mut().tick(&mut mob);
            mob.time += 1;
        }

        assert!(mob.attacks.is_empty(), "an empty world must never produce a ram attack");
        assert!(
            goat.brain().is_active(Activity::IDLE),
            "with nothing to ram, the goat must fall back to IDLE, not freeze in RAM"
        );
    }

    /// A piglin with a nearby visible zombified piglin swaps to `AVOID` and
    /// actually starts walking away from it — driven through
    /// `brain_for("piglin")`, the exact production path, the same shape the
    /// villager panic test above uses for an activity-swap species. A piglin
    /// with no zombified piglin nearby stays `IDLE`.
    #[test]
    fn a_piglin_near_a_zombified_piglin_avoids_it_and_otherwise_stays_idle() {
        let candidates = [Activity::AVOID, Activity::IDLE];

        let mut fleeing_piglin = brain_for("piglin").expect("piglin is a brain species");
        let mut fleeing_mob = PanicTestMob::new();
        fleeing_mob.zombified = Some(lodestone_model::Vec3::new(3.0, 0.0, 0.0));
        fleeing_mob.time = 1;
        // Two ticks: the first CORE-only pass writes `AVOID_TARGET` from the
        // sensor's first read, the second is what lets `AVOID` actually
        // become eligible — the same one-tick lag the villager test above
        // documents for an activity-swap species.
        fleeing_piglin.brain_mut().set_active_activity_to_first_valid(&candidates);
        fleeing_piglin.brain_mut().tick(&mut fleeing_mob);
        fleeing_mob.time += 1;
        fleeing_piglin.brain_mut().set_active_activity_to_first_valid(&candidates);
        fleeing_piglin.brain_mut().tick(&mut fleeing_mob);
        assert!(
            fleeing_piglin.brain().is_active(Activity::AVOID),
            "a piglin near a zombified piglin must swap to AVOID"
        );
        assert!(
            fleeing_piglin.brain().running_behavior_names().contains(&"avoid_target"),
            "and the flee behaviour itself must be running: {:?}",
            fleeing_piglin.brain().running_behavior_names()
        );

        let mut calm_piglin = brain_for("piglin").expect("piglin is a brain species");
        let mut calm_mob = PanicTestMob::new();
        calm_mob.time = 1;
        calm_piglin.brain_mut().set_active_activity_to_first_valid(&candidates);
        calm_piglin.brain_mut().tick(&mut calm_mob);
        calm_mob.time += 1;
        calm_piglin.brain_mut().set_active_activity_to_first_valid(&candidates);
        calm_piglin.brain_mut().tick(&mut calm_mob);
        assert!(
            calm_piglin.brain().is_active(Activity::IDLE),
            "with no zombified piglin visible, a piglin must stay IDLE"
        );
        assert!(
            !calm_piglin.brain().running_behavior_names().contains(&"avoid_target"),
            "and must not be fleeing anything: {:?}",
            calm_piglin.brain().running_behavior_names()
        );
    }

    /// `piglin_brute` must never pick up the piglin's `AVOID` package —
    /// `PiglinBruteAi.updateActivity` offers only `[FIGHT, IDLE]` in the jar,
    /// so a brute is too brave to flee a zombified piglin the way an
    /// ordinary piglin does.
    #[test]
    fn piglin_brute_does_not_get_the_piglin_avoid_package() {
        let candidates = [Activity::AVOID, Activity::IDLE];
        let mut brute = brain_for("piglin_brute").expect("piglin_brute is a brain species");
        let mut mob = PanicTestMob::new();
        mob.zombified = Some(lodestone_model::Vec3::new(3.0, 0.0, 0.0));
        mob.time = 1;
        brute.brain_mut().set_active_activity_to_first_valid(&candidates);
        brute.brain_mut().tick(&mut mob);
        mob.time += 1;
        brute.brain_mut().set_active_activity_to_first_valid(&candidates);
        brute.brain_mut().tick(&mut mob);
        assert!(
            !brute.brain().is_active(Activity::AVOID),
            "a piglin_brute must never swap into AVOID, even near a zombified piglin"
        );
    }
}
