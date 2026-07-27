//! A representative set of goals.
//!
//! These cover each flag and the common shapes of vanilla goals (periodic,
//! target-driven, continuous). They are faithful in *scheduler-visible*
//! behaviour — flags, `can_use`/`can_continue_to_use`, lifecycle — while the
//! actual movement is delegated through [`MobController`]. The aim is to prove
//! the architecture, not to port every goal.

use super::goal::{Flag, FlagSet, Goal};
use super::mob::{MobController, distance_sqr};
use lodestone_model::Vec3;

/// Swims: repeatedly requests a jump while in water or lava so the mob floats.
///
/// Vanilla `FloatGoal` (flag JUMP, updates every tick).
#[derive(Debug)]
pub struct FloatGoal;

impl Goal for FloatGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Jump])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.in_water() || mob.in_lava()
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if mob.next_f32() < 0.8 {
            mob.set_jumping(true);
        }
    }
}

/// Wanders to random reachable positions with idle pauses.
///
/// Vanilla `RandomStrollGoal` (flag MOVE). `interval` is the reciprocal chance
/// per tick of picking a new destination.
#[derive(Debug)]
pub struct RandomStrollGoal {
    speed: f64,
    interval: i32,
    target: Option<Vec3>,
    check_no_action: bool,
}

impl RandomStrollGoal {
    /// Creates a stroll goal at `speed`, choosing a new target on average once
    /// per 120 ticks (vanilla default).
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            interval: 120,
            target: None,
            check_no_action: true,
        }
    }

    /// Overrides the average ticks between destination picks.
    #[must_use]
    pub fn with_interval(mut self, interval: i32) -> Self {
        self.interval = interval.max(1);
        self
    }
}

impl Goal for RandomStrollGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        // Vanilla yields when the mob has been idle-throttled unless forced.
        if self.check_no_action && mob.no_action_time() >= 100 {
            return false;
        }
        if mob.next_i32(self.interval) != 0 {
            return false;
        }
        self.target = mob.random_stroll_target();
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.navigation_done()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(t) = self.target {
            mob.move_to(t, self.speed);
        }
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.target = None;
    }
}

/// Turns the mob's head toward the nearest player.
///
/// Vanilla `LookAtPlayerGoal` (flag LOOK).
#[derive(Debug)]
pub struct LookAtPlayerGoal {
    look_distance: f64,
    probability: f32,
    look_time: i32,
    target: Option<Vec3>,
}

impl LookAtPlayerGoal {
    /// Creates the goal with the given max look distance and per-tick chance.
    #[must_use]
    pub fn new(look_distance: f64, probability: f32) -> Self {
        Self {
            look_distance,
            probability,
            look_time: 0,
            target: None,
        }
    }
}

impl Goal for LookAtPlayerGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if mob.next_f32() >= self.probability {
            return false;
        }
        match mob.nearest_player() {
            Some(p)
                if distance_sqr(p, mob.position()) <= self.look_distance * self.look_distance =>
            {
                self.target = Some(p);
                true
            }
            _ => false,
        }
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.look_time <= 0 {
            return false;
        }
        match (self.target, mob.nearest_player()) {
            (Some(_), Some(p)) => {
                self.target = Some(p);
                distance_sqr(p, mob.position()) <= self.look_distance * self.look_distance
            }
            _ => false,
        }
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.look_time = 40 + mob.next_i32(40);
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.target = None;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if let Some(t) = self.target {
            mob.look_at(t);
        }
        self.look_time -= 1;
    }
}

/// Idly rotates the head to random directions.
///
/// Vanilla `RandomLookAroundGoal` (flags MOVE + LOOK, updates every tick).
#[derive(Debug)]
pub struct RandomLookAroundGoal {
    rel_x: f64,
    rel_z: f64,
    look_time: i32,
}

impl Default for RandomLookAroundGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl RandomLookAroundGoal {
    /// Creates the goal.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rel_x: 0.0,
            rel_z: 0.0,
            look_time: 0,
        }
    }
}

