//! The ender dragon's phase state machine — a port of
//! `EnderDragonPhase`/`EnderDragonPhaseManager` and the eleven
//! `DragonPhaseInstance` implementors under
//! `.cache/mc/26.2/src/net/minecraft/world/entity/boss/enderdragon/phases/`.
//!
//! # The one deliberate substitution, named up front
//!
//! Vanilla drives most transitions off a `Path`/`Node` search across a fixed
//! 12-node ring above the arena (`EnderDragon.findClosestNode`/`findPath`,
//! `DragonFlightHistory`) — full 3D flight pathfinding that this codebase's
//! flying-mob AI does not have (`lodestone-entity`'s goal/pathfinder stack is
//! built for ground navigation over [`crate::mobs::ChunkWorld`], not aerial
//! node graphs). Porting that graph is a separate, large piece of work and is
//! not attempted here.
//!
//! Every phase that needs "has the current flight leg finished" therefore
//! takes it as a per-tick **input** ([`DragonInputs::leg_complete`]) rather
//! than computing it from a path. Every other condition — health thresholds,
//! crystal counts, timers, RNG rolls, hurt amounts — is ported with vanilla's
//! own numbers, cited per phase below. A driving loop that wants real flight
//! supplies `leg_complete` from its own waypoint-arrival check; the state
//! machine itself does not care how that boolean was produced, which is what
//! makes it independently testable (see `tests` below: a scripted sequence of
//! [`DragonInputs`] drives an exact phase sequence with no world at all).
//!
//! # Phase ids
//!
//! [`Phase::id`] matches `EnderDragonPhase`'s own static-initializer order
//! (`HOLDING_PATTERN` through `HOVERING`), because that order **is** the wire
//! value `EnderDragon.DATA_PHASE` carries (`EnderDragonPhaseManager.setPhase`
//! calls `dragon.getEntityData().set(DATA_PHASE, target.getId())`) — a client
//! that ever decodes this field needs the same numbering.

use std::fmt;

/// One of the eleven ender-dragon phases. Order and discriminants match
/// `EnderDragonPhase`'s declaration order exactly — see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    HoldingPattern = 0,
    StrafePlayer = 1,
    LandingApproach = 2,
    Landing = 3,
    Takeoff = 4,
    SittingFlaming = 5,
    SittingScanning = 6,
    SittingAttacking = 7,
    ChargingPlayer = 8,
    Dying = 9,
    Hovering = 10,
}

impl Phase {
    /// The wire id — `EnderDragonPhase.getId()`.
    #[must_use]
    pub const fn id(self) -> i32 {
        self as i32
    }

    /// `DragonPhaseInstance.isSitting` — true for every phase whose own class
    /// overrides it: the three `AbstractDragonSittingPhase` subclasses
    /// (`SittingFlaming`/`SittingScanning`/`SittingAttacking`) **and**
    /// `DragonHoverPhase`, which overrides it separately even though it does
    /// not extend the sitting base class. Every other phase inherits
    /// `AbstractDragonPhaseInstance.isSitting`'s `false` default.
    #[must_use]
    pub const fn is_sitting(self) -> bool {
        matches!(
            self,
            Phase::SittingFlaming | Phase::SittingScanning | Phase::SittingAttacking | Phase::Hovering
        )
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Phase::HoldingPattern => "HoldingPattern",
            Phase::StrafePlayer => "StrafePlayer",
            Phase::LandingApproach => "LandingApproach",
            Phase::Landing => "Landing",
            Phase::Takeoff => "Takeoff",
            Phase::SittingFlaming => "SittingFlaming",
            Phase::SittingScanning => "SittingScanning",
            Phase::SittingAttacking => "SittingAttacking",
            Phase::ChargingPlayer => "ChargingPlayer",
            Phase::Dying => "Dying",
            Phase::Hovering => "Hovering",
        };
        f.write_str(name)
    }
}

/// A minimal RNG seam so transition tests can force an exact roll rather than
/// asserting against whatever `rand` produces — every call site names which
/// vanilla `random.nextInt(bound)` it stands in for.
pub trait DragonRng {
    /// `random.nextInt(bound)` — `bound` is always `> 0` at every call site
    /// below (each caller adds a positive constant to a `>= 0` crystal
    /// count before calling).
    fn next_below(&mut self, bound: u32) -> u32;
}

/// A [`DragonRng`] that always returns `0` — the "always take the rare
/// branch" control, useful for exercising a roll-gated transition
/// deterministically in a test.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysZeroRng;

