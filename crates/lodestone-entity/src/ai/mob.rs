//! The interface goals drive.
//!
//! Vanilla goals operate on a `Mob`, reaching into its navigation, look control,
//! jump control and random source. Reproducing the whole `Mob` here would drag
//! in the world and physics; instead [`MobController`] is a narrow seam of the
//! *intents* goals actually express (move toward a point, look at a target,
//! jump, perceive the nearest player). A host wires these to the real navigator,
//! physics and world. This keeps the AI module about **scheduler semantics** —
//! which the design brief calls out as the thing that matters — rather than
//! about re-deriving movement.

use lodestone_model::Vec3;

use crate::pathfinding::BlockCues;

/// The mob-facing operations a [`Goal`](crate::ai::Goal) may perform.
///
/// All methods take `&mut self` because goals both observe and command the mob;
/// a host implementation typically holds the entity state, a
/// [`PathNavigator`](crate::pathfinding::PathNavigator) and an RNG.
pub trait MobController {
    /// A uniform random `f32` in `[0, 1)` (vanilla's `random.nextFloat`).
    fn next_f32(&mut self) -> f32;

    /// A uniform random `i32` in `[0, bound)` (vanilla's `random.nextInt`).
    fn next_i32(&mut self, bound: i32) -> i32;

    /// A uniform random `f64` in `[0, 1)`.
    fn next_f64(&mut self) -> f64;

    /// The mob's current position.
    fn position(&self) -> Vec3;

    /// Whether the mob is in water.
    fn in_water(&self) -> bool {
        false
    }

    /// Whether the mob is in lava.
    fn in_lava(&self) -> bool {
        false
    }

    /// Ticks the mob has spent taking no deliberate action (vanilla's
    /// `getNoActionTime`), used by strolling to yield when idle-throttled.
    fn no_action_time(&self) -> i32 {
        0
    }

    /// Commands the navigation to move toward `target` at `speed`. Returns
    /// whether a path was found (vanilla's `navigation.moveTo`).
    fn move_to(&mut self, target: Vec3, speed: f64) -> bool;

    /// Whether the navigation has finished or has no path.
    fn navigation_done(&self) -> bool;

    /// Stops the navigation.
    fn stop_navigation(&mut self);

    /// Requests the jump control to jump this tick.
    fn set_jumping(&mut self, jumping: bool);

    /// Points the look control at a world position.
    fn look_at(&mut self, target: Vec3);

    /// Sets the desired look direction from a horizontal offset (used by the
    /// random-look goal).
    fn look_toward(&mut self, dx: f64, dz: f64);

    /// The nearest player's position, if one is within perception range.
    fn nearest_player(&self) -> Option<Vec3> {
        None
    }

    /// A candidate wander destination (vanilla's `DefaultRandomPos.getPos`).
    /// Returning `None` means no valid spot was found this attempt.
    fn random_stroll_target(&mut self) -> Option<Vec3>;

    /// The current attack target's position, if the mob has one.
    fn attack_target(&self) -> Option<Vec3> {
        None
    }

    /// Sets (or clears, with `None`) the mob's attack target. Target-selection
    /// goals call this; movement goals read it back via [`attack_target`].
    ///
    /// [`attack_target`]: MobController::attack_target
    fn set_attack_target(&mut self, target: Option<Vec3>) {
        let _ = target;
    }

    /// The nearest position the mob considers an attackable target — the host
    /// applies the version/type-specific filter (hostility, follow range, line
    /// of sight). Drives `NearestAttackableTargetGoal`.
    ///
    /// **A host that returns [`attack_target`](MobController::attack_target)
    /// here has written an island, not an implementation** (issue #455): the
    /// goal that calls this is the same goal that writes `attack_target` in its
    /// `start`, so the loop cannot bootstrap and the mob never attacks
    /// unprovoked. Whatever the host's perception feed is, this must read
    /// *that*. `NavigatingMob::find_nearest_target` documents where each of the
    /// three filters ended up.
    fn find_nearest_target(&mut self) -> Option<Vec3> {
        None
    }

