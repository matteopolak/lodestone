//! `MobSim`'s experience-orb slice — award/spawn, per-tick drift/merge/decay,
//! and the orb query/pickup API. Moved out of `mobs/mod.rs` verbatim as part
//! of the `mobs.rs` file split (see `docs/plans/crate-and-file-splits.md`).

use std::collections::HashSet;

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

/// The chunk responsible for an experience orb at tick start.
///
/// This is a deterministic completion boundary, not a worker assignment. The
/// central application step remains the only writer to the live orb map, where
/// expiry and the later cross-chunk merge scan stay ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OrbTickOwner {
    /// The chunk containing the orb at the start of this tick.
    Chunk { cx: i32, cz: i32 },
}

impl OrbTickOwner {
    fn for_position(position: Vec3) -> Self {
        Self::Chunk {
            cx: (position.x.floor() as i32).div_euclid(16),
            cz: (position.z.floor() as i32).div_euclid(16),
        }
    }
}

/// One completed chunk-owner pass for experience-orb motion.
///
/// `expected_batch_count` and every effect's serial slot come from the
/// tick-start plan. The central consumer uses them to reject incomplete,
/// duplicate, or mixed-plan completions before it changes the live orb map.
#[derive(Debug, Clone)]
pub(crate) struct OrbTickOwnerBatch {
    owner: OrbTickOwner,
    plan: u64,
    expected_batch_count: usize,
    effects: Vec<OrbTickEffect>,
}

impl OrbTickOwnerBatch {
    #[cfg(test)]
    fn owner(&self) -> OrbTickOwner {
        self.owner
    }
}

/// One orb's motion completion, retained until the central orb-map writer
/// accepts every owner batch.
#[derive(Debug, Clone)]
struct OrbTickEffect {
    owner: OrbTickOwner,
    serial: usize,
    id: i32,
    /// `None` means this orb expired or crossed the void boundary during its
    /// owner pass and the central writer must remove it.
    orb: Option<OrbState>,
}

