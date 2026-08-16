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

use crate::brain::BrainMob;
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

    /// The bare item id this mob is currently holding in its main hand (e.g.
    /// `"trident"`), or `None` for empty-handed. Feeds a goal whose vanilla
    /// `canUse` reads `getMainHandItem()` — [`RangedAttackGoal`]'s optional
    /// weapon requirement is the one production consumer today.
    ///
    /// Defaults to `None` so every existing implementor (including hermetic
    /// test doubles) keeps compiling; only [`NavigatingMob`] overrides it.
    ///
    /// [`RangedAttackGoal`]: crate::ai::roster::ranged::RangedAttackGoal
    /// [`NavigatingMob`]: crate::ai::NavigatingMob
    fn main_hand_item(&self) -> Option<&str> {
        None
    }

    /// The nearest position the mob considers an attackable target — the host
    /// applies the version/type-specific filter (hostility, follow range, line
    /// of sight). Drives `NearestAttackableTargetGoal`.
    ///
    /// **A host that returns [`attack_target`](MobController::attack_target)
    /// here has written an island, not an implementation**: the
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
    /// (`TargetGoal::getFollowDistance`) and
    /// `TargetGoal::canContinueToUse` **drops a target that leaves it**
    /// (`distanceToSqr(target) > within * within`).
    /// The default is `Mob::createMobAttributes`'s `16.0`, so a
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
    /// (`NeutralMob::setTimeToRemainAngry` adds to `level().getGameTime()`, and
    /// `NeutralMob::isAngry` subtracts the current game time back off the stored
    /// deadline; `NO_ANGER_END_TIME = -1`), and the grudge is a uniform
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

    /// The position of this mob's owner, if it has one — vanilla
    /// `OwnableEntity::getOwner` (inherited by `TamableAnimal`, which
    /// implements `OwnableEntity`), read as a position because that is all
    /// this seam carries.
    ///
    /// # Why the owner's identity does not cross
    ///
    /// The seam deliberately carries positions, never entity ids — the same
    /// division [`angry_target`](MobController::angry_target) documents — and
    /// a *tamed* owner is, in vanilla, a **player**:
    /// `TamableAnimal::DATA_OWNERUUID_ID` stores an `EntityReference` wrapping
    /// a `UUID`, and `OwnableEntity::getOwner` resolves it against the level
    /// (`EntityReference::getLivingEntity`). This seam has no notion of a
    /// player at all, so the host resolves "who owns me" and feeds only the
    /// owner's current position, exactly as it feeds
    /// [`parent_position`](MobController::parent_position).
    /// The host's own record of *which id owns which mob* — the thing a
    /// wolf-pack `alertOthers` same-owner filter needs
    /// (`HurtByTargetGoal::alertOthers`) — is a census question this
    /// crate cannot answer; `lodestone_server`'s `SimMob::owner_id` is where
    /// it lives.
    ///
    /// Defaults to `None` — a wild, ownerless mob.
    fn owner_position(&self) -> Option<Vec3> {
        None
    }

    /// Whether this mob is *tame at all* — vanilla
    /// `TamableAnimal.isTame()`, the `0x04` bit of `TamableAnimal.DATA_FLAGS_ID`.
    ///
    /// Distinct from [`owner_position`](MobController::owner_position) being
    /// `Some`, and the distinction is load-bearing rather than pedantic: a tamed
    /// wolf whose owner has logged out has no owner *position* and is still
    /// tame. A goal that reads `owner_position().is_some()` as "am I tame" would
    /// therefore un-tame every pet the moment its owner walked out of the
    /// player list, which is what `SitWhenOrderedToGoal`'s `!isTame()` arm and
    /// `Wolf.WolfAvoidEntityGoal`'s `!wolf.isTame()` guard both hinge on.
    fn is_tame(&self) -> bool {
        false
    }

    /// Whether the owner has told this mob to sit — vanilla
    /// `TamableAnimal.isOrderedToSit()`, which is the *persisted intent* rather
    /// than the pose.
    ///
    /// Vanilla keeps two pieces of state here and only one of them is this:
    /// `orderedToSit` is the field an owner's right-click toggles and NBT
    /// round-trips (`TamableAnimal.addAdditionalSaveData`'s `Sitting`), while
    /// `setInSittingPose` is the *synced* `0x01` flag bit that
    /// `SitWhenOrderedToGoal::start`/`stop` writes as the goal actually runs.
    /// The intent is what a goal must read to decide whether to run; the pose is
    /// what the goal produces. Collapsing them means a sitting order silently
    /// evaporates whenever the goal is preempted by a higher-priority flag
    /// holder.
    fn is_ordered_to_sit(&self) -> bool {
        false
    }

    /// Reports that this mob has entered or left the sitting **pose** — vanilla
    /// `TamableAnimal.setInSittingPose`, called by `SitWhenOrderedToGoal`'s
    /// `start` and `stop`. The host turns this into the synced `0x01` flag bit.
    fn set_in_sitting_pose(&mut self, sitting: bool) {
        let _ = sitting;
    }

    /// Host-computed candidate target for `CatSitOnBlockGoal` — the nearest
    /// chest or lit furnace within its search radius, or `None`. Following
    /// `docs/mob-block-perception.md`'s own guidance ("a goal that needs to
    /// *search* a neighbourhood… must not be built on [`block_cues_at_feet`]…
    /// that is a host-computed candidate position instead"), the same shape
    /// [`owner_position`](Self::owner_position)/`parent_candidate` already
    /// use: the host owns the block registry and the bounded search, and
    /// hands the goal an answer rather than a query.
    ///
    /// Defaults to `None`, the honest state for a controller with no such
    /// feed — the goal simply never finds anything to sit on.
    fn cat_sit_target(&self) -> Option<Vec3> {
        None
    }

    /// Host-computed candidate target for `CatLieOnBedGoal` — the nearest bed
    /// foot within its search radius, or `None`. A separate field from
    /// [`cat_sit_target`](Self::cat_sit_target) because the two goals hunt
    /// different block sets (chests/lit furnaces vs. beds) and vanilla itself
    /// keeps them as two distinct `MoveToBlockGoal` searches.
    fn cat_bed_target(&self) -> Option<Vec3> {
        None
    }

    /// Whether this mob is in the lying pose — vanilla `Cat.isLying()`
    /// (`DATA_LIES`), what [`CatLieOnBedGoal`](super::goals::CatLieOnBedGoal)
    /// toggles once it reaches its bed.
    fn is_lying(&self) -> bool {
        false
    }

    /// Sets the lying pose — vanilla `Cat.setLying`. The host turns this into
    /// the synced flag.
    fn set_lying(&mut self, lying: bool) {
        let _ = lying;
    }

    /// Whether this mob is part of an active pillager patrol — vanilla
    /// `PatrollingMonster.isPatrolling()`.
    ///
    /// Defaults to `false`.
    fn is_patrolling(&self) -> bool {
        false
    }

    /// Whether this mob leads its patrol — vanilla
    /// `PatrollingMonster.isPatrolLeader()`. Only a leader repicks its own
    /// far-off waypoint once it arrives; see
    /// [`LongDistancePatrolGoal`](super::goals::LongDistancePatrolGoal)'s own
    /// doc comment for why a follower's movement is driven by
    /// [`patrol_group_target`](MobController::patrol_group_target) instead.
    ///
    /// Defaults to `false`.
    fn is_patrol_leader(&self) -> bool {
        false
    }

    /// This mob's own current long-distance patrol waypoint — vanilla
    /// `PatrollingMonster.getPatrolTarget()`.
    ///
    /// Defaults to `None`, vanilla's `hasPatrolTarget() == false` state.
    fn patrol_target(&self) -> Option<Vec3> {
        None
    }

    /// Records a newly chosen patrol waypoint — vanilla
    /// `PatrollingMonster.setPatrolTarget`/`findPatrolTarget`, called both when
    /// a leader picks a fresh far-off target and when a follower adopts
    /// [`patrol_group_target`](MobController::patrol_group_target).
    fn set_patrol_target(&mut self, target: Option<Vec3>) {
        let _ = target;
    }

    /// For a **non-leader**: the patrol's shared waypoint, as the host
    /// resolves it by searching nearby patrol leaders. Vanilla's own
    /// `LongDistancePatrolGoal.tick` runs this search itself
    /// (`findPatrolCompanions`, a `getEntitiesOfClass` query) and *pushes* its
    /// current near-term waypoint out to every companion it finds; this crate
    /// has no "find nearby entities of a class" query on this seam at all — it
    /// hands goals answers, never populations — so the direction is reversed: a
    /// follower *pulls* its leader's long-distance target from the host here
    /// instead. See [`LongDistancePatrolGoal`](super::goals::LongDistancePatrolGoal)
    /// for the full account of what that changes.
    ///
    /// Defaults to `None`.
    fn patrol_group_target(&self) -> Option<Vec3> {
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

    /// Whether a player is currently staring at this mob — vanilla
    /// `LivingEntity::isLookingAtMe(player, coneSize, adjustForDistance, …)`,
    /// wrapped for the enderman by `EnderMan::isBeingStaredBy`.
    ///
    /// The host computes the answer from each player's eye position **and view
    /// vector** and feeds only the boolean: the geometric half is the free
    /// function [`is_in_view_cone`], which mirrors vanilla's exact
    /// `dot > 1.0 - coneSize / dist` test, and the line-of-sight half is a
    /// world raycast — the same disclosed gap
    /// [`find_nearest_target`](MobController::find_nearest_target) names for
    /// its own `hasLineOfSight`, omitted rather than faked, erring permissive.
    ///
    /// Defaults to `false` (nobody staring). The two consumers are the
    /// enderman's [`EndermanFreezeWhenLookedAt`](crate::ai::goals::EndermanFreezeWhenLookedAt)
    /// and [`EndermanLookForPlayerGoal`](crate::ai::goals::EndermanLookForPlayerGoal),
    /// and a host that never feeds this must agree with both on `false` — a
    /// default of `true` would make every enderman react to every player on
    /// sight. Both goals exist and the feed is live in production:
    /// `lodestone_server::mobs::MobSim::tick_with_terrain` computes this every
    /// tick from each connected player's real position and view direction.
    fn is_being_stared_at(&self) -> bool {
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

    /// Whether the mob is ignited (vanilla `Creeper::isIgnited`).
    /// While `true`, `Creeper::tick` forces the swell direction to
    /// climb every tick regardless of what
    /// [`SwellGoal`](crate::ai::goals::SwellGoal) would otherwise pick.
    /// Defaults to `false` for every mob that carries no fuse.
    fn is_ignited(&self) -> bool {
        false
    }

    /// The mob's current swell direction (vanilla `Creeper::getSwellDir`,
    /// `DATA_SWELL_DIR`). Defaults to `-1`, matching
    /// vanilla's own default (`Creeper::defineSynchedData`'s
    /// `entityData.define(DATA_SWELL_DIR, -1)`) for a mob that never sets one.
    fn swell_dir(&self) -> i32 {
        -1
    }

    /// Sets the swell direction (vanilla `Creeper::setSwellDir`). A no-op for
    /// a mob that does not track one.
    fn set_swell_dir(&mut self, dir: i32) {
        let _ = dir;
    }

    /// The [`BlockCues`] of the block the mob is standing **in** — vanilla's
    /// `level.getBlockState(mob.blockPosition())`.
    ///
    /// # Why this is a query and not a per-tick feed
    ///
    /// Every other perception method on this trait is a value the host's census
    /// pushed in once per tick (`nearest_player`, `temptation`, …), and a
    /// pre-fed block snapshot would have matched that shape. It would also have
    /// been about **three orders of magnitude** more work than the goals need:
    /// `EatBlockGoal` is the only reader, and its `EatBlockGoal::canUse` consults
    /// a block on roughly one tick in 500
    /// (`random.nextInt(adjustedTickDelay(1000))`). Pushing two block lookups
    /// per mob per tick to serve that multiplies by the whole mob population;
    /// pulling them costs exactly nothing on the 499 ticks nobody asks.
    ///
    /// It stays object-safe and mockable because the *world handle does not go
    /// on this trait*. The production implementor already borrows a
    /// `&dyn PathWorld` for pathfinding and answers from that, so there is no
    /// new lifetime and no new parameter here — which keeps this cheaper than
    /// pre-feeding a block snapshot each tick.
    ///
    /// Defaults to [`BlockCues::NONE`]. A controller that cannot see blocks
    /// makes every cue-reading goal inert rather than wrong.
    fn block_cues_at_feet(&self) -> BlockCues {
        BlockCues::NONE
    }

    /// The [`BlockCues`] of the block **below** the mob — vanilla's
    /// `mob.blockPosition().below()`, the one a sheep grazes when it is standing
    /// on grass rather than in it (`EatBlockGoal::canUse`).
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
    /// Vanilla `EatBlockGoal::tick` does the mutation inline — `destroyBlock` for
    /// the block at the mob's feet, `setBlock(below, DIRT)` for the grass block
    /// under it — and then calls
    /// `mob.ate()`, which for a sheep is `setSheared(false)` plus `ageUp(60)`
    /// (wool regrowth, `Sheep::ate`). None of that is expressible
    /// here: this crate can neither write a block nor touch entity metadata. So
    /// this is an **intent**, the same shape as [`attack`](MobController::attack)
    /// and [`launch_projectile`](MobController::launch_projectile), drained once
    /// per tick by the host.
    ///
    /// **A host that never drains it turns grazing into an island**: the goal
    /// runs, the animation plays, and the grass never changes. Note vanilla's
    /// `EatBlockGoal::tick` calls `ate()` even when the `mobGriefing` gamerule
    /// suppresses the block change, so the two effects are separable on the
    /// host side and the gamerule check belongs there, not here.
    fn ate(&mut self, what: EatenBlock) {
        let _ = what;
    }

    /// Records the intent to launch a projectile this tick — vanilla's
    /// `RangedAttackMob::performRangedAttack`.
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

    /// Teleports the mob directly to `target` — vanilla `Entity::teleportTo`
    /// or, for the enderman, its
    /// `EnderMan::teleport` / `EnderMan::teleportTowards` displacement variants.
    ///
    /// An **instant** relocation, not a fast path-follow: the position is
    /// rewritten immediately and any in-progress path is abandoned. The
    /// enderman's own variants pick the destination — `EnderMan::teleport` a
    /// random point within ±32 blocks on each axis, `EnderMan::teleportTowards`
    /// 16 blocks past the target in its facing direction — so the goal or
    /// host resolves *where* and this primitive resolves *that it happens*.
    ///
    /// Defaults to a no-op — a controller without a position to move silently
    /// declines, rather than every implementor having to say so.
    fn teleport_to(&mut self, target: Vec3) {
        let _ = target;
    }

    /// Records that this mob wants to damage itself by `amount` — the bee's
    /// sting self-destruct, where `Bee::customServerAiStep` eventually calls
    /// `this.hurtServer(level, this.damageSources().generic(), this.getHealth())`.
    ///
    /// An **intent**, exactly like [`attack`](MobController::attack) and
    /// [`launch_projectile`](MobController::launch_projectile): health lives on
    /// the host, so this seam only records the request and the host drains it
    /// once per tick and applies it through its normal damage pipeline
    /// (i-frames and armour reductions included, matching vanilla's
    /// `hurtServer`). Defaults to a no-op so a controller that cannot model
    /// self-harm drops it.
    fn damage_self(&mut self, amount: f32) {
        let _ = amount;
    }

    /// This controller viewed as a [`BrainMob`], if it can drive vanilla's *other*
    /// AI architecture. `None` — the default — means it cannot.
    ///
    /// # Why the two architectures meet here
    ///
    /// 26.2 ships both AI systems and vanilla's `Mob` carries **both** fields:
    /// `goalSelector` and `brain`, ticked in the same `customServerAiStep`. This
    /// repo had only half of that: [`GoalSelector`](crate::ai::GoalSelector)
    /// reached production through [`NavigatingMob`](crate::ai::NavigatingMob) and
    /// `MobSim`, while [`Brain`](crate::brain::Brain) had no production caller at
    /// all — the [`Sensor`](crate::brain::Sensor)/`BehaviorControl` machinery was
    /// individually complete and reached zero mobs.
    ///
    /// This method is the join, and it is deliberately **on the existing seam**
    /// rather than a second parallel one. A [`BrainGoal`](crate::brain::BrainGoal)
    /// is an ordinary [`Goal`](crate::ai::Goal), so every host that already ticks
    /// a `GoalSelector` ticks a brain too, with no host change whatsoever. The
    /// alternative — a second `tick_brain` entry point every driver must learn to
    /// call — is how the first island was built.
    ///
    /// # The default is `None` on purpose, and that is load-bearing
    ///
    /// A test fake that overrides every perception method (`ScriptMob`,
    /// `ai::roster::probe`) is exactly how a target-acquisition island and a
    /// perception-seam island both stayed hidden previously: the
    /// goal had a green unit test while its `can_use` was constant-`false` in
    /// production. Returning `None` by default means a brain installed on a fake
    /// mob does **nothing at all**, loudly — so a brain behaviour cannot be
    /// "proven" against a double. The only way to observe one is to drive the real
    /// [`NavigatingMob`](crate::ai::NavigatingMob), which is the sole implementor
    /// that answers `Some`.
    fn brain_mob(&mut self) -> Option<&mut dyn BrainMob> {
        None
    }
}

/// Which block a grazing mob just ate, relative to the mob — the two positions
/// `EatBlockGoal` distinguishes, because vanilla's world mutation differs
/// between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EatenBlock {
    /// The block the mob was standing *in* (`#edible_for_sheep`, e.g.
    /// `short_grass`). Vanilla **destroys** it in `EatBlockGoal::tick`:
    /// `level.destroyBlock(pos, false)` — no drops, hence the `false`.
    AtFeet,
    /// The `grass_block` the mob was standing *on*. Vanilla **replaces** it with
    /// dirt rather than destroying it, plus level event `2001` for the break
    /// particles, also in `EatBlockGoal::tick`:
    /// `setBlock(below, Blocks.DIRT.defaultBlockState(), 2)`.
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
    /// (`AbstractSkeleton::performRangedAttack`).
    Arrow,
    /// `minecraft:small_fireball` — blaze
    /// (`Blaze.BlazeAttackGoal::tick`).
    SmallFireball,
    /// `minecraft:snowball` — snow golem
    /// (`SnowGolem::performRangedAttack`).
    Snowball,
    /// `minecraft:splash_potion` — witch (`Witch::performRangedAttack`).
    SplashPotion,
    /// `minecraft:trident` — drowned
    /// (`Drowned::performRangedAttack`).
    Trident,
    /// `minecraft:fireball` — the ghast's own attack
    /// (`Ghast.GhastShootFireballGoal.tick`, which constructs a
    /// `net.minecraft.world.entity.projectile.hurtingprojectile.LargeFireball`).
    /// **The registry name is not the class name**: `LargeFireball`'s own
    /// constructor passes `EntityTypes.FIREBALL` to its `Fireball` superclass,
    /// so the wire id is `minecraft:fireball`, the same one a small fireball's
    /// class is *not* named after either — checked against
    /// `lodestone_data::generated::entity_types`, not assumed from the Java
    /// class name.
    LargeFireball,
    /// `minecraft:wither_skull` — the wither's own ranged attack
    /// (`WitherBoss::performRangedAttack`, ported at
    /// `lodestone_server::wither`/`lodestone_server::mobs::wither`). Not
    /// currently launched through this goal-driven seam — the wither is a
    /// plain tracked entity, not a `SimMob`, the same shape
    /// `mobs::dragon`'s own module doc explains for the ender dragon — but
    /// the variant lives here so `projectile_entity_type`/`integrates_as_arrow`
    /// have one shared table to read from either way.
    WitherSkull,
    /// `minecraft:dragon_fireball` — the ender dragon's strafe attack
    /// (`DragonStrafePlayerPhase`, `crate::dragon::phase::PhaseEffect::FireFireball`,
    /// ported at `lodestone_server::mobs::dragon::tick_one_dragon`). Not
    /// launched through this goal-driven seam either, for the same reason
    /// [`WitherSkull`](Self::WitherSkull) is not.
    DragonFireball,
}

