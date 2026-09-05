//! `MobSim`'s wither slice — summon-pattern detection, spawn, per-tick
//! emergence/heal/skull-fire, and the query API. Follows the split
//! `mobs::dragon` established: [`super::TrackedWither`] lives in
//! `mobs/mod.rs`, the behaviour lives here, driving [`crate::wither`] (this
//! crate's pure, world-free port of `WitherBoss`/`WitherSkull`) with real
//! inputs pulled from this sim's own state.
//!
//! # What is a real port and what is a named simplification
//!
//! * **Max health (`300.0`), the invulnerable-emergence countdown, both heal
//!   intervals, the emergence/skull-impact blast powers, the powered-armor
//!   arrow/wind-charge immunity and the skull's damage/wither-effect numbers
//!   are real ports** — driven through [`crate::wither`] exactly as that
//!   module's own doc describes.
//! * **No movement.** A live wither stays at its spawn position. Vanilla's
//!   `WitherBoss` has real `FlyingMoveControl` navigation
//!   (`WaterAvoidingRandomFlyingGoal`); this codebase's flying-mob AI has no
//!   aerial pathfinder (the same gap `mobs::dragon`'s own doc names for the
//!   ender dragon, substituted there with a simplified orbit — the wither
//!   gets no substitute at all here, a smaller scope than the dragon's own
//!   because a stationary boss is still a real fight: it out-heals, out-
//!   arms and out-shoots a player who is not careful).
//! * **One skull-firing schedule, not vanilla's three independent heads.**
//!   See `crate::wither`'s own module doc for the full accounting
//!   (`WitherBoss.DATA_TARGET_A/B/C`, indices 16-18 of the committed
//!   `entity_data_index_jvm.txt` dump). [`tick_one_wither`] fires **at most
//!   one** skull per cooldown: an **aimed** shot at the nearest player in
//!   range when one exists (vanilla's main-head targeted fire, with the
//!   real `0.1%` "dangerous" roll — [`crate::wither::should_fire_dangerous_skull`]),
//!   or, when no player is in range, an occasional **unaimed** shot toward a
//!   random nearby offset (vanilla's idle-head fallback, always dangerous) —
//!   giving both the "homing" (aimed) and "non-homing" (unaimed) variants,
//!   on a simplified single-head cadence rather than
//!   vanilla's per-head 10-30-tick jitter.
//! * **"Aimed" means "aimed at launch", not steered in flight** —
//!   [`lodestone_entity::projectile::Projectile::throwable`] does not home;
//!   neither does vanilla's own `WitherSkull` (it is a ballistic
//!   `AbstractHurtingProjectile`, same family as a blaze fireball). Calling
//!   the targeted shot "homing" is the colloquial name, not a claim that the
//!   projectile curves toward a moving target after launch.
//! * **The 220-tick emergence blast and each skull's impact blast both call
//!   [`MobSim::explode`] with no source exemption** — `explode`'s own doc
//!   already discloses it exempts no entity from its own blast; a wither
//!   caught in another wither's skull blast (vanilla: immune, via
//!   `source.getEntity() instanceof WitherBoss`) is not exempted here. Named
//!   rather than silently applied, matching this crate's existing disclosure
//!   for the creeper self-detonation path.
//! * **No block destruction.** `destroyBlocksTick`'s post-hurt block-break
//!   pulse is out of scope — see `crate::wither`'s own module doc for why
//!   (no block-write authority in this pure-detection-and-combat slice).
//! * **The wither-effect duration always assumes Normal difficulty** (`10`
//!   seconds) rather than threading a real `Difficulty` through
//!   [`MobSim::resolve_projectile_impacts`]'s per-tick call chain — that
//!   function has no difficulty parameter today and widening it would touch
//!   [`MobSim::tick`]'s own signature, a hot shared path. Disclosed rather
//!   than silently wrong: `crate::wither::wither_effect_ticks` is real and
//!   tested for all four difficulties, just not fed a real one yet.

use lodestone_model::{BlockPos, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::mob_spawn::SpawnRng;
use crate::wither as pure;

use super::wither_pattern::{self, WitherPatternMatch};
use super::{MobSim, TrackedWither};

/// The tick-start chunk owner of one wither.
///
/// This is a deterministic hand-off boundary, not a worker assignment. The
/// central application step retains the serial entity-id order before it
/// changes live wither state, triggers an emergence blast, or fires a skull.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WitherTickOwner {
    Chunk { cx: i32, cz: i32 },
}

impl WitherTickOwner {
    fn for_position(position: Vec3) -> Self {
        Self::Chunk {
            cx: (position.x.floor() as i32).div_euclid(16),
            cz: (position.z.floor() as i32).div_euclid(16),
        }
    }
}

/// One completed wither-owner batch.
///
/// The expected batch count and serial slots come from tick start. The central
/// writer validates them before one owner can make an explosion or projectile
/// visible ahead of an earlier entity-id slot.
#[derive(Debug, Clone)]
pub(crate) struct WitherTickOwnerBatch {
    owner: WitherTickOwner,
    expected_batch_count: usize,
    effects: Vec<WitherTickEffect>,
}