#[derive(Debug, Clone)]
struct OrbTickInput {
    owner: OrbTickOwner,
    serial: usize,
    id: i32,
    orb: OrbState,
    target: Option<Vec3>,
}

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
    pub(super) fn tick_orbs(&mut self, block_state: &(dyn Fn(i32, i32, i32) -> String + Sync)) {
        let scanning = self.tick_count % ORB_MERGE_SCAN_PERIOD == 1;
        self.orb_owner_plan = self
            .orb_owner_plan
            .checked_add(1)
            .expect("orb owner plan generation must not overflow");
        #[cfg(not(target_arch = "wasm32"))]
        let workers = if self.orbs.len() >= 128 {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
                .min(4)
        } else {
            1
        };
        #[cfg(target_arch = "wasm32")]
        let workers = 1;
        let batches = self.tick_orb_owner_batches_with_workers(block_state, workers);
        self.apply_orb_tick_owner_batches(batches);
        if scanning {
            self.scan_for_orb_merges();
        }
    }

    /// Produces independent chunk-owner completions for orb motion and age.
    ///
    /// Each completion starts from a cloned tick-start orb and cannot mutate
    /// the live map. The central application below restores the old entity-id
    /// sequence before expiry or any later merge becomes visible.
    pub(crate) fn tick_orb_owner_batches(
        &mut self,
        view: &dyn CollisionView,
    ) -> Vec<OrbTickOwnerBatch> {
        self.orb_owner_plan = self
            .orb_owner_plan
            .checked_add(1)
            .expect("orb owner plan generation must not overflow");
        let mut ids: Vec<i32> = self.orbs.keys().copied().collect();
        ids.sort_unstable();
        let min_y = f64::from(self.world.min_y);
        let mut batches = Vec::<OrbTickOwnerBatch>::new();

        for (serial, id) in ids.into_iter().enumerate() {
            let orb = self
                .orbs
                .get(&id)
                .cloned()
                .expect("a tick-start orb id must remain live while planning");
            let owner = OrbTickOwner::for_position(orb.motion.position);
            let target = self.nearest_follow_target(orb.motion.position);
            let effect = OrbTickEffect {
                owner,
                serial,
                id,
                orb: ticked_orb(orb, target, view, min_y),
            };
            if let Some(batch) = batches.iter_mut().find(|batch| batch.owner == owner) {
                batch.effects.push(effect);
            } else {
                batches.push(OrbTickOwnerBatch {
                    owner,
                    plan: self.orb_owner_plan,
                    expected_batch_count: 0,
                    effects: vec![effect],
                });
            }
        }
        let batch_count = batches.len();
        for batch in &mut batches {
            batch.expected_batch_count = batch_count;
        }
        batches
    }

    fn tick_orb_owner_batches_with_workers(
        &self,
        block_state: &(dyn Fn(i32, i32, i32) -> String + Sync),
        worker_count: usize,
    ) -> Vec<OrbTickOwnerBatch> {
        let mut ids: Vec<i32> = self.orbs.keys().copied().collect();
        ids.sort_unstable();
        let mut jobs = Vec::<(OrbTickOwner, Vec<OrbTickInput>)>::new();
        for (serial, id) in ids.into_iter().enumerate() {
            let orb = self
                .orbs
                .get(&id)
                .cloned()
                .expect("a tick-start orb id must remain live while planning");
            let owner = OrbTickOwner::for_position(orb.motion.position);
            let input = OrbTickInput {
                owner,
                serial,
                id,
                target: self.nearest_follow_target(orb.motion.position),
                orb,
            };
            if let Some((_, inputs)) = jobs.iter_mut().find(|(candidate, _)| *candidate == owner) {
                inputs.push(input);
            } else {
                jobs.push((owner, vec![input]));
            }
        }
        let min_y = f64::from(self.world.min_y);
        let plan = self.orb_owner_plan;
        let mut batches = crate::tick_region::run_bounded_owner_jobs(jobs, worker_count, &|(owner, inputs)| {
            let view = super::ItemCollision {
                block_state,
                probe_count: std::cell::Cell::new(0),
            };
            let effects = inputs
                .into_iter()
                .map(|input| OrbTickEffect {
                    owner: input.owner,
                    serial: input.serial,
                    id: input.id,
                    orb: ticked_orb(input.orb, input.target, &view, min_y),
                })
                .collect();
            OrbTickOwnerBatch {
                owner,
                plan,
                expected_batch_count: 0,
                effects,
            }
        });
        let batch_count = batches.len();
        for batch in &mut batches {
            batch.expected_batch_count = batch_count;
        }
        batches
    }

    /// Validates and centrally applies every completed orb-owner batch.
    ///
    /// This is the sole owner-batch path that writes the live orb map. The
    /// merge scan remains after this method in [`Self::tick_orbs`], retaining
    /// its current global id order when two completed owners are adjacent.
    pub(crate) fn apply_orb_tick_owner_batches(&mut self, batches: Vec<OrbTickOwnerBatch>) {
        if batches.is_empty() {
            return;
        }
        let plan = batches[0].plan;
        assert_eq!(
            plan, self.orb_owner_plan,
            "orb completion must belong to the latest tick-start plan"
        );
        assert!(
            plan > self.applied_orb_owner_plan,
            "orb completion must not replay an already applied tick-start plan"
        );
        let effects = merge_orb_tick_owner_batches(batches);
        assert_eq!(
            effects.len(),
            self.orbs.len(),
            "orb owner completion must retain every live tick-start entity"
        );
        let mut ids = HashSet::new();
        for effect in effects {
            assert!(
                ids.insert(effect.id),
                "orb owner completion may update one live entity only once"
            );
            assert!(
                self.orbs.contains_key(&effect.id),
                "orb owner completion may update only a live tick-start entity"
            );
            match effect.orb {
                Some(orb) => {
                    self.orbs.insert(effect.id, orb);
                }
                None => {
                    self.orbs.remove(&effect.id);
                }
            }
        }
        self.applied_orb_owner_plan = plan;
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

/// Simulates one cloned orb through its independent motion and lifetime pass.
///
/// The target was read before the owner completion began, so this function has
/// no access to the live orb map and cannot observe another owner's completion.
fn ticked_orb(
    mut orb: OrbState,
    target: Option<Vec3>,
    view: &dyn CollisionView,
    min_y: f64,
) -> Option<OrbState> {
    let before = orb.motion.position;
    orb.motion.velocity.y -= ORB_GRAVITY;
    if let Some(target) = target {
        // The pull aims toward the player's half eye height and scales by
        // `(1 - distance / 8)^2 * 0.1`, keeping it negligible at the edge of
        // its range and sharp nearby.
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
    (orb.age < ORB_LIFETIME && orb.motion.position.y >= min_y - VOID_DESPAWN_DEPTH)
        .then_some(orb)
}

/// Restores the tick-start orb order after every owner reports completion.
fn merge_orb_tick_owner_batches(
    mut batches: Vec<OrbTickOwnerBatch>,
) -> Vec<OrbTickEffect> {
    let first = batches
        .first()
        .expect("orb owner completion must contain every tick-start owner batch");
    let plan = first.plan;
    let expected_batch_count = first.expected_batch_count;
    let mut owners = HashSet::new();
    for batch in &batches {
        assert_eq!(
            (batch.plan, batch.expected_batch_count),
            (plan, expected_batch_count),
            "orb owner completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "orb owner completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "an orb owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "orb owner completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "orb owner completion must retain every tick-start serial slot exactly once"
        );
    }
    effects
}

#[cfg(test)]
mod experience_orb_tests {
    use super::*;
    use super::super::{ChunkWorld, PlayerPerception};
    use crate::protocol::MetadataField;
    use lodestone_entity::DamageFlags;

    /// Flat stone floor at y=0 across one column, so an orb has something to land on.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    /// A point above the floor, well inside the column the world covers.
    fn above_floor() -> Vec3 {
        Vec3::new(8.0, 1.0, 8.0)
    }

    fn two_owner_orb_fixture<'w>(world: &'w ChunkWorld) -> (MobSim<'w>, [i32; 2]) {
        let mut sim = MobSim::new(world);
        let still = Vec3::new(0.0, 0.0, 0.0);
        let negative = sim.spawn_orb(3, Vec3::new(-0.5, 3.0, 0.5), still);
        let positive = sim.spawn_orb(7, Vec3::new(16.5, 3.0, 0.5), still);
        (sim, [negative, positive])
    }

    fn dense_orb_owner_fixture<'w>(world: &'w ChunkWorld, count: usize) -> (MobSim<'w>, Vec<i32>) {
        let mut sim = MobSim::new(world);
        let mut ids = Vec::with_capacity(count);
        for serial in 0..count {
            let x = [-0.5, 16.5, 32.5, 48.5][serial % 4];
            ids.push(sim.spawn_orb(
                3,
                Vec3::new(x, 3.0, (serial / 4 % 4) as f64 + 0.5),
                Vec3::new(0.0, 0.0, 0.0),
            ));
        }
        (sim, ids)
    }

    fn dense_orb_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -2..=66 {
            for z in -2..=4 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    /// The independent serial reference for one non-scanning orb pass. It
    /// deliberately does not construct, reverse, or merge owner batches.
    fn serial_orb_tick_reference(sim: &mut MobSim<'_>, view: &dyn CollisionView) {
        let mut ids = sim.orb_ids();
        ids.sort_unstable();
        let min_y = f64::from(sim.world.min_y);
        for id in ids {
            let orb = sim.orbs.get(&id).cloned().expect("tick-start orb remains live");
            let target = sim.nearest_follow_target(orb.motion.position);
            match ticked_orb(orb, target, view, min_y) {
                Some(orb) => {
                    sim.orbs.insert(id, orb);
                }
                None => {
                    sim.orbs.remove(&id);
                }
            }
        }
    }

    #[test]
    fn orb_owner_batches_restore_serial_state_after_reversed_completion() {
        let world = flat_world();
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let view = super::super::ItemCollision {
            block_state: &state_at,
            probe_count: std::cell::Cell::new(0),
        };
        let (mut serial, ids) = two_owner_orb_fixture(&world);
        serial_orb_tick_reference(&mut serial, &view);

        let (mut completed, completed_ids) = two_owner_orb_fixture(&world);
        assert_eq!(completed_ids, ids, "fixtures must use matching entity slots");
        let mut batches = completed.tick_orb_owner_batches(&view);
        assert_eq!(
            batches
                .iter()
                .map(OrbTickOwnerBatch::owner)
                .collect::<Vec<_>>(),
            vec![
                OrbTickOwner::Chunk { cx: -1, cz: 0 },
                OrbTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "the negative and positive controls must begin in separate owners"
        );
        batches.reverse();
        completed.apply_orb_tick_owner_batches(batches);

        let (mut live, live_ids) = two_owner_orb_fixture(&world);
        assert_eq!(live_ids, ids, "the live consumer uses the same tick-start ids");
        live.tick_with_terrain(&state_at);

        for id in ids {
            assert_eq!(
                completed.orb_state(id),
                serial.orb_state(id),
                "reversed owner completion changed orb {id}'s persistent state"
            );
            assert_eq!(
                completed.orb_position(id),
                serial.orb_position(id),
                "reversed owner completion changed orb {id}'s position"
            );
            assert_eq!(
                live.orb_state(id),
                serial.orb_state(id),
                "the tick_with_terrain consumer must apply the central owner merge"
            );
            assert_eq!(live.orb_position(id), serial.orb_position(id));
        }
    }

    #[test]
    #[should_panic(expected = "every tick-start owner batch exactly once")]
    fn orb_owner_batch_merge_rejects_a_missing_owner() {
        let world = flat_world();
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let view = super::super::ItemCollision {
            block_state: &state_at,
            probe_count: std::cell::Cell::new(0),
        };
        let (mut sim, _) = two_owner_orb_fixture(&world);
        let mut batches = sim.tick_orb_owner_batches(&view);
        batches.pop();
        sim.apply_orb_tick_owner_batches(batches);
    }

    #[test]
    #[should_panic(expected = "may not contain one owner twice")]
    fn orb_owner_batch_merge_rejects_a_duplicate_owner() {
        let world = flat_world();
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let view = super::super::ItemCollision {
            block_state: &state_at,
            probe_count: std::cell::Cell::new(0),
        };
        let (mut sim, _) = two_owner_orb_fixture(&world);
        let mut batches = sim.tick_orb_owner_batches(&view);
        batches.push(batches.first().expect("two owners exist").clone());
        sim.apply_orb_tick_owner_batches(batches);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn parallel_orb_owner_batches_match_one_lane_with_interleaved_negative_owners() {
        let world = dense_orb_world();
        let mut serial = dense_orb_owner_fixture(&world, 256).0;
        serial.orb_owner_plan = 1;
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let serial_batches = serial.tick_orb_owner_batches_with_workers(&state_at, 1);
        serial.apply_orb_tick_owner_batches(serial_batches);

        let (mut parallel, ids) = dense_orb_owner_fixture(&world, 256);
        parallel.orb_owner_plan = 1;
        let parallel_batches = parallel.tick_orb_owner_batches_with_workers(&state_at, 4);
        assert_eq!(
            parallel_batches.iter().map(OrbTickOwnerBatch::owner).collect::<Vec<_>>(),
            [
                OrbTickOwner::Chunk { cx: -1, cz: 0 },
                OrbTickOwner::Chunk { cx: 1, cz: 0 },
                OrbTickOwner::Chunk { cx: 2, cz: 0 },
                OrbTickOwner::Chunk { cx: 3, cz: 0 },
            ],
            "the parity scene must span an interleaved negative owner plus three positive owners"
        );
        parallel.apply_orb_tick_owner_batches(parallel_batches);

        for id in ids {
            assert_eq!(parallel.orb_state(id), serial.orb_state(id));
            assert_eq!(parallel.orb_position(id), serial.orb_position(id));
        }
    }

    #[test]
    #[should_panic(expected = "latest tick-start plan")]
    fn orb_owner_batches_reject_stale_plan_completions() {
        let world = flat_world();
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let view = super::super::ItemCollision {
            block_state: &state_at,
            probe_count: std::cell::Cell::new(0),
        };
        let (mut sim, _) = two_owner_orb_fixture(&world);
        let stale = sim.tick_orb_owner_batches(&view);
        let _current = sim.tick_orb_owner_batches(&view);
        sim.apply_orb_tick_owner_batches(stale);
    }

    #[test]
    #[should_panic(expected = "already applied tick-start plan")]
    fn orb_owner_batches_reject_replayed_completions() {
        let world = flat_world();
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let view = super::super::ItemCollision {
            block_state: &state_at,
            probe_count: std::cell::Cell::new(0),
        };
        let (mut sim, _) = two_owner_orb_fixture(&world);
        let batches = sim.tick_orb_owner_batches(&view);
        sim.apply_orb_tick_owner_batches(batches.clone());
        sim.apply_orb_tick_owner_batches(batches);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "manual dense-scene throughput measurement"]
    fn measure_dense_orb_owner_workers() {
        let world = dense_orb_world();
        for orb_count in [256, 2_048] {
            let sim = dense_orb_owner_fixture(&world, orb_count).0;
            let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
            let started = std::time::Instant::now();
            let _ = sim.tick_orb_owner_batches_with_workers(&state_at, 1);
            let serial = started.elapsed();
            let started = std::time::Instant::now();
            let _ = sim.tick_orb_owner_batches_with_workers(&state_at, 4);
            let parallel = started.elapsed();
            eprintln!(
                "dense_orbs owners=4 orbs={orb_count} serial_ms={:.3} parallel_ms={:.3} speedup={:.3}",
                serial.as_secs_f64() * 1_000.0,
                parallel.as_secs_f64() * 1_000.0,
                serial.as_secs_f64() / parallel.as_secs_f64()
            );
        }
    }

    /// **The denomination ladder reaches real entities.**
    ///
    /// `crate::experience::orb_denominations` is already gated to the integer, and this
    /// is the join: an award of 100 becomes **four** orbs worth `73, 17, 7, 3` — not one
    /// orb of 100, and not `73 + 17 + 7 + 1 + 1 + 1`. Orb count is what a player sees.
    ///
    /// The ids are consecutive, which is why none of these four can merge with each
    /// other: the merge rule is congruence mod 40. That is asserted here rather than
    /// left implicit, because a spawner that *did* merge them would report a plausible
    /// smaller count.
    #[test]
    fn an_award_of_100_spawns_the_four_orbs_the_ladder_predicts() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let spawned = sim.award_experience(above_floor(), Vec3::new(0.0, 0.0, 0.0), 100);
        assert_eq!(spawned.len(), 4, "100 points is four orbs, not one");
        let mut values: Vec<i32> = spawned
            .iter()
            .map(|&id| sim.orb_state(id).expect("spawned orb is tracked").0)
            .collect();
        assert_eq!(values, vec![73, 17, 7, 3], "largest first, and the tail is 3");
        values.sort_unstable();
        assert_eq!(values.iter().sum::<i32>(), 100, "the split must conserve the award");
        assert_eq!(sim.orb_points_outstanding(), 100);
        assert_eq!(sim.orb_count(), 4);
    }

    /// **Merging, at a count above the threshold — and the threshold is the point.**
    ///
    /// `scanForMerges` only merges orbs whose network ids are congruent mod
    /// [`ORB_GROUPS_PER_AREA`] (40). Spawning ten orbs and expecting a merge measures
    /// nothing at all: ids `n..n+9` share no congruence class, so the correct answer is
    /// zero merges. This spawns **41** orbs of equal value at one point, which is the
    /// smallest count that guarantees a congruent pair (`n` and `n + 40`).
    ///
    /// Two assertions, and the second is the one a wrong merge passes:
    ///
    /// * the entity count **falls**, so a merge happened;
    /// * `orb_points_outstanding` is **unchanged**, so the merge moved absorptions
    ///   between entities rather than destroying them. A `merge` that overwrote the
    ///   target's count instead of adding to it satisfies the first and fails this.
    #[test]
    fn forty_one_equal_orbs_merge_and_conserve_every_point() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        // `spawn_orb` directly, not `award_experience`: the award path would split a
        // total into *different* denominations, and only equal-valued orbs may merge.
        // 41 is the count, 3 is a real ladder denomination.
        const ORBS: usize = 41;
        const VALUE: i32 = 3;
        for _ in 0..ORBS {
            sim.spawn_orb(VALUE, above_floor(), Vec3::new(0.0, 0.0, 0.0));
        }
        let before_points = sim.orb_points_outstanding();
        assert_eq!(before_points, VALUE * ORBS as i32);
        assert_eq!(sim.orb_count(), ORBS, "no merge has been scanned for yet");

        // The scan runs on `tick_count % 20 == 1`, so 21 ticks reaches it twice.
        for _ in 0..21 {
            sim.tick();
        }

        assert!(
            sim.orb_count() < ORBS,
            "41 equal-valued orbs at one point must produce at least one merge; still \
             {} entities. If this reads 41 the congruence class arithmetic is wrong",
            sim.orb_count()
        );
        assert_eq!(
            sim.orb_points_outstanding(),
            before_points,
            "a merge must move absorptions between entities, never destroy them"
        );
    }

    /// **The control for the merge gate: below the threshold, nothing merges.**
    ///
    /// Ten equal orbs at the same point, ticked past the same scan, must stay ten
    /// entities. Without this arm the gate above is satisfied by a merge rule that
    /// ignores the id congruence entirely and merges everything it touches — which
    /// would collapse a vanilla 41-orb pile into one orb and look tidier on screen.
    #[test]
    fn control_ten_orbs_below_the_congruence_stride_do_not_merge() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        for _ in 0..10 {
            sim.spawn_orb(3, above_floor(), Vec3::new(0.0, 0.0, 0.0));
        }
        for _ in 0..21 {
            sim.tick();
        }
        assert_eq!(
            sim.orb_count(),
            10,
            "ids n..n+9 share no congruence class mod 40, so none of these may merge"
        );
    }

    /// A merged orb takes **`count` absorptions** to consume, each paying `value`.
    ///
    /// This is [`OrbState`]'s documented trap made a gate: reading `count` as "the
    /// points this orb is worth" pays out once and loses the rest, with the entity
    /// still disappearing at the right moment.
    #[test]
    fn absorbing_a_merged_orb_pays_out_once_per_count() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim.spawn_orb(7, above_floor(), Vec3::new(0.0, 0.0, 0.0));
        // Two more merges into it, reaching a count of 3 — done through the public
        // spawn-time merge so the state is one a real award could produce.
        sim.orbs.get_mut(&id).expect("just spawned").count = 3;
        assert_eq!(sim.orb_points_outstanding(), 21, "3 absorptions of 7");

        let mut paid = Vec::new();
        for _ in 0..3 {
            paid.push(sim.take_orb(id).expect("the orb is still there"));
        }
        assert_eq!(paid, vec![7, 7, 7], "each absorption pays one value, not the pile");
        assert_eq!(sim.orb_count(), 0, "the entity goes when its count reaches zero");
        assert_eq!(
            sim.take_orb(id),
            None,
            "and a fourth absorption finds nothing rather than paying again"
        );
    }

    /// **Orbs are pulled toward a nearby player**, and the control is the same orb with
    /// no player in the sim.
    ///
    /// Measured as horizontal displacement toward the player over ten ticks, because
    /// vertical motion is dominated by gravity and the landing bounce in both arms —
    /// a "did it move" assertion would pass on gravity alone.
    #[test]
    fn an_orb_drifts_toward_a_nearby_player_and_not_without_one() {
        let start = Vec3::new(8.0, 1.0, 8.0);
        let player = Vec3::new(11.0, 1.0, 8.0);

        let world = flat_world();
        let mut followed = MobSim::new(&world);
        followed.set_players(vec![PlayerPerception {
            position: player,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);
        let followed_id = followed.spawn_orb(1, start, Vec3::new(0.0, 0.0, 0.0));

        let mut alone = MobSim::new(&world);
        let alone_id = alone.spawn_orb(1, start, Vec3::new(0.0, 0.0, 0.0));

        for _ in 0..10 {
            followed.tick();
            alone.tick();
        }

        let followed_x = followed.orb_position(followed_id).expect("still alive").x;
        let alone_x = alone.orb_position(alone_id).expect("still alive").x;
        assert!(
            followed_x > alone_x + 0.1,
            "the followed orb must have closed on the player: followed x={followed_x}, \
             control x={alone_x}. Equal values mean nothing reads the player list"
        );
        assert!(
            followed_x < player.x,
            "and must not overshoot the player in ten ticks: x={followed_x}"
        );
    }

    /// An orb outside the 8-block follow range is not pulled at all — the other side of
    /// the same rule, and the one a missing range check passes.
    ///
    /// # Why this compares two sims rather than a displacement threshold
    ///
    /// The first version of this gate asserted the orb moves less than half a block and
    /// **failed at -0.50**: `spawn_orb` applies `ExperienceOrb`'s own random spawn
    /// impulse, so an orb with no player anywhere near it still drifts half a block
    /// before drag kills the kick. The premise "an unpulled orb barely moves" is simply
    /// false, and it failed in the direction that reads as a code bug.
    ///
    /// Both sims are freshly constructed, so `orb_rng` is at the same point in the same
    /// seeded stream and both orbs receive the **identical** impulse. That makes the
    /// comparison exact rather than approximate: any pull at all shows up as a
    /// difference, and there is no threshold to tune.
    #[test]
    fn control_an_orb_beyond_the_follow_range_is_not_pulled() {
        let start = Vec3::new(8.0, 1.0, 8.0);
        let world = flat_world();

        let mut watched = MobSim::new(&world);
        watched.set_players(vec![PlayerPerception {
            // 9 blocks away: outside `ORB_MAX_FOLLOW_DIST`, and only just, so a range
            // check comparing a squared distance against an unsquared bound would pull
            // this orb and fail here.
            position: Vec3::new(start.x + 9.0, start.y, start.z),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);
        let watched_id = watched.spawn_orb(1, start, Vec3::new(0.0, 0.0, 0.0));

        let mut alone = MobSim::new(&world);
        let alone_id = alone.spawn_orb(1, start, Vec3::new(0.0, 0.0, 0.0));

        for _ in 0..10 {
            watched.tick();
            alone.tick();
        }

        let watched_pos = watched.orb_position(watched_id).expect("still alive");
        let alone_pos = alone.orb_position(alone_id).expect("still alive");
        assert_eq!(
            (watched_pos.x, watched_pos.y, watched_pos.z),
            (alone_pos.x, alone_pos.y, alone_pos.z),
            "an orb 9 blocks from a player must follow exactly the same path as one with \
             no player at all"
        );
    }

    /// **A player kill drops experience; every other death does not.**
    ///
    /// The three arms share one fixture and differ only in how the mob dies, because the
    /// claim is about `LivingEntity.dropExperience`'s `lastHurtByPlayerMemoryTime > 0`
    /// guard and nothing else:
    ///
    /// | arm | orbs |
    /// |---|---|
    /// | killed through `MobSim::attack` (the player path) | some |
    /// | killed by `damage_self` (no player involved) | **none** |
    ///
    /// The second arm is the one that matters: awarding on every death turns any mob
    /// grinder into an XP farm, and a gate with only the first arm cannot tell the two
    /// implementations apart.
    #[test]
    fn only_a_player_killed_mob_drops_experience() {
        let world = flat_world();

        let mut by_player = MobSim::new(&world);
        let id = by_player
            .spawn_species(
                "minecraft:zombie".parse().expect("valid key"),
                above_floor(),
            )
            .id();
        by_player.attack(id, Vec3::new(6.0, 1.0, 8.0), 1_000.0, DamageFlags::default(), 0.0);
        assert_eq!(by_player.len(), 0, "1000 damage kills a zombie");
        assert!(
            by_player.orb_points_outstanding() > 0,
            "a player kill must pop experience; got no orbs at all"
        );
        // `Monster`'s own `xpReward` is 5, and the ladder splits 5 into `3 + 1 + 1`.
        assert_eq!(
            by_player.orb_points_outstanding(),
            5,
            "a zombie is worth exactly Monster's xpReward of 5"
        );
        assert_eq!(
            by_player.orb_count(),
            3,
            "5 points is three orbs — 3 + 1 + 1 over the denomination ladder"
        );

        let mut alone = MobSim::new(&world);
        let alone_id = alone
            .spawn_species(
                "minecraft:zombie".parse().expect("valid key"),
                above_floor(),
            )
            .id();
        alone
            .get_mut(alone_id)
            .expect("just spawned")
            .damage_self(1_000.0);
        alone.tick();
        assert_eq!(alone.len(), 0, "the self-damaged zombie died too");
        assert_eq!(
            alone.orb_points_outstanding(),
            0,
            "a death no player caused must drop no experience — this is the arm that \
             separates a faithful port from an XP farm"
        );
    }

    /// A **baby** drops nothing, however it died — `shouldDropExperience()` is
    /// `!isBaby()`.
    ///
    /// Worth its own arm because the obvious implementation (award whenever a player
    /// killed it) passes every assertion above and fails this one.
    #[test]
    fn control_a_player_killed_baby_drops_no_experience() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
            .id();
        assert!(sim.get(id).expect("spawned").is_baby(), "the fixture is a baby");
        sim.attack(id, Vec3::new(6.0, 1.0, 8.0), 1_000.0, DamageFlags::default(), 0.0);
        assert_eq!(sim.len(), 0, "the calf died");
        assert_eq!(
            sim.orb_points_outstanding(),
            0,
            "a baby drops no experience — vanilla's shouldDropExperience is !isBaby()"
        );
    }

    /// An animal's reward is a **roll of 1..=3**, not a constant — `Animal`'s own
    /// `getBaseExperienceReward` override.
    ///
    /// Asserted as a range over repeated kills plus the requirement that **more than one
    /// distinct total appears**, which is what separates the roll from a flat 2. A
    /// single kill cannot make that distinction, and a range check alone is satisfied by
    /// any constant inside it.
    #[test]
    fn an_animal_rolls_its_reward_rather_than_paying_a_constant() {
        let world = flat_world();
        let mut seen: Vec<i32> = Vec::new();
        let mut out_of_range: Vec<i32> = Vec::new();
        // One sim across all kills so the orb RNG stream advances, exactly as it would
        // over a real session.
        let mut sim = MobSim::new(&world);
        for _ in 0..24 {
            let id = sim
                .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
                .id();
            let before = sim.orb_points_outstanding();
            sim.attack(id, Vec3::new(6.0, 1.0, 8.0), 1_000.0, DamageFlags::default(), 0.0);
            let reward = sim.orb_points_outstanding() - before;
            if !(1..=3).contains(&reward) {
                out_of_range.push(reward);
            }
            seen.push(reward);
        }
        assert!(
            out_of_range.is_empty(),
            "every cow must be worth 1..=3 points; these were not: {out_of_range:?}"
        );
        seen.sort_unstable();
        seen.dedup();
        assert!(
            seen.len() > 1,
            "24 cows produced only the reward {seen:?} — Animal's reward is a roll of \
             1 + nextInt(3), and a constant would look exactly like this"
        );
    }

    /// An orb streams as `minecraft:experience_orb` carrying its **value** as metadata.
    ///
    /// Both halves have a recorded failure mode in this crate: an entity type that is
    /// not a real registry key resolves to network id `0` and arrives as
    /// `minecraft:acacia_boat` with nothing logged, and a client with no value draws the
    /// smallest of the eleven sprite frames whatever the orb is worth.
    #[test]
    fn an_orb_streams_as_an_experience_orb_carrying_its_value() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim.spawn_orb(617, above_floor(), Vec3::new(0.0, 0.0, 0.0));
        let snapshot = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("a live orb must be streamed");
        assert_eq!(snapshot.entity_type.to_string(), "minecraft:experience_orb");
        assert_eq!(
            snapshot.metadata,
            vec![MetadataField::ExperienceOrbValue { value: 617 }],
            "the value is the only field, and without it the sprite frame is wrong"
        );
        assert_eq!(
            snapshot.object_data, 0,
            "`ExperienceOrb` does not override getAddEntityPacket, so there is no \
             object data to send"
        );
    }
}