impl Goal for RandomLookAroundGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.next_f32() < 0.02
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        self.look_time >= 0
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        let angle = std::f64::consts::TAU * mob.next_f64();
        self.rel_x = angle.cos();
        self.rel_z = angle.sin();
        self.look_time = 20 + mob.next_i32(20);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.look_time -= 1;
        mob.look_toward(self.rel_x, self.rel_z);
    }
}

/// Walks to and attacks the current target.
///
/// Vanilla `MeleeAttackGoal` (flag MOVE), simplified: it re-paths toward the
/// target and attacks when within reach.
#[derive(Debug)]
pub struct MeleeAttackGoal {
    speed: f64,
    reach_sqr: f64,
    cooldown: i32,
    target: Option<Vec3>,
}

impl MeleeAttackGoal {
    /// Creates the goal with movement `speed` and a melee `reach` (blocks).
    #[must_use]
    pub fn new(speed: f64, reach: f64) -> Self {
        Self {
            speed,
            reach_sqr: reach * reach,
            cooldown: 0,
            target: None,
        }
    }
}

impl Goal for MeleeAttackGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.target = mob.attack_target();
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_some()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.cooldown = 0;
        if let Some(t) = self.target {
            mob.move_to(t, self.speed);
        }
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.target = None;
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = mob.attack_target() else {
            return;
        };
        self.target = Some(target);
        mob.look_at(target);
        mob.move_to(target, self.speed);
        self.cooldown = (self.cooldown - 1).max(0);
        if self.cooldown == 0 && distance_sqr(target, mob.position()) <= self.reach_sqr {
            mob.attack(target);
            self.cooldown = 20;
        }
    }
}

/// Flees from a nearby avoided entity.
///
/// Vanilla `AvoidEntityGoal` (flag MOVE), simplified to: when a threat is close,
/// stroll to a random position and keep going until far enough or the path ends.
#[derive(Debug)]
pub struct AvoidEntityGoal {
    max_distance: f64,
    speed: f64,
    flee_target: Option<Vec3>,
}

impl AvoidEntityGoal {
    /// Creates the goal; the mob avoids threats within `max_distance` blocks.
    #[must_use]
    pub fn new(max_distance: f64, speed: f64) -> Self {
        Self {
            max_distance,
            speed,
            flee_target: None,
        }
    }
}

impl Goal for AvoidEntityGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(threat) = mob.avoid_threat() else {
            return false;
        };
        if distance_sqr(threat, mob.position()) > self.max_distance * self.max_distance {
            return false;
        }
        self.flee_target = mob.random_stroll_target();
        self.flee_target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.navigation_done()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(t) = self.flee_target {
            mob.move_to(t, self.speed);
        }
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.flee_target = None;
    }
}

/// Runs away after taking damage.
///
/// Vanilla `PanicGoal` (flag MOVE). Uninterruptible while panicking.
#[derive(Debug)]
pub struct PanicGoal {
    speed: f64,
    target: Option<Vec3>,
}

impl PanicGoal {
    /// Creates the goal at panic `speed`.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            target: None,
        }
    }
}

impl Goal for PanicGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !mob.is_panicking() {
            return false;
        }
        self.target = mob.random_stroll_target();
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.navigation_done()
    }

    fn is_interruptable(&self) -> bool {
        false
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(t) = self.target {
            mob.move_to(t, self.speed);
        }
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.target = None;
    }
}

/// Acquires the nearest attackable entity as the mob's target.
///
/// Vanilla `NearestAttackableTargetGoal` (flag TARGET). Runs in the mob's
/// *target* selector. `random_interval` throttles the (potentially expensive)
/// search: on average only one in `random_interval` ticks actually scans.
#[derive(Debug)]
pub struct NearestAttackableTargetGoal {
    random_interval: i32,
    target: Option<Vec3>,
}

