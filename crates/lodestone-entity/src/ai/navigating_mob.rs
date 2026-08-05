//! A reference mob composition that wires the goal scheduler to the *real*
//! pathfinder and navigator.
//!
//! Everywhere else the [`MobController`] seam is filled by a test fake whose
//! `move_to` just records a call and returns `true` — so a goal deciding to move
//! has never once driven an A\* search or followed a computed path. The goal
//! scheduler ([`GoalSelector`](super::GoalSelector)) is proven hermetically and
//! the [`PathFinder`] is proven against a live zombie, but *nothing composes
//! them*: they are two islands joined by a seam a fake always stubs. That is the
//! same shape as a decoder the adapter never calls.
//!
//! [`NavigatingMob`] is the composition that closes the gap. Its `move_to` runs
//! the real [`PathFinder`] over the [`PathWorld`] seam, [`advance`] follows the
//! resulting [`Path`](crate::pathfinding::Path) one step through the real
//! [`PathNavigator`], and the whole thing is drivable by a `GoalSelector`. It
//! owns only `lodestone-entity` parts over the version-free `PathWorld` seam, so
//! it introduces no world, physics or version dependency.
//!
//! The follower is deliberately **kinematic**, not the physics integrator: each
//! tick it steps toward the next waypoint at a caller-supplied blocks/tick
//! (derived from the mob's movement-speed attribute). The exact
//! ground-speed→velocity mapping is `lodestone-physics`' job. What this
//! composition proves is the goal→navigation→movement *wiring* and the
//! *topological* behaviour the seam's fakes could never show: that a
//! goal-driven mob actually invokes A\*, reaches its target, and detours an
//! unjumpable fence instead of walking through it.

use lodestone_model::{BlockPos, Vec3};

use super::goal::GoalSelector;
use super::mob::{MobController, ProjectileLaunch};
use crate::pathfinding::{
    MobShape, PathFinder, PathNavigator, PathParams, PathStart, PathType, PathWorld,
};

/// Vanilla `Animal::setInLove`'s love-mode duration, in ticks
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/animal/Animal.java:174`,
/// `this.inLove = 600;`).
pub const LOVE_TICKS: i32 = 600;

/// Vanilla `AgeableMob.BABY_START_AGE`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/AgeableMob.java:31`). The
/// age timer a freshly bred (or otherwise spawned) baby starts at; it counts
/// up by one every tick until it reaches `0` (adult).
pub const BABY_START_AGE: i32 = -24_000;

/// Vanilla `Animal.PARENT_AGE_AFTER_BREEDING`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/animal/Animal.java:44`).
/// The post-breeding cooldown applied to both parents' age timer; it counts
/// down by one every tick until it reaches `0` (breedable again).
pub const PARENT_AGE_AFTER_BREEDING: i32 = 6000;

/// Vanilla `Creeper.DEFAULT_MAX_SWELL`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/monster/Creeper.java:51`,
/// `private static final short DEFAULT_MAX_SWELL = 30;`). The fuse length in
/// ticks: [`swell`](NavigatingMob::swell) climbs by
/// [`swell_dir`](MobController::swell_dir) once per [`advance`](NavigatingMob::advance)
/// call, and reaching this value is detonation
/// (`Creeper.java:144-146`, `explodeCreeper()`).
pub const MAX_SWELL: i32 = 30;

/// How long a mob remembers who hurt it, in ticks. Vanilla `LivingEntity.tick`
/// clears `lastHurtByMob` once the record ages past this
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/LivingEntity.java:493`,
/// `else if (this.tickCount - this.lastHurtByMobTimestamp > 100)`), which is
/// what bounds `HurtByTargetGoal`'s retaliation window
/// (`ai/goal/target/HurtByTargetGoal.java:34-36` reads exactly that pair).
pub const LAST_HURT_BY_TICKS: i32 = 100;

/// How long a mob stays panicked after taking damage, in ticks. Vanilla's
/// `PanicGoal.shouldPanic` (`ai/goal/PanicGoal.java:61-63`) tests
/// `getLastDamageSource() != null`, and `getLastDamageSource` self-clears once
/// the stamp ages past this
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/LivingEntity.java:1420-1421`,
/// `if (this.level().getGameTime() - this.lastDamageStamp > 40L)`).
///
/// Note this is a **different, shorter** window than [`LAST_HURT_BY_TICKS`]:
/// vanilla panics off the *damage source* and retaliates off the *attacking
/// mob*, two independently-decaying records, so a mob keeps chasing its
/// attacker for 60 ticks after it stops fleeing. Collapsing them into one
/// timer would be a silent behaviour change, not a simplification.
pub const PANIC_DAMAGE_TICKS: i32 = 40;