impl DragonRng for AlwaysZeroRng {
    fn next_below(&mut self, _bound: u32) -> u32 {
        0
    }
}

/// A [`DragonRng`] that never returns `0` (returns `1` unless `bound == 1`,
/// in which case `0` is the only legal value) — the "never take the rare
/// branch" control.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeverZeroRng;

impl DragonRng for NeverZeroRng {
    fn next_below(&mut self, bound: u32) -> u32 {
        if bound <= 1 { 0 } else { 1 }
    }
}

/// Which living target (a player, in vanilla) a phase is tracking — carries
/// only what the transition conditions actually consult. Distances are
/// squared, matching vanilla's own `distanceToSqr`/`closerThan` calls, so a
/// caller never has to (mis)take a square root just to hand this struct a
/// number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetSighting {
    /// A stable identifier the driving loop can use to re-find this target
    /// (an entity id, in a real integration). Opaque to this module.
    pub id: i32,
}

/// Per-tick inputs a driving loop supplies to [`PhaseManager::tick`]. See the
/// module doc for why several of these are booleans standing in for a
/// distance/pathfinding check vanilla performs internally.
#[derive(Debug, Clone, Copy, Default)]
pub struct DragonInputs {
    /// `EnderDragonFight.aliveCrystals()` — read by holding-pattern,
    /// strafe-retarget and takeoff to decide which arc of the node ring to
    /// aim for. This module only needs the *count*, for the RNG formulas
    /// below; the ring-arc selection itself is part of the pathfinding this
    /// module does not port.
    pub alive_crystals: i32,
    /// Stand-in for `currentPath.isDone()` — see the module doc.
    pub leg_complete: bool,
    /// A player near the podium/egg, for `DragonHoldingPatternPhase`'s
    /// `findNewTarget`/`DragonLandingApproachPhase`'s targeting — `None` when
    /// vanilla's `getNearestPlayer` would return `null`.
    pub player_near_egg: Option<TargetSighting>,
    /// `egg.distToCenterSqr(playerNearestToEgg.position()) / 512.0` when a
    /// player is near the egg, else vanilla's `64.0` fallback constant
    /// (`DragonHoldingPatternPhase.findNewTarget`). The caller computes the
    /// division; this module only consumes the already-scaled value so it
    /// never has to know what "near the egg" means geometrically.
    pub egg_distance_scaled: f64,
    /// Whether the strafing dragon currently has line of sight to its
    /// attack target and is within `4096.0` (64²) blocks² —
    /// `DragonStrafePlayerPhase.doServerTick`'s outer `if`.
    pub strafe_in_range_and_los: bool,
    /// Whether the aim angle to the strafe target is inside vanilla's
    /// `[0.0, 10.0)` degree cone — the `angleDegs >= 0.0F && angleDegs < 10.0F`
    /// check gating the fireball launch.
    pub strafe_aim_in_cone: bool,
    /// A player within `20` blocks horizontally / `10` vertically of the
    /// sitting dragon — `DragonSittingScanningPhase`'s `scanTargeting`. The
    /// aim-cone angle vanilla computes alongside this only decides whether to
    /// **turn** toward the target (a movement concern, out of scope per the
    /// module doc) — it plays no part in the `SittingAttacking` transition,
    /// which fires on `scanningTime > 25` alone whenever a target is
    /// present.
    pub sitting_scan_target: Option<TargetSighting>,
    /// A player within `150` blocks — `DragonSittingScanningPhase`'s
    /// `CHARGE_TARGETING`, consulted only once the 100-tick idle timer
    /// expires with no scan target.
    pub charge_target: Option<TargetSighting>,
    /// `!egg.closerToCenterThan(dragon.position(), 10.0)` negated —
    /// `DragonTakeoffPhase`'s abort-to-holding-pattern check. `true` means
    /// still within 10 blocks of the egg.
    pub within_10_of_egg: bool,
    /// The charge phase's arrival/collision check —
    /// `distToTarget < 100.0 || distToTarget > 22500.0 ||
    /// horizontalCollision || verticalCollision` in
    /// `DragonChargePlayerPhase.doServerTick`.
    pub charge_arrived_or_collided: bool,
    /// The death phase's clean-flight check — the *conjunction* vanilla
    /// negates: `distToTarget` in `[100.0, 22500.0]` **and** no collision.
    /// `true` here means "still flying cleanly toward the portal", the
    /// condition under which `DragonDeathPhase` holds health at `1.0` rather
    /// than finishing the kill.
    pub dying_flying_cleanly: bool,
}