impl WitherTickOwnerBatch {
    #[cfg(test)]
    fn owner(&self) -> WitherTickOwner {
        self.owner
    }
}

#[derive(Debug, Clone)]
struct WitherTickEffect {
    owner: WitherTickOwner,
    serial: usize,
    id: i32,
    wither: TrackedWither,
    action: WitherTickAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum WitherTickAction {
    None,
    EmergeBlast(Vec3),
    FireSkull,
}

/// `WitherBoss.createAttributes`'s `Attributes.MAX_HEALTH`.
pub const MAX_HEALTH: f32 = 300.0;

/// Seed for [`MobSim::wither_rng`] — its own stream, matching
/// [`dragon::DRAGON_PHASE_SEED`](super::dragon)'s own reasoning: a wither's
/// dangerous-skull roll must not shift any other roll.
pub(super) const WITHER_SKULL_SEED: u64 = 0x5749_5448_4552_2121;

/// A player within this many blocks (squared) counts as a live skull target
/// — vanilla's own `RangedAttackGoal(this, 1.0, 40, 20.0F)` uses `20.0F` as
/// its *attack radius* parameter (squared internally); reused here as the
/// single flat threshold standing in for vanilla's several different named
/// ranges, exactly as `mobs::dragon`'s own `NEARBY_PLAYER_RANGE_SQ` does.
const SKULL_TARGET_RANGE_SQ: f64 = 20.0 * 20.0;

/// The simplified single-head cooldown — not a vanilla constant (vanilla has
/// three independent per-head timers, see module doc); chosen in the same
/// rough order of magnitude as vanilla's own `10 + random.nextInt(10)`
/// aimed-shot cadence and `40 + random.nextInt(20)` post-shot cooldown
/// folded into one number.
const SKULL_COOLDOWN_TICKS: i32 = 30;

/// The unaimed idle shot's own, sparser cooldown — standing in for vanilla's
/// "15 idle updates with no target" gate (`idleHeadUpdates[i] > 15`, checked
/// every `~15` ticks, so roughly `225` ticks of being targetless).
const IDLE_SKULL_COOLDOWN_TICKS: i32 = 220;

/// The entity-type key every wither streams as.
pub(super) fn wither_entity_type() -> ResourceKey {
    "minecraft:wither".parse().expect("`minecraft:wither` is a valid resource key")
}

/// The wither's own hitbox — width then height, in blocks. A
/// [`TrackedWither`](super::TrackedWither) carries no `MobShape` the way a
/// goal-driven `SimMob` does (this module doc's "no movement" disclosure is
/// the same reason: nothing needed one until now), so
/// [`MobSim::resolve_projectile_impacts`](super::MobSim::resolve_projectile_impacts)'s
/// wither candidate search (`mobs/projectiles.rs`) needs its own constant
/// rather than a shared shape lookup. See `docs/wither-fight.md` for the
/// vanilla citation.
pub(super) const HITBOX: (f32, f32) = (0.9, 3.5);

/// What [`MobSim::try_construct_wither`] built. Mirrors `golem::GolemConstruction`'s
/// shape (species/id/consumed) even though there is only one wither species,
/// so a caller's block-placement handling can treat the two the same way.
#[derive(Debug, Clone)]
pub struct WitherConstruction {
    pub id: i32,
    pub consumed: Vec<BlockPos>,
}

impl<'w> MobSim<'w> {
    /// Given a just-placed wither skull (or wall skull) at `skull_pos`,
    /// checks whether it completes the soul-sand-and-skull pattern and, if
    /// so, spawns a wither — vanilla `WitherSkullBlock.setPlacedBy` →
    /// `checkSpawn`. Same shape and same disclosed contract as
    /// [`try_construct_golem`](Self::try_construct_golem): a pure detection
    /// query with no block-write authority — the caller (the block-placement
    /// owner) clears the consumed cells and fires the level event.
    ///
    /// **Not yet called by any production code path** — see this crate's own
    /// report for the exact hunk a block-placement owner needs, mirroring
    /// `try_construct_golem`'s real call site in `crate::server`.
    pub fn try_construct_wither(
        &mut self,
        block_at: &dyn Fn(i32, i32, i32) -> String,
        skull_pos: (i32, i32, i32),
    ) -> Option<WitherConstruction> {
        let found = wither_pattern::find_wither_pattern(block_at, skull_pos)?;
        let consumed = found.consumed();
        let id = self.spawn_wither(found);
        Some(WitherConstruction { id, consumed })
    }

    fn spawn_wither(&mut self, found: WitherPatternMatch) -> i32 {
        let anchor = found.spawn_anchor();
        let position = wither_pattern::wither_anchor_to_spawn_pos(anchor);
        let yaw = wither_pattern::wither_spawn_yaw(found.forwards());
        let id = self.next_id;
        self.next_id += 1;
        self.withers.insert(
            id,
            TrackedWither {
                uuid: Uuid::new_v4(),
                position,
                yaw,
                health: pure::spawn_health(MAX_HEALTH),
                max_health: MAX_HEALTH,
                invulnerable_ticks: pure::INVULNERABLE_TICKS,
                age: 0,
                next_skull_tick: SKULL_COOLDOWN_TICKS,
            },
        );
        id
    }

