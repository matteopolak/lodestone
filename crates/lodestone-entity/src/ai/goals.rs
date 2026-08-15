//! A representative set of goals.
//!
//! These cover each flag and the common shapes of vanilla goals (periodic,
//! target-driven, continuous). They are faithful in *scheduler-visible*
//! behaviour — flags, `can_use`/`can_continue_to_use`, lifecycle — while the
//! actual movement is delegated through [`MobController`]. The aim is to prove
//! the architecture, not to port every goal.

use super::goal::{Flag, FlagSet, Goal};
use super::mob::{EatenBlock, MobController, distance_sqr};
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
/// Vanilla `SwellGoal` (flag MOVE, `requiresUpdateEveryTick`); `Creeper.registerGoals`
/// registers it at priority 2 — one below `MeleeAttackGoal`'s own priority 4 —
/// so once eligible it preempts melee on their shared MOVE flag through
/// [`GoalSelector`]'s ordinary priority preemption, no special case needed. A
/// creeper keeps *both* goals (`Creeper.registerGoals` also registers
/// `MeleeAttackGoal` at priority 4); this is "alongside", not "instead of".
///
/// `SwellGoal.canUse`: eligible while already swelling
/// (`mob.swell_dir() > 0` — true whenever [`is_ignited`](MobController::is_ignited)
/// forced it, even with no target at all) or while a target exists within
/// `distanceToSqr < 9.0` (3 blocks). `SwellGoal.tick`: while a
/// target remains, sets the direction to shrink (`-1`) once `distanceToSqr >
/// 49.0` (7 blocks), otherwise to climb (`1`); with no target, always shrinks.
///
/// Vanilla's `tick` also drops the fuse on lost line of sight
/// (`SwellGoal.tick`'s `!hasLineOfSight` branch); this seam has no raycast
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
    /// (`SwellGoal.canUse`'s `distanceToSqr(target) < 9.0` — 3 blocks).
    const START_RANGE_SQR: f64 = 9.0;
    /// Vanilla's retreat-squared threshold that reverses it
    /// (`SwellGoal.tick`'s `distanceToSqr(target) > 49.0` — 7 blocks).
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

/// Stops an enderman dead and stares back while its current target watches it.
///
/// Vanilla `EnderMan.EndermanFreezeWhenLookedAt` (flags `{JUMP, MOVE}`):
///
/// ```text
/// canUse():  target = enderman.getTarget()
///            target is Player && distanceToSqr(target) <= 256.0
///              && enderman.isBeingStaredBy(target)
/// start():   navigation.stop()
/// tick():    lookControl.setLookAt(target.x, target.getEyeY(), target.z)
/// ```
///
/// `canContinueToUse` is not overridden in vanilla, so it defaults to
/// re-running `canUse` every tick — this port takes the same default rather
/// than adding one.
///
/// # Two disclosed narrowings
///
/// * **The `Player` type check does not exist on this seam.** Every
///   [`attack_target`](MobController::attack_target) this crate ever sets is a
///   player position — nothing else can become one; see that method's own doc
///   comment — so the port reads it directly rather than re-deriving a type
///   test with nothing to check against.
/// * **The gaze test itself is not computed here.** `enderman.isBeingStaredBy`
///   is [`MobController::is_being_stared_at`], a host-fed boolean: the
///   geometry is [`is_in_view_cone`](super::mob::is_in_view_cone), which
///   mirrors `LivingEntity.isLookingAtMe`'s exact
///   `dot > 1.0 - coneSize / dist` tolerance. Only the 16-block range check
///   (`distanceToSqr <= 256.0`) belongs to *this* goal, exactly where vanilla
///   puts it — folding it into the boolean would silently take the minimum of
///   two ranges, the same trap this crate's `LookAtPlayerGoal`/`nearest_player`
///   split already avoids.
#[derive(Debug, Default)]
pub struct EndermanFreezeWhenLookedAt {
    target: Option<Vec3>,
}

impl EndermanFreezeWhenLookedAt {
    /// Creates the goal with no remembered target.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Goal for EndermanFreezeWhenLookedAt {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Jump, Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.target = mob.attack_target();
        match self.target {
            Some(target) => {
                distance_sqr(mob.position(), target) <= 256.0 && mob.is_being_stared_at()
            }
            None => false,
        }
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if let Some(target) = self.target {
            mob.look_at(target);
        }
    }
}