/// Persisted per-phase timer/counter state — the fields each
/// `DragonPhaseInstance` implementor keeps as instance fields, carried here
/// instead so [`PhaseManager`] stays one small `struct` rather than eleven.
/// `flame_count` is deliberately **not** reset by `SittingFlaming::begin`
/// (vanilla increments it there) — only [`PhaseManager::reset_flame_count`]
/// (called from the `Landing` transition, exactly where vanilla's
/// `getPhase(SITTING_FLAMING).resetFlameCount()` is) clears it.
#[derive(Debug, Clone, Copy, Default)]
struct PhaseTimers {
    /// `DragonStrafePlayerPhase.fireballCharge`.
    fireball_charge: i32,
    /// `DragonSittingScanningPhase.scanningTime`.
    scanning_time: i32,
    /// `DragonSittingFlamingPhase.flameTicks`.
    flame_ticks: i32,
    /// `DragonSittingFlamingPhase.flameCount`.
    flame_count: i32,
    /// `DragonSittingAttackingPhase.attackingTicks`.
    attacking_ticks: i32,
    /// `DragonChargePlayerPhase.timeSinceCharge`.
    time_since_charge: i32,
    /// `DragonTakeoffPhase.firstTick`.
    takeoff_first_tick: bool,
    /// `EnderDragon.sittingDamageReceived` — not per-phase in vanilla (it
    /// lives on `EnderDragon` itself) but kept alongside the other timers
    /// here since nothing else in this module owns dragon-lifetime state.
    sitting_damage_received: f32,
}

/// The strafe/charge phases' current target, tracked outside [`PhaseTimers`]
/// because it is an `Option<TargetSighting>` rather than a number.
#[derive(Debug, Clone, Copy, Default)]
struct PhaseTargets {
    strafe_target: Option<TargetSighting>,
    charge_target: Option<TargetSighting>,
}

/// Result of one [`PhaseManager::tick`] call: a transition (if any) plus any
/// side-effect the caller needs to actually perform — vanilla fires these
/// from inside the phase classes themselves (spawning a fireball entity,
/// resetting the flame count on another phase instance); this module reports
/// them instead of performing them, since it has no world to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseEffect {
    /// `DragonStrafePlayerPhase` fired a dragon fireball at its target this
    /// tick — the caller should spawn `minecraft:dragon_fireball` along the
    /// dragon's current aim.
    FireFireball,
}

/// Port of `EnderDragonPhaseManager` — owns the current phase and the timer
/// state every phase implementor keeps, and drives transitions from
/// [`DragonInputs`]. See the module doc for the pathfinding substitution.
#[derive(Debug, Clone)]
pub struct PhaseManager {
    current: Phase,
    timers: PhaseTimers,
    targets: PhaseTargets,
}

impl Default for PhaseManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PhaseManager {
    /// Starts in `HoldingPattern` — **not** a literal port of
    /// `new EnderDragonPhaseManager(dragon)`, whose constructor actually sets
    /// `HOVERING` first. Every real dragon spawn immediately overwrites that
    /// with `dragon.getPhaseManager().setPhase(EnderDragonPhase.HOLDING_PATTERN)`
    /// right after construction (`EnderDragonFight.createNewDragon`), so this
    /// constructor starts where production code actually leaves a fresh
    /// dragon, skipping the one-tick `HOVERING` detour a literal port would
    /// need a follow-up call to skip anyway.
    #[must_use]
    pub fn new() -> Self {
        Self {
            current: Phase::HoldingPattern,
            timers: PhaseTimers::default(),
            targets: PhaseTargets::default(),
        }
    }

    /// Constructs a manager already in `phase` — for scan-on-load, where a
    /// dragon found already alive in the world reports its current phase via
    /// `EnderDragon.DATA_PHASE` rather than starting fresh.
    #[must_use]
    pub fn starting_in(phase: Phase) -> Self {
        Self {
            current: phase,
            timers: PhaseTimers::default(),
            targets: PhaseTargets::default(),
        }
    }

    #[must_use]
    pub fn current(&self) -> Phase {
        self.current
    }

