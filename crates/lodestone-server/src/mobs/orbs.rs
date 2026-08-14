//! `MobSim`'s experience-orb slice — award/spawn, per-tick drift/merge/decay,
//! and the orb query/pickup API. Moved out of `mobs/mod.rs` verbatim as part
//! of the `mobs.rs` file split (see `docs/plans/crate-and-file-splits.md`).

use lodestone_entity::item_entity::ItemMotion;
use lodestone_physics::{CollisionView, EntityDimensions};
use lodestone_model::Vec3;
use uuid::Uuid;

use super::{
    MobSim, OrbState, PLAYER_EYE_HEIGHT, VOID_DESPAWN_DEPTH, dist_sqr, settle_entity, within_box,
};

// ---------------------------------------------------------------------------
// `ExperienceOrb` — every constant below is transcribed from that class
// ---------------------------------------------------------------------------

/// `ExperienceOrb.LIFETIME`: ticks before an orb discards itself. Five minutes, the
/// same figure `ItemLifecycle` uses, and reset to `0` by a merge.
const ORB_LIFETIME: i32 = 6000;

/// `ExperienceOrb.ENTITY_SCAN_PERIOD`, and the phase matters: vanilla scans when
/// `tickCount % 20 == 1`, not `== 0`, so an orb spawned this tick does not scan on its
/// own first tick.
const ORB_MERGE_SCAN_PERIOD: u64 = 20;

/// `ExperienceOrb.MAX_FOLLOW_DIST`. Doubles as the divisor in
/// `followNearbyPlayer`'s falloff, so it is one constant and not two.
const ORB_MAX_FOLLOW_DIST: f64 = 8.0;

/// `ExperienceOrb.ORB_GROUPS_PER_AREA`, the modulus of the merge rule
/// `(orb.getId() - id) % 40 == 0`.
///
/// **This is the whole reason a big award is a handful of orbs rather than one pile.**
/// Only orbs whose network ids are congruent mod 40 may merge, so consecutive spawns
/// (ids `n`, `n+1`, …) cannot merge with each other at all — the first candidate for id
/// `n` is id `n + 40`. A gate that spawns ten orbs and expects a merge is measuring
/// nothing; it needs more than 40.
const ORB_GROUPS_PER_AREA: i32 = 40;

/// `ExperienceOrb.getDefaultGravity` — `0.03`, **not** the item entity's `0.04`.
const ORB_GRAVITY: f64 = 0.03;

/// `ExperienceOrb.getAirDrag`. Applied to all three components, unlike
/// `ItemMotion::tick`'s split drag.
const ORB_AIR_DRAG: f64 = 0.98;

/// The landing bounce: `setDeltaMovement(x, -fallSpeed * 0.4, z)` where `fallSpeed` is
/// the y velocity captured **before** the move. An item's is `velocity.y *= -0.5`
/// applied after drag, so the two are not interchangeable.
const ORB_LANDING_BOUNCE: f64 = 0.4;

/// The strength `followNearbyPlayer` scales its normalised pull by.
const ORB_FOLLOW_PULL: f64 = 0.1;

/// `EntityType.EXPERIENCE_ORB`'s hitbox, `0.5 × 0.5`, with no auto-step for
/// [`ITEM_DIMENSIONS`]' reason: `ExperienceOrb` extends `Entity` directly and never
/// overrides `maxUpStep()`.
const ORB_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.5, 0.5, 0.0);

/// Reach of `scanForMerges`' search, per axis: `getBoundingBox().inflate(0.5)` against
/// another orb's own box, so `0.25 + 0.5 + 0.25`.
///
/// **Isotropic**, unlike [`ITEM_MERGE_REACH_XZ`]/[`ITEM_MERGE_REACH_Y`]: `inflate(0.5)`
/// with one argument inflates y too, where `ItemEntity`'s three-argument
/// `inflate(0.5, 0.0, 0.5)` deliberately does not. Two orbs a block apart vertically
/// *do* merge; two items never do.
const ORB_MERGE_REACH: f64 = 0.25 + 0.5 + 0.25;

/// Reach of `tryMergeToExisting`' search, per axis: `AABB.ofSize(pos, 1, 1, 1)` is a
/// unit cube centred on the spawn point (half-extent `0.5`) against the candidate's own
/// box, so `0.5 + 0.25`.
const ORB_SPAWN_MERGE_REACH: f64 = 0.5 + 0.25;

/// Seed for [`MobSim::orb_rng`], in the same shape as
/// [`crate::block_drops::BLOCK_DROPS_BEHAVIOR_SEED`] and its siblings: an arbitrary
/// fixed constant, so a replay of the same awards produces the same merges.
pub(super) const ORB_BEHAVIOR_SEED: u64 = 0x584f_5242_5f53_4545;

