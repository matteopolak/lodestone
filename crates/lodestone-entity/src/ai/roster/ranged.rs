//! Goal sets for the ranged attackers, and the goals themselves: the bow goal,
//! the blaze's fireball burst, and vanilla's generic `RangedAttackGoal`.
//!
//! # What it is
//!
//! Issue [#227]'s first half (**B3a**): a real ranged-attack goal family, and the
//! [`ProjectileLaunch`] intent that carries a shot out of the AI layer. Before
//! this module, `RangedAttackGoal` and `BowAttack` were zero hits tree-wide, so
//! **no mob in this repo could shoot anything**.
//!
//! Three goals live here rather than in [`goals`](crate::ai::goals) because they
//! are the only consumers of the launch seam and because five roster units were
//! editing that file in parallel. Nothing else needs them.
//!
//! # How it works
//!
//! A goal never spawns an entity. It computes vanilla's aiming maths, then calls
//! [`MobController::launch_projectile`] — the exact shape
//! [`MobController::attack`] already had for melee, and for the same reason:
//! `lodestone-entity` has no world, no entity-id allocator and no projectile
//! registry. The host drains
//! [`NavigatingMob::take_new_launches`](crate::ai::NavigatingMob::take_new_launches)
//! once per tick and turns each launch into a real projectile.
//!
//! ```text
//! BlazeFireballGoal::tick
//!   -> MobController::launch_projectile(ProjectileLaunch { SmallFireball, origin, velocity })
//!   -> NavigatingMob.launches                         (this crate ends here)
//!   -> MobSim::tick drains take_new_launches()        <-- THE ONE REMAINING LINK
//!   -> MobSim::spawn_projectile -> ProjectileRegistry
//!   -> MobSim::snapshots() lowers it to an EntitySnapshot   (mobs.rs:2281-2310)
//!   -> LiveMobSource -> EntityStreamer::sync -> encode_add_entity  (server.rs:248-262)
//!   -> a real client sees minecraft:small_fireball appear
//! ```
//!
//! # What is NOT wired, as of this module landing
//!
//! **The `MobSim::tick` drain on the fourth line above does not exist yet**, and
//! it is one patch in `crates/lodestone-server/src/mobs.rs`, a file this unit does
//! not own (a concurrent agent holds it). Everything above and below that line is
//! built and gated. Until that patch lands, **a ranged goal fires, computes the
//! right shot, and no projectile is created in production** — an island of exactly
//! the kind CLAUDE.md's first rule is about, and it is named here rather than left
//! for someone to discover.
//!
//! The projectile *wire* path is not the gap. A3's survey recorded that "nothing
//! launches a projectile in production" and that `ProjectileRegistry` is
//! "permanently empty at runtime", which is true, but the reason is only the
//! missing drain: `MobSim::snapshots` has lowered tracked projectiles into
//! `EntitySnapshot`s since issue #211, so a projectile that *does* get spawned
//! already reaches a client over the same `ADD_ENTITY` path a mob uses.
//! `projectile_reaches_a_real_client` in
//! `crates/lodestone-server/tests/ranged_projectile_visibility.rs` measures that
//! end of it against a real connection.
//!
//! # Hit detection is B3b, and is genuinely absent
//!
//! Nothing damages anything when a projectile arrives. `MobSim::spawn_projectile`
//! says so itself (`mobs.rs:2137-2143`, "hit detection against terrain/entities
//! and the resulting damage/area-effect are **not** done here"), as does
//! `ProjectileRegistry`'s own module doc (`projectile.rs:186-191`). `tick` only
//! ever advances motion.
//!
//! # How to change it
//!
//! Add each species path to [`SPECIES`] and an arm to [`lookup`]. Priorities are
//! vanilla's own numbers, unshifted — see [`super`]'s module doc for why that is
//! sound. Two rows for this family live in [`super::hostile_melee`] instead,
//! because skeletons and drowned resolve there; [`bow_attack`] and
//! [`trident_attack`] are `pub` for exactly that reason.
//!
//! [#227]: https://github.com/matteopolak/lodestone/issues/227

use lodestone_model::Vec3;

use super::{
    Registration, Selector, SpeciesContext, avoid_entity, float_goal, hurt_by_target,
    look_at_player_6, look_at_player_8, nearest_attackable_target, random_look_around, stroll,
};
use crate::ai::goal::{Flag, FlagSet, Goal};
use crate::ai::mob::{MobController, ProjectileKind, ProjectileLaunch, distance_sqr};

// -- shared aiming constants -------------------------------------------------

/// Where a shot leaves the shooter, as a height above its feet.
///
/// Vanilla spawns a mob arrow at the shooter's shoulder — `ProjectileUtil.getMobArrow`
/// builds it at `eyeY - 0.1` — and the blaze puts its fireball at
/// `getY(0.5) + 0.5` (`monster/Blaze.java:239`). The [`MobController`] seam
/// carries neither eye height nor bounding box, so this is one flat figure: a
/// skeleton is 1.99 tall with a 1.74 eye height, giving 1.64, and 1.4 is that
/// rounded down to sit inside the shorter mobs in the family too. **A disclosed
/// approximation of a number the seam cannot see**, not a misread jar line.
const SHOOTER_SHOULDER_Y: f64 = 1.4;

/// Where a shot is aimed on the target, as a height above its feet.
///
/// Vanilla aims a third of the way up the target's box
/// (`target.getY(0.3333)`, `monster/skeleton/AbstractSkeleton.java:165`); for a
/// 1.8-tall player that is 0.6. Same disclosure as
/// [`SHOOTER_SHOULDER_Y`] — the seam has no target bounding box.
const AIM_HEIGHT: f64 = 0.6;

/// Vanilla's ballistic arc compensation: the vertical aim component gains
/// `horizontalDistance * 0.2` (`AbstractSkeleton.java:170`,
/// `monster/Witch.java:249`, `animal/golem/SnowGolem.java:122`). All three
/// species in this family use the same `0.2F`.
const ARC_LIFT: f64 = 0.2;

/// A bow at full draw. Vanilla releases at `getTicksUsingItem() >= 20`
/// (`ai/goal/RangedBowAttackGoal.java:128`), which is also the point
/// `BowItem.getPowerForTime` reaches its `1.0` ceiling.
const BOW_FULL_DRAW_TICKS: i32 = 20;

/// Arrow launch power, vanilla's `1.6F`
/// (`AbstractSkeleton.java:170` — the `pow` argument to
/// `Projectile.spawnProjectileUsingShoot`). The snow golem
/// (`SnowGolem.java:129`) uses the same figure.
const ARROW_POWER: f64 = 1.6;

/// A blaze fireball's launch speed. `SmallFireball` is an
/// `AbstractHurtingProjectile`, whose constructor sets
/// `direction.normalize().scale(accelerationPower)` with `accelerationPower`
/// defaulting to `0.1` (`hurtingprojectile/AbstractHurtingProjectile.java:24`
/// and `:180-183`). **Not `1.6`** — a fireball is two orders of magnitude
/// slower off the muzzle than an arrow, and it accelerates afterwards
/// (`:126`) rather than falling.
const FIREBALL_POWER: f64 = 0.1;

