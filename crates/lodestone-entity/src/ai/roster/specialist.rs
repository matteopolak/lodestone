//! Goal sets for the mobs whose attack is neither melee nor a projectile:
//! guardian, elder guardian, ghast.
//!
//! # What it is
//!
//! Issue [#232]'s family. Its reason to exist is the **guardian's beam**, which
//! is a third attack shape: no projectile entity, no contact — a charge-up on a
//! tick counter, then damage when the counter reaches the species' own
//! `getAttackDuration()`. It needs neither [`MeleeAttackGoal`] nor issue [#227]'s
//! launch path, so this family does not wait on either.
//!
//! [`GuardianBeamGoal`] is the one new goal type, and it lives **here** rather
//! than in [`goals`](crate::ai::goals). That is deliberate: `goals.rs` is shared
//! by all five roster families running in parallel, and nothing about
//! [`Coverage::Modelled`](super::Coverage::Modelled) requires a `Goal` impl to
//! live in one module — it takes a `fn(&SpeciesContext) -> Box<dyn Goal>` and does
//! not care where the type came from. A family-local goal is the seam working as
//! intended. Promote it to `goals.rs` only if a second family needs it.
//!
//! # How to change it
//!
//! Add each species path to [`SPECIES`] and an arm to [`lookup`]; see
//! [`super::hostile_melee`] for the shape and the citation discipline. Check each
//! species' **own** `registerGoals` — see the elder guardian below for why
//! "extends X and adds no `registerGoals`" is not the same as "behaves like X".
//!
//! # Two things this family deliberately does not contain
//!
//! ## The warden is not a `GoalSelector` mob at all, and it is not this unit's
//!
//! `monster/warden/Warden.java` declares **no `registerGoals` and no `addGoal`,
//! anywhere in the file**. It is `makeBrain`/`BRAIN_PROVIDER`
//! (`Warden.java:373-379` → `monster/warden/WardenAi.java`), so a warden's
//! behaviour lives in the Brain driver (issue #209) and a warden table in this
//! roster would be an empty table that lied about being one. There is nothing to
//! transcribe.
//!
//! Its vibration sensing is a second, larger reason, and it is a **subsystem, not
//! a goal**: `Warden implements VibrationSystem` (`Warden.java:76`) and owns a
//! `DynamicGameEventListener<VibrationSystem.Listener>` (`:117-126`) ticked by
//! `VibrationSystem.Ticker.tick(serverLevel, …)` (`:249`), filtered by the
//! `GameEventTags.WARDEN_CAN_LISTEN` tag (`:585-586`) through
//! `canReceiveVibration` (`:595`). That is `level/gameevent/` — `GameEvent`,
//! `GameEventDispatcher`, `GameEventListenerRegistry`,
//! `EuclideanGameEventListenerRegistry`, `PositionSource` and the whole
//! `gameevent/vibrations/` package — a level-wide event bus with per-listener
//! radii and occlusion.
//!
//! **None of it exists here, and the name `GameEvent` is already taken twice by
//! unrelated things**, which is the trap: `lodestone_ecs::GameEvent`
//! (`lodestone-ecs/src/events.rs:61`) is the *client-side plugin* event bus, and
//! `GAME_EVENT` (`protocol/v770/src/generated/packet_ids.rs:195`) is vanilla's
//! `ClientboundGameEventPacket` (weather and win-game codes). Neither has
//! anything to do with vibrations. Grepping `GameEvent` and concluding "we have
//! one" is the mistake available here.
//!
//! Two further blockers, so this is costed rather than merely deferred:
//! [`MobController`] exposes **no block or world access at all**, so a
//! positional-event source has to arrive through the server's own
//! `feed_perception` census (`lodestone-server/src/mobs.rs:1666`) plus a new
//! accessor, exactly as issue #441 did for the existing eight perception methods;
//! and `crates/lodestone-entity/src/brain/` — which already has the `Sensor` trait
//! a vibration sensor would implement (`brain/sensor.rs:13`) — has **no
//! production caller**. So the warden needs the Brain driver wired *and* a
//! vibration substrate built. That is its own unit.
//!
//! ## Anything that reduces to "launch a projectile on an interval" is [#227]'s
//!
//! The ghast's fireball is exactly that shape — `chargeTime == 20` then a
//! `LargeFireball`, resetting to `-40` (`monster/Ghast.java:360-381`) — so
//! [`GHAST`]'s row for it is [`Coverage::Missing`](super::Coverage::Missing)
//! rather than a second, competing implementation of #227's goal. The drowned's
//! trident is the same call and **already** lives in
//! [`super::hostile_melee::DROWNED`] as a `Missing` row at goal-priority 2
//! (`Drowned.DrownedTridentAttackGoal`); this file must not duplicate it.
//!
//! # Still unclaimed by this family, and why
//!
//! Recorded here so the next agent does not have to re-derive it:
//!
//! * **`shulker`** (`monster/Shulker.java:95-103`) — four of its seven rows are
//!   its own nested classes (`ShulkerAttackGoal` fires a `ShulkerBullet`, so
//!   #227's; plus `ShulkerPeekGoal`, `ShulkerNearestAttackGoal`,
//!   `ShulkerDefenseAttackGoal`), and peeking is a block-state/attachment
//!   mechanic with no seam here.
//! * **`vex`** (`monster/Vex.java:86-96`) and **`ravager`**
//!   (`monster/Ravager.java:74-85`) both call **`super.registerGoals()` as their
//!   first statement**, so their real tables are not the lines you can see.
//!   `Ravager extends Raider extends PatrollingMonster`, which drags in the raid
//!   goals; `Vex.VexCopyOwnerTargetGoal` needs an owner relation that does not
//!   exist. Transcribing either means transcribing its whole ancestry, and
//!   getting that wrong silently is precisely what the multiset gate is supposed
//!   to catch — so neither is guessed at here.
//!
//! [#232]: https://github.com/matteopolak/lodestone/issues/232
//! [#227]: https://github.com/matteopolak/lodestone/issues/227
//! [`MeleeAttackGoal`]: crate::ai::goals::MeleeAttackGoal