/// Turns a stare into an actual attack: the enderman's own aggro/teleport
/// state machine, and the goal that is missing precisely when "an enderman
/// does nothing when I look at it" is reported — `EndermanFreezeWhenLookedAt`
/// only pins the head, this is what ever calls
/// [`set_attack_target`](MobController::set_attack_target) unprovoked.
///
/// Vanilla `EnderMan.EndermanLookForPlayerGoal` (target-selector priority 1,
/// flag TARGET, `EnderMan.java`):
///
/// ```text
/// canUse():      pendingTarget = nearestPlayer(startAggroTargetConditions)
///                // isAngerInducing: isBeingStaredBy(p) || isAngryAt(p)
///                return pendingTarget != null
/// start():       aggroTime = adjustedTickDelay(5); teleportTime = 0
/// canContinueToUse():
///                if pendingTarget != null: isAngerInducing(pendingTarget)
///                else: target != null && continueAggroTargetConditions(target)
/// tick():        if pendingTarget != null:
///                    if --aggroTime <= 0: target = pendingTarget; pendingTarget = null
///                else if target != null:
///                    if isBeingStaredBy(target):
///                        if distanceToSqr(target) < 16.0: teleport()  // random blink
///                        teleportTime = 0
///                    else if distanceToSqr(target) > 256.0
///                         && teleportTime++ >= adjustedTickDelay(30)
///                         && teleportTowards(target):
///                        teleportTime = 0
/// ```
///
/// # Three disclosed narrowings, same shape as [`EndermanFreezeWhenLookedAt`]
///
/// * **No per-player identity.** This seam's [`MobController::is_being_stared_at`]
///   is one boolean over every nearby player, not "is *this specific* player
///   staring", and [`MobController::nearest_player`] is a position, not a
///   reference. So `isAngerInducing`/`pendingTarget` collapse to: anger-inducing
///   iff `is_being_stared_at() || angry_target().is_some()`, and the candidate
///   position is `nearest_player()` (falling back to `angry_target()` when only
///   the grudge, not a nearby player, is live). With one player in range — the
///   case every existing gaze/target gate in this crate exercises — this agrees
///   with vanilla exactly; a second player in range could pick a different one
///   than vanilla's live-reference `pendingTarget` would have, the same
///   divergence [`NearestAttackableTargetGoal`]'s own doc comment already
///   discloses for its position-valued seam.
/// * **Line of sight is not modelled**, the same disclosed gap
///   [`MobController::find_nearest_target`] already carries.
/// * **The "teleport away" blink has no landing check.** Vanilla's
///   `EnderMan::teleport` walks blocks downward looking for solid ground before
///   committing; [`MobController::teleport_to`] writes position directly (see
///   its own doc comment), so this goal picks vanilla's exact random offset
///   (±32 blocks XZ, `nextInt(64) - 32` on Y) and hands it over unchecked,
///   exactly like every other user of that primitive.
#[derive(Debug, Default)]
pub struct EndermanLookForPlayerGoal {
    /// The candidate found by `can_use`, still counting down `aggro_time`
    /// before it is promoted to a real target — vanilla's `pendingTarget`.
    pending: Option<Vec3>,
    /// The live target, once promoted — vanilla's `target` (this goal's own
    /// copy; [`MobController::set_attack_target`] is the mirror every other
    /// goal reads).
    target: Option<Vec3>,
    aggro_time: i32,
    teleport_time: i32,
}

impl EndermanLookForPlayerGoal {
    /// Creates the goal with no pending or live target.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Vanilla's `isAngerInducing` selector, narrowed to this seam's boolean
    /// gaze feed plus the persistent-grudge primitive — see this type's own
    /// doc comment.
    fn anger_inducing(mob: &mut dyn MobController) -> bool {
        mob.is_being_stared_at() || mob.angry_target().is_some()
    }

    /// The candidate position vanilla's `getNearestPlayer` would have found —
    /// see this type's own doc comment for why a position, not a reference.
    fn candidate(mob: &mut dyn MobController) -> Option<Vec3> {
        mob.nearest_player().or_else(|| mob.angry_target())
    }
}