/// A tiny deterministic RNG (SplitMix64) so a `NavigatingMob` needs no `rand`
/// dependency and its stroll behaviour is reproducible in tests.
#[derive(Debug, Clone)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A float in `[0, 1)`.
    fn next_unit(&mut self) -> f64 {
        // 53-bit mantissa, matching the usual `nextDouble` construction.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A reference mob that composes a [`GoalSelector`] with the real
/// [`PathFinder`] / [`PathNavigator`] over a [`PathWorld`].
///
/// Drive it each tick with [`NavigatingMob::tick`], which runs the goals (they
/// call back into this mob's [`MobController`] impl) and then advances the
/// follower one kinematic step.
pub struct NavigatingMob<'w> {
    world: &'w dyn PathWorld,
    shape: MobShape,
    finder: PathFinder,
    navigator: PathNavigator,
    pos: Vec3,
    /// Blocks travelled per tick along the path (kinematic follower speed).
    step_per_tick: f64,
    rng: SplitMix64,
    attack_target: Option<Vec3>,
    /// The block the current path was computed toward, so `move_to` reuses the
    /// active path instead of recomputing every tick (vanilla `moveTo` reuse).
    active_target_block: Option<BlockPos>,
    last_look: Option<Vec3>,
    jumping: bool,
    attacks: Vec<Vec3>,
    /// Projectile launches a ranged goal asked for, awaiting a host drain
    /// (issue #227). The mirror of [`attacks`](Self::attacks): this crate can
    /// resolve neither into a real world effect, so both accumulate here for
    /// whoever owns the entity ids.
    launches: Vec<ProjectileLaunch>,
    move_calls: u32,
    path_searches: u32,
    /// Monotonic tick counter (advanced once per [`advance`]/[`tick`]), used to
    /// throttle recomputation the way vanilla's game clock does.
    tick_count: u64,
    /// The tick a same-destination re-search last ran, so a wedged mob does not
    /// recompute A\* every tick (vanilla `PathNavigation.recomputePath` refuses
    /// to recompute within 20 ticks — `MAX_TIME_RECOMPUTE`).
    last_search_tick: Option<u64>,
    /// The actual position delta applied on the last [`advance`], i.e. the mob's
    /// velocity in **blocks per tick** (vanilla `getDeltaMovement`). Zero when the
    /// follower did not move this tick.
    velocity: Vec3,
    /// The mob's body yaw in degrees, derived from its horizontal movement
    /// direction and retained across idle ticks (vanilla `yBodyRot`).
    body_yaw: f32,
    /// Vanilla `Animal.inLove`: remaining love-mode ticks, set to
    /// [`LOVE_TICKS`] by [`set_in_love`](Self::set_in_love) and decremented
    /// once per [`advance`](Self::advance) regardless of what any goal does
    /// (vanilla `Animal::aiStep` ages it unconditionally). `> 0` is
    /// "in love" ([`MobController::is_in_love`]).
    love_ticks: i32,
    /// Host injection point, refreshed once per tick before
    /// [`tick`](Self::tick)/[`advance`](Self::advance) runs: the current
    /// position of the breeding partner this mob should pursue, or `None` if
    /// no eligible partner exists right now. `lodestone-entity` has no
    /// concept of a *population* of mobs, so — exactly as
    /// [`MobController::find_love_partner`]'s doc comment specifies — the
    /// host performs vanilla's `getFreePartner`/`canMate` search across
    /// siblings and hands back only the answer. Both
    /// [`find_love_partner`](MobController::find_love_partner) and
    /// [`love_partner_position`](MobController::love_partner_position) read
    /// this same field: the host is expected to clear it the instant the
    /// chosen partner becomes ineligible, which is what ends
    /// [`BreedGoal`](super::goals::BreedGoal).
    partner_candidate: Option<Vec3>,
    /// Set by [`MobController::breed`] the tick a `BreedGoal` connects;
    /// drained by [`take_bred`](Self::take_bred) so a host can resolve the
    /// intent into an actual child spawn (this seam has no notion of the
    /// partner's identity or of creating a new entity, only of the *event*
    /// happening).
    bred: bool,
    /// Vanilla `AgeableMob.age`
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/AgeableMob.java:37`):
    /// negative while a baby (ticks up toward `0`), positive as the
    /// post-breeding parent cooldown (ticks down toward `0`), `0` for an
    /// adult with no cooldown. [`MobController::is_baby`] is `age < 0`.
    age: i32,
    /// Vanilla `AgeableMob.AGE_LOCKED`: freezes [`age`](Self::age) from
    /// advancing at all while `true`. The golden-dandelion interaction that
    /// sets this in vanilla is not implemented here, but the freeze itself is
    /// honoured so a host that does implement it gets correct behaviour.
    age_locked: bool,
    /// Host injection point, refreshed once per tick: the position of the
    /// nearest eligible adult of this mob's own kind, or `None`. Drives
    /// [`FollowParentGoal`](super::goals::FollowParentGoal) through
    /// [`MobController::parent_position`], the same host-computes-the-filter
    /// shape as [`partner_candidate`](Self::partner_candidate).
    parent_candidate: Option<Vec3>,
    /// Vanilla `Creeper.swellDir` (`DATA_SWELL_DIR`, defaults to `-1` —
    /// `Creeper.java:100`, `entityData.define(DATA_SWELL_DIR, -1)`). Set by
    /// [`SwellGoal`](super::goals::SwellGoal) through
    /// [`MobController::set_swell_dir`], or forced to `1` every
    /// [`advance`](Self::advance) while [`ignited`](Self::ignited) is `true`
    /// (`Creeper.java:129-131`). `> 0` climbs [`swell`](Self::swell) toward
    /// [`MAX_SWELL`]; `<= 0` lets it fall back toward zero.
    swell_dir: i32,
    /// Vanilla `Creeper.swell`: the live fuse counter, integrated once per
    /// tick in [`advance`](Self::advance) unconditionally — exactly like
    /// [`age`](Self::age)/[`love_ticks`](Self::love_ticks) — regardless of
    /// whether any goal ran this tick. This is the entity's own `tick()`
    /// (`Creeper.java:126-151`), distinct from `SwellGoal`, which only ever
    /// decides the *direction*.
    swell: i32,
    /// Vanilla `Creeper.isIgnited()` / `DATA_IS_IGNITED`. `true` forces
    /// [`swell_dir`](Self::swell_dir) to `1` every tick regardless of what
    /// [`SwellGoal`](super::goals::SwellGoal) would otherwise choose
    /// (`Creeper.java:129-131`). Set by [`ignite`](Self::ignite); no
    /// production caller wires a flint-and-steel/fire-charge interaction to
    /// it yet (`Creeper.java:210-228`'s `mobInteract`) — that is a separate,
    /// disclosed gap, not modelled here.
    ignited: bool,
    /// Drained by [`take_detonated`](Self::take_detonated): `true` for
    /// exactly one call, the tick [`swell`](Self::swell) first reaches
    /// [`MAX_SWELL`] (`Creeper.java:144-146`, `explodeCreeper()`). Mirrors
    /// [`bred`](Self::bred)'s "flag the host drains" shape — this seam has no
    /// notion of triggering an explosion, only of the *event* happening.
    detonated: bool,
    /// Host injection point, refreshed once per tick: the nearest player's
    /// position, or `None` when no player is in perception range. Drives
    /// [`MobController::nearest_player`] and therefore
    /// [`LookAtPlayerGoal`](super::goals::LookAtPlayerGoal).
    ///
    /// Host-injected for the same reason as
    /// [`partner_candidate`](Self::partner_candidate): `lodestone-entity` has
    /// no concept of a *player*, let alone a population of them, so vanilla's
    /// `level.getNearestPlayer(lookAtContext, mob, x, eyeY, z)`
    /// (`ai/goal/LookAtPlayerGoal.java:62`) is the host's search to run. The
    /// goal still applies its own `lookDistance` cut-off on top, so a host
    /// that over-reports is merely wasteful, not wrong.
    nearest_player: Option<Vec3>,
    /// Host injection point, refreshed once per tick: the position of a nearby
    /// entity currently tempting this mob, or `None`. Drives
    /// [`MobController::temptation`] and therefore
    /// [`TemptGoal`](super::goals::TemptGoal).
    ///
    /// The host owns **both** halves of vanilla's test: the range (an
    /// attribute, `Attributes.TEMPT_RANGE`, default `10.0` —
    /// `ai/attributes/Attributes.java:107`) and the item predicate, which in
    /// 26.2 is an item *tag* per species (`pig_food` is 3 items,
    /// `chicken_food` is 6) rather than the single item older versions used.
    /// Resolving those tags is a data-generation job this crate deliberately
    /// does not do; see `docs/mob-perception.md`.
    temptation: Option<Vec3>,
    /// Host injection point, refreshed once per tick: the position of a nearby
    /// entity this mob wants to flee, or `None`. Drives
    /// [`MobController::avoid_threat`] and therefore
    /// [`AvoidEntityGoal`](super::goals::AvoidEntityGoal).
    ///
    /// Vanilla's avoid set is per-species and per-goal-instance (a creeper
    /// registers two separate `AvoidEntityGoal`s, `Ocelot` and `Cat`, both at
    /// `6.0F` — `monster/Creeper.java:67-68`), so the *class filter* is the
    /// host's, exactly like the temptation predicate above.
    avoid_threat: Option<Vec3>,
    /// Host injection point: vanilla `Mob.noActionTime`
    /// (`Mob.java:717`, `this.noActionTime++`, reset to `0` at `:707`/`:711`).
    /// Read by [`RandomStrollGoal`](super::goals::RandomStrollGoal)'s idle
    /// suppression, which yields at `>= 100` (`ai/goal/RandomStrollGoal.java:43`).
    ///
    /// Injected rather than counted here because the *reset* conditions are
    /// the host's: vanilla zeroes it when a player is within the immune
    /// radius, which is population knowledge this crate does not have. A host
    /// that never sets it leaves the vanilla-default `0`, i.e. stroll is never
    /// idle-suppressed — which is exactly the permissive-direction bug this
    /// field exists to fix, so it is worth stating that a silent `0` here is
    /// not neutral.
    no_action_time: i32,
    /// The position of the mob that most recently damaged this one, retained
    /// for [`LAST_HURT_BY_TICKS`] after the hit. Recorded by
    /// [`note_hurt`](Self::note_hurt), decayed unconditionally in
    /// [`advance`](Self::advance), and read by
    /// [`MobController::last_hurt_by`] — which is what
    /// [`HurtByTargetGoal`](super::goals::HurtByTargetGoal) retaliates against.
    last_hurt_by: Option<Vec3>,
    /// Ticks remaining on [`last_hurt_by`](Self::last_hurt_by). Vanilla stores
    /// the *timestamp* and compares against `tickCount`
    /// (`LivingEntity.java:493`); a countdown is the same thing with no need
    /// for a shared clock, and it decays in `advance` alongside
    /// [`love_ticks`](Self::love_ticks) for the same reason — vanilla ages it
    /// every tick regardless of whether any goal ran.
    hurt_by_ticks: i32,
    /// Ticks remaining on "took damage recently", the panic window
    /// ([`PANIC_DAMAGE_TICKS`]). Set by [`note_hurt`](Self::note_hurt) for
    /// **every** hit, including one with no identifiable attacker, because
    /// vanilla's `shouldPanic` reads the damage *source* rather than the
    /// attacking mob (`ai/goal/PanicGoal.java:61-63`). Read by
    /// [`MobController::is_panicking`].
    damage_ticks: i32,
}

/// Minecraft body yaw (degrees) for a horizontal movement delta: 0 = +Z (south),
/// −90 = +X (east), 90 = −X (west), 180 = −Z (north). Mirrors vanilla's
/// `atan2(dz, dx) * 180/PI - 90` idiom used when a mob faces its motion.
fn movement_yaw(dx: f64, dz: f64) -> f32 {
    (dz.atan2(dx).to_degrees() - 90.0) as f32
}

impl std::fmt::Debug for NavigatingMob<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavigatingMob")
            .field("shape", &self.shape)
            .field("pos", &self.pos)
            .field("step_per_tick", &self.step_per_tick)
            .field("attack_target", &self.attack_target)
            .field("active_target_block", &self.active_target_block)
            .field("jumping", &self.jumping)
            .field("attacks", &self.attacks)
            .field("move_calls", &self.move_calls)
            .field("path_searches", &self.path_searches)
            .field("love_ticks", &self.love_ticks)
            .field("age", &self.age)
            .finish_non_exhaustive()
    }
}

impl<'w> NavigatingMob<'w> {
    /// Creates a mob at `pos` with body `shape`, moving `step_per_tick` blocks
    /// per tick, pathfinding through `world`.
    ///
    /// `visited_budget` bounds the A\* open set (vanilla derives it as
    /// `floor(followRange * 16)`).
    #[must_use]
    pub fn new(
        world: &'w dyn PathWorld,
        shape: MobShape,
        pos: Vec3,
        step_per_tick: f64,
        visited_budget: i32,
    ) -> Self {
        let width = shape.width;
        Self {
            world,
            shape,
            finder: PathFinder::new(visited_budget),
            navigator: PathNavigator::new(width),
            pos,
            step_per_tick,
            rng: SplitMix64(0x1234_5678_9ABC_DEF0),
            attack_target: None,
            active_target_block: None,
            last_look: None,
            jumping: false,
            attacks: Vec::new(),
            launches: Vec::new(),
            move_calls: 0,
            path_searches: 0,
            tick_count: 0,
            last_search_tick: None,
            velocity: Vec3::new(0.0, 0.0, 0.0),
            body_yaw: 0.0,
            love_ticks: 0,
            partner_candidate: None,
            bred: false,
            age: 0,
            age_locked: false,
            parent_candidate: None,
            swell_dir: -1,
            swell: 0,
            ignited: false,
            detonated: false,
            nearest_player: None,
            temptation: None,
            avoid_threat: None,
            no_action_time: 0,
            last_hurt_by: None,
            hurt_by_ticks: 0,
            damage_ticks: 0,
        }
    }

    /// Overrides the RNG seed (affects stroll target selection only).
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = SplitMix64(seed);
        self
    }

    /// The mob's current position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.pos
    }

    /// The mob's collision body (width/height/step and traversal parameters).
    #[must_use]
    pub fn shape(&self) -> &MobShape {
        &self.shape
    }

    /// The targets the mob has struck, in order (for tests).
    #[must_use]
    pub fn attacks(&self) -> &[Vec3] {
        &self.attacks
    }

    /// Drains the attacks recorded since the last call, returning them in
    /// order. Unlike [`attacks`](Self::attacks) (an inspection peek used
    /// throughout this module's tests), this **consumes** them — the shape a
    /// real per-tick consumer needs so it resolves each strike exactly once
    /// instead of re-processing the whole history every tick.
    pub fn take_new_attacks(&mut self) -> Vec<Vec3> {
        std::mem::take(&mut self.attacks)
    }

    /// The projectile launches a ranged goal has asked for (for tests).
    #[must_use]
    pub fn launches(&self) -> &[ProjectileLaunch] {
        &self.launches
    }

    /// Drains the projectile launches recorded since the last call — the
    /// [`take_new_attacks`](Self::take_new_attacks) shape, for issue #227's
    /// ranged goals.
    ///
    /// **A host that never calls this turns every ranged goal into an island.**
    /// The goal runs, `can_use` is true, the launch lands in this `Vec`, and no
    /// projectile ever exists. `lodestone_server::mobs::MobSim::tick` is the one
    /// production caller; see [`ranged`](super::roster::ranged) for the wiring
    /// and what proves it.
    pub fn take_new_launches(&mut self) -> Vec<ProjectileLaunch> {
        std::mem::take(&mut self.launches)
    }

    /// How many times a goal asked this mob to move.
    #[must_use]
    pub fn move_calls(&self) -> u32 {
        self.move_calls
    }

    /// How many actual A\* searches ran — the count the seam's fakes can never
    /// produce, since their `move_to` never touches a pathfinder.
    #[must_use]
    pub fn path_searches(&self) -> u32 {
        self.path_searches
    }

    /// Whether the navigator gave up because the mob stopped progressing.
    #[must_use]
    pub fn is_stuck(&self) -> bool {
        self.navigator.is_stuck()
    }

    /// The last position a goal asked the mob to look at, if any.
    #[must_use]
    pub fn facing(&self) -> Option<Vec3> {
        self.last_look
    }

    /// The mob's velocity in **blocks per tick** — the position delta applied on
    /// the last [`advance`]. Zero when the mob did not move. This is the unit
    /// vanilla's wire packing assumes, so it can be encoded directly.
    #[must_use]
    pub fn velocity(&self) -> Vec3 {
        self.velocity
    }

    /// Applies an external velocity impulse — e.g. melee/explosion knockback
    /// (`lodestone_physics::knockback::knockback_impulse`) — as an immediate
    /// one-tick position displacement, and reports it as this tick's
    /// [`velocity`](Self::velocity) until the next [`advance`] overwrites it
    /// from path-following.
    ///
    /// This follower has no drag/persistent-velocity model to blend an
    /// impulse into: it is explicitly "kinematic... not the physics
    /// integrator" (see this struct's own doc comment) — every tick's motion
    /// comes from stepping toward the current waypoint, recomputed from
    /// scratch, with no notion of "current speed" surviving between ticks
    /// beyond what [`advance`] just derived. So rather than adding an ongoing
    /// impulse-decay state this composition was never built to carry, the
    /// impulse is applied once, directly to position — the same "one-shot
    /// simplification, disclosed rather than silent" trade `docs/combat.md`
    /// already makes for the crit-particle burst (one tick's worth instead of
    /// vanilla's three-tick `TrackingEmitter`). The next `advance()` call
    /// (path-following) is unaffected: it recomputes fresh from the
    /// post-impulse position, exactly as if the mob had walked there itself.
    pub fn apply_knockback(&mut self, impulse: Vec3) {
        self.pos.x += impulse.x;
        self.pos.y += impulse.y;
        self.pos.z += impulse.z;
        self.velocity = impulse;
    }

    /// The mob's body yaw in degrees (vanilla `yBodyRot`), derived from its
    /// movement direction and retained while idle.
    #[must_use]
    pub fn body_yaw(&self) -> f32 {
        self.body_yaw
    }

    /// The mob's head yaw in degrees: the direction toward its current look
    /// target if a goal set one, otherwise the body yaw.
    #[must_use]
    pub fn head_yaw(&self) -> f32 {
        if let Some(look) = self.last_look {
            let dx = look.x - self.pos.x;
            let dz = look.z - self.pos.z;
            if dx * dx + dz * dz > 1e-12 {
                return movement_yaw(dx, dz);
            }
        }
        self.body_yaw
    }

    /// Whether a goal has the mob holding jump this tick.
    #[must_use]
    pub fn is_jumping(&self) -> bool {
        self.jumping
    }

    /// Whether a path is currently being followed.
    #[must_use]
    pub fn has_path(&self) -> bool {
        !self.navigator.is_done()
    }

    /// Current age-timer value. See the `age` field's own doc comment:
    /// negative is a baby (ticking toward `0`), positive is a post-breeding
    /// parent cooldown (ticking toward `0`), `0` is a cooldown-free adult.
    #[must_use]
    pub fn age(&self) -> i32 {
        self.age
    }

    /// Sets the age timer directly — e.g. [`BABY_START_AGE`] to spawn this
    /// mob as a baby, or [`PARENT_AGE_AFTER_BREEDING`] to apply the
    /// post-breeding cooldown (vanilla `AgeableMob::setAge`).
    pub fn set_age(&mut self, age: i32) -> &mut Self {
        self.age = age;
        self
    }

    /// Whether ageing is currently frozen (vanilla `AgeableMob.isAgeLocked`).
    #[must_use]
    pub fn is_age_locked(&self) -> bool {
        self.age_locked
    }

    /// Freezes (`true`) or resumes (`false`) age advancement.
    pub fn set_age_locked(&mut self, locked: bool) -> &mut Self {
        self.age_locked = locked;
        self
    }

    /// Enters love mode for [`LOVE_TICKS`] (vanilla `Animal::setInLove`).
    pub fn set_in_love(&mut self) -> &mut Self {
        self.love_ticks = LOVE_TICKS;
        self
    }

    /// Remaining love-mode ticks (vanilla `Animal.getInLoveTime`).
    #[must_use]
    pub fn love_time(&self) -> i32 {
        self.love_ticks
    }

    /// Ends love mode immediately (vanilla `Animal::resetLove`).
    pub fn reset_love(&mut self) -> &mut Self {
        self.love_ticks = 0;
        self
    }

    /// Host injection point: refreshes the breeding-partner candidate this
    /// mob's [`BreedGoal`](super::goals::BreedGoal) should see this tick. See
    /// the `partner_candidate` field's own doc comment.
    pub fn set_love_partner_candidate(&mut self, partner: Option<Vec3>) -> &mut Self {
        self.partner_candidate = partner;
        self
    }

    /// Host injection point: refreshes the nearest-eligible-parent candidate
    /// this mob's [`FollowParentGoal`](super::goals::FollowParentGoal) should
    /// see this tick.
    pub fn set_parent_candidate(&mut self, parent: Option<Vec3>) -> &mut Self {
        self.parent_candidate = parent;
        self
    }

    /// Drains the "a goal called `breed()` this tick" flag — `true` at most
    /// once per tick. The host resolves it into an actual child spawn using
    /// this mob's and its partner's identity, which this seam does not
    /// carry.
    pub fn take_bred(&mut self) -> bool {
        std::mem::take(&mut self.bred)
    }

    /// Current fuse counter (vanilla `Creeper.swell`), `0..=`[`MAX_SWELL`].
    #[must_use]
    pub fn swell(&self) -> i32 {
        self.swell
    }

    /// Marks the mob ignited (vanilla `Creeper.ignite()`,
    /// `Creeper.java:264-266`), forcing its swell direction to climb every
    /// tick regardless of what [`SwellGoal`](super::goals::SwellGoal) would
    /// otherwise pick from proximity alone. See the `ignited` field's own doc
    /// comment for the interaction (flint-and-steel) that would call this in
    /// a full implementation.
    pub fn ignite(&mut self) -> &mut Self {
        self.ignited = true;
        self
    }

    /// Drains the "swell just reached [`MAX_SWELL`]" flag — `true` for
    /// exactly one call. See the `detonated` field's own doc comment.
    pub fn take_detonated(&mut self) -> bool {
        std::mem::take(&mut self.detonated)
    }

    /// Host injection point: refreshes the nearest-player position this mob's
    /// [`LookAtPlayerGoal`](super::goals::LookAtPlayerGoal) should see this
    /// tick. See the `nearest_player` field's own doc comment.
    pub fn set_nearest_player(&mut self, player: Option<Vec3>) -> &mut Self {
        self.nearest_player = player;
        self
    }

    /// Host injection point: refreshes the tempting-entity position this mob's
    /// [`TemptGoal`](super::goals::TemptGoal) should see this tick. See the
    /// `temptation` field's own doc comment for why the item predicate is the
    /// host's and not this crate's.
    pub fn set_temptation(&mut self, temptation: Option<Vec3>) -> &mut Self {
        self.temptation = temptation;
        self
    }

    /// Host injection point: refreshes the threat position this mob's
    /// [`AvoidEntityGoal`](super::goals::AvoidEntityGoal) should flee this
    /// tick. See the `avoid_threat` field's own doc comment.
    pub fn set_avoid_threat(&mut self, threat: Option<Vec3>) -> &mut Self {
        self.avoid_threat = threat;
        self
    }

    /// Host injection point: sets vanilla `Mob.noActionTime`, which
    /// [`RandomStrollGoal`](super::goals::RandomStrollGoal) uses to yield when
    /// idle-throttled. See the `no_action_time` field's own doc comment —
    /// leaving this at `0` is *not* a neutral default.
    pub fn set_no_action_time(&mut self, ticks: i32) -> &mut Self {
        self.no_action_time = ticks;
        self
    }

    /// Records that this mob just took damage, optionally from an attacker at
    /// `attacker`.
    ///
    /// This is one call for vanilla's two separate records, because one hit
    /// writes both: `LivingEntity.hurt` sets `lastDamageSource`
    /// (`LivingEntity.java:1268-1269`) — which is what
    /// [`PanicGoal`](super::goals::PanicGoal) reads — *and*, when the source
    /// has a living attacker, `setLastHurtByMob`
    /// (`LivingEntity.java:1358`), which is what
    /// [`HurtByTargetGoal`](super::goals::HurtByTargetGoal) reads. They then
    /// expire on **different** timers ([`PANIC_DAMAGE_TICKS`] vs
    /// [`LAST_HURT_BY_TICKS`]), so both are tracked separately here.
    ///
    /// Pass `None` for damage with no living attacker — fall damage, drowning,
    /// an explosion whose source this seam cannot name. Such a hit still
    /// panics the mob (vanilla's panic is source-driven, not attacker-driven)
    /// but gives it nothing to retaliate against, and deliberately **leaves any
    /// existing** [`last_hurt_by`](MobController::last_hurt_by) alone rather
    /// than clearing it: vanilla's two records are independent, so a mob shoved
    /// into a cactus mid-fight does not forget who it was fighting.
    pub fn note_hurt(&mut self, attacker: Option<Vec3>) -> &mut Self {
        self.damage_ticks = PANIC_DAMAGE_TICKS;
        if let Some(attacker) = attacker {
            self.last_hurt_by = Some(attacker);
            self.hurt_by_ticks = LAST_HURT_BY_TICKS;
        }
        self
    }

    /// The block cell the mob's feet occupy, for the fluid classification
    /// below. The follower snaps `pos.y` to the floor it stands on, so this is
    /// the cell *containing* the feet, not the floor beneath them.
    fn feet_block(&self) -> (i32, i32, i32) {
        (
            self.pos.x.floor() as i32,
            self.pos.y.floor() as i32,
            self.pos.z.floor() as i32,
        )
    }

    /// Runs one AI tick: the goal selector (whose goals call back through the
    /// [`MobController`] seam) followed by one kinematic follower step.
    pub fn tick(&mut self, ai: &mut GoalSelector) {
        ai.tick(self);
        self.advance();
    }

    /// Advances the follower one step toward the current waypoint. Public so a
    /// caller running its own goal loop can drive movement explicitly.
    pub fn advance(&mut self) {
        self.tick_count += 1;
        // Vanilla `Animal::aiStep`/`AgeableMob::aiStep`: love mode and the age
        // timer both age unconditionally every tick — not gated on whether any
        // goal ran this tick, and not reset by anything below.
        if self.love_ticks > 0 {
            self.love_ticks -= 1;
        }
        if !self.age_locked {
            if self.age < 0 {
                self.age += 1;
            } else if self.age > 0 {
                self.age -= 1;
            }
        }
        // Vanilla `LivingEntity.tick` ages both damage records every tick with
        // no goal involvement: `lastHurtByMob` is dropped past 100 ticks
        // (`LivingEntity.java:493`) and `getLastDamageSource` self-clears past
        // 40 (`LivingEntity.java:1420-1421`). Same "integrate unconditionally"
        // placement as the age/love/swell counters above, and for the same
        // reason — a goal that stops running must not freeze the timer that
        // ends it.
        if self.hurt_by_ticks > 0 {
            self.hurt_by_ticks -= 1;
            if self.hurt_by_ticks == 0 {
                self.last_hurt_by = None;
            }
        }
        if self.damage_ticks > 0 {
            self.damage_ticks -= 1;
        }
        // Vanilla `Creeper.tick()` (`Creeper.java:126-151`): runs every tick
        // regardless of whether `SwellGoal` (or anything else) is currently
        // running, exactly like the age/love integration above. `ignited`
        // overrides whatever direction the goal picked.
        if self.ignited {
            self.swell_dir = 1;
        }
        self.swell += self.swell_dir;
        if self.swell < 0 {
            self.swell = 0;
        }
        if self.swell >= MAX_SWELL {
            self.swell = MAX_SWELL;
            self.detonated = true;
        }
        let old = self.pos;
        let Some(waypoint) = self.navigator.tick(self.pos) else {
            self.velocity = Vec3::new(0.0, 0.0, 0.0);
            return;
        };
        let dx = waypoint.x - self.pos.x;
        let dz = waypoint.z - self.pos.z;
        let horizontal = (dx * dx + dz * dz).sqrt();
        if horizontal <= self.step_per_tick || horizontal == 0.0 {
            self.pos.x = waypoint.x;
            self.pos.z = waypoint.z;
        } else {
            let scale = self.step_per_tick / horizontal;
            self.pos.x += dx * scale;
            self.pos.z += dz * scale;
        }
        // Grounded follower: snap the vertical to the waypoint's floor.
        self.pos.y = waypoint.y;
        // Record the applied delta as blocks/tick velocity, and face the
        // horizontal motion (retaining the last body yaw while stationary).
        let moved_x = self.pos.x - old.x;
        let moved_z = self.pos.z - old.z;
        self.velocity = Vec3::new(moved_x, self.pos.y - old.y, moved_z);
        if moved_x * moved_x + moved_z * moved_z > 1e-12 {
            self.body_yaw = movement_yaw(moved_x, moved_z);
        }
    }
}

impl MobController for NavigatingMob<'_> {
    fn next_f32(&mut self) -> f32 {
        self.rng.next_unit() as f32
    }

    fn next_i32(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        (self.rng.next_u64() % bound as u64) as i32
    }

    fn next_f64(&mut self) -> f64 {
        self.rng.next_unit()
    }

    fn position(&self) -> Vec3 {
        self.pos
    }

    /// Whether the mob's feet cell holds water, read straight from the
    /// [`PathWorld`] this struct already borrows — no host injection, so
    /// [`FloatGoal`](super::goals::FloatGoal) works for any world that
    /// classifies its blocks, which every real one does.
    ///
    /// **Scope cut, disclosed:** vanilla is
    /// `isInWater() && getFluidHeight(WATER) > getFluidJumpThreshold()`
    /// (`ai/goal/FloatGoal.java:18`), where `isInWater` is a bounding-box
    /// sweep (`Entity.java:1605-1607`, `wasTouchingWater`) and the threshold is
    /// `getEyeHeight() < 0.4 ? 0.0 : 0.4` (`Entity.java:3692-3694`). This
    /// composition has no fluid-height model at all — `PathWorld` exposes
    /// per-cell classification and collision tops, not fluid levels — so the
    /// feet cell being water stands in for both halves. The practical
    /// difference is at the surface: vanilla stops floating once the mob's feet
    /// are in water shallower than 0.4, and this does not. Getting that right
    /// needs a fluid-level seam on `PathWorld`, which is a wider change than
    /// this perception fix.
    fn in_water(&self) -> bool {
        let (x, y, z) = self.feet_block();
        // `is_water` rather than matching `base_path_type` directly: the seam
        // gives hosts an override for exactly this question (`world.rs:146-150`)
        // and its own default is the `PathType::Water` match anyway, so going
        // through it honours a host that classifies waterlogged blocks.
        self.world.is_water(x, y, z)
    }

    /// Whether the mob's feet cell holds lava. Same world-derived,
    /// injection-free shape as [`in_water`](MobController::in_water); vanilla's
    /// `isInLava` (`Entity.java:1748-1750`) has no height threshold, so this
    /// side is faithful rather than cut.
    fn in_lava(&self) -> bool {
        let (x, y, z) = self.feet_block();
        matches!(self.world.base_path_type(x, y, z), PathType::Lava)
    }

    fn no_action_time(&self) -> i32 {
        self.no_action_time
    }

    fn nearest_player(&self) -> Option<Vec3> {
        self.nearest_player
    }

    fn last_hurt_by(&self) -> Option<Vec3> {
        self.last_hurt_by
    }

    fn temptation(&self) -> Option<Vec3> {
        self.temptation
    }

    fn avoid_threat(&self) -> Option<Vec3> {
        self.avoid_threat
    }

    fn is_panicking(&self) -> bool {
        self.damage_ticks > 0
    }

    fn move_to(&mut self, target: Vec3, speed: f64) -> bool {
        let block = BlockPos::new(
            target.x.floor() as i32,
            target.y.floor() as i32,
            target.z.floor() as i32,
        );
        // Reuse the active path unless it finished or the goal now wants a
        // different destination block (vanilla `PathNavigation.moveTo` reuse).
        let same_target = self.active_target_block == Some(block);
        let recompute = self.navigator.is_done() || !same_target;
        if !recompute {
            self.move_calls += 1;
            return true;
        }

        // Vanilla `recomputePath` refuses to re-search the *same* destination
        // within `MAX_TIME_RECOMPUTE` (20) ticks. Only a genuinely new target
        // block bypasses the throttle; a wedged mob whose path finished stands
        // still until the cooldown elapses instead of hammering A\* every tick.
        if same_target
            && self
                .last_search_tick
                .is_some_and(|last| self.tick_count.saturating_sub(last) < 20)
        {
            // Report whether we still hold a followable path.
            return !self.navigator.is_done();
        }

        self.path_searches += 1;
        self.last_search_tick = Some(self.tick_count);
        // Remember the block we searched toward *regardless of success*, so an
        // unreachable target throttles re-search the same as a reachable one
        // (otherwise a wedged mob resets `same_target` every tick and hammers A*).
        self.active_target_block = Some(block);
        let start = PathStart::grounded(self.pos.x, self.pos.y, self.pos.z);
        let params = PathParams {
            max_path_length: 200.0,
            reach_range: 1,
            visited_multiplier: 1.0,
        };
        match self
            .finder
            .find_path(self.world, &self.shape, start, &[block], params)
        {
            Some(path) => {
                self.navigator.start(path, speed as f32);
                self.move_calls += 1;
                true
            }
            None => false,
        }
    }

    fn navigation_done(&self) -> bool {
        self.navigator.is_done()
    }

    fn stop_navigation(&mut self) {
        self.navigator.stop();
        self.active_target_block = None;
    }

    fn set_jumping(&mut self, jumping: bool) {
        self.jumping = jumping;
    }

    fn look_at(&mut self, target: Vec3) {
        self.last_look = Some(target);
    }

    fn look_toward(&mut self, dx: f64, dz: f64) {
        self.last_look = Some(Vec3::new(self.pos.x + dx, self.pos.y, self.pos.z + dz));
    }

    fn attack_target(&self) -> Option<Vec3> {
        self.attack_target
    }

    fn set_attack_target(&mut self, target: Option<Vec3>) {
        self.attack_target = target;
    }

    fn find_nearest_target(&mut self) -> Option<Vec3> {
        self.attack_target
    }

    fn attack(&mut self, target: Vec3) {
        self.attacks.push(target);
    }

    fn launch_projectile(&mut self, launch: ProjectileLaunch) {
        self.launches.push(launch);
    }

    fn random_stroll_target(&mut self) -> Option<Vec3> {
        // A random destination in a 10-block box around the mob, matching
        // `RandomStroll`'s ±7 horizontal reach closely enough for the seam.
        let dx = (self.rng.next_unit() * 20.0 - 10.0).round();
        let dz = (self.rng.next_unit() * 20.0 - 10.0).round();
        Some(Vec3::new(self.pos.x + dx, self.pos.y, self.pos.z + dz))
    }

    fn is_baby(&self) -> bool {
        self.age < 0
    }

    fn parent_position(&self) -> Option<Vec3> {
        self.parent_candidate
    }

    fn is_in_love(&self) -> bool {
        self.love_ticks > 0
    }

    fn find_love_partner(&mut self) -> Option<Vec3> {
        self.partner_candidate
    }

    fn love_partner_position(&self) -> Option<Vec3> {
        self.partner_candidate
    }

    fn breed(&mut self) {
        // Vanilla `Animal::finalizeSpawnChildFromBreeding` calls
        // `resetLove()` on both parents immediately
        // (`.cache/mc/26.2/src/net/minecraft/world/entity/animal/Animal.java:227-228`).
        // The age cooldown (`setAge(PARENT_AGE_AFTER_BREEDING)`, same file
        // line 225-226) and the child itself are the host's job — this seam
        // has no notion of the partner's identity or of creating an entity —
        // so the host applies `set_age(PARENT_AGE_AFTER_BREEDING)` to both
        // parents itself after observing `take_bred()`.
        self.bred = true;
        self.love_ticks = 0;
    }

    fn clear_love_partner(&mut self) {
        self.partner_candidate = None;
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::ai::goal::Goal;
    use crate::ai::goals::{
        AvoidEntityGoal, FloatGoal, HurtByTargetGoal, LookAtPlayerGoal, MeleeAttackGoal, PanicGoal,
        RandomStrollGoal, SwellGoal, TemptGoal,
    };
    use crate::pathfinding::{Aabb, PathType};

    /// Flat ground one block below `y=0`, plus a set of fence cells with a 1.5
    /// collision top (unjumpable). Mirrors the live-navigation arena so the
    /// composition is exercised against the same block classification a live
    /// zombie was measured on.
    struct Arena {
        walls: HashSet<(i32, i32, i32)>,
    }

    impl Arena {
        fn is_ground(y: i32) -> bool {
            y <= -1
        }
        fn is_wall(&self, x: i32, y: i32, z: i32) -> bool {
            self.walls.contains(&(x, y, z))
        }
        fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
            self.is_wall(x, y, z) || Self::is_ground(y)
        }
    }

    impl PathWorld for Arena {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
            if self.is_solid(x, y, z) {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
            if self.is_wall(x, y, z) {
                1.5
            } else if Self::is_ground(y) {
                1.0
            } else {
                0.0
            }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            let x0 = aabb.min_x.floor() as i32;
            let x1 = (aabb.max_x - 1e-7).floor() as i32;
            let y0 = aabb.min_y.floor() as i32;
            let y1 = (aabb.max_y - 1e-7).floor() as i32;
            let z0 = aabb.min_z.floor() as i32;
            let z1 = (aabb.max_z - 1e-7).floor() as i32;
            for x in x0..=x1 {
                for y in y0..=y1 {
                    for z in z0..=z1 {
                        if self.is_solid(x, y, z) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
            false
        }
    }

    fn fence_wall() -> Arena {
        let mut walls = HashSet::new();
        for z in -3..=3 {
            walls.insert((5, -1, z));
            // Fence occupies the standing layer too (its collision is 1.5 tall).
            walls.insert((5, 0, z));
        }
        Arena { walls }
    }

    fn run_to_target(world: &dyn PathWorld, target: Vec3) -> (bool, f64, Vec<Vec3>) {
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 8000);
        mob.set_attack_target(Some(target));

        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        let mut route = vec![mob.position()];
        let mut reached = false;
        for _ in 0..2000 {
            mob.tick(&mut ai);
            let p = mob.position();
            route.push(p);
            let dx = target.x - p.x;
            let dz = target.z - p.z;
            if (dx * dx + dz * dz).sqrt() < 1.5 {
                reached = true;
                break;
            }
            if mob.is_stuck() {
                break;
            }
        }
        let max_abs_z = route.iter().map(|p| p.z.abs()).fold(0.0f64, f64::max);
        (reached, max_abs_z, route)
    }

    #[test]
    fn goal_drives_pathfinder_straight_line_with_no_obstacle() {
        // Control: no wall. A melee goal must reach the target on a near-straight
        // line — max|z| stays small because nothing forces a detour. This is the
        // anti-vacuity partner of the fence test: if the mob detoured here, the
        // pathfinder (not the goal wiring) would be the thing under test.
        let world = Arena {
            walls: HashSet::new(),
        };
        let (reached, max_abs_z, _route) = run_to_target(&world, Vec3::new(10.5, 0.0, 0.5));
        assert!(reached, "mob reached the open-ground target");
        assert!(
            max_abs_z < 2.0,
            "with no obstacle the goal-driven path stays near z=0, got max|z|={max_abs_z:.2}"
        );
    }

    #[test]
    fn goal_drives_pathfinder_to_detour_an_unjumpable_fence() {
        // The load-bearing test: a `MeleeAttackGoal` — through the real
        // `MobController` seam — must invoke A\*, and the path must go *around*
        // the fence (|z| beyond ±3), not through it. A fake `move_to` (the only
        // other implementor of this seam) could never exercise any of this.
        let world = fence_wall();
        let (reached, max_abs_z, _route) = run_to_target(&world, Vec3::new(10.5, 0.0, 0.5));
        assert!(
            reached,
            "goal-driven mob reached the target past the fence (max|z|={max_abs_z:.2})"
        );
        assert!(
            max_abs_z >= 4.0,
            "goal-driven mob must detour the fence end (|z|>=4), got max|z|={max_abs_z:.2}"
        );
    }

    #[test]
    fn goal_actually_invokes_astar_and_strikes_in_reach() {
        // Proves the seam is wired end to end: real searches ran (not a counter
        // bump), and the mob struck the target once within melee reach.
        let world = fence_wall();
        let shape = MobShape::land(0.6, 1.95);
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 8000);
        mob.set_attack_target(Some(target));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        for _ in 0..2000 {
            mob.tick(&mut ai);
            if !mob.attacks().is_empty() {
                break;
            }
            if mob.is_stuck() {
                break;
            }
        }
        assert!(
            mob.path_searches() >= 1,
            "a real A* search must have run; got {}",
            mob.path_searches()
        );
        assert!(
            !mob.attacks().is_empty(),
            "mob never reached melee reach to strike (searches={}, pos={:?})",
            mob.path_searches(),
            mob.position()
        );
        let hit = mob.attacks()[0];
        assert!((hit.x - target.x).abs() < 0.01 && (hit.z - target.z).abs() < 0.01);
    }

    #[test]
    fn goal_driven_mob_approaches_but_cannot_strike_a_sealed_target() {
        // A target enclosed by a solid wall two cells thick: vanilla's pathfinder
        // returns a *best-effort partial* path (not `None`), so the mob genuinely
        // walks up to the wall — but the nearest reachable cell is >2 blocks from
        // the sealed target, so a `MeleeAttackGoal` can never strike. This asserts
        // two things a fake `move_to` (which teleports/strikes unconditionally)
        // could never satisfy: the mob *does* make forward progress (it followed a
        // real partial path), yet *never* reaches melee reach of the sealed cell.
        let mut walls = HashSet::new();
        for z in -2..=2 {
            for x in 8..=12 {
                for y in -1..=1 {
                    walls.insert((x, y, z));
                }
            }
        }
        // Carve out the target pocket: a walkable floor at (10,-1,0) with open
        // standing space at (10,0,0), fully surrounded by the solid shell.
        walls.remove(&(10, -1, 0));
        walls.remove(&(10, 0, 0));
        walls.remove(&(10, 1, 0));
        let world = Arena { walls };
        let shape = MobShape::land(0.6, 1.95);
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 3000);
        mob.set_attack_target(Some(target));

        // move_to yields a (partial) path, matching vanilla best-effort behaviour.
        let found = mob.move_to(target, 1.0);
        assert!(
            found,
            "vanilla returns a partial path toward an unreachable target"
        );

        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        let mut closest = f64::INFINITY;
        let mut last_x = mob.position().x;
        let mut stalled = 0u32;
        for _ in 0..800 {
            mob.tick(&mut ai);
            let p = mob.position();
            let dx = target.x - p.x;
            let dz = target.z - p.z;
            closest = closest.min((dx * dx + dz * dz).sqrt());
            // Stop once the mob has clearly stalled against the wall: it cannot
            // make progress, so further ticks only re-run A* fruitlessly.
            if (p.x - last_x).abs() < 1e-4 {
                stalled += 1;
                if stalled > 40 {
                    break;
                }
            } else {
                stalled = 0;
            }
            last_x = p.x;
            if mob.is_stuck() {
                break;
            }
        }
        // It walked toward the target (real path following, not a no-op)...
        assert!(
            mob.position().x > 3.0,
            "mob should have advanced along the partial path, stuck at x={:.2}",
            mob.position().x
        );
        // ...but the sealed shell keeps it >2 blocks out, so it never strikes.
        assert!(
            mob.attacks().is_empty(),
            "a sealed target is unreachable and must never be struck (closest={closest:.2})"
        );
        assert!(
            closest > 2.0,
            "the solid shell must keep the mob out of melee reach, got closest={closest:.2}"
        );
    }

    /// Builds the two-thick sealed shell around the target pocket at (10,0,0).
    fn sealed_shell() -> Arena {
        let mut walls = HashSet::new();
        for z in -2..=2 {
            for x in 8..=12 {
                for y in -1..=1 {
                    walls.insert((x, y, z));
                }
            }
        }
        walls.remove(&(10, -1, 0));
        walls.remove(&(10, 0, 0));
        walls.remove(&(10, 1, 0));
        Arena { walls }
    }

    #[test]
    fn endurance_wedged_mob_neither_hammers_astar_nor_oscillates() {
        // Duration test (the class a 200-tick gate cannot see): a mob chasing an
        // *unreachable* target for 4000 ticks. Two end-state invariants:
        //   1. The 20-tick recompute throttle holds for the whole run — a
        //      regression to per-tick searching would make `path_searches` ~4000;
        //      the throttle caps it near ticks/20. This is the "navigator that
        //      leaks / hammers over time" detector.
        //   2. The mob *settles* against the wall rather than pacing forever — its
        //      position over the final 500 ticks stays inside a <1-block box.
        let world = sealed_shell();
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, 0.0, 0.5),
            0.25,
            600,
        );
        mob.set_attack_target(Some(target));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        const TICKS: usize = 2000;
        let mut tail: Vec<Vec3> = Vec::new();
        for t in 0..TICKS {
            mob.tick(&mut ai);
            if t >= TICKS - 500 {
                tail.push(mob.position());
            }
        }

        // (1) Throttle held all run: far below one search per tick.
        assert!(
            mob.path_searches() < (TICKS as u32) / 15,
            "wedged mob hammered A* — {} searches over {TICKS} ticks (throttle regressed?)",
            mob.path_searches()
        );
        // (2) Settled, not oscillating: bounded box over the final 500 ticks.
        let min_x = tail.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let max_x = tail.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        let min_z = tail.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        let max_z = tail.iter().map(|p| p.z).fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (max_x - min_x) < 1.0 && (max_z - min_z) < 1.0,
            "mob never settled: final-500 span x={:.2} z={:.2}",
            max_x - min_x,
            max_z - min_z
        );
        // Never phased through the shell.
        assert!(
            mob.attacks().is_empty(),
            "unreachable target must never be struck"
        );
    }

    #[test]
    fn endurance_reached_mob_settles_at_target_and_does_not_wander_off() {
        // The mirror invariant: a mob that *reaches* a reachable target and then
        // keeps ticking for thousands more ticks must stay *at* the target, not
        // drift away or orbit it. Asserts the end state after long idling — the
        // "works then wanders" bug a short test that breaks-on-reach cannot see.
        let world = Arena {
            walls: HashSet::new(),
        };
        let target = Vec3::new(10.5, 0.0, 0.5);
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, 0.0, 0.5),
            0.25,
            800,
        );
        mob.set_attack_target(Some(target));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));

        const TICKS: usize = 2000;
        let mut ever_reached = false;
        let mut tail: Vec<Vec3> = Vec::new();
        for t in 0..TICKS {
            mob.tick(&mut ai);
            let p = mob.position();
            let d = ((target.x - p.x).powi(2) + (target.z - p.z).powi(2)).sqrt();
            if d < 1.5 {
                ever_reached = true;
            }
            if t >= TICKS - 500 {
                tail.push(p);
            }
        }
        assert!(ever_reached, "mob never reached the reachable target");
        // End state after 3500+ ticks of idling at the target: still there.
        let final_pos = *tail.last().unwrap();
        let final_dist =
            ((target.x - final_pos.x).powi(2) + (target.z - final_pos.z).powi(2)).sqrt();
        assert!(
            final_dist < 2.0,
            "mob wandered away from the target it reached (final dist={final_dist:.2})"
        );
        // And it struck it (melee goal actually engaged), repeatedly over the run.
        assert!(
            !mob.attacks().is_empty(),
            "a reached mob should have struck the target at least once"
        );
    }

    // ---- Breeding / aging (issues #234, #237) -----------------------------
    //
    // These are driver-level: a real `GoalSelector` runs a real `BreedGoal`
    // against two real `NavigatingMob`s. The only "host" logic here is the
    // per-tick candidate refresh `MobController::find_love_partner`'s own doc
    // comment calls for (a population-wide `canMate` search this crate has no
    // way to do itself) — everything downstream of that one input is the
    // production seam, and `breed()` is never called directly.

    use crate::ai::goals::{BreedGoal, FollowParentGoal};

    /// Refreshes each mob's love-partner candidate from the other, mirroring
    /// what `MobSim::tick` will do every tick in production: a population
    /// scan for the nearest still-in-love, not-already-bred sibling. Kept
    /// deliberately trivial (exactly two mobs, no eligibility beyond
    /// `is_in_love`) because this test's subject is the goal→seam wiring, not
    /// the partner-selection policy — that lives in the server-side patch.
    fn refresh_partner_candidates(a: &mut NavigatingMob<'_>, b: &mut NavigatingMob<'_>) {
        let (pos_a, pos_b) = (a.position(), b.position());
        a.set_love_partner_candidate(if b.is_in_love() { Some(pos_b) } else { None });
        b.set_love_partner_candidate(if a.is_in_love() { Some(pos_a) } else { None });
    }

    #[test]
    fn breed_goal_drives_two_navigating_mobs_to_a_predicted_tick() {
        // Two in-love animals, 2 blocks apart (distSqr=4 < BreedGoal's 9.0
        // range) on open ground, each running the production `BreedGoal`.
        // Vanilla's own timer (`BreedGoal.java:57`, `loveTime >=
        // adjustedTickDelay(60)`) is exactly `BreedGoal::BREED_TIME` (60) in
        // `goals.rs` — so this predicts the *tick*, not just "eventually":
        // both `can_use` on tick 1 (already in range, no travel needed), so
        // `GoalSelector::tick`'s own start-then-tick-same-call semantics
        // (`goal.rs`'s `update`/`tick_running`) put `love_time` at exactly
        // `N` after the Nth call — bred must be false through tick 59 and
        // true from tick 60, on both mobs simultaneously.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut a = NavigatingMob::new(&world, shape.clone(), Vec3::new(0.0, 0.0, 0.0), 1.0, 400);
        let mut b = NavigatingMob::new(&world, shape, Vec3::new(2.0, 0.0, 0.0), 1.0, 400);
        a.set_in_love();
        b.set_in_love();

        let mut ai_a = GoalSelector::new();
        ai_a.add(0, Box::new(BreedGoal::new(1.0)));
        let mut ai_b = GoalSelector::new();
        ai_b.add(0, Box::new(BreedGoal::new(1.0)));

        for tick in 1..=60 {
            refresh_partner_candidates(&mut a, &mut b);
            a.tick(&mut ai_a);
            b.tick(&mut ai_b);
            if tick < 60 {
                assert!(
                    !a.take_bred() && !b.take_bred(),
                    "bred before the predicted tick 60 (at tick {tick})"
                );
            } else {
                assert!(
                    a.take_bred(),
                    "mob a must breed on the predicted tick (60)"
                );
                assert!(
                    b.take_bred(),
                    "mob b must breed on the predicted tick (60)"
                );
            }
        }
        // Vanilla resets love on both parents immediately
        // (`Animal.java:227-228`) — proven through the seam, not asserted by
        // calling `breed()` again.
        assert!(!a.is_in_love(), "breeding must end this mob's love mode");
        assert!(!b.is_in_love(), "breeding must end this mob's love mode");
    }

    #[test]
    fn breed_goal_never_fires_without_a_partner_candidate() {
        // Negative control for the test above: the same setup, minus ever
        // refreshing the partner candidate, must never breed even though
        // both mobs are in love the whole time and start in range. This is
        // the control CLAUDE.md's evidence standards ask for — it proves the
        // 60-tick assertion above is actually detecting the candidate wiring
        // and not some other coincidence (e.g. a goal that ignores its
        // `can_use` gate).
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut a = NavigatingMob::new(&world, shape.clone(), Vec3::new(0.0, 0.0, 0.0), 1.0, 400);
        let mut b = NavigatingMob::new(&world, shape, Vec3::new(2.0, 0.0, 0.0), 1.0, 400);
        a.set_in_love();
        b.set_in_love();
        let mut ai_a = GoalSelector::new();
        ai_a.add(0, Box::new(BreedGoal::new(1.0)));
        let mut ai_b = GoalSelector::new();
        ai_b.add(0, Box::new(BreedGoal::new(1.0)));

        for _ in 1..=200 {
            // No `refresh_partner_candidates` call: `find_love_partner`
            // always answers `None`, exactly like a lone in-love animal with
            // nothing nearby to mate with.
            a.tick(&mut ai_a);
            b.tick(&mut ai_b);
            assert!(!a.take_bred() && !b.take_bred());
        }
    }

    #[test]
    fn love_ticks_and_age_decay_unconditionally_each_advance() {
        // Vanilla ages both timers every entity tick regardless of what goals
        // ran (`Animal::aiStep`/`AgeableMob::aiStep`) — exercised here with no
        // goals attached at all, just repeated `advance()` calls.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 1.0, 400);
        mob.set_in_love();
        assert_eq!(mob.love_time(), LOVE_TICKS);
        for _ in 0..LOVE_TICKS {
            mob.advance();
        }
        assert_eq!(mob.love_time(), 0, "love mode must expire after exactly LOVE_TICKS");
        assert!(!mob.is_in_love());

        // A baby's age counts up from BABY_START_AGE to 0 at one tick per
        // tick (`AgeableMob.java:207-212`), so growing up takes exactly
        // `-BABY_START_AGE` advances — predicted, not just "eventually 0".
        mob.set_age(-10);
        assert!(mob.is_baby());
        for i in 1..=10 {
            mob.advance();
            if i < 10 {
                assert!(mob.is_baby(), "still a baby at age {}", mob.age());
            }
        }
        assert_eq!(mob.age(), 0);
        assert!(!mob.is_baby(), "must be an adult once age reaches 0");
    }

    #[test]
    fn age_locked_freezes_growth_and_control_proves_it_would_otherwise_grow() {
        // Control-then-subject, in that order, so the assertion of "locked
        // means frozen" is backed by a run that shows the same starting state
        // *would* have grown had it not been locked.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);

        // Control: unlocked, same starting age, ages normally over 50 ticks.
        let mut control = NavigatingMob::new(&world, shape.clone(), Vec3::new(0.0, 0.0, 0.0), 1.0, 400);
        control.set_age(-10);
        for _ in 0..50 {
            control.advance();
        }
        assert_eq!(
            control.age(),
            0,
            "control must reach adulthood (proves the detector can see growth at all)"
        );

        // Subject: locked, identical starting age, must not move at all.
        let mut locked = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 1.0, 400);
        locked.set_age(-10);
        locked.set_age_locked(true);
        for _ in 0..50 {
            locked.advance();
        }
        assert_eq!(locked.age(), -10, "age-locked mob must not age at all");
        assert!(locked.is_baby());

        // Unlocking resumes growth from exactly where it was frozen.
        locked.set_age_locked(false);
        for _ in 0..10 {
            locked.advance();
        }
        assert_eq!(locked.age(), 0);
    }

    #[test]
    fn follow_parent_goal_drives_a_baby_navigating_mob_toward_its_parent() {
        // The second goal this seam unblocks: a baby's `is_baby`/
        // `parent_position` are now real (host-injected) instead of the
        // `MobController` trait defaults (`false`/`None`), so
        // `FollowParentGoal` — already fully implemented in `goals.rs` — is
        // reachable through the concrete production type. Mirrors the
        // existing melee/pathfinder composition tests above: real A*, not a
        // fake `move_to`.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let baby_start = Vec3::new(0.0, 0.0, 0.0);
        let parent_pos = Vec3::new(10.0, 0.0, 0.0);
        let mut baby = NavigatingMob::new(&world, shape, baby_start, 0.25, 8000);
        baby.set_age(-10); // is_baby() == true, far from BABY_START_AGE so it
        // does not grow up mid-test.
        baby.set_parent_candidate(Some(parent_pos));

        let mut ai = GoalSelector::new();
        ai.add(0, Box::new(FollowParentGoal::new(1.0)));

        let mut reached = false;
        for _ in 0..500 {
            baby.set_parent_candidate(Some(parent_pos));
            baby.tick(&mut ai);
            let d = (parent_pos - baby.position()).length();
            if d < 4.0 {
                reached = true;
                break;
            }
        }
        assert!(
            reached,
            "a baby with a real parent candidate must actually path toward it, ended at {:?}",
            baby.position()
        );
        assert!(
            baby.path_searches() >= 1,
            "FollowParentGoal must have driven a real A* search"
        );
    }

    // ---- Knockback (issue #12) --------------------------------------------

    #[test]
    fn apply_knockback_displaces_position_and_reports_the_impulse_as_velocity() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let start = Vec3::new(5.0, 0.0, 5.0);
        let mut mob = NavigatingMob::new(&world, shape, start, 0.25, 400);

        let impulse = Vec3::new(-0.6, 0.4, 0.2);
        mob.apply_knockback(impulse);

        assert_eq!(
            mob.position(),
            Vec3::new(start.x + impulse.x, start.y + impulse.y, start.z + impulse.z),
            "knockback must displace position by exactly the impulse"
        );
        assert_eq!(
            mob.velocity(),
            impulse,
            "velocity() must report the impulse itself until the next advance()"
        );
    }

    #[test]
    fn advance_after_knockback_recomputes_fresh_from_the_post_impulse_position() {
        // Control: a subsequent `advance()` with no goal/path set must not
        // "remember" the impulse — velocity resets to zero, matching the
        // struct's own "no drag/persistent-velocity model" contract. This is
        // the control that proves the impulse is a one-shot displacement, not
        // a leaked ongoing velocity nothing ever decays.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400);
        mob.apply_knockback(Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(mob.velocity(), Vec3::new(1.0, 0.0, 0.0));

        mob.advance();
        assert_eq!(
            mob.velocity(),
            Vec3::new(0.0, 0.0, 0.0),
            "with no path, advance() must not perpetuate the knockback velocity"
        );
    }

    // ---- Creeper fuse (issue: creepers never prime or detonate) -----------

    #[test]
    fn ignited_mob_climbs_by_exactly_one_per_tick_then_detonates_at_max_swell() {
        // Predicts the exact tick-29 value, not merely "increased" — see
        // CLAUDE.md's *magnitude* vacuous-test species.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400);
        mob.ignite();

        for expected in 1..MAX_SWELL {
            mob.advance();
            assert_eq!(mob.swell(), expected, "swell must climb by exactly 1/tick while ignited");
            assert!(
                !mob.take_detonated(),
                "must not detonate before reaching MAX_SWELL (tick {expected})"
            );
        }
        assert_eq!(mob.swell(), MAX_SWELL - 1, "predicted tick-29 value");

        mob.advance(); // the 30th tick
        assert_eq!(mob.swell(), MAX_SWELL);
        assert!(
            mob.take_detonated(),
            "swell reaching MAX_SWELL must fire the detonation flag exactly once"
        );
        assert!(
            !mob.take_detonated(),
            "the flag must be drained (take), not re-armed, on the next read"
        );
    }

    #[test]
    fn un_ignited_mob_with_no_target_never_swells_or_detonates() {
        // Negative control: with nothing ever calling `ignite()` or
        // `set_swell_dir`, `swell_dir()` must stay at its default `-1`
        // indefinitely, so `swell` clamps at 0 and detonation never fires —
        // proving the fuse does not run unconditionally.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400);

        for _ in 0..500 {
            mob.advance();
        }
        assert_eq!(mob.swell(), 0);
        assert_eq!(mob.swell_dir(), -1);
        assert!(!mob.take_detonated());
    }

    #[test]
    fn swell_goal_drives_a_proximate_stationary_target_to_detonation_in_exactly_max_swell_ticks() {
        // End-to-end: `SwellGoal` (proximity only, no ignition) through the
        // real `GoalSelector` + `advance()` composition, exactly the path a
        // production `MobSim::tick` drives.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400);
        mob.set_attack_target(Some(Vec3::new(1.0, 0.0, 0.0))); // distSqr 1 < 9

        let mut ai = GoalSelector::new();
        ai.add(0, Box::new(SwellGoal::new()));

        let mut detonated_at: Option<i32> = None;
        for t in 1..=MAX_SWELL {
            mob.tick(&mut ai);
            if mob.take_detonated() {
                detonated_at = Some(t);
                break;
            }
        }
        assert_eq!(
            detonated_at,
            Some(MAX_SWELL),
            "a stationary target within 3 blocks must detonate in exactly MAX_SWELL ticks"
        );
    }

    #[test]
    fn swell_goal_does_not_fire_for_a_distant_target() {
        // Negative control for the goal itself, through the real scheduler:
        // a target well beyond the 3-block start gate, with no prior swell,
        // must never move the fuse off zero.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400);
        mob.set_attack_target(Some(Vec3::new(20.0, 0.0, 0.0))); // distSqr 400

        let mut ai = GoalSelector::new();
        ai.add(0, Box::new(SwellGoal::new()));

        for _ in 0..100 {
            mob.tick(&mut ai);
        }
        assert_eq!(mob.swell(), 0);
        assert!(!mob.take_detonated());
    }

    // ---------------------------------------------------------------------
    // Perception seam (issue #441, plan unit A1).
    //
    // Every goal below had a **constant-false `can_use` in production** before
    // this seam existed, because `impl MobController for NavigatingMob` left
    // the eight perception methods at their trait defaults (`false`/`None`/`0`).
    // Each also had a green unit test, because those tests drive `ScriptMob`
    // (`goals.rs`), a fake that overrides all eight — CLAUDE.md's *world*
    // species of vacuous test, where the flaw is in which controller the test
    // was pointed at and reading the test source cannot reveal it.
    //
    // So the load-bearing property of every test in this section is the
    // **type**: `NavigatingMob`, the one production implementor. A rewrite of
    // these against `ScriptMob` would pass identically and prove nothing.
    // ---------------------------------------------------------------------

    /// Flat ground with a per-cell fluid map, so [`MobController::in_water`] /
    /// [`MobController::in_lava`] are exercised against a real [`PathWorld`]
    /// classification rather than a setter.
    ///
    /// A separate fixture from [`Arena`] rather than a new field on it: the
    /// water/lava distinction is the *only* thing these tests need from a
    /// world, and widening `Arena` would touch every existing construction of
    /// it for no benefit.
    struct FluidArena {
        fluids: std::collections::HashMap<(i32, i32, i32), PathType>,
    }

    impl FluidArena {
        fn dry() -> Self {
            Self {
                fluids: std::collections::HashMap::new(),
            }
        }

        fn with(cell: (i32, i32, i32), fluid: PathType) -> Self {
            let mut fluids = std::collections::HashMap::new();
            fluids.insert(cell, fluid);
            Self { fluids }
        }
    }

    impl PathWorld for FluidArena {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
            if let Some(fluid) = self.fluids.get(&(x, y, z)) {
                return *fluid;
            }
            if y <= -1 {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
            if y <= -1 { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            (aabb.min_y.floor() as i32) <= -1
        }
        // Deliberately *not* overridden: the seam's own default is the
        // `PathType::Water` match (`pathfinding/world.rs:146-150`), which is
        // what `in_water` calls through, so leaving it default keeps this
        // fixture from being able to fake the answer.
    }

    fn perception_mob<'w>(world: &'w dyn PathWorld, at: Vec3) -> NavigatingMob<'w> {
        NavigatingMob::new(world, MobShape::land(0.6, 1.95), at, 0.25, 400)
    }

    /// Whether each of the six previously-dead goals reports `can_use` for the
    /// mob as currently configured, in a fixed order:
    /// `[float, look_at_player, hurt_by_target, tempt, avoid_entity, panic]`.
    ///
    /// `LookAtPlayerGoal` is built with `probability == 1.0` so its
    /// `next_f32() >= probability` pre-roll (`goals.rs:130`, vanilla's `0.02F`
    /// default at `ai/goal/LookAtPlayerGoal.java:26`) cannot make this test
    /// flaky in either direction — a probability roll is not what is under
    /// test here, the perception read behind it is.
    fn six_verdicts(mob: &mut NavigatingMob<'_>) -> [bool; 6] {
        // Distances are all inside each goal's own range gate for a mob at the
        // origin, so a `false` can only come from the perception method
        // returning the trait default.
        [
            FloatGoal.can_use(mob),
            LookAtPlayerGoal::new(8.0, 1.0).can_use(mob),
            HurtByTargetGoal::new().can_use(mob),
            TemptGoal::new(1.25).can_use(mob),
            AvoidEntityGoal::new(6.0, 1.0).can_use(mob),
            PanicGoal::new(1.25).can_use(mob),
        ]
    }

    #[test]
    fn all_six_perception_starved_goals_fire_on_a_real_navigating_mob() {
        // Water at the mob's feet cell drives `FloatGoal` with no injection at
        // all — vanilla `ai/goal/FloatGoal.java:18`.
        let world = FluidArena::with((0, 0, 0), PathType::Water);
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));

        // Each of the remaining five, at a distance vanilla would accept:
        //  * player 3 blocks away, inside `LookAtPlayerGoal`'s 8.0
        //    (`monster/Creeper.java:70`, `LookAtPlayerGoal(Player, 8.0F)`);
        //  * attacker 2 blocks away — `HurtByTargetGoal` has no range gate of
        //    its own (`ai/goal/target/HurtByTargetGoal.java:33-40` tests only
        //    the timestamp and non-null attacker);
        //  * tempter 4 blocks away, inside `Attributes.TEMPT_RANGE`'s default
        //    `10.0` (`ai/attributes/Attributes.java:107`);
        //  * threat 3 blocks away, inside the `6.0F` every vanilla
        //    `AvoidEntityGoal` registration uses (`monster/Creeper.java:67-68`,
        //    `monster/skeleton/AbstractSkeleton.java:79`,
        //    `monster/spider/Spider.java:59`).
        mob.set_nearest_player(Some(Vec3::new(3.5, 0.0, 0.5)))
            .set_temptation(Some(Vec3::new(4.5, 0.0, 0.5)))
            .set_avoid_threat(Some(Vec3::new(-2.5, 0.0, 0.5)))
            // One hit records both the retaliation target and the panic
            // window, exactly as vanilla's single `hurt` call writes both
            // records (`LivingEntity.java:1268-1269` and `:1358`).
            .note_hurt(Some(Vec3::new(2.5, 0.0, 0.5)));

        let got = six_verdicts(&mut mob);
        assert_eq!(
            got,
            [true; 6],
            "a fed NavigatingMob must satisfy all six goals; \
             order is [float, look_at_player, hurt_by_target, tempt, avoid_entity, panic], got {got:?}"
        );
    }

    #[test]
    fn the_same_six_goals_all_refuse_an_unfed_navigating_mob() {
        // The negative control the plan requires: identical construction,
        // identical goals, identical distances — only the perception inputs
        // withheld, and the world dry. Every verdict must invert.
        //
        // For the four injected methods this proves the value came from the
        // setter; for `in_water`/`in_lava` it proves it came from the *world*,
        // since there is no setter to withhold.
        let world = FluidArena::dry();
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));

        let got = six_verdicts(&mut mob);
        assert_eq!(
            got,
            [false; 6],
            "an unfed NavigatingMob must satisfy none of the six; got {got:?}"
        );
    }

    #[test]
    fn lava_alone_floats_a_mob_and_water_alone_does_too() {
        // `FloatGoal`'s condition is a disjunction (`FloatGoal.java:18`), so a
        // test that only ever sets water cannot tell `in_water() || in_lava()`
        // from `in_water()` — one arm could be dead. Drive each arm alone.
        let lava = FluidArena::with((0, 0, 0), PathType::Lava);
        let mut in_lava = perception_mob(&lava, Vec3::new(0.5, 0.0, 0.5));
        assert!(in_lava.in_lava(), "lava cell must classify as in_lava");
        assert!(
            !in_lava.in_water(),
            "a lava cell must not also read as water — that would make the \
             two methods indistinguishable and the disjunction untestable"
        );
        assert!(FloatGoal.can_use(&mut in_lava), "lava alone must float");

        let water = FluidArena::with((0, 0, 0), PathType::Water);
        let mut in_water = perception_mob(&water, Vec3::new(0.5, 0.0, 0.5));
        assert!(in_water.in_water());
        assert!(!in_water.in_lava());
        assert!(FloatGoal.can_use(&mut in_water), "water alone must float");
    }

    #[test]
    fn a_navigating_mob_in_water_actually_jumps_through_the_real_scheduler() {
        // Behavioural, not `can_use`: run `FloatGoal` through the same
        // `GoalSelector`/`NavigatingMob::tick` path production uses and assert
        // the mob ends up *jumping*. `can_use` returning true is the wiring;
        // the jump is the observable effect a player would see as floating.
        let world = FluidArena::with((0, 0, 0), PathType::Water);
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        let mut ai = GoalSelector::new();
        // Vanilla registers `FloatGoal` at priority 1 on a creeper
        // (`monster/Creeper.java:66`) and 9 on a bee (`animal/bee/Bee.java:191`);
        // the absolute number is private to one mob's set, so 0 is fine here.
        ai.add(0, Box::new(FloatGoal));

        // `tick` is 0.8-probability per tick (`FloatGoal.java:28`), so a
        // handful of ticks makes a miss vanishingly unlikely; 20 is generous.
        let mut jumped = false;
        for _ in 0..20 {
            mob.tick(&mut ai);
            if mob.is_jumping() {
                jumped = true;
                break;
            }
        }
        assert!(jumped, "a mob standing in water must be driven to jump");

        // Control, same 20 ticks on dry land: the goal must never start, so
        // the mob must never jump. Without this the assertion above is
        // satisfied by anything that sets `jumping` for any reason.
        let dry = FluidArena::dry();
        let mut dry_mob = perception_mob(&dry, Vec3::new(0.5, 0.0, 0.5));
        let mut dry_ai = GoalSelector::new();
        dry_ai.add(0, Box::new(FloatGoal));
        for _ in 0..20 {
            dry_mob.tick(&mut dry_ai);
            assert!(
                !dry_mob.is_jumping(),
                "a mob on dry land must never be driven to jump by FloatGoal"
            );
        }
    }

    #[test]
    fn a_hurt_mob_retaliates_through_the_real_scheduler_and_forgets_on_vanillas_timer() {
        // `HurtByTargetGoal` end to end: note a hit, run the scheduler, and
        // assert the mob's *attack target* became the attacker — the state a
        // `MeleeAttackGoal` then chases. This is the observable retaliation,
        // not a `can_use` probe.
        let world = FluidArena::dry();
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        let attacker = Vec3::new(4.5, 0.0, 0.5);
        mob.note_hurt(Some(attacker));

        let mut ai = GoalSelector::new();
        // Vanilla puts `HurtByTargetGoal` at target-priority 1 everywhere it
        // appears (`monster/zombie/Zombie.java:124`,
        // `monster/zombie/ZombifiedPiglin.java:75`).
        ai.add(0, Box::new(HurtByTargetGoal::new()));

        mob.tick(&mut ai);
        assert_eq!(
            mob.attack_target(),
            Some(attacker),
            "a hurt mob must adopt its attacker as its attack target"
        );

        // Vanilla forgets the attacker past `LAST_HURT_BY_TICKS`
        // (`LivingEntity.java:493`). Prove the decay is real and lands on the
        // predicted tick rather than merely "eventually": one `note_hurt`
        // followed by exactly that many `advance`s must clear it, and one
        // fewer must not.
        let mut early = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        early.note_hurt(Some(attacker));
        for _ in 0..LAST_HURT_BY_TICKS - 1 {
            early.advance();
        }
        assert_eq!(
            early.last_hurt_by(),
            Some(attacker),
            "the attacker must still be remembered one tick before the window closes"
        );
        early.advance();
        assert_eq!(
            early.last_hurt_by(),
            None,
            "the attacker must be forgotten exactly at LAST_HURT_BY_TICKS"
        );
    }

    #[test]
    fn panic_expires_on_its_own_shorter_window_while_retaliation_persists() {
        // The two records decay independently and on *different* timers
        // (40 vs 100 — `LivingEntity.java:1420-1421` and `:493`). A single
        // shared timer would satisfy "panics then stops panicking", so the
        // discriminating assertion is that at tick 40 the mob has stopped
        // panicking *and is still hunting*.
        let world = FluidArena::dry();
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        let attacker = Vec3::new(2.5, 0.0, 0.5);
        mob.note_hurt(Some(attacker));
        assert!(mob.is_panicking(), "a freshly hit mob must panic");

        for _ in 0..PANIC_DAMAGE_TICKS {
            mob.advance();
        }
        assert!(
            !mob.is_panicking(),
            "panic must expire at PANIC_DAMAGE_TICKS ({PANIC_DAMAGE_TICKS})"
        );
        assert_eq!(
            mob.last_hurt_by(),
            Some(attacker),
            "retaliation must OUTLIVE panic — this is the assertion that fails \
             if the two windows are collapsed into one timer"
        );
    }

    #[test]
    fn attacker_less_damage_panics_without_giving_the_mob_anything_to_chase() {
        // Vanilla's panic reads the damage *source*, not the attacking mob
        // (`ai/goal/PanicGoal.java:61-63` vs
        // `ai/goal/target/HurtByTargetGoal.java:35`), so fall damage panics a
        // cow and gives it no retaliation target. `note_hurt(None)` is that
        // case; without this test the two records could be one field.
        let world = FluidArena::dry();
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        mob.note_hurt(None);
        assert!(mob.is_panicking(), "attacker-less damage must still panic");
        assert_eq!(
            mob.last_hurt_by(),
            None,
            "attacker-less damage must not invent a retaliation target"
        );
        assert!(PanicGoal::new(1.25).can_use(&mut mob));
        assert!(!HurtByTargetGoal::new().can_use(&mut mob));
    }

    #[test]
    fn no_action_time_suppresses_stroll_at_vanillas_threshold() {
        // The seventh, subtler case: `no_action_time`'s trait default of `0`
        // is inert *in the permissive direction*, so stroll was always
        // eligible where vanilla suppresses it. No dead-code warning could
        // fire for this — the goal simply behaved wrong.
        //
        // Vanilla: `checkNoActionTime && mob.getNoActionTime() >= 100`
        // (`ai/goal/RandomStrollGoal.java:43`). Predict the boundary rather
        // than asserting a direction: 99 must still allow, 100 must suppress.
        let world = FluidArena::dry();

        // `interval(1)` makes the goal's own `next_i32(interval) != 0` roll
        // (`goals.rs`, vanilla `RandomStrollGoal.java:47`) deterministic, so
        // the only variable left is the idle suppression.
        let mut allowed = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        allowed.set_no_action_time(99);
        assert!(
            RandomStrollGoal::new(1.0).with_interval(1).can_use(&mut allowed),
            "no_action_time 99 is below vanilla's threshold and must still stroll"
        );

        let mut suppressed = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        suppressed.set_no_action_time(100);
        assert!(
            !RandomStrollGoal::new(1.0).with_interval(1).can_use(&mut suppressed),
            "no_action_time 100 must suppress stroll (RandomStrollGoal.java:43)"
        );
    }
}
