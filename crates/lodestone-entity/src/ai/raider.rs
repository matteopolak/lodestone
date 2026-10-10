//! Goals shared by the raiders: fetching the leader banner, walking between
//! homes during a raid, holding ground on patrol and celebrating a lost village.

use lodestone_model::Vec3;

use super::goal::{Flag, FlagSet, Goal};
use super::mob::{MobController, distance_sqr};

/// Walks to a dropped ominous banner and asks the host to hand it over once
/// within reach. A banner that cannot be pathed to is ignored for 600 ticks.
#[derive(Debug)]
pub struct FetchLeaderBannerGoal {
    speed: f64,
    unreachable_until: u64,
    pursued: Option<Vec3>,
}

impl FetchLeaderBannerGoal {
    /// A fetch at `speed` blocks per tick.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self { speed, unreachable_until: 0, pursued: None }
    }
}

impl Goal for FetchLeaderBannerGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if mob.tick_count() < self.unreachable_until {
            return false;
        }
        mob.banner_target().is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.pursued.is_some() && mob.banner_target().is_some() && !mob.navigation_done()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.pursued = mob.banner_target();
        if let Some(at) = self.pursued
            && !mob.move_to(at, self.speed)
        {
            self.unreachable_until = mob.tick_count() + 600;
            self.pursued = None;
        }
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.pursued = None;
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if let Some(at) = self.pursued
            && distance_sqr(mob.position(), at) < 1.414 * 1.414
        {
            mob.request_banner_pickup();
        }
    }
}

/// How far from a home a raider looks for the next one to visit.
const HOME_SEARCH_RADIUS: f64 = 48.0;
/// Visited homes remembered.
const VISITED_MEMORY: usize = 15;

/// While its raid is ongoing and it holds no target, walks from bed to bed in
/// the village, never revisiting the last ones it reached.
#[derive(Debug)]
pub struct RaiderHomeVisitGoal {
    speed: f64,
    reach: f64,
    target: Option<Vec3>,
    visited: Vec<Vec3>,
    stuck: bool,
}

impl RaiderHomeVisitGoal {
    /// A visit at `speed` blocks per tick ending within `reach` blocks of a bed.
    #[must_use]
    pub fn new(speed: f64, reach: f64) -> Self {
        Self { speed, reach, target: None, visited: Vec::new(), stuck: false }
    }
}

impl Goal for RaiderHomeVisitGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.visited.len() > VISITED_MEMORY {
            self.visited.remove(0);
        }
        if mob.raid_center().is_none() || mob.attack_target().is_some() {
            return false;
        }
        let here = mob.position();
        let candidates: Vec<Vec3> = mob
            .home_pois()
            .iter()
            .copied()
            .filter(|p| distance_sqr(*p, here) <= HOME_SEARCH_RADIUS * HOME_SEARCH_RADIUS && !self.visited.contains(p))
            .collect();
        if candidates.is_empty() {
            return false;
        }
        let pick = mob.next_i32(candidates.len() as i32).max(0) as usize;
        self.target = Some(candidates[pick.min(candidates.len() - 1)]);
        true
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(target) = self.target else { return false };
        !mob.navigation_done()
            && mob.attack_target().is_none()
            && distance_sqr(target, mob.position()) > (self.reach + 0.6).powi(2)
            && !self.stuck
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.reset_no_action_time();
        self.stuck = false;
        if let Some(target) = self.target {
            mob.move_to(target, self.speed);
        }
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        if let Some(target) = self.target
            && distance_sqr(target, mob.position()) <= self.reach * self.reach
        {
            self.visited.push(target);
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = self.target else { return };
        if mob.navigation_done() {
            match mob.random_target_towards(target) {
                Some(step) => {
                    mob.move_to(step, self.speed);
                }
                None => self.stuck = true,
            }
        }
    }
}

/// A patrolling raider that has just acquired a target stands still and calls
/// the raiders near it to the same target, attacking once the target comes
/// within the hostile radius.
#[derive(Debug)]
pub struct HoldGroundAttackGoal {
    hostile_radius_sqr: f64,
}