impl Goal for EndermanLookForPlayerGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Target])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !Self::anger_inducing(mob) {
            self.pending = None;
            return false;
        }
        self.pending = Self::candidate(mob);
        self.pending.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.pending.is_some() {
            return Self::anger_inducing(mob);
        }
        let Some(_) = self.target else {
            return false;
        };
        // Vanilla's `continueAggroTargetConditions` ignores line of sight and
        // the stare test — it is the ordinary "still a valid combat target"
        // check, which this seam resolves the same way
        // `NearestAttackableTargetGoal::can_continue_to_use` re-derives a live
        // position: ask the same feed `can_use` used, and refresh from it so a
        // moving player is actually pursued rather than chased to a frozen
        // point.
        let Some(live) = Self::candidate(mob) else {
            return false;
        };
        let within = mob.follow_range();
        if distance_sqr(mob.position(), live) > within * within {
            return false;
        }
        self.target = Some(live);
        mob.set_attack_target(Some(live));
        true
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.aggro_time = 5;
        self.teleport_time = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.pending = None;
        self.target = None;
        mob.set_attack_target(None);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if let Some(pending) = self.pending {
            self.aggro_time -= 1;
            if self.aggro_time <= 0 {
                self.target = Some(pending);
                self.pending = None;
                mob.set_attack_target(self.target);
            }
            return;
        }
        let Some(target) = self.target else {
            return;
        };
        if mob.is_being_stared_at() {
            if distance_sqr(mob.position(), target) < 16.0 {
                // Vanilla `EnderMan::teleport`: a random point ±32 blocks on X
                // and Z, and `nextInt(64) - 32` on Y.
                let dx = (mob.next_f64() - 0.5) * 64.0;
                let dy = f64::from(mob.next_i32(64) - 32);
                let dz = (mob.next_f64() - 0.5) * 64.0;
                let pos = mob.position();
                mob.teleport_to(Vec3::new(pos.x + dx, pos.y + dy, pos.z + dz));
            }
            self.teleport_time = 0;
        } else if distance_sqr(mob.position(), target) > 256.0 {
            // Vanilla `this.teleportTime++ >= this.adjustedTickDelay(30)`: a
            // Java post-increment compares the value *before* incrementing,
            // then always increments — so the gate opens the tick
            // `teleport_time` is read as `30`, one tick after it is first
            // *set* to `30`, not the tick it reaches `30`. Rust has no
            // post-increment operator, so the pre-increment value has to be
            // captured explicitly or this silently becomes `>= 30` one tick
            // early.
            let before_increment = self.teleport_time;
            self.teleport_time += 1;
            if before_increment >= 30 {
                // Vanilla `EnderMan::teleportTowards`:
                //
                // ```text
                // dir = normalize(this.pos - entity.pos)   // enderman -> target, reversed
                // xx = this.getX() + (random - 0.5) * 8.0 - dir.x * 16.0
                // yy = this.getY() + (randomInt(16) - 8)   - dir.y * 16.0
                // zz = this.getZ() + (random - 0.5) * 8.0 - dir.z * 16.0
                // ```
                //
                // The offset is anchored on the enderman's **own** position
                // and displaced by a fixed 16 blocks toward the target, not
                // anchored on the target's position — a previous version of
                // this port computed `target + normalize(pos - target) * 16`,
                // which lands at a fixed 16-block radius from the target
                // regardless of how far the enderman started, and is a much
                // *smaller* jump than vanilla's whenever the starting
                // distance exceeds 16 blocks (the only case this branch ever
                // runs, since it is gated on `distance_sqr(...) > 256.0`).
                // For a 100-block gap, vanilla closes it to 84; the earlier
                // formula jumped straight to a fixed 16, an enormous
                // over-correction in the *other* direction for any distance
                // much larger than 16.
                //
                // Y uses `getY(0.5)` (a mid-body point) against the target's
                // eye height in vanilla; this seam has no eye-height API, so
                // both sides use the plain feet [`MobController::position`]
                // — a materially smaller divergence than the position bug
                // above, since it only skews the vertical component of one
                // direction vector, not the whole destination.
                let pos = mob.position();
                let dir = Vec3::new(pos.x - target.x, pos.y - target.y, pos.z - target.z);
                let len = (dir.x * dir.x + dir.y * dir.y + dir.z * dir.z).sqrt();
                if len > 1.0e-6 {
                    let (nx, ny, nz) = (dir.x / len, dir.y / len, dir.z / len);
                    let jx = (mob.next_f64() - 0.5) * 8.0;
                    let jy = f64::from(mob.next_i32(16) - 8);
                    let jz = (mob.next_f64() - 0.5) * 8.0;
                    mob.teleport_to(Vec3::new(
                        pos.x + jx - nx * 16.0,
                        pos.y + jy - ny * 16.0,
                        pos.z + jz - nz * 16.0,
                    ));
                }
                self.teleport_time = 0;
            }
        }
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
    /// never searched. The neutral family's three anger-gated rows
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

    /// Vanilla `TargetGoal.canContinueToUse`, which does three things this
    /// used to skip entirely — it held only `attack_target().is_some()`:
    ///
    /// 1. **Releases a target that left follow range**
    ///    (`if (this.mob.distanceToSqr(target) > within * within) return false;`).
    ///    Without it the acquisition cut is a one-way door: acquire at 16
    ///    blocks, then chase across the world forever.
    /// 2. **Re-writes the target every tick** (`this.mob.setTarget(target)`).
    ///    Vanilla's target is a *live entity reference*, so its position is
    ///    always current; ours is a `Vec3` frozen at acquisition, which means a
    ///    mob pursued a **moving player to where that player used to be** and
    ///    stopped. Refreshing from the same feed acquisition used is how a
    ///    position-valued seam reproduces a reference-valued one.
    /// 3. Releases when the candidate disappears (`target == null`).
    ///
    /// The one divergence worth stating: vanilla keeps the *specific* entity it
    /// acquired, while our feed answers "the nearest player", so if a second
    /// player becomes nearer mid-pursuit this switches and vanilla would not.
    /// That is a property of the seam carrying a position rather than an
    /// identity — with one player in range the two agree exactly, and the
    /// alternative (a frozen point) is wrong every time the player moves.
    ///
    /// Vanilla's `mustSee`/`unseenMemoryTicks` half of `TargetGoal.canContinueToUse`
    /// is not modelled — it needs the line of sight `NavigatingMob::find_nearest_target`
    /// explains is a ray query this seam cannot answer. `canAttack` and the team
    /// check, also part of that method, have no analogue here either.
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
        // bounded by follow range in the feed), so `TargetGoal.canContinueToUse`'s
        // own follow-range test still has to run here rather than being left
        // to the filter.
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

/// A tamed pet sits where it was told to and stops navigating.
///
/// Vanilla `SitWhenOrderedToGoal` (flags MOVE + JUMP, registered at goal
/// priority 2 by the wolf, the cat and the parrot alike).
///
/// # The clauses of vanilla's `canUse`, and which of them this implements
///
/// `canUse` is a five-clause conjunction and a conjunction is the shape that
/// hides a half-port, so each clause is named with what answers it here:
///
/// | vanilla clause | here |
/// |---|---|
/// | `isOrderedToSit() \|\| isTame()` | [`MobController::is_ordered_to_sit`] / [`MobController::is_tame`] |
/// | `!isInWater()` | [`MobController::in_water`] |
/// | `onGround()` | **not implemented** — no ground state exists on this seam |
/// | `owner == null \|\| owner.level() != level()` → `true` | [`MobController::owner_position`] being `None`, which the host feeds for an offline or out-of-world owner alike |
/// | `dist² < 144 && owner.getLastHurtByMob() != null` → `false` | **not implemented** — no owner-hurt state on this seam |
///
/// The last row is why the `Some(owner)` arm below is a bare
/// `is_ordered_to_sit()` rather than a ternary: with the owner-hurt conjunct
/// unavailable, vanilla's `? false :` branch is unreachable and transcribing the
/// ternary would be writing a dead arm. Its behavioural cost is that a pet does
/// **not** stand up to defend an owner who was just attacked nearby — it keeps
/// sitting. `onGround()`'s cost is that a pet ordered to sit mid-fall sits
/// immediately.
///
/// # Why the `None` arm returns `true` and not `is_ordered_to_sit()`
///
/// Because vanilla's does, and it is deliberate rather than a decompiler
/// artefact: having already passed `orderedToSit || isTame()`, a **tame** pet
/// with no resolvable owner sits down even though nobody told it to. That is the
/// "pets settle when you log out" behaviour. Reading the summary ("a pet sits
/// when ordered") instead of the record would have produced
/// `is_ordered_to_sit()` in both arms and silently dropped it.
#[derive(Debug, Default)]
pub struct SitWhenOrderedToGoal;