/// One projectile a goal asked the mob to launch, in world terms.
///
/// Carries a resolved `origin` and `velocity` rather than a target, because
/// vanilla's aiming maths is **per species** — the skeleton adds
/// `horizontalDistance * 0.2` to the vertical component and shoots at power
/// `1.6` (`AbstractSkeleton::performRangedAttack`), the blaze normalises a
/// triangle-jittered direction and scales by its acceleration power `0.1`
/// (`Blaze.BlazeAttackGoal::tick`, `AbstractHurtingProjectile::assignDirectionalMovement`
/// and its `accelerationPower` field). Resolving
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
    /// `Projectile::getMovementToShoot`:
    /// normalise the direction, then scale by power.
    ///
    /// **The inaccuracy term is not modelled.** Vanilla adds
    /// `random.triangle(0.0, 0.0172275 * uncertainty)` on each axis before
    /// scaling, in the same `Projectile::getMovementToShoot`, which for a
    /// skeleton is `14 - difficulty * 4` (`AbstractSkeleton::performRangedAttack`)
    /// — a real spread. Ours
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

/// The geometric half of vanilla `LivingEntity::isLookingAtMe`: whether the
/// point `target` lies inside a viewer's acceptance cone.
///
/// `viewer_eye` is the viewer's eye position and `look` its view vector
/// (normalised internally, matching vanilla's `getViewVector(1.0F).normalize()`);
/// `target` is the point being stared at — for the enderman,
/// `EnderMan::isBeingStaredBy` passes `(this.getX(), getEyeY(), this.getZ())`.
/// Vanilla accepts a stare when
///
/// ```text
/// look · dir > 1.0 - coneSize / (adjustForDistance ? dist : 1.0)
/// ```
///
/// with `dir` the unit vector from `viewer_eye` to `target` and `dist` its
/// length. **The tolerance is divided by distance when `adjustForDistance` is
/// true, so the required precision *increases* with range** — dividing a
/// fixed `coneSize` by a growing `dist` pushes the threshold toward `1.0`,
/// shrinking the angular cone the further away the viewer stands. Measured
/// from this function's own test: `coneSize` `1.0` accepts a `dot` of `0.6`
/// (about 53°) at 2 blocks but rejects the identical angle at 5 blocks
/// (threshold rises from `0.5` to `0.8`). This is the opposite of a
/// fixed-angle cone — that reads `coneSize` as the tolerance directly,
/// independent of `dist` — and the reason this is a faithful port rather than
/// an approximation (the enderman passes `0.025, true`).
///
/// The full vanilla test is this cone *and* line of sight
/// (`target.hasLineOfSight(viewer, …)`, a world raycast) — the same disclosed
/// gap [`find_nearest_target`](MobController::find_nearest_target) names for
/// its own `hasLineOfSight`, omitted here rather than faked, erring permissive.
///
/// One deliberate divergence: a viewer whose eye is **at** `target` (distance
/// below `1e-9`) reads `false`. Vanilla's own formula divides by `dist` and
/// `dir.normalize()` returns ZERO, so a zero distance yields a degenerate
/// `dot > -infinity` which is `true` for `adjustForDistance`; treating
/// "viewer inside the target" as not-a-stare is the saner reading of a
/// position that is physically impossible to hold.
#[must_use]
pub fn is_in_view_cone(
    viewer_eye: Vec3,
    look: Vec3,
    target: Vec3,
    cone_size: f64,
    adjust_for_distance: bool,
) -> bool {
    let dir = target - viewer_eye;
    let dist = dir.length();
    if dist < 1.0e-9 {
        return false;
    }
    let dot = look.normalize().dot(dir / dist);
    let tolerance = if adjust_for_distance {
        cone_size / dist
    } else {
        cone_size
    };
    dot > 1.0 - tolerance
}