/// Resolves the shot from `shooter_feet` to `target_feet` for a species that
/// uses vanilla's `xd / yd + dist * 0.2 / zd` aim, at `power`.
///
/// Shared by the bow goal and the generic ranged goal because all three jar
/// sites compute it identically apart from the power figure
/// (`AbstractSkeleton.java:164-171`, `Witch.java:225-249`,
/// `SnowGolem.java:119-129`).
fn arced_shot(
    kind: ProjectileKind,
    shooter_feet: Vec3,
    target_feet: Vec3,
    power: f64,
) -> ProjectileLaunch {
    let origin = Vec3::new(
        shooter_feet.x,
        shooter_feet.y + SHOOTER_SHOULDER_Y,
        shooter_feet.z,
    );
    let dx = target_feet.x - origin.x;
    let dz = target_feet.z - origin.z;
    let dy = (target_feet.y + AIM_HEIGHT) - origin.y;
    let horizontal = (dx * dx + dz * dz).sqrt();
    ProjectileLaunch::aimed(kind, origin, dx, dy + horizontal * ARC_LIFT, dz, power)
}

// -- RangedBowAttackGoal -----------------------------------------------------

/// Vanilla `RangedBowAttackGoal` (`ai/goal/RangedBowAttackGoal.java`) — the
/// skeleton family's bow.
///
/// # What is modelled, and what is not
///
/// The **draw/release cycle and the approach/hold distance logic** are the
/// behaviour: close to `attackRadius` with the target in sight the mob stops
/// moving and shoots on a fixed interval; further out it walks in. That is
/// `:70-137`.
///
/// Three things are deliberately not modelled, each because the seam cannot see
/// them:
///
/// * **`isHoldingBow`** (`:40-42`). Nothing in this repo gives a mob an
///   inventory, so [`can_use`](Goal::can_use) gates on having a target alone.
///   This is also why the runtime melee↔bow swap in `AbstractSkeleton.reassessWeaponGoal`
///   (`:132-146`) has nothing to swap *on*: a skeleton's weapon never changes,
///   so its table registers the bow at priority 4 statically.
/// * **Line of sight** (`getSensing().hasLineOfSight`). The server's own census
///   already applies a visibility filter before it feeds
///   [`MobController::find_nearest_target`], so "has a target" stands in for
///   "can see the target", and `seeTime` therefore only ever climbs.
/// * **Strafing** (`:94-113`). It drives `MoveControl.strafe`, a controller this
///   repo has no equivalent of; the mob holds position instead of circling.
#[derive(Debug)]
pub struct RangedBowAttackGoal {
    speed: f64,
    /// Vanilla's `attackIntervalMin`, the cooldown after a release.
    attack_interval: i32,
    attack_radius_sqr: f64,
    /// `-1` until the first release, then counts down (`:17`, `:134`).
    attack_time: i32,
    see_time: i32,
    /// `Some(ticks)` while the bow is drawn — vanilla's `isUsingItem` plus
    /// `getTicksUsingItem` (`:123-136`).
    drawing: Option<i32>,
}

impl RangedBowAttackGoal {
    /// `RangedBowAttackGoal(mob, speedModifier, attackIntervalMin, attackRadius)`
    /// (`ai/goal/RangedBowAttackGoal.java:23-29`). `speed` is already absolute
    /// (the caller has applied the jar's multiplier).
    #[must_use]
    pub fn new(speed: f64, attack_interval: i32, attack_radius: f64) -> Self {
        Self {
            speed,
            attack_interval,
            attack_radius_sqr: attack_radius * attack_radius,
            attack_time: -1,
            see_time: 0,
            drawing: None,
        }
    }
}

impl Goal for RangedBowAttackGoal {
    fn flags(&self) -> FlagSet {
        // `setFlags(EnumSet.of(MOVE, LOOK))` (`:28`).
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        // `getTarget() == null ? false : isHoldingBow()` (`:36-38`), minus the
        // inventory half — see the type's own doc.
        mob.attack_target().is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        // `canUse() || !getNavigation().isDone()` (`:46`).
        mob.attack_target().is_some() || !mob.navigation_done()
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        // `:56-62` — clears both timers and drops the draw.
        self.see_time = 0;
        self.attack_time = -1;
        self.drawing = None;
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = mob.attack_target() else {
            return;
        };
        let target_dist_sqr = distance_sqr(mob.position(), target);
        self.see_time += 1;

        // `:86-92`: hold position once close enough and settled, else close in.
        if target_dist_sqr <= self.attack_radius_sqr && self.see_time >= 20 {
            mob.stop_navigation();
        } else {
            mob.move_to(target, self.speed);
        }
        mob.look_at(target);

        // `:123-136`: draw for 20 ticks, release, then wait out the interval.
        match self.drawing {
            Some(pull) => {
                let pull = pull + 1;
                if pull >= BOW_FULL_DRAW_TICKS {
                    self.drawing = None;
                    self.attack_time = self.attack_interval;
                    mob.launch_projectile(arced_shot(
                        ProjectileKind::Arrow,
                        mob.position(),
                        target,
                        ARROW_POWER,
                    ));
                } else {
                    self.drawing = Some(pull);
                }
            }
            None => {
                self.attack_time -= 1;
                if self.attack_time <= 0 {
                    self.drawing = Some(0);
                }
            }
        }
    }
}

// -- RangedAttackGoal --------------------------------------------------------

/// Vanilla `RangedAttackGoal` (`ai/goal/RangedAttackGoal.java`) — the generic
/// one, shared by the snow golem, the witch and the drowned's trident.
///
/// Unlike the bow goal this has no draw phase: it fires the moment its
/// interval expires, and the interval itself **scales with range** between
/// `attackIntervalMin` and `attackIntervalMax` (`:88-99`). Every species in this
/// family passes the same value for both, via the four-argument constructor
/// (`:22-24`), so the lerp is currently a constant — transcribed anyway, because
/// a species that passes two different values is a one-line change and a
/// flattened constant could not be checked against the jar.
///
/// Line of sight is modelled the same way [`RangedBowAttackGoal`] models it, for
/// the same reason.
#[derive(Debug)]
pub struct RangedAttackGoal {
    kind: ProjectileKind,
    power: f64,
    speed: f64,
    interval_min: i32,
    interval_max: i32,
    attack_radius: f64,
    attack_radius_sqr: f64,
    attack_time: i32,
    see_time: i32,
}

impl RangedAttackGoal {
    /// `RangedAttackGoal(mob, speedModifier, attackIntervalMin, attackIntervalMax, attackRadius)`
    /// (`ai/goal/RangedAttackGoal.java:26-41`), plus which projectile this
    /// species throws and at what power — vanilla carries those in the species'
    /// own `performRangedAttack` rather than in the goal.
    #[must_use]
    pub fn new(
        kind: ProjectileKind,
        power: f64,
        speed: f64,
        interval_min: i32,
        interval_max: i32,
        attack_radius: f64,
    ) -> Self {
        Self {
            kind,
            power,
            speed,
            interval_min,
            interval_max,
            attack_radius,
            attack_radius_sqr: attack_radius * attack_radius,
            attack_time: -1,
            see_time: 0,
        }
    }

    /// Vanilla's own `Mth.floor(Mth.lerp(dist / radius, min, max))` (`:98`).
    fn interval_at(&self, distance: f64) -> i32 {
        let t = (distance / self.attack_radius).clamp(0.0, 1.0);
        let min = f64::from(self.interval_min);
        let max = f64::from(self.interval_max);
        (min + (max - min) * t).floor() as i32
    }
}