use crate::ai::goal::{Flag, FlagSet, Goal};
use crate::ai::mob::{MobController, distance_sqr};

use super::{
    Registration, Selector, SpeciesContext, look_at_player_8, nearest_attackable_target,
    random_look_around, stroll,
};

/// Every species this family claims. Iterated by `roster`'s invariant gates.
pub const SPECIES: &[&str] = &["guardian", "elder_guardian", "ghast"];

/// Resolves a species path to its table, or `None` if this family does not claim
/// it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        "guardian" => Some(GUARDIAN),
        // `ElderGuardian` declares **no `registerGoals`** (`monster/ElderGuardian.java`
        // has no such method), so its transcription is byte-for-byte the
        // guardian's. It still gets its own table, because it overrides
        // `getAttackDuration()` to 60 (`ElderGuardian.java:39-41`) — the rows are
        // identical and the behaviour is not. Sharing `GUARDIAN`'s pointer the way
        // `hostile_melee` shares one table between a zombie and a husk would
        // silently give an elder the guardian's 80-tick charge.
        "elder_guardian" => Some(ELDER_GUARDIAN),
        "ghast" => Some(GHAST),
        _ => None,
    }
}

// -- the beam ----------------------------------------------------------------

/// The guardian's charge-then-zap attack: `Guardian.GuardianAttackGoal`
/// (`monster/Guardian.java:341-419`).
///
/// # Why it is not a ranged goal
///
/// There is no projectile. The goal holds a tick counter, and when the counter
/// reaches the guardian's `getAttackDuration()` the target is hurt directly,
/// wherever it is. Nothing is spawned, nothing travels, and the mob does not close
/// the distance — `start` and every `tick` **stop** the navigation
/// (`Guardian.java:366`, `:390`). So it shares no machinery with issue #227's
/// launch path and none with `MeleeAttackGoal`'s reach check.
///
/// # The timing, which is the whole behaviour
///
/// Two jar facts combine, and missing either gives a plausible wrong answer:
///
/// * `start()` sets `attackTime = -10` (`Guardian.java:365`) — a lead-in, not a
///   zero.
/// * `tick()` increments **first** and then tests
///   `attackTime >= getAttackDuration()` (`:396`, `:402`).
///
/// So damage lands on the `duration + 10`-th tick the goal runs, not the
/// `duration`-th: **90** for a guardian (`getAttackDuration()` → 80,
/// `Guardian.java:109-111`, matching `ATTACK_TIME = 80` at `:48`) and **70** for
/// an elder guardian (→ 60, `ElderGuardian.java:39-41`). At `attackTime == 0` —
/// the 10th tick — vanilla flips `DATA_ID_ATTACK_TARGET` and broadcasts entity
/// event 21 (`:397-401`), which is what makes the beam *visible*; see "not
/// modelled" below.
///
/// # How to change it, and the gotcha
///
/// [`attack_duration`](Self::attack_duration) is the only per-species number, so a
/// new guardian variant is one more constructor. **Do not reach for
/// `SpeciesContext`** to carry it: that struct is shared by all five families and
/// adding a field to it is a shared edit. Two `fn` builders are the whole cost —
/// see [`guardian_beam`] and [`elder_guardian_beam`].
///
/// # Not modelled, each with what it would need
///
/// * **The visible beam.** `setActiveAttackTarget` writes the
///   `DATA_ID_ATTACK_TARGET` entity-metadata field (`Guardian.java:113-115`) and
///   `getAttackAnimationScale` (`:289`) drives the render. [`MobController`] has
///   no metadata or entity-event seam, so this goal deals damage a player would
///   feel and draws no laser. Wiring it needs a metadata index from
///   `protocol/v770/oracle-java/EntityDataIndexOracle.java` — never hand-counted —
///   plus a renderer.
/// * **The 1.0 magic-damage component.** Vanilla hurts the target *twice*:
///   `indirectMagic` for `magicDamage` (1.0, +2.0 on Hard, +2.0 for an elder) and
///   then `doHurtTarget` for the ordinary attack-damage hit (`:403-408`).
///   [`MobController::attack`] is the single melee verb, so ours is the
///   `doHurtTarget` half only. There is no difficulty concept here either.
/// * **Line of sight.** Vanilla drops the target when `!hasLineOfSight`
///   (`:393-394`). This seam has no raycast primitive — the same disclosed
///   simplification `SwellGoal`'s own doc comment already makes.
/// * **`randomStrollGoal.trigger()` on stop** (`:379`), which has no seam.
#[derive(Debug)]
pub struct GuardianBeamGoal {
    /// The species' `getAttackDuration()`: 80 for a guardian, 60 for an elder.
    attack_duration: i32,
    /// Vanilla's `attackTime` field (`Guardian.java:343`). Starts negative.
    attack_time: i32,
    /// Vanilla's `elder` flag (`Guardian.java:344`, set at `:348`). An elder keeps
    /// beaming a target that has closed inside 3 blocks; an ordinary guardian
    /// gives up (`:361`).
    elder: bool,
}

