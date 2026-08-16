//! The body seam a brain's behaviours drive.
//!
//! Analogous to [`MobController`](crate::ai::MobController) for the goal system:
//! [`BrainMob`] is the narrow set of *body* operations behaviours need, keeping
//! the brain free of world and physics. Coordination between behaviours happens
//! through [`Memories`](super::memory::Memories), not through this trait — the
//! trait is only for things the brain cannot express as memory (rolling dice,
//! reading the clock, commanding the real navigator, perceiving the world).

use lodestone_model::Vec3;

/// One entity a brain's perception can see, for sensors that need more than
/// "the nearest player" — [`super::sensor::NearestHostileSensor`] is the one
/// production reader today, and a target-acquisition behaviour (a ram target,
/// a golem's attacker) is the shape this exists for next.
///
/// Deliberately thin: an id, a position, and the one classification question
/// [`NearestHostileSensor`](super::sensor::NearestHostileSensor) needs
/// answered by the host rather than re-derived here — this crate has no
/// species table to decide hostility from, so the host (which already knows
/// [`MobCategory`]/the roster) resolves it once per entity rather than this
/// crate re-implementing a second, possibly-drifting hostility classifier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NearbyBrainEntity {
    /// The entity's id, for writing into an `Entity`-shaped memory value.
    pub id: i32,
    /// The entity's current position.
    pub position: Vec3,
    /// Whether the host classifies this entity as hostile to the perceiving
    /// mob — vanilla's `Monster`/hostile-category test, resolved by the host.
    pub hostile: bool,
}

/// The mob-facing operations a brain behaviour or sensor may perform.
///
/// # This trait deliberately overlaps [`MobController`], and that has a cost
///
/// `NavigatingMob` implements **both**, and the two declare
/// same-named methods: `position`, `next_i32`, `next_f32`, `in_water`, `move_to`,
/// `navigation_done`, `stop_navigation`, `look_at`. On a type implementing both,
/// every call to one of those is `E0034 multiple applicable items in scope` and
/// must be spelled `MobController::in_water(self)` or
/// `BrainMob::move_to(self, …)`. Three call sites in `navigating_mob.rs`'s tests
/// pay that tax today.
///
/// **Worse, the two `move_to`s differ in float width** — `MobController`'s speed is
/// `f64`, this one's is `f32` — so a disambiguation that picks the wrong trait
/// changes the literal's type rather than failing to compile. The split is
/// pre-existing and faithful (vanilla's `WalkTarget.speedModifier` is a `float`
/// while the goal system's speeds are doubles), but it means the ambiguity cannot
/// be resolved by inference and never will be.
///
/// **Making this trait `BrainMob: MobController` would remove the ambiguity
/// permanently, and was considered and rejected.** It would force every
/// implementor — including the brain's own hermetic `TestMob` — to supply all ~35
/// `MobController` methods, most of which no behaviour ever calls; and it would
/// make the Brain system *depend on* the goal system's seam rather than sit beside
/// it, which is a layering inversion for two architectures vanilla treats as
/// peers. Revisit if a third implementor appears, or if the disambiguation tax
/// spreads beyond test code.
///
/// [`MobController`]: crate::ai::MobController
pub trait BrainMob {
    /// A uniform random `i32` in `[0, bound)` (vanilla's `random.nextInt`).
    fn next_i32(&mut self, bound: i32) -> i32;

    /// A uniform random `f32` in `[0, 1)`.
    fn next_f32(&mut self) -> f32;

    /// The current game time in ticks. Behaviour timeouts and cooldowns are
    /// measured against this.
    fn game_time(&self) -> i64;

    /// The current time of day, `0..24000` — vanilla's `dayTime % 24000`,
    /// what [`Brain::update_activity_from_schedule`](super::Brain::update_activity_from_schedule)
    /// switches a scheduled activity against. Deliberately **not**
    /// [`game_time`](Self::game_time): that is a per-mob monotonic tick
    /// counter with no relation to the world clock (see its own doc), while a
    /// schedule needs the *real* time of day. Defaults to `0` (perpetual
    /// midnight) so every existing implementor, including hermetic test
    /// doubles, keeps compiling; only a host with a real world clock overrides
    /// it.
    fn day_time(&self) -> i32 {
        0
    }

    /// The mob's current position.
    fn position(&self) -> Vec3;

    /// Whether the mob is currently in water (gates swim/stroll variants).
    fn in_water(&self) -> bool {
        false
    }

    /// Commands navigation toward `target` at `speed`. Returns whether a path
    /// was found (vanilla's `navigation.moveTo`).
    fn move_to(&mut self, target: Vec3, speed: f32) -> bool;

    /// Whether navigation has finished or has no path.
    fn navigation_done(&self) -> bool;

    /// Whether navigation is stuck (used to set a retry cooldown).
    fn navigation_stuck(&self) -> bool {
        false
    }

    /// Stops navigation.
    fn stop_navigation(&mut self);