impl Goal for RangedAttackGoal {
    fn flags(&self) -> FlagSet {
        // `:40`.
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        // `:44-52` — a live target, nothing more.
        mob.attack_target().is_some()
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        // `:60-64`.
        self.see_time = 0;
        self.attack_time = -1;
        mob.stop_navigation();
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = mob.attack_target() else {
            return;
        };
        let position = mob.position();
        let target_dist_sqr = distance_sqr(position, target);
        self.see_time += 1;

        // `:81-85`. Note the threshold is 5 here, not the bow goal's 20.
        if target_dist_sqr <= self.attack_radius_sqr && self.see_time >= 5 {
            mob.stop_navigation();
        } else {
            mob.move_to(target, self.speed);
        }
        mob.look_at(target);

        // `:88-99`.
        self.attack_time -= 1;
        let distance = target_dist_sqr.sqrt();
        if self.attack_time == 0 {
            self.attack_time = self.interval_at(distance);
            mob.launch_projectile(arced_shot(self.kind, position, target, self.power));
        } else if self.attack_time < 0 {
            self.attack_time = self.interval_at(distance);
        }
    }
}

// -- BlazeFireballGoal -------------------------------------------------------

/// Vanilla's private `Blaze.BlazeAttackGoal` (`monster/Blaze.java:156-252`).
///
/// Not a `RangedAttackGoal`: a blaze fires in **bursts of three** on a fixed
/// cadence, and melees instead when very close. The state machine is `:216-233`
/// and the exact numbers are load-bearing:
///
/// | `attack_step` after increment | `attack_time` set to | fires? |
/// |---|---|---|
/// | 1 | 60 | no — this is the charge-up (`setCharged(true)`) |
/// | 2, 3, 4 | 6 | yes |
/// | 5 | 100, and step resets to 0 | no |
///
/// So a burst is **three fireballs six ticks apart**, after a 60-tick wind-up,
/// then a 100-tick pause. `attackStep > 1` is tested *after* the reset, which is
/// why step 5 fires nothing.
///
/// Within 4 blocks (`:206`) it melees on a 20-tick cooldown instead, via
/// [`MobController::attack`] — the same intent `MeleeAttackGoal` uses, so the
/// host's existing melee resolution picks it up with no extra wiring.
///
/// The triangle-distributed spread on each fireball (`:236-238`,
/// `random.triangle(xd, 2.297 * sqrt(sqrt(distance)) * 0.5)`) is not modelled,
/// the same disclosure [`ProjectileLaunch::aimed`] carries.
#[derive(Debug)]
pub struct BlazeFireballGoal {
    speed: f64,
    /// `Attributes.FOLLOW_RANGE`, `48.0` for a blaze (`monster/Blaze.java:55`).
    follow_range: f64,
    attack_step: i32,
    attack_time: i32,
}

/// A blaze melees below this distance rather than shooting (`Blaze.java:206`,
/// `distance < 4.0` — already a squared distance in the jar).
const BLAZE_MELEE_DIST_SQR: f64 = 4.0;

impl BlazeFireballGoal {
    /// The goal as a blaze registers it — no arguments in vanilla
    /// (`Blaze.java:45`, `new Blaze.BlazeAttackGoal(this)`); `speed` is the
    /// absolute figure behind `setWantedPosition(..., 1.0)` (`:212`, `:246`) and
    /// `follow_range` the blaze's own attribute.
    #[must_use]
    pub fn new(speed: f64, follow_range: f64) -> Self {
        Self {
            speed,
            follow_range,
            attack_step: 0,
            attack_time: 0,
        }
    }
}

impl Goal for BlazeFireballGoal {
    fn flags(&self) -> FlagSet {
        // `:164`.
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        // `:168-171` — `getTarget() != null && isAlive() && canAttack(target)`.
        mob.attack_target().is_some()
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        // `:174-176`.
        self.attack_step = 0;
    }

    fn stop(&mut self, _mob: &mut dyn MobController) {
        // `:179-182` — `setCharged(false)`, which for us is just the step.
        self.attack_step = 0;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        // `:191` — the decrement happens before anything else, every tick.
        self.attack_time -= 1;
        let Some(target) = mob.attack_target() else {
            return;
        };
        let position = mob.position();
        let distance_sq = distance_sqr(position, target);

        if distance_sq < BLAZE_MELEE_DIST_SQR {
            // `:206-215`.
            if self.attack_time <= 0 {
                self.attack_time = 20;
                mob.attack(target);
            }
            mob.move_to(target, self.speed);
        } else if distance_sq < self.follow_range * self.follow_range {
            // `:216-245`.
            if self.attack_time <= 0 {
                self.attack_step += 1;
                if self.attack_step == 1 {
                    self.attack_time = 60;
                } else if self.attack_step <= 4 {
                    self.attack_time = 6;
                } else {
                    self.attack_time = 100;
                    self.attack_step = 0;
                }

                // `:235` — tested *after* the reset above, so step 5 is silent.
                if self.attack_step > 1 {
                    // `:234-240`: the fireball leaves from `getY(0.5) + 0.5` and
                    // aims at the target's own mid-height, with no arc lift —
                    // a fireball flies flat and accelerates.
                    let origin = Vec3::new(
                        position.x,
                        position.y + SHOOTER_SHOULDER_Y,
                        position.z,
                    );
                    let dx = target.x - position.x;
                    let dy = (target.y + AIM_HEIGHT) - origin.y;
                    let dz = target.z - position.z;
                    mob.launch_projectile(ProjectileLaunch::aimed(
                        ProjectileKind::SmallFireball,
                        origin,
                        dx,
                        dy,
                        dz,
                        FIREBALL_POWER,
                    ));
                }
            }
            mob.look_at(target);
        } else {
            // `:246-247`.
            mob.move_to(target, self.speed);
        }
    }
}

// -- builders ----------------------------------------------------------------

/// `RangedBowAttackGoal<>(this, 1.0, 20, 15.0F)`
/// (`monster/skeleton/AbstractSkeleton.java:55`), registered at priority 4 by
/// `reassessWeaponGoal` (`:144`).
///
/// The interval argument is `20`, but `reassessWeaponGoal` overwrites it with
/// `getAttackInterval()` = **40** on anything below Hard difficulty (`:139-144`,
/// `:155-157`). 40 is the figure used here: nothing in this repo carries a world
/// difficulty, and Normal is the default a player meets.
///
/// `pub` because the skeleton's row lives in [`super::hostile_melee`].
#[must_use]
pub fn bow_attack(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RangedBowAttackGoal::new(ctx.speed * 1.0, 40, 15.0))
}

/// `Drowned.DrownedTridentAttackGoal(this, 1.0, 40, 10.0F)`
/// (`monster/zombie/Drowned.java:93`), a `RangedAttackGoal` subclass whose only
/// override is the animation (`:531-534`).
///
/// `pub` because the drowned's row lives in [`super::hostile_melee`].
#[must_use]
pub fn trident_attack(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RangedAttackGoal::new(
        ProjectileKind::Trident,
        ARROW_POWER,
        ctx.speed * 1.0,
        40,
        40,
        10.0,
    ))
}

/// `Blaze.BlazeAttackGoal(this)` (`monster/Blaze.java:45`). The `1.0` speed
/// multiplier is inside the goal (`:212`), and `48.0` is the blaze's own
/// `FOLLOW_RANGE` (`:55`).
fn blaze_fireball(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(BlazeFireballGoal::new(ctx.speed * 1.0, 48.0))
}

/// `RangedAttackGoal(this, 1.25, 20, 10.0F)`
/// (`animal/golem/SnowGolem.java:56`), throwing a snowball at `1.6F`
/// (`:129`).
fn snowball_attack(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RangedAttackGoal::new(
        ProjectileKind::Snowball,
        ARROW_POWER,
        ctx.speed * 1.25,
        20,
        20,
        10.0,
    ))
}