impl Goal for SitWhenOrderedToGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Jump])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        let ordered = mob.is_ordered_to_sit();
        if !ordered && !mob.is_tame() {
            return false;
        }
        if mob.in_water() {
            return false;
        }
        match mob.owner_position() {
            None => true,
            Some(_) => ordered,
        }
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.is_ordered_to_sit()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.stop_navigation();
        mob.set_in_sitting_pose(true);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_in_sitting_pose(false);
    }
}

/// A tamed pet walks after its owner.
///
/// Vanilla `FollowOwnerGoal(speedModifier, startDistance, stopDistance)` (flags
/// MOVE + LOOK). The per-species arguments are **not** uniform and the
/// difference is behavioural, not cosmetic: `Wolf` is `(1.0, 10, 2)`, `Cat` is
/// `(1.0, 10, 5)` — a cat stops five blocks out rather than at your heel — and
/// `Parrot` is `(1.0, 5, 1)`, which both starts and stops far tighter. Passing
/// one set of constants for all three would be wrong for two of them, so the
/// distances are constructor arguments.
///
/// # What is not implemented
///
/// * **The teleport.** `FollowOwnerGoal::tick` prefers
///   `tryToTeleportToOwner()` over pathing once `distanceToSqr(owner) >= 144`,
///   and that lands on `TamableAnimal.canTeleportTo`, which needs a
///   `PathType.WALKABLE` probe plus a `noCollision` box test at an arbitrary
///   candidate cell. This seam answers block questions only about the mob's own
///   feet cell, so the goal paths the whole way instead. Behavioural cost: a pet
///   left far behind never catches up across terrain a path cannot cross, where
///   vanilla's would blink to you.
/// * **`getPathfindingMalus(PathType.WATER)`** — vanilla's `start`/`stop` zero
///   and restore the water malus so a following pet will wade. No per-path-type
///   malus is settable through this seam.
///
/// `unableToMoveToOwner()`'s `isOrderedToSit()` conjunct **is** implemented, and
/// it is the load-bearing one: without it a pet ordered to sit would still be
/// dragged along behind its owner, with the two goals fighting over MOVE every
/// tick.
#[derive(Debug)]
pub struct FollowOwnerGoal {
    speed: f64,
    /// Squared `startDistance` — the goal begins beyond this.
    start_sqr: f64,
    /// Squared `stopDistance` — the goal ends within this.
    stop_sqr: f64,
    time_to_recalc: i32,
}

impl FollowOwnerGoal {
    /// Vanilla's `adjustedTickDelay(10)`. `FollowOwnerGoal` does not override
    /// `requiresUpdateEveryTick`, so `Goal.adjustedTickDelay` halves it — the
    /// re-path interval is **5** ticks, not the literal 10.
    const RECALC_TICKS: i32 = 5;

    /// Creates the goal with a species' own `(speed, startDistance,
    /// stopDistance)`. The distances are in blocks and squared here once.
    #[must_use]
    pub fn new(speed: f64, start_distance: f64, stop_distance: f64) -> Self {
        Self {
            speed,
            start_sqr: start_distance * start_distance,
            stop_sqr: stop_distance * stop_distance,
            time_to_recalc: 0,
        }
    }
}

impl Goal for FollowOwnerGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(owner) = mob.owner_position() else {
            return false;
        };
        if mob.is_ordered_to_sit() {
            return false;
        }
        distance_sqr(owner, mob.position()) >= self.start_sqr
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        // Vanilla's own first clause, and it is checked *before* the distance:
        // once the path has run out there is nothing left to continue, whatever
        // the distance says (`FollowOwnerGoal.canContinueToUse`).
        if mob.navigation_done() {
            return false;
        }
        let Some(owner) = mob.owner_position() else {
            return false;
        };
        if mob.is_ordered_to_sit() {
            return false;
        }
        distance_sqr(owner, mob.position()) > self.stop_sqr
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.time_to_recalc = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(owner) = mob.owner_position() else {
            return;
        };
        mob.look_at(owner);
        self.time_to_recalc -= 1;
        if self.time_to_recalc <= 0 {
            self.time_to_recalc = Self::RECALC_TICKS;
            mob.move_to(owner, self.speed);
        }
    }
}

/// Grazes: stands still, plays out an eat animation, and consumes the grass at
/// or under the mob's feet.
///
/// Vanilla `EatBlockGoal` (flags MOVE + LOOK + JUMP —
/// `EatBlockGoal`'s constructor), registered by the sheep at goal-priority 5
/// (`animal/sheep/Sheep.java`). **The first goal in this module whose predicate
/// reads the world**, which is why it could not exist before
/// [`MobController::block_cues_below`] was put on the seam.
///
/// Two blocks, two behaviours, and vanilla checks them in this order, in
/// `EatBlockGoal.canUse` and again in `EatBlockGoal.tick`: the block the mob
/// is standing *in* if it is `#edible_for_sheep`, otherwise the `grass_block`
/// it is standing *on*. Which one it was decides the host's mutation, so the
/// goal reports it as an [`EatenBlock`].
///
/// # Timing, and why every constant here is halved
///
/// `Goal.adjustedTickDelay` is `Goal.reducedTickDelay` — `Mth.positiveCeilDiv(t, 2)` —
/// for any goal that does not override `requiresUpdateEveryTick`, and this one
/// does not. So the jar's `1000`, `50`, `40` and `4`
/// are **500, 25, 20 and 2** ticks in practice. Transcribing the unhalved
/// numbers would make a sheep graze half as often and hold the animation twice
/// as long, which no test asserting only "it eventually ate" would catch.
#[derive(Debug, Default)]
pub struct EatBlockGoal {
    /// Counts down from [`EAT_ANIMATION_TICKS`](Self::EAT_ANIMATION_TICKS);
    /// `> 0` is "still eating" (the `eatAnimationTick` field and
    /// `EatBlockGoal.canContinueToUse`).
    eat_animation_tick: i32,
}