    /// Spawns a wither directly at `position`, bypassing structure detection
    /// — the test/summon-command entry point, mirroring
    /// [`spawn_dragon`](Self::spawn_dragon)'s own shape.
    pub fn spawn_wither_at(&mut self, position: Vec3) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.withers.insert(
            id,
            TrackedWither {
                uuid: Uuid::new_v4(),
                position,
                yaw: 0.0,
                health: pure::spawn_health(MAX_HEALTH),
                max_health: MAX_HEALTH,
                invulnerable_ticks: pure::INVULNERABLE_TICKS,
                age: 0,
                next_skull_tick: SKULL_COOLDOWN_TICKS,
            },
        );
        id
    }

    #[must_use]
    pub fn wither_health(&self, id: i32) -> Option<f32> {
        self.withers.get(&id).map(|w| w.health)
    }

    #[must_use]
    pub fn wither_invulnerable_ticks(&self, id: i32) -> Option<i32> {
        self.withers.get(&id).map(|w| w.invulnerable_ticks)
    }

    #[must_use]
    pub fn wither_position(&self, id: i32) -> Option<Vec3> {
        self.withers.get(&id).map(|w| w.position)
    }

    /// One tick of every live wither: the emergence countdown (with its
    /// emergence blast on the tick it ends), heal ticks, and skull firing.
    pub fn tick_withers(&mut self) {
        let batches = self.tick_wither_owner_batches();
        self.apply_wither_tick_owner_batches(batches);
    }