impl<'w> MobSim<'w> {
    // -----------------------------------------------------------------------
    // `ExperienceOrb`
    // -----------------------------------------------------------------------

    /// `ExperienceOrb.awardWithDirection`: turns `amount` points into orbs at
    /// `position`, merging into an existing orb where vanilla would.
    ///
    /// Returns the ids of the orbs actually *spawned* — shorter than
    /// [`crate::experience::orb_denominations`]'s list whenever a denomination merged
    /// into an existing orb instead, which is the observable difference between this
    /// and a bare spawn loop.
    ///
    /// The split itself is [`crate::experience::orb_denominations`]: greedy
    /// change-making over an irregular ladder, so 100 is `73 + 17 + 7 + 3` and not one
    /// orb of 100. That module owns the ladder; this owns the entity.
    ///
    /// `rough_direction` is `awardWithDirection`'s bias — vanilla offsets the spawn
    /// along it and flips the random impulse to agree with it. `Vec3::ZERO` is
    /// `ExperienceOrb.award`, which is what every vanilla caller except a few block
    /// drops uses.
    pub fn award_experience(
        &mut self,
        position: Vec3,
        rough_direction: Vec3,
        amount: i32,
    ) -> Vec<i32> {
        let mut spawned = Vec::new();
        for value in crate::experience::orb_denominations(amount) {
            if self.try_merge_to_existing(position, value) {
                continue;
            }
            spawned.push(self.spawn_orb(value, position, rough_direction));
        }
        spawned
    }

    /// `ExperienceOrb.tryMergeToExisting`: hands `value` to an orb already sitting at
    /// `position` rather than spawning a new one, if the `nextInt(40)` draw picks a
    /// congruence class one of them is in.
    ///
    /// The draw is made **whether or not a candidate exists**, matching vanilla's own
    /// order (`level.getRandom().nextInt(40)` precedes the entity query), so the roll
    /// stream does not depend on how many orbs happen to be nearby.
    fn try_merge_to_existing(&mut self, position: Vec3, value: i32) -> bool {
        let id = self.orb_rng.next_int(ORB_GROUPS_PER_AREA);
        let mut candidates: Vec<i32> = self
            .orbs
            .iter()
            .filter(|(orb_id, orb)| {
                orb.value == value
                    && (**orb_id - id) % ORB_GROUPS_PER_AREA == 0
                    && within_box(orb.motion.position, position, ORB_SPAWN_MERGE_REACH)
            })
            .map(|(&orb_id, _)| orb_id)
            .collect();
        // Vanilla takes `orbs.get(0)` out of a level query whose order is its own
        // entity-section iteration; the lowest id is the deterministic stand-in, for
        // `merge_neighbouring_items`' reason.
        candidates.sort_unstable();
        let Some(&target) = candidates.first() else {
            return false;
        };
        let Some(orb) = self.orbs.get_mut(&target) else {
            return false;
        };
        orb.count += 1;
        orb.age = 0;
        true
    }

