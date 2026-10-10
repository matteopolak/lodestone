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

use lodestone_data::item::Item;
use lodestone_model::{BlockPos, Vec3};

use super::goal::GoalSelector;
use super::locomotion;
use super::mob::{EatenBlock, MobController, ProjectileLaunch, SwoopState, distance_sqr};
use crate::brain::BrainMob;
use crate::pathfinding::{
    Aabb, BlockCues, MobShape, NavMode, SwimRule, PathFinder, PathNavigator, PathParams, PathStart, PathType,
    PathWorld,
};

/// An item accepted by [`NavigatingMob::set_main_hand_item`].
///
/// Spawn equipment is a validated generated [`Item`]; item names acquired
/// dynamically after spawn remain strings. The AI stores only the bare path
/// because its goal predicates intentionally compare paths rather than taking
/// ownership of a registry identity.
pub trait MainHandItem {
    /// Converts the item to the bare path consumed by AI goal predicates.
    fn into_main_hand_path(self) -> String;
}

impl MainHandItem for Item {
    fn into_main_hand_path(self) -> String {
        self.path().to_owned()
    }
}

impl MainHandItem for String {
    fn into_main_hand_path(self) -> String {
        self
    }
}

/// Vanilla `Animal::setInLove`'s love-mode duration, in ticks
/// (`this.inLove = 600;`).
pub const LOVE_TICKS: i32 = 600;

/// Vanilla `AgeableMob::BABY_START_AGE`. The
/// age timer a freshly bred (or otherwise spawned) baby starts at; it counts
/// up by one every tick until it reaches `0` (adult).
pub const BABY_START_AGE: i32 = -24_000;

/// Ticks after a golden dandelion use before the same mob accepts another.
pub const AGE_LOCK_COOLDOWN_TICKS: i32 = 40;

/// Vanilla `Animal::PARENT_AGE_AFTER_BREEDING`.
/// The post-breeding cooldown applied to both parents' age timer; it counts
/// down by one every tick until it reaches `0` (breedable again).
pub const PARENT_AGE_AFTER_BREEDING: i32 = 6000;

/// Vanilla `Creeper::DEFAULT_MAX_SWELL`
/// (`private static final short DEFAULT_MAX_SWELL = 30;`). The fuse length in
/// ticks: [`swell`](NavigatingMob::swell) climbs by
/// [`swell_dir`](MobController::swell_dir) once per [`advance`](NavigatingMob::advance)
/// call, and reaching this value is detonation
/// (`Creeper::tick`, `explodeCreeper()`).
pub const MAX_SWELL: i32 = 30;

/// How long a mob remembers who hurt it, in ticks. Vanilla `LivingEntity::baseTick`
/// clears `lastHurtByMob` once the record ages past this
/// (`else if (this.tickCount - this.lastHurtByMobTimestamp > 100)`), which is
/// what bounds `HurtByTargetGoal`'s retaliation window
/// (`HurtByTargetGoal::canUse` reads exactly that pair).
pub const LAST_HURT_BY_TICKS: i32 = 100;

/// How long a mob stays panicked after taking damage, in ticks. Vanilla's
/// `PanicGoal::shouldPanic` tests
/// `getLastDamageSource() != null`, and `getLastDamageSource` self-clears once
/// the stamp ages past this
/// (`LivingEntity::getLastDamageSource`,
/// `if (this.level().getGameTime() - this.lastDamageStamp > 40L)`).
///
/// Note this is a **different, shorter** window than [`LAST_HURT_BY_TICKS`]:
/// vanilla panics off the *damage source* and retaliates off the *attacking
/// mob*, two independently-decaying records, so a mob keeps chasing its
/// attacker for 60 ticks after it stops fleeing. Collapsing them into one
/// timer would be a silent behaviour change, not a simplification.
pub const PANIC_DAMAGE_TICKS: i32 = 40;

/// Vanilla's base `FOLLOW_RANGE` attribute value, in blocks — the range at
/// which a mob acquires an attack target.
///
/// Vanilla's own generic mob attribute-builder sets it to `16.0` for **every** mob.
/// Note the *registry* default on the attribute itself is `32.0`
/// and is the wrong number to copy: no
/// living entity ever uses it, because the mob supplier always overrides it.
/// Species that raise it do so in their own attribute-builder — zombie and
/// its subclasses `35.0`, blaze `48.0`,
/// enderman `64.0` —
/// which is why this is only the *default* and a host is expected to feed the
/// real per-species value with [`set_follow_range`](NavigatingMob::set_follow_range).
pub const DEFAULT_FOLLOW_RANGE: f64 = 16.0;

/// The sea level a mob starts with, until its host sets its dimension's.
const OVERWORLD_SEA_LEVEL: i32 = 63;

/// The floor vanilla puts under the target-acquisition range, in blocks:
/// its own targeting-conditions test takes the larger of the follow range
/// times a modifier and `2.0`, where `2.0` is vanilla's own
/// minimum-visibility-distance-for-invisible-target constant. It exists so an invisible
/// target is still attackable at point-blank range; the floor applies
/// unconditionally, so a mob whose `FOLLOW_RANGE` is *below* `2.0` still
/// acquires at `2.0`.
pub const MIN_TARGET_VISIBILITY_DISTANCE: f64 = 2.0;

/// Vanilla's own default base-gravity constant (`0.08`): the downward
/// acceleration [`advance`](NavigatingMob::advance) integrates each tick a
/// waypoint sits below the mob, so a drop the pathfinder allowed (see
/// [`crate::pathfinding::MobShape::max_up_step`]'s sibling, the fall-limit
/// this crate's own `mob_drops_down_within_fall_limit` test names) is a
/// gravity-accelerated fall rather than an instant teleport to the landing
/// height. An instant snap is what let the mob's rendered position sink into
/// the block under its *old* x/z before it had walked far enough horizontally
/// to actually be over the drop — the reported "phases through the ground".
pub const FALL_GRAVITY_PER_TICK: f64 = 0.08;

/// Horizontal velocity below this is dropped before each tick's thrust.
/// A mob's eye sits this fraction of the way up its body.
const MOB_EYE_FRACTION: f64 = 0.85;
/// A standing player's eye height.
const PLAYER_EYE_HEIGHT: f64 = 1.62;
/// How far above or below a waypoint a swimmer may be and still have reached it.
const SWIM_VERTICAL_REACH: f64 = 0.5;
/// A path search reaches at least this far however short the mob's follow range.
const MIN_PATH_LENGTH: f64 = 16.0;
const DRIFT_FLOOR: f64 = 0.003;
/// How fast a climber rises while pressed against a wall, blocks per tick.
const CLIMB_SPEED: f64 = 0.2;

/// How far below the feet the block that sets slipperiness is sampled.
const SUPPORT_PROBE: f64 = 0.500_001;

/// Vanilla's own base-vertical-air-drag constant (`0.98`): the per-tick decay
/// [`advance`](NavigatingMob::advance) applies to the stored fall speed
/// between ticks. Paired with [`FALL_GRAVITY_PER_TICK`], the two converge to
/// vanilla's real terminal velocity (`-3.92` blocks/tick), though a path-driven
/// drop is rarely long enough to reach it.
pub const FALL_VERTICAL_AIR_DRAG: f64 = 0.98;

/// The upward speed (blocks/tick) a jump launches with — the default,
/// no-Jump-Boost jump strength every mob without a distinct override gets.
/// [`step_vertical`](NavigatingMob::step_vertical) seeds `fall_speed` with
/// `-JUMP_POWER` the tick a rise exceeds `max_up_step`. See
/// `docs/mob-vertical-motion.md` for the citation, the integration order this
/// value depends on, and the measured peak height it produces.
pub const JUMP_POWER: f64 = 0.42;

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
    /// Everything but the world borrow, so [`tick_in`](Self::tick_in) can run
    /// the same mob against a world that lives for one tick only.
    body: Option<Box<MobBody>>,
}

impl std::ops::Deref for NavigatingMob<'_> {
    type Target = MobBody;
    fn deref(&self) -> &MobBody {
        self.body.as_deref().expect("body is present outside tick_in")
    }
}

impl std::ops::DerefMut for NavigatingMob<'_> {
    fn deref_mut(&mut self) -> &mut MobBody {
        self.body.as_deref_mut().expect("body is present outside tick_in")
    }
}

/// The state of a [`NavigatingMob`] that does not borrow the world.
#[doc(hidden)]
pub struct MobBody {
    shape: MobShape,
    finder: PathFinder,
    navigator: PathNavigator,
    pos: Vec3,
    /// The `movement_speed` attribute in force (baby bonus included): the unit
    /// every navigation speed is expressed in, not a distance.
    movement_speed: f64,
    /// The attribute the mob's goals were built with. Goals bake their speed
    /// at spawn, so a later change to `movement_speed` reaches them as the
    /// ratio of the two.
    goal_basis: f64,
    /// Horizontal velocity carried into the next tick (x, z), before this
    /// tick's thrust.
    drift: (f64, f64),
    /// Whether the feet cell held water / lava when the current tick began.
    in_water: bool,
    in_lava: bool,
    rng: SplitMix64,
    attack_target: Option<Vec3>,
    /// The bare item id (e.g. `"trident"`) this mob spawned holding in its
    /// main hand, if any — vanilla's `getMainHandItem()` narrowed to the one
    /// question a goal like `DrownedTridentAttackGoal.canUse` actually asks.
    /// Set once by the host from
    /// [`crate::spawn_equipment::populate_default_equipment_slots`]; nothing
    /// here mutates it mid-life (no drop/pickup model exists yet).
    main_hand: Option<String>,
    /// What [`crate::brain::NearestHostileSensor`] reads through
    /// [`BrainMob::nearby_entities`] — a host-fed snapshot, refreshed once per
    /// tick, the same shape [`MobController::nearest_player`]'s own host-fed
    /// field is for the goal system. Empty for a goal-driven mob, since only a
    /// host that ticks a [`crate::brain::Brain`] has any reason to populate
    /// it.
    nearby_entities: Vec<crate::brain::NearbyBrainEntity>,
    /// Host-injected world time-of-day, `0..24000` — feeds
    /// [`BrainMob::day_time`], what a villager's schedule
    /// (`crate::brain::roster::villager_brain`) switches `WORK`/`MEET`/`REST`
    /// against. `0` (perpetual midnight) until a host that ticks a real world
    /// clock calls [`set_day_time`](Self::set_day_time) — see that method's
    /// own doc for why this is a *different* clock from
    /// [`tick_count`](Self::tick_count).
    day_time: i32,
    /// Host-injected claimed job-site position — feeds [`BrainMob::job_site`],
    /// which [`crate::brain::VillagerPoiSensor`] copies into
    /// [`crate::brain::MemoryModuleType::JOB_SITE`] each tick. `None` until a
    /// host tracking a live workstation claim calls
    /// [`set_job_site`](Self::set_job_site).
    job_site: Option<Vec3>,
    /// Host-injected claimed bed position — [`job_site`](Self::job_site)'s
    /// sibling for [`BrainMob::home`]/`MemoryModuleType::HOME`.
    home: Option<Vec3>,
    /// Host-injected claimed bell position — [`job_site`](Self::job_site)'s
    /// sibling for [`BrainMob::meeting_point`]/`MemoryModuleType::MEETING_POINT`.
    meeting_point: Option<Vec3>,
    /// Host-injected nearest visible zombified piglin position —
    /// [`job_site`](Self::job_site)'s sibling for
    /// [`BrainMob::nearest_visible_zombified`]/`MemoryModuleType::NEAREST_VISIBLE_ZOMBIFIED`.
    nearest_visible_zombified: Option<Vec3>,
    /// Host-injected nearest eligible tongue-attack prey position —
    /// [`job_site`](Self::job_site)'s sibling for
    /// [`BrainMob::nearest_attackable_food`]/`MemoryModuleType::NEAREST_ATTACKABLE_FOOD`.
    nearest_attackable_food: Option<Vec3>,
    /// Host-injected allay delivery target — [`job_site`](Self::job_site)'s
    /// sibling for [`BrainMob::delivery_target`]/`MemoryModuleType::DELIVERY_TARGET`.
    delivery_target: Option<Vec3>,
    /// Host-injected sniffer dig-search target — [`job_site`](Self::job_site)'s
    /// sibling for [`BrainMob::sniffer_dig_target`]/`MemoryModuleType::SNIFFER_DIG_TARGET`.
    sniffer_dig_target: Option<Vec3>,
    /// The block the current path was computed toward, so `move_to` reuses the
    /// active path instead of recomputing every tick (vanilla `moveTo` reuse).
    active_target_block: Option<BlockPos>,
    last_look: Option<Vec3>,
    jumping: bool,
    attacks: Vec<Vec3>,
    /// Projectile launches a ranged goal asked for, awaiting a host drain.
    /// The mirror of [`attacks`](Self::attacks): this crate can
    /// resolve neither into a real world effect, so both accumulate here for
    /// whoever owns the entity ids.
    launches: Vec<ProjectileLaunch>,
    move_calls: u32,
    path_searches: u32,
    /// Monotonic tick counter (advanced once per [`advance`]/[`tick`]), used to
    /// throttle recomputation the way vanilla's game clock does.
    tick_count: u64,
    /// Offsets this mob's goal cadence (its entity id, so neighbours alternate).
    ai_phase: u64,
    /// The tick a same-destination re-search last ran, so a wedged mob does not
    /// recompute A\* every tick (vanilla `PathNavigation.recomputePath` refuses
    /// to recompute within 20 ticks — `MAX_TIME_RECOMPUTE`).
    last_search_tick: Option<u64>,
    /// The actual position delta applied on the last [`advance`], i.e. the mob's
    /// velocity in **blocks per tick** (vanilla `getDeltaMovement`). Zero when the
    /// follower did not move this tick.
    velocity: Vec3,
    /// Last feet position accepted by the server's live terrain sweep. External
    /// impulses can arrive after the AI step, so the next sweep starts here
    /// rather than silently treating that displacement as already collision-free.
    live_collision_origin: Vec3,
    /// A newly spawned body has no prior live sweep, so the server must check
    /// whether the requested spawn point is embedded in a block. Explicit
    /// position authorities clear this: a teleport or ridden-mob report is not
    /// a physics proposal to depenetrate.
    needs_live_unembed: bool,
    /// The stored downward speed carried between ticks while the mob is
    /// falling toward a waypoint below it — vanilla's own `deltaMovement.y`
    /// between calls to `LivingEntity.travel`, integrated by
    /// [`FALL_GRAVITY_PER_TICK`]/[`FALL_VERTICAL_AIR_DRAG`] each
    /// [`advance`](Self::advance) call and reset to `0.0` the tick the mob is
    /// climbing, level, or has landed. Always `>= 0.0` (a magnitude; this
    /// follower's gravity is always downward, unlike vanilla's single
    /// signed `deltaMovement.y` which also carries jump/knockback).
    fall_speed: f64,
    /// A swimmer's forward speed, eased toward the path's speed, and its vertical
    /// velocity (the horizontal part is [`drift`](Self::drift)).
    swim_speed: f64,
    swim_vy: f64,
    /// A drifter's pulse state: the velocity its goals chose, the velocity it
    /// currently carries, and the phase and rate of its pulse cycle.
    drift_vector: Vec3,
    drift_velocity: Vec3,
    pulse: f64,
    pulse_rate: f64,
    /// A floater's wanted position, the ticks until its next push, its velocity
    /// and its flying speed.
    float_wanted: Option<Vec3>,
    float_duration: i32,
    fly_velocity: Vec3,
    flying_speed: f64,
    /// Whether a path-following flier has started moving, after which gravity no longer applies.
    air_ready: bool,
    /// An amphibian's swimming pitch in degrees, and whether it is looking for land to leave the water.
    swim_pitch: f32,
    searching_for_land: bool,
    /// A bat's roost state: hanging from a ceiling, its wander target block, and
    /// the forward input it last flew with.
    bat: BatState,
    /// A phantom's circling anchor, move target and phase, and its move control's speed.
    swoop: SwoopState,
    swoop_speed: f64,
    swoop_anchored: bool,
    /// A climber's destination, kept after its ground path ends so it keeps
    /// heading straight at it, and whether it pressed against a wall last tick.
    climb_goal: Option<(Vec3, f64)>,
    /// Host-fed daylight state: bright outside, on fire, wearing a helmet.
    sun: (bool, bool, bool),
    /// The dimension's sea level, set by the host each tick.
    sea_level: i32,
    against_wall: bool,
    /// Whether the last step was a wall climb, which gravity does not touch.
    climbed: bool,
    /// Whether the last terrain sweep blocked downward motion. The navigation
    /// snapshot answers path topology, while the server owns the live collision
    /// sweep that refreshes this after each tick.
    on_ground: bool,
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
    /// Vanilla `AgeableMob::age`:
    /// negative while a baby (ticks up toward `0`), positive as the
    /// post-breeding parent cooldown (ticks down toward `0`), `0` for an
    /// adult with no cooldown. [`MobController::is_baby`] is `age < 0`.
    age: i32,
    /// Vanilla `AgeableMob::AGE_LOCKED`: freezes [`age`](Self::age) from
    /// advancing at all while `true`. Toggled by the golden-dandelion
    /// interaction through [`NavigatingMob::toggle_age_lock`].
    age_locked: bool,
    /// Ticks until the golden dandelion may be used on this mob again; set by
    /// [`NavigatingMob::toggle_age_lock`] and counted down by `advance`.
    age_lock_cooldown: i32,
    /// Host injection point, refreshed once per tick: the position of the
    /// nearest eligible adult of this mob's own kind, or `None`. Drives
    /// [`FollowParentGoal`](super::goals::FollowParentGoal) through
    /// [`MobController::parent_position`], the same host-computes-the-filter
    /// shape as [`partner_candidate`](Self::partner_candidate).
    parent_candidate: Option<Vec3>,
    /// Vanilla `Creeper::swellDir` (`DATA_SWELL_DIR`, defaults to `-1` —
    /// `Creeper::defineSynchedData`, `entityData.define(DATA_SWELL_DIR, -1)`). Set by
    /// [`SwellGoal`](super::goals::SwellGoal) through
    /// [`MobController::set_swell_dir`], or forced to `1` every
    /// [`advance`](Self::advance) while [`ignited`](Self::ignited) is `true`
    /// (`Creeper::tick`). `> 0` climbs [`swell`](Self::swell) toward
    /// [`MAX_SWELL`]; `<= 0` lets it fall back toward zero.
    swell_dir: i32,
    /// Vanilla `Creeper::swell`: the live fuse counter, integrated once per
    /// tick in [`advance`](Self::advance) unconditionally — exactly like
    /// [`age`](Self::age)/[`love_ticks`](Self::love_ticks) — regardless of
    /// whether any goal ran this tick. This is the entity's own `tick()`
    /// (`Creeper::tick`), distinct from `SwellGoal`, which only ever
    /// decides the *direction*.
    swell: i32,
    /// Vanilla `Creeper::isIgnited` / `DATA_IS_IGNITED`. `true` forces
    /// [`swell_dir`](Self::swell_dir) to `1` every tick regardless of what
    /// [`SwellGoal`](super::goals::SwellGoal) would otherwise choose
    /// (`Creeper::tick`). Set by [`ignite`](Self::ignite); no
    /// production caller wires a flint-and-steel/fire-charge interaction to
    /// it yet (`Creeper::mobInteract`) — that is a separate,
    /// disclosed gap, not modelled here.
    ignited: bool,
    /// Drained by [`take_detonated`](Self::take_detonated): `true` for
    /// exactly one call, the tick [`swell`](Self::swell) first reaches
    /// [`MAX_SWELL`] (`Creeper::tick`, `explodeCreeper()`). Mirrors
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
    /// (`LookAtPlayerGoal::canUse`) is the host's search to run. The
    /// goal still applies its own `lookDistance` cut-off on top, so a host
    /// that over-reports is merely wasteful, not wrong.
    nearest_player: Option<Vec3>,
    /// Host injection point, refreshed once per tick: the position of a nearby
    /// entity currently tempting this mob, or `None`. Drives
    /// [`MobController::temptation`] and therefore
    /// [`TemptGoal`](super::goals::TemptGoal).
    ///
    /// The host owns **both** halves of vanilla's test: the range (an
    /// attribute, `Attributes::TEMPT_RANGE`, default `10.0`) and the item predicate, which in
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
    /// `6.0F` — `Creeper::registerGoals`), so the *class filter* is the
    /// host's, exactly like the temptation predicate above.
    avoid_threat: Option<Vec3>,
    /// Host injection point: vanilla `Mob::noActionTime`
    /// (`Mob::serverAiStep`'s `this.noActionTime++`, reset to `0` in
    /// `Mob::checkDespawn`). Read by
    /// [`RandomStrollGoal`](super::goals::RandomStrollGoal)'s idle
    /// suppression, which yields at `>= 100` (`RandomStrollGoal::canUse`).
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
    /// (`LivingEntity::baseTick`); a countdown is the same thing with no need
    /// for a shared clock, and it decays in `advance` alongside
    /// [`love_ticks`](Self::love_ticks) for the same reason — vanilla ages it
    /// every tick regardless of whether any goal ran.
    hurt_by_ticks: i32,
    /// The position of whoever most recently damaged this mob's **owner** —
    /// the owner-scoped twin of [`last_hurt_by`](Self::last_hurt_by). A
    /// player is a `LivingEntity` like any other, so `OwnerHurtByTargetGoal`
    /// reading `owner.getLastHurtByMob()` is the *same* field and the *same*
    /// 100-tick clear vanilla applies to a mob's own record — hence sharing
    /// [`LAST_HURT_BY_TICKS`] rather than a separate constant. Recorded by
    /// [`set_owner_hurt_by`](Self::set_owner_hurt_by), decayed in
    /// [`advance`](Self::advance), and read by
    /// [`MobController::owner_hurt_by`] — what
    /// [`OwnerHurtByTargetGoal`](super::goals::OwnerHurtByTargetGoal)
    /// retaliates against.
    owner_hurt_by: Option<Vec3>,
    /// Ticks remaining on [`owner_hurt_by`](Self::owner_hurt_by), decayed the
    /// same way as [`hurt_by_ticks`](Self::hurt_by_ticks).
    owner_hurt_by_ticks: i32,
    /// The position of whoever this mob's **owner** most recently attacked —
    /// drives [`OwnerHurtTargetGoal`](super::goals::OwnerHurtTargetGoal),
    /// which reads vanilla's `owner.getLastHurtMob()` (again the owner's own
    /// `LivingEntity` field, same [`LAST_HURT_BY_TICKS`] clear). Recorded by
    /// [`set_owner_hurt_target`](Self::set_owner_hurt_target).
    owner_hurt_target: Option<Vec3>,
    /// Ticks remaining on [`owner_hurt_target`](Self::owner_hurt_target),
    /// decayed the same way as [`hurt_by_ticks`](Self::hurt_by_ticks).
    owner_hurt_target_ticks: i32,
    /// Ticks remaining on "took damage recently", the panic window
    /// ([`PANIC_DAMAGE_TICKS`]). Set by [`note_hurt`](Self::note_hurt) for
    /// **every** hit, including one with no identifiable attacker, because
    /// vanilla's `shouldPanic` reads the damage *source* rather than the
    /// attacking mob (`PanicGoal::shouldPanic`). Read by
    /// [`MobController::is_panicking`].
    damage_ticks: i32,
    /// This mob's `FOLLOW_RANGE` attribute value, in blocks — the range cut
    /// [`find_nearest_target`](MobController::find_nearest_target) applies to
    /// [`nearest_player`](Self::nearest_player). Defaults to
    /// [`DEFAULT_FOLLOW_RANGE`]; a host feeds the real per-species value with
    /// [`set_follow_range`](Self::set_follow_range).
    ///
    /// **This is the filter, and it is not optional.** The host's
    /// `nearest_player` feed is deliberately *unbounded* — `mobs.rs`'s
    /// `feed_perception` passes no range at all, because the range for
    /// `LookAtPlayerGoal` (its original consumer) lives in the goal — so a
    /// `find_nearest_target` that returned it raw would make every hostile mob
    /// in the world target the player from any distance. Vanilla's cut is
    /// `TargetGoal::getFollowDistance`, i.e. exactly this attribute.
    follow_range: f64,
    /// Host injection point: the entity this mob holds a live persistent grudge
    /// against, or `None`. Drives [`MobController::angry_target`] — see that
    /// method for why the *deadline* stays on the host side and only the answer
    /// crosses the seam.
    ///
    /// Nothing in `lodestone-server` feeds this yet, and no roster row installs
    /// the goal that reads it, so it is `None` in production today. That is the
    /// correct state: a neutral mob whose anger cannot be expressed must be
    /// **passive**, not hostile-on-sight, and the reason the neutral family's
    /// three anger-gated target rows are deliberately `Coverage::Missing`
    /// (`roster::neutral`'s `no_anger_gated_target_row_is_modelled`).
    angry_target: Option<Vec3>,
    /// Blocks a grazing goal has eaten, awaiting a host drain via
    /// [`take_new_eaten`](Self::take_new_eaten) — the
    /// [`attacks`](Self::attacks)/[`launches`](Self::launches) shape, for the
    /// same reason: this crate can write neither a block nor entity metadata.
    eaten: Vec<EatenBlock>,
    /// Host injection point, refreshed once per tick: whether a player is
    /// currently staring at this mob. Drives
    /// [`MobController::is_being_stared_at`] and therefore the enderman's
    /// stare-gated goals. The host computes vanilla's `isLookingAtMe` cone +
    /// line-of-sight from real player view vectors and feeds the boolean —
    /// see that method and [`is_in_view_cone`](super::mob::is_in_view_cone).
    ///
    /// `lodestone_server::mobs::MobSim::tick_with_terrain` feeds this every
    /// tick in production, from real connected-player positions and view
    /// directions; a mob with no host feed at all stays `false`, the value
    /// the trait default is pinned to.
    stared_at: bool,
    /// Host injection point, refreshed once per tick: the position of this
    /// mob's owner, or `None` when it has none. Drives
    /// [`MobController::owner_position`].
    ///
    /// Host-injected exactly like [`parent_candidate`](Self::parent_candidate):
    /// the host owns the owner *identity* (`lodestone_server`'s
    /// `SimMob::owner`) and resolves it to a position each tick. `None` for a
    /// wild mob, **and also for a tamed mob whose owner is offline or out of the
    /// player list** — see [`tame`](Self::tame) for why those are two different
    /// states.
    owner: Option<Vec3>,
    /// Host injection point: vanilla's own tameness flag (the `0x04` bit of
    /// vanilla's own tamed-animal data-flags field). Drives [`MobController::is_tame`].
    ///
    /// Separate from [`owner`](Self::owner) rather than derived from it: a tamed
    /// pet whose owner has logged out still *is* tame, so deriving tameness from
    /// a resolved owner position would un-tame every pet whenever its owner left
    /// the player list. `SitWhenOrderedToGoal`'s untamed-guard arm and
    /// the wolf's own avoid-entity goal's untamed guard both read this.
    tame: bool,
    /// Host injection point: vanilla's own sit-order flag, the persisted
    /// *intent* an owner's right-click toggles. Drives
    /// [`MobController::is_ordered_to_sit`].
    ordered_to_sit: bool,
    /// The sitting **pose** — vanilla's `0x01` `DATA_FLAGS_ID` bit, written by
    /// [`SitWhenOrderedToGoal`](super::goals::SitWhenOrderedToGoal)'s `start`
    /// and `stop` through [`MobController::set_in_sitting_pose`], and read back
    /// by the host to publish that flag.
    ///
    /// Distinct from [`ordered_to_sit`](Self::ordered_to_sit) for the reason
    /// [`MobController::is_ordered_to_sit`] gives: the order survives the goal
    /// being preempted, the pose does not.
    in_sitting_pose: bool,
    /// Host injection point, refreshed once per tick: the nearest chest/lit
    /// furnace candidate, or `None`. Drives [`MobController::cat_sit_target`]
    /// — see that method's own doc for why this is a host-computed candidate
    /// rather than an in-goal search.
    cat_sit_target: Option<Vec3>,
    /// Host injection point, refreshed once per tick: the nearest bed-foot
    /// candidate, or `None`. Drives [`MobController::cat_bed_target`].
    cat_bed_target: Option<Vec3>,
    /// The lying **pose** — vanilla's own cat lying-pose data flag, written by
    /// [`CatLieOnBedGoal`](super::goals::CatLieOnBedGoal) through
    /// [`MobController::set_lying`], and read back by the host to publish
    /// that flag.
    lying: bool,
    /// Host injection point, refreshed once per tick: how long the owner has
    /// been asleep, or `None` if awake. Drives
    /// [`MobController::owner_sleep_ticks`].
    owner_sleep_ticks: Option<u32>,
    /// Set by [`MobController::request_gift`]; drained by the host once per
    /// tick, the same shape as [`eaten`](Self::eaten).
    gift_requested: bool,
    /// Host injection point: vanilla `ShoulderRidingEntity.rideCooldownCounter`.
    /// Drives [`MobController::ticks_since_shoulder_dismount`]. Defaults to
    /// `i32::MAX` — see that method's own doc for why the permissive default.
    ticks_since_shoulder_dismount: i32,
    /// Set by [`MobController::request_shoulder_ride`]; drained by the host
    /// once per tick, the same shape as [`gift_requested`](Self::gift_requested).
    shoulder_ride_requested: bool,
    /// Host injection point: vanilla `PatrollingMonster.patrolling`. Drives
    /// [`MobController::is_patrolling`].
    patrolling: bool,
    /// Host injection point: vanilla `PatrollingMonster.patrolLeader`. Drives
    /// [`MobController::is_patrol_leader`].
    patrol_leader: bool,
    /// This mob's own current long-distance patrol waypoint, round-tripped
    /// through [`LongDistancePatrolGoal`](super::goals::LongDistancePatrolGoal)
    /// via [`MobController::patrol_target`]/[`MobController::set_patrol_target`]
    /// — vanilla `PatrollingMonster.patrolTarget`.
    patrol_target: Option<Vec3>,
    /// Host injection point, refreshed once per tick for a non-leader only:
    /// the patrol's shared waypoint, as the host resolves it from nearby
    /// patrol leaders. Drives [`MobController::patrol_group_target`]; see that
    /// method's own doc comment for why this exists instead of the census
    /// `LongDistancePatrolGoal` cannot run itself.
    patrol_group_target: Option<Vec3>,
    /// Self-damage requests this mob recorded via
    /// [`MobController::damage_self`], awaiting a host drain via
    /// [`take_self_damage`](Self::take_self_damage) — the
    /// [`attacks`](Self::attacks)/[`launches`](Self::launches) shape, for the
    /// same reason: health lives on the host, so this crate can only record
    /// the *intent*. The bee's sting self-destruct is the production consumer
    /// (`Bee::customServerAiStep`).
    self_damage: Vec<f32>,
}