    /// Produces independent wither-owner completions from cloned tick-start
    /// state. No completion mutates the live map or fires an action; central
    /// application restores serial entity-id order before either becomes live.
    pub(crate) fn tick_wither_owner_batches(&self) -> Vec<WitherTickOwnerBatch> {
        let mut ids: Vec<i32> = self.withers.keys().copied().collect();
        ids.sort_unstable();
        let mut batches = Vec::<WitherTickOwnerBatch>::new();
        for (serial, id) in ids.into_iter().enumerate() {
            let wither = self
                .withers
                .get(&id)
                .cloned()
                .expect("a tick-start wither id must remain live while planning");
            let owner = WitherTickOwner::for_position(wither.position);
            let (wither, action) = ticked_wither(wither);
            let effect = WitherTickEffect {
                owner,
                serial,
                id,
                wither,
                action,
            };
            if let Some(batch) = batches.iter_mut().find(|batch| batch.owner == owner) {
                batch.effects.push(effect);
            } else {
                batches.push(WitherTickOwnerBatch {
                    owner,
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

    /// Validates and centrally applies completed wither-owner batches.
    ///
    /// This is the only owner-batch path that writes the live wither map or
    /// consumes the skull RNG. The serial slots keep projectile ids and blast
    /// effects independent of the order in which owners complete.
    pub(crate) fn apply_wither_tick_owner_batches(&mut self, batches: Vec<WitherTickOwnerBatch>) {
        if batches.is_empty() {
            return;
        }
        let effects = merge_wither_tick_owner_batches(batches);
        assert_eq!(
            effects.len(),
            self.withers.len(),
            "wither owner completion must retain every live tick-start entity"
        );
        let mut ids = std::collections::HashSet::new();
        for effect in effects {
            assert!(
                ids.insert(effect.id),
                "wither owner completion may update one live entity only once"
            );
            assert!(
                self.withers.contains_key(&effect.id),
                "a wither owner completion may update only a live tick-start wither"
            );
            self.withers.insert(effect.id, effect.wither);
            match effect.action {
                WitherTickAction::None => {}
                WitherTickAction::EmergeBlast(position) => {
                    self.explode(position, pure::EMERGE_EXPLOSION_POWER, lodestone_entity::DamageFlags::default());
                }
                WitherTickAction::FireSkull => self.maybe_fire_skull(effect.id),
            }
        }
    }

    fn maybe_fire_skull(&mut self, id: i32) {
        // Copy out just what's needed and drop the borrow immediately — the
        // target search below reads `self.players` and the eventual spawn
        // needs `&mut self`, neither of which can coexist with a live borrow
        // of `self.withers`.
        let Some((origin, age, ready)) =
            self.withers.get(&id).map(|w| (w.position, w.age, w.next_skull_tick <= 0))
        else {
            return;
        };
        if !ready {
            return;
        }

        let nearest_player = self
            .players
            .iter()
            .filter_map(|p| p.identity.map(|_| p.perception.position))
            .map(|pos| (pos, dist_sq(origin, pos)))
            .filter(|(_, d)| *d <= SKULL_TARGET_RANGE_SQ)
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).expect("distances are always finite"));

        // `WitherBoss.performRangedAttack`'s two shapes: an **aimed** shot at
        // a real target (vanilla's main head, real `TargetingConditions`
        // range), or, with nothing in range, an **unaimed** shot toward a
        // random nearby offset (vanilla's idle-head fallback,
        // `idleHeadUpdates[i] > 15`) — see module doc for why this is one
        // schedule rather than three.
        let (target, cooldown) = match nearest_player {
            Some((pos, _)) => (pos, SKULL_COOLDOWN_TICKS),
            None => {
                let mut rng = SpawnRng::new(WITHER_SKULL_SEED ^ (id as u64) ^ (age as u64));
                let offset = Vec3::new(
                    (rng.next_f64() - 0.5) * 20.0,
                    (rng.next_f64() - 0.5) * 10.0,
                    (rng.next_f64() - 0.5) * 20.0,
                );
                (origin + offset, IDLE_SKULL_COOLDOWN_TICKS)
            }
        };

        // `head == 0 && random.nextFloat() < 0.001F` — rolled and named, but
        // with no consumer yet: see module doc for why (no block-destruction
        // model, no `MetadataField::WitherSkullDangerous` to carry it to a
        // client).
        let dangerous = pure::should_fire_dangerous_skull(self.wither_rng.next_f32());
        let _ = dangerous;

        let delta = target - origin;
        let dir = if delta.length() > 1e-6 { delta.normalize() } else { Vec3::new(0.0, 0.0, 1.0) };
        let velocity = dir.scale(1.0);
        let projectile = lodestone_entity::projectile::Projectile::throwable(origin, velocity);
        self.spawn_projectile_from(
            "minecraft:wither_skull".parse().expect("valid key"),
            projectile,
            Some(id),
        );

        if let Some(w) = self.withers.get_mut(&id) {
            w.next_skull_tick = cooldown;
        }
    }

    /// Applies `damage` to a live wither through the emergence-invulnerability
    /// and powered-armor gates — the `WitherBoss.hurtServer` clauses that are
    /// phase-state rather than plain health subtraction. `is_arrow_or_wind_charge`
    /// is the caller's classification of the direct damage source (see
    /// [`crate::wither::blocks_projectile_while_powered`]'s own doc).
    /// `bypasses_invulnerability` lets a caller force a hit through the
    /// emergence phase for the rare damage types that carry that tag — most
    /// callers pass `false`. Returns the resulting health, or `None` if `id`
    /// is not a live wither, or if the hit was refused outright (health
    /// unchanged either way — `Some` vs `None` here is "did the hit land",
    /// matching `hurtServer`'s own `bool` return repurposed as an `Option`).
    ///
    /// A wither reduced to `0.0` or below is removed from the sim on this
    /// call, matching vanilla's own death — this crate's `MobSim` has no
    /// separate "dying" phase for the wither the way [`crate::dragon::phase`]
    /// gives the ender dragon (`WitherBoss` has no analogous redirect;
    /// `handleKillingBlow` is an `EnderDragon`-only override).
    pub fn damage_wither(&mut self, id: i32, damage: f32, is_arrow_or_wind_charge: bool, bypasses_invulnerability: bool) -> Option<f32> {
        let w = self.withers.get_mut(&id)?;
        if pure::blocked_by_emerging_invulnerability(w.invulnerable_ticks, bypasses_invulnerability) {
            return None;
        }
        let is_powered_now = pure::is_powered(w.health, w.max_health);
        if pure::blocks_projectile_while_powered(is_powered_now, is_arrow_or_wind_charge) {
            return None;
        }
        w.health = (w.health - damage).max(0.0);
        let health = w.health;
        if health <= 0.0 {
            self.withers.remove(&id);
        }
        Some(health)
    }

    pub(super) fn push_wither_snapshots(&self, out: &mut Vec<crate::protocol::EntitySnapshot>) {
        let mut ids: Vec<i32> = self.withers.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(w) = self.withers.get(&id) else { continue };
            out.push(crate::protocol::EntitySnapshot {
                id,
                uuid: w.uuid,
                entity_type: wither_entity_type(),
                position: w.position,
                rotation: Rotation::new(w.yaw, 0.0),
                head_yaw: w.yaw,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                // `WitherBoss.DATA_ID_INV` — drives the client-side "still
                // emerging" shield visual while the summon animation plays.
                metadata: vec![crate::protocol::MetadataField::WitherInvulnerableTicks(
                    w.invulnerable_ticks,
                )],
                object_data: 0,
                leash_link: None,
            });
        }
    }

    /// The wither half of [`MobSim::boss_bars`] — see
    /// `WitherBoss`'s own `ServerBossEvent` construction
    /// (`BossEvent.BossBarColor.PURPLE`, `BossEvent.BossBarOverlay.PROGRESS`,
    /// `setDarkenScreen(true)`); the darken-screen flag has no carrier in
    /// [`crate::protocol::BossBarSnapshot`] today (see this crate's own
    /// report).
    pub(super) fn push_wither_boss_bars(&self, out: &mut Vec<crate::protocol::BossBarSnapshot>) {
        let mut ids: Vec<i32> = self.withers.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(w) = self.withers.get(&id) else { continue };
            let progress = if w.invulnerable_ticks > 0 {
                pure::boss_bar_progress_while_invulnerable(w.invulnerable_ticks)
            } else {
                pure::boss_bar_progress_while_active(w.health, w.max_health)
            };
            out.push(crate::protocol::BossBarSnapshot {
                id: w.uuid,
                name: lodestone_model::Text::translate("entity.minecraft.wither", Vec::new()),
                progress,
                visible: true,
            });
        }
    }
}

fn dist_sq(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

fn ticked_wither(mut wither: TrackedWither) -> (TrackedWither, WitherTickAction) {
    wither.age += 1;
    if wither.invulnerable_ticks > 0 {
        let (new_ticks, emerge_effect) = pure::invulnerable_tick(wither.invulnerable_ticks);
        wither.invulnerable_ticks = new_ticks;
        if pure::should_heal_while_invulnerable(wither.age) && wither.health < wither.max_health {
            wither.health = (wither.health + pure::HEAL_AMOUNT_INVULNERABLE).min(wither.max_health);
        }
        let action = if new_ticks == 0 && emerge_effect == Some(pure::WitherEffect::EmergeBlast) {
            WitherTickAction::EmergeBlast(wither.position)
        } else {
            WitherTickAction::None
        };
        return (wither, action);
    }

    if pure::should_heal_while_active(wither.age) && wither.health < wither.max_health {
        wither.health = (wither.health + pure::HEAL_AMOUNT_ACTIVE).min(wither.max_health);
    }
    wither.next_skull_tick -= 1;
    (wither, WitherTickAction::FireSkull)
}

fn merge_wither_tick_owner_batches(
    mut batches: Vec<WitherTickOwnerBatch>,
) -> Vec<WitherTickEffect> {
    let expected_batch_count = batches
        .first()
        .map(|batch| batch.expected_batch_count)
        .expect("wither owner completion must contain every tick-start owner batch");
    let mut owners = std::collections::HashSet::new();
    for batch in &batches {
        assert_eq!(
            batch.expected_batch_count, expected_batch_count,
            "wither owner completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "wither owner completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "a wither owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "wither owner completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "wither owner completion must retain every tick-start serial slot exactly once"
        );
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ChunkWorld;

    fn sim() -> MobSim<'static> {
        // A real floor at `y = 63` under every test's own `y = 64.0` spawn
        // height — a bare void `ChunkWorld` stopped being safe once idle
        // mobs got real gravity (see `NavigatingMob::advance`'s no-waypoint
        // branch): an ordinary (non-wither) target left to idle for many
        // ticks would fall out of a horizontally-travelling skull's flight
        // path instead of standing still at the height every test spawns it
        // at.
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=16 {
            for z in -8..=8 {
                world.set_solid(x, 63, z, true);
            }
        }
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        MobSim::new(world)
    }

    #[test]
    fn a_spawned_wither_starts_at_one_third_health_and_invulnerable() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        assert_eq!(sim.wither_health(id), Some(MAX_HEALTH / 3.0));
        assert_eq!(sim.wither_invulnerable_ticks(id), Some(pure::INVULNERABLE_TICKS));
    }

    #[test]
    fn a_wither_is_streamed_and_visible() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        let snap = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("a live wither must be streamed, or it reaches zero pixels");
        assert_eq!(snap.entity_type, wither_entity_type());
    }