impl GuardianBeamGoal {
    /// `start()`'s lead-in, before the charge counter reaches zero
    /// (`monster/Guardian.java:365`).
    const CHARGE_LEAD_IN: i32 = -10;

    /// An ordinary guardian's `getAttackDuration()`
    /// (`monster/Guardian.java:109-111`; the `ATTACK_TIME = 80` constant at
    /// `:48`).
    const GUARDIAN_DURATION: i32 = 80;

    /// An elder guardian's overridden `getAttackDuration()`
    /// (`monster/ElderGuardian.java:39-41`).
    const ELDER_DURATION: i32 = 60;

    /// Vanilla's squared give-up distance for a non-elder
    /// (`monster/Guardian.java:361`, `distanceToSqr(target) > 9.0` — 3 blocks).
    /// The same 9.0 appears in `GuardianAttackSelector` (`:430`), which is why a
    /// guardian never beams something in its face.
    const MIN_RANGE_SQR: f64 = 9.0;

    /// An ordinary guardian's beam.
    #[must_use]
    pub fn guardian() -> Self {
        Self {
            attack_duration: Self::GUARDIAN_DURATION,
            attack_time: Self::CHARGE_LEAD_IN,
            elder: false,
        }
    }

    /// An elder guardian's beam: shorter charge, and no give-up range.
    #[must_use]
    pub fn elder() -> Self {
        Self {
            attack_duration: Self::ELDER_DURATION,
            attack_time: Self::CHARGE_LEAD_IN,
            elder: true,
        }
    }

    /// The tick, counted from the first [`Goal::tick`] after [`Goal::start`], on
    /// which this beam deals damage.
    ///
    /// Exposed because it is the number a gate must predict from the jar rather
    /// than read back off the implementation — `duration + 10`, not `duration`.
    #[must_use]
    pub const fn damage_tick(&self) -> i32 {
        self.attack_duration - Self::CHARGE_LEAD_IN
    }
}

impl Goal for GuardianBeamGoal {
    /// Vanilla sets `EnumSet.of(Goal.Flag.MOVE, Goal.Flag.LOOK)`
    /// (`monster/Guardian.java:349`), and this is transcribed exactly rather than
    /// narrowed to `{MOVE}` the way `MeleeAttackGoal` is.
    ///
    /// The narrowing in `MeleeAttackGoal` is a pre-existing, deliberately
    /// conservative deviation, and the reason it stays is that changing a shared
    /// goal's flag set reschedules every species at once. Neither applies here:
    /// this goal is new and has exactly two registrations, both in this file. And
    /// LOOK is load-bearing for the guardian specifically — vanilla's beam at
    /// priority 4 is *meant* to hold LOOK against the two `LookAtPlayerGoal`
    /// registrations at 8 and `RandomLookAroundGoal` at 9, so a charging guardian
    /// stares at its victim instead of glancing away.
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    /// `canUse` (`monster/Guardian.java:353-356`): a target exists and is alive.
    /// This seam's target is a bare [`Vec3`], so there is no liveness to check.
    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_some()
    }

    /// `canContinueToUse` (`monster/Guardian.java:359-362`): `super` (which is
    /// `canUse`) **and** — for a non-elder only — the target still being further
    /// than 3 blocks away.
    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(target) = mob.attack_target() else {
            return false;
        };
        self.elder || distance_sqr(target, mob.position()) > Self::MIN_RANGE_SQR
    }

    /// `start` (`monster/Guardian.java:364-372`): reset the counter to the
    /// lead-in, stop navigating, and lock the look onto the target.
    fn start(&mut self, mob: &mut dyn MobController) {
        self.attack_time = Self::CHARGE_LEAD_IN;
        mob.stop_navigation();
        if let Some(target) = mob.attack_target() {
            mob.look_at(target);
        }
    }

    /// `stop` (`monster/Guardian.java:375-380`): clear the synced beam target and
    /// the attack target. Ours has only the latter.
    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(None);
    }

    /// `tick` (`monster/Guardian.java:387-417`), in vanilla's order: hold still,
    /// keep looking, **then** increment, **then** test. Damage clears the target,
    /// so one acquisition buys exactly one beam.
    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = mob.attack_target() else {
            return;
        };
        mob.stop_navigation();
        mob.look_at(target);
        self.attack_time += 1;
        if self.attack_time >= self.attack_duration {
            mob.attack(target);
            mob.set_attack_target(None);
        }
    }
}

