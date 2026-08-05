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

/// Swells toward detonation near its target, or backs off and shrinks
/// otherwise.
///
/// Vanilla `SwellGoal` (flag MOVE, `requiresUpdateEveryTick`); `Creeper.java:66`
/// registers it at priority 2 — one below `MeleeAttackGoal`'s own priority 4 —
/// so once eligible it preempts melee on their shared MOVE flag through
/// [`GoalSelector`]'s ordinary priority preemption, no special case needed. A
/// creeper keeps *both* goals (`Creeper.java:65-74` also registers
/// `MeleeAttackGoal` at priority 4); this is "alongside", not "instead of".
///
/// `can_use` (`SwellGoal.java:18-21`): eligible while already swelling
/// (`mob.swell_dir() > 0` — true whenever [`is_ignited`](MobController::is_ignited)
/// forced it, even with no target at all) or while a target exists within
/// `distanceToSqr < 9.0` (3 blocks). `tick` (`SwellGoal.java:40-52`): while a
/// target remains, sets the direction to shrink (`-1`) once `distanceToSqr >
/// 49.0` (7 blocks), otherwise to climb (`1`); with no target, always shrinks.
///
/// Vanilla's `tick` also drops the fuse on lost line of sight
/// (`SwellGoal.java:44`, `!hasLineOfSight`); this seam has no raycast
/// primitive (see [`MobController`]'s own doc comment on why movement/
/// perception specifics are delegated to the host), so that check is
/// deliberately omitted — the same disclosed simplification
/// [`MeleeAttackGoal`]'s own doc comment already makes for its reach check.
/// Distance alone is sufficient to close the bug this goal exists to fix
/// (creepers never priming near a player).
#[derive(Debug, Default)]
pub struct SwellGoal;

impl SwellGoal {
    /// Vanilla's proximity-squared threshold that starts the fuse
    /// (`SwellGoal.java:20`, `distanceToSqr(target) < 9.0` — 3 blocks).
    const START_RANGE_SQR: f64 = 9.0;
    /// Vanilla's retreat-squared threshold that reverses it
    /// (`SwellGoal.java:42`, `distanceToSqr(target) > 49.0` — 7 blocks).
    const STOP_RANGE_SQR: f64 = 49.0;