    /// This mob's `FOLLOW_RANGE` attribute value, in blocks.
    ///
    /// Vanilla reads it in two places with the *same* number:
    /// `NearestAttackableTargetGoal` acquires within it
    /// (`ai/goal/target/TargetGoal.java:74-76`, `getFollowDistance`) and
    /// `TargetGoal.canContinueToUse` **drops a target that leaves it**
    /// (`TargetGoal.java:57-60`, `distanceToSqr(target) > within * within`).
    /// The default is `Mob.createMobAttributes()`' `16.0`
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/Mob.java:166-168`), so a
    /// controller that does not track the attribute still releases targets at a
    /// vanilla-plausible distance rather than chasing one forever.
    fn follow_range(&self) -> f64 {
        16.0
    }

    /// The position of the entity that most recently damaged this mob, within
    /// the retaliation window. Drives `HurtByTargetGoal`.
    fn last_hurt_by(&self) -> Option<Vec3> {
        None
    }

    /// The position of the entity this mob currently holds a **persistent
    /// grudge** against, or `None` when its anger has expired or never started.
    ///
    /// This is the third hostility state, and it is why a per-species boolean
    /// would be wrong. Vanilla has always-hostile mobs (zombie, creeper), never-
    /// hostile ones (cow), and *neutral* ones — zombified piglin, wolf, bee,
    /// enderman — whose target registration ends in a `this::isAngryAt` selector
    /// (`NeutralMob.isAngryAt`), which narrows the candidate set to the one
    /// entity the grudge names. A neutral mob with no grudge has an empty
    /// candidate set, which is what makes it neutral;
    /// [`NearestAttackableTargetGoal::anger_gated`](crate::ai::goals::NearestAttackableTargetGoal::anger_gated)
    /// is the registration shape that reads this.
    ///
    /// **The host owns the clock, on purpose.** 26.2 stores an **absolute
    /// game-time deadline**, not a countdown
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/NeutralMob.java:20-22`,
    /// `:112-120`; `NO_ANGER_END_TIME = -1`), and the grudge is a uniform
    /// `[400, 780]` ticks for all four species (`rangeOfSeconds(20, 39)` →
    /// `UniformInt.of(400, 780)`). A decrementing counter is the wrong model —
    /// it drifts against a stepped tick loop — and this seam has no shared game
    /// clock to compare a deadline against, so expiry is resolved by the host
    /// and only the *answer* crosses. That is the same division as
    /// [`find_love_partner`](MobController::find_love_partner).
    fn angry_target(&self) -> Option<Vec3> {
        None
    }

    /// The position of a nearby entity currently tempting this mob (e.g. a
    /// player holding food). Drives `TemptGoal`.
    fn temptation(&self) -> Option<Vec3> {
        None
    }

    /// Whether this mob is a baby (`Age < 0` for animals). Gates
    /// `FollowParentGoal`.
    fn is_baby(&self) -> bool {
        false
    }

    /// The position of the nearest adult of the same kind, if one is in range.
    /// Drives `FollowParentGoal`.
    fn parent_position(&self) -> Option<Vec3> {
        None
    }

    /// Performs a melee attack against `target`.
    fn attack(&mut self, target: Vec3) {
        let _ = target;
    }

    /// A position of a nearby entity the mob wants to avoid, if any.
    fn avoid_threat(&self) -> Option<Vec3> {
        None
    }

    /// Whether the mob is currently panicking (e.g. was just hurt).
    fn is_panicking(&self) -> bool {
        false
    }

    /// Whether this animal is in "love mode" (fed a breeding item and looking
    /// for a mate). Gates [`BreedGoal`](crate::ai::goals::BreedGoal).
    fn is_in_love(&self) -> bool {
        false
    }

    /// Selects and remembers a free breeding partner — another in-love animal of
    /// the same kind, within range and not panicking — returning its position if
    /// one was found. Mirrors vanilla's `getFreePartner`: the host performs the
    /// version/type-specific `canMate` filter and holds the chosen partner so
    /// [`love_partner_position`] can track it.
    ///
    /// [`love_partner_position`]: MobController::love_partner_position
    fn find_love_partner(&mut self) -> Option<Vec3> {
        None
    }

    /// The current position of the remembered breeding partner, but only while
    /// it stays a valid mate (alive, still in love, not panicking). Returns
    /// `None` the moment the partner becomes ineligible, which ends the goal.
    fn love_partner_position(&self) -> Option<Vec3> {
        None
    }

    /// Spawns a child from this animal and its partner and clears love mode on
    /// both (vanilla's `spawnChildFromBreeding`).
    fn breed(&mut self) {}

    /// Forgets the currently-selected breeding partner (called when the goal
    /// stops), mirroring vanilla clearing `this.partner = null`.
    fn clear_love_partner(&mut self) {}

    /// Whether the mob is ignited (vanilla `Creeper.isIgnited`,
    /// `.cache/mc/26.2/src/net/minecraft/world/entity/monster/Creeper.java:260-262`).
    /// While `true`, [`Creeper.java:129-131`] forces the swell direction to
    /// climb every tick regardless of what
    /// [`SwellGoal`](crate::ai::goals::SwellGoal) would otherwise pick.
    /// Defaults to `false` for every mob that carries no fuse.
    fn is_ignited(&self) -> bool {
        false
    }