impl Default for NearestAttackableTargetGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl NearestAttackableTargetGoal {
    /// Creates the goal with vanilla's default 10-tick search throttle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            random_interval: 10,
            target: None,
        }
    }

    /// Overrides the search throttle (`1` scans every tick).
    #[must_use]
    pub fn with_interval(mut self, interval: i32) -> Self {
        self.random_interval = interval.max(1);
        self
    }
}

impl Goal for NearestAttackableTargetGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Target])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.random_interval > 1 && mob.next_i32(self.random_interval) != 0 {
            return false;
        }
        self.target = mob.find_nearest_target();
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_some()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(self.target);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(None);
        self.target = None;
    }
}

/// Retaliates against whatever last damaged the mob.
///
/// Vanilla `HurtByTargetGoal` (flag TARGET). Ignores line of sight — a mob shot
/// from cover still turns to fight.
#[derive(Debug, Default)]
pub struct HurtByTargetGoal {
    target: Option<Vec3>,
}

impl HurtByTargetGoal {
    /// Creates the goal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Goal for HurtByTargetGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Target])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.target = mob.last_hurt_by();
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_some()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(self.target);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(None);
        self.target = None;
    }
}

/// Follows a nearby entity offering a tempting item, stopping just short of it.
///
/// Vanilla `TemptGoal` (flags MOVE + LOOK). After it ends, a `calm_down` cooldown
/// briefly suppresses re-tempting (vanilla's 100 ticks).
#[derive(Debug)]
pub struct TemptGoal {
    speed: f64,
    stop_distance_sqr: f64,
    calm_down: i32,
    target: Option<Vec3>,
}

impl TemptGoal {
    /// Creates the goal with the given follow `speed`; uses vanilla's 2.5-block
    /// stop distance.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            stop_distance_sqr: 2.5 * 2.5,
            calm_down: 0,
            target: None,
        }
    }
}

impl Goal for TemptGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.calm_down > 0 {
            self.calm_down -= 1;
            return false;
        }
        self.target = mob.temptation();
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.can_use(mob)
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(t) = mob.temptation() else {
            return;
        };
        self.target = Some(t);
        mob.look_at(t);
        if distance_sqr(t, mob.position()) < self.stop_distance_sqr {
            mob.stop_navigation();
        } else {
            mob.move_to(t, self.speed);
        }
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.target = None;
        mob.stop_navigation();
        self.calm_down = 100;
    }
}

/// A baby animal follows the nearest adult of its own kind.
///
/// Vanilla `FollowParentGoal` (flag MOVE). Follows when the parent is at least
/// 3 blocks away (`distSqr >= 9`) and gives up beyond 16 (`distSqr > 256`);
/// re-paths every 10 ticks.
#[derive(Debug)]
pub struct FollowParentGoal {
    speed: f64,
    time_to_recalc: i32,
    parent: Option<Vec3>,
}

impl FollowParentGoal {
    const NEAR_SQR: f64 = 9.0;
    const FAR_SQR: f64 = 256.0;

    /// Creates the goal at the given follow `speed`.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            time_to_recalc: 0,
            parent: None,
        }
    }
}