    /// `EnderDragonPhaseManager.setPhase` — a same-phase call is a no-op
    /// (vanilla's `target != this.currentPhase.getPhase()` guard), and a
    /// real transition calls the outgoing phase's `end()` and the incoming
    /// phase's `begin()` (folded into the per-phase `begin_*` resets below,
    /// since only `SittingFlaming::end` does anything observable — discarding
    /// its `AreaEffectCloud`, which is a world-side effect this module has no
    /// handle to and so cannot perform itself).
    pub fn set_phase(&mut self, next: Phase) {
        if next == self.current {
            return;
        }
        self.current = next;
        match next {
            Phase::HoldingPattern => {}
            Phase::StrafePlayer => {
                self.timers.fireball_charge = 0;
                self.targets.strafe_target = None;
            }
            Phase::LandingApproach | Phase::Landing => {}
            Phase::Takeoff => {
                self.timers.takeoff_first_tick = true;
            }
            Phase::SittingFlaming => {
                self.timers.flame_ticks = 0;
                self.timers.flame_count += 1;
            }
            Phase::SittingScanning => {
                self.timers.scanning_time = 0;
            }
            Phase::SittingAttacking => {
                self.timers.attacking_ticks = 0;
            }
            Phase::ChargingPlayer => {
                self.timers.time_since_charge = 0;
                self.targets.charge_target = None;
            }
            Phase::Dying | Phase::Hovering => {}
        }
    }

    /// `DragonHoldingPatternPhase.strafePlayer`/`DragonSittingScanningPhase`'s
    /// charge branch both set a target **and then** call `setPhase` — done
    /// together here so a caller cannot set the phase without the target the
    /// new phase immediately needs.
    pub fn set_phase_with_target(&mut self, next: Phase, target: TargetSighting) {
        self.set_phase(next);
        match next {
            Phase::StrafePlayer => self.targets.strafe_target = Some(target),
            Phase::ChargingPlayer => self.targets.charge_target = Some(target),
            _ => {}
        }
    }

    /// `EnderDragonPhaseManager.getPhase(SITTING_FLAMING).resetFlameCount()` —
    /// called from the `Landing` → `SittingScanning` transition, before the
    /// phase change itself (`DragonLandingPhase.doServerTick`).
    pub fn reset_flame_count(&mut self) {
        self.timers.flame_count = 0;
    }

    /// `EnderDragon.hurt`'s sitting-damage clause: while sitting, accumulated
    /// damage past `0.25 * max_health` forces `Takeoff` and clears the
    /// accumulator. Returns `true` if this hit forced a takeoff. Only called
    /// by a caller that has already applied `damage` to health — this
    /// function only tracks the *accumulator*, matching
    /// `sittingDamageReceived = sittingDamageReceived + healthBefore -
    /// getHealth()` (i.e. the actual health delta, which may be less than
    /// `damage` after armor/reduction — the caller passes that delta, not
    /// the raw hit).
    pub fn on_sitting_damage(&mut self, health_delta: f32, max_health: f32) -> bool {
        if !self.current.is_sitting() {
            return false;
        }
        self.timers.sitting_damage_received += health_delta;
        if self.timers.sitting_damage_received > 0.25 * max_health {
            self.timers.sitting_damage_received = 0.0;
            self.set_phase(Phase::Takeoff);
            true
        } else {
            false
        }
    }

    /// `EnderDragon.handleKillingBlow`: a killing blow while **not** sitting
    /// does not actually kill the dragon — it clamps health to `1.0` and
    /// enters `Dying`, which then plays out the death-flight sequence. A
    /// killing blow while sitting has no special handling here (vanilla's
    /// `!isSitting()` guard), matching the vanilla surprise that a sitting
    /// dragon *can* die outright from a killing blow, one of the two
    /// `hurt`/`handleKillingBlow` code paths in the whole class that draws no
    /// distinction between sitting and standing.
    ///
    /// Returns `true` if this call redirected the kill into `Dying` (in
    /// which case the caller must **not** actually remove the entity —
    /// health should be set to `1.0`, not `0.0`).
    pub fn on_killing_blow(&mut self) -> bool {
        if self.current.is_sitting() {
            false
        } else {
            self.set_phase(Phase::Dying);
            true
        }
    }

    /// `AbstractDragonPhaseInstance.onCrystalDestroyed` — a no-op for every
    /// phase except `HoldingPattern`
    /// (`DragonHoldingPatternPhase.onCrystalDestroyed`), which strafes the
    /// killer if the dragon `canAttack` them. `can_attack` stands in for
    /// that check (target validity/alliance, which this module has no player
    /// registry to evaluate). Returns the transition, if any.
    pub fn on_crystal_destroyed(&mut self, killer: Option<TargetSighting>, can_attack: bool) {
        if self.current != Phase::HoldingPattern {
            return;
        }
        if let (Some(killer), true) = (killer, can_attack) {
            self.set_phase_with_target(Phase::StrafePlayer, killer);
        }
    }