impl EatBlockGoal {
    /// `adjustedTickDelay(40)` — the eat animation's length in ticks
    /// (`EatBlockGoal.EAT_ANIMATION_TICKS` and `EatBlockGoal.start`).
    pub const EAT_ANIMATION_TICKS: i32 = 20;

    /// `adjustedTickDelay(1000)` — an adult's mean interval between grazing
    /// attempts (`EatBlockGoal.canUse`).
    pub const ADULT_INTERVAL: i32 = 500;

    /// `adjustedTickDelay(50)` — a baby's, which grazes 20× as often.
    pub const BABY_INTERVAL: i32 = 25;

    /// `adjustedTickDelay(4)` — the tick *within* the animation on which the
    /// block is actually consumed (`EatBlockGoal.tick`). Note this is
    /// near the animation's **end**, so a goal interrupted early eats nothing.
    pub const CONSUME_AT: i32 = 2;

    /// Creates the goal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ticks left in the eat animation (vanilla's `EatBlockGoal.getEatAnimationTick`).
    /// Vanilla drives the head-down pose from this, broadcast as entity event
    /// `10` in `EatBlockGoal.start` — a wire concern this crate cannot reach,
    /// so a host that wants the animation reads it here.
    #[must_use]
    pub fn eat_animation_tick(&self) -> i32 {
        self.eat_animation_tick
    }
}

impl Goal for EatBlockGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look, Flag::Jump])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        let interval = if mob.is_baby() {
            Self::BABY_INTERVAL
        } else {
            Self::ADULT_INTERVAL
        };
        if mob.next_i32(interval) != 0 {
            return false;
        }
        // `EatBlockGoal.canUse`: `IS_EDIBLE.test(getBlockState(pos)) ? true :
        // getBlockState(pos.below()).is(GRASS_BLOCK)`.
        mob.block_cues_at_feet().edible_for_sheep || mob.block_cues_below().grass_block
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        self.eat_animation_tick > 0
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.eat_animation_tick = Self::EAT_ANIMATION_TICKS;
        mob.stop_navigation();
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        self.eat_animation_tick = 0;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.eat_animation_tick = (self.eat_animation_tick - 1).max(0);
        if self.eat_animation_tick != Self::CONSUME_AT {
            return;
        }
        // Re-read rather than trusting `can_use`'s answer: `EatBlockGoal.tick`
        // re-tests both blocks here because the mob has been standing on them
        // for 20 ticks and something else may have changed them.
        if mob.block_cues_at_feet().edible_for_sheep {
            mob.ate(EatenBlock::AtFeet);
        } else if mob.block_cues_below().grass_block {
            mob.ate(EatenBlock::Below);
        }
    }
}

/// Steers a pillager patrol across the map, leader and followers alike.
///
/// Vanilla `PatrollingMonster.LongDistancePatrolGoal` (flag MOVE), registered
/// at goal priority 4 for every `PatrollingMonster` in
/// `PatrollingMonster.registerGoals` — the pillager only, today.
///
/// # What is ported, and what is not
///
/// * **The leader's own long-distance steering** is ported faithfully: the
///   lateral-offset waypoint formula and the "close enough, pick
///   a new far-off target" repick, both in `LongDistancePatrolGoal.tick`, are
///   pure position arithmetic this seam can already answer through
///   [`MobController::position`]/[`MobController::move_to`].
/// * **The companion census is not, and cannot be without a new seam
///   primitive.** Vanilla's own `LongDistancePatrolGoal.findPatrolCompanions` is a
///   `getEntitiesOfClass` query with no analogue on [`MobController`] — the
///   trait hands goals answers about *this* mob, never a population (see
///   `roster`'s own module doc, "not perception data"). So a **follower**
///   never runs this goal's leader branch at all: instead of vanilla's
///   leader-pushes-to-nearby-companions data flow, a follower here *pulls*
///   [`MobController::patrol_group_target`] — the host's answer to "what does
///   my patrol's leader currently want" — and then runs the **same**
///   lateral-offset movement formula toward it, from its own position. The
///   visible result is the same shape (a loose cluster marching toward one
///   shared destination) through a different data path: vanilla's followers
///   track the leader's *immediate* 10-block waypoint and go stale the moment
///   they leave its search radius; these track the leader's *long-distance*
///   target continuously, which is the more forgiving direction to diverge
///   in — a straggler still knows where the patrol is headed.
/// * **`LongDistancePatrolGoal.moveRandomly`'s fallback** is ported, using
///   this seam's own random draw in place of vanilla's `RandomSource`.
/// * **Vanilla's `hasControllingPassenger()` clause is not modelled** — no
///   passenger state crosses this seam (see `docs/pillager-patrols.md`).
#[derive(Debug)]
pub struct LongDistancePatrolGoal {
    /// `speedModifier` — a non-leader's pace.
    follower_speed: f64,
    /// `leaderSpeedModifier` — faster than a follower's, so the leader does
    /// not get boxed in by its own patrol.
    leader_speed: f64,
    /// Ticks remaining after a failed path attempt before this goal may run
    /// again — vanilla's `NAVIGATION_FAILED_COOLDOWN` (200).
    cooldown: i32,
}