    /// Registers one orb worth `value` points at `position`.
    ///
    /// The spawn impulse is `ExperienceOrb`'s own constructor: a random
    /// `(±0.2, +0.4, ±0.2)`-ish kick, flipped to agree with `rough_direction` when that
    /// is non-zero, and the position offset half a bounding box along it. Returns the
    /// assigned entity id.
    pub fn spawn_orb(&mut self, value: i32, position: Vec3, rough_direction: Vec3) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        let mut impulse = Vec3::new(
            (self.orb_rng.next_f64() * 0.2 - 0.1) * 2.0,
            self.orb_rng.next_f64() * 0.2 * 2.0,
            (self.orb_rng.next_f64() * 0.2 - 0.1) * 2.0,
        );
        let mut spawn_at = position;
        let bias_len_sqr = rough_direction.x * rough_direction.x
            + rough_direction.y * rough_direction.y
            + rough_direction.z * rough_direction.z;
        if bias_len_sqr > 0.0 {
            let dot = rough_direction.x * impulse.x
                + rough_direction.y * impulse.y
                + rough_direction.z * impulse.z;
            if dot < 0.0 {
                impulse = Vec3::new(-impulse.x, -impulse.y, -impulse.z);
            }
            // `getBoundingBox().getSize()` is the box's average edge length, which for
            // the orb's cube is just its width; the offset is half of it.
            let len = bias_len_sqr.sqrt();
            let scale = f64::from(ORB_DIMENSIONS.width) * 0.5 / len;
            spawn_at = Vec3::new(
                position.x + rough_direction.x * scale,
                position.y + rough_direction.y * scale,
                position.z + rough_direction.z * scale,
            );
        }
        self.orbs.insert(
            id,
            OrbState {
                uuid: Uuid::new_v4(),
                value,
                count: 1,
                age: 0,
                motion: ItemMotion::new(spawn_at, impulse),
            },
        );
        id
    }

    /// One tick of every live orb — `ExperienceOrb.tick`, in its order.
    ///
    /// The order is the part worth transcribing rather than reconstructing:
    ///
    /// 1. gravity, unless the orb is already inside a collision box;
    /// 2. `scanForMerges`, on `tickCount % 20 == 1`;
    /// 3. `followNearbyPlayer`, which *adds* to the velocity;
    /// 4. capture `fallSpeed`, then move;
    /// 5. drag — `0.98`, times the ground friction when resting;
    /// 6. the landing bounce, from the **captured** `fallSpeed`;
    /// 7. age, and discard at [`ORB_LIFETIME`].
    ///
    /// Step 3 before step 4 is what makes an orb visibly home in on a player rather
    /// than lag a tick behind them, and step 6 reading the captured speed rather than
    /// the post-drag one is why the bounce height does not decay differently from
    /// vanilla's.
    // `pub(super)`, not private: `tick_with_terrain` (mod.rs, this file's
    // *parent* module) calls this every tick.
    pub(super) fn tick_orbs(&mut self, view: &dyn CollisionView) {
        let scanning = self.tick_count % ORB_MERGE_SCAN_PERIOD == 1;
        // The follow target per orb, resolved under a shared borrow of `players`
        // before the mutable pass — `feed_perception`'s two-pass shape.
        let follow: Vec<(i32, Option<Vec3>)> = self
            .orbs
            .iter()
            .map(|(&id, orb)| (id, self.nearest_follow_target(orb.motion.position)))
            .collect();
        let min_y = f64::from(self.world.min_y);
        let mut expired: Vec<i32> = Vec::new();
        for (id, target) in follow {
            let Some(orb) = self.orbs.get_mut(&id) else {
                continue;
            };
            let before = orb.motion.position;
            orb.motion.velocity.y -= ORB_GRAVITY;
            if let Some(target) = target {
                // `followNearbyPlayer`'s pull: toward the player's *half eye height*,
                // scaled by `(1 - dist/8)^2 * 0.1`. Squaring the falloff is what makes
                // the pull negligible at the edge of the range and sharp up close; a
                // linear falloff yanks orbs from 8 blocks away.
                let delta = Vec3::new(
                    target.x - orb.motion.position.x,
                    target.y - orb.motion.position.y,
                    target.z - orb.motion.position.z,
                );
                let dist = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
                if dist > f64::EPSILON {
                    let power = 1.0 - dist / ORB_MAX_FOLLOW_DIST;
                    let pull = power * power * ORB_FOLLOW_PULL;
                    orb.motion.velocity.x += delta.x / dist * pull;
                    orb.motion.velocity.y += delta.y / dist * pull;
                    orb.motion.velocity.z += delta.z / dist * pull;
                }
            }
            let fall_speed = orb.motion.velocity.y;
            orb.motion.position = Vec3::new(
                before.x + orb.motion.velocity.x,
                before.y + orb.motion.velocity.y,
                before.z + orb.motion.velocity.z,
            );
            settle_entity(view, ORB_DIMENSIONS, &mut orb.motion, before);
            let mut drag = ORB_AIR_DRAG;
            if orb.motion.on_ground {
                drag *= orb.motion.block_friction;
            }
            orb.motion.velocity.x *= drag;
            orb.motion.velocity.y *= drag;
            orb.motion.velocity.z *= drag;
            if orb.motion.on_ground && fall_speed < -ORB_GRAVITY {
                orb.motion.velocity.y = -fall_speed * ORB_LANDING_BOUNCE;
            }
            orb.age += 1;
            if orb.age >= ORB_LIFETIME
                || orb.motion.position.y < min_y - VOID_DESPAWN_DEPTH
            {
                expired.push(id);
            }
        }
        for id in expired {
            self.orbs.remove(&id);
        }
        if scanning {
            self.scan_for_orb_merges();
        }
    }

    /// `Level.getNearestPlayer(this, 8.0)`, filtered as `followNearbyPlayer` filters
    /// it, returning the point the pull aims at.
    ///
    /// Vanilla aims at `player.getY() + player.getEyeHeight() / 2.0`, i.e. the player's
    /// *waist*, not their feet and not their eyes. Aiming at the feet makes orbs skim
    /// the floor and get stuck on a block edge; aiming at the eyes makes them arc over
    /// the player's head.
    fn nearest_follow_target(&self, orb: Vec3) -> Option<Vec3> {
        let range_sqr = ORB_MAX_FOLLOW_DIST * ORB_MAX_FOLLOW_DIST;
        let mut best: Option<(f64, Vec3)> = None;
        for player in &self.players {
            let d = dist_sqr(player.perception.position, orb);
            if d > range_sqr {
                continue;
            }
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((
                    d,
                    Vec3::new(
                        player.perception.position.x,
                        player.perception.position.y + PLAYER_EYE_HEIGHT / 2.0,
                        player.perception.position.z,
                    ),
                ));
            }
        }
        best.map(|(_, target)| target)
    }

    /// `ExperienceOrb.scanForMerges`: orbs of equal value whose ids are congruent mod
    /// [`ORB_GROUPS_PER_AREA`] and which have drifted within [`ORB_MERGE_REACH`] become
    /// one entity.
    ///
    /// `merge` takes the **minimum** of the two ages, not the absorbing orb's own — so
    /// a fresh orb absorbed into an old one resets the pile's despawn clock. Keeping
    /// the older age would make a continuously-fed pile vanish mid-feed.
    fn scan_for_orb_merges(&mut self) {
        let mut ids: Vec<i32> = self.orbs.keys().copied().collect();
        ids.sort_unstable();
        for i in 0..ids.len() {
            let to_id = ids[i];
            for j in (i + 1)..ids.len() {
                let from_id = ids[j];
                let (Some(to), Some(from)) = (self.orbs.get(&to_id), self.orbs.get(&from_id))
                else {
                    continue;
                };
                if to.value != from.value
                    || (from_id - to_id) % ORB_GROUPS_PER_AREA != 0
                    || !within_box(to.motion.position, from.motion.position, ORB_MERGE_REACH)
                {
                    continue;
                }
                let (count, age) = (from.count, from.age.min(to.age));
                self.orbs.remove(&from_id);
                if let Some(to) = self.orbs.get_mut(&to_id) {
                    to.count += count;
                    to.age = age;
                }
            }
        }
    }

    /// Every orb a player standing at `player_feet` may absorb right now, as
    /// `(entity id, value)` and lowest id first.
    ///
    /// The range test is [`crate::block_drops::is_within_pickup_range`], the same
    /// inflated-AABB intersection `Player.aiStep` uses for items — an orb has no
    /// pickup delay of its own (`ExperienceOrb` defines none), so unlike an item it
    /// *is* absorbable on the tick it spawns. What limits the rate is the **player's**
    /// `takeXpDelay`, which lives on the connection, not here.
    ///
    /// Read-only: the caller absorbs with [`take_orb`](Self::take_orb).
    #[must_use]
    pub fn orbs_within_pickup_range(&self, player_feet: Vec3) -> Vec<(i32, i32)> {
        let mut collectable: Vec<(i32, i32)> = self
            .orbs
            .iter()
            .filter(|(_, orb)| {
                crate::block_drops::is_within_pickup_range(player_feet, orb.motion.position)
            })
            .map(|(&id, orb)| (id, orb.value))
            .collect();
        collectable.sort_by_key(|&(id, _)| id);
        collectable
    }

    /// `ExperienceOrb.playerTouch`'s absorption: pays out **one** `value` and drops the
    /// orb's count by one, discarding the entity at zero.
    ///
    /// Returns the points awarded, or `None` if no orb is tracked under `id`. A merged
    /// orb therefore takes `count` calls to consume, which is the behaviour
    /// [`OrbState`]'s own doc warns is easy to collapse into a single payout.
    pub fn take_orb(&mut self, id: i32) -> Option<i32> {
        let orb = self.orbs.get_mut(&id)?;
        let value = orb.value;
        orb.count -= 1;
        if orb.count <= 0 {
            self.orbs.remove(&id);
        }
        Some(value)
    }

    /// The number of live orb *entities* — not the number of absorptions they hold.
    #[must_use]
    pub fn orb_count(&self) -> usize {
        self.orbs.len()
    }

    /// The total points every live orb would pay out if all of them were absorbed:
    /// `sum(value * count)`.
    ///
    /// The figure a conservation gate asserts on, and the reason it exists as an
    /// accessor: merging must move points between entities without creating or
    /// destroying any, and `orb_count()` alone cannot see a merge that lost a count.
    #[must_use]
    pub fn orb_points_outstanding(&self) -> i32 {
        self.orbs
            .values()
            .map(|orb| orb.value.saturating_mul(orb.count))
            .sum()
    }

    /// One orb's `(value, count, age)`, for a gate that needs to see the merge state
    /// rather than infer it.
    #[must_use]
    pub fn orb_state(&self, id: i32) -> Option<(i32, i32, i32)> {
        self.orbs.get(&id).map(|orb| (orb.value, orb.count, orb.age))
    }

    /// One orb's current position.
    #[must_use]
    pub fn orb_position(&self, id: i32) -> Option<Vec3> {
        self.orbs.get(&id).map(|orb| orb.motion.position)
    }

    /// Every live orb id, ascending.
    #[must_use]
    pub fn orb_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self.orbs.keys().copied().collect();
        ids.sort_unstable();
        ids
    }
}