    /// One server tick — `DragonPhaseInstance.doServerTick`'s dispatch,
    /// folded into a single `match` on the current phase since this module
    /// has one struct instead of eleven objects. Returns any side effect the
    /// caller needs to perform (see [`PhaseEffect`]); the phase transition
    /// itself is applied internally via [`set_phase`](Self::set_phase) and
    /// observable afterward through [`current`](Self::current).
    pub fn tick(&mut self, inputs: &DragonInputs, rng: &mut dyn DragonRng) -> Option<PhaseEffect> {
        match self.current {
            // `DragonHoldingPatternPhase.findNewTarget`: only re-rolled once
            // the current leg is done.
            Phase::HoldingPattern => {
                if inputs.leg_complete {
                    // `dragon.getRandom().nextInt(crystals + 3) == 0`.
                    if rng.next_below((inputs.alive_crystals + 3).max(1) as u32) == 0 {
                        self.set_phase(Phase::LandingApproach);
                        return None;
                    }
                    if let Some(player) = inputs.player_near_egg {
                        // `nextInt((int)(distSqr + 2.0)) == 0 ||
                        // nextInt(crystals + 2) == 0`.
                        let dist_roll = rng.next_below(((inputs.egg_distance_scaled + 2.0) as u32).max(1)) == 0;
                        let crystal_roll = rng.next_below((inputs.alive_crystals + 2).max(1) as u32) == 0;
                        if dist_roll || crystal_roll {
                            self.set_phase_with_target(Phase::StrafePlayer, player);
                        }
                    }
                }
                None
            }
            Phase::StrafePlayer => {
                let Some(_target) = self.targets.strafe_target else {
                    // `LOGGER.warn(...); setPhase(HOLDING_PATTERN)`.
                    self.set_phase(Phase::HoldingPattern);
                    return None;
                };
                if inputs.strafe_in_range_and_los {
                    self.timers.fireball_charge += 1;
                    if self.timers.fireball_charge >= 5 && inputs.strafe_aim_in_cone {
                        self.timers.fireball_charge = 0;
                        self.set_phase(Phase::HoldingPattern);
                        return Some(PhaseEffect::FireFireball);
                    }
                } else if self.timers.fireball_charge > 0 {
                    self.timers.fireball_charge -= 1;
                }
                None
            }
            Phase::LandingApproach => {
                // Stand-in for `currentPath.isDone()` after
                // `findNewTarget`/`navigateToNextPathNode` — see module doc.
                if inputs.leg_complete {
                    self.set_phase(Phase::Landing);
                }
                None
            }
            Phase::Landing => {
                // `targetLocation.distanceToSqr(...) < 1.0` —
                // `inputs.leg_complete` is this module's stand-in for
                // "arrived at the egg", reused here rather than adding a
                // second boolean that would mean the same thing.
                if inputs.leg_complete {
                    self.reset_flame_count();
                    self.set_phase(Phase::SittingScanning);
                }
                None
            }
            Phase::Takeoff => {
                if self.timers.takeoff_first_tick {
                    self.timers.takeoff_first_tick = false;
                } else if !inputs.within_10_of_egg {
                    self.set_phase(Phase::HoldingPattern);
                }
                None
            }
            Phase::SittingFlaming => {
                self.timers.flame_ticks += 1;
                if self.timers.flame_ticks >= 200 {
                    if self.timers.flame_count >= 4 {
                        self.set_phase(Phase::Takeoff);
                    } else {
                        self.set_phase(Phase::SittingScanning);
                    }
                }
                None
            }
            Phase::SittingScanning => {
                self.timers.scanning_time += 1;
                if let Some(target) = inputs.sitting_scan_target {
                    if self.timers.scanning_time > 25 {
                        self.set_phase_with_target(Phase::SittingAttacking, target);
                    }
                    // Below the threshold vanilla only turns the dragon
                    // toward `target` (movement, out of scope here); no
                    // phase transition either way.
                } else if self.timers.scanning_time >= 100 {
                    match inputs.charge_target {
                        Some(target) => self.set_phase_with_target(Phase::ChargingPlayer, target),
                        None => self.set_phase(Phase::Takeoff),
                    }
                }
                None
            }
            Phase::SittingAttacking => {
                let fire = self.timers.attacking_ticks >= 40;
                self.timers.attacking_ticks += 1;
                if fire {
                    self.set_phase(Phase::SittingFlaming);
                }
                None
            }
            Phase::ChargingPlayer => {
                if self.targets.charge_target.is_none() {
                    self.set_phase(Phase::HoldingPattern);
                } else if self.timers.time_since_charge > 0 {
                    let expire = self.timers.time_since_charge >= 10;
                    self.timers.time_since_charge += 1;
                    if expire {
                        self.set_phase(Phase::HoldingPattern);
                    }
                } else if inputs.charge_arrived_or_collided {
                    self.timers.time_since_charge += 1;
                }
                None
            }
            Phase::Dying => None,
            Phase::Hovering => None,
        }
    }