/// `Guardian.GuardianAttackGoal(this)` on an ordinary guardian
/// (`monster/Guardian.java:74`). Takes no speed — the goal never moves the mob.
pub fn guardian_beam(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(GuardianBeamGoal::guardian())
}

/// The same registration on an elder guardian, whose `getAttackDuration()` is 60
/// (`monster/ElderGuardian.java:39-41`).
///
/// A separate builder rather than a parameter because a [`Registration`] table is
/// a `const` and `build` must be a plain `fn` item — a closure capturing the
/// duration is not a function pointer.
pub fn elder_guardian_beam(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(GuardianBeamGoal::elder())
}

// -- tables ------------------------------------------------------------------

/// `monster/Guardian.java:71-83`.
///
/// `registerGoals` does **not** call `super`, and `Monster` declares no
/// `registerGoals` at all, so these seven rows are the guardian's entire table —
/// checked, not assumed, because two species in this family (`vex`, `ravager`) do
/// call `super` and are excluded for exactly that reason.
///
/// Three rows are not modelled or are narrowed, and none of them is the beam:
///
/// * **`MoveTowardsRestrictionGoal`** at 5 (`:72`, `:75`) walks a mob back inside
///   its `restrictTo` home radius. Nothing here has a home position, so there is
///   no seam to approximate — [`Coverage::Missing`](super::Coverage::Missing).
/// * **The second `LookAtPlayerGoal`** at 8 (`:78`) targets `Guardian.class` at
///   12.0 blocks with a 0.01 probability — guardians eyeing each other. Ours takes
///   no target class and resolves through
///   [`MobController::nearest_player`], so it is `Missing`, **not** `CoveredBy`.
///   That is a different call from the creeper's two `AvoidEntityGoal` rows, which
///   collapse into one because the server's `avoided_species` feed already
///   resolves *both* classes into the one perception method. There is no
///   equivalent feed making `nearest_player` return a guardian, so installing a
///   second instance of our goal would duplicate the `Player` row rather than add
///   this one.
/// * **The target row** at 1 (`:82`) is `LivingEntity.class` filtered by
///   `GuardianAttackSelector` — `Player`, `Squid` or `Axolotl`, further than 3
///   blocks (`:424-434`). Ours resolves to the nearest player, which is the
///   selector's first case; the squid and axolotl cases and the 3-block floor are
///   the disclosed narrowing every `nearest_attackable_target` row in this roster
///   shares.
///
/// One row is modelled with a deviation worth naming: vanilla's stroll is
/// `RandomStrollGoal(this, 1.0, 80)` with `setFlags(MOVE, LOOK)` applied at `:80`.
/// Ours takes no interval (so the 80-tick — and the elder's 400-tick,
/// `ElderGuardian.java:30-32` — pause between wanders is not modelled) and claims
/// `{MOVE}` only. The flag half is the same class of conservative deviation
/// `MeleeAttackGoal` already carries and is left alone for the same reason; its
/// one visible effect is that our guardian may glance around while strolling,
/// where vanilla's cannot.
pub const GUARDIAN: &[Registration] = &[
    Registration::goal(4, "Guardian.GuardianAttackGoal", guardian_beam),
    Registration::missing(Selector::Goal, 5, "MoveTowardsRestrictionGoal"),
    Registration::goal(7, "RandomStrollGoal", stroll),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::missing(Selector::Goal, 8, "LookAtPlayerGoal(Guardian)"),
    Registration::goal(9, "RandomLookAroundGoal", random_look_around),
    Registration::target(
        1,
        "NearestAttackableTargetGoal(LivingEntity)",
        nearest_attackable_target,
    ),
];