    #[test]
    fn a_wither_streams_its_real_invulnerable_ticks_as_metadata() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        let snap = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("a live wither must be streamed");
        assert_eq!(
            snap.metadata,
            vec![crate::protocol::MetadataField::WitherInvulnerableTicks(pure::INVULNERABLE_TICKS)],
            "the client's emerging-shield visual reads this field; a wither must not stream with empty metadata"
        );

        for _ in 0..220 {
            sim.tick_withers();
        }
        let snap = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("a live wither must be streamed");
        assert_eq!(
            snap.metadata,
            vec![crate::protocol::MetadataField::WitherInvulnerableTicks(0)],
            "invulnerable_ticks must track the sim's real countdown, not a frozen spawn-time value"
        );
    }

    #[test]
    fn a_wither_heals_during_emergence_and_becomes_active_after_220_ticks() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        for _ in 0..219 {
            sim.tick_withers();
        }
        assert!(sim.wither_invulnerable_ticks(id).unwrap() > 0, "still emerging one tick before the 220th");
        assert!(
            sim.wither_health(id).unwrap() > MAX_HEALTH / 3.0,
            "the 10 HP/10-tick heal must have raised health above the 1/3 spawn value"
        );
        sim.tick_withers();
        assert_eq!(sim.wither_invulnerable_ticks(id), Some(0), "invulnerability must end at exactly tick 220");
    }

    #[test]
    fn damage_is_blocked_entirely_while_emerging() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        let before = sim.wither_health(id).unwrap();
        let result = sim.damage_wither(id, 50.0, false, false);
        assert_eq!(result, None, "an emerging wither must refuse the hit entirely");
        assert_eq!(sim.wither_health(id), Some(before), "health must not change on a refused hit");
    }

    #[test]
    fn damage_lands_once_active_and_powered_armor_blocks_only_arrows() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        for _ in 0..220 {
            sim.tick_withers();
        }
        assert_eq!(sim.wither_invulnerable_ticks(id), Some(0));

        // Push it below half health with a melee-style hit (not an
        // arrow/wind-charge), which must always land.
        let after_first = sim.damage_wither(id, MAX_HEALTH * 0.6, false, false);
        assert!(after_first.unwrap() < MAX_HEALTH / 2.0, "must be below half health now (isPowered)");

        // Now powered: an arrow-family hit must be refused...
        let before = sim.wither_health(id).unwrap();
        let refused = sim.damage_wither(id, 10.0, true, false);
        assert_eq!(refused, None, "powered armor must block an arrow-family hit");
        assert_eq!(sim.wither_health(id), Some(before));

        // ...but a non-arrow hit still lands while powered.
        let landed = sim.damage_wither(id, 10.0, false, false);
        assert_eq!(landed, Some(before - 10.0));
    }

    #[test]
    fn a_killing_blow_removes_the_wither_from_the_sim() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        for _ in 0..220 {
            sim.tick_withers();
        }
        let result = sim.damage_wither(id, 10_000.0, false, false);
        assert_eq!(result, Some(0.0));
        assert!(sim.wither_health(id).is_none(), "a dead wither must leave the sim");
        assert!(
            sim.snapshots().into_iter().all(|s| s.entity_type != wither_entity_type()),
            "a dead wither must stop streaming"
        );
    }

    #[test]
    fn an_active_wither_with_a_nearby_player_fires_a_skull() {
        let mut sim = sim();
        let id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        for _ in 0..220 {
            sim.tick_withers();
        }
        sim.set_players(vec![crate::PerceivedPlayer {
            identity: Some(crate::PlayerIdentity {
                uuid: uuid::Uuid::new_v4(),
                entity_id: 1,
            }),
            perception: crate::PlayerPerception {
                position: Vec3::new(5.0, 64.0, 0.0),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }]);
        let before = sim.projectile_count();
        for _ in 0..SKULL_COOLDOWN_TICKS {
            sim.tick_withers();
        }
        assert!(sim.projectile_count() > before, "a nearby target must eventually draw skull fire");
    }

    fn owner_batch_fixture() -> MobSim<'static> {
        let mut sim = sim();
        for position in [Vec3::new(-0.5, 64.0, -0.5), Vec3::new(16.5, 64.0, 0.5)] {
            let id = sim.spawn_wither_at(position);
            let wither = sim.withers.get_mut(&id).expect("just spawned");
            wither.invulnerable_ticks = 0;
            wither.next_skull_tick = 0;
        }
        sim
    }

    #[test]
    fn wither_owner_batches_restore_serial_state_after_reversed_completion() {
        let mut completed = owner_batch_fixture();
        let mut batches = completed.tick_wither_owner_batches();
        assert_eq!(
            batches.iter().map(WitherTickOwnerBatch::owner).collect::<Vec<_>>(),
            vec![
                WitherTickOwner::Chunk { cx: -1, cz: -1 },
                WitherTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "tick-start wither positions must determine distinct negative and positive chunk owners"
        );
        let expected: Vec<_> = merge_wither_tick_owner_batches(batches.clone())
            .into_iter()
            .map(|effect| {
                let next_skull_tick = match effect.action {
                    WitherTickAction::FireSkull => IDLE_SKULL_COOLDOWN_TICKS,
                    WitherTickAction::None | WitherTickAction::EmergeBlast(_) => effect.wither.next_skull_tick,
                };
                (effect.id, effect.wither.age, next_skull_tick)
            })
            .collect();
        batches.reverse();
        let projectiles_before = completed.projectile_count();
        completed.apply_wither_tick_owner_batches(batches);

        let actual: Vec<_> = expected
            .iter()
            .map(|(id, _, _)| {
                let wither = completed.withers.get(id).expect("central owner merge retained wither");
                (*id, wither.age, wither.next_skull_tick)
            })
            .collect();
        assert_eq!(actual, expected, "reversed owners must restore tick-start entity-id state order");
        assert_eq!(
            completed.projectile_count(),
            projectiles_before + 2,
            "central application must consume both ready skull actions after restoring serial order"
        );
    }

    #[test]
    #[should_panic(expected = "every tick-start owner batch exactly once")]
    fn wither_owner_batch_merge_rejects_a_missing_owner() {
        let mut sim = owner_batch_fixture();
        let mut batches = sim.tick_wither_owner_batches();
        batches.pop();
        sim.apply_wither_tick_owner_batches(batches);
    }

    #[test]
    #[should_panic(expected = "one owner twice")]
    fn wither_owner_batch_merge_rejects_a_duplicate_owner() {
        let mut sim = owner_batch_fixture();
        let mut batches = sim.tick_wither_owner_batches();
        batches.push(batches.first().expect("two owner batches").clone());
        sim.apply_wither_tick_owner_batches(batches);
    }

    #[test]
    fn the_summon_pattern_spawns_a_real_wither() {
        let mut sim = sim();
        let world = |x: i32, y: i32, z: i32| -> String {
            let cells: &[((i32, i32, i32), &str)] = &[
                ((9, 6, 10), "minecraft:wither_skeleton_skull"),
                ((10, 6, 10), "minecraft:wither_skeleton_skull"),
                ((11, 6, 10), "minecraft:wither_skeleton_skull"),
                ((9, 5, 10), "minecraft:soul_sand"),
                ((10, 5, 10), "minecraft:soul_sand"),
                ((11, 5, 10), "minecraft:soul_sand"),
                ((10, 4, 10), "minecraft:soul_soil"),
            ];
            cells
                .iter()
                .find(|(p, _)| *p == (x, y, z))
                .map(|(_, n)| (*n).to_owned())
                .unwrap_or_else(|| "minecraft:air".to_owned())
        };
        let result = sim
            .try_construct_wither(&world, (10, 6, 10))
            .expect("a complete wither pattern must spawn a wither");
        assert_eq!(result.consumed.len(), 7, "three skulls plus three soul sand plus one soul soil");
        assert_eq!(sim.wither_health(result.id), Some(MAX_HEALTH / 3.0));
        let pos = sim.wither_position(result.id).unwrap();
        assert_eq!(pos, Vec3::new(10.5, 4.55, 10.5), "spawn position is the base block's cell, offset per vanilla's own snapTo");
    }

    #[test]
    fn control_an_incomplete_pattern_spawns_nothing() {
        let mut sim = sim();
        let world = |x: i32, y: i32, z: i32| -> String {
            if (x, y, z) == (10, 6, 10) {
                "minecraft:wither_skeleton_skull".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        };
        assert!(sim.try_construct_wither(&world, (10, 6, 10)).is_none());
    }

    fn spawn_target<'w>(sim: &mut MobSim<'w>, pos: Vec3) -> i32 {
        sim.spawn(pos, lodestone_entity::pathfinding::MobShape::land(0.6, 1.95), 0.2, 32)
            .id()
    }

    /// **The discriminating gate for the skull-impact chain**: a wither
    /// skull's damage is flat (`WitherSkull.onHitEntity`'s `8.0F`), not
    /// speed-scaled the way an arrow's is
    /// (`lodestone_entity::projectile::arrow_impact_damage`) — two shots at
    /// very different speeds must deal the *same* damage, which an
    /// arrow-shaped formula would not. Also checks the landed hit applies
    /// `minecraft:wither` (a real status effect, not merely "some effect").
    #[test]
    fn a_wither_skull_deals_flat_not_speed_scaled_damage_and_applies_wither() {
        let mut mismatches: Vec<String> = Vec::new();
        let mut dealt_by_speed: Vec<f32> = Vec::new();
        // Both speeds must fully cross the 3-block gap within one tick's
        // segment — `resolve_projectile_impacts` tests only the step about
        // to be taken, not the whole flight, so a speed that would need more
        // than one tick to arrive is a fixture bug, not a real "slow shot".
        for speed in [5.0, 20.0] {
            let mut sim = sim();
            let wither_id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
            let target = spawn_target(&mut sim, Vec3::new(3.0, 64.0, 0.0));
            let before = sim.get(target).expect("just spawned").health();
            sim.spawn_projectile_from(
                "minecraft:wither_skull".parse().expect("valid key"),
                lodestone_entity::projectile::Projectile::throwable(
                    Vec3::new(0.0, 64.0, 0.0),
                    Vec3::new(speed, 0.0, 0.0),
                ),
                Some(wither_id),
            );
            sim.resolve_projectile_impacts();
            let Some(after_mob) = sim.get(target) else {
                mismatches.push(format!("speed {speed}: target did not survive a single skull hit"));
                continue;
            };
            let dealt = before - after_mob.health();
            if dealt <= 0.0 {
                mismatches.push(format!("speed {speed}: expected nonzero damage, dealt {dealt}"));
            }
            dealt_by_speed.push(dealt);
            let has_wither = after_mob.effects().get("minecraft:wither").is_some();
            if !has_wither {
                mismatches.push(format!("speed {speed}: expected minecraft:wither effect, found none"));
            }
        }
        if dealt_by_speed.len() == 2 && (dealt_by_speed[0] - dealt_by_speed[1]).abs() > 1e-4 {
            mismatches.push(format!(
                "damage must not scale with speed (flat 8.0 base): got {:?} at 0.5 vs 4.0 blocks/tick",
                dealt_by_speed
            ));
        }
        assert!(mismatches.is_empty(), "wither skull impact mismatches:\n{}", mismatches.join("\n"));
    }

    /// A killing skull hit heals the shooting wither — `WitherSkull.
    /// onHitEntity`'s `livingOwner.heal(5.0F)`, capped at max health. The
    /// wither is first damaged so the heal is observable rather than a
    /// silent no-op against a full-health owner.
    #[test]
    fn a_killing_skull_hit_heals_the_owning_wither() {
        let mut sim = sim();
        let wither_id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        for _ in 0..220 {
            sim.tick_withers();
        }
        sim.damage_wither(wither_id, 20.0, false, false);
        let before = sim.wither_health(wither_id).unwrap();

        // Bring a target to 1 HP through the public damage API so the
        // single skull hit that follows is lethal. `SimMob::apply_damage`
        // runs through the same i-frame gate a real hit does
        // (`HurtDecision::Ignored`/`Topup` for a same-or-smaller follow-up
        // hit within the cooldown window), so a real number of ticks has to
        // pass before the skull's own hit can land at full force again.
        let target = spawn_target(&mut sim, Vec3::new(3.0, 64.0, 0.0));
        let starting_health = sim.get(target).expect("just spawned").health();
        if let Some(m) = sim.get_mut(target) {
            m.apply_damage(starting_health - 1.0, lodestone_entity::DamageFlags::default());
        }
        for _ in 0..30 {
            sim.tick();
        }
        assert!(sim.get(target).expect("still alive at 1 hp").health() > 0.0);

        sim.spawn_projectile_from(
            "minecraft:wither_skull".parse().expect("valid key"),
            lodestone_entity::projectile::Projectile::throwable(Vec3::new(0.0, 64.0, 0.0), Vec3::new(10.0, 0.0, 0.0)),
            Some(wither_id),
        );
        sim.resolve_projectile_impacts();

        assert!(sim.get(target).is_none(), "the killing skull hit must remove the target");
        let after = sim.wither_health(wither_id).unwrap();
        assert_eq!(after, (before + pure::OWNER_HEAL_ON_KILL).min(MAX_HEALTH), "the owner must heal 5.0 HP, capped at max health");
        assert!(after > before, "control: the heal must actually be observable, not a no-op against full health");
    }

    /// **The discriminating gate for the melee-combat island**: before
    /// `MobSim::attack_from_player` grew a wither branch, a wither lived
    /// only in `self.withers`, `self.attack` reads and writes only
    /// `self.mobs`, and `self.get_mut(wither_id)` returns `None` — so this
    /// call landed on `self.attack(target_id, ..)?`'s early return and
    /// `attack_from_player` returned `None` for *every* melee hit against a
    /// wither, silently. A wither still emerging must refuse the hit
    /// outright (the same invulnerability gate `damage_wither` already has
    /// its own coverage for); this only proves the *routing* — that a
    /// melee hit reaches a live, active wither at all.
    #[test]
    fn a_players_melee_hit_damages_an_active_wither() {
        let mut sim = sim();
        let wither_id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        for _ in 0..220 {
            sim.tick_withers();
        }
        let before = sim.wither_health(wither_id).expect("still live");
        let outcome = sim.attack_from_player(
            wither_id,
            Some(crate::PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 1 }),
            Vec3::new(3.0, 64.0, 0.0),
            7.0,
            lodestone_entity::DamageFlags::default(),
            0.0,
        );
        let outcome = outcome.expect("a melee hit against an active wither must land");
        assert!(outcome.damage_dealt > 0.0, "control: the outcome must report real damage, not a no-op");
        let after = sim.wither_health(wither_id).expect("still live after a non-lethal hit");
        assert!(after < before, "a player's melee attack must actually reduce the wither's health");
    }

    /// The reciprocal control for the test above: a still-emerging wither's
    /// invulnerability gate must still refuse a melee hit through the new
    /// routing, exactly as it already does for `damage_wither` called
    /// directly — proving the branch in `attack_from_player` did not bypass
    /// that gate on its way in.
    #[test]
    fn a_players_melee_hit_is_refused_while_the_wither_is_still_emerging() {
        let mut sim = sim();
        let wither_id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        let before = sim.wither_health(wither_id).expect("still live");
        let outcome = sim.attack_from_player(
            wither_id,
            None,
            Vec3::new(3.0, 64.0, 0.0),
            7.0,
            lodestone_entity::DamageFlags::default(),
            0.0,
        );
        assert!(outcome.is_none(), "an emerging wither must refuse a melee hit outright");
        assert_eq!(sim.wither_health(wither_id), Some(before), "control: health must be unchanged by the refused hit");
    }

    /// The projectile counterpart of the two tests above: before
    /// `resolve_projectile_impacts` grew a wither candidate search, an
    /// arrow's own nearest-mob scan only read `self.mobs`, so an arrow shot
    /// at a wither found no target and passed straight through — the same
    /// island, one call site over.
    #[test]
    fn a_players_arrow_damages_an_active_wither() {
        let mut sim = sim();
        let wither_id = sim.spawn_wither_at(Vec3::new(0.0, 64.0, 0.0));
        for _ in 0..220 {
            sim.tick_withers();
        }
        let before = sim.wither_health(wither_id).expect("still live");
        sim.spawn_projectile_from(
            "minecraft:arrow".parse().expect("valid key"),
            lodestone_entity::projectile::Projectile::throwable(Vec3::new(3.0, 64.0, 0.0), Vec3::new(-10.0, 0.0, 0.0)),
            None,
        );
        sim.resolve_projectile_impacts();
        let after = sim.wither_health(wither_id).expect("still live after a non-lethal hit");
        assert!(after < before, "a player's arrow must actually reduce the wither's health");
    }
}