/// Minecraft body yaw (degrees) for a horizontal movement delta: 0 = +Z (south),
/// −90 = +X (east), 90 = −X (west), 180 = −Z (north). Mirrors vanilla's
/// `atan2(dz, dx) * 180/PI - 90` idiom used when a mob faces its motion.
/// Wraps an angle in degrees into (-180, 180].
/// What a fluttering bat remembers between ticks.
#[derive(Debug, Clone)]
struct BatState {
    resting: bool,
    target: Option<(i32, i32, i32)>,
    forward: f64,
}

impl Default for BatState {
    fn default() -> Self {
        Self { resting: true, target: None, forward: 0.0 }
    }
}

fn wrap_degrees(angle: f32) -> f32 {
    let wrapped = angle.rem_euclid(360.0);
    if wrapped > 180.0 { wrapped - 360.0 } else { wrapped }
}

fn movement_yaw(dx: f64, dz: f64) -> f32 {
    (dz.atan2(dx).to_degrees() - 90.0) as f32
}

impl std::fmt::Debug for MobBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobBody").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for NavigatingMob<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavigatingMob")
            .field("shape", &self.shape)
            .field("pos", &self.pos)
            .field("movement_speed", &self.movement_speed)
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
    /// Creates a mob at `pos` with body `shape` and the given `movement_speed`
    /// attribute, pathfinding through `world`.
    ///
    /// `visited_budget` bounds the A\* open set (vanilla derives it as
    /// `floor(followRange * 16)`).
    ///
    /// `seed` seeds the per-mob deterministic RNG (a SplitMix64). Callers
    /// **must** pass a seed that is unique per entity — e.g. the entity's
    /// network id (`id as u64`) — so two mobs of the same species do not act
    /// in lockstep. The seed is deterministic by design: the same world and
    /// seed replayed produces byte-identical mob behaviour. Vanilla seeds
    /// its own per-entity random source the same way; this is our
    /// equivalent, minus the non-determinism.
    #[must_use]
    pub fn new(
        world: &'w dyn PathWorld,
        shape: MobShape,
        pos: Vec3,
        movement_speed: f64,
        visited_budget: i32,
        seed: u64,
    ) -> Self {
        let width = shape.width;
        let swims = shape.nav_mode == NavMode::Swim;
        Self {
            world,
            body: Some(Box::new(MobBody {
            shape,
            finder: PathFinder::new(visited_budget),
            navigator: if swims {
                PathNavigator::new(width).with_vertical_limit(SWIM_VERTICAL_REACH)
            } else {
                PathNavigator::new(width)
            },
            pos,
            movement_speed,
            goal_basis: movement_speed,
            drift: (0.0, 0.0),
            in_water: false,
            in_lava: false,
            rng: SplitMix64(seed),
            attack_target: None,
            main_hand: None,
            nearby_entities: Vec::new(),
            day_time: 0,
            job_site: None,
            home: None,
            meeting_point: None,
            nearest_visible_zombified: None,
            nearest_attackable_food: None,
            delivery_target: None,
            sniffer_dig_target: None,
            active_target_block: None,
            last_look: None,
            jumping: false,
            attacks: Vec::new(),
            launches: Vec::new(),
            move_calls: 0,
            path_searches: 0,
            tick_count: 0,
            ai_phase: 0,
            last_search_tick: None,
            velocity: Vec3::new(0.0, 0.0, 0.0),
            live_collision_origin: pos,
            needs_live_unembed: true,
            fall_speed: 0.0,
            swim_speed: 0.0,
            swim_vy: 0.0,
            drift_vector: Vec3::new(0.0, 0.0, 0.0),
            drift_velocity: Vec3::new(0.0, 0.0, 0.0),
            pulse: 0.0,
            pulse_rate: 0.0,
            float_wanted: None,
            float_duration: 0,
            fly_velocity: Vec3::new(0.0, 0.0, 0.0),
            flying_speed: 0.0,
            air_ready: false,
            swim_pitch: 0.0,
            searching_for_land: false,
            bat: BatState::default(),
            swoop: SwoopState {
                move_target: Vec3::new(0.0, 0.0, 0.0),
                anchor: (0, 0, 0),
                swooping: false,
                blocked: false,
                half_width: 0.0,
                height: 0.0,
            },
            swoop_speed: 0.1,
            swoop_anchored: false,
            climb_goal: None,
            sun: (false, false, false),
            sea_level: OVERWORLD_SEA_LEVEL,
            against_wall: false,
            climbed: false,
            on_ground: false,
            body_yaw: 0.0,
            love_ticks: 0,
            partner_candidate: None,
            bred: false,
            age: 0,
            age_locked: false,
            age_lock_cooldown: 0,
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
            owner_hurt_by: None,
            owner_hurt_by_ticks: 0,
            owner_hurt_target: None,
            owner_hurt_target_ticks: 0,
            damage_ticks: 0,
            follow_range: DEFAULT_FOLLOW_RANGE,
            angry_target: None,
            eaten: Vec::new(),
            stared_at: false,
            owner: None,
            tame: false,
            ordered_to_sit: false,
            in_sitting_pose: false,
            cat_sit_target: None,
            cat_bed_target: None,
            lying: false,
            owner_sleep_ticks: None,
            gift_requested: false,
            ticks_since_shoulder_dismount: i32::MAX,
            shoulder_ride_requested: false,
            patrolling: false,
            patrol_leader: false,
            patrol_target: None,
            patrol_group_target: None,
            self_damage: Vec::new(),
            })),
        }
    }

    /// The mob's current position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.pos
    }

    /// Overwrites the mob's position directly, bypassing pathfinding and the
    /// [`advance`](Self::advance)/[`tick`](Self::tick) follower entirely.
    ///
    /// The one legitimate caller is a driver that is *itself* authoritative
    /// over this mob's movement for the duration — a ridden mount whose rider
    /// reports position the way [`MobSim::apply_vehicle_move`](crate) does for
    /// a boat, or a `/teleport`-style host command. Calling this while the
    /// same tick also runs [`tick`](Self::tick) fights that follower and
    /// produces jitter; a caller that owns the mob's movement for a stretch
    /// must skip `tick` for exactly as long as it calls this instead.
    pub fn set_position(&mut self, pos: Vec3) -> &mut Self {
        self.pos = pos;
        self.live_collision_origin = pos;
        self.needs_live_unembed = false;
        self
    }

    /// Overwrites the mob's body yaw directly — the rotation half of
    /// [`set_position`](Self::set_position), for the same ridden-mount case:
    /// vanilla drives a ridden `AbstractHorse`'s yaw from the rider's own
    /// reported yaw (`Player.setYRot` propagating through
    /// `Entity.positionRider`), not from [`advance`]'s movement-direction
    /// derivation.
    pub fn set_body_yaw(&mut self, yaw: f32) -> &mut Self {
        self.body_yaw = yaw;
        self
    }

    /// The mob's collision body (width/height/step and traversal parameters).
    #[must_use]
    pub fn shape(&self) -> &MobShape {
        &self.shape
    }

    /// Replaces the mob's collision body — the host's hook for
    /// vanilla `LivingEntity::refreshDimensions`, called when
    /// [`set_age`](Self::set_age) crosses the baby/adult boundary and the
    /// host recomputes its species' dimensions for the new state. Vanilla's
    /// `AgeableMob::setAge` flips `DATA_BABY_ID` rather than calling
    /// `refreshDimensions()` itself; the call happens indirectly, through
    /// `AgeableMob::onSyncedDataUpdated` reacting to that flag changing.
    ///
    /// Updates the hitbox only. [`PathNavigator`](super::super::pathfinding::navigation::PathNavigator)
    /// keeps the width it was constructed with — rebuilding it here would
    /// drop any path already in flight, a worse behavioural change than a
    /// baby's slightly-too-wide navigator width. Disclosed rather than
    /// silently traded off.
    /// Sets the entity id that staggers this mob's goal evaluation.
    pub fn set_ai_phase(&mut self, id: u64) {
        self.ai_phase = id;
    }

    /// Sets how far down a drop the next path search may route.
    pub fn set_max_fall_distance(&mut self, blocks: i32) {
        self.shape.max_fall_distance = blocks;
    }

    pub fn set_shape(&mut self, shape: MobShape) -> &mut Self {
        self.shape = shape;
        self
    }

    /// Replaces the `movement_speed` attribute, the host's hook for an age or
    /// effect modifier this crate has no attribute system to recompute.
    pub fn set_movement_speed(&mut self, movement_speed: f64) -> &mut Self {
        self.movement_speed = movement_speed;
        self
    }

    /// The `movement_speed` attribute in force.
    #[must_use]
    pub fn movement_speed(&self) -> f64 {
        self.movement_speed
    }

    /// A goal's requested speed (`modifier * attribute at spawn`) expressed
    /// against the attribute now in force.
    fn goal_speed(&self, requested: f64) -> f64 {
        if self.goal_basis > 0.0 { requested * self.movement_speed / self.goal_basis } else { 0.0 }
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
    /// [`take_new_attacks`](Self::take_new_attacks) shape, for
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

    /// The blocks a grazing goal has eaten (for tests).
    #[must_use]
    pub fn eaten(&self) -> &[EatenBlock] {
        &self.eaten
    }

    /// Drains the blocks eaten since the last call — the
    /// [`take_new_attacks`](Self::take_new_attacks) shape, for
    /// block-perception goals.
    ///
    /// **A host that never calls this makes grazing an island**:
    /// `EatBlockGoal` runs, the eat animation plays out, the sheep's head goes
    /// down, and no grass ever turns to dirt. The host owes two things per
    /// drained entry — the world mutation described on
    /// [`EatenBlock`](super::mob::EatenBlock), gated on `mobGriefing`, and the
    /// species' `ate()` effects (for a sheep, wool regrowth as entity metadata,
    /// which is a wire concern this crate cannot reach).
    pub fn take_new_eaten(&mut self) -> Vec<EatenBlock> {
        std::mem::take(&mut self.eaten)
    }

    /// Drains the gift request since the last call — the same one-shot-flag
    /// shape [`take_new_eaten`](Self::take_new_eaten) uses for a `Vec`. **A
    /// host that never calls this makes the morning gift an island**: the
    /// cat walks to its sleeping owner, lies down, and no item ever appears.
    pub fn take_gift_requested(&mut self) -> bool {
        std::mem::take(&mut self.gift_requested)
    }

    /// Drains the shoulder-ride request since the last call, same shape as
    /// [`take_gift_requested`](Self::take_gift_requested). **A host that
    /// never calls this makes shoulder-riding an island**: the parrot walks
    /// up to its owner and simply stands there forever.
    pub fn take_shoulder_ride_requested(&mut self) -> bool {
        std::mem::take(&mut self.shoulder_ride_requested)
    }

    /// The block position this mob occupies — vanilla `mob.blockPosition()`,
    /// the floor of its feet position.
    #[must_use]
    pub fn block_position(&self) -> BlockPos {
        BlockPos::new(
            self.pos.x.floor() as i32,
            self.pos.y.floor() as i32,
            self.pos.z.floor() as i32,
        )
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

    /// Feet position from which the next live collision sweep must begin.
    #[must_use]
    pub fn live_collision_origin(&self) -> Vec3 {
        self.live_collision_origin
    }

    /// Whether the most recent live-terrain sweep found support below this mob.
    #[must_use]
    pub fn is_on_ground(&self) -> bool {
        self.on_ground
    }

    /// Whether the initial command/plugin spawn position still needs one
    /// live-shape overlap check before normal swept movement begins.
    #[must_use]
    pub fn needs_live_unembed(&self) -> bool {
        self.needs_live_unembed
    }

    /// Commits the live terrain collision result after this tick's kinematic
    /// navigation step.
    ///
    /// `NavigatingMob` deliberately owns navigation over an immutable
    /// [`PathWorld`], while the server owns the current block-state source and
    /// its collision-shape census. The server therefore sweeps the actual body
    /// after [`tick`](Self::tick) and calls this with the resolved feet position.
    /// Keeping the write here makes the next navigation tick observe the same
    /// velocity, grounded state, and vertical reset as the snapshot sent to a
    /// client instead of leaving collision as a server-only correction.
    pub fn apply_live_collision(
        &mut self,
        before: Vec3,
        resolved: Vec3,
        on_ground: bool,
        vertical_collision: bool,
    ) {
        let movement_attempted = self.pos != before;
        if movement_attempted {
            self.against_wall = (resolved.x - self.pos.x).abs() > 1e-9 || (resolved.z - self.pos.z).abs() > 1e-9;
        }
        // A blocked axis kills the velocity carried along it.
        if (resolved.x - self.pos.x).abs() > 1e-9 {
            self.fly_velocity.x = 0.0;
        }
        if (resolved.y - self.pos.y).abs() > 1e-9 {
            self.fly_velocity.y = 0.0;
        }
        if (resolved.z - self.pos.z).abs() > 1e-9 {
            self.fly_velocity.z = 0.0;
        }
        if (resolved.x - self.pos.x).abs() > 1e-9 {
            self.drift.0 = 0.0;
        }
        if (resolved.z - self.pos.z).abs() > 1e-9 {
            self.drift.1 = 0.0;
        }
        self.pos = resolved;
        self.live_collision_origin = resolved;
        self.needs_live_unembed = false;
        if movement_attempted {
            self.velocity = Vec3::new(
                resolved.x - before.x,
                resolved.y - before.y,
                resolved.z - before.z,
            );
        }
        self.on_ground = on_ground;
        if vertical_collision {
            self.fall_speed = 0.0;
            self.swim_vy = 0.0;
        }
    }

    /// Starts one gravity step when the live world removed the support that the
    /// navigation snapshot still contains.
    ///
    /// The ordinary [`advance`](Self::advance) path already integrated a fall
    /// when its snapshot had no floor. This narrow companion is only for the
    /// inverse case: a block was mined after the snapshot, so navigation
    /// supplied a stationary floor height even though the live collision sweep
    /// found no support. Keeping the integration beside `fall_speed` preserves
    /// the same gravity and drag sequence instead of inventing a server-side
    /// teleport-down correction.
    pub fn begin_live_fall_from_unsupported_surface(&mut self) {
        if self.fall_speed < 0.0 || self.climbed || self.shape.nav_mode == NavMode::Swim
            || self.shape.nav_mode.is_airborne()
            || (self.shape.nav_mode == NavMode::Amphibious && self.in_water) {
            return;
        }
        let displacement = self.fall_speed + FALL_GRAVITY_PER_TICK;
        self.fall_speed = displacement * FALL_VERTICAL_AIR_DRAG;
        self.pos.y -= displacement;
    }

    /// Sets the velocity an external hit (melee, explosion, piston) imparts, in
    /// blocks per tick. The horizontal part joins the carried drift, which
    /// then decays by the medium's drag like any other motion; an upward part
    /// launches the body into the jump/fall integrator.
    pub fn apply_knockback(&mut self, velocity: Vec3) {
        self.drift = (velocity.x, velocity.z);
        // Reported at once: the impulse is the mob's velocity from this moment,
        // which the hit's velocity packet carries before the next tick moves it.
        self.velocity = velocity;
        if velocity.y > 0.0 {
            self.fall_speed = -velocity.y;
        }
    }

    /// Moves the body by `delta` immediately and reports it as this tick's
    /// velocity. For host nudges (entity push, leash pull, piston shove) that
    /// are positional corrections rather than velocity the body keeps.
    pub fn displace(&mut self, delta: Vec3) {
        self.pos.x += delta.x;
        self.pos.y += delta.y;
        self.pos.z += delta.z;
        self.velocity = delta;
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

    /// The last cell of the path being followed, if any.
    #[must_use]
    pub fn path_end(&self) -> Option<BlockPos> {
        self.navigator.path().and_then(|p| p.end_node()).map(|n| n.block_pos())
    }

    /// Whether the path being followed actually reaches its destination, as
    /// opposed to ending at the closest point the search found.
    #[must_use]
    pub fn path_reaches_target(&self) -> bool {
        self.navigator.path().is_some_and(|p| p.reached())
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

    /// Freezes (`true`) or resumes (`false`) age advancement.
    pub fn set_age_locked(&mut self, locked: bool) -> &mut Self {
        self.age_locked = locked;
        self
    }

    /// Whether age advancement is frozen.
    #[must_use]
    pub fn is_age_locked(&self) -> bool {
        self.age_locked
    }

    /// Whether the golden dandelion may be used on this mob right now: it is a
    /// baby and the previous use's cooldown has run out.
    #[must_use]
    pub fn can_toggle_age_lock(&self) -> bool {
        self.is_baby() && self.age_lock_cooldown == 0
    }

    /// The golden-dandelion effect: flips the lock, puts the mob back at the
    /// start of babyhood, and starts the [`AGE_LOCK_COOLDOWN_TICKS`] cooldown.
    /// Returns the new lock state.
    pub fn toggle_age_lock(&mut self) -> bool {
        self.age_locked = !self.age_locked;
        self.age = BABY_START_AGE;
        self.age_lock_cooldown = AGE_LOCK_COOLDOWN_TICKS;
        self.age_locked
    }

    /// Enters love mode for [`LOVE_TICKS`] (vanilla `Animal::setInLove`).
    pub fn set_in_love(&mut self) -> &mut Self {
        self.love_ticks = LOVE_TICKS;
        self
    }

    /// Restores a saved love timer, clamped to `0..=`[`LOVE_TICKS`].
    pub fn set_love_time(&mut self, ticks: i32) -> &mut Self {
        self.love_ticks = ticks.clamp(0, LOVE_TICKS);
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

    /// Marks the mob ignited (vanilla `Creeper::ignite`),
    /// forcing its swell direction to climb every
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

    /// Sets this mob's `FOLLOW_RANGE` attribute value in blocks — the range at
    /// which [`find_nearest_target`](MobController::find_nearest_target)
    /// acquires the nearest player.
    ///
    /// Unlike the `set_*` calls around it this is **not** per-tick perception:
    /// it is a species attribute, set once at spawn from the same
    /// `minecraft:follow_range` value `mobs.rs` already reads to size the A\*
    /// visited budget (`floor(follow_range * 16)`). Leaving it at
    /// [`DEFAULT_FOLLOW_RANGE`] is correct for most species and *wrong for the
    /// zombie family*, whose `35.0` is more than twice it
    /// (`Zombie::createAttributes`).
    pub fn set_follow_range(&mut self, blocks: f64) -> &mut Self {
        self.follow_range = blocks;
        self
    }

    /// Host injection point: the item this mob spawned holding in its main
    /// hand, or `None` for empty-handed. A generated [`Item`] reaches this
    /// boundary directly; a `String` remains available for dynamic item names
    /// obtained after spawn. Both become the AI's intentionally bare path,
    /// because its goal predicates compare item paths rather than registry
    /// identities.
    pub fn set_main_hand_item<T: MainHandItem>(
        &mut self,
        item: Option<T>,
    ) -> &mut Self {
        self.main_hand = item.map(MainHandItem::into_main_hand_path);
        self
    }

    /// Host injection point: every entity a brain's perception can currently
    /// see, refreshed once per tick — see [`nearby_entities`](Self::nearby_entities)'s
    /// own doc for what reads it.
    pub fn set_nearby_entities(&mut self, entities: Vec<crate::brain::NearbyBrainEntity>) -> &mut Self {
        self.nearby_entities = entities;
        self
    }

    /// Host injection point: the real world time-of-day, `0..24000` — see
    /// [`day_time`](Self::day_time)'s own field doc for what reads it and why
    /// it is not derived from this mob's own [`tick_count`](Self::tick_count).
    pub fn set_day_time(&mut self, day_time: i32) -> &mut Self {
        self.day_time = day_time;
        self
    }

    /// Host injection point: this villager's claimed job-site position, or
    /// `None` — see [`job_site`](Self::job_site)'s own field doc.
    pub fn set_job_site(&mut self, job_site: Option<Vec3>) -> &mut Self {
        self.job_site = job_site;
        self
    }

    /// Host injection point: this villager's claimed bed position, or `None`
    /// — [`set_job_site`](Self::set_job_site)'s sibling.
    pub fn set_home(&mut self, home: Option<Vec3>) -> &mut Self {
        self.home = home;
        self
    }

    /// Host injection point: this villager's claimed bell position, or `None`
    /// — [`set_job_site`](Self::set_job_site)'s sibling.
    pub fn set_meeting_point(&mut self, meeting_point: Option<Vec3>) -> &mut Self {
        self.meeting_point = meeting_point;
        self
    }

    /// Host injection point: the nearest visible zombified piglin's position,
    /// or `None` — [`set_job_site`](Self::set_job_site)'s sibling for
    /// [`BrainMob::nearest_visible_zombified`].
    pub fn set_nearest_visible_zombified(&mut self, target: Option<Vec3>) -> &mut Self {
        self.nearest_visible_zombified = target;
        self
    }

    /// Host injection point: the nearest eligible tongue-attack prey's
    /// position, or `None` — [`set_job_site`](Self::set_job_site)'s sibling
    /// for [`BrainMob::nearest_attackable_food`].
    pub fn set_nearest_attackable_food(&mut self, target: Option<Vec3>) -> &mut Self {
        self.nearest_attackable_food = target;
        self
    }

    /// Host injection point: the position an allay carrying items should fly
    /// to deposit them, or `None` — [`set_job_site`](Self::set_job_site)'s
    /// sibling for [`BrainMob::delivery_target`].
    pub fn set_delivery_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.delivery_target = target;
        self
    }

    /// Host injection point: a sniffer's current candidate dig position, or
    /// `None` — [`set_job_site`](Self::set_job_site)'s sibling for
    /// [`BrainMob::sniffer_dig_target`].
    pub fn set_sniffer_dig_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.sniffer_dig_target = target;
        self
    }

    /// Host injection point: the entity this mob holds a live persistent grudge
    /// against, or `None` once the grudge expires. The host resolves vanilla's
    /// absolute anger deadline and feeds only the answer — see
    /// [`MobController::angry_target`] for the citation and for why a countdown
    /// would be the wrong model.
    pub fn set_angry_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.angry_target = target;
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

    /// Host injection point: whether a player is currently staring at this
    /// mob. Computed by the host from vanilla's `isLookingAtMe` cone plus line
    /// of sight — see [`MobController::is_being_stared_at`] and
    /// [`is_in_view_cone`](super::mob::is_in_view_cone).
    pub fn set_stared_at(&mut self, stared_at: bool) -> &mut Self {
        self.stared_at = stared_at;
        self
    }

    /// Host injection point: refreshes this mob's owner position from the
    /// host's owner-identity census. `None` for a wild (ownerless) mob — the
    /// state every mob starts in.
    pub fn set_owner(&mut self, owner: Option<Vec3>) -> &mut Self {
        self.owner = owner;
        self
    }

    /// Host injection point: refreshes [`CatSitOnBlockGoal`](super::goals::CatSitOnBlockGoal)'s
    /// candidate target from the host's bounded block search. `None` when the
    /// host found nothing (or has not searched this species at all).
    pub fn set_cat_sit_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.cat_sit_target = target;
        self
    }

    /// Host injection point: refreshes [`CatLieOnBedGoal`](super::goals::CatLieOnBedGoal)'s
    /// candidate target from the host's bounded block search.
    pub fn set_cat_bed_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.cat_bed_target = target;
        self
    }

    /// Host injection point: refreshes
    /// [`CatRelaxOnOwnerGoal`](super::goals::CatRelaxOnOwnerGoal)'s read of
    /// how long the owner has been asleep. See
    /// [`MobController::owner_sleep_ticks`]'s own doc for the two moments it
    /// is read.
    pub fn set_owner_sleep_ticks(&mut self, ticks: Option<u32>) -> &mut Self {
        self.owner_sleep_ticks = ticks;
        self
    }

    /// Host injection point: refreshes
    /// [`LandOnOwnersShoulderGoal`](super::goals::LandOnOwnersShoulderGoal)'s
    /// read of `ShoulderRidingEntity.rideCooldownCounter`.
    pub fn set_ticks_since_shoulder_dismount(&mut self, ticks: i32) -> &mut Self {
        self.ticks_since_shoulder_dismount = ticks;
        self
    }

    /// Host injection point: vanilla `TamableAnimal.setTame`'s `0x04` bit. See
    /// the `tame` field's own doc comment for why this is not
    /// `set_owner(Some(..)).is_some()`.
    pub fn set_tame(&mut self, tame: bool) -> &mut Self {
        self.tame = tame;
        self
    }

    /// Host injection point: vanilla `TamableAnimal.setOrderedToSit`, the
    /// persisted sitting *intent*.
    pub fn set_ordered_to_sit(&mut self, ordered_to_sit: bool) -> &mut Self {
        self.ordered_to_sit = ordered_to_sit;
        self
    }

    /// Host injection point: vanilla `PatrollingMonster.setPatrolling`/the
    /// `patrolling` side effect of `setPatrolLeader`/`setPatrolTarget`.
    pub fn set_patrolling(&mut self, patrolling: bool) -> &mut Self {
        self.patrolling = patrolling;
        self
    }

    /// Host injection point: vanilla `PatrollingMonster.setPatrolLeader`.
    /// **Does not** also set `patrolling` — unlike vanilla's own method, which
    /// folds the two together — because the host here already calls
    /// [`set_patrolling`](Self::set_patrolling) explicitly at spawn time; two
    /// setters each doing one thing is more legible at the call site than one
    /// that quietly does two.
    pub fn set_patrol_leader(&mut self, leader: bool) -> &mut Self {
        self.patrol_leader = leader;
        self
    }

    /// Host injection point, refreshed once per tick for a non-leader: the
    /// patrol's shared waypoint, resolved by the host from nearby patrol
    /// leaders. See [`MobController::patrol_group_target`]'s own doc comment.
    pub fn set_patrol_group_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.patrol_group_target = target;
        self
    }

    /// This mob's own current long-distance patrol waypoint, for a host that
    /// wants to read back what [`LongDistancePatrolGoal`](super::goals::LongDistancePatrolGoal)
    /// last chose — e.g. to resolve [`patrol_group_target`](Self::set_patrol_group_target)
    /// for a *different* mob's follower this same tick.
    #[must_use]
    pub fn patrol_target(&self) -> Option<Vec3> {
        self.patrol_target
    }

    /// Whether this mob is currently patrolling — vanilla's own
    /// patrolling-monster query, exposed for a host census (e.g.
    /// "which nearby mobs are patrol leaders").
    #[must_use]
    pub fn is_patrolling(&self) -> bool {
        self.patrolling
    }

    /// Whether this mob leads its patrol — vanilla's own
    /// patrol-leader query, exposed for the same census
    /// reason as [`is_patrolling`](Self::is_patrolling).
    #[must_use]
    pub fn is_patrol_leader(&self) -> bool {
        self.patrol_leader
    }

    /// Whether this mob is currently in the sitting **pose** —
    /// [`SitWhenOrderedToGoal`](super::goals::SitWhenOrderedToGoal)'s observable
    /// output, and the `0x01` `DATA_FLAGS_ID` bit the host publishes. Read this
    /// (not [`is_ordered_to_sit`](MobController::is_ordered_to_sit)) to answer
    /// "did the goal actually run".
    #[must_use]
    pub fn is_in_sitting_pose(&self) -> bool {
        self.in_sitting_pose
    }

    /// The self-damage requests a goal or host logic recorded (for tests).
    #[must_use]
    pub fn self_damage(&self) -> &[f32] {
        &self.self_damage
    }

    /// Drains the self-damage requests recorded since the last call — the
    /// [`take_new_attacks`](Self::take_new_attacks) shape. The host applies
    /// each amount through its normal damage pipeline (i-frames and reductions
    /// included, matching vanilla's `hurtServer`).
    ///
    /// **A host that never calls this turns a mob's self-harm into a no-op
    /// with no error**: the request lands in this `Vec` and no health ever
    /// changes. `lodestone_server::mobs::MobSim::tick` is the one production
    /// caller.
    pub fn take_self_damage(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.self_damage)
    }

    /// Records that this mob just took damage, optionally from an attacker at
    /// `attacker`.
    ///
    /// This is one call for vanilla's two separate records, because one hit
    /// writes both: `LivingEntity::hurtServer` sets `lastDamageSource`
    /// directly — which is what
    /// [`PanicGoal`](super::goals::PanicGoal) reads — *and*, when the source
    /// has a living attacker, calls `LivingEntity::resolveMobResponsibleForDamage`,
    /// which calls `setLastHurtByMob`, which is what
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

    /// Records that this mob's **owner** was just damaged by an attacker at
    /// `attacker` — vanilla's `LivingEntity::setLastHurtByMob`, called on the
    /// *owner's* entity rather than this one. The host (which alone knows who
    /// owns whom, and resolves a hit against a player) calls this on every
    /// tamed pet belonging to that player the instant the hit resolves; see
    /// [`owner_hurt_by`](Self::owner_hurt_by)'s own doc comment for the decay
    /// rule. `None` clears the record without restarting the countdown.
    pub fn set_owner_hurt_by(&mut self, attacker: Option<Vec3>) -> &mut Self {
        if let Some(attacker) = attacker {
            self.owner_hurt_by = Some(attacker);
            self.owner_hurt_by_ticks = LAST_HURT_BY_TICKS;
        } else {
            self.owner_hurt_by = None;
            self.owner_hurt_by_ticks = 0;
        }
        self
    }

    /// Records that this mob's **owner** just attacked `target` — vanilla's
    /// `LivingEntity::setLastHurtMob`, called on the owner's entity. Same
    /// host-drives-it shape as [`set_owner_hurt_by`](Self::set_owner_hurt_by);
    /// see [`owner_hurt_target`](Self::owner_hurt_target)'s doc comment for
    /// the decay rule.
    pub fn set_owner_hurt_target(&mut self, target: Option<Vec3>) -> &mut Self {
        if let Some(target) = target {
            self.owner_hurt_target = Some(target);
            self.owner_hurt_target_ticks = LAST_HURT_BY_TICKS;
        } else {
            self.owner_hurt_target = None;
            self.owner_hurt_target_ticks = 0;
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
    /// [`MobController`] seam) followed by one locomotion step.
    pub fn tick(&mut self, ai: &mut GoalSelector) {
        self.sense_fluids();
        // Goals are evaluated on every second tick, offset per mob so a herd
        // does not think in lockstep; the off ticks run only the goals that
        // need every tick.
        let off_phase = (self.tick_count + 1 + self.ai_phase) % 2 != 0 && self.tick_count + 1 > 1;
        if off_phase {
            ai.tick_every_tick_goals(self);
        } else {
            ai.tick(self);
        }
        self.advance();
    }

    /// Reads the feet cell's fluid from the current world.
    fn sense_fluids(&mut self) {
        let (x, y, z) = self.feet_block();
        self.in_water = self.world.is_water(x, y, z);
        self.in_lava = matches!(self.world.base_path_type(x, y, z), PathType::Lava);
    }

    /// [`tick`](Self::tick) against `world` instead of the one the mob was
    /// built with, for a world that exists only for this tick (live terrain).
    pub fn tick_in(&mut self, world: &dyn PathWorld, ai: &mut GoalSelector) {
        let mut scoped = NavigatingMob { world, body: self.body.take() };
        scoped.tick(ai);
        self.body = scoped.body.take();
    }

    /// One tick of vertical motion toward `waypoint_y`, bounded independently
    /// of the horizontal step [`advance`](Self::advance) applies alongside
    /// it. Three cases, keyed on `dy = waypoint_y - pos_y` and whether a
    /// jump/fall is already in progress (`*fall_speed != 0.0`, a
    /// positive-down signed speed carried between calls) — see
    /// `docs/mob-vertical-motion.md` for the full derivation, the measured
    /// jump-peak height, and why the two motion cases below integrate
    /// gravity in different orders on purpose.
    ///
    /// - **Auto-step** (grounded, `0.0 <= dy <= max_up_step`): resolves in
    ///   this one call, no jump — a rise this small is absorbed within one
    ///   tick's collision response with no visible pause.
    /// - **Jump** (grounded, `dy > max_up_step`): seeds `*fall_speed =
    ///   -JUMP_POWER` and integrates one tick of real projectile motion —
    ///   this tick's displacement is the speed *already stored* (not yet
    ///   reduced by this tick's gravity), and gravity/drag then update the
    ///   stored speed for the *next* call. A jump already in progress
    ///   (`*fall_speed < 0.0`) keeps integrating the same way regardless of
    ///   `dy`, exactly like vanilla's own jump control staying committed to
    ///   an ascent already under way.
    /// - **Falling** (an ordinary drop, or a jump's arc past its peak):
    ///   gravity is folded into *this* tick's own displacement before
    ///   moving, and landing on `waypoint_y` resets `*fall_speed` to `0.0`.
    ///
    /// An unconditional `pos.y = waypoint_y` — what the fall case here
    /// replaced — produced two symptoms from one cause: a full-block rise
    /// "glided" up with no jump, and a drop let the mob's rendered y reach
    /// the lower floor before its x/z had travelled far enough to actually
    /// be over the edge, so it visibly sank into the block under its *old*
    /// position — "phases through the ground". A climb bounded at the
    /// step-per-tick rate (what the jump case here replaced in turn) fixed
    /// the glide but still had no upward hop at all — "unnatural" jumping.
    fn step_vertical(pos_y: f64, waypoint_y: f64, max_up_step: f64, fall_speed: &mut f64) -> f64 {
        let dy = waypoint_y - pos_y;
        if *fall_speed == 0.0 {
            if dy >= 0.0 && dy <= max_up_step {
                return waypoint_y;
            }
            if dy > max_up_step {
                *fall_speed = -JUMP_POWER;
            }
            // dy < 0.0 here (a fresh drop) leaves `*fall_speed` at `0.0` and
            // falls through to the falling case below.
        }
        if *fall_speed < 0.0 {
            let displacement = *fall_speed;
            *fall_speed = (*fall_speed + FALL_GRAVITY_PER_TICK) * FALL_VERTICAL_AIR_DRAG;
            return pos_y - displacement;
        }
        let displacement = *fall_speed + FALL_GRAVITY_PER_TICK;
        *fall_speed = displacement * FALL_VERTICAL_AIR_DRAG;
        let landed = (pos_y - displacement).max(waypoint_y);
        if landed <= waypoint_y {
            *fall_speed = 0.0;
        }
        landed
    }

    /// The y coordinate a mob standing at `pos` should rest at: the top of
    /// the first solid or fluid column beneath its feet, searched downward
    /// from the block it currently occupies. This is [`step_vertical`]'s
    /// "no active waypoint" substitute for a pathfinder-supplied target —
    /// see [`advance`](Self::advance)'s no-waypoint branch for why one is
    /// needed at all. Bounded by [`PathWorld::min_y`], so a mob over the
    /// void falls until it passes the world floor rather than looping
    /// forever.
    fn ground_below(&self, pos: Vec3) -> f64 {
        let x = pos.x.floor() as i32;
        let z = pos.z.floor() as i32;
        let min_y = self.world.min_y();
        let mut y = pos.y.floor() as i32;
        while y > min_y {
            let below = y - 1;
            let top = self.world.collision_top(x, below, z);
            if top > 0.0 {
                return f64::from(below) + top;
            }
            if self.world.is_water(x, below, z) {
                return f64::from(below) + 1.0;
            }
            y = below;
        }
        f64::from(min_y)
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
        self.age_lock_cooldown = (self.age_lock_cooldown - 1).max(0);
        if !self.age_locked {
            if self.age < 0 {
                self.age += 1;
            } else if self.age > 0 {
                self.age -= 1;
            }
        }
        // Vanilla ages both damage records every tick with
        // no goal involvement: `lastHurtByMob` is dropped past 100 ticks
        // (`LivingEntity::baseTick`) and `getLastDamageSource` self-clears past
        // 40 (`LivingEntity::getLastDamageSource`). Same "integrate unconditionally"
        // placement as the age/love/swell counters above, and for the same
        // reason — a goal that stops running must not freeze the timer that
        // ends it.
        if self.hurt_by_ticks > 0 {
            self.hurt_by_ticks -= 1;
            if self.hurt_by_ticks == 0 {
                self.last_hurt_by = None;
            }
        }
        // Same decay, applied to the owner-scoped twins — see
        // [`owner_hurt_by`](Self::owner_hurt_by)'s own doc comment for why
        // they share vanilla's ordinary `lastHurtByMob`/`lastHurtMob` clear
        // rather than a bespoke window.
        if self.owner_hurt_by_ticks > 0 {
            self.owner_hurt_by_ticks -= 1;
            if self.owner_hurt_by_ticks == 0 {
                self.owner_hurt_by = None;
            }
        }
        if self.owner_hurt_target_ticks > 0 {
            self.owner_hurt_target_ticks -= 1;
            if self.owner_hurt_target_ticks == 0 {
                self.owner_hurt_target = None;
            }
        }
        if self.damage_ticks > 0 {
            self.damage_ticks -= 1;
        }
        // Vanilla `Creeper::tick`: runs every tick
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
        let pos = self.pos;
        let mut waypoint = self.navigator.tick(pos);
        let mut heading_only = false;
        if self.shape.can_climb && waypoint.is_none() {
            waypoint = self.climb_waypoint();
            heading_only = waypoint.is_some();
        }
        if self.shape.nav_mode == NavMode::Amphibious {
            if self.in_water && self.swims_now() {
                self.amphibious_swim_step(waypoint);
                self.velocity = Vec3::new(self.pos.x - old.x, self.pos.y - old.y, self.pos.z - old.z);
                if self.velocity.x * self.velocity.x + self.velocity.z * self.velocity.z > 1e-12 {
                    self.body_yaw = movement_yaw(self.velocity.x, self.velocity.z);
                }
                return;
            }
            self.fly_velocity = Vec3::new(0.0, 0.0, 0.0);
        }
        if self.shape.nav_mode == NavMode::Swoop {
            self.swoop_step();
            self.velocity = Vec3::new(self.pos.x - old.x, self.pos.y - old.y, self.pos.z - old.z);
            return;
        }
        if self.shape.nav_mode == NavMode::Flutter {
            self.flutter_step();
            self.velocity = Vec3::new(self.pos.x - old.x, self.pos.y - old.y, self.pos.z - old.z);
            return;
        }
        if self.shape.nav_mode == NavMode::Air {
            self.air_step(waypoint);
            self.velocity = Vec3::new(self.pos.x - old.x, self.pos.y - old.y, self.pos.z - old.z);
            return;
        }
        if self.shape.nav_mode == NavMode::Fly {
            self.fly_step();
            self.velocity = Vec3::new(self.pos.x - old.x, self.pos.y - old.y, self.pos.z - old.z);
            if self.velocity.x * self.velocity.x + self.velocity.z * self.velocity.z > 1e-12 {
                self.body_yaw = movement_yaw(self.velocity.x, self.velocity.z);
            }
            return;
        }
        if self.shape.nav_mode == NavMode::Drift {
            self.drift_step();
            self.velocity = Vec3::new(self.pos.x - old.x, self.pos.y - old.y, self.pos.z - old.z);
            if self.velocity.x * self.velocity.x + self.velocity.z * self.velocity.z > 1e-12 {
                self.body_yaw = movement_yaw(self.velocity.x, self.velocity.z);
            }
            return;
        }
        if self.shape.nav_mode == NavMode::Swim {
            self.swim_step(waypoint);
            self.velocity = Vec3::new(self.pos.x - old.x, self.pos.y - old.y, self.pos.z - old.z);
            if self.velocity.x * self.velocity.x + self.velocity.z * self.velocity.z > 1e-12 {
                self.body_yaw = movement_yaw(self.velocity.x, self.velocity.z);
            }
            return;
        }
        let medium = self.medium();
        if self.drift.0.abs() < DRIFT_FLOOR {
            self.drift.0 = 0.0;
        }
        if self.drift.1.abs() < DRIFT_FLOOR {
            self.drift.1 = 0.0;
        }
        // The move control pushes toward the waypoint; with no path the body
        // only coasts (or falls).
        let mut to_waypoint = (0.0, 0.0);
        let mut thrust = (0.0, 0.0);
        if let Some(w) = waypoint {
            to_waypoint = (w.x - self.pos.x, w.z - self.pos.z);
            let distance = to_waypoint.0.hypot(to_waypoint.1);
            if distance > 0.0 {
                let push = locomotion::thrust(self.navigator.speed() * self.land_speed_factor(), medium) / distance;
                thrust = (to_waypoint.0 * push, to_waypoint.1 * push);
            }
        }
        let step = (self.drift.0 + thrust.0, self.drift.1 + thrust.1);
        self.pos.x += step.0;
        self.pos.z += step.1;
        let retained = f64::from(self.footing_speed_factor()) * locomotion::drag(medium);
        self.drift = (step.0 * retained, step.1 * retained);

        let climbing = self.shape.can_climb && self.against_wall;
        self.climbed = climbing;
        match waypoint {
            _ if climbing => {
                self.pos.y += CLIMB_SPEED;
                self.fall_speed = 0.0;
            }
            Some(w) if !heading_only => {
                if self.reached_vertical_transition(w, to_waypoint, step) || self.fall_speed != 0.0 {
                    self.pos.y = Self::step_vertical(
                        self.pos.y,
                        w.y,
                        f64::from(self.shape.max_up_step),
                        &mut self.fall_speed,
                    );
                }
            }
            _ => {
                // Most of a mob's life is between paths; gravity still applies.
                let ground_y = self.ground_below(self.pos);
                self.pos.y = Self::step_vertical(
                    self.pos.y,
                    ground_y,
                    f64::from(self.shape.max_up_step),
                    &mut self.fall_speed,
                );
            }
        }
        self.against_wall = false;
        let moved_x = self.pos.x - old.x;
        let moved_z = self.pos.z - old.z;
        self.velocity = Vec3::new(moved_x, self.pos.y - old.y, moved_z);
        if moved_x * moved_x + moved_z * moved_z > 1e-12 {
            self.body_yaw = movement_yaw(moved_x, moved_z);
        }
    }

    /// Sets the sea level of the dimension the mob is in.
    pub fn set_sea_level(&mut self, sea_level: i32) {
        self.sea_level = sea_level;
    }

    /// Feeds the daylight state the sun goals read: whether it is bright
    /// outside, whether the mob is on fire, and whether it wears a helmet.
    pub fn set_sun_state(&mut self, bright_outside: bool, burning: bool, helmeted: bool) {
        self.sun = (bright_outside, burning, helmeted);
    }

    /// Where a climber heads once its ground path has ended: straight at its
    /// destination until the body is within its own width of it, or above it and
    /// within that width horizontally.
    fn climb_waypoint(&mut self) -> Option<Vec3> {
        let (goal, speed) = self.climb_goal?;
        let width = f64::from(self.shape.width);
        let flat = (goal.x - self.pos.x).hypot(goal.z - self.pos.z);
        let near = flat.hypot(goal.y - self.pos.y) < width;
        if near || (self.pos.y > goal.y && flat < width) {
            self.climb_goal = None;
            return None;
        }
        self.navigator.set_speed(speed);
        
        Some(goal)
    }

    /// One tick of path-following flight. Toward the waypoint the body turns at
    /// most 90 degrees a tick, then accelerates by 0.02 along its heading scaled
    /// by the requested speed (the flying-speed attribute times the goal's
    /// modifier), with a vertical share of the same size whenever the waypoint
    /// is not level; air keeps 0.91 of the velocity. Before its first move the
    /// body still falls.
    fn air_step(&mut self, waypoint: Option<Vec3>) {
        const THRUST: f64 = 0.02;
        const AIR_DRAG: f64 = 0.91;
        const MAX_TURN: f32 = 90.0;
        let mut input = (0.0, 0.0);
        if let Some(w) = waypoint {
            self.air_ready = true;
            let (xd, yd, zd) = (w.x - self.pos.x, w.y - self.pos.y, w.z - self.pos.z);
            if xd * xd + yd * yd + zd * zd >= 2.500_000_3e-7 {
                let target = movement_yaw(xd, zd);
                let turn = wrap_degrees(target - self.body_yaw).clamp(-MAX_TURN, MAX_TURN);
                self.body_yaw = wrap_degrees(self.body_yaw + turn);
                let modifier = if self.movement_speed > 0.0 { self.navigator.speed() / self.movement_speed } else { 0.0 };
                let speed = modifier * self.flying_speed;
                let flat = xd.hypot(zd);
                let vertical = if yd.abs() > 1e-5 || flat > 1e-5 { if yd > 0.0 { speed } else { -speed } } else { 0.0 };
                input = (vertical, speed);
            }
        }
        let (vertical, forward) = input;
        let length = vertical.hypot(forward);
        let scale = if length > 1.0 { THRUST / length } else { THRUST };
        let yaw = f64::from(self.body_yaw).to_radians();
        let v = self.fly_velocity;
        let mut vy = v.y + vertical * scale;
        if !self.air_ready {
            vy -= FALL_GRAVITY_PER_TICK;
        }
        let (vx, vz) = (v.x - forward * scale * yaw.sin(), v.z + forward * scale * yaw.cos());
        self.pos.x += vx;
        self.pos.y += vy;
        self.pos.z += vz;
        self.fly_velocity = Vec3::new(vx * AIR_DRAG, vy * AIR_DRAG, vz * AIR_DRAG);
    }

    /// A wander destination for a path-following flier: up to eight blocks out
    /// within a quarter turn either side of its heading and seven vertically,
    /// lifted one to three blocks above any solid it lands in, and rejected on
    /// water or a pathing penalty; if ten tries fail, up to eight out and four
    /// vertically, two below its level, lifted clear of solid only.
    fn air_wander_target(&mut self) -> Option<Vec3> {
        const TRIES: u32 = 10;
        let heading = f64::from(self.body_yaw).to_radians();
        let (dir_x, dir_z) = (-heading.sin(), heading.cos());
        let solid = |w: &dyn PathWorld, x: i32, y: i32, z: i32| w.collision_top(x, y, z) > 0.0;
        for (vertical, flying_height, hover) in [(7, 0, true), (4, -2, false)] {
            for _ in 0..TRIES {
                let Some((xt, yt, zt)) = self.random_direction(8.0, vertical, flying_height, dir_x, dir_z) else {
                    continue;
                };
                let x = (f64::from(xt.floor() as i32) + self.pos.x).floor() as i32;
                let mut y = (f64::from(yt) + self.pos.y).floor() as i32;
                let z = (f64::from(zt.floor() as i32) + self.pos.z).floor() as i32;
                if y < self.world.min_y() {
                    continue;
                }
                let lift = if hover { MobController::next_i32(self, 3) + 1 } else { 0 };
                if solid(self.world, x, y, z) {
                    y += 1;
                    while y < self.world.min_y() + 4096 && solid(self.world, x, y, z) {
                        y += 1;
                    }
                    let first_open = y;
                    while y - first_open < lift && !solid(self.world, x, y + 1, z) {
                        y += 1;
                    }
                }
                let kind = self.world.base_path_type(x, y, z);
                if kind == PathType::Water || self.shape.malus(kind) != 0.0 {
                    continue;
                }
                return Some(Vec3::new(f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5));
            }
        }
        None
    }

    /// A random offset up to `reach` blocks horizontally within a quarter turn
    /// either side of the direction (`dir_x`, `dir_z`), and `vertical` blocks
    /// up or down shifted by `flying_height`.
    fn random_direction(&mut self, reach: f64, vertical: i32, flying_height: i32, dir_x: f64, dir_z: f64) -> Option<(f64, i32, f64)> {
        let centre = dir_z.atan2(dir_x) - std::f64::consts::FRAC_PI_2;
        let angle = centre + (2.0 * f64::from(MobController::next_f32(self)) - 1.0) * std::f64::consts::FRAC_PI_2;
        let dist = MobController::next_f64(self).sqrt() * reach * std::f64::consts::SQRT_2;
        let (xt, zt) = (-dist * angle.sin(), dist * angle.cos());
        if xt.abs() > reach || zt.abs() > reach {
            return None;
        }
        let yt = MobController::next_i32(self, 2 * vertical + 1) - vertical + flying_height;
        Some((xt, yt, zt))
    }

    /// Sets the circling anchor five blocks above where the mob first acts.
    fn anchor_at_spawn(&mut self) {
        if !self.swoop_anchored {
            self.swoop.anchor = (self.pos.x.floor() as i32, self.pos.y.floor() as i32 + 5, self.pos.z.floor() as i32);
            self.swoop_anchored = true;
        }
    }

    /// One tick of the circle-and-swoop move control. A wall turns the body
    /// around and resets its speed. Toward the move target the heading turns at
    /// most 4 degrees a tick, and the speed climbs toward 1.8 while the heading
    /// holds within 3 degrees of last tick's and falls toward 0.2 while it
    /// turns; the velocity then eases a fifth of the way to the speed spread
    /// over the three axes by the target's direction. Air keeps 0.91 of it.
    fn swoop_step(&mut self) {
        const MAX_TURN: f32 = 4.0;
        self.anchor_at_spawn();
        self.swoop.half_width = f64::from(self.shape.width) / 2.0;
        self.swoop.height = f64::from(self.shape.height);
        self.swoop.blocked = self.against_wall;
        if self.swoop.blocked {
            self.body_yaw += 180.0;
            self.swoop_speed = 0.1;
        }
        let mut tdx = self.swoop.move_target.x - self.pos.x;
        let tdy = self.swoop.move_target.y - self.pos.y;
        let mut tdz = self.swoop.move_target.z - self.pos.z;
        let mut flat = tdx.hypot(tdz);
        if flat.abs() > 1e-5 {
            let scale = 1.0 - (tdy * f64::from(0.7_f32)).abs() / flat;
            tdx *= scale;
            tdz *= scale;
            flat = tdx.hypot(tdz);
            let reach = (tdx * tdx + tdz * tdz + tdy * tdy).sqrt();
            let previous = self.body_yaw;
            let heading = wrap_degrees(tdz.atan2(tdx).to_degrees() as f32);
            let current = wrap_degrees(self.body_yaw + 90.0);
            let turn = wrap_degrees(heading - current).clamp(-MAX_TURN, MAX_TURN);
            self.body_yaw = current + turn - 90.0;
            let (speed, steady) = (self.swoop_speed, wrap_degrees(self.body_yaw - previous).abs() < 3.0);
            self.swoop_speed = if steady {
                (speed + 0.005 * (1.8 / speed)).min(1.8)
            } else {
                (speed - 0.025).max(0.2)
            };
            let pitch = -(-tdy).atan2(flat);
            let travel = f64::from(self.body_yaw + 90.0).to_radians();
            let want = Vec3::new(
                self.swoop_speed * travel.cos() * (tdx / reach).abs(),
                self.swoop_speed * pitch.sin() * (tdy / reach).abs(),
                self.swoop_speed * travel.sin() * (tdz / reach).abs(),
            );
            let v = self.fly_velocity;
            self.fly_velocity = Vec3::new(v.x + (want.x - v.x) * 0.2, v.y + (want.y - v.y) * 0.2, v.z + (want.z - v.z) * 0.2);
        }
        let v = self.fly_velocity;
        self.pos.x += v.x;
        self.pos.y += v.y;
        self.pos.z += v.z;
        self.fly_velocity = Vec3::new(v.x * 0.91, v.y * 0.91, v.z * 0.91);
    }

    /// The factor a walking amphibian applies to its requested speed.
    fn land_speed_factor(&self) -> f64 {
        match (self.shape.nav_mode, self.shape.swim_rule) {
            (NavMode::Amphibious, SwimRule::Smooth { on_land, .. }) => on_land,
            _ => 1.0,
        }
    }

    /// Whether an amphibian in water uses its swimming locomotion. A drowned
    /// swims only toward a target that is in water, or while looking for land;
    /// otherwise it moves like any walker.
    fn swims_now(&self) -> bool {
        match self.shape.swim_rule {
            SwimRule::Drowned => self.searching_for_land || self.attack_target.is_some_and(|t| self.target_in_water(t)),
            _ => true,
        }
    }

    fn target_in_water(&self, target: Vec3) -> bool {
        self.world.is_water(target.x.floor() as i32, target.y.floor() as i32, target.z.floor() as i32)
    }

    /// One tick of an amphibian's swimming. The velocity gains thrust by the
    /// species' [`SwimRule`], moves the body, and keeps 0.9 of itself.
    fn amphibious_swim_step(&mut self, waypoint: Option<Vec3>) {
        const DRAG: f64 = 0.9;
        let speed = self.navigator.speed();
        let yaw_to = |from: Vec3, w: Vec3| movement_yaw(w.x - from.x, w.z - from.z);
        let mut accel = Vec3::new(0.0, 0.0, 0.0);
        let mut push_y = 0.0;
        match self.shape.swim_rule {
            SwimRule::Smooth { in_water, buoyant, .. } => {
                if buoyant {
                    push_y += 0.005;
                }
                if let Some(w) = waypoint {
                    let (xd, yd, zd) = (w.x - self.pos.x, w.y - self.pos.y, w.z - self.pos.z);
                    if xd * xd + yd * yd + zd * zd >= 2.500_000_3e-7 {
                        let turn = wrap_degrees(yaw_to(self.pos, w) - self.body_yaw).clamp(-10.0, 10.0);
                        self.body_yaw = wrap_degrees(self.body_yaw + turn);
                        let flat = xd.hypot(zd);
                        if yd.abs() > 1e-5 || flat > 1e-5 {
                            let wanted = wrap_degrees((-yd.atan2(flat).to_degrees()) as f32).clamp(-85.0, 85.0);
                            self.swim_pitch += (wanted - self.swim_pitch).clamp(-5.0, 5.0);
                        }
                        let pitch = f64::from(self.swim_pitch).to_radians();
                        let (up, forward) = (-pitch.sin() * speed, pitch.cos() * speed);
                        let length = up.hypot(forward);
                        let scale = speed * in_water / length.max(1.0);
                        let yaw = f64::from(self.body_yaw).to_radians();
                        accel = Vec3::new(-forward * scale * yaw.sin(), up * scale, forward * scale * yaw.cos());
                    }
                }
            }
            SwimRule::Turtle => {
                if let Some(w) = waypoint {
                    let (xd, yd, zd) = (w.x - self.pos.x, w.y - self.pos.y, w.z - self.pos.z);
                    let reach = (xd * xd + yd * yd + zd * zd).sqrt();
                    if reach >= 1e-5 {
                        let turn = wrap_degrees(yaw_to(self.pos, w) - self.body_yaw).clamp(-90.0, 90.0);
                        self.body_yaw = wrap_degrees(self.body_yaw + turn);
                        self.swim_speed += (speed - self.swim_speed) * 0.125;
                        push_y += self.swim_speed * (yd / reach) * 0.1;
                    }
                } else {
                    self.swim_speed = 0.0;
                }
                push_y += 0.005;
                let yaw = f64::from(self.body_yaw).to_radians();
                let thrust = 0.1 * self.swim_speed;
                accel = Vec3::new(-thrust * yaw.sin(), 0.0, thrust * yaw.cos());
                if self.attack_target.is_none() {
                    push_y -= 0.005;
                }
            }
            SwimRule::Drowned => {
                if self.searching_for_land || self.attack_target.is_some_and(|t| t.y > self.pos.y) {
                    push_y += 0.002;
                }
                if let Some(w) = waypoint {
                    let (xd, yd, zd) = (w.x - self.pos.x, w.y - self.pos.y, w.z - self.pos.z);
                    let reach = (xd * xd + yd * yd + zd * zd).sqrt();
                    if reach > 0.0 {
                        let turn = wrap_degrees(yaw_to(self.pos, w) - self.body_yaw).clamp(-90.0, 90.0);
                        self.body_yaw = wrap_degrees(self.body_yaw + turn);
                        self.swim_speed += (speed - self.swim_speed) * 0.125;
                        let yaw = f64::from(self.body_yaw).to_radians();
                        let forward = 0.01 * self.swim_speed;
                        accel = Vec3::new(
                            self.swim_speed * xd * 0.005 - forward * yaw.sin(),
                            0.0,
                            self.swim_speed * zd * 0.005 + forward * yaw.cos(),
                        );
                        push_y += self.swim_speed * (yd / reach) * 0.1;
                    }
                } else {
                    self.swim_speed = 0.0;
                }
            }
        }
        let v = self.fly_velocity;
        let v = Vec3::new(v.x + accel.x, v.y + accel.y + push_y, v.z + accel.z);
        self.pos.x += v.x;
        self.pos.y += v.y;
        self.pos.z += v.z;
        self.fly_velocity = Vec3::new(v.x * DRAG, v.y * DRAG, v.z * DRAG);
    }

    /// Whether a bat is hanging from a ceiling.
    #[must_use]
    pub fn is_resting(&self) -> bool {
        self.bat.resting
    }

    /// One tick of bat flight. A hanging bat wakes when the block above stops
    /// being a full cube or a player is within four blocks. A flying bat heads
    /// for a random block within six horizontally and between two below and
    /// three above, easing its velocity toward half a block per tick sideways
    /// and 0.7 vertically by a tenth of the difference, and now and then hangs
    /// from a full cube overhead. The move then applies 0.01 of forward thrust,
    /// gravity, and drags of 0.91 sideways and 0.98 vertically; vertical speed
    /// is cut to 0.6 each tick while flying, and a hanging bat is held still,
    /// flush with the top of its block.
    fn flutter_step(&mut self) {
        const THRUST: f64 = 0.02;
        const GRAVITY: f64 = 0.08;
        let (cx, cy, cz) = (self.pos.x.floor() as i32, self.pos.y.floor() as i32, self.pos.z.floor() as i32);
        let hangs = self.world.is_roost(cx, cy + 1, cz);
        if self.bat.resting {
            let near = self.nearest_player.is_some_and(|p| {
                (p.x - self.pos.x).powi(2) + (p.y - self.pos.y).powi(2) + (p.z - self.pos.z).powi(2) <= 16.0
            });
            if !hangs || near {
                self.bat.resting = false;
            }
        } else {
            self.flutter_steer();
            if MobController::next_i32(self, 100) == 0 && hangs {
                self.bat.resting = true;
            }
        }
        let yaw = f64::from(self.body_yaw).to_radians();
        let thrust = self.bat.forward * THRUST;
        let v = self.fly_velocity;
        let (vx, vz) = (v.x - thrust * yaw.sin(), v.z + thrust * yaw.cos());
        self.pos.x += vx;
        self.pos.y += v.y;
        self.pos.z += vz;
        self.fly_velocity = Vec3::new(vx * 0.91, (v.y - GRAVITY) * 0.98, vz * 0.91);
        if self.bat.resting {
            self.fly_velocity = Vec3::new(0.0, 0.0, 0.0);
            self.pos.y = self.pos.y.floor() + 1.0 - f64::from(self.shape.height);
        } else {
            self.fly_velocity.y *= 0.6;
        }
        self.bat.forward *= 0.98;
    }

    /// Picks and chases the bat's wander block.
    fn flutter_steer(&mut self) {
        let empty = |w: &dyn PathWorld, (x, y, z): (i32, i32, i32)| {
            w.collision_top(x, y, z) == 0.0 && w.base_path_type(x, y, z) == PathType::Open
        };
        if let Some(t) = self.bat.target
            && (!empty(self.world, t) || t.1 <= self.world.min_y())
        {
            self.bat.target = None;
        }
        let close = |t: (i32, i32, i32), p: Vec3| {
            (f64::from(t.0) + 0.5 - p.x).powi(2) + (f64::from(t.1) + 0.5 - p.y).powi(2) + (f64::from(t.2) + 0.5 - p.z).powi(2) < 4.0
        };
        let pos = self.pos;
        if self.bat.target.is_none_or(|t| MobController::next_i32(self, 30) == 0 || close(t, pos)) {
            let dx = MobController::next_i32(self, 7) - MobController::next_i32(self, 7);
            let dy = MobController::next_i32(self, 6) - 2;
            let dz = MobController::next_i32(self, 7) - MobController::next_i32(self, 7);
            self.bat.target = Some((
                (pos.x + f64::from(dx)).floor() as i32,
                (pos.y + f64::from(dy)).floor() as i32,
                (pos.z + f64::from(dz)).floor() as i32,
            ));
        }
        let Some((tx, ty, tz)) = self.bat.target else { return };
        let (dx, dy, dz) = (f64::from(tx) + 0.5 - pos.x, f64::from(ty) + 0.1 - pos.y, f64::from(tz) + 0.5 - pos.z);
        let sign = |d: f64| if d == 0.0 { 0.0 } else { d.signum() };
        let v = self.fly_velocity;
        let v = Vec3::new(
            v.x + (sign(dx) * 0.5 - v.x) * 0.1,
            v.y + (sign(dy) * 0.7 - v.y) * 0.1,
            v.z + (sign(dz) * 0.5 - v.z) * 0.1,
        );
        self.fly_velocity = v;
        self.body_yaw = movement_yaw(v.x, v.z);
        self.bat.forward = 0.5;
    }

    /// Sets the speed a floater accelerates at (the `flying_speed` attribute).
    pub fn set_flying_speed(&mut self, speed: f64) {
        self.flying_speed = speed;
    }

    /// One tick of floating. Every two to six ticks the mob accelerates toward
    /// its wanted position by five thirds of its flying speed, or gives the
    /// position up when the body could not get there; air keeps 0.91 of the
    /// velocity each tick.
    fn fly_step(&mut self) {
        const AIR_DRAG: f64 = 0.91;
        if let Some(wanted) = self.float_wanted {
            let ready = self.float_duration <= 0;
            self.float_duration -= 1;
            if ready {
                self.float_duration += MobController::next_i32(self, 5) + 2;
                let travel = Vec3::new(wanted.x - self.pos.x, wanted.y - self.pos.y, wanted.z - self.pos.z);
                if self.can_reach(travel) {
                    let length = travel.length();
                    if length > 0.0 {
                        let push = self.flying_speed * 5.0 / 3.0 / length;
                        let v = self.fly_velocity;
                        self.fly_velocity = Vec3::new(v.x + travel.x * push, v.y + travel.y * push, v.z + travel.z * push);
                    }
                } else {
                    self.float_wanted = None;
                }
            }
        }
        let v = self.fly_velocity;
        self.pos.x += v.x;
        self.pos.y += v.y;
        self.pos.z += v.z;
        self.fly_velocity = Vec3::new(v.x * AIR_DRAG, v.y * AIR_DRAG, v.z * AIR_DRAG);
    }

    /// Whether the body, swept along `travel` in half-block steps, stays clear
    /// of solid blocks.
    fn can_reach(&self, travel: Vec3) -> bool {
        let half = f64::from(self.shape.width) / 2.0;
        let height = f64::from(self.shape.height);
        let steps = (travel.length() * 2.0).ceil().max(1.0) as u32;
        (0..=steps).all(|i| {
            let f = f64::from(i) / f64::from(steps);
            let (x, y, z) = (self.pos.x + travel.x * f, self.pos.y + travel.y * f, self.pos.z + travel.z * f);
            !self.world.collides(Aabb::new(x - half, y, z - half, x + half, y + height, z + half))
        })
    }

    /// One tick of pulsed drifting. A pulse cycle runs a phase from 0 to a full
    /// turn at its own rate; in water the chosen velocity is applied during the
    /// stretch of the first half-turn past three quarters of it, the body
    /// glides at 0.9 of its velocity per tick through the rest, and out of water
    /// it only falls.
    fn drift_step(&mut self) {
        const TURN: f64 = std::f64::consts::TAU;
        const GLIDE: f64 = 0.9;
        const AIR_DRAG: f64 = 0.98;
        if self.pulse_rate == 0.0 {
            self.pulse_rate = 1.0 / (f64::from(MobController::next_f32(self)) + 1.0) * 0.2;
        }
        self.pulse += self.pulse_rate;
        if self.pulse > TURN {
            self.pulse -= TURN;
            if MobController::next_i32(self, 10) == 0 {
                self.pulse_rate = 1.0 / (f64::from(MobController::next_f32(self)) + 1.0) * 0.2;
            }
        }
        if self.in_water {
            if self.pulse < TURN / 2.0 {
                if self.pulse / (TURN / 2.0) > 0.75 {
                    self.drift_velocity = self.drift_vector;
                }
            } else {
                let v = self.drift_velocity;
                self.drift_velocity = Vec3::new(v.x * GLIDE, v.y * GLIDE, v.z * GLIDE);
            }
        } else {
            self.drift_velocity = Vec3::new(0.0, (self.drift_velocity.y - FALL_GRAVITY_PER_TICK) * AIR_DRAG, 0.0);
        }
        self.pos.x += self.drift_velocity.x;
        self.pos.y += self.drift_velocity.y;
        self.pos.z += self.drift_velocity.z;
    }

    /// One tick of swimming: the speed eases toward the path's, thrust is a
    /// hundredth of it along the horizontal heading while the vertical pull
    /// scales with the climb ratio; everything then keeps 0.9 of its velocity,
    /// and a mob with no target sinks slowly. Cruise is `0.1 * speed` blocks per
    /// tick.
    fn swim_step(&mut self, waypoint: Option<Vec3>) {
        const SPEED_EASING: f64 = 0.125;
        const THRUST: f64 = 0.01;
        const CLIMB: f64 = 0.1;
        const DRAG: f64 = 0.9;
        const BUOYANCY: f64 = 0.005;
        const AIR_DRAG: f64 = 0.91;
        if self.in_water {
            self.swim_vy += BUOYANCY;
        }
        let mut thrust = (0.0, 0.0);
        match waypoint {
            Some(w) => {
                let target = self.navigator.speed();
                self.swim_speed += (target - self.swim_speed) * SPEED_EASING;
                let (xd, yd, zd) = (w.x - self.pos.x, w.y - self.pos.y, w.z - self.pos.z);
                let reach = (xd * xd + yd * yd + zd * zd).sqrt();
                if yd != 0.0 {
                    self.swim_vy += self.swim_speed * (yd / reach) * CLIMB;
                }
                let flat = xd.hypot(zd);
                if flat > 0.0 {
                    let push = THRUST * self.swim_speed / flat;
                    thrust = (xd * push, zd * push);
                }
            }
            None => self.swim_speed = 0.0,
        }
        let floor = |v: f64| if v.abs() < DRIFT_FLOOR { 0.0 } else { v };
        self.drift = (floor(self.drift.0), floor(self.drift.1));
        self.swim_vy = floor(self.swim_vy);
        let step = (self.drift.0 + thrust.0, self.drift.1 + thrust.1);
        self.pos.x += step.0;
        self.pos.z += step.1;
        self.pos.y += self.swim_vy;
        if self.in_water {
            self.drift = (step.0 * DRAG, step.1 * DRAG);
            self.swim_vy *= DRAG;
            if self.attack_target.is_none() {
                self.swim_vy -= BUOYANCY;
            }
        } else {
            // Out of the water a swimmer falls like any other body.
            self.drift = (step.0 * AIR_DRAG, step.1 * AIR_DRAG);
            self.swim_vy = (self.swim_vy - FALL_GRAVITY_PER_TICK) * FALL_VERTICAL_AIR_DRAG;
        }
    }

    /// Whether the body is close enough to `waypoint` for its vertical move to
    /// begin: a jump starts within a block, a step or a drop only once the body
    /// is at (or past) the waypoint's centre line.
    fn reached_vertical_transition(
        &self,
        waypoint: Vec3,
        to_waypoint: (f64, f64),
        step: (f64, f64),
    ) -> bool {
        let horizontal_sqr = to_waypoint.0 * to_waypoint.0 + to_waypoint.1 * to_waypoint.1;
        let dy = waypoint.y - self.pos.y;
        if dy > f64::from(self.shape.max_up_step) {
            return horizontal_sqr < f64::from(self.shape.width).max(1.0);
        }
        let travelled = step.0.hypot(step.1);
        let reach = if dy < 0.0 {
            travelled.max((0.5 - f64::from(self.shape.width) / 2.0).max(0.0))
        } else {
            travelled
        };
        horizontal_sqr.sqrt() <= reach
    }

    /// What the body is moving through, for thrust and drag.
    fn medium(&self) -> locomotion::Medium {
        let (x, _, z) = self.feet_block();
        if self.in_water {
            locomotion::Medium::Water
        } else if self.in_lava {
            locomotion::Medium::Lava
        } else if self.fall_speed != 0.0 {
            locomotion::Medium::Air
        } else {
            locomotion::Medium::Ground { friction: self.world.footing(x, self.support_y(), z).friction }
        }
    }

    /// The block row whose slipperiness the body feels.
    fn support_y(&self) -> i32 {
        (self.pos.y - SUPPORT_PROBE).floor() as i32
    }

    /// The speed factor of the block the body stands in, else the one it
    /// stands on; water never slows.
    fn footing_speed_factor(&self) -> f32 {
        let (x, y, z) = self.feet_block();
        if self.in_water {
            return 1.0;
        }
        let here = self.world.footing(x, y, z).speed_factor;
        if here == 1.0 { self.world.footing(x, self.support_y(), z).speed_factor } else { here }
    }
}

impl NavigatingMob<'_> {
    /// Moves a ground destination onto the surface: a target in the air drops to
    /// the first block below it, and one inside a solid block rises out of it.
    /// A destination for a swimmer: the stroll search, repeated while the
    /// chosen cell is not water.
    fn swim_target(&mut self) -> Option<Vec3> {
        const RETRIES: u32 = 10;
        let mut target = self.stroll_search(None);
        for _ in 0..RETRIES {
            let Some(t) = target else { break };
            let (x, y, z) = (t.x.floor() as i32, t.y.floor() as i32, t.z.floor() as i32);
            if self.world.base_path_type(x, y, z) == PathType::Water {
                break;
            }
            target = self.stroll_search(None);
        }
        target
    }

    /// Ten random offsets, the first acceptable one winning (every candidate
    /// weighs the same). Without `away` the offset is uniform in a 21 by 15 by
    /// 21 box; with it, a point up to 16 out within a quarter turn of the
    /// direction away from `away`.
    fn stroll_search(&mut self, away: Option<Vec3>) -> Option<Vec3> {
        const ATTEMPTS: u32 = 10;
        const HORIZONTAL: i32 = 10;
        const FLEE_REACH: f64 = 16.0;
        const VERTICAL: i32 = 7;
        let origin = (self.pos.x.floor() as i32, self.pos.y.floor() as i32, self.pos.z.floor() as i32);
        for _ in 0..ATTEMPTS {
            let (dx, dy, dz) = match away {
                None => (
                    MobController::next_i32(self, 2 * HORIZONTAL + 1) - HORIZONTAL,
                    MobController::next_i32(self, 2 * VERTICAL + 1) - VERTICAL,
                    MobController::next_i32(self, 2 * HORIZONTAL + 1) - HORIZONTAL,
                ),
                Some(threat) => {
                    let (fx, fz) = (self.pos.x - threat.x, self.pos.z - threat.z);
                    let centre = fz.atan2(fx) - std::f64::consts::FRAC_PI_2;
                    let angle = centre
                        + (2.0 * f64::from(MobController::next_f32(self)) - 1.0) * std::f64::consts::FRAC_PI_2;
                    let reach = MobController::next_f64(self).sqrt() * FLEE_REACH * std::f64::consts::SQRT_2;
                    let (x, z) = (-reach * angle.sin(), reach * angle.cos());
                    if x.abs() > FLEE_REACH || z.abs() > FLEE_REACH {
                        continue;
                    }
                    (x.floor() as i32, MobController::next_i32(self, 2 * VERTICAL + 1) - VERTICAL, z.floor() as i32)
                }
            };
            if let Some(found) = self.stroll_candidate(origin.0 + dx, origin.1 + dy, origin.2 + dz) {
                return Some(found);
            }
        }
        None
    }

    /// Whether the cell is an acceptable stroll destination for this body, and
    /// where its bottom centre stands once lifted out of any solid block. A
    /// walker needs ground under it and no penalised or watery type; a swimmer
    /// needs open space and no penalty.
    fn stroll_candidate(&self, x: i32, mut y: i32, z: i32) -> Option<Vec3> {
        const MAX_LIFT: i32 = 64;
        if y < self.world.min_y() {
            return None;
        }
        let solid = |w: &dyn PathWorld, x: i32, y: i32, z: i32| w.collision_top(x, y, z) > 0.0;
        if self.shape.nav_mode == NavMode::Swim || (self.shape.nav_mode == NavMode::Amphibious && self.in_water) {
            if solid(self.world, x, y, z) || self.shape.malus(self.world.base_path_type(x, y, z)) != 0.0 {
                return None;
            }
        } else {
            if self.world.base_path_type(x, y - 1, z) == PathType::Open {
                return None;
            }
            let limit = y + MAX_LIFT;
            while solid(self.world, x, y, z) && y <= limit {
                y += 1;
            }
            let kind = self.world.base_path_type(x, y, z);
            if y > limit || kind == PathType::Water || self.shape.malus(kind) != 0.0 {
                return None;
            }
        }
        Some(Vec3::new(f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5))
    }

    fn surface_block(&self, block: BlockPos) -> BlockPos {
        if self.shape.nav_mode.is_volume()
            || (self.shape.nav_mode == NavMode::Amphibious && self.world.is_water(block.x, block.y, block.z))
        {
            return block;
        }
        const SEARCH_UP: i32 = 64;
        let world = self.world;
        let is_air = |y: i32| world.base_path_type(block.x, y, block.z) == PathType::Open;
        let is_solid = |y: i32| world.collision_top(block.x, y, block.z) > 0.0;
        let mut y = block.y;
        if is_air(y) {
            let mut below = y - 1;
            while below >= world.min_y() && is_air(below) {
                below -= 1;
            }
            if below >= world.min_y() {
                return BlockPos::new(block.x, below + 1, block.z);
            }
            let limit = y + SEARCH_UP;
            y += 1;
            while y <= limit && is_air(y) {
                y += 1;
            }
        }
        while is_solid(y) && y <= block.y + SEARCH_UP {
            y += 1;
        }
        BlockPos::new(block.x, y, block.z)
    }

    /// Paths to `target` and starts following, stopping within `reach` blocks
    /// (Manhattan) of it.
    pub(crate) fn move_to_within(&mut self, target: Vec3, speed: f64, reach: i32) -> bool {
        if self.shape.can_climb {
            self.climb_goal = Some((target, self.goal_speed(speed)));
        }
        let block = self.surface_block(BlockPos::new(
            target.x.floor() as i32,
            target.y.floor() as i32,
            target.z.floor() as i32,
        ));
        // Reuse the active path unless it finished or the goal now wants a
        // different destination block (vanilla `PathNavigation.moveTo` reuse).
        let same_target = self.active_target_block == Some(block);
        let recompute = self.navigator.is_done() || !same_target;
        if !recompute {
            let speed = self.goal_speed(speed);
            self.navigator.set_speed(speed);
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
            max_path_length: self.follow_range.max(MIN_PATH_LENGTH) as f32,
            reach_range: reach,
            visited_multiplier: 1.0,
        };
        match self
            .finder
            .find_path(self.world, &self.shape, start, &[block], params)
        {
            Some(path) => {
                let speed = self.goal_speed(speed);
                self.navigator.start(path, speed);
                self.move_calls += 1;
                true
            }
            None => false,
        }
    }
}