impl HoldGroundAttackGoal {
    /// A hold-ground that turns aggressive within `hostile_radius` blocks.
    #[must_use]
    pub fn new(hostile_radius: f64) -> Self {
        Self { hostile_radius_sqr: hostile_radius * hostile_radius }
    }
}

impl Goal for HoldGroundAttackGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.raid_center().is_none()
            && mob.is_patrolling()
            && mob.attack_target().is_some()
            && !mob.is_aggressive()
            && !mob.last_hurt_by_was_player()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.stop_navigation();
        mob.shout_to_raiders(false);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        if mob.attack_target().is_some() {
            mob.shout_to_raiders(true);
            mob.set_aggressive(true);
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = mob.attack_target() else { return };
        if distance_sqr(mob.position(), target) > self.hostile_radius_sqr {
            mob.look_at(target);
        } else {
            mob.set_aggressive(true);
        }
    }
}

/// Once the village is lost, a raider with no target jumps about and shows it
/// is celebrating.
#[derive(Debug, Default)]
pub struct RaiderCelebrationGoal;

impl Goal for RaiderCelebrationGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_none() && mob.raid_lost()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_celebrating(true);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_celebrating(false);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if mob.next_i32(self.adjusted_tick_delay(50)) == 0 {
            mob.hop();
        }
    }
}

/// Ticks a witch waits after choosing a raider to heal before it may heal again.
const HEAL_COOLDOWN: i32 = super::goal::reduced_tick_delay(200);

/// A witch's search for a raider of its raid to heal: half the ticks it looks,
/// only while its own raid is ongoing, and after finding one it pauses both
/// this search and its attack on players for [`HEAL_COOLDOWN`] ticks.
#[derive(Debug)]
pub struct HealRaiderTargetGoal {
    inner: super::goals::NearestTargetGoal,
}

impl Default for HealRaiderTargetGoal {
    fn default() -> Self {
        Self { inner: super::goals::NearestTargetGoal::of_class(super::TargetClass::HealableRaider, true) }
    }
}

impl Goal for HealRaiderTargetGoal {
    fn flags(&self) -> FlagSet {
        self.inner.flags()
    }

    fn target_class(&self) -> Option<super::TargetClass> {
        self.inner.target_class()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if mob.heal_cooldown() > 0 || mob.next_i32(2) == 0 || mob.raid_center().is_none() {
            return false;
        }
        self.inner.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_heal_cooldown(HEAL_COOLDOWN);
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.inner.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.inner.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.inner.tick(mob);
    }
}

/// A target search that stays quiet while the witch's heal cooldown runs.
#[derive(Debug)]
pub struct AfterHealingTargetGoal {
    inner: super::goals::NearestTargetGoal,
}

impl Default for AfterHealingTargetGoal {
    fn default() -> Self {
        Self { inner: super::goals::NearestTargetGoal::new() }
    }
}

impl Goal for AfterHealingTargetGoal {
    fn flags(&self) -> FlagSet {
        self.inner.flags()
    }

    fn target_class(&self) -> Option<super::TargetClass> {
        self.inner.target_class()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.heal_cooldown() <= 0 && self.inner.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.inner.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.inner.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.inner.tick(mob);
    }
}

/// A vindicator named Johnny hunts every mob near it.
#[derive(Debug)]
pub struct JohnnyTargetGoal {
    inner: super::goals::NearestTargetGoal,
}

impl Default for JohnnyTargetGoal {
    fn default() -> Self {
        Self { inner: super::goals::NearestTargetGoal::of_class(super::TargetClass::AnyMob, true) }
    }
}

impl Goal for JohnnyTargetGoal {
    fn flags(&self) -> FlagSet {
        self.inner.flags()
    }

    fn target_class(&self) -> Option<super::TargetClass> {
        self.inner.target_class()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.is_johnny() && self.inner.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.reset_no_action_time();
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.inner.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.inner.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.inner.tick(mob);
    }
}