/// `RangedAttackGoal(this, 1.0, 60, 10.0F)` (`monster/Witch.java:68`), throwing a
/// splash potion.
///
/// The power is `Witch.performRangedAttack`'s own `dist <= 2.0 ? 0.45F : 0.75F`
/// (`:249`). `0.75` is used: the goal only fires while the witch is inside its
/// 10-block attack radius and closing, so the far branch is the one a player meets,
/// and this crate's `RangedAttackGoal` carries one power rather than a per-shot
/// function of distance.
///
/// **Two disclosed divergences, both about the potion rather than the throw.**
///
/// First, *which* potion is not modelled. Vanilla picks between harming, healing,
/// regeneration, slowness, poison and weakness from the target's own health,
/// distance and existing effects (`:229-244`) — a five-way branch over state a
/// `ProjectileKind` cannot carry, and one of whose arms even clears the target.
/// Every throw here is a plain `SplashPotion`.
///
/// Second, and the reason the first one costs less than it looks: **a splash potion
/// applies no effect on impact yet.** `impact_effect` gives it zero damage, and
/// there is no per-mob status-effect store for an area effect to land in — the
/// effect model that exists is the *player's* (`/effect`'s consumer). So a witch
/// currently throws a real projectile that really flies and really impacts, and the
/// impact does nothing. That is the honest state, and it is the seam a potion-cloud
/// pass plugs into rather than a hole in this table.
fn witch_potion(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RangedAttackGoal::new(
        ProjectileKind::SplashPotion,
        WITCH_POTION_POWER,
        ctx.speed * 1.0,
        60,
        60,
        10.0,
    ))
}

/// `Witch.performRangedAttack`'s far-distance throw power (`:249`,
/// `dist <= 2.0 ? 0.45F : 0.75F`).
const WITCH_POTION_POWER: f64 = 0.75;

/// `RangedCrossbowAttackGoal<>(this, 1.0, 8.0F)`
/// (`monster/illager/Pillager.java:74`), firing at `1.6F`
/// (`Pillager.performRangedAttack` → `performCrossbowAttack(this, 1.6F)`, `:198`).
///
/// Modelled with [`RangedAttackGoal`] rather than a new crossbow goal, and the
/// difference is worth stating: vanilla's `RangedCrossbowAttackGoal` has a
/// four-state machine (uncharged → charging → charged → ready) driven by the
/// crossbow item's own `CHARGED_PROJECTILES` component, which this repo has no item
/// component model for. `RangedAttackGoal`'s fixed interval stands in for the
/// charge cycle. The **projectile and its speed are exact**; the *cadence* is an
/// approximation, and the interval below is the crossbow's own charge duration
/// rather than a guess — see [`CROSSBOW_CHARGE_TICKS`].
fn crossbow_attack(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RangedAttackGoal::new(
        ProjectileKind::Arrow,
        ARROW_POWER,
        ctx.speed * 1.0,
        CROSSBOW_CHARGE_TICKS,
        CROSSBOW_CHARGE_TICKS,
        8.0,
    ))
}

/// `CrossbowItem.getChargeDuration`'s unenchanted value, used as the stand-in
/// cadence for [`crossbow_attack`]: `Mth.floor(MAX_CHARGE_DURATION * 20.0F)` with
/// `MAX_CHARGE_DURATION = 1.25F`, so **25** ticks.
///
/// A real number from the jar rather than a plausible round one. Reusing the bow's
/// `40` would make a pillager fire noticeably slower than vanilla, and `20` — one
/// second, the obvious guess — is 20% fast. Note the crossbow *item's* own
/// `ARROW_POWER` is `3.15F`, which is **not** the figure a mob uses:
/// `Pillager.performRangedAttack` calls `performCrossbowAttack(this, 1.6F)`, so the
/// launch speed is [`ARROW_POWER`]'s `1.6`. Picking `3.15` here would have made
/// pillager bolts hit twice as hard as vanilla's.
const CROSSBOW_CHARGE_TICKS: i32 = 25;

// -- tables ------------------------------------------------------------------