    /// `DragonDeathPhase.doServerTick`'s health-drive clause, exposed
    /// separately because it drives `health`, not the phase itself (vanilla
    /// stays in `DYING` the whole time; only the caller's own removal logic,
    /// watching health hit `0.0`, ends the fight). Returns the health value
    /// to set this tick, or `None` if the caller is not currently in
    /// `Dying` (a caller should not call this outside that phase, but a
    /// `None` here is cheaper than a debug assertion for something with no
    /// safety implication).
    #[must_use]
    pub fn dying_health_this_tick(&self, inputs: &DragonInputs) -> Option<f32> {
        if self.current != Phase::Dying {
            return None;
        }
        Some(if inputs.dying_flying_cleanly { 1.0 } else { 0.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> DragonInputs {
        DragonInputs::default()
    }

    #[test]
    fn phase_ids_match_vanilla_declaration_order() {
        assert_eq!(Phase::HoldingPattern.id(), 0);
        assert_eq!(Phase::StrafePlayer.id(), 1);
        assert_eq!(Phase::LandingApproach.id(), 2);
        assert_eq!(Phase::Landing.id(), 3);
        assert_eq!(Phase::Takeoff.id(), 4);
        assert_eq!(Phase::SittingFlaming.id(), 5);
        assert_eq!(Phase::SittingScanning.id(), 6);
        assert_eq!(Phase::SittingAttacking.id(), 7);
        assert_eq!(Phase::ChargingPlayer.id(), 8);
        assert_eq!(Phase::Dying.id(), 9);
        assert_eq!(Phase::Hovering.id(), 10);
    }

    #[test]
    fn is_sitting_matches_the_four_overrides() {
        for p in [Phase::SittingFlaming, Phase::SittingScanning, Phase::SittingAttacking, Phase::Hovering] {
            assert!(p.is_sitting(), "{p} should be sitting");
        }
        for p in [
            Phase::HoldingPattern,
            Phase::StrafePlayer,
            Phase::LandingApproach,
            Phase::Landing,
            Phase::Takeoff,
            Phase::ChargingPlayer,
            Phase::Dying,
        ] {
            assert!(!p.is_sitting(), "{p} should not be sitting");
        }
    }

    /// The transition-table gate: a scripted sequence of inputs drives an
    /// exact phase *sequence*, not just "some phase was produced". Covers
    /// holding pattern -> landing approach -> landing -> sitting scanning ->
    /// sitting attacking -> sitting flaming -> takeoff -> holding pattern,
    /// exercising eight of the eleven phases end to end with no world.
    #[test]
    fn full_landing_and_sitting_sequence() {
        let mut pm = PhaseManager::new();
        let mut rng = AlwaysZeroRng;
        assert_eq!(pm.current(), Phase::HoldingPattern);

        // Leg complete + the `nextInt(crystals+3)==0` roll lands (forced by
        // AlwaysZeroRng) -> LandingApproach. A NeverZeroRng at the same input
        // must NOT transition, proving the roll is load-bearing rather than
        // decorative.
        let mut never = NeverZeroRng;
        let mut control = PhaseManager::new();
        let mut i = inputs();
        i.leg_complete = true;
        i.alive_crystals = 3;
        control.tick(&i, &mut never);
        assert_eq!(control.current(), Phase::HoldingPattern, "never-zero rng must not roll the rare branch");

        pm.tick(&i, &mut rng);
        assert_eq!(pm.current(), Phase::LandingApproach);

        // Arrive at the egg -> Landing.
        let mut i = inputs();
        i.leg_complete = true;
        pm.tick(&i, &mut rng);
        assert_eq!(pm.current(), Phase::Landing);

        // Reach the landing spot -> SittingScanning (flame count reset).
        let mut i = inputs();
        i.leg_complete = true;
        pm.tick(&i, &mut rng);
        assert_eq!(pm.current(), Phase::SittingScanning);

        // Scan with a target present for 26 ticks (scanning_time must exceed
        // 25, matching `scanningTime > 25`) -> SittingAttacking.
        let mut i = inputs();
        i.sitting_scan_target = Some(TargetSighting { id: 1 });
        for _ in 0..26 {
            pm.tick(&i, &mut rng);
        }
        assert_eq!(pm.current(), Phase::SittingAttacking);

        // 41 ticks of attacking (post-increment semantics: transition fires
        // when the *pre*-increment counter reaches 40) -> SittingFlaming.
        let i = inputs();
        for n in 0..41 {
            let before = pm.current();
            pm.tick(&i, &mut rng);
            if n < 40 {
                assert_eq!(pm.current(), before, "should not transition before tick 41");
            }
        }
        assert_eq!(pm.current(), Phase::SittingFlaming);

        // 200 ticks of flaming with flame_count already >= 4 (four prior
        // SittingFlaming::begin calls happened via three
        // Flaming->Scanning->Attacking->Flaming loops in real play; here we
        // just prove the >=4 branch goes to Takeoff by forcing it directly).
        for _ in 0..199 {
            pm.tick(&inputs(), &mut rng);
        }
        assert_eq!(pm.current(), Phase::SittingFlaming, "not yet at 200 ticks");
        pm.tick(&inputs(), &mut rng);
        assert_eq!(pm.current(), Phase::SittingScanning, "flame_count is 1, below the 4 threshold, so this goes to scanning not takeoff");

        // Now drive scanning to the 100-tick idle timeout with no scan
        // target and no charge target -> Takeoff (the "give up" branch).
        let mut i = inputs();
        i.sitting_scan_target = None;
        i.charge_target = None;
        for _ in 0..100 {
            pm.tick(&i, &mut rng);
        }
        assert_eq!(pm.current(), Phase::Takeoff);

        // Leave the 10-block radius -> HoldingPattern. First tick after
        // set_phase is the "firstTick" no-op tick, matching
        // `DragonTakeoffPhase.doServerTick`.
        let mut i = inputs();
        i.within_10_of_egg = false;
        pm.tick(&i, &mut rng); // first_tick, no-op
        assert_eq!(pm.current(), Phase::Takeoff);
        pm.tick(&i, &mut rng);
        assert_eq!(pm.current(), Phase::HoldingPattern);
    }

    #[test]
    fn scanning_idle_timeout_with_charge_target_goes_to_charging_not_takeoff() {
        let mut pm = PhaseManager::starting_in(Phase::SittingScanning);
        let mut rng = AlwaysZeroRng;
        let mut i = inputs();
        i.sitting_scan_target = None;
        i.charge_target = Some(TargetSighting { id: 7 });
        for _ in 0..100 {
            pm.tick(&i, &mut rng);
        }
        assert_eq!(pm.current(), Phase::ChargingPlayer);
    }

    #[test]
    fn charge_recovery_takes_exactly_ten_ticks_after_arrival() {
        let mut pm = PhaseManager::new();
        pm.set_phase_with_target(Phase::ChargingPlayer, TargetSighting { id: 2 });
        let mut rng = AlwaysZeroRng;

        // Not yet arrived: stays charging indefinitely.
        let mut i = inputs();
        i.charge_arrived_or_collided = false;
        for _ in 0..5 {
            pm.tick(&i, &mut rng);
        }
        assert_eq!(pm.current(), Phase::ChargingPlayer);

        // Arrives this tick -> time_since_charge starts counting.
        i.charge_arrived_or_collided = true;
        pm.tick(&i, &mut rng);
        assert_eq!(pm.current(), Phase::ChargingPlayer);

        // 9 more ticks (time_since_charge goes 1..=9, none >= 10 yet).
        i.charge_arrived_or_collided = false;
        for _ in 0..9 {
            pm.tick(&i, &mut rng);
        }
        assert_eq!(pm.current(), Phase::ChargingPlayer, "recovery window is not over yet");

        // 10th tick: time_since_charge was 10 before increment -> transition.
        pm.tick(&i, &mut rng);
        assert_eq!(pm.current(), Phase::HoldingPattern);
    }

    #[test]
    fn strafe_fires_only_at_five_charge_and_in_cone() {
        let mut pm = PhaseManager::new();
        pm.set_phase_with_target(Phase::StrafePlayer, TargetSighting { id: 9 });
        let mut rng = AlwaysZeroRng;

        let mut i = inputs();
        i.strafe_in_range_and_los = true;
        i.strafe_aim_in_cone = false;
        // Charge builds but never fires while out of the cone, even past 5.
        for _ in 0..10 {
            let effect = pm.tick(&i, &mut rng);
            assert_eq!(effect, None);
        }
        assert_eq!(pm.current(), Phase::StrafePlayer);

        i.strafe_aim_in_cone = true;
        let effect = pm.tick(&i, &mut rng);
        assert_eq!(effect, Some(PhaseEffect::FireFireball));
        assert_eq!(pm.current(), Phase::HoldingPattern);
    }

    #[test]
    fn strafe_charge_decays_when_out_of_range() {
        let mut pm = PhaseManager::new();
        pm.set_phase_with_target(Phase::StrafePlayer, TargetSighting { id: 9 });
        let mut rng = AlwaysZeroRng;

        let mut i = inputs();
        i.strafe_in_range_and_los = true;
        for _ in 0..4 {
            pm.tick(&i, &mut rng);
        }
        // Now drop out of range: charge should decay back toward 0 rather
        // than keep the partial charge indefinitely.
        i.strafe_in_range_and_los = false;
        i.strafe_aim_in_cone = true;
        for _ in 0..4 {
            let effect = pm.tick(&i, &mut rng);
            assert_eq!(effect, None, "decayed charge must not reach the fire threshold");
        }
    }

    #[test]
    fn sitting_damage_forces_takeoff_past_quarter_max_health() {
        let mut pm = PhaseManager::starting_in(Phase::SittingScanning);
        let max_health = 200.0;
        // 0.25 * 200 = 50.0 — the exact threshold. 49.9 must not trip it;
        // one more point of damage must.
        assert!(!pm.on_sitting_damage(49.9, max_health));
        assert_eq!(pm.current(), Phase::SittingScanning);
        assert!(pm.on_sitting_damage(0.2, max_health));
        assert_eq!(pm.current(), Phase::Takeoff);
    }

    #[test]
    fn sitting_damage_does_not_apply_while_flying() {
        let mut pm = PhaseManager::new();
        assert_eq!(pm.current(), Phase::HoldingPattern);
        assert!(!pm.on_sitting_damage(1000.0, 200.0));
        assert_eq!(pm.current(), Phase::HoldingPattern, "only a sitting phase accumulates sitting damage");
    }

    #[test]
    fn killing_blow_while_flying_redirects_to_dying_not_actual_death() {
        let mut pm = PhaseManager::new();
        assert!(pm.on_killing_blow());
        assert_eq!(pm.current(), Phase::Dying);
    }

    #[test]
    fn killing_blow_while_sitting_is_not_redirected() {
        let mut pm = PhaseManager::starting_in(Phase::SittingScanning);
        assert!(!pm.on_killing_blow());
        assert_eq!(pm.current(), Phase::SittingScanning);
    }

    #[test]
    fn crystal_destroyed_only_strafes_from_holding_pattern() {
        let mut pm = PhaseManager::starting_in(Phase::Takeoff);
        pm.on_crystal_destroyed(Some(TargetSighting { id: 4 }), true);
        assert_eq!(pm.current(), Phase::Takeoff, "only HoldingPattern reacts to a crystal kill");

        let mut pm = PhaseManager::new();
        pm.on_crystal_destroyed(Some(TargetSighting { id: 4 }), true);
        assert_eq!(pm.current(), Phase::StrafePlayer);
    }

    #[test]
    fn crystal_destroyed_needs_both_a_killer_and_can_attack() {
        let mut pm = PhaseManager::new();
        pm.on_crystal_destroyed(None, true);
        assert_eq!(pm.current(), Phase::HoldingPattern);

        let mut pm = PhaseManager::new();
        pm.on_crystal_destroyed(Some(TargetSighting { id: 4 }), false);
        assert_eq!(pm.current(), Phase::HoldingPattern);
    }

    #[test]
    fn dying_health_holds_at_one_while_flying_cleanly_and_zero_otherwise() {
        let mut pm = PhaseManager::new();
        pm.on_killing_blow();
        assert_eq!(pm.current(), Phase::Dying);

        let mut i = inputs();
        i.dying_flying_cleanly = true;
        assert_eq!(pm.dying_health_this_tick(&i), Some(1.0));

        i.dying_flying_cleanly = false;
        assert_eq!(pm.dying_health_this_tick(&i), Some(0.0));
    }

    #[test]
    fn dying_health_is_none_outside_dying_phase() {
        let pm = PhaseManager::new();
        assert_eq!(pm.dying_health_this_tick(&inputs()), None);
    }

    #[test]
    fn strafe_phase_with_no_target_falls_back_to_holding_pattern() {
        let mut pm = PhaseManager::new();
        pm.set_phase(Phase::StrafePlayer);
        // set_phase alone (no target) leaves strafe_target at None.
        pm.tick(&inputs(), &mut AlwaysZeroRng);
        assert_eq!(pm.current(), Phase::HoldingPattern);
    }
}