impl LongDistancePatrolGoal {
    /// Vanilla's `LongDistancePatrolGoal.NAVIGATION_FAILED_COOLDOWN`.
    const NAVIGATION_FAILED_COOLDOWN: i32 = 200;
    /// Vanilla's `closerToCenterThan(…, 10.0)` repick threshold, squared.
    const REPICK_RANGE_SQR: f64 = 100.0;
    /// The half-width of vanilla's `moveRandomly` candidate box
    /// (`-8 + nextInt(16)`, i.e. `[-8, 7]`).
    const RANDOM_MOVE_SPREAD: i32 = 16;
    /// The half-width of `findPatrolTarget`'s far-off offset
    /// (`-500 + nextInt(1000)`, i.e. `[-500, 499]`).
    const FAR_TARGET_SPREAD: i32 = 1000;

    /// Creates the goal with `(follower_speed, leader_speed)` — vanilla's own
    /// constructor order is `(speedModifier, leaderSpeedModifier)`
    /// (`PatrollingMonster.registerGoals` calls `new LongDistancePatrolGoal<>(this, 0.7,
    /// 0.595)`), and `speedModifier` is what `tick` uses whenever `!patrolLeader`
    /// — a follower. Worth naming explicitly because it reads backwards at a
    /// glance: **the leader is the slower of the two** (`0.595` against a
    /// follower's `0.7`), so stragglers can close the gap and the patrol stays
    /// clustered instead of stringing out behind whoever is out front.
    #[must_use]
    pub fn new(follower_speed: f64, leader_speed: f64) -> Self {
        Self {
            follower_speed,
            leader_speed,
            cooldown: 0,
        }
    }
}