/// `monster/Blaze.java:44-52`. No `super.registerGoals()` call, so this is the
/// blaze's whole table.
pub const BLAZE: &[Registration] = &[
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal", nearest_attackable_target),
    Registration::goal(4, "Blaze.BlazeAttackGoal", blaze_fireball),
    // A blaze wanders back toward its spawn restriction point. Nothing in this
    // repo gives a mob a home position, so there is no approximation to make —
    // and unlike the stroll goal it is not a *simplification* of something we
    // have.
    Registration::missing(Selector::Goal, 5, "MoveTowardsRestrictionGoal"),
    Registration::goal(7, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(8, "LookAtPlayerGoal", look_at_player_8),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
];

/// `animal/golem/SnowGolem.java:54-61`. No `super.registerGoals()` call.
pub const SNOW_GOLEM: &[Registration] = &[
    // `NearestAttackableTargetGoal<>(this, Mob.class, 10, true, false, target -> target instanceof Enemy)`
    // (`:60`) — a snow golem hunts *hostile mobs*, not players. Our
    // `NearestAttackableTargetGoal` resolves through
    // `MobController::find_nearest_target`, which the server answers with the
    // nearest **player**, so substituting it here would make snow golems shoot
    // the player: not a simplification, an inversion. Per `super`'s rule, a
    // registration naming another class is a `Missing` row.
    //
    // The consequence is worth stating plainly: with no target feed, a snow
    // golem's `RangedAttackGoal` below cannot fire in production. It is
    // registered because vanilla registers it, and because the day
    // `find_nearest_target` learns about mob-vs-mob hostility (#221/#233) this
    // row starts working with no change here.
    Registration::missing(Selector::Target, 1, "NearestAttackableTargetGoal"),
    Registration::goal(1, "RangedAttackGoal", snowball_attack),
    Registration::goal(2, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(3, "LookAtPlayerGoal", look_at_player_6),
    Registration::goal(4, "RandomLookAroundGoal", random_look_around),
];

/// `monster/Witch.java:61-75`, **including** the `super.registerGoals()` chain the
/// first line calls: `Raider.registerGoals` (`raid/Raider.java:62-68`), which itself
/// calls `PatrollingMonster.registerGoals` (`:38-41`), which calls `Monster`'s.
///
/// Those inherited rows are the reason this table is mostly `Missing`, and the
/// reason the witch was left out of this family until now: every one of them is
/// raid or patrol machinery — a raid to path to, a village to move through, a leader
/// banner to obtain, a celebration to perform — and there is no raid system in this
/// repo to approximate. They are transcribed as `Missing` rather than omitted so a
/// gate comparing this table against the jar sees the whole `addGoal` set.
///
/// The ranged row itself is real, and it is what #227 asked for.
pub const WITCH: &[Registration] = &[
    // -- inherited from PatrollingMonster / Raider --
    Registration::missing(Selector::Goal, 4, "PatrollingMonster.LongDistancePatrolGoal"),
    Registration::missing(Selector::Goal, 1, "Raider.ObtainRaidLeaderBannerGoal"),
    Registration::missing(Selector::Goal, 3, "PathfindToRaidGoal"),
    Registration::missing(Selector::Goal, 4, "Raider.RaiderMoveThroughVillageGoal"),
    Registration::missing(Selector::Goal, 5, "Raider.RaiderCelebration"),
    // -- the witch's own --
    Registration::goal(1, "FloatGoal", float_goal),
    Registration::goal(2, "RangedAttackGoal", witch_potion),
    Registration::goal(2, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(3, "LookAtPlayerGoal", look_at_player_8),
    Registration::goal(3, "RandomLookAroundGoal", random_look_around),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    // `NearestHealableRaiderTargetGoal` — a witch heals *other raiders*, which
    // needs both a raid and mob-vs-mob targeting. Neither exists.
    Registration::missing(Selector::Target, 2, "NearestHealableRaiderTargetGoal"),
    // `NearestAttackableWitchTargetGoal` is a `NearestAttackableTargetGoal`
    // subclass whose only override suppresses targeting *while a raid is active
    // and the witch has not finished its wave* (`Witch.java`'s inner class). With
    // no raid, the override is inert and the base behaviour is exactly ours — so
    // this is `Modelled`, not `Missing`, and that is a claim about the subclass
    // rather than a convenient substitution.
    Registration::target(3, "NearestAttackableWitchTargetGoal", nearest_attackable_target),
];

/// `monster/illager/Pillager.java:69-82`, plus the same inherited
/// `Raider`/`PatrollingMonster` chain [`WITCH`] documents.
///
/// The crossbow row is [`crossbow_attack`]; read its doc comment for what is exact
/// (the projectile and its launch speed) and what is a stand-in (the charge-state
/// machine, replaced by a fixed interval).
pub const PILLAGER: &[Registration] = &[
    // -- inherited from PatrollingMonster / Raider --
    Registration::missing(Selector::Goal, 4, "PatrollingMonster.LongDistancePatrolGoal"),
    Registration::missing(Selector::Goal, 1, "Raider.ObtainRaidLeaderBannerGoal"),
    Registration::missing(Selector::Goal, 3, "PathfindToRaidGoal"),
    Registration::missing(Selector::Goal, 4, "Raider.RaiderMoveThroughVillageGoal"),
    Registration::missing(Selector::Goal, 5, "Raider.RaiderCelebration"),
    // -- the pillager's own --
    Registration::goal(0, "FloatGoal", float_goal),
    // `AvoidEntityGoal<Creaking>` (`:72`). Ours resolves the avoided species
    // through the host's own feed, the same route the creeper's cat/ocelot
    // avoidance takes.
    Registration::goal(1, "AvoidEntityGoal", avoid_entity),
    // `Raider.HoldGroundAttackGoal` — the raid-wave "stand and fight at the
    // village bell" behaviour. Raid machinery again.
    Registration::missing(Selector::Goal, 2, "Raider.HoldGroundAttackGoal"),
    Registration::goal(3, "RangedCrossbowAttackGoal", crossbow_attack),
    // `RandomStrollGoal(this, 0.6)` (`:75`) — note this is the plain stroll, not
    // the water-avoiding one the witch gets, and vanilla's speed factor is 0.6.
    // Ours is one goal for both, so the row is `Modelled` with the factor visible
    // at `stroll`'s own definition rather than here.
    Registration::goal(8, "RandomStrollGoal", stroll),
    Registration::goal(9, "LookAtPlayerGoal", look_at_player_8),
    // The second `LookAtPlayerGoal` at priority 10 targets `Mob`, not `Player`
    // (`:77`) — a different class, so per this family's own rule it is a row
    // covered by the one above rather than a second instance fighting it for LOOK.
    Registration::covered(Selector::Goal, 10, "LookAtPlayerGoal", "LookAtPlayerGoal"),
    Registration::target(1, "HurtByTargetGoal", hurt_by_target),
    Registration::target(2, "NearestAttackableTargetGoal", nearest_attackable_target),
    // The two priority-3 target rows name `AbstractVillager` and `IronGolem`
    // (`:80-81`). Both are mob-vs-mob targeting, which `find_nearest_target`
    // answers with the nearest *player* — substituting ours would make a pillager
    // shoot the player under a villager's priority, which is not a simplification
    // but a duplicate of the row above. `Missing`, for the same reason the snow
    // golem's target row is.
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal"),
    Registration::missing(Selector::Target, 3, "NearestAttackableTargetGoal"),
];

/// The registry path a [`ProjectileKind`] spawns as.
///
/// Lives here rather than in the host so that the host's drain is a single
/// `spawn_projectile(rk(projectile_entity_type(launch.kind)), …)` call with no
/// decisions in it, and so the entity-type names sit next to the jar lines that
/// chose them. Paths (not full keys) because everything in this family is
/// `minecraft:`-namespaced and `ResourceKey` lives in `lodestone-model`, which
/// the AI module does not need to reach for one string.
///
/// A `Trident` spawns as `minecraft:trident` — the *thrown entity* shares its
/// name with the item, unlike `SplashPotion`, whose entity is
/// `minecraft:splash_potion` while `ThrownSplashPotion` is the class
/// (`monster/Witch.java:248`).
#[must_use]
pub const fn projectile_entity_type(kind: ProjectileKind) -> &'static str {
    match kind {
        ProjectileKind::Arrow => "arrow",
        ProjectileKind::SmallFireball => "small_fireball",
        ProjectileKind::Snowball => "snowball",
        ProjectileKind::SplashPotion => "splash_potion",
        ProjectileKind::Trident => "trident",
    }
}

/// Whether a [`ProjectileKind`] integrates as an arrow or as a throwable.
///
/// The two families apply their per-tick steps in a **different order**, and
/// `projectile.rs`'s own module doc is emphatic that getting it wrong drifts the
/// landing point: arrows are move → drag → gravity, throwables are gravity →
/// drag → move. A host drain picks
/// `lodestone_entity::projectile::Projectile::arrow` when this is `true` and
/// `::throwable` when it is `false`.
///
/// A small fireball is **neither** in vanilla — `AbstractHurtingProjectile`
/// *accelerates* instead of falling (`:126`) — so it is reported as a throwable,
/// the closer of the two, and its trajectory is wrong past the first few ticks.
/// Named here rather than left implicit because it is a real inaccuracy that the
/// launch velocity being jar-exact does not fix.
#[must_use]
pub const fn integrates_as_arrow(kind: ProjectileKind) -> bool {
    match kind {
        ProjectileKind::Arrow | ProjectileKind::Trident => true,
        ProjectileKind::SmallFireball | ProjectileKind::Snowball | ProjectileKind::SplashPotion => {
            false
        }
    }
}

/// Every species this family claims.
///
/// **Not the skeleton family, and not the drowned**: `Skeleton`, `Stray`,
/// `Bogged`, `Parched` and `Drowned` all resolve through
/// [`super::hostile_melee`], which claimed them first because each has a melee
/// half too. Their bow/trident rows are [`bow_attack`] and [`trident_attack`]
/// registered from that file.
///
/// **Not the ghast** — its fireball feeds `explosion.rs` and it belongs to the
/// specialist family (#232).
///
/// **The witch and the pillager are here now.** This list used to exclude them with
/// the reasoning that both extend `Raider`, whose `registerGoals` chain adds raid
/// machinery this repo has none of, "so their tables are mostly `Missing` rows about
/// a raid system rather than about ranged attacks". Both halves of that were true
/// and neither was a reason to omit the species: the inherited raid rows are
/// transcribed as `Missing` (which is what that coverage variant is *for*), and the
/// ranged rows underneath them — [`witch_potion`] and [`crossbow_attack`] — are real
/// and are exactly what a player meets outside a raid. Leaving them out meant a
/// witch and a pillager fell through to the fallback table and had no ranged attack
/// at all.
pub const SPECIES: &[&str] = &["blaze", "snow_golem", "witch", "pillager"];

/// Resolves a species path to its table, or `None` if this family does not claim
/// it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        "blaze" => Some(BLAZE),
        "snow_golem" => Some(SNOW_GOLEM),
        "witch" => Some(WITCH),
        "pillager" => Some(PILLAGER),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::ai::goal::GoalSelector;
    use crate::ai::goals_for;
    use crate::ai::navigating_mob::NavigatingMob;
    use crate::ai::roster::probe::SpeedProbe;
    use crate::pathfinding::{Aabb, MobShape, PathType, PathWorld};

    /// Flat solid ground below `y = 0`, open above. The minimum a real
    /// [`NavigatingMob`] needs to exist and walk.
    struct Flat;

    impl PathWorld for Flat {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if y <= -1 { PathType::Blocked } else { PathType::Open }
        }
        fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
            if y <= -1 { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            aabb.min_y < 0.0
        }
    }

    /// A real production controller — **not `ScriptMob`**. Issue #441's island
    /// was eight goals with green `ScriptMob` tests and a constant-false
    /// `can_use` in production, and the only thing that separates the two is the
    /// *type* under the `&mut dyn MobController`.
    fn real_mob<'w>(world: &'w dyn PathWorld, at: Vec3, speed: f64) -> NavigatingMob<'w> {
        NavigatingMob::new(world, MobShape::land(0.6, 1.95), at, speed, 400, 0)
    }

    // -- the seam itself -----------------------------------------------------

    #[test]
    fn a_real_navigating_mob_records_a_bow_launch() {
        let world = Flat;
        // Skeleton speed 0.25 (`AbstractSkeleton.java:91`), target 8 blocks out
        // — inside the bow goal's 15.0 radius.
        let mut mob = real_mob(&world, Vec3::new(0.0, 0.0, 0.0), 0.25);
        let target = Vec3::new(8.0, 0.0, 0.0);
        MobController::set_attack_target(&mut mob, Some(target));

        let mut goal = RangedBowAttackGoal::new(0.25, 40, 15.0);
        assert!(
            goal.can_use(&mut mob),
            "a fed NavigatingMob must satisfy the bow goal's can_use; if this \
             fails, the goal is issue #441's island shape again"
        );

        // First tick starts the draw (attack_time -1 -> -2 <= 0), then 20 ticks
        // of pull before the release.
        for _ in 0..=BOW_FULL_DRAW_TICKS {
            goal.tick(&mut mob);
        }

        let launches = mob.take_new_launches();
        assert_eq!(
            launches.len(),
            1,
            "exactly one arrow after a full draw, got {launches:?}"
        );
        assert_eq!(launches[0].kind, ProjectileKind::Arrow);

        // The predicted value, from outside this file: the shot leaves at
        // (0, 1.4, 0), the aim point is (8, 0.6, 0), so dx = 8, dz = 0,
        // dy = 0.6 - 1.4 = -0.8, horizontal = 8, arc lift = 8 * 0.2 = 1.6,
        // giving an aim vector of (8, 0.8, 0) whose length is 8.0399...
        // Scaled to power 1.6 that is a speed of exactly 1.6.
        let v = launches[0].velocity;
        let speed = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
        assert!(
            (speed - ARROW_POWER).abs() < 1e-9,
            "an arrow leaves at exactly power 1.6 (AbstractSkeleton.java:170), got {speed}"
        );
        // The arc lift is the part a direction-only assertion could not see: a
        // goal that forgot `+ horizontal * 0.2` aims *down* at this range
        // (dy = -0.8), so the sign of y alone separates the two hypotheses.
        assert!(
            v.y > 0.0,
            "the ballistic arc must aim above the target at 8 blocks; \
             without ARC_LIFT this is negative. got {v:?}"
        );
        let expected_len = (8.0f64 * 8.0 + 0.8 * 0.8).sqrt();
        let expected = Vec3::new(
            8.0 / expected_len * ARROW_POWER,
            0.8 / expected_len * ARROW_POWER,
            0.0,
        );
        assert!(
            (v.x - expected.x).abs() < 1e-9
                && (v.y - expected.y).abs() < 1e-9
                && (v.z - expected.z).abs() < 1e-9,
            "predicted {expected:?} from the jar's own maths, got {v:?}"
        );
    }

    /// The negative control for the test above: identical construction and the
    /// same number of ticks, with only the target withheld. If this ever
    /// records a launch, the assertion above is measuring something other than
    /// the goal firing.
    #[test]
    fn an_unfed_navigating_mob_records_no_bow_launch() {
        let world = Flat;
        let mut mob = real_mob(&world, Vec3::new(0.0, 0.0, 0.0), 0.25);
        let mut goal = RangedBowAttackGoal::new(0.25, 40, 15.0);
        assert!(
            !goal.can_use(&mut mob),
            "no target means no bow goal"
        );
        for _ in 0..=BOW_FULL_DRAW_TICKS {
            goal.tick(&mut mob);
        }
        assert!(
            mob.launches().is_empty(),
            "a mob with no target must not shoot, got {:?}",
            mob.launches()
        );
    }

    // -- the blaze burst -----------------------------------------------------

    #[test]
    fn a_blaze_fires_three_fireballs_per_burst_on_the_jars_cadence() {
        let world = Flat;
        // Blaze speed 0.23 (`Blaze.java:55`). Target 10 blocks out: past the
        // 4-block melee threshold, inside the 48-block follow range.
        let mut mob = real_mob(&world, Vec3::new(0.0, 0.0, 0.0), 0.23);
        let target = Vec3::new(10.0, 0.0, 0.0);
        MobController::set_attack_target(&mut mob, Some(target));
        let mut goal = BlazeFireballGoal::new(0.23, 48.0);
        assert!(goal.can_use(&mut mob), "a fed blaze must satisfy can_use");
        goal.start(&mut mob);

        // Tick counts at which a fireball appeared, so cadence is measured
        // rather than just the total.
        let mut fired_at = Vec::new();
        for tick in 1..=200 {
            goal.tick(&mut mob);
            for _ in mob.take_new_launches() {
                fired_at.push(tick);
            }
        }

        // Predicted from `Blaze.java:216-233`, computed here rather than copied
        // from a run: tick 1 sets step 1 / attack_time 60 and fires nothing.
        // attack_time then reaches 0 at tick 61, which fires (step 2) and sets
        // 6; tick 67 fires (step 3); tick 73 fires (step 4); tick 79 sets step
        // 5 -> resets to 0 and 100, firing nothing; tick 179 begins the next
        // burst's charge, and tick 200 is before its first shot.
        assert_eq!(
            fired_at,
            vec![61, 67, 73],
            "a blaze burst is three fireballs six ticks apart after a 60-tick \
             wind-up (Blaze.java:216-233). A goal that fired on step 1, or on \
             step 5 after the reset, gives four or five here."
        );
    }

    #[test]
    fn a_blaze_fireball_leaves_at_its_acceleration_power_not_an_arrows() {
        let world = Flat;
        let mut mob = real_mob(&world, Vec3::new(0.0, 0.0, 0.0), 0.23);
        MobController::set_attack_target(&mut mob, Some(Vec3::new(10.0, 0.0, 0.0)));
        let mut goal = BlazeFireballGoal::new(0.23, 48.0);
        goal.start(&mut mob);
        let mut first = None;
        for _ in 1..=70 {
            goal.tick(&mut mob);
            if let Some(l) = mob.take_new_launches().first().copied() {
                first = Some(l);
                break;
            }
        }
        let launch = first.expect("a blaze must have fired within 70 ticks");
        assert_eq!(launch.kind, ProjectileKind::SmallFireball);
        let v = launch.velocity;
        let speed = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
        // The two competing hypotheses, both computed from outside this file:
        // `AbstractHurtingProjectile.accelerationPower` 0.1 (`:24`, `:180-183`)
        // and an arrow's 1.6 (`AbstractSkeleton.java:170`). They differ by 16x,
        // so landing on one refutes the other.
        assert!(
            (speed - FIREBALL_POWER).abs() < 1e-9,
            "a small fireball leaves at 0.1 (AbstractHurtingProjectile.java:24), \
             not an arrow's 1.6; got {speed}"
        );
        // A fireball has no arc lift, so at equal feet-height with the aim point
        // below the muzzle the y component is negative — the opposite sign to
        // the arrow test above, and the thing that would break if `arced_shot`
        // were reused here by mistake.
        assert!(
            v.y < 0.0,
            "a fireball flies flat, so aiming at a target below the muzzle must \
             give a negative y; got {v:?}"
        );
    }

    // -- speeds, by value ----------------------------------------------------

    /// A priority multiset gate cannot see a wrong *speed*, and neither can "the
    /// mob moved toward its target". Each figure below is the jar's multiplier
    /// times the species' `MOVEMENT_SPEED` attribute, computed here.
    ///
    /// The per-case distance is load-bearing, and finding that out was worth the
    /// trip: at the probe's default 4 blocks a **blaze records no `move_to` at
    /// all**, because 4 blocks is inside its follow range and outside its
    /// 2-block melee radius, so vanilla has it stand still and shoot
    /// (`Blaze.java:216-245`). Reading that as "the builder passes no speed"
    /// would have been wrong; the speed is only observable in the branches that
    /// actually walk.
    #[test]
    fn every_ranged_builder_passes_the_jars_speed_multiplier() {
        // (builder, ctx speed, expected move_to speed, target distance, why)
        let cases: [(fn(&SpeciesContext) -> Box<dyn Goal>, f64, f64, f64, &str); 4] = [
            // `RangedBowAttackGoal<>(this, 1.0, ...)` × skeleton 0.25
            // (`AbstractSkeleton.java:55`, `:91`). Walks while `seeTime < 20`.
            (bow_attack, 0.25, 0.25, 4.0, "skeleton bow, 1.0 x 0.25"),
            // `Blaze.BlazeAttackGoal` uses 1.0 internally (`Blaze.java:212`) ×
            // blaze 0.23 (`:55`). Only walks inside the melee radius (< 2
            // blocks) or beyond follow range.
            (blaze_fireball, 0.23, 0.23, 1.0, "blaze melee approach, 1.0 x 0.23"),
            // `RangedAttackGoal(this, 1.25, ...)` (`SnowGolem.java:56`) × snow
            // golem 0.2 (`:64`). Walks while `seeTime < 5`.
            (snowball_attack, 0.2, 0.25, 4.0, "snow golem, 1.25 x 0.2"),
            // `DrownedTridentAttackGoal(this, 1.0, ...)` (`Drowned.java:93`) ×
            // drowned 0.23.
            (trident_attack, 0.23, 0.23, 4.0, "drowned trident, 1.0 x 0.23"),
        ];

        for (build, speed, expected, distance, why) in cases {
            let ctx = SpeciesContext::new(speed);
            let mut goal = build(&ctx);
            let mut probe = SpeedProbe::new();
            probe.nearby = Vec3::new(distance, 0.0, 0.0);
            assert!(goal.can_use(&mut probe), "{why}: goal refused the probe");
            goal.start(&mut probe);
            for _ in 0..3 {
                goal.tick(&mut probe);
            }
            let got = probe
                .first_speed()
                .unwrap_or_else(|| panic!("{why}: goal never called move_to"));
            assert!(
                (got - expected).abs() < 1e-9,
                "{why}: expected {expected}, got {got}"
            );
        }
    }

    // -- the tables ----------------------------------------------------------

    /// The priority multiset, against the jar. Every row, whatever its coverage
    /// — a table that silently dropped `MoveTowardsRestrictionGoal` would still
    /// build a working blaze, and this is what refuses it.
    #[test]
    fn every_table_matches_its_cited_addgoal_multiset() {
        // (species, file:line cite, expected (selector, priority, class) rows)
        let blaze_expected = vec![
            (Selector::Goal, 4, "Blaze.BlazeAttackGoal"),
            (Selector::Goal, 5, "MoveTowardsRestrictionGoal"),
            (Selector::Goal, 7, "WaterAvoidingRandomStrollGoal"),
            (Selector::Goal, 8, "LookAtPlayerGoal"),
            (Selector::Goal, 8, "RandomLookAroundGoal"),
            (Selector::Target, 1, "HurtByTargetGoal"),
            (Selector::Target, 2, "NearestAttackableTargetGoal"),
        ];
        let snow_golem_expected = vec![
            (Selector::Goal, 1, "RangedAttackGoal"),
            (Selector::Goal, 2, "WaterAvoidingRandomStrollGoal"),
            (Selector::Goal, 3, "LookAtPlayerGoal"),
            (Selector::Goal, 4, "RandomLookAroundGoal"),
            (Selector::Target, 1, "NearestAttackableTargetGoal"),
        ];

        // The witch's own five goal rows and three target rows, plus the five
        // inherited `Raider`/`PatrollingMonster` rows its `super.registerGoals()`
        // pulls in. The inherited rows are the point: a table that transcribed only
        // the witch's own `addGoal` calls would look complete and would be missing
        // five, and no behavioural test could see the difference because all five
        // are `Missing` anyway.
        let witch_expected = vec![
            (Selector::Goal, 4, "PatrollingMonster.LongDistancePatrolGoal"),
            (Selector::Goal, 1, "Raider.ObtainRaidLeaderBannerGoal"),
            (Selector::Goal, 3, "PathfindToRaidGoal"),
            (Selector::Goal, 4, "Raider.RaiderMoveThroughVillageGoal"),
            (Selector::Goal, 5, "Raider.RaiderCelebration"),
            (Selector::Goal, 1, "FloatGoal"),
            (Selector::Goal, 2, "RangedAttackGoal"),
            (Selector::Goal, 2, "WaterAvoidingRandomStrollGoal"),
            (Selector::Goal, 3, "LookAtPlayerGoal"),
            (Selector::Goal, 3, "RandomLookAroundGoal"),
            (Selector::Target, 1, "HurtByTargetGoal"),
            (Selector::Target, 2, "NearestHealableRaiderTargetGoal"),
            (Selector::Target, 3, "NearestAttackableWitchTargetGoal"),
        ];
        let pillager_expected = vec![
            (Selector::Goal, 4, "PatrollingMonster.LongDistancePatrolGoal"),
            (Selector::Goal, 1, "Raider.ObtainRaidLeaderBannerGoal"),
            (Selector::Goal, 3, "PathfindToRaidGoal"),
            (Selector::Goal, 4, "Raider.RaiderMoveThroughVillageGoal"),
            (Selector::Goal, 5, "Raider.RaiderCelebration"),
            (Selector::Goal, 0, "FloatGoal"),
            (Selector::Goal, 1, "AvoidEntityGoal"),
            (Selector::Goal, 2, "Raider.HoldGroundAttackGoal"),
            (Selector::Goal, 3, "RangedCrossbowAttackGoal"),
            (Selector::Goal, 8, "RandomStrollGoal"),
            (Selector::Goal, 9, "LookAtPlayerGoal"),
            (Selector::Goal, 10, "LookAtPlayerGoal"),
            (Selector::Target, 1, "HurtByTargetGoal"),
            (Selector::Target, 2, "NearestAttackableTargetGoal"),
            (Selector::Target, 3, "NearestAttackableTargetGoal"),
            (Selector::Target, 3, "NearestAttackableTargetGoal"),
        ];

        for (species, cite, expected) in [
            ("blaze", "monster/Blaze.java:44-52", blaze_expected),
            (
                "snow_golem",
                "animal/golem/SnowGolem.java:54-61",
                snow_golem_expected,
            ),
            (
                "witch",
                "monster/Witch.java:61-75 plus raid/Raider.java:62-68 and \
                 monster/PatrollingMonster.java:38-41",
                witch_expected,
            ),
            (
                "pillager",
                "monster/illager/Pillager.java:69-82 plus the same inherited chain",
                pillager_expected,
            ),
        ] {
            let table = lookup(species).unwrap_or_else(|| panic!("{species} has no table"));
            let mut got: Vec<_> = table
                .iter()
                .map(|r| (r.selector, r.priority, r.vanilla))
                .collect();
            let mut want = expected;
            got.sort_by_key(|&(s, p, v)| (format!("{s:?}"), p, v));
            want.sort_by_key(|&(s, p, v)| (format!("{s:?}"), p, v));
            assert_eq!(
                got, want,
                "{species}'s table does not match {cite}. A row here is one \
                 `addGoal` call in that range — if the jar disagrees, the jar wins."
            );
        }
    }

    /// Both species must install a real brain through the production entry
    /// point, not just hold a plausible table. `goals_for` is what
    /// `MobSim::spawn_species` calls.
    #[test]
    fn both_species_install_goals_through_the_production_entry_point() {
        // The witch builds 7 of its 13 rows and the pillager 7 of its 16 — every
        // `Modelled` row and no `Missing` or `CoveredBy` one, which is what makes the
        // raid rows honest bookkeeping rather than silently-registered no-ops. The
        // pillager's second `LookAtPlayerGoal` is `CoveredBy`, so 16 rows minus 8
        // uncovered gives 7 and not 8.
        for (species, expected_built) in
            [("blaze", 6), ("snow_golem", 4), ("witch", 7), ("pillager", 7)]
        {
            let ctx = SpeciesContext::new(0.23);
            let built = goals_for(species, &ctx);
            assert_eq!(
                built.len(),
                expected_built,
                "{species} must build every Modelled row and no Missing one"
            );
            // And they must actually load into a selector at those priorities.
            let mut selector = GoalSelector::new();
            for (priority, goal) in built {
                selector.add(priority, goal);
            }
            assert_eq!(selector.len(), expected_built);
        }
    }

    /// A blaze registers two goals at priority 8 (`Blaze.java:48-49`), which is
    /// legal in vanilla and must stay legal here: `LookAtPlayerGoal` claims LOOK
    /// and `RandomLookAroundGoal` claims LOOK, so the second is simply
    /// preempted, not rejected. A `GoalSelector` that deduplicated by priority
    /// would silently drop one.
    #[test]
    fn a_duplicate_priority_keeps_both_goals() {
        let ctx = SpeciesContext::new(0.23);
        let at_eight = goals_for("blaze", &ctx)
            .into_iter()
            .filter(|(p, _)| *p == 8)
            .count();
        assert_eq!(at_eight, 2, "both priority-8 blaze goals must survive");
    }

    /// Every name [`projectile_entity_type`] returns must be a **real** entity
    /// type, checked against `lodestone-data`'s generated registry — Mojang's
    /// own `registries.json`, an expected value from outside this tree.
    ///
    /// This is not pedantry about strings. `encode_add_entity_body` resolves the
    /// type with `entity_type_id(name).unwrap_or(0)`
    /// (`crates/protocol/v770/src/server_protocol.rs:1117`), and id `0` is a
    /// real entity — so a typo here does not fail, it silently streams **the
    /// wrong entity** to the client, which renders it happily. There is no error
    /// anywhere in that path.
    #[test]
    fn every_projectile_kind_names_a_real_entity_type() {
        let kinds = [
            ProjectileKind::Arrow,
            ProjectileKind::SmallFireball,
            ProjectileKind::Snowball,
            ProjectileKind::SplashPotion,
            ProjectileKind::Trident,
        ];
        for kind in kinds {
            let path = projectile_entity_type(kind);
            let full = format!("minecraft:{path}");
            let id = lodestone_data::entity_types::entity_type_id(&full);
            assert!(
                id.is_some(),
                "{kind:?} names `{full}`, which is not in the generated entity-type \
                 registry. A wrong name here does not fail — `encode_add_entity_body` \
                 falls back to id 0 and the client renders the wrong entity."
            );
            assert_ne!(
                id,
                Some(0),
                "{kind:?} resolves to id 0, indistinguishable from the \
                 `unwrap_or(0)` fallback that means 'unknown'"
            );
        }
        // The control: the same lookup must reject a name that does not exist,
        // or the assertions above are satisfied by a census that says yes to
        // everything.
        assert!(
            lodestone_data::entity_types::entity_type_id("minecraft:not_a_projectile").is_none(),
            "the entity-type census accepts a made-up name, so the checks above \
             measure nothing"
        );
    }

    /// Nothing in this family may claim a species another family already owns —
    /// the failure mode of five roster units adding arms in parallel. `super`'s
    /// own gate covers this globally; this one names the neighbour, because the
    /// skeleton family really is the tempting mistake here.
    #[test]
    fn this_family_claims_no_species_the_skeleton_family_owns() {
        let ours: HashSet<&str> = SPECIES.iter().copied().collect();
        for species in super::super::hostile_melee::SPECIES {
            assert!(
                !ours.contains(species),
                "{species} is claimed by both ranged and hostile_melee; the \
                 skeleton family's bow row belongs in hostile_melee's own table"
            );
        }
        // And the reverse direction: our own species must resolve to *our*
        // tables, not be shadowed by an earlier family in FAMILIES.
        for species in SPECIES {
            let resolved = super::super::registrations_for(species);
            let ours = lookup(species).expect("SPECIES and lookup must agree");
            assert!(
                std::ptr::eq(resolved.as_ptr(), ours.as_ptr()),
                "{species} resolves to another family's table"
            );
        }
    }

    /// A `GoalSelector` is what actually runs a goal, and `remove` is what
    /// `AbstractSkeleton.reassessWeaponGoal` needs (`GoalSelector.java:42-44`).
    /// A bow goal removed mid-flight must stop cleanly and stop shooting.
    #[test]
    fn removing_the_bow_goal_stops_the_shooting() {
        let world = Flat;
        let mut mob = real_mob(&world, Vec3::new(0.0, 0.0, 0.0), 0.25);
        MobController::set_attack_target(&mut mob, Some(Vec3::new(8.0, 0.0, 0.0)));
        let mut selector = GoalSelector::new();
        let id = selector.add(4, Box::new(RangedBowAttackGoal::new(0.25, 40, 15.0)));

        // `NavigatingMob::tick` is the production driver — the exact call
        // `MobSim::tick` makes (`mobs.rs`, `m.mob.tick(&mut m.goals)`).
        for _ in 0..30 {
            mob.tick(&mut selector);
        }
        let before = mob.take_new_launches().len();
        assert!(before >= 1, "the bow goal must fire while registered");

        assert!(
            selector.remove(id, &mut mob),
            "GoalSelector::remove must report the removal"
        );
        for _ in 0..60 {
            mob.tick(&mut selector);
        }
        assert!(
            mob.launches().is_empty(),
            "a removed bow goal must not shoot; got {:?}",
            mob.launches()
        );
    }
}