    /// Creates the goal.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Goal for SwellGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if mob.swell_dir() > 0 {
            return true;
        }
        match mob.attack_target() {
            Some(t) => distance_sqr(t, mob.position()) < Self::START_RANGE_SQR,
            None => false,
        }
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        match mob.attack_target() {
            Some(t) if distance_sqr(t, mob.position()) <= Self::STOP_RANGE_SQR => {
                mob.set_swell_dir(1);
            }
            _ => mob.set_swell_dir(-1),
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
    /// Whether this registration carries vanilla's `this::isAngryAt` selector,
    /// i.e. whether it is a *neutral* mob's row. See
    /// [`anger_gated`](Self::anger_gated).
    anger_gated: bool,
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
            anger_gated: false,
        }
    }

    /// The **neutral** mob's form of this registration: vanilla passes a
    /// `this::isAngryAt` selector as the goal's last argument (zombified piglin,
    /// wolf and bee all do), and `NeutralMob.isAngryAt` narrows the candidate set
    /// to the single entity the mob's persistent grudge names. So this form does
    /// not search at all — it targets
    /// [`MobController::angry_target`](crate::ai::MobController::angry_target),
    /// and a mob with no live grudge acquires nothing.
    ///
    /// **This is the difference between a neutral mob and a hostile one**, and
    /// without it a predicate-free registration makes piglins, wolves and bees
    /// attack on sight — which was invisible for as long as
    /// `NavigatingMob::find_nearest_target` returned its own `attack_target` and
    /// never searched (issue #455). The neutral family's three anger-gated rows
    /// are `Coverage::Missing` and must stay that way until a host feeds
    /// `set_angry_target`; `roster::neutral`'s
    /// `no_anger_gated_target_row_is_modelled` enforces it, and this constructor
    /// existing is not permission to flip them.
    ///
    /// Not modelled: `isAngryAtAllPlayers` (`NeutralMob.isAngryAt`'s first
    /// branch), vanilla's universal anger from group alerting, under which *any*
    /// player is a candidate rather than only the grudge holder. Same-species
    /// propagation is a census question for `MobSim::feed_perception`, not a
    /// seam method — this trait hands goals answers, never populations.
    #[must_use]
    pub fn anger_gated() -> Self {
        Self {
            anger_gated: true,
            ..Self::new()
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
        self.target = if self.anger_gated {
            mob.angry_target()
        } else {
            mob.find_nearest_target()
        };
        self.target.is_some()
    }

    /// Vanilla `TargetGoal.canContinueToUse`
    /// (`ai/goal/target/TargetGoal.java:36-71`), which does three things this
    /// used to skip entirely — it held only `attack_target().is_some()`:
    ///
    /// 1. **Releases a target that left follow range** (`:57-60`,
    ///    `if (this.mob.distanceToSqr(target) > within * within) return false;`).
    ///    Without it the acquisition cut is a one-way door: acquire at 16
    ///    blocks, then chase across the world forever.
    /// 2. **Re-writes the target every tick** (`:70`, `this.mob.setTarget(target)`).
    ///    Vanilla's target is a *live entity reference*, so its position is
    ///    always current; ours is a `Vec3` frozen at acquisition, which means a
    ///    mob pursued a **moving player to where that player used to be** and
    ///    stopped. Refreshing from the same feed acquisition used is how a
    ///    position-valued seam reproduces a reference-valued one.
    /// 3. Releases when the candidate disappears (`:43-45`, `target == null`).
    ///
    /// The one divergence worth stating: vanilla keeps the *specific* entity it
    /// acquired, while our feed answers "the nearest player", so if a second
    /// player becomes nearer mid-pursuit this switches and vanilla would not.
    /// That is a property of the seam carrying a position rather than an
    /// identity — with one player in range the two agree exactly, and the
    /// alternative (a frozen point) is wrong every time the player moves.
    ///
    /// Vanilla's `mustSee`/`unseenMemoryTicks` half (`:62-68`) is not modelled —
    /// it needs the line of sight `NavigatingMob::find_nearest_target` explains
    /// is a ray query this seam cannot answer. `canAttack` and the team check
    /// (`:47-55`) have no analogue here either.
    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        if mob.attack_target().is_none() {
            return false;
        }
        // Vanilla's live-reference position, resolved the way the seam can: ask
        // the same source `can_use` did. `None` covers both of vanilla's exits —
        // the candidate is gone, or the range re-test failed.
        let live = if self.anger_gated {
            mob.angry_target()
        } else {
            mob.find_nearest_target()
        };
        let Some(target) = live else {
            return false;
        };
        // `angry_target` carries no range cut of its own (a grudge is not
        // bounded by follow range in the feed), so vanilla's own `:57-60` test
        // still has to run here rather than being left to the filter.
        let within = mob.follow_range();
        if distance_sqr(mob.position(), target) > within * within {
            return false;
        }
        mob.set_attack_target(Some(target));
        true
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

/// Two in-love adults of the same kind seek each other out and breed.
///
/// Vanilla `BreedGoal` (flags MOVE, LOOK). It starts when this animal is in love
/// and a free partner exists (also in love, within 8 blocks, not panicking), and
/// continues while that partner stays a valid mate and `love_time < 60`. Once the
/// pair has spent 60 ticks within 3 blocks of each other (`distSqr < 9`), a child
/// is spawned and both leave love mode.
///
/// The partner filter (`canMate`, range, line-of-sight) is version/type-specific
/// and lives behind the [`MobController`] seam, so this goal holds only the
/// scheduler-visible timing state.
#[derive(Debug)]
pub struct BreedGoal {
    speed: f64,
    love_time: i32,
}

impl BreedGoal {
    /// Ticks the pair must stay together before a child spawns (vanilla's 60).
    const BREED_TIME: i32 = 60;
    /// Squared distance within which breeding completes (vanilla's `9.0`).
    const BREED_RANGE_SQR: f64 = 9.0;

    /// Creates the goal with the given approach `speed`.
    #[must_use]
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            love_time: 0,
        }
    }
}