impl MobController for NavigatingMob<'_> {
    fn sea_level(&self) -> i32 {
        self.sea_level
    }

    fn set_searching_for_land(&mut self, searching: bool) {
        self.searching_for_land = searching;
    }

    fn swoop(&mut self) -> Option<&mut SwoopState> {
        self.anchor_at_spawn();
        (self.shape.nav_mode == NavMode::Swoop).then_some(&mut self.swoop)
    }

    fn motion_blocking_height(&self, x: i32, from_y: i32, z: i32) -> i32 {
        let blocks = |y: i32| self.world.collision_top(x, y, z) > 0.0 || self.world.is_water(x, y, z);
        let mut y = from_y;
        while y > self.world.min_y() && !blocks(y) {
            y -= 1;
        }
        y + 1
    }

    fn float_to(&mut self, target: Vec3) {
        self.float_wanted = Some(target);
    }

    fn float_wanted(&self) -> Option<Vec3> {
        self.float_wanted
    }

    fn bright_outside(&self) -> bool {
        self.sun.0
    }

    fn is_burning(&self) -> bool {
        self.sun.1
    }

    fn wears_helmet(&self) -> bool {
        self.sun.2
    }

    fn sees_sky_at(&self, at: Vec3) -> bool {
        self.world.sees_sky(at.x.floor() as i32, at.y.floor() as i32, at.z.floor() as i32)
    }

    fn drift_vector(&self) -> Vec3 {
        self.drift_vector
    }

    fn set_drift_vector(&mut self, vector: Vec3) {
        self.drift_vector = vector;
    }

    fn water_at(&self, at: Vec3) -> bool {
        self.world.is_water(at.x.floor() as i32, at.y.floor() as i32, at.z.floor() as i32)
    }

    fn air_at(&self, at: Vec3) -> bool {
        let (x, y, z) = (at.x.floor() as i32, at.y.floor() as i32, at.z.floor() as i32);
        self.world.base_path_type(x, y, z) == PathType::Open && !self.world.is_water(x, y, z)
    }

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

    /// Whether the mob's feet cell held water when the tick began, sensed from
    /// the [`PathWorld`] the tick runs against, so
    /// [`FloatGoal`](super::goals::FloatGoal) works for any world that
    /// classifies its blocks. Between ticks it keeps the last sensed value.
    ///
    /// **Scope cut, disclosed:** vanilla is
    /// `isInWater() && getFluidHeight(WATER) > getFluidJumpThreshold()`
    /// (`FloatGoal::canUse`), where `isInWater` is a bounding-box
    /// sweep (`Entity::isInWater`, `wasTouchingWater`) and the threshold is
    /// `getEyeHeight() < 0.4 ? 0.0 : 0.4` (`Entity::getFluidJumpThreshold`). This
    /// composition has no fluid-height model at all — `PathWorld` exposes
    /// per-cell classification and collision tops, not fluid levels — so the
    /// feet cell being water stands in for both halves. The practical
    /// difference is at the surface: vanilla stops floating once the mob's feet
    /// are in water shallower than 0.4, and this does not. Getting that right
    /// needs a fluid-level seam on `PathWorld`, which is a wider change than
    /// this perception fix.
    fn in_water(&self) -> bool {
        self.in_water
    }

    /// Whether the feet cell held lava when the tick began; vanilla's
    /// `Entity::isInLava` has no height threshold, so this side is faithful.
    fn in_lava(&self) -> bool {
        self.in_lava
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

    fn owner_hurt_by(&self) -> Option<Vec3> {
        self.owner_hurt_by
    }

    fn owner_hurt_target(&self) -> Option<Vec3> {
        self.owner_hurt_target
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
        self.move_to_within(target, speed, 1)
    }

    fn chase(&mut self, target: Vec3, speed: f64) -> bool {
        self.move_to_within(target, speed, 0)
    }

    fn tick_count(&self) -> u64 {
        self.tick_count
    }

    fn navigation_done(&self) -> bool {
        self.navigator.is_done()
    }

    fn stop_navigation(&mut self) {
        self.navigator.stop();
        self.active_target_block = None;
        self.climb_goal = None;
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

    fn main_hand_item(&self) -> Option<&str> {
        self.main_hand.as_deref()
    }

    /// Vanilla's own target-acquisition search for the player-targeting
    /// registration: its own nearest-player search,
    /// whose range cut
    /// is its own targeting-conditions test — a full 3-D
    /// `distanceToSqr` against `max(range * visibility, 2.0)`, with `range` =
    /// `TargetGoal::getFollowDistance` = the `FOLLOW_RANGE` attribute.
    ///
    /// **This used to return `self.attack_target` — the field the goal calling
    /// it exists to write.** `NearestAttackableTargetGoal::can_use`
    /// asks this and `start` writes the answer back, so the only production
    /// writers of `attack_target` were that goal and `HurtByTargetGoal`: the
    /// loop could not bootstrap and no mob ever attacked unprovoked. The data
    /// was already fed every tick and simply never read.
    ///
    /// Its doc on [`MobController`] says the host applies three filters. Where
    /// each one actually lives:
    ///
    /// * **Follow range — here.** See [`follow_range`](Self::follow_range).
    /// * **Hostility — in the roster, structurally.** A cow must not target the
    ///   player, and it cannot: this method is reached *only* from
    ///   [`NearestAttackableTargetGoal`](super::goals::NearestAttackableTargetGoal),
    ///   which is installed only for species whose vanilla table registers it
    ///   (`roster::goals_for`). No passive table has that row, so a cow never
    ///   owns the goal and never asks. Re-testing hostility here would need a
    ///   species name list — the very thing `mobs.rs`'s `is_hostile_species`
    ///   already got wrong for `drowned`, `cave_spider`, `zombie_villager` and
    ///   `parched` — to re-derive an answer the jar-cited table already holds.
    ///   `no_passive_species_can_acquire_a_target` gates it.
    /// * **Line of sight — a ray between the two eyes** through the same
    ///   collision shapes movement uses ([`PathWorld::has_line_of_sight`]), so
    ///   a mob does not acquire a player behind a wall, and a column that is not
    ///   loaded blocks the ray.
    fn find_nearest_target(&mut self) -> Option<Vec3> {
        let player = self.nearest_in_range()?;
        self.has_line_of_sight(player).then_some(player)
    }

    fn nearest_in_range(&mut self) -> Option<Vec3> {
        let player = self.nearest_player?;
        // `modifier` is `target.getVisibilityPercent(targeter)`, which is 1.0
        // for a plainly visible player and only shrinks for an invisible or
        // sneaking one — neither modelled at this seam. The `max(…, 2.0)` floor
        // is vanilla's own and applies regardless.
        let range = self.follow_range.max(MIN_TARGET_VISIBILITY_DISTANCE);
        (distance_sqr(self.pos, player) <= range * range).then_some(player)
    }

    fn has_line_of_sight(&self, target: Vec3) -> bool {
        let eyes = self.pos + Vec3::new(0.0, f64::from(self.shape.height) * MOB_EYE_FRACTION, 0.0);
        self.world.has_line_of_sight(eyes, target + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0))
    }

    fn follow_range(&self) -> f64 {
        self.follow_range
    }

    fn angry_target(&self) -> Option<Vec3> {
        self.angry_target
    }

    fn owner_position(&self) -> Option<Vec3> {
        self.owner
    }

    fn is_tame(&self) -> bool {
        self.tame
    }

    fn is_ordered_to_sit(&self) -> bool {
        self.ordered_to_sit
    }

    fn set_in_sitting_pose(&mut self, sitting: bool) {
        self.in_sitting_pose = sitting;
    }

    fn cat_sit_target(&self) -> Option<Vec3> {
        self.cat_sit_target
    }

    fn cat_bed_target(&self) -> Option<Vec3> {
        self.cat_bed_target
    }

    fn is_lying(&self) -> bool {
        self.lying
    }

    fn set_lying(&mut self, lying: bool) {
        self.lying = lying;
    }

    fn owner_sleep_ticks(&self) -> Option<u32> {
        self.owner_sleep_ticks
    }

    fn request_gift(&mut self) {
        self.gift_requested = true;
    }

    fn ticks_since_shoulder_dismount(&self) -> i32 {
        self.ticks_since_shoulder_dismount
    }

    fn request_shoulder_ride(&mut self) {
        self.shoulder_ride_requested = true;
    }

    fn is_patrolling(&self) -> bool {
        self.patrolling
    }

    fn is_patrol_leader(&self) -> bool {
        self.patrol_leader
    }

    fn patrol_target(&self) -> Option<Vec3> {
        self.patrol_target
    }

    fn set_patrol_target(&mut self, target: Option<Vec3>) {
        self.patrol_target = target;
    }

    fn patrol_group_target(&self) -> Option<Vec3> {
        self.patrol_group_target
    }

    fn is_being_stared_at(&self) -> bool {
        self.stared_at
    }

    /// Answered from the [`PathWorld`] this mob already borrows for
    /// pathfinding — the whole reason the block-cue seam needs no world handle on
    /// [`MobController`] and no per-tick block feed.
    fn block_cues_at_feet(&self) -> BlockCues {
        let p = self.block_position();
        self.world.block_cues(p.x, p.y, p.z)
    }

    fn block_cues_below(&self) -> BlockCues {
        let p = self.block_position();
        self.world.block_cues(p.x, p.y - 1, p.z)
    }

    fn ate(&mut self, what: EatenBlock) {
        self.eaten.push(what);
    }

    fn attack(&mut self, target: Vec3) {
        self.attacks.push(target);
    }

    fn teleport_to(&mut self, target: Vec3) {
        // Vanilla `Entity::teleportTo` rewrites position immediately;
        // the path is abandoned because a
        // stale route to the old location is worse than none. Position is
        // written directly — the same "one-shot instant" treatment
        // `apply_knockback` gives an impulse — and the next `advance()` (path
        // following) recomputes fresh from the new position, exactly as it
        // does after knockback.
        self.pos = target;
        self.live_collision_origin = target;
        self.needs_live_unembed = false;
        self.velocity = Vec3::new(0.0, 0.0, 0.0);
        self.drift = (0.0, 0.0);
        // Fully-qualified: `BrainMob` also declares `stop_navigation`.
        MobController::stop_navigation(self);
    }

    /// Vanilla `EnderMan::teleport(x, y, z)` + `LivingEntity::randomTeleport`'s
    /// landing search — see [`MobController::validate_teleport_landing`]'s own
    /// doc comment for why this lives here rather than inside
    /// [`teleport_to`](Self::teleport_to). Walks the `(target.x, target.z)`
    /// column down from `target.y`, same direction and same per-cell test
    /// [`ground_below`](Self::ground_below) already uses for unpathed gravity
    /// (a solid or fluid top), except a fluid landing is rejected outright
    /// here — vanilla's `!isWet` — rather than accepted as a resting surface.
    fn validate_teleport_landing(&self, target: Vec3) -> Option<Vec3> {
        let bx = target.x.floor() as i32;
        let bz = target.z.floor() as i32;
        let min_y = self.world.min_y();
        let mut by = target.y.floor() as i32;
        loop {
            if by <= min_y {
                return None;
            }
            let top = self.world.collision_top(bx, by, bz);
            if top > 0.0 {
                if self.world.is_water(bx, by, bz) {
                    return None;
                }
                let landing_y = f64::from(by) + top;
                // Vanilla's second-stage `level.noCollision(this) &&
                // !containsAnyLiquid(...)`: the mob's own footprint at the
                // landing spot must not overlap a block — e.g. the random
                // offset landed the feet on solid ground but the body inside
                // a wall or ceiling above it.
                let half_width = f64::from(self.shape.width) / 2.0;
                let height = f64::from(self.shape.height);
                let footprint = Aabb::new(
                    target.x - half_width,
                    landing_y,
                    target.z - half_width,
                    target.x + half_width,
                    landing_y + height,
                    target.z + half_width,
                );
                if self.world.collides(footprint) {
                    return None;
                }
                return Some(Vec3::new(target.x, landing_y, target.z));
            }
            by -= 1;
        }
    }

    fn damage_self(&mut self, amount: f32) {
        self.self_damage.push(amount);
    }

    fn launch_projectile(&mut self, launch: ProjectileLaunch) {
        self.launches.push(launch);
    }

    /// Ten random offsets in a 21 x 15 x 21 box, each kept only if it stands on
    /// something, is lifted out of any solid block, and is not water or a
    /// pathing-penalty cell; the first valid one wins (every candidate weighs
    /// the same), snapped to its cell's bottom centre.
    fn random_stroll_target(&mut self) -> Option<Vec3> {
        if self.shape.nav_mode == NavMode::Swim || (self.shape.nav_mode == NavMode::Amphibious && self.in_water) {
            return self.swim_target();
        }
        if self.shape.nav_mode == NavMode::Air {
            return self.air_wander_target();
        }
        self.stroll_search(None)
    }

    fn flee_target(&mut self, threat: Vec3) -> Option<Vec3> {
        let target = self.stroll_search(Some(threat))?;
        let farther = (target.x - threat.x).powi(2) + (target.y - threat.y).powi(2) + (target.z - threat.z).powi(2)
            >= (self.pos.x - threat.x).powi(2) + (self.pos.y - threat.y).powi(2) + (self.pos.z - threat.z).powi(2);
        farther.then_some(target)
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
        // `resetLove()` on both parents immediately.
        // The age cooldown (`setAge(PARENT_AGE_AFTER_BREEDING)`, the same
        // method) and the child itself are the host's job — this seam
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

    /// Yes — this is the one production type that can drive a brain.
    ///
    /// Every other implementor of [`MobController`] in this workspace is a test
    /// double, and each one inherits the `None` default. That is what forces a
    /// brain gate to be a *behavioural* gate over a real world: the composition
    /// simply does not run against a fake.
    fn brain_mob(&mut self) -> Option<&mut dyn BrainMob> {
        Some(self)
    }
}

/// The Brain-system half of the composition.
///
/// # Why this is the same struct and not a `BrainNavigatingMob`
///
/// The brief this closes asks for "a `BrainMob` implementation over the real
/// navigator/world (mirroring `NavigatingMob`)". Mirroring it would have meant a
/// second struct duplicating the pathfinder, the follower, the fluid
/// classification and the whole host-injection field set — and, worse, a second
/// thing for `MobSim` to know how to spawn and tick.
///
/// Vanilla does not do that either: `Mob` has **one** body carrying both
/// `goalSelector` and `brain`, and a `Villager` navigates with exactly the same
/// `PathNavigation` a `Zombie` does. So the faithful shape is one body
/// implementing both seams, which is what this is. Both traits are narrow views
/// of the same mob; the overlapping methods (`position`, `move_to`,
/// `navigation_done`, `look_at`, the RNG) resolve to the same state, so a brain
/// and a goal cannot disagree about where the mob is.
///
/// The consequence worth naming: a brain-driven mob gets the **real A\*** here,
/// not a stub. `RandomStroll` writes a `WALK_TARGET`, `MoveToTargetSink` hands it
/// to [`MobController::move_to`]'s pathfinder, and
/// [`advance`](NavigatingMob::advance) walks the resulting path. That whole chain
/// had never once executed before this impl existed.
impl BrainMob for NavigatingMob<'_> {
    fn next_i32(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        (self.rng.next_u64() % bound as u64) as i32
    }

    fn next_f32(&mut self) -> f32 {
        self.rng.next_unit() as f32
    }

    /// The follower's own monotonic tick counter, advanced once per
    /// [`advance`](NavigatingMob::advance).
    ///
    /// Vanilla's brain compares behaviour timeouts against `level.getGameTime()`,
    /// a world clock this crate has no access to. A per-mob counter is
    /// interchangeable for that purpose because **every** comparison a brain makes
    /// is a difference between two readings of this same clock — `end_timestamp =
    /// time + duration` in [`Leaf::try_start`](crate::brain::Leaf) and
    /// `time > end_timestamp` in `tick_or_stop`. It is the same reasoning
    /// [`angry_target`](MobController::angry_target) uses to keep the *deadline*
    /// on the host side: what matters is that the clock is monotonic and shared
    /// between the two readings, not that it agrees with the world.
    ///
    /// It starts at `0`, so the first tick's `time` is `1`. Behaviour timeouts are
    /// all positive spans, so nothing depends on the origin.
    fn game_time(&self) -> i64 {
        // Saturating rather than `as`: a wrap would make `time > end_timestamp`
        // read as "not timed out" forever and wedge every running behaviour. It
        // takes ~14.6 billion years of ticks to reach, but the cast is free.
        i64::try_from(self.tick_count).unwrap_or(i64::MAX)
    }

    fn position(&self) -> Vec3 {
        self.pos
    }

    fn in_water(&self) -> bool {
        // Fully-qualified: `MobController` declares an `in_water` too, and both
        // must answer from the same world read. Delegating rather than
        // re-deriving keeps the disclosed fluid-height scope cut in one place.
        MobController::in_water(self)
    }

    fn move_to(&mut self, target: Vec3, speed: f32) -> bool {
        MobController::move_to(self, target, self.goal_basis * f64::from(speed))
    }

    fn navigation_done(&self) -> bool {
        self.navigator.is_done()
    }

    /// Vanilla's `navigation.isStuck()`, which `MoveToTargetSink::stop` reads to
    /// decide whether to arm its retry cooldown. The goal system exposes the same
    /// state as [`NavigatingMob::is_stuck`]; leaving this at the trait's `false`
    /// default would have made a wedged brain mob re-search A\* on every tick it
    /// re-strolled.
    fn navigation_stuck(&self) -> bool {
        self.navigator.is_stuck()
    }

    fn stop_navigation(&mut self) {
        MobController::stop_navigation(self);
    }

    fn look_at(&mut self, target: Vec3) {
        self.last_look = Some(target);
    }

    /// The host-injected nearest player, unfiltered.
    ///
    /// The brain's own [`SetPlayerLookTarget`](crate::brain::SetPlayerLookTarget)
    /// applies the distance cut (its `max_dist`), exactly as
    /// `LookAtPlayerGoal` does on the goal side — so an over-reporting host feed
    /// is wasteful here, not wrong. Note this is deliberately **not**
    /// [`find_nearest_target`](MobController::find_nearest_target)'s
    /// `follow_range`-cut answer: that one is for acquiring something to attack,
    /// and a brain's `NEAREST_VISIBLE_PLAYER` memory is perception, not targeting.
    fn nearest_visible_player(&self) -> Option<Vec3> {
        self.nearest_player
    }

    /// Vanilla `LandRandomPos.getPos`: a random destination within `max_xz`
    /// horizontally.
    ///
    /// **Two disclosed cuts, both in the same direction as the goal system's
    /// existing [`random_stroll_target`](MobController::random_stroll_target),
    /// which this mirrors on purpose.**
    ///
    /// * `max_y` is unused. Vanilla samples a vertical offset and then calls
    ///   `PathfinderMob.getWalkTargetValue`/`isWalkable` to validate the result;
    ///   this follower snaps `pos.y` to whatever floor the path resolves to, so a
    ///   random vertical offset would only produce unreachable targets.
    /// * The position is **not** pre-validated as land. Vanilla's `getPos` retries
    ///   up to 10 times and returns `null` if none validate. Here an invalid pick
    ///   is caught one step later and cheaply: `MoveToTargetSink` calls `move_to`,
    ///   the real A\* search fails, and the sink erases `WALK_TARGET` so the next
    ///   tick strolls again. The observable difference is a wasted search, not a
    ///   mob walking into a wall — the follower can only ever walk a path A\*
    ///   returned.
    fn random_land_pos(&mut self, max_xz: i32, _max_y: i32) -> Option<Vec3> {
        if max_xz <= 0 {
            return None;
        }
        let span = f64::from(max_xz) * 2.0;
        let dx = (self.rng.next_unit() * span - f64::from(max_xz)).round();
        let dz = (self.rng.next_unit() * span - f64::from(max_xz)).round();
        Some(Vec3::new(self.pos.x + dx, self.pos.y, self.pos.z + dz))
    }

    /// Delegates to the same `last_hurt_by` field
    /// [`MobController::last_hurt_by`] reads — fully qualified for the same
    /// reason [`in_water`](Self::in_water) is: both traits declare it, so an
    /// unqualified call would be `E0034`.
    fn last_hurt_by(&self) -> Option<Vec3> {
        MobController::last_hurt_by(self)
    }

    /// Delegates to the same `angry_target` field
    /// [`MobController::angry_target`] reads — fully qualified for the same
    /// reason [`last_hurt_by`](Self::last_hurt_by) is: both traits declare
    /// it, so an unqualified call would be `E0034`.
    fn angry_target(&self) -> Option<Vec3> {
        MobController::angry_target(self)
    }

    fn nearby_entities(&self) -> Vec<crate::brain::NearbyBrainEntity> {
        self.nearby_entities.clone()
    }

    /// The host-injected world time-of-day — see
    /// [`day_time`](Self::day_time)'s own field doc.
    fn day_time(&self) -> i32 {
        self.day_time
    }

    /// The host-injected claimed job-site position — see
    /// [`job_site`](Self::job_site)'s own field doc.
    fn job_site(&self) -> Option<Vec3> {
        self.job_site
    }

    /// The host-injected claimed bed position — see [`home`](Self::home)'s
    /// own field doc.
    fn home(&self) -> Option<Vec3> {
        self.home
    }

    /// The host-injected claimed bell position — see
    /// [`meeting_point`](Self::meeting_point)'s own field doc.
    fn meeting_point(&self) -> Option<Vec3> {
        self.meeting_point
    }

    /// The host-injected nearest visible zombified piglin position — see
    /// [`nearest_visible_zombified`](Self::nearest_visible_zombified)'s own
    /// field doc.
    fn nearest_visible_zombified(&self) -> Option<Vec3> {
        self.nearest_visible_zombified
    }

    /// The host-injected nearest eligible tongue-attack prey position — see
    /// [`nearest_attackable_food`](Self::nearest_attackable_food)'s own
    /// field doc.
    fn nearest_attackable_food(&self) -> Option<Vec3> {
        self.nearest_attackable_food
    }

    /// The host-injected allay delivery target — see
    /// [`delivery_target`](Self::delivery_target)'s own field doc.
    fn delivery_target(&self) -> Option<Vec3> {
        self.delivery_target
    }

    /// The host-injected sniffer dig-search target — see
    /// [`sniffer_dig_target`](Self::sniffer_dig_target)'s own field doc.
    fn sniffer_dig_target(&self) -> Option<Vec3> {
        self.sniffer_dig_target
    }

    /// Delegates to the same `attacks` queue [`MobController::attack`] writes
    /// (fully qualified: both traits declare `attack`, so an unqualified call
    /// would be `E0034`, the same disambiguation [`last_hurt_by`](Self::last_hurt_by)
    /// needs) — see [`BrainMob::attack`]'s own doc for why a brain-driven hit
    /// and a goal-driven hit share one queue rather than two.
    fn attack(&mut self, target: Vec3) {
        MobController::attack(self, target);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::ai::goal::Goal;
    use crate::ai::mob::is_in_view_cone;
    use crate::ai::goals::{
        AvoidEntityGoal, FloatGoal, HurtByTargetGoal, LookAtPlayerGoal, MeleeAttackGoal, PanicGoal,
        RandomStrollGoal, SwellGoal, TemptGoal,
    };
    use crate::pathfinding::{Aabb, Path, PathType};

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
        let mut mob = NavigatingMob::new(world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 8000, 0);
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
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 8000, 0);
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
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 3000, 0);
        mob.set_attack_target(Some(target));

        // move_to yields a (partial) path, matching vanilla best-effort behaviour.
        // Disambiguated: `BrainMob` also defines `move_to`, with an `f32` speed.
        let found = crate::ai::mob::MobController::move_to(&mut mob, target, 0.25);
        assert!(
            found,
            "vanilla returns a partial path toward an unreachable target"
        );

        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(0.25, 2.0)));
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
            0,
        );
        mob.set_attack_target(Some(target));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(MeleeAttackGoal::new(0.25, 2.0)));

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
            0,
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

    // ---- Breeding / aging ---------------------------------------------------
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
        // The goal times 60 ticks as 30 goal ticks, and a goal ticks on game
        // ticks 1, 2 and then every even tick. Its 30th tick is therefore game
        // tick 2 * (30 - 1) = 58: bred must be false through tick 57 and
        // true from tick 58, on both mobs simultaneously.
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut a = NavigatingMob::new(&world, shape.clone(), Vec3::new(0.0, 0.0, 0.0), 1.0, 400, 0);
        let mut b = NavigatingMob::new(&world, shape, Vec3::new(2.0, 0.0, 0.0), 1.0, 400, 0);
        a.set_in_love();
        b.set_in_love();

        let mut ai_a = GoalSelector::new();
        ai_a.add(0, Box::new(BreedGoal::new(1.0)));
        let mut ai_b = GoalSelector::new();
        ai_b.add(0, Box::new(BreedGoal::new(1.0)));

        for tick in 1..=58 {
            refresh_partner_candidates(&mut a, &mut b);
            a.tick(&mut ai_a);
            b.tick(&mut ai_b);
            if tick < 58 {
                assert!(
                    !a.take_bred() && !b.take_bred(),
                    "bred before the predicted tick 58 (at tick {tick})"
                );
            } else {
                assert!(
                    a.take_bred(),
                    "mob a must breed on the predicted tick (58)"
                );
                assert!(
                    b.take_bred(),
                    "mob b must breed on the predicted tick (58)"
                );
            }
        }
        // Vanilla resets love on both parents immediately
        // (`Animal::finalizeSpawnChildFromBreeding`) — proven through the seam,
        // not asserted by calling `breed()` again.
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
        let mut a = NavigatingMob::new(&world, shape.clone(), Vec3::new(0.0, 0.0, 0.0), 1.0, 400, 0);
        let mut b = NavigatingMob::new(&world, shape, Vec3::new(2.0, 0.0, 0.0), 1.0, 400, 0);
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
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 1.0, 400, 0);
        mob.set_in_love();
        assert_eq!(mob.love_time(), LOVE_TICKS);
        for _ in 0..LOVE_TICKS {
            mob.advance();
        }
        assert_eq!(mob.love_time(), 0, "love mode must expire after exactly LOVE_TICKS");
        assert!(!mob.is_in_love());

        // A baby's age counts up from BABY_START_AGE to 0 at one tick per
        // tick (`AgeableMob::aiStep`), so growing up takes exactly
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
        let mut control = NavigatingMob::new(&world, shape.clone(), Vec3::new(0.0, 0.0, 0.0), 1.0, 400, 0);
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
        let mut locked = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 1.0, 400, 0);
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
        let mut baby = NavigatingMob::new(&world, shape, baby_start, 0.25, 8000, 0);
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

    // ---- Knockback ----------------------------------------------------------

    #[test]
    fn knockback_decays_geometrically_under_ground_friction() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let start = Vec3::new(5.0, 0.0, 5.0);
        let mut mob = NavigatingMob::new(&world, MobShape::land(0.6, 1.95), start, 0.25, 400, 0);
        mob.apply_knockback(Vec3::new(-0.6, 0.0, 0.2));

        // Stone retains 0.6 * 0.91 of the velocity each tick.
        mob.advance();
        assert!((mob.velocity().x - -0.6).abs() < 1e-12);
        mob.advance();
        assert!((mob.velocity().x - -0.6 * 0.546).abs() < 1e-6);
        assert!((mob.velocity().z - 0.2 * 0.546).abs() < 1e-6);

        for _ in 0..60 {
            mob.advance();
        }
        // The geometric series 1 / (1 - 0.546), less the sub-0.003 tail the
        // velocity is cut to zero at.
        let travelled = mob.position().x - start.x;
        assert!((travelled - -0.6 / (1.0 - 0.546)).abs() < 0.01, "travelled {travelled}");
        assert_eq!(mob.velocity(), Vec3::new(0.0, 0.0, 0.0), "at rest after the tail");
    }

    // ---- Vertical motion: step-up bound and gravity-accelerated fall -------
    //
    // `step_vertical` is exercised directly rather than through the full
    // path/goal machinery: the timing of exactly which tick first sees a
    // raised or lowered waypoint depends on navigator internals this module
    // does not expose, so driving the pure function is what makes the
    // expected values exact rather than "eventually converges".

    /// Two arms, because one alone cannot separate "bounded" from
    /// "unbounded" (both pass a rise that already fits) or from "zero" (both
    /// fail a rise that needs any step at all).
    #[test]
    fn a_rise_within_max_up_step_resolves_in_one_call_and_a_larger_one_does_not() {
        let mut fall_speed = 0.0;

        // A 0.5-block slab: within the default 0.6 step height, so vanilla's
        // auto-step (and this follower) resolves it in one call.
        let after_slab = NavigatingMob::step_vertical(0.0, 0.5, 0.6, &mut fall_speed);
        assert_eq!(
            after_slab, 0.5,
            "a rise at or under max_up_step must resolve in a single call"
        );
        assert_eq!(fall_speed, 0.0, "resolving a rise must not leave a fall speed");

        // A full block: over the step height, so it must NOT glide straight
        // to 1.0 in one call — the control this repo's evidence standards
        // require, proving the bound actually fires rather than merely
        // existing in the source. It also must not be a bare
        // `max_up_step`-sized ratchet step (the disclosed simplification
        // this replaced): the first call's displacement is the real launch
        // speed itself, `JUMP_POWER` (`0.42`), not `max_up_step` (`0.6`).
        let mut fall_speed = 0.0;
        let after_block = NavigatingMob::step_vertical(0.0, 1.0, 0.6, &mut fall_speed);
        assert!(
            after_block < 1.0,
            "a rise beyond max_up_step must not resolve in one call (an \
             unbounded step is exactly the reported 'glides up immediately' \
             bug), got {after_block}"
        );
        assert_eq!(
            after_block, JUMP_POWER,
            "the first call of a jump must move by exactly the launch speed \
             (this integration order applies the stored speed before this \
             tick's own gravity/drag), not `max_up_step` or any other value"
        );
        assert!(
            fall_speed < 0.0,
            "a jump in progress must be recorded as an ascending (negative) \
             speed, not left at 0.0 as a completed step would be"
        );
    }

    /// Control proving the bound is load-bearing: with `max_up_step` at
    /// `f64::INFINITY` (the pre-fix behaviour), the same full-block rise DOES
    /// glide straight to the waypoint in one call — so the assertion above
    /// that it does not is a real property of the bound, not a coincidence of
    /// the chosen numbers.
    #[test]
    fn removing_the_step_bound_reproduces_the_glide_bug() {
        let mut fall_speed = 0.0;
        let after = NavigatingMob::step_vertical(0.0, 1.0, f64::INFINITY, &mut fall_speed);
        assert_eq!(
            after, 1.0,
            "control: an unbounded step must reproduce the instant glide, or \
             the subject test above proves nothing about the bound"
        );
    }

    /// **Predicts the exact jump peak from vanilla's own launch speed,
    /// gravity and drag constants, re-derived independently of
    /// `step_vertical`'s implementation** — the magnitude species of vacuous
    /// test this repo's evidence standards warn about would settle for "rises
    /// above 0.6"; this asserts the real number, and that it clears a full
    /// block (`docs/mob-vertical-motion.md`'s own measured ≈1.252).
    #[test]
    fn a_jump_over_a_full_block_reaches_the_real_vanilla_peak_height() {
        let mut pos_y = 0.0f64;
        let mut fall_speed = 0.0f64;
        let mut peak = 0.0f64;
        for _ in 0..40 {
            pos_y = NavigatingMob::step_vertical(pos_y, 1.0, 0.6, &mut fall_speed);
            peak = peak.max(pos_y);
            if fall_speed == 0.0 && pos_y >= 1.0 {
                break;
            }
        }
        // Independently re-derived: move by the stored (pre-update) speed,
        // then update it with gravity/drag for the next tick — the same
        // order `step_vertical`'s jump case uses, computed in a separate
        // loop so a shared bug cannot cancel out. Only the *ascending* phase
        // matters for a peak, so this stops the tick velocity first turns
        // non-negative (the arc's own apex) rather than modelling landing at
        // all.
        let mut y = 0.0f64;
        let mut v = -JUMP_POWER;
        let mut expected_peak = 0.0f64;
        loop {
            let displacement = v;
            v = (v + FALL_GRAVITY_PER_TICK) * FALL_VERTICAL_AIR_DRAG;
            y -= displacement;
            expected_peak = expected_peak.max(y);
            if v >= 0.0 {
                break;
            }
        }
        assert!(
            (peak - expected_peak).abs() < 1.0e-9,
            "expected peak {expected_peak} from JUMP_POWER/gravity/drag, got {peak}"
        );
        assert!(
            peak > 1.0,
            "a jump over a 1-block rise must clear it (real vanilla mobs can \
             hop a full block), got peak {peak}"
        );
        assert_eq!(pos_y, 1.0, "the arc must land exactly on the waypoint");
        assert_eq!(fall_speed, 0.0, "landing must reset the stored speed");
    }

    /// A jump already ascending must not be re-triggered by a second call
    /// that still sees `dy > max_up_step` — vanilla's own `MoveControl` stays
    /// in its `JUMPING` operation (ignoring new jump requests) until the mob
    /// is back on the ground, and re-seeding `-JUMP_POWER` mid-arc would
    /// silently cancel the ascent already under way.
    #[test]
    fn a_jump_already_in_progress_is_not_re_triggered() {
        let mut fall_speed = 0.0;
        let first = NavigatingMob::step_vertical(0.0, 1.0, 0.6, &mut fall_speed);
        let speed_after_first = fall_speed;
        // Still well short of the waypoint (`dy > max_up_step` again), but a
        // re-trigger would reset `fall_speed` to `-JUMP_POWER` outright,
        // which is a strictly *larger* ascending speed than the naturally
        // decayed one already in flight.
        let second = NavigatingMob::step_vertical(first, 1.0, 0.6, &mut fall_speed);
        assert!(
            second > first,
            "the ascent must keep rising on the second call, got {first} then {second}"
        );
        assert!(
            fall_speed > speed_after_first,
            "the stored speed must only ever decay toward 0 under gravity \
             (never jump back to a more-negative value), got {speed_after_first} \
             then {fall_speed}"
        );
    }

    /// The descent half of a jump arc, isolated: once past its peak, it must
    /// land exactly on the waypoint and reset the stored speed — the same
    /// landing contract an ordinary fall already has, proven here for the
    /// jump-then-fall handoff specifically rather than assumed from the fall
    /// tests alone.
    #[test]
    fn a_jump_lands_exactly_on_the_waypoint_after_its_arc() {
        let mut pos_y = 0.0f64;
        let mut fall_speed = 0.0f64;
        for _ in 0..40 {
            pos_y = NavigatingMob::step_vertical(pos_y, 1.0, 0.6, &mut fall_speed);
            if fall_speed == 0.0 && pos_y >= 1.0 {
                break;
            }
        }
        assert_eq!(pos_y, 1.0, "a completed jump must land exactly on the waypoint");
        assert_eq!(fall_speed, 0.0, "landing must reset the stored speed to 0.0");
    }

    /// Predicts the exact y after each tick of a fast fall from vanilla's own
    /// gravity/drag constants, computed independently of `step_vertical`'s
    /// implementation — not "y decreased" (the *magnitude* species of
    /// vacuous test this repo's evidence standards name explicitly).
    #[test]
    fn a_fall_accelerates_under_gravity_instead_of_snapping_to_the_landing_height() {
        let waypoint_y = -20.0; // far enough below that the fall never lands early
        let mut pos_y = 0.0f64;
        let mut fall_speed = 0.0f64;
        for _ in 1..=5 {
            pos_y = NavigatingMob::step_vertical(pos_y, waypoint_y, 0.6, &mut fall_speed);
        }
        // Independently re-derived expectation from vanilla's own constants —
        // a separate loop, not a refactor of `step_vertical`'s body, so a bug
        // shared between the test and the implementation cannot cancel out.
        let mut y = 0.0f64;
        let mut v = 0.0f64;
        for _ in 0..5 {
            let d = v + FALL_GRAVITY_PER_TICK;
            v = d * FALL_VERTICAL_AIR_DRAG;
            y -= d;
        }
        assert!(
            (pos_y - y).abs() < 1.0e-12,
            "after 5 ticks of unobstructed fall, expected y={y} from vanilla's \
             gravity/drag constants, got {pos_y}"
        );
        // The discriminating check: an instant-snap implementation would have
        // reached the landing height (or at least moved much farther) on the
        // very first tick; a gravity-accelerated one barely moves at first.
        let mut first_tick_fall_speed = 0.0f64;
        let first_tick_y = NavigatingMob::step_vertical(0.0, waypoint_y, 0.6, &mut first_tick_fall_speed);
        assert!(
            first_tick_y > -1.0,
            "the first tick of a fall must move well under a block (gravity \
             starts at ~0.08/tick), not snap toward the landing height; got \
             {first_tick_y}"
        );
    }

    /// The mob must land exactly on the waypoint's floor, never overshoot past
    /// it even once the accelerating fall speed exceeds the remaining
    /// distance, and the stored fall speed must reset once landed so the next
    /// climb or fall starts clean rather than inheriting terminal velocity.
    #[test]
    fn a_fall_lands_exactly_on_the_surface_and_resets_afterwards() {
        let waypoint_y = -2.0;
        let mut pos_y = 0.0f64;
        let mut fall_speed = 0.0f64;
        for _ in 0..200 {
            pos_y = NavigatingMob::step_vertical(pos_y, waypoint_y, 0.6, &mut fall_speed);
            if pos_y <= waypoint_y {
                break;
            }
        }
        assert_eq!(pos_y, waypoint_y, "the fall must land exactly on the floor, never past it");
        assert_eq!(fall_speed, 0.0, "landing must reset the stored fall speed");

        // A subsequent small rise from the landed position must resolve in
        // one call, exactly as if the mob had never fallen — proving the
        // reset actually took effect rather than merely being asserted above.
        let after_step = NavigatingMob::step_vertical(pos_y, waypoint_y + 0.5, 0.6, &mut fall_speed);
        assert_eq!(after_step, waypoint_y + 0.5);
    }

    fn walk_one_block(rise: i32, speed: f64) -> NavigatingMob<'static> {
        let world: &'static Arena = Box::leak(Box::new(Arena { walls: HashSet::new() }));
        let mut mob = NavigatingMob::new(
            world,
            MobShape::land(0.6, 1.95),
            Vec3::new(0.5, if rise < 0 { 1.0 } else { 0.0 }, 0.5),
            0.25,
            400,
            0,
        );
        let from_y = mob.position().y as i32;
        mob.navigator.start(
            Path::new(
                vec![
                    crate::pathfinding::PathNode { x: 0, y: from_y, z: 0, kind: PathType::Walkable },
                    crate::pathfinding::PathNode { x: 1, y: from_y + rise, z: 0, kind: PathType::Walkable },
                ],
                BlockPos::new(1, from_y + rise, 0),
                true,
            ),
            speed,
        );
        mob
    }

    #[test]
    fn a_drop_keeps_the_body_level_until_it_has_cleared_the_edge() {
        let mut mob = walk_one_block(-1, 0.25);
        let mut x_before_fall = None;
        for _ in 0..60 {
            let x = mob.position().x;
            mob.advance();
            if mob.position().y < 1.0 {
                x_before_fall = Some(x);
                break;
            }
        }
        // The body (width 0.6) is clear of the edge once its centre is within
        // 0.5 - 0.3 of the lower cell's centre line at x = 1.5.
        let x = x_before_fall.expect("it eventually drops");
        assert!(x >= 1.5 - 0.2 - 1e-9, "dropped early at x={x}");
        assert!((mob.position().y - 0.92).abs() < 1e-12, "first fall tick is one gravity step");
    }

    #[test]
    fn navigation_speed_sets_the_thrust_as_its_square() {
        // The first tick from rest moves exactly `speed^2` on stone.
        for speed in [0.1_f64, 0.2, 0.4] {
            let mut mob = walk_one_block(0, speed);
            mob.advance();
            let moved = mob.position().x - 0.5;
            assert!((moved - speed * speed).abs() < 1e-7, "speed {speed}: moved {moved}");
        }
    }

    #[test]
    fn a_one_block_rise_jumps_once_the_waypoint_is_within_a_block() {
        let mut mob = walk_one_block(1, 0.25);
        // The waypoint starts exactly one block away: still outside.
        mob.advance();
        assert_eq!(mob.position().y, 0.0);
        assert!(mob.fall_speed == 0.0);
        // After the first 0.25^2 of thrust it is 0.9375 away, inside.
        mob.advance();
        assert!(mob.fall_speed < 0.0, "the jump starts inside one block");
        assert!((mob.position().y - 0.42).abs() < 1e-12);
    }

    // ---- Gaze / teleport / self-damage / ownership primitives ---------------

    #[test]
    fn is_in_view_cone_implements_vanillas_exact_tolerance() {
        // Hand-computed from `LivingEntity::isLookingAtMe`: accept iff
        // `look · dir > 1.0 - coneSize / (adjustForDistance ? dist : 1.0)`.
        // Every case below is a closed-form dot and distance, so the expected
        // verdict is exact rather than a re-derivation of the code.
        let eye = Vec3::new(0.0, 0.0, 0.0);
        let look = Vec3::new(1.0, 0.0, 0.0);

        // On-axis: dot 1.0, tolerance 0.025/10 = 0.0025 → 1.0 > 0.9975.
        assert!(is_in_view_cone(eye, look, Vec3::new(10.0, 0.0, 0.0), 0.025, true));
        // 90° off: dot 0.0 → 0.0 > 0.9975 is false.
        assert!(!is_in_view_cone(eye, look, Vec3::new(0.0, 0.0, 10.0), 0.025, true));
        // Behind: dot −1.0 → false.
        assert!(!is_in_view_cone(eye, look, Vec3::new(-10.0, 0.0, 0.0), 0.025, true));

        // The load-bearing pair: the tolerance is **divided by distance**, so
        // the *required precision increases* with range — the cone narrows,
        // it does not widen. A 3-4-5 triangle gives dot 0.6 at both distances;
        // cone 1.0 makes the thresholds 0.5 (dist 2) and 0.8 (dist 5). The
        // same angular offset is a stare close up and not far away — a
        // fixed-angle cone would answer identically at both, so this pair is
        // what distinguishes the distance-scaled model from an approximation.
        assert!(is_in_view_cone(eye, look, Vec3::new(1.2, 0.0, 1.6), 1.0, true));
        assert!(!is_in_view_cone(eye, look, Vec3::new(3.0, 0.0, 4.0), 1.0, true));

        // Same 3-4-5 point with adjustForDistance=false: the tolerance is the
        // plain coneSize (1.0 → threshold 0.0), so even 53° off is accepted.
        assert!(is_in_view_cone(eye, look, Vec3::new(3.0, 0.0, 4.0), 1.0, false));

        // Degenerate: a viewer standing exactly on the target is not a stare
        // (documented divergence from vanilla's divide-by-zero `true`).
        assert!(!is_in_view_cone(eye, look, eye, 1.0, true));
    }

    /// The boundary at the enderman's own parameters (`coneSize` `0.025`,
    /// `adjustForDistance` `true` — `EnderMan::isBeingStaredBy`), re-derived from the
    /// formula itself rather than guessed: at 10 blocks the threshold is
    /// `1.0 - 0.025 / 10.0 = 0.9975` exactly, so a dot of `0.998` is just
    /// inside the cone and `0.997` is just outside it.
    ///
    /// Both points sit on the same 10-length vector (`adjacent² + opposite² =
    /// 100`), so this is the same closed-form-triangle construction as the
    /// pair above, at the constant the enderman actually uses instead of a
    /// round `1.0`.
    #[test]
    fn is_in_view_cone_boundary_at_the_endermans_own_cone_size() {
        let eye = Vec3::new(0.0, 0.0, 0.0);
        let look = Vec3::new(0.0, 0.0, 1.0);
        const CONE_SIZE: f64 = 0.025; // EnderMan::isBeingStaredBy
        const DIST: f64 = 10.0;
        // threshold = 1.0 - CONE_SIZE / DIST = 0.9975

        // adjacent 9.98 of a 10-length vector: dot = 0.998 > 0.9975.
        let inside = Vec3::new((DIST * DIST - 9.98 * 9.98).sqrt(), 0.0, 9.98);
        assert!(
            is_in_view_cone(eye, look, inside, CONE_SIZE, true),
            "dot 0.998 must clear the derived threshold 0.9975"
        );

        // adjacent 9.97: dot = 0.997 < 0.9975.
        let outside = Vec3::new((DIST * DIST - 9.97 * 9.97).sqrt(), 0.0, 9.97);
        assert!(
            !is_in_view_cone(eye, look, outside, CONE_SIZE, true),
            "dot 0.997 must fall short of the derived threshold 0.9975"
        );

        // A naive fixed-angle port — one that reads `coneSize` as the
        // tolerance directly, never dividing by distance, which is exactly
        // the approximation this function's own doc comment warns against —
        // would use threshold `1.0 - 0.025 = 0.975` regardless of range. Both
        // chosen points clear that: 0.998 and 0.997 both exceed 0.975, so a
        // naive implementation cannot tell them apart and would call both a
        // stare. Only the distance-divided threshold (0.9975 at 10 blocks)
        // separates them, which is the discriminating property this pair
        // exists to demonstrate.
        let naive_threshold = 1.0 - CONE_SIZE;
        let naive_dot = |target: Vec3| look.dot((target - eye).normalize());
        assert!(naive_dot(inside) > naive_threshold);
        assert!(
            naive_dot(outside) > naive_threshold,
            "a naive fixed-coneSize threshold accepts the 'outside' point too \
             (dot {} > {naive_threshold}), which is exactly why it cannot \
             reject what the real, distance-adjusted test rejects",
            naive_dot(outside)
        );
    }

    #[test]
    fn teleport_to_relocates_instantly_and_abandons_the_active_path() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.5, 0.0, 0.5), 0.25, 8000, 0);
        // Establish a real path first, then teleport away from it.
        assert!(
            crate::ai::mob::MobController::move_to(&mut mob, Vec3::new(10.5, 0.0, 0.5), 1.0),
            "a reachable target must yield a path"
        );
        assert!(mob.has_path());

        let target = Vec3::new(-40.0, 0.0, -40.0);
        mob.teleport_to(target);

        assert_eq!(
            mob.position(),
            target,
            "teleport must rewrite position exactly, not walk there"
        );
        assert!(
            !mob.has_path(),
            "teleport must abandon any in-progress path to the old destination"
        );
        assert_eq!(
            mob.velocity(),
            Vec3::new(0.0, 0.0, 0.0),
            "teleport must zero velocity (vanilla Entity.teleportTo)"
        );
    }

    #[test]
    fn damage_self_records_intents_that_the_host_drains() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);
        mob.damage_self(5.0);
        mob.damage_self(3.0);
        assert_eq!(
            mob.take_self_damage(),
            vec![5.0, 3.0],
            "each damage_self request must be recorded in order"
        );
        assert!(
            mob.take_self_damage().is_empty(),
            "the drain must be one-shot, like take_new_attacks"
        );
    }

    #[test]
    fn stared_at_is_host_injected_and_read_back() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);
        assert!(
            !mob.is_being_stared_at(),
            "a mob nobody has fed must read not-stared-at"
        );
        mob.set_stared_at(true);
        assert!(mob.is_being_stared_at(), "the host-fed boolean must read back");
        mob.set_stared_at(false);
        assert!(!mob.is_being_stared_at(), "it must also clear");
    }

    #[test]
    fn owner_is_host_injected_and_read_back() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);
        assert_eq!(mob.owner_position(), None, "a wild mob has no owner");
        let owner = Vec3::new(4.0, 0.0, 4.0);
        mob.set_owner(Some(owner));
        assert_eq!(mob.owner_position(), Some(owner));
        mob.set_owner(None);
        assert_eq!(mob.owner_position(), None, "ownership must be clearable");
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
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);
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
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);

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
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);
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
    fn swell_goal_holds_the_fuse_down_when_a_wall_hides_a_close_target() {
        // A target 2 blocks away behind a full-height wall is within the start
        // range but out of sight, so the fuse never rises. Control: the same
        // geometry with the wall removed detonates (the test above).
        let walls: HashSet<_> = (0..=2).map(|y| (1, y, 0)).collect();
        let world = Arena { walls };
        let shape = MobShape::land(0.6, 1.95);
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);
        mob.set_attack_target(Some(Vec3::new(2.0, 0.0, 0.0)));
        let mut ai = GoalSelector::new();
        ai.add(0, Box::new(SwellGoal::new()));
        for _ in 0..MAX_SWELL {
            mob.tick(&mut ai);
            assert_eq!(mob.swell(), 0, "the fuse must not rise without sight");
        }
        assert!(!mob.take_detonated());
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
        let mut mob = NavigatingMob::new(&world, shape, Vec3::new(0.0, 0.0, 0.0), 0.25, 400, 0);
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
    // Perception seam.
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
        // `PathType::Water` match (`PathWorld::is_water`), which is
        // what `in_water` calls through, so leaving it default keeps this
        // fixture from being able to fake the answer.
    }

    fn perception_mob<'w>(world: &'w dyn PathWorld, at: Vec3) -> NavigatingMob<'w> {
        let mut mob = NavigatingMob::new(world, MobShape::land(0.6, 1.95), at, 0.25, 400, 0);
        mob.sense_fluids();
        mob
    }

    /// Spawn equipment supplies a validated registry item, whereas a later
    /// interaction may supply an extension item as text. The AI deliberately
    /// compares only its path, so both inputs must reach the same reader in
    /// their respective forms without making the spawn producer untyped.
    #[test]
    fn main_hand_accepts_typed_spawn_items_and_dynamic_names_at_the_boundary() {
        let world = Arena { walls: HashSet::new() };
        let mut mob = perception_mob(&world, Vec3::new(0.0, 0.0, 0.0));
        let trident = Item::from_name("trident").expect("built-in spawn item");

        mob.set_main_hand_item(Some(trident));
        assert_eq!(mob.main_hand_item(), Some("trident"));

        mob.set_main_hand_item(Some(String::from("custom_spear")));
        assert_eq!(mob.main_hand_item(), Some("custom_spear"));
    }

    /// Whether each of the six previously-dead goals reports `can_use` for the
    /// mob as currently configured, in a fixed order:
    /// `[float, look_at_player, hurt_by_target, tempt, avoid_entity, panic]`.
    ///
    /// `LookAtPlayerGoal` is built with `probability == 1.0` so its
    /// `next_f32() >= probability` pre-roll (this crate's
    /// `LookAtPlayerGoal::can_use`, vanilla's `0.02F`
    /// default at `LookAtPlayerGoal::DEFAULT_PROBABILITY`) cannot make this test
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
        // all — vanilla `FloatGoal::canUse`.
        let world = FluidArena::with((0, 0, 0), PathType::Water);
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));

        // Each of the remaining five, at a distance vanilla would accept:
        //  * player 3 blocks away, inside `LookAtPlayerGoal`'s 8.0
        //    (`Creeper::registerGoals`'s `LookAtPlayerGoal(Player, 8.0F)`);
        //  * attacker 2 blocks away — `HurtByTargetGoal` has no range gate of
        //    its own (`HurtByTargetGoal::canUse` tests only
        //    the timestamp and non-null attacker);
        //  * tempter 4 blocks away, inside `Attributes::TEMPT_RANGE`'s default
        //    `10.0`;
        //  * threat 3 blocks away, inside the `6.0F` every vanilla
        //    `AvoidEntityGoal` registration uses (`Creeper::registerGoals`,
        //    `AbstractSkeleton::registerGoals`,
        //    `Spider::registerGoals`).
        mob.set_nearest_player(Some(Vec3::new(3.5, 0.0, 0.5)))
            .set_temptation(Some(Vec3::new(4.5, 0.0, 0.5)))
            .set_avoid_threat(Some(Vec3::new(-2.5, 0.0, 0.5)))
            // One hit records both the retaliation target and the panic
            // window, exactly as vanilla's single `hurtServer` call writes both
            // records (`LivingEntity::hurtServer` and
            // `LivingEntity::resolveMobResponsibleForDamage`).
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
        // `FloatGoal`'s condition is a disjunction (`FloatGoal::canUse`), so a
        // test that only ever sets water cannot tell `in_water() || in_lava()`
        // from `in_water()` — one arm could be dead. Drive each arm alone.
        let lava = FluidArena::with((0, 0, 0), PathType::Lava);
        let mut in_lava = perception_mob(&lava, Vec3::new(0.5, 0.0, 0.5));
        assert!(in_lava.in_lava(), "lava cell must classify as in_lava");
        assert!(
            !crate::ai::mob::MobController::in_water(&in_lava),
            "a lava cell must not also read as water — that would make the \
             two methods indistinguishable and the disjunction untestable"
        );
        assert!(FloatGoal.can_use(&mut in_lava), "lava alone must float");

        let water = FluidArena::with((0, 0, 0), PathType::Water);
        let mut in_water = perception_mob(&water, Vec3::new(0.5, 0.0, 0.5));
        assert!(crate::ai::mob::MobController::in_water(&in_water));
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
        // Vanilla registers `FloatGoal` at priority 1 on a creeper and 9 on a
        // bee (both in their own `registerGoals`); the absolute number is
        // private to one mob's set, so 0 is fine here.
        ai.add(0, Box::new(FloatGoal));

        // `tick` is 0.8-probability per tick (`FloatGoal::tick`), so a
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
        // appears (`Zombie::addBehaviourGoals`,
        // `ZombifiedPiglin::addBehaviourGoals`).
        ai.add(0, Box::new(HurtByTargetGoal::new()));

        mob.tick(&mut ai);
        assert_eq!(
            mob.attack_target(),
            Some(attacker),
            "a hurt mob must adopt its attacker as its attack target"
        );

        // Vanilla forgets the attacker past `LAST_HURT_BY_TICKS`
        // (`LivingEntity::baseTick`). Prove the decay is real and lands on the
        // predicted tick rather than merely "eventually": one `note_hurt`
        // followed by exactly that many `advance`s must clear it, and one
        // fewer must not.
        let mut early = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        early.note_hurt(Some(attacker));
        for _ in 0..LAST_HURT_BY_TICKS - 1 {
            early.advance();
        }
        assert_eq!(
            MobController::last_hurt_by(&early),
            Some(attacker),
            "the attacker must still be remembered one tick before the window closes"
        );
        early.advance();
        assert_eq!(
            MobController::last_hurt_by(&early),
            None,
            "the attacker must be forgotten exactly at LAST_HURT_BY_TICKS"
        );
    }

    /// The owner-scoped twins decay on exactly the same window as
    /// [`last_hurt_by`](NavigatingMob::last_hurt_by) — see
    /// `owner_hurt_by`/`owner_hurt_target`'s own doc comments for why they
    /// share [`LAST_HURT_BY_TICKS`] rather than a bespoke constant. Same
    /// exact-tick-boundary shape as the test above, for both records at once
    /// since they are independent fields set by independent host calls.
    #[test]
    fn owner_combat_records_decay_on_the_same_window_as_last_hurt_by() {
        let world = FluidArena::dry();
        let attacker = Vec3::new(4.5, 0.0, 0.5);
        let victim = Vec3::new(-2.0, 0.0, 6.0);

        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        mob.set_owner_hurt_by(Some(attacker));
        mob.set_owner_hurt_target(Some(victim));
        for _ in 0..LAST_HURT_BY_TICKS - 1 {
            mob.advance();
        }
        assert_eq!(
            MobController::owner_hurt_by(&mob),
            Some(attacker),
            "owner_hurt_by must still be live one tick before the window closes"
        );
        assert_eq!(
            MobController::owner_hurt_target(&mob),
            Some(victim),
            "owner_hurt_target must still be live one tick before the window closes"
        );
        mob.advance();
        assert_eq!(
            MobController::owner_hurt_by(&mob),
            None,
            "owner_hurt_by must clear exactly at LAST_HURT_BY_TICKS"
        );
        assert_eq!(
            MobController::owner_hurt_target(&mob),
            None,
            "owner_hurt_target must clear exactly at LAST_HURT_BY_TICKS"
        );
    }

    #[test]
    fn panic_expires_on_its_own_shorter_window_while_retaliation_persists() {
        // The two records decay independently and on *different* timers
        // (40 vs 100 — `LivingEntity::getLastDamageSource` and
        // `LivingEntity::baseTick`). A single
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
            MobController::last_hurt_by(&mob),
            Some(attacker),
            "retaliation must OUTLIVE panic — this is the assertion that fails \
             if the two windows are collapsed into one timer"
        );
    }

    #[test]
    fn attacker_less_damage_panics_without_giving_the_mob_anything_to_chase() {
        // Vanilla's panic reads the damage *source*, not the attacking mob
        // (`PanicGoal::shouldPanic` vs
        // `HurtByTargetGoal::canUse`), so fall damage panics a
        // cow and gives it no retaliation target. `note_hurt(None)` is that
        // case; without this test the two records could be one field.
        let world = FluidArena::dry();
        let mut mob = perception_mob(&world, Vec3::new(0.5, 0.0, 0.5));
        mob.note_hurt(None);
        assert!(mob.is_panicking(), "attacker-less damage must still panic");
        assert_eq!(
            MobController::last_hurt_by(&mob),
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
        // (`RandomStrollGoal::canUse`). Predict the boundary rather
        // than asserting a direction: 99 must still allow, 100 must suppress.
        let world = FluidArena::dry();

        // `interval(1)` makes the goal's own `next_i32(interval) != 0` roll
        // (`goals.rs`, vanilla `RandomStrollGoal::canUse`) deterministic, so
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
            "no_action_time 100 must suppress stroll (RandomStrollGoal::canUse)"
        );
    }

    // ── per-mob RNG seed gates ──────────────────────────────────────

    /// Divergence gate: two mobs at the same position with different seeds
    /// produce different stroll targets on the very first call — the mobs do
    /// not act in lockstep.
    ///
    /// The bounded tick is zero: `random_stroll_target` consumes two
    /// `next_unit` draws, and the SplitMix64 streams separate on the first
    /// draw. The negative control below proves this assertion would *pass*
    /// (not fire) under the old shared seed.
    #[test]
    fn different_seeds_produce_different_stroll_targets() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let pos = Vec3::new(0.5, 0.0, 0.5);

        let mut mob_a =
            NavigatingMob::new(&world, shape.clone(), pos, 0.25, 400, 0xAAAA_AAAA_AAAA_AAAA);
        let mut mob_b =
            NavigatingMob::new(&world, shape, pos, 0.25, 400, 0xBBBB_BBBB_BBBB_BBBB);

        let target_a = mob_a.random_stroll_target();
        let target_b = mob_b.random_stroll_target();

        assert_ne!(
            target_a, target_b,
            "different seeds at the same position must produce different stroll \
             targets; otherwise two mobs of the same species act in lockstep"
        );
    }

    /// Determinism gate: the same seed and starting state produces the same
    /// stroll target every time — replaying a world yields byte-identical
    /// mob behaviour.
    #[test]
    fn same_seed_and_position_produces_identical_stroll_target() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);
        let pos = Vec3::new(0.5, 0.0, 0.5);

        let mut mob_a =
            NavigatingMob::new(&world, shape.clone(), pos, 0.25, 400, 0xC0DE_C0DE_C0DE_C0DE);
        let mut mob_b =
            NavigatingMob::new(&world, shape, pos, 0.25, 400, 0xC0DE_C0DE_C0DE_C0DE);

        assert_eq!(
            mob_a.random_stroll_target(),
            mob_b.random_stroll_target(),
            "same seed + same position must produce identical stroll targets; \
             determinism requires replay yields byte-identical behaviour"
        );
    }

    /// Negative control: with the **same** seed, two mobs at different
    /// positions get the same random offset, so the separation between
    /// their targets is exactly the position delta — lockstep, the
    /// behaviour this issue fixes.
    ///
    /// This is the counter-assertion for the divergence gate above: if
    /// someone restores a shared seed, this test still passes (it measures
    /// the lockstep property) while `different_seeds_produce_different_stroll_targets`
    /// would fail — the two mobs would get the same target because both
    /// position and seed match, or would differ only by position delta if
    /// positions differ.
    #[test]
    fn shared_seed_yields_lockstep_offset_across_different_positions() {
        let world = Arena {
            walls: HashSet::new(),
        };
        let shape = MobShape::land(0.6, 1.95);

        let pos_a = Vec3::new(0.5, 0.0, 0.5);
        let pos_b = Vec3::new(10.5, 0.0, 10.5);
        let shared = 0x1234_5678_9ABC_DEF0;

        let mut mob_a =
            NavigatingMob::new(&world, shape.clone(), pos_a, 0.25, 400, shared);
        let mut mob_b =
            NavigatingMob::new(&world, shape, pos_b, 0.25, 400, shared);

        let target_a = mob_a.random_stroll_target().unwrap();
        let target_b = mob_b.random_stroll_target().unwrap();

        let pos_delta = pos_b - pos_a;
        let target_delta = target_b - target_a;

        // With the same seed, the random offset (dx, dz) is identical for
        // both mobs, so target_b - target_a must equal pos_b - pos_a
        // (both mobs move in the same direction by the same amount).
        assert!(
            (target_delta.x - pos_delta.x).abs() < 1e-9
                && (target_delta.z - pos_delta.z).abs() < 1e-9,
            "same seed => identical random offsets: target delta \
             ({target_delta:?}) must equal position delta ({pos_delta:?}); \
             this is the lockstep the per-mob seed eliminates"
        );
    }

    /// A world purpose-built for [`MobController::validate_teleport_landing`]'s
    /// own gate: a small solid platform at `(x, -1, z)` for `-2..=2` on both
    /// axes (so most random ±32-block teleport offsets land in open air with
    /// nothing below them all the way to `min_y`), a waterlogged solid cell
    /// at `(10, -1, 10)` (solid, but its fluid state is water), and a
    /// one-block-tall gap at `(3, 3)` — floor at `y=-1`, ceiling at `y=1`,
    /// too low for a 1.95-tall mob to stand in.
    struct TeleportWorld;

    impl PathWorld for TeleportWorld {
        fn min_y(&self) -> i32 {
            -64
        }
        fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
            if self.collision_top(x, y, z) > 0.0 {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
            if (-2..=2).contains(&x) && (-2..=2).contains(&z) && y == -1 {
                1.0
            } else if (x, y, z) == (10, -1, 10) {
                1.0
            } else if (x, y, z) == (3, -1, 3) || (x, y, z) == (3, 1, 3) {
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
                        if self.collision_top(x, y, z) > 0.0 {
                            return true;
                        }
                    }
                }
            }
            false
        }
        fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
            (x, y, z) == (10, -1, 10)
        }
    }

    fn teleport_mob(pos: Vec3) -> NavigatingMob<'static> {
        let world: &'static TeleportWorld = &TeleportWorld;
        NavigatingMob::new(world, MobShape::land(0.6, 1.95), pos, 0.2, 32, 1)
    }

    /// The positive case: a candidate whose column has solid, dry ground
    /// beneath it lands exactly on top of that ground (`y = -1`'s collision
    /// top `1.0` → feet at `y = 0.0`), not at the raw random `y` vanilla's
    /// `EnderMan::teleport` only ever proposes as a *candidate*.
    #[test]
    fn validate_teleport_landing_snaps_to_the_top_of_solid_ground() {
        let mob = teleport_mob(Vec3::new(0.0, 0.0, 0.0));
        let landing = mob
            .validate_teleport_landing(Vec3::new(1.5, 20.0, -1.5))
            .expect("the 5x5 platform covers (1, -1) and (-1, ...)... (1.5, -1.5) is inside it");
        assert!(
            (landing.y - 0.0).abs() < 1e-9,
            "must rest on top of the y=-1 platform (collision top 1.0), got y={}",
            landing.y
        );
        assert!(
            (landing.x - 1.5).abs() < 1e-9 && (landing.z - (-1.5)).abs() < 1e-9,
            "x/z must be preserved from the candidate: got {landing:?}"
        );
    }

    /// **This is the enderman-in-the-sky bug, reproduced and fixed.** A
    /// candidate over open air with nothing solid anywhere below it down to
    /// `min_y` must be rejected outright — the pre-fix code (`teleport_to`
    /// called unconditionally with the raw random offset) would have
    /// happily placed the mob at `y = 20.0`, floating over nothing.
    #[test]
    fn validate_teleport_landing_rejects_a_column_with_no_floor_before_min_y() {
        let mob = teleport_mob(Vec3::new(0.0, 0.0, 0.0));
        let landing = mob.validate_teleport_landing(Vec3::new(50.0, 20.0, 50.0));
        assert_eq!(
            landing, None,
            "column (50, 50) has no ground anywhere in this world; a teleport there \
             must be refused entirely, not resolved to some fallback height"
        );
    }

    /// Vanilla's `!isWet` half of `EnderMan::teleport`'s guard: a solid but
    /// waterlogged landing block must be rejected too, not just an empty
    /// column.
    #[test]
    fn validate_teleport_landing_rejects_a_waterlogged_landing() {
        let mob = teleport_mob(Vec3::new(0.0, 0.0, 0.0));
        let landing = mob.validate_teleport_landing(Vec3::new(10.5, 20.0, 10.5));
        assert_eq!(
            landing, None,
            "(10, -1, 10) is solid but waterlogged; vanilla's !isWet check must reject it"
        );
    }

    /// Vanilla's `level.noCollision(this)` half: solid ground exists directly
    /// below the candidate, but the mob's own 1.95-tall footprint does not
    /// fit under the one-block gap's ceiling at `y = 1`, so the landing must
    /// be rejected even though the ground check alone would have passed. The
    /// candidate `y` (`0.5`) is deliberately *inside* the gap — matching
    /// vanilla's own `getY() + (nextInt(64) - 32)` offset, which can land
    /// anywhere relative to the mob's current height, not only far above
    /// everything — so the downward scan reaches the true floor at `y=-1`
    /// instead of resting on the ceiling itself.
    #[test]
    fn validate_teleport_landing_rejects_a_footprint_that_cannot_fit() {
        let mob = teleport_mob(Vec3::new(0.0, 0.0, 0.0));
        let landing = mob.validate_teleport_landing(Vec3::new(3.5, 0.5, 3.5));
        assert_eq!(
            landing, None,
            "(3, 3) has ground at y=-1 and a ceiling at y=1, a one-block gap; a \
             1.95-tall mob cannot stand there, and vanilla's noCollision check must \
             catch it even though the ground beneath the candidate is solid and dry"
        );
    }

    /// Open sea from y 0 to 40: every cell water, nothing solid.
    struct Sea;

    impl PathWorld for Sea {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if (0..=40).contains(&y) { PathType::Water } else { PathType::Open }
        }
        fn collision_top(&self, _x: i32, _y: i32, _z: i32) -> f64 {
            0.0
        }
        fn collides(&self, _aabb: Aabb) -> bool {
            false
        }
    }

    #[test]
    fn a_drifter_pulses_its_vector_for_exactly_the_hand_derived_ticks() {
        // Pulse rate 0.2 per tick, so after k ticks the phase is 0.2k. The
        // vector is applied while phase/pi is above 0.75 and phase is below
        // pi: k = 12 (2.4 / 3.1416 = 0.76) through k = 15 (3.0). Before that
        // the body is at rest; from k = 16 (3.2 > pi) it keeps 0.9 per tick.
        let world = Sea;
        let mut mob =
            NavigatingMob::new(&world, MobShape::drifter(0.8, 0.8), Vec3::new(0.5, 20.0, 0.5), 0.0, 100, 0);
        mob.pulse_rate = 0.2;
        mob.drift_vector = Vec3::new(0.2, 0.0, 0.0);
        let mut ai = GoalSelector::new();
        let mut speeds = Vec::new();
        for _ in 0..18 {
            mob.tick(&mut ai);
            speeds.push(mob.velocity().x);
        }
        let expected = [
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.2, 0.2, 0.2, 0.2, 0.18, 0.162, 0.1458,
        ];
        for (k, (got, want)) in speeds.iter().zip(expected).enumerate() {
            assert!((got - want).abs() < 1e-9, "tick {}: {got} vs {want}", k + 1);
        }
    }

    #[test]
    fn a_drifter_out_of_water_only_falls() {
        // Out of water the horizontal velocity is zeroed and the vertical one
        // follows (v - 0.08) * 0.98: -0.0784, then (-0.0784 - 0.08) * 0.98.
        let world = FluidArena::dry();
        let mut mob =
            NavigatingMob::new(&world, MobShape::drifter(0.8, 0.8), Vec3::new(0.5, 10.0, 0.5), 0.0, 100, 0);
        mob.drift_vector = Vec3::new(0.2, 0.1, 0.0);
        let mut ai = GoalSelector::new();
        mob.tick(&mut ai);
        assert!((mob.velocity().y + 0.0784).abs() < 1e-9 && mob.velocity().x == 0.0);
        mob.tick(&mut ai);
        assert!((mob.velocity().y + 0.155_232).abs() < 1e-9, "{}", mob.velocity().y);
    }

    fn flee_vector(attacker_x: f64) -> Vec3 {
        use crate::ai::goals::DriftFleeGoal;
        let world = Sea;
        let mut mob =
            NavigatingMob::new(&world, MobShape::drifter(0.8, 0.8), Vec3::new(0.5, 20.0, 0.5), 0.0, 100, 0);
        mob.note_hurt(Some(Vec3::new(attacker_x, 20.0, 0.5)));
        let mut ai = GoalSelector::new();
        ai.add(1, Box::new(DriftFleeGoal::new()));
        mob.tick(&mut ai);
        mob.drift_vector()
    }

    #[test]
    fn a_hurt_drifter_flees_scaled_by_distance() {
        // Attacker 2 blocks behind: away vector 2, boost 3, divided by 20 = 0.3.
        assert!((flee_vector(-1.5).x - 0.3).abs() < 1e-9);
        // 8 blocks behind: boost 3 - (8 - 5) / 5 = 2.4, so 8 * 2.4 / 20 = 0.96.
        assert!((flee_vector(-7.5).x - 0.96).abs() < 1e-9);
        // 10 or more blocks away it does not flee at all.
        assert_eq!(flee_vector(-9.5).x, 0.0);
    }

    fn floater<'w>(world: &'w dyn PathWorld) -> NavigatingMob<'w> {
        let mut mob = NavigatingMob::new(world, MobShape::flier(NavMode::Fly, 4.0, 4.0), Vec3::new(0.5, 20.0, 0.5), 0.0, 100, 0);
        mob.set_flying_speed(0.06);
        mob
    }

    #[test]
    fn a_floater_accelerates_by_five_thirds_of_its_flying_speed_then_coasts() {
        // 0.06 * 5 / 3 = 0.1 along the unit direction on the first push, which
        // applies the same tick; the next push is at least one tick away, so the
        // second tick keeps 0.91 of it.
        let mut mob = floater(&Sea);
        mob.float_to(Vec3::new(10.5, 20.0, 0.5));
        let mut ai = GoalSelector::new();
        mob.tick(&mut ai);
        assert!((mob.velocity().x - 0.1).abs() < 1e-12 && mob.velocity().y == 0.0);
        mob.tick(&mut ai);
        assert!((mob.velocity().x - 0.091).abs() < 1e-12, "{}", mob.velocity().x);
    }

    #[test]
    fn a_floater_gives_up_a_wanted_position_behind_a_wall() {
        let walls: HashSet<_> = (15..=25).flat_map(|y| (-3..=3).flat_map(move |z| [(3, y, z)])).collect();
        let world = Arena { walls };
        let mut mob = floater(&world);
        mob.float_to(Vec3::new(10.5, 20.0, 0.5));
        let mut ai = GoalSelector::new();
        mob.tick(&mut ai);
        assert_eq!(mob.float_wanted(), None);
        assert_eq!(mob.velocity().x, 0.0);
    }

    fn bee<'w>(world: &'w dyn PathWorld, mode: NavMode) -> NavigatingMob<'w> {
        let mut mob = NavigatingMob::new(world, MobShape::flier(mode, 0.7, 0.6), Vec3::new(0.5, 5.0, 0.5), 0.3, 4000, 0);
        mob.set_flying_speed(0.6);
        mob
    }

    #[test]
    fn a_bee_on_a_level_path_cruises_at_its_thrust_limit() {
        // Thrust 0.02 along a heading scaled by the requested speed 0.6 gives
        // 0.012 a tick added before the move; air then keeps 0.91, so the
        // per-tick displacement settles at 0.012 / (1 - 0.91).
        let world = Arena { walls: HashSet::new() };
        let mut mob = bee(&world, NavMode::Air);
        assert!(MobController::move_to(&mut mob, Vec3::new(40.5, 5.0, 0.5), 0.3));
        let mut ai = GoalSelector::new();
        for _ in 0..70 {
            mob.tick(&mut ai);
        }
        let cruise = mob.velocity().x;
        assert!((cruise - 0.012 / 0.09).abs() < 0.002, "{cruise}");
        assert!((mob.position().y - 5.0).abs() < 0.1, "a level path stays level, at {}", mob.position().y);
    }

    #[test]
    fn a_bee_flies_over_a_wall_a_walker_could_not_cross() {
        let walls: HashSet<_> = (0..=5).flat_map(|y| (-20..=20).map(move |z| (6, y, z))).collect();
        let world = Arena { walls };
        let mut mob = bee(&world, NavMode::Air);
        assert!(MobController::move_to(&mut mob, Vec3::new(12.5, 5.0, 0.5), 0.3));
        let mut ai = GoalSelector::new();
        let mut highest = 0.0_f64;
        for _ in 0..400 {
            mob.tick(&mut ai);
            highest = highest.max(mob.position().y);
        }
        assert!(mob.position().x > 10.5, "{:?}", mob.position());
        assert!(highest > 5.9, "it went over, peaking at {highest}");
    }

    fn bat<'w>(world: &'w dyn PathWorld) -> NavigatingMob<'w> {
        NavigatingMob::new(world, MobShape::flier(NavMode::Flutter, 0.5, 0.9), Vec3::new(0.5, 10.0, 0.5), 0.0, 100, 0)
    }

    #[test]
    fn a_bat_in_the_open_eases_toward_its_target_block() {
        // From rest toward a block up and to +x: the velocity eases a tenth of
        // the way to (0.5, 0.7, 0) = (0.05, 0.07, 0); facing +x, forward input
        // 0.5 adds 0.5 * 0.02 along x for a displacement of (0.06, 0.07, 0).
        // Drag then leaves (0.06 * 0.91, (0.07 - 0.08) * 0.98 * 0.6, 0).
        let world = Arena { walls: HashSet::new() };
        let mut mob = bat(&world);
        mob.bat.resting = false;
        mob.bat.target = Some((5, 12, 0));
        let mut ai = GoalSelector::new();
        let start = mob.position();
        mob.tick(&mut ai);
        let p = mob.position();
        assert!((p.x - start.x - 0.06).abs() < 1e-9 && (p.y - start.y - 0.07).abs() < 1e-9, "{p:?}");
        assert!(mob.fly_velocity.z.abs() < 1e-9);
        assert!((mob.fly_velocity.x - 0.0546).abs() < 1e-9, "{}", mob.fly_velocity.x);
        assert!((mob.fly_velocity.y - (-0.01 * 0.98 * 0.6)).abs() < 1e-9, "{}", mob.fly_velocity.y);
    }

    fn phantom<'w>(world: &'w dyn PathWorld) -> NavigatingMob<'w> {
        NavigatingMob::new(world, MobShape::flier(NavMode::Swoop, 0.9, 0.5), Vec3::new(0.5, 20.0, 0.5), 0.0, 100, 0)
    }

    #[test]
    fn a_phantom_facing_its_target_gains_speed_and_eases_its_velocity() {
        // Facing +x (yaw -90) at a target straight ahead, the heading is steady,
        // so the speed climbs from 0.1 by 0.005 * (1.8 / 0.1) = 0.09 to 0.19. The
        // velocity then eases a fifth of the way to (0.19, 0, 0).
        let world = Arena { walls: HashSet::new() };
        let mut mob = phantom(&world);
        mob.set_body_yaw(-90.0);
        mob.swoop.move_target = Vec3::new(20.5, 20.0, 0.5);
        let mut ai = GoalSelector::new();
        mob.tick(&mut ai);
        assert!((mob.swoop_speed - 0.19).abs() < 1e-6, "{}", mob.swoop_speed);
        assert!((mob.position().x - 0.5 - 0.038).abs() < 1e-6, "{:?}", mob.position());
        assert!(mob.position().y == 20.0 && (mob.position().z - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_phantom_turns_four_degrees_a_tick_and_slows_while_turning() {
        // From yaw 0 (facing +z) toward +x the first tick turns 4 degrees to -4,
        // too sharp for the steady rule, so the speed drops to its 0.2 floor. The
        // velocity eases a fifth of the way to 0.2 * cos(86 degrees) along x.
        let world = Arena { walls: HashSet::new() };
        let mut mob = phantom(&world);
        mob.swoop.move_target = Vec3::new(20.5, 20.0, 0.5);
        let mut ai = GoalSelector::new();
        mob.tick(&mut ai);
        assert!((mob.body_yaw() + 4.0).abs() < 1e-4, "{}", mob.body_yaw());
        assert!((mob.swoop_speed - 0.2).abs() < 1e-9);
        let expected = 0.2 * 86.0_f64.to_radians().cos() * 0.2;
        assert!((mob.position().x - 0.5 - expected).abs() < 1e-6, "{:?}", mob.position());
    }

    /// Water for x < 10 from the floor at y -6 up to the surface cell y -1, dry
    /// land for x >= 10 standing at y 0, nothing above.
    struct Shore;

    impl Shore {
        fn solid(x: i32, y: i32) -> bool {
            y <= -6 || (x >= 10 && y <= -1)
        }
    }

    impl PathWorld for Shore {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, x: i32, y: i32, _z: i32) -> PathType {
            if Self::solid(x, y) {
                PathType::Blocked
            } else if x < 10 && (-5..=-1).contains(&y) {
                PathType::Water
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, x: i32, y: i32, _z: i32) -> f64 {
            if Self::solid(x, y) { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            let (x0, x1) = (aabb.min_x.floor() as i32, (aabb.max_x - 1e-7).floor() as i32);
            let (y0, y1) = (aabb.min_y.floor() as i32, (aabb.max_y - 1e-7).floor() as i32);
            (x0..=x1).any(|x| (y0..=y1).any(|y| Self::solid(x, y)))
        }
    }

    fn axolotl<'w>(world: &'w dyn PathWorld, at: Vec3) -> NavigatingMob<'w> {
        let shape = MobShape::amphibian(SwimRule::Smooth { in_water: 0.1, on_land: 0.5, buoyant: false }, 0.75, 0.42);
        NavigatingMob::new(world, shape, at, 1.0, 4000, 0)
    }

    #[test]
    fn a_smooth_swimmer_cruises_at_its_hand_derived_thrust_limit() {
        // A goal speed of 0.5 against an attribute of 1.0: the in-water factor
        // 0.1 makes the move speed 0.05, the input of length 0.5 adds 0.025 a
        // tick along the heading, and 0.9 drag settles the displacement at
        // 0.025 / (1 - 0.9).
        let mut mob = axolotl(&Shore, Vec3::new(1.5, -4.0, 0.5));
        assert!(MobController::move_to(&mut mob, Vec3::new(8.5, -4.0, 0.5), 0.5));
        let mut ai = GoalSelector::new();
        let mut best = 0.0_f64;
        for _ in 0..60 {
            mob.tick(&mut ai);
            best = best.max(mob.velocity().x);
        }
        assert!((best - 0.25).abs() < 0.02, "{best}");
    }

    #[test]
    fn an_amphibian_swims_out_and_walks_up_the_shore() {
        let mut mob = axolotl(&Shore, Vec3::new(1.5, -4.0, 0.5));
        mob.body_yaw = -90.0;
        mob.set_follow_range(32.0);
        assert!(MobController::move_to(&mut mob, Vec3::new(14.5, 0.0, 0.5), 0.3));
        let mut ai = GoalSelector::new();
        for _ in 0..600 {
            mob.tick(&mut ai);
        }
        let p = mob.position();
        assert!(p.x > 12.0 && p.y > -0.5, "{p:?}");
    }

    #[test]
    fn a_water_only_swimmer_stays_behind_the_shore() {
        let shape = MobShape::flier(NavMode::Swim, 0.75, 0.42);
        let mut mob = NavigatingMob::new(&Shore, shape, Vec3::new(1.5, -4.0, 0.5), 1.0, 4000, 0);
        mob.body_yaw = -90.0;
        mob.set_follow_range(32.0);
        MobController::move_to(&mut mob, Vec3::new(14.5, 0.0, 0.5), 0.3);
        let mut ai = GoalSelector::new();
        for _ in 0..600 {
            mob.tick(&mut ai);
        }
        assert!(mob.position().x < 10.0, "{:?}", mob.position());
    }

    fn drowned<'w>(world: &'w dyn PathWorld, at: Vec3) -> NavigatingMob<'w> {
        let mut shape = MobShape::amphibian(SwimRule::Drowned, 0.6, 1.95);
        shape.max_up_step = 1.0;
        let mut mob = NavigatingMob::new(world, shape, at, 1.0, 4000, 0);
        mob.set_follow_range(32.0);
        mob
    }

    /// A turtle narrowed to one cell, so the sheer shore of [`Shore`] does not
    /// leave a two-cell body hanging over the drop.
    fn turtle<'w>(world: &'w dyn PathWorld, at: Vec3) -> NavigatingMob<'w> {
        let mut shape = MobShape::amphibian(SwimRule::Turtle, 0.9, 0.4);
        shape.max_up_step = 1.0;
        let mut mob = NavigatingMob::new(world, shape, at, 1.0, 4000, 0);
        mob.set_follow_range(48.0);
        mob
    }

    fn selector_for(species: &str, speed: f64) -> GoalSelector {
        let mut ai = GoalSelector::new();
        for (priority, goal) in crate::ai::roster::goals_for(species, &crate::ai::roster::SpeciesContext::new(speed)) {
            ai.add(priority, goal);
        }
        ai
    }

    fn run_species(mob: &mut NavigatingMob<'_>, species: &str, speed: f64, ticks: usize) {
        let mut ai = selector_for(species, speed);
        for _ in 0..ticks {
            mob.tick(&mut ai);
        }
    }

    #[test]
    fn a_turtle_ashore_walks_into_water_found_two_blocks_below_its_feet() {
        let mut mob = turtle(&Shore, Vec3::new(11.5, 0.0, 0.5));
        let mut ai = selector_for("turtle", 0.25);
        let mut entered = false;
        for _ in 0..120 {
            mob.tick(&mut ai);
            entered |= mob.in_water;
        }
        assert!(entered, "{:?}", mob.position());
    }

    #[test]
    fn a_turtle_with_no_water_in_reach_stays_on_land() {
        let mut mob = turtle(&Shore, Vec3::new(70.5, 0.0, 0.5));
        run_species(&mut mob, "turtle", 0.25, 150);
        assert!(!mob.in_water && mob.position().x > 40.0, "{:?}", mob.position());
    }

    #[test]
    fn a_drowned_in_daylight_leaves_the_land_for_water_and_at_night_does_not() {
        let mut day = drowned(&Shore, Vec3::new(11.5, 0.0, 0.5));
        day.set_sun_state(true, false, false);
        run_species(&mut day, "drowned", 0.23, 300);
        assert!(day.in_water, "{:?}", day.position());

        let mut night = drowned(&Shore, Vec3::new(11.5, 0.0, 0.5));
        run_species(&mut night, "drowned", 0.23, 300);
        assert!(!night.in_water, "{:?}", night.position());
    }

    #[test]
    fn a_drowned_heading_for_land_adds_its_buoyancy_to_the_swim_push() {
        // From rest the move speed eases to 0.23 * 0.125 = 0.02875; straight
        // up, the push is 0.1 of that, and heading for land adds 0.002. A
        // drowned also swims toward a target in water; one level with it adds no
        // buoyancy.
        let rise = |searching: bool| {
            let mut mob = drowned(&Shore, Vec3::new(1.5, -5.0, 0.5));
            let mut ai = GoalSelector::new();
            mob.tick(&mut ai);
            if searching {
                MobController::set_searching_for_land(&mut mob, true);
            } else {
                MobController::set_attack_target(&mut mob, Some(Vec3::new(1.5, -5.0, 0.5)));
            }
            assert!(MobController::move_to(&mut mob, Vec3::new(1.5, -2.0, 0.5), 0.23));
            let before = mob.position().y;
            mob.tick(&mut ai);
            mob.position().y - before
        };
        assert!((rise(true) - 0.004_875).abs() < 1e-9, "{}", rise(true));
        assert!((rise(false) - 0.002_875).abs() < 1e-9, "{}", rise(false));
    }

    #[test]
    fn the_swim_up_goal_reads_the_hosts_sea_level() {
        let heads_for_land = |sea_level: i32| {
            let mut mob = drowned(&Shore, Vec3::new(1.5, -4.0, 0.5));
            mob.set_sea_level(sea_level);
            let mut ai = selector_for("drowned", 0.23);
            let mut flagged = false;
            for _ in 0..10 {
                mob.tick(&mut ai);
                flagged |= mob.searching_for_land;
            }
            flagged
        };
        // The goal runs below two blocks under sea level: -4 is under 61 and
        // under -2 would need a sea level above -2.
        assert!(heads_for_land(63));
        assert!(!heads_for_land(-2));
    }

    #[test]
    fn a_walking_amphibian_applies_its_land_factor_to_the_move_speed() {
        // Ground thrust is the speed times the input of the same size, so a
        // land factor of 0.5 quarters the first step.
        let first_step = |on_land: f64| {
            let shape = MobShape::amphibian(SwimRule::Smooth { in_water: 0.1, on_land, buoyant: false }, 0.75, 0.42);
            let mut mob = NavigatingMob::new(&Shore, shape, Vec3::new(12.5, 0.0, 0.5), 1.0, 4000, 0);
            mob.tick(&mut GoalSelector::new());
            MobController::move_to(&mut mob, Vec3::new(20.5, 0.0, 0.5), 0.2);
            let before = mob.position().x;
            mob.tick(&mut GoalSelector::new());
            mob.position().x - before
        };
        let ratio = first_step(0.5) / first_step(1.0);
        assert!((ratio - 0.25).abs() < 1e-9, "{ratio}");
    }
}