    /// The mob's current swell direction (vanilla `Creeper.getSwellDir`,
    /// `DATA_SWELL_DIR`, `Creeper.java:195-197`). Defaults to `-1`, matching
    /// vanilla's own default (`Creeper.java:100`,
    /// `entityData.define(DATA_SWELL_DIR, -1)`) for a mob that never sets one.
    fn swell_dir(&self) -> i32 {
        -1
    }

    /// Sets the swell direction (vanilla `Creeper.setSwellDir`,
    /// `Creeper.java:199-201`). A no-op for a mob that does not track one.
    fn set_swell_dir(&mut self, dir: i32) {
        let _ = dir;
    }

    /// The [`BlockCues`] of the block the mob is standing **in** — vanilla's
    /// `level.getBlockState(mob.blockPosition())`.
    ///
    /// # Why this is a query and not a per-tick feed (issue #456)
    ///
    /// Every other perception method on this trait is a value the host's census
    /// pushed in once per tick (`nearest_player`, `temptation`, …), and a
    /// pre-fed block snapshot would have matched that shape. It would also have
    /// been about **three orders of magnitude** more work than the goals need:
    /// `EatBlockGoal` is the only reader, and its `can_use` consults a block on
    /// roughly one tick in 500 (`random.nextInt(adjustedTickDelay(1000))`,
    /// `ai/goal/EatBlockGoal.java:29`). Pushing two block lookups per mob per
    /// tick to serve that multiplies by the whole mob population; pulling them
    /// costs exactly nothing on the 499 ticks nobody asks.
    ///
    /// It stays object-safe and mockable because the *world handle does not go
    /// on this trait*. The production implementor already borrows a
    /// `&dyn PathWorld` for pathfinding and answers from that, so there is no
    /// new lifetime and no new parameter here — which is why this is neither of
    /// the two options #456 posed, and cheaper than both.
    ///
    /// Defaults to [`BlockCues::NONE`]. A controller that cannot see blocks
    /// makes every cue-reading goal inert rather than wrong.
    fn block_cues_at_feet(&self) -> BlockCues {
        BlockCues::NONE
    }

    /// The [`BlockCues`] of the block **below** the mob — vanilla's
    /// `mob.blockPosition().below()`, the one a sheep grazes when it is standing
    /// on grass rather than in it (`ai/goal/EatBlockGoal.java:34`).
    ///
    /// Two separate methods rather than one taking an offset because these are
    /// the only two positions any of the goals in question reads, and vanilla
    /// spells them as two distinct expressions. A goal that needs to *search* a
    /// neighbourhood (`MoveToBlockGoal`'s 16- or 24-block spiral) must not be
    /// built on this — see `docs/mob-block-perception.md` for why that is a
    /// host-computed candidate position instead.
    fn block_cues_below(&self) -> BlockCues {
        BlockCues::NONE
    }

    /// Records that the mob just ate a block, for the host to resolve into the
    /// world mutation and the species' own `ate()` side effects.
    ///
    /// Vanilla `EatBlockGoal.tick` does the mutation inline — `destroyBlock` for
    /// the block at the mob's feet, `setBlock(below, DIRT)` for the grass block
    /// under it (`ai/goal/EatBlockGoal.java:59-80`) — and then calls
    /// `mob.ate()`, which for a sheep is `setSheared(false)` plus `ageUp(60)`
    /// (wool regrowth, `animal/sheep/Sheep.java`). None of that is expressible
    /// here: this crate can neither write a block nor touch entity metadata. So
    /// this is an **intent**, the same shape as [`attack`](MobController::attack)
    /// and [`launch_projectile`](MobController::launch_projectile), drained once
    /// per tick by the host.
    ///
    /// **A host that never drains it turns grazing into an island**: the goal
    /// runs, the animation plays, and the grass never changes. Note vanilla
    /// calls `ate()` even when the `mobGriefing` gamerule suppresses the block
    /// change (`:64-68`), so the two effects are separable on the host side and
    /// the gamerule check belongs there, not here.
    fn ate(&mut self, what: EatenBlock) {
        let _ = what;
    }

    /// Records the intent to launch a projectile this tick — vanilla's
    /// `RangedAttackMob.performRangedAttack`
    /// (`monster/RangedAttackMob.java:5-7`).
    ///
    /// This is an **intent**, exactly like [`attack`](MobController::attack): a
    /// goal in `lodestone-entity` has no access to a world, an entity id
    /// allocator or a projectile registry, all of which live in the host. The
    /// host drains the recorded launches once per tick and turns each into a real
    /// projectile entity. Defaults to a no-op so a controller that cannot spawn
    /// projectiles simply drops them, rather than every implementor having to
    /// say so.
    fn launch_projectile(&mut self, launch: ProjectileLaunch) {
        let _ = launch;
    }
}