impl Goal for BreedGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !mob.is_in_love() {
            return false;
        }
        mob.find_love_partner().is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        // `love_partner_position` folds vanilla's partner alive/in-love/not-
        // panicking checks: it returns `None` the instant the mate is ineligible.
        mob.love_partner_position().is_some() && self.love_time < Self::BREED_TIME
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.love_time = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.clear_love_partner();
        self.love_time = 0;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(partner) = mob.love_partner_position() else {
            return;
        };
        mob.look_at(partner);
        mob.move_to(partner, self.speed);
        self.love_time += 1;
        if self.love_time >= Self::BREED_TIME
            && distance_sqr(partner, mob.position()) < Self::BREED_RANGE_SQR
        {
            mob.breed();
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
        in_love: bool,
        love_partner: Option<Vec3>,
        partner_valid: bool,
        bred: u32,
        partner_cleared: u32,
        f32_queue: std::collections::VecDeque<f32>,
        i32_val: i32,
        jumped: u32,
        attacked: u32,
        move_calls: u32,
        swell_dir: i32,
        ignited: bool,
        stopped_navigation: u32,
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
        fn stop_navigation(&mut self) {
            self.stopped_navigation += 1;
        }
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
        fn is_in_love(&self) -> bool {
            self.in_love
        }
        fn find_love_partner(&mut self) -> Option<Vec3> {
            if self.love_partner.is_some() {
                self.partner_valid = true;
            }
            self.love_partner
        }
        fn love_partner_position(&self) -> Option<Vec3> {
            if self.partner_valid {
                self.love_partner
            } else {
                None
            }
        }
        fn breed(&mut self) {
            self.bred += 1;
            self.in_love = false;
            self.partner_valid = false;
        }
        fn clear_love_partner(&mut self) {
            self.partner_cleared += 1;
            self.love_partner = None;
            self.partner_valid = false;
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
        fn is_ignited(&self) -> bool {
            self.ignited
        }
        fn swell_dir(&self) -> i32 {
            self.swell_dir
        }
        fn set_swell_dir(&mut self, dir: i32) {
            self.swell_dir = dir;
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
    fn swell_starts_and_climbs_within_three_blocks() {
        let mut goal = SwellGoal::new();
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(2.0, 64.0, 0.0)), // distSqr 4 < 9
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        assert_eq!(mob.stopped_navigation, 1);
        goal.tick(&mut mob);
        assert_eq!(mob.swell_dir, 1);
    }

    #[test]
    fn swell_does_not_start_beyond_three_blocks() {
        // Negative control: a target well outside the 3-block proximity gate,
        // and no swell already in progress, must not make the goal eligible —
        // proving `can_use` actually gates on distance rather than firing
        // unconditionally whenever a target exists at all.
        let mut goal = SwellGoal::new();
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(10.0, 64.0, 0.0)), // distSqr 100 > 9
            swell_dir: -1,
            ..Default::default()
        };
        assert!(!goal.can_use(&mut mob));
    }

    #[test]
    fn swell_keeps_climbing_past_three_blocks_once_started() {
        // Vanilla's hysteresis: once already swelling, `can_use` (which also
        // serves as the default `can_continue_to_use`) stays eligible from
        // `swell_dir() > 0` alone, regardless of the 3-block start gate — the
        // 7-block *stop* gate is a separate, wider threshold checked in `tick`.
        let mut goal = SwellGoal::new();
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(6.0, 64.0, 0.0)), // distSqr 36: >9, <=49
            swell_dir: 1,
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.tick(&mut mob);
        assert_eq!(mob.swell_dir, 1); // still within the 7-block stop range
    }

    #[test]
    fn swell_reverses_beyond_seven_blocks() {
        let mut goal = SwellGoal::new();
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(8.0, 64.0, 0.0)), // distSqr 64 > 49
            swell_dir: 1,
            ..Default::default()
        };
        goal.tick(&mut mob);
        assert_eq!(mob.swell_dir, -1);
    }

    #[test]
    fn swell_reverses_and_stops_once_the_target_is_lost() {
        let mut goal = SwellGoal::new();
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: None,
            swell_dir: 1,
            ..Default::default()
        };
        goal.tick(&mut mob);
        assert_eq!(mob.swell_dir, -1);
        // The next scheduler tick re-checks `can_use` (the default
        // `can_continue_to_use`): direction is now `-1` and there is still no
        // target, so the goal stops running.
        assert!(!goal.can_use(&mut mob));
    }

    #[test]
    fn swell_fires_from_ignition_alone_with_no_target() {
        // An ignited creeper forces `swell_dir` to `1` every tick from the
        // entity's own unconditional integration (`NavigatingMob::advance`),
        // independent of any goal target — `can_use`'s first branch must see
        // that and start the goal even though `attack_target()` is `None`.
        let mut goal = SwellGoal::new();
        let mut mob = ScriptMob {
            attack: None,
            swell_dir: 1,
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
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

    #[test]
    fn breed_requires_love_and_a_partner() {
        let mut goal = BreedGoal::new(1.0);
        // In love but no partner nearby: cannot start.
        let mut lonely = ScriptMob {
            in_love: true,
            love_partner: None,
            ..Default::default()
        };
        assert!(!goal.can_use(&mut lonely));
        // Partner present but this animal is not in love: cannot start.
        let mut unwilling = ScriptMob {
            in_love: false,
            love_partner: Some(Vec3::new(1.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(!goal.can_use(&mut unwilling));
        // Both conditions met: starts, and the partner is now remembered.
        let mut ready = ScriptMob {
            in_love: true,
            love_partner: Some(Vec3::new(1.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut ready));
        assert!(ready.partner_valid);
    }

    #[test]
    fn breed_spawns_a_child_after_sixty_ticks_in_range() {
        let mut goal = BreedGoal::new(1.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            in_love: true,
            love_partner: Some(Vec3::new(1.0, 64.0, 0.0)), // distSqr 1 < 9
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        // No child before the 60-tick timer elapses.
        for _ in 0..59 {
            assert!(goal.can_continue_to_use(&mut mob));
            goal.tick(&mut mob);
        }
        assert_eq!(mob.bred, 0);
        // The 60th tick breeds exactly once and clears love mode.
        goal.tick(&mut mob);
        assert_eq!(mob.bred, 1);
        assert!(!mob.in_love);
        // With love spent, the goal no longer continues, and stopping forgets
        // the partner.
        assert!(!goal.can_continue_to_use(&mut mob));
        goal.stop(&mut mob);
        assert_eq!(mob.partner_cleared, 1);
    }

    #[test]
    fn breed_stops_when_the_partner_becomes_ineligible() {
        let mut goal = BreedGoal::new(1.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            in_love: true,
            love_partner: Some(Vec3::new(1.0, 64.0, 0.0)),
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        // Partner wanders off / stops loving: host reports it invalid.
        mob.partner_valid = false;
        assert!(!goal.can_continue_to_use(&mut mob));
        // A tick in that state is a no-op — no movement, no child.
        goal.tick(&mut mob);
        assert_eq!(mob.move_calls, 0);
        assert_eq!(mob.bred, 0);
    }
}