impl Goal for LongDistancePatrolGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !mob.is_patrolling() || mob.attack_target().is_some() {
            return false;
        }
        // A leader's own `patrol_target` must already exist — nothing else
        // ever sets one for it. A **follower**'s does not yet exist on its
        // very first usable tick: `tick`'s own adoption step is what copies
        // `patrol_group_target` into `patrol_target`, and that step cannot
        // run before `can_use` says yes. So a follower is usable the moment
        // the host census hands it *either* value, matching vanilla's own
        // shape where a follower's `hasPatrolTarget()` only ever becomes true
        // because something external (there, the leader's tick; here, the
        // host census) wrote it.
        if mob.is_patrol_leader() {
            mob.patrol_target().is_some()
        } else {
            mob.patrol_target().is_some() || mob.patrol_group_target().is_some()
        }
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.can_use(mob)
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return;
        }
        if !mob.navigation_done() {
            return;
        }
        let is_leader = mob.is_patrol_leader();
        if !is_leader {
            // Vanilla's leader pushes a near-term waypoint out to nearby
            // companions; this pulls the leader's own long-distance target
            // instead — see this goal's own doc comment for why.
            if let Some(group_target) = mob.patrol_group_target() {
                mob.set_patrol_target(Some(group_target));
            }
        }
        let Some(target) = mob.patrol_target() else {
            return;
        };
        let self_pos = mob.position();
        if is_leader && distance_sqr(target, self_pos) < Self::REPICK_RANGE_SQR {
            // `findPatrolTarget`: a fresh far-off offset from the mob's
            // current position, not from the old target.
            let dx = f64::from(mob.next_i32(Self::FAR_TARGET_SPREAD) - 500);
            let dz = f64::from(mob.next_i32(Self::FAR_TARGET_SPREAD) - 500);
            mob.set_patrol_target(Some(Vec3::new(self_pos.x + dx, self_pos.y, self_pos.z + dz)));
            return;
        }
        // `distance.yRot(90.0F).scale(0.4)`: rotating the mob→target vector
        // 90° around Y leaves Y untouched and maps `(x, z) -> (z, -x)`
        // (`Vec3.yRot`'s own matrix at a quarter turn), then the rotated
        // vector is shrunk to two-fifths and added back onto the target —
        // this is the lateral wobble that keeps a patrol marching in a loose
        // line rather than nose-to-tail.
        let dist_x = self_pos.x - target.x;
        let dist_z = self_pos.z - target.z;
        let long_distance_target = Vec3::new(
            target.x + dist_z * 0.4,
            target.y,
            target.z - dist_x * 0.4,
        );
        let to_target_x = long_distance_target.x - self_pos.x;
        let to_target_z = long_distance_target.z - self_pos.z;
        let len = to_target_x.hypot(to_target_z);
        let move_target = if len > 1e-9 {
            Vec3::new(
                self_pos.x + to_target_x / len * 10.0,
                self_pos.y,
                self_pos.z + to_target_z / len * 10.0,
            )
        } else {
            self_pos
        };
        let speed = if is_leader {
            self.leader_speed
        } else {
            self.follower_speed
        };
        if !mob.move_to(move_target, speed) {
            // `moveRandomly`.
            let rx = f64::from(mob.next_i32(Self::RANDOM_MOVE_SPREAD) - 8);
            let rz = f64::from(mob.next_i32(Self::RANDOM_MOVE_SPREAD) - 8);
            mob.move_to(
                Vec3::new(self_pos.x + rx, self_pos.y, self_pos.z + rz),
                self.follower_speed,
            );
            self.cooldown = Self::NAVIGATION_FAILED_COOLDOWN;
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
        stared_at: bool,
        looked_at: Option<Vec3>,
        angry: Option<Vec3>,
        teleported: Vec<Vec3>,
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
        fn look_at(&mut self, t: Vec3) {
            self.looked_at = Some(t);
        }
        fn look_toward(&mut self, _dx: f64, _dz: f64) {}
        fn is_being_stared_at(&self) -> bool {
            self.stared_at
        }
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
        fn angry_target(&self) -> Option<Vec3> {
            self.angry
        }
        fn teleport_to(&mut self, target: Vec3) {
            self.teleported.push(target);
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

    // ---- EndermanFreezeWhenLookedAt --------------------------------------

    /// The discriminating pair, at goal level: identical position and target,
    /// only `is_being_stared_at` differs. A distance-only implementation (the
    /// wrong one someone could plausibly ship instead of a real gaze test)
    /// cannot produce this split, because both mobs stand at the same spot.
    #[test]
    fn freezes_only_while_its_target_stares_within_range() {
        let mut goal = EndermanFreezeWhenLookedAt::new();
        let mut watched = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(6.0, 64.0, 0.0)), // distSqr 36 <= 256
            stared_at: true,
            ..Default::default()
        };
        assert!(
            goal.can_use(&mut watched),
            "a target within 16 blocks that is staring must make the goal eligible"
        );

        let mut unwatched = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(6.0, 64.0, 0.0)), // identical position and target
            stared_at: false,
            ..Default::default()
        };
        assert!(
            !goal.can_use(&mut unwatched),
            "the identical target at the identical distance must NOT freeze the \
             goal when is_being_stared_at() is false — if this fires, the goal \
             degenerated into a distance check"
        );
    }

    #[test]
    fn freeze_ignores_a_staring_target_beyond_sixteen_blocks() {
        // `EnderMan.EndermanFreezeWhenLookedAt.canUse`: the
        // `distanceToSqr(target) > 256.0` branch returns `false` before even
        // consulting `isBeingStaredBy`, so a stare from far away never freezes
        // the goal — a control on the 16-block gate, independent of the gaze
        // boolean.
        let mut goal = EndermanFreezeWhenLookedAt::new();
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(17.0, 64.0, 0.0)), // distSqr 289 > 256
            stared_at: true,
            ..Default::default()
        };
        assert!(!goal.can_use(&mut mob));
    }

    #[test]
    fn freeze_stops_navigation_and_looks_at_its_target() {
        let mut goal = EndermanFreezeWhenLookedAt::new();
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            attack: Some(Vec3::new(6.0, 64.0, 0.0)),
            stared_at: true,
            ..Default::default()
        };
        assert!(goal.can_use(&mut mob));
        goal.start(&mut mob);
        assert_eq!(
            mob.stopped_navigation, 1,
            "vanilla's start() calls navigation.stop() unconditionally"
        );
        goal.tick(&mut mob);
        assert_eq!(
            mob.looked_at,
            Some(Vec3::new(6.0, 64.0, 0.0)),
            "tick() must aim the look control at the remembered target every tick"
        );
    }

    // ---- EndermanLookForPlayerGoal ---------------------------------------
    //
    // Unlike `EndermanFreezeWhenLookedAt` above, nothing previously exercised
    // this goal's own state machine directly — only the roster's multiset
    // gate (which checks the *table row*, not runtime behaviour) and, at a
    // much higher level, `lodestone-server`'s `feed_perception` wiring. This
    // block is the first thing that drives `can_use`/`start`/`tick` for real
    // and predicts an exact promotion tick and an exact teleport
    // destination, the same "magnitude, not just direction" standard the
    // rest of this file already holds combat and movement goals to.

    /// A stared-at candidate is not acquired immediately — vanilla's
    /// `aggroTime = adjustedTickDelay(5)` must count all the way down first.
    /// Ticks 1-4 must leave `attack_target` untouched; only the 5th promotes
    /// it, at which point it must be exactly the candidate `can_use` found
    /// (`nearest_player`, since nothing here is angry yet).
    #[test]
    fn look_for_player_acquires_only_after_the_five_tick_aggro_delay() {
        let mut goal = EndermanLookForPlayerGoal::new();
        let candidate = Vec3::new(6.0, 64.0, 0.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            player: Some(candidate),
            stared_at: true,
            ..Default::default()
        };
        assert!(
            goal.can_use(&mut mob),
            "a stared-at mob with a nearby player must find a pending candidate"
        );
        goal.start(&mut mob);
        assert_eq!(
            mob.attack, None,
            "start() must not acquire immediately — only arm the aggro delay"
        );
        for n in 1..=4 {
            goal.tick(&mut mob);
            assert_eq!(mob.attack, None, "must still be counting down at tick {n}");
        }
        goal.tick(&mut mob);
        assert_eq!(
            mob.attack,
            Some(candidate),
            "the 5th tick must promote the pending candidate to the live attack target"
        );
    }

    /// The anger-gated half of `isAngerInducing`: with no stare at all, a mob
    /// already holding a persistent grudge (`angry_target`) still finds and
    /// acquires a candidate — vanilla's `isAngryAt` disjunct in
    /// `isAngerInducing`, ported as `is_being_stared_at() ||
    /// angry_target().is_some()`.
    #[test]
    fn look_for_player_acquires_from_a_grudge_alone_with_no_stare() {
        let mut goal = EndermanLookForPlayerGoal::new();
        let grudge = Vec3::new(9.0, 64.0, 0.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            player: None,
            stared_at: false,
            angry: Some(grudge),
            ..Default::default()
        };
        assert!(
            goal.can_use(&mut mob),
            "an angry mob with no player in view must still find its grudge holder"
        );
        goal.start(&mut mob);
        for _ in 0..5 {
            goal.tick(&mut mob);
        }
        assert_eq!(mob.attack, Some(grudge));
    }

    /// Once acquired, a target staring back from close range (`distanceToSqr
    /// < 16.0`, i.e. under 4 blocks) triggers the "teleport away" blink —
    /// and the *promotion* tick itself must not also fire it (vanilla's
    /// `tick()` is `if (pendingTarget != null) {...} else {...}`, an
    /// either/or, not a fallthrough).
    #[test]
    fn look_for_player_teleports_away_when_stared_at_up_close() {
        let mut goal = EndermanLookForPlayerGoal::new();
        let close = Vec3::new(3.0, 64.0, 0.0); // distSqr 9 < 16
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            player: Some(close),
            stared_at: true,
            ..Default::default()
        };
        goal.can_use(&mut mob);
        goal.start(&mut mob);
        for _ in 0..5 {
            goal.tick(&mut mob);
        }
        assert_eq!(mob.attack, Some(close), "precondition: must have acquired");
        assert!(
            mob.teleported.is_empty(),
            "the promotion tick itself must not also run the teleport branch"
        );

        goal.tick(&mut mob);
        assert_eq!(
            mob.teleported.len(),
            1,
            "a close, still-staring target must trigger exactly one blink"
        );
    }

    /// A target far outside follow-adjacent range (`distanceToSqr > 256.0`,
    /// over 16 blocks) that is *not* staring triggers "teleport towards"
    /// once the 30-tick throttle opens — never before it, and landing at the
    /// **exact** predicted destination (vanilla's own jitter formula, fed
    /// through `ScriptMob`'s fixed RNG stubs: `next_f64() == 0.0` always,
    /// `next_i32(_) == 0` always).
    ///
    /// Hand-derived: enderman at `(0, 64, 0)`, target at `(20, 64, 0)`.
    /// `dir = normalize(pos - target) = (-1, 0, 0)`. `jx = (0.0 - 0.5) * 8.0
    /// = -4.0`, `jy = 0 - 8 = -8`, `jz = -4.0`. `x' = 0 + (-4.0) - (-1 * 16)
    /// = 12.0`. `y' = 64 + (-8) - 0 = 56.0`. `z' = 0 + (-4.0) - 0 = -4.0`.
    #[test]
    fn look_for_player_teleports_towards_after_the_throttle_once_out_of_range() {
        let mut goal = EndermanLookForPlayerGoal::new();
        let target = Vec3::new(20.0, 64.0, 0.0); // distSqr 400 > 256
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            player: Some(target),
            stared_at: true,
            ..Default::default()
        };
        goal.can_use(&mut mob);
        goal.start(&mut mob);
        for _ in 0..5 {
            goal.tick(&mut mob);
        }
        assert_eq!(mob.attack, Some(target), "precondition: must have acquired");

        // No longer being watched: the far branch, not the close one.
        mob.stared_at = false;
        for n in 0..30 {
            goal.tick(&mut mob);
            assert!(
                mob.teleported.is_empty(),
                "must not teleport before the throttle elapses (tick {n})"
            );
        }
        goal.tick(&mut mob); // the 31st far-branch tick opens the throttle
        assert_eq!(mob.teleported.len(), 1, "must teleport exactly once once the throttle opens");
        let landed = mob.teleported[0];
        let expected = Vec3::new(12.0, 56.0, -4.0);
        assert!(
            (landed.x - expected.x).abs() < 1e-9
                && (landed.y - expected.y).abs() < 1e-9
                && (landed.z - expected.z).abs() < 1e-9,
            "expected {expected:?}, got {landed:?}"
        );
    }

    /// Predicts the **wrong** hypothesis explicitly and requires the real
    /// output to land on the right one, not merely differ from zero —
    /// CLAUDE.md's magnitude species. The wrong hypothesis is the exact
    /// formula this file used to compute (`target + normalize(pos - target)
    /// * 16`, anchored on the *target's* position and carrying no jitter):
    /// for this test's inputs that lands at `(4.0, 64.0, 0.0)`, a materially
    /// different point from the correct, enderman-anchored `(12.0, 56.0,
    /// -4.0)` the primary test above pins. Same scenario as that test,
    /// re-run here only to keep this assertion self-contained.
    #[test]
    fn look_for_player_teleport_towards_does_not_land_on_the_target_anchored_formula() {
        let mut goal = EndermanLookForPlayerGoal::new();
        let target = Vec3::new(20.0, 64.0, 0.0);
        let mut mob = ScriptMob {
            pos: Vec3::new(0.0, 64.0, 0.0),
            player: Some(target),
            stared_at: true,
            ..Default::default()
        };
        goal.can_use(&mut mob);
        goal.start(&mut mob);
        for _ in 0..5 {
            goal.tick(&mut mob);
        }
        mob.stared_at = false;
        for _ in 0..31 {
            goal.tick(&mut mob);
        }
        let landed = mob.teleported[0];
        let wrong_hypothesis = Vec3::new(4.0, 64.0, 0.0);
        let dist = ((landed.x - wrong_hypothesis.x).powi(2)
            + (landed.y - wrong_hypothesis.y).powi(2)
            + (landed.z - wrong_hypothesis.z).powi(2))
        .sqrt();
        assert!(
            dist > 1.0,
            "landed at {landed:?}, which is suspiciously close to the old \
             target-anchored (and un-jittered) formula's {wrong_hypothesis:?} \
             — this must be the enderman-anchored formula, not the one it replaced"
        );
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