/// Which block a grazing mob just ate, relative to the mob — the two positions
/// `EatBlockGoal` distinguishes, because vanilla's world mutation differs
/// between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EatenBlock {
    /// The block the mob was standing *in* (`#edible_for_sheep`, e.g.
    /// `short_grass`). Vanilla **destroys** it: `level.destroyBlock(pos, false)`
    /// (`ai/goal/EatBlockGoal.java:65`) — no drops, hence the `false`.
    AtFeet,
    /// The `grass_block` the mob was standing *on*. Vanilla **replaces** it with
    /// dirt rather than destroying it, plus level event `2001` for the break
    /// particles: `setBlock(below, Blocks.DIRT.defaultBlockState(), 2)`
    /// (`ai/goal/EatBlockGoal.java:72-74`).
    Below,
}

/// Which projectile a [`ProjectileLaunch`] asks the host to spawn.
///
/// Deliberately a small closed enum rather than a `ResourceKey`: the goal knows
/// *what kind of thing it is throwing* from the jar, and mapping that to a
/// registry name is the host's job (it is the side that owns the registry). This
/// also keeps `lodestone-entity`'s AI module free of any registry dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectileKind {
    /// `minecraft:arrow` — skeleton/stray bow shots
    /// (`monster/skeleton/AbstractSkeleton.java:160-175`).
    Arrow,
    /// `minecraft:small_fireball` — blaze
    /// (`monster/Blaze.java:238-240`).
    SmallFireball,
    /// `minecraft:snowball` — snow golem
    /// (`animal/golem/SnowGolem.java:118-131`).
    Snowball,
    /// `minecraft:splash_potion` — witch (`monster/Witch.java:222-251`).
    SplashPotion,
    /// `minecraft:trident` — drowned
    /// (`monster/zombie/Drowned.java:531-534`).
    Trident,
}

/// One projectile a goal asked the mob to launch, in world terms.
///
/// Carries a resolved `origin` and `velocity` rather than a target, because
/// vanilla's aiming maths is **per species** — the skeleton adds
/// `horizontalDistance * 0.2` to the vertical component and shoots at power
/// `1.6` (`AbstractSkeleton.java:165-171`), the blaze normalises a
/// triangle-jittered direction and scales by its acceleration power `0.1`
/// (`Blaze.java:236-240`, `AbstractHurtingProjectile.java:24,180-183`). Resolving
/// it in the goal keeps that citation next to the numbers it came from, and
/// leaves the host with nothing to re-derive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectileLaunch {
    /// Which projectile to spawn.
    pub kind: ProjectileKind,
    /// Where it appears, in world coordinates.
    pub origin: Vec3,
    /// Its initial velocity, in **blocks per tick** — already scaled by the
    /// species' own power figure.
    pub velocity: Vec3,
}

impl ProjectileLaunch {
    /// A launch aimed along `(dx, dy, dz)` at `power`, mirroring vanilla
    /// `Projectile.getMovementToShoot` (`projectile/Projectile.java:130-139`):
    /// normalise the direction, then scale by power.
    ///
    /// **The inaccuracy term is not modelled.** Vanilla adds
    /// `random.triangle(0.0, 0.0172275 * uncertainty)` on each axis before
    /// scaling (`Projectile.java:133-137`), which for a skeleton is
    /// `14 - difficulty * 4` (`AbstractSkeleton.java:170`) — a real spread. Ours
    /// flies dead straight. That is a disclosed simplification, not a
    /// transcription error: the spread needs vanilla's `RandomSource.triangle`
    /// distribution to match, and a deterministic velocity is also what lets a
    /// gate predict the exact value rather than assert a direction.
    #[must_use]
    pub fn aimed(kind: ProjectileKind, origin: Vec3, dx: f64, dy: f64, dz: f64, power: f64) -> Self {
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        // `Vec3.normalize` returns ZERO for a zero-length vector rather than
        // NaN, and vanilla relies on that (`phys/Vec3.java`); reproduce it, or a
        // mob standing exactly on its target launches a NaN projectile that
        // poisons every later position it is integrated into.
        let velocity = if len < 1.0e-4 {
            Vec3::default()
        } else {
            Vec3::new(dx / len * power, dy / len * power, dz / len * power)
        };
        Self {
            kind,
            origin,
            velocity,
        }
    }
}

/// Squared horizontal+vertical distance between two points.
#[must_use]
pub fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    let d = a - b;
    d.x * d.x + d.y * d.y + d.z * d.z
}