    /// Points the look control at a world position.
    fn look_at(&mut self, target: Vec3);

    /// The nearest visible player's position, if any is within perception
    /// range. Feeds the nearest-player sensor.
    fn nearest_visible_player(&self) -> Option<Vec3> {
        None
    }

    /// A candidate land wander destination within the given block radii
    /// (vanilla's `LandRandomPos.getPos`). `None` means none was found.
    fn random_land_pos(&mut self, max_xz: i32, max_y: i32) -> Option<Vec3>;

    /// The position of whoever last damaged this mob, if that memory has not
    /// yet expired — feeds [`super::sensor::HurtBySensor`], the `HURT_BY`
    /// analogue of [`MobController::last_hurt_by`]. Defaults to `None` so
    /// every existing implementor (including the brain's own hermetic test
    /// doubles) keeps compiling; only [`NavigatingMob`](crate::ai::NavigatingMob)
    /// overrides it, delegating to the exact same field
    /// `MobController::last_hurt_by` already reads — one hurt event, two
    /// seams onto it, not two independent trackers that could disagree.
    ///
    /// [`MobController`]: crate::ai::MobController
    fn last_hurt_by(&self) -> Option<Vec3> {
        None
    }

    /// Every entity the mob can currently perceive nearby — the feed
    /// [`super::sensor::NearestHostileSensor`] filters and reduces to a single
    /// nearest hostile. Defaults to empty so every existing implementor
    /// (including hermetic test doubles) keeps compiling; only a host that
    /// actually tracks nearby entities (e.g. `MobSim`) overrides it.
    ///
    /// No range cut is specified here on purpose — vanilla's own sensors carry
    /// their own radius (`NearestHostileSensor` uses `8.0`), so a host is free
    /// to pre-filter to a generous radius and let the *sensor* apply the exact
    /// cut, exactly as [`nearest_visible_player`](Self::nearest_visible_player)
    /// already delegates its own range decision to the host.
    fn nearby_entities(&self) -> Vec<NearbyBrainEntity> {
        Vec::new()
    }

    /// This villager's claimed job-site position, if any — feeds
    /// [`MemoryModuleType::JOB_SITE`](super::MemoryModuleType::JOB_SITE)
    /// through [`super::sensor::VillagerPoiSensor`]. Defaults to `None` so
    /// every existing implementor keeps compiling; only a host tracking a
    /// live workstation claim (`MobSim`) overrides it.
    fn job_site(&self) -> Option<Vec3> {
        None
    }

    /// This villager's claimed bed position, if any — feeds
    /// [`MemoryModuleType::HOME`](super::MemoryModuleType::HOME) the same way
    /// [`job_site`](Self::job_site) feeds `JOB_SITE`.
    fn home(&self) -> Option<Vec3> {
        None
    }

    /// This villager's claimed bell position, if any — feeds
    /// [`MemoryModuleType::MEETING_POINT`](super::MemoryModuleType::MEETING_POINT)
    /// the same way [`job_site`](Self::job_site) feeds `JOB_SITE`.
    fn meeting_point(&self) -> Option<Vec3> {
        None
    }

    /// The nearest visible zombified piglin's position, if any — feeds
    /// [`MemoryModuleType::NEAREST_VISIBLE_ZOMBIFIED`](super::MemoryModuleType::NEAREST_VISIBLE_ZOMBIFIED)
    /// through [`super::sensor::NearestVisibleZombifiedSensor`], the same
    /// host-computed-candidate shape [`job_site`](Self::job_site) already
    /// uses: this crate's `BrainMob` seam has no same-species census a
    /// sensor could search itself (see [`ai::roster::neutral`](crate::ai::roster::neutral)'s
    /// module doc on why that is a host question). Defaults to `None`.
    fn nearest_visible_zombified(&self) -> Option<Vec3> {
        None
    }

    /// Records a melee hit landing on whatever occupies `target` this tick —
    /// vanilla's `LivingEntity.hurtServer`/`knockback` calls a ram or an
    /// attack-target behaviour makes directly on the target entity. This
    /// crate's [`BrainMob`] has no entity handle to call a method *on*, only
    /// a position, so — the same seam
    /// [`MobController::attack`](crate::ai::MobController::attack) already
    /// uses for goal-driven melee — recording is all a behaviour can do; a
    /// host with the real world resolves the position to a victim and applies
    /// damage/knockback.
    ///
    /// Defaults to a no-op so every existing implementor (including hermetic
    /// test doubles) keeps compiling; [`NavigatingMob`](crate::ai::NavigatingMob)
    /// overrides it to push onto the **same** attack queue
    /// [`MobController::attack`](crate::ai::MobController::attack) writes, so
    /// a host's existing melee-hit resolution (already draining that queue
    /// for goal-driven mobs) picks up a brain-driven hit for free — one
    /// queue, two producers, not two independent trackers that could
    /// disagree.
    fn attack(&mut self, _target: Vec3) {}
}