impl Goal for FollowParentGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !mob.is_baby() {
            return false;
        }
        let Some(parent) = mob.parent_position() else {
            return false;
        };
        if distance_sqr(parent, mob.position()) < Self::NEAR_SQR {
            return false;
        }
        self.parent = Some(parent);
        true
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !mob.is_baby() {
            return false;
        }
        let Some(parent) = mob.parent_position() else {
            return false;
        };
        let d = distance_sqr(parent, mob.position());
        self.parent = Some(parent);
        (Self::NEAR_SQR..=Self::FAR_SQR).contains(&d)
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.time_to_recalc = 0;
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.parent = None;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.time_to_recalc -= 1;
        if self.time_to_recalc <= 0
            && let Some(parent) = self.parent
        {
            self.time_to_recalc = 10;
            mob.move_to(parent, self.speed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::goal::GoalSelector;
    use super::*;

    /// A scripted mob for exercising goal behaviour deterministically.
    #[derive(Default)]
    struct ScriptMob {
        pos: Vec3,
        in_water: bool,
        panicking: bool,
        nav_done: bool,
        player: Option<Vec3>,
        threat: Option<Vec3>,
        attack: Option<Vec3>,
        stroll: Option<Vec3>,
        hurt_by: Option<Vec3>,
        tempt: Option<Vec3>,
        parent: Option<Vec3>,
        baby: bool,
        f32_queue: std::collections::VecDeque<f32>,
        i32_val: i32,
        jumped: u32,
        attacked: u32,
        move_calls: u32,
    }
    impl MobController for ScriptMob {
        fn next_f32(&mut self) -> f32 {
            self.f32_queue.pop_front().unwrap_or(0.0)
        }
        fn next_i32(&mut self, _bound: i32) -> i32 {
            self.i32_val
        }
        fn next_f64(&mut self) -> f64 {
            0.0
        }
        fn position(&self) -> Vec3 {
            self.pos
        }
        fn in_water(&self) -> bool {
            self.in_water
        }
        fn is_panicking(&self) -> bool {
            self.panicking
        }
        fn move_to(&mut self, _t: Vec3, _s: f64) -> bool {
            self.move_calls += 1;
            true
        }
        fn navigation_done(&self) -> bool {
            self.nav_done
        }
        fn stop_navigation(&mut self) {}
        fn set_jumping(&mut self, j: bool) {
            if j {
                self.jumped += 1;
            }
        }
        fn look_at(&mut self, _t: Vec3) {}
        fn look_toward(&mut self, _dx: f64, _dz: f64) {}
        fn nearest_player(&self) -> Option<Vec3> {
            self.player
        }
        fn attack_target(&self) -> Option<Vec3> {
            self.attack
        }
        fn set_attack_target(&mut self, target: Option<Vec3>) {
            self.attack = target;
        }
        fn find_nearest_target(&mut self) -> Option<Vec3> {
            self.player
        }
        fn last_hurt_by(&self) -> Option<Vec3> {
            self.hurt_by
        }
        fn temptation(&self) -> Option<Vec3> {
            self.tempt
        }
        fn is_baby(&self) -> bool {
            self.baby
        }
        fn parent_position(&self) -> Option<Vec3> {
            self.parent
        }
        fn attack(&mut self, _t: Vec3) {
            self.attacked += 1;
        }
        fn avoid_threat(&self) -> Option<Vec3> {
            self.threat
        }
        fn random_stroll_target(&mut self) -> Option<Vec3> {
            self.stroll
        }
    }

    #[test]
    fn float_jumps_while_in_water() {
        let mut sel = GoalSelector::new();
        sel.add(0, Box::new(FloatGoal));
        let mut mob = ScriptMob {
            in_water: true,
            ..Default::default()
        };
        mob.f32_queue.push_back(0.5); // < 0.8 => jump
        sel.tick(&mut mob);
        assert_eq!(mob.jumped, 1);
    }

    #[test]
    fn float_preempts_stroll_but_they_share_no_flag() {
        // Float uses JUMP, stroll uses MOVE: both can run together.
        let mut sel = GoalSelector::new();
        sel.add(0, Box::new(FloatGoal));
        sel.add(1, Box::new(RandomStrollGoal::new(1.0)));
        let mut mob = ScriptMob {
            in_water: true,
            i32_val: 0, // stroll's nextInt == 0 => picks target
            stroll: Some(Vec3::new(5.0, 64.0, 5.0)),
            ..Default::default()
        };
        mob.f32_queue.push_back(0.5);
        sel.tick(&mut mob);
        assert_eq!(sel.running_indices(), vec![0, 1]);
    }

    #[test]
    fn melee_attacks_when_in_reach() {
        let mut goal = MeleeAttackGoal::new(1.0, 2.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(1.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        goal.tick(&mut mob);
        assert_eq!(mob.attacked, 1);
    }

    #[test]
    fn panic_is_uninterruptable() {
        let g = PanicGoal::new(1.5);
        assert!(!g.is_interruptable());
    }

    #[test]
    fn avoid_flees_when_threat_close() {
        let mut goal = AvoidEntityGoal::new(8.0, 1.2);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            threat: Some(Vec3::new(2.0, 64.0, 0.0)),
            stroll: Some(Vec3::new(-10.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
    }

    #[test]
    fn nearest_target_sets_and_clears_target() {
        let mut goal = NearestAttackableTargetGoal::new().with_interval(1);
        let mut mob = ScriptMob {
            player: Some(Vec3::new(4.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        assert_eq!(mob.attack_target(), Some(Vec3::new(4.0, 64.0, 0.0)));
        goal.stop(&mut mob);
        assert_eq!(mob.attack_target(), None);
    }

    #[test]
    fn target_goal_then_melee_across_two_selectors() {
        // The scheduler contract the brief cares about: a TARGET-flag goal in the
        // target selector sets the target, and a MOVE-flag goal in the goal
        // selector consumes it — with no flag contention between them.
        let mut ai = super::super::goal::MobAi::new();
        ai.target_selector.add(
            0,
            Box::new(NearestAttackableTargetGoal::new().with_interval(1)),
        );
        ai.goal_selector
            .add(0, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            player: Some(Vec3::new(1.0, 64.0, 0.0)),
            ..Default::default()
        };
        ai.tick(&mut mob);
        // Target selector acquired the player; melee attacked within reach.
        assert_eq!(mob.attack_target(), Some(Vec3::new(1.0, 64.0, 0.0)));
        assert_eq!(mob.attacked, 1);
        assert_eq!(ai.target_selector.running_indices(), vec![0]);
        assert_eq!(ai.goal_selector.running_indices(), vec![0]);
    }

    #[test]
    fn hurt_by_retaliates() {
        let mut goal = HurtByTargetGoal::new();
        let mut mob = ScriptMob {
            hurt_by: Some(Vec3::new(-3.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        assert_eq!(mob.attack_target(), Some(Vec3::new(-3.0, 64.0, 0.0)));
    }

    #[test]
    fn tempt_follows_then_calms_down() {
        let mut goal = TemptGoal::new(1.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            tempt: Some(Vec3::new(10.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.tick(&mut mob);
        assert_eq!(mob.move_calls, 1); // beyond 2.5 -> navigates
        goal.stop(&mut mob);
        // calm_down now suppresses re-tempting for the next tick.
        assert!(!goal.can_use(&mut mob));
    }

    #[test]
    fn tempt_stops_within_range() {
        let mut goal = TemptGoal::new(1.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            tempt: Some(Vec3::new(1.0, 64.0, 0.0)), // within 2.5
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.tick(&mut mob);
        assert_eq!(mob.move_calls, 0); // close enough -> stops navigation
    }

    #[test]
    fn follow_parent_only_when_baby_and_far() {
        let mut goal = FollowParentGoal::new(1.0);
        // Adult: never follows.
        let mut adult = ScriptMob {
            baby: false,
            parent: Some(Vec3::new(10.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(!goal.can_use(&mut adult));
        // Baby, parent close (<3): does not follow.
        let mut near = ScriptMob {
            baby: true,
            pos: Vec3::new(0.0, 64.0, 0.0),
            parent: Some(Vec3::new(2.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(!goal.can_use(&mut near));
        // Baby, parent far enough: follows and paths.
        let mut far = ScriptMob {
            baby: true,
            pos: Vec3::new(0.0, 64.0, 0.0),
            parent: Some(Vec3::new(6.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut far));
        goal.start(&mut far);
        goal.tick(&mut far);
        assert_eq!(far.move_calls, 1);
    }
}