/// `monster/Guardian.java:71-83`, inherited verbatim by `ElderGuardian`.
///
/// **The rows are identical to [`GUARDIAN`]'s and the table is still separate.**
/// `ElderGuardian` declares no `registerGoals`, so there is nothing to transcribe
/// differently — but it overrides `getAttackDuration()` to 60
/// (`ElderGuardian.java:39-41`), so its beam charges in 70 ticks where a
/// guardian's takes 90, and `GuardianAttackGoal`'s own `elder` flag
/// (`Guardian.java:348`) also removes its 3-block give-up range.
///
/// This is the shape of trap the "check each species' own `registerGoals`" rule is
/// really about. The usual form is a subclass that *adds* a row —
/// `WitherSkeleton` overriding and calling `super`. This is the inverse: the
/// override is nowhere near `registerGoals`, so a multiset gate comparing tables
/// sees two identical, correct transcriptions and cannot fail. Only a gate that
/// predicts the *tick count* can tell these two species apart, which is what
/// `the_beam_lands_on_vanillas_ninetieth_tick_and_the_elders_on_its_seventieth`
/// is for.
pub const ELDER_GUARDIAN: &[Registration] = &[
    Registration::goal(4, "Guardian.GuardianAttackGoal", elder_guardian_beam),
    Registration::missing(Selector::Goal, 5, "MoveTowardsRestrictionGoal"),
    Registration::goal(7, "RandomStrollGoal", stroll),
    Registration::goal(8, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::missing(Selector::Goal, 8, "LookAtPlayerGoal(Guardian)"),
    Registration::goal(9, "RandomLookAroundGoal", random_look_around),
    Registration::target(
        1,
        "NearestAttackableTargetGoal(LivingEntity)",
        nearest_attackable_target,
    ),
];

/// `monster/Ghast.java:56-62`.
///
/// **Three of four rows are `Missing`, and the table exists anyway.** That is a
/// deliberate trade, so read this before "fixing" it:
///
/// Without an entry, `ghast` falls to [`FALLBACK`](super::FALLBACK) and a ghast
/// **walks around on the ground looking for something to look at**. With this
/// entry it acquires a target and holds still. Neither is vanilla, but only one of
/// them is a lie about what a ghast is, and only one of them records *why* the
/// ghast is unreachable at the place the next agent will look. Losing the
/// fallback's stroll is the price and it is worth paying: a ghast is a flying mob
/// and ground strolling is not a degraded version of flying, it is a different
/// animal.
///
/// Nothing can spawn a ghast today in any case — `seed_demo_mobs`
/// (`lodestone-server/src/mobs.rs:2573`) creates zombies and nothing else — so
/// this changes no observable behaviour in the running game. It changes what issue
/// [#227] has to do: one row.
///
/// * **`Ghast.RandomFloatAroundGoal`** at 5 (`:57`) and **`Ghast.GhastLookGoal`**
///   at 7 (`:58`) both drive `GhastMoveControl` (`:228`), a free-flight controller
///   with no pathfinding. `NavigatingMob` is ground-based A\*; there is no flying
///   navigation seam at all, so these are not approximations waiting on a
///   constant, they are waiting on a navigator.
/// * **`Ghast.GhastShootFireballGoal`** at 7 (`:59`) is #227's: charge to 20
///   ticks, launch a `LargeFireball`, reset to `-40` (`:360-381`). The explosion
///   half of that is already real (`MobSim::explode`, and `encode_explode` in
///   `protocol/v770/src/server_protocol.rs`), and issue #227 has since built the
///   launch seam — [`MobController::launch_projectile`] and
///   [`ProjectileKind`](crate::ai::mob::ProjectileKind), plus a generic
///   `RangedAttackGoal` in [`super::ranged`]. This row still stays `Missing`, for
///   a reason that is now specific rather than general: `ProjectileKind` has
///   **no `LargeFireball` variant**, and adding one is an edit to the shared
///   `ai/mob.rs`. So the remaining work is a variant plus a ghast-shaped goal
///   (its charge is 20 up / 40 down, not the generic interval), and it belongs
///   with the rest of #227 rather than being a second, competing ranged
///   implementation started here.
/// * **The target row** at 1 (`:60-61`) is `Player.class` with a ±4-block vertical
///   band, which ours does not model.
pub const GHAST: &[Registration] = &[
    Registration::missing(Selector::Goal, 5, "Ghast.RandomFloatAroundGoal"),
    Registration::missing(Selector::Goal, 7, "Ghast.GhastLookGoal"),
    Registration::missing(Selector::Goal, 7, "Ghast.GhastShootFireballGoal"),
    Registration::target(
        1,
        "NearestAttackableTargetGoal(Player)",
        nearest_attackable_target,
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lodestone_model::Vec3;

    use super::super::{goals_for, is_fallback, registrations_for};
    use super::*;
    use crate::ai::goal::GoalSelector;
    use crate::ai::navigating_mob::NavigatingMob;
    use crate::pathfinding::{Aabb, MobShape, PathType, PathWorld};

    /// One transcribed `addGoal` row, as the multiset gate compares them.
    type Row = (Selector, i32, &'static str);

    /// Every table in this family against a hand transcription of its cited
    /// `addGoal` block — including the rows this repo does not implement, so an
    /// omission cannot go quiet.
    ///
    /// The expected values here originate in the jar, not in the tables above;
    /// copying them from `GUARDIAN` would be satisfied by any self-consistent
    /// mistake, which is the closed loop CLAUDE.md's evidence standards are about.
    #[test]
    fn every_table_matches_the_jars_addgoal_block() {
        let guardian_rows: &[Row] = &[
            (Selector::Goal, 4, "Guardian.GuardianAttackGoal"),
            (Selector::Goal, 5, "MoveTowardsRestrictionGoal"),
            (Selector::Goal, 7, "RandomStrollGoal"),
            (Selector::Goal, 8, "LookAtPlayerGoal(Player)"),
            (Selector::Goal, 8, "LookAtPlayerGoal(Guardian)"),
            (Selector::Goal, 9, "RandomLookAroundGoal"),
            (
                Selector::Target,
                1,
                "NearestAttackableTargetGoal(LivingEntity)",
            ),
        ];

        let cases: &[(&str, &str, &[Row])] = &[
            ("guardian", "monster/Guardian.java:74-82", guardian_rows),
            // `ElderGuardian` declares no `registerGoals`, so its expected rows
            // are the guardian's *same* cited lines. The difference between the
            // two species is `getAttackDuration()`, which no multiset can see.
            (
                "elder_guardian",
                "monster/Guardian.java:74-82, inherited",
                guardian_rows,
            ),
            (
                "ghast",
                "monster/Ghast.java:57-61",
                &[
                    (Selector::Goal, 5, "Ghast.RandomFloatAroundGoal"),
                    (Selector::Goal, 7, "Ghast.GhastLookGoal"),
                    (Selector::Goal, 7, "Ghast.GhastShootFireballGoal"),
                    (Selector::Target, 1, "NearestAttackableTargetGoal(Player)"),
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

    /// The two guardian tables must be **separate allocations** even though their
    /// rows are equal.
    ///
    /// `hostile_melee` deliberately shares one table pointer between a zombie and
    /// a husk, and that is correct there. Doing it here would be a bug the
    /// multiset gate above cannot see, because the rows really are identical: the
    /// elder's shorter charge lives in the build function, not the row.
    #[test]
    fn the_elder_guardian_has_its_own_table_despite_identical_rows() {
        let guardian = registrations_for("guardian");
        let elder = registrations_for("elder_guardian");

        let rows = |t: &'static [Registration]| -> Vec<Row> {
            t.iter()
                .map(|r| (r.selector, r.priority, r.vanilla))
                .collect()
        };
        assert_eq!(
            rows(guardian),
            rows(elder),
            "precondition: ElderGuardian declares no registerGoals, so the rows \
             must be identical — if this fails, one of the two transcriptions \
             drifted"
        );
        assert!(
            !std::ptr::eq(guardian.as_ptr(), elder.as_ptr()),
            "an elder guardian must not share the guardian's table: the shared \
             pointer would give it the guardian's 80-tick charge instead of \
             ElderGuardian.java:39-41's 60"
        );
    }

    // -- behaviour, through the production controller -------------------------

    /// Flat ground below `y = 0`, so a real [`NavigatingMob`] has somewhere to
    /// stand. The subject under test is the roster and the beam, not the
    /// pathfinder.
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

    /// A guardian's `MOVEMENT_SPEED` (`monster/Guardian.java:88`). Passed in
    /// explicitly because `lodestone_entity::attribute::type_spec` has **no
    /// guardian arm**, so the running game would build this context with `0.0`;
    /// see this unit's report.
    const GUARDIAN_SPEED: f64 = 0.5;
    /// An elder guardian's (`monster/ElderGuardian.java:35`).
    const ELDER_SPEED: f64 = 0.3;

    /// Builds a real [`NavigatingMob`] over [`Flat`] and fills a [`GoalSelector`]
    /// from **[`goals_for`] alone**.
    ///
    /// **No `add` call of its own.** A gate that installs the goal it then observes
    /// cannot tell whether the *roster* installed it — the closed loop that hid
    /// issue #441's island. And nothing here touches `ScriptMob`, whose permissive
    /// perception is what let that island's `can_use` be constant-false in
    /// production while every unit test stayed green.
    fn rostered<'w>(
        world: &'w Flat,
        species: &str,
        speed: f64,
    ) -> (NavigatingMob<'w>, GoalSelector) {
        let ctx = SpeciesContext::new(speed);
        let mob = NavigatingMob::new(
            world,
            // A guardian is 0.85 × 0.85 (`EntityTypes.GUARDIAN`).
            MobShape::land(0.85, 0.85),
            Vec3::new(0.5, 0.0, 0.5),
            speed,
            160,
            0,
        );
        let mut ai = GoalSelector::new();
        for (priority, goal) in goals_for(species, &ctx) {
            ai.add(priority, goal);
        }
        (mob, ai)
    }

    /// Ticks a rostered mob with an attack target at `at`, returning the 1-based
    /// tick index of every recorded attack.
    ///
    /// The target is set explicitly because nothing in production sets one:
    /// `NavigatingMob::find_nearest_target` returns `self.attack_target`
    /// (`ai/navigating_mob.rs:904`) and `set_attack_target` has no production
    /// caller. That perception island is documented in
    /// [`super::super::hostile_melee`]'s header and is not in this file's reach.
    fn beam_ticks(species: &str, speed: f64, at: Vec3, ticks: usize) -> Vec<usize> {
        let world = Flat::new();
        let (mut mob, mut ai) = rostered(&world, species, speed);
        mob.set_attack_target(Some(at));

        let mut at_ticks = Vec::new();
        for tick in 1..=ticks {
            mob.tick(&mut ai);
            if !mob.take_new_attacks().is_empty() {
                at_ticks.push(tick);
            }
        }
        at_ticks
    }

    /// **The magnitude gate.** A guardian's beam deals damage on the **90th** tick
    /// and an elder guardian's on the **70th**, both predicted from jar constants
    /// before either was measured.
    ///
    /// Asserting that the beam "starts charging", or that damage lands
    /// "eventually", is the *magnitude* species of vacuous test: it proves
    /// direction and says nothing about the charge duration, which is the only
    /// thing a player experiences. So this predicts the value, and there are three
    /// hypotheses to separate rather than two:
    ///
    /// | hypothesis | guardian | elder | comes from |
    /// |---|---|---|---|
    /// | **correct** | **90** | **70** | `duration + 10`; `start()` sets `attackTime = -10` and `tick` increments before comparing |
    /// | dropped the lead-in | 80 | 60 | reading `ATTACK_TIME = 80` / `getAttackDuration()` and stopping there |
    /// | missed the elder override | 90 | 90 | `ElderGuardian` declares no `registerGoals`, so its table is the guardian's |
    ///
    /// Every assertion below is written to fail under both wrong hypotheses, and
    /// the last one keys on the *difference* — 20 ticks, which is `80 - 60` — so a
    /// gate cannot be satisfied by two independently wrong numbers that happen to
    /// straddle the right ones.
    #[test]
    fn the_beam_lands_on_vanillas_ninetieth_tick_and_the_elders_on_its_seventieth() {
        // Beyond the non-elder's 3-block give-up range (`Guardian.java:361`) so
        // `can_continue_to_use` holds for the whole charge. The beam stops the
        // navigation every tick, so this distance does not change.
        let target = Vec3::new(6.5, 0.0, 0.5);

        let guardian = beam_ticks("guardian", GUARDIAN_SPEED, target, 200);
        let elder = beam_ticks("elder_guardian", ELDER_SPEED, target, 200);

        assert_eq!(
            guardian.first().copied(),
            Some(90),
            "a guardian's beam must land on tick 80 + 10 = 90. Measured \
             {guardian:?}. Tick 80 means the -10 lead-in at Guardian.java:365 was \
             dropped; nothing at all means the beam is not reaching the goal \
             selector"
        );
        assert_eq!(
            elder.first().copied(),
            Some(70),
            "an elder guardian's beam must land on tick 60 + 10 = 70. Measured \
             {elder:?}. Tick 90 means ELDER_GUARDIAN is building the guardian's \
             beam — ElderGuardian.java:39-41 overrides getAttackDuration() to 60, \
             and it does so nowhere near registerGoals, so the table gate cannot \
             see this"
        );

        let (g, e) = (guardian[0], elder[0]);
        assert_eq!(
            g - e,
            20,
            "the two charges must differ by exactly 80 - 60 = 20 ticks; measured \
             {g} and {e}. This is the assertion two independently wrong constants \
             cannot satisfy"
        );
    }

    /// The **cadence** half: a beam fires once per target acquisition, and a
    /// re-acquired target charges the full 90 ticks again rather than firing
    /// immediately off a counter that was never reset.
    ///
    /// This is the other thing a player notices and the other thing a
    /// "does it charge?" assertion cannot see. A goal that reset `attack_time` in
    /// its constructor but not in `start` would pass the gate above and then
    /// machine-gun on every later acquisition.
    #[test]
    fn a_re_acquired_target_charges_the_full_duration_again() {
        let target = Vec3::new(6.5, 0.0, 0.5);
        let world = Flat::new();
        let (mut mob, mut ai) = rostered(&world, "guardian", GUARDIAN_SPEED);

        let run = |mob: &mut NavigatingMob<'_>, ai: &mut GoalSelector| {
            mob.set_attack_target(Some(target));
            let mut first = None;
            for tick in 1..=200 {
                mob.tick(ai);
                if !mob.take_new_attacks().is_empty() && first.is_none() {
                    first = Some(tick);
                }
            }
            first
        };

        let first = run(&mut mob, &mut ai);
        assert_eq!(
            first,
            Some(90),
            "precondition: the first beam must land on tick 90, got {first:?}"
        );

        // Vanilla's `tick` clears the target after damage (`Guardian.java:407`),
        // and `NavigatingMob::find_nearest_target` returns that same field, so a
        // guardian in this sim can never re-acquire on its own. Exactly one beam
        // per acquisition is therefore the correct observation, not a shortfall.
        let second = run(&mut mob, &mut ai);
        assert_eq!(
            second,
            Some(90),
            "a re-acquired target must charge the full 90 ticks again, got \
             {second:?}. `1` means `start` is not resetting attack_time and the \
             beam fires on contact"
        );
    }

    /// The beam belongs to the guardian's table specifically, and neither the
    /// harness nor the fallback can produce it.
    ///
    /// Two controls that need no edit to any table. A **ghast** is claimed by this
    /// very family with every goal-selector row `Missing`, and a **llama** is
    /// claimed by nobody and takes [`FALLBACK`](super::super::FALLBACK). Both share
    /// the guardian's world, target, tick budget and speed, so "the guardian dealt
    /// damage on tick 90" cannot be an artefact of `beam_ticks` itself.
    #[test]
    fn no_other_species_fires_a_beam_through_the_same_harness() {
        let target = Vec3::new(6.5, 0.0, 0.5);

        assert!(
            !is_fallback(registrations_for("ghast")),
            "precondition: ghast must be claimed by this family, or it is silently \
             the llama control twice"
        );
        assert!(
            is_fallback(registrations_for("llama")),
            "precondition: llama must be unclaimed, or this control measures a \
             real table"
        );

        let ghast = beam_ticks("ghast", GUARDIAN_SPEED, target, 200);
        assert!(
            ghast.is_empty(),
            "a ghast's only modelled row is a target-selector goal — its fireball \
             is issue #227's — so it must never reach an attack. Got {ghast:?}"
        );

        let llama = beam_ticks("llama", GUARDIAN_SPEED, target, 200);
        assert!(
            llama.is_empty(),
            "FALLBACK is stroll + look and has no attacking goal at all, so a \
             rosterless species must never attack. Got {llama:?}"
        );
    }

    /// The beam holds MOVE **and** LOOK, and at vanilla's priority 4 it outranks
    /// every other MOVE goal in the guardian's table.
    ///
    /// Not a restatement of the constant: this asserts the property the priority
    /// exists to produce. `RandomStrollGoal` at 7 also claims MOVE, so transcribing
    /// the beam above 7 hands the stroll the MOVE flag and the beam never runs at
    /// all — the control this unit ran and observed. And the roster's own
    /// `target_and_goal_namespaces_cannot_contend` invariant requires a
    /// goal-selector goal to claim no TARGET, so that is checked here too rather
    /// than left to a sibling file.
    #[test]
    fn the_beam_outranks_every_other_move_goal_in_the_table() {
        let ctx = SpeciesContext::new(GUARDIAN_SPEED);
        let flags = guardian_beam(&ctx).flags();

        assert!(
            flags.contains(Flag::Move) && flags.contains(Flag::Look),
            "vanilla sets EnumSet.of(MOVE, LOOK) at Guardian.java:349"
        );
        assert!(
            !flags.contains(Flag::Target),
            "a goal-selector registration must claim no TARGET, or the two \
             priority namespaces can contend and every number in this file has to \
             be re-derived"
        );

        let beam_priority = GUARDIAN
            .iter()
            .find(|r| r.vanilla == "Guardian.GuardianAttackGoal")
            .expect("the guardian's beam row")
            .priority;
        let mut compared = 0;
        for row in GUARDIAN {
            if row.selector != Selector::Goal || row.vanilla == "Guardian.GuardianAttackGoal" {
                continue;
            }
            let Some(build) = row.build() else { continue };
            if build(&ctx).flags().contains(Flag::Move) {
                compared += 1;
                assert!(
                    beam_priority < row.priority,
                    "{} claims MOVE at priority {} and the beam is at {} — a \
                     charging guardian would be preempted and never fire",
                    row.vanilla,
                    row.priority,
                    beam_priority
                );
            }
        }
        assert!(
            compared > 0,
            "no other MOVE goal was compared, so this gate measured nothing — the \
             guardian's RandomStrollGoal row at priority 7 should have been found"
        );
    }

    /// The damage tick a gate predicts must be derived from the duration, not
    /// stored twice.
    #[test]
    fn damage_tick_is_the_duration_plus_the_lead_in() {
        assert_eq!(GuardianBeamGoal::guardian().damage_tick(), 90);
        assert_eq!(GuardianBeamGoal::elder().damage_tick(), 70);
    }
}
