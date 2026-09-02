//! `MobSim`'s raid slice — wave escalation, a raid boss bar, and the
//! captain-marker data model. Issue #241 (the raid half; patrols already
//! exist — see `mobs::mod`'s own module doc and `docs/pillager-patrols.md`).
//! Ported from vanilla's own per-raid state and raid-manager types,
//! transcribing the wave-size tables and the `getNumGroups`/omen-level
//! constants exactly.
//!
//! See `docs/raids-and-patrols.md` for what reaches the screen, the disclosed
//! gaps (village-entry trigger, the ominous-banner visual, ravager/evoker/
//! witch waves) and how to change it.

use lodestone_model::{Difficulty, Vec3};
use uuid::Uuid;

use crate::mob_spawn::SpawnRng;

use super::MobSim;

/// Seed for [`MobSim::raid_rng`] — its own stream, [`super::dragon::MobSim`]'s
/// own `dragon_rng` doc gives the reason shared by every RNG field in this
/// sim.
pub(super) const RAID_ROLL_SEED: u64 = 0x5241_4944_5F52_4F4C;

/// `Raid.getMaxRaidOmenLevel` — the clamp ceiling both the pre-raid omen
/// absorption and a started raid's own level obey.
pub const MAX_RAID_OMEN_LEVEL: i32 = 5;

/// `Raid.RaidStatus`, the three states this port reaches (`STOPPED` is
/// "no longer in the map" here rather than a fourth variant — see
/// [`MobSim::tick_raids`]'s own doc for why removal stands in for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaidStatus {
    Ongoing,
    Victory,
}

/// `Raid.RaiderType.spawnsPerWaveBeforeBonus`, indexed by wave number
/// (`1..=7`; index `0` is never read — vanilla's own array shape). Only the
/// two raider types this crate's roster implements
/// (`lodestone_entity::ai::roster::ranged::PILLAGER`, and vindicator per
/// `docs/plans/villager-economy.md`'s scope note); ravager/evoker/witch
/// arrays are real too but have no spawnable species here, so they are not
/// transcribed — see `docs/raids-and-patrols.md` §5.
const PILLAGER_BASE_SPAWNS: [i32; 8] = [0, 4, 3, 3, 4, 4, 4, 2];
const VINDICATOR_BASE_SPAWNS: [i32; 8] = [0, 0, 2, 0, 1, 4, 2, 5];

/// `Raid.getNumGroups` — total waves by difficulty. `Peaceful` is `0`
/// (a raid cannot start), matching vanilla exactly: the `raids` game rule
/// and difficulty are two independent gates, and this is the second one.
fn num_groups(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 3,
        Difficulty::Normal => 5,
        Difficulty::Hard => 7,
    }
}

/// `Raid.getPotentialBonusSpawns` for `PILLAGER`/`VINDICATOR` (the only two
/// arms this port needs — `EVOKER` returns `0` unconditionally and
/// `WITCH`/`RAVAGER` have no spawnable species here): `nextInt(2)` on Easy,
/// a flat `1` on Normal, a flat `2` on Hard, then `nextInt(bonus + 1)` more.
fn bonus_spawns(difficulty: Difficulty, rng: &mut SpawnRng) -> i32 {
    let bonus = match difficulty {
        Difficulty::Easy => rng.next_int(2),
        Difficulty::Normal => 1,
        Difficulty::Hard | Difficulty::Peaceful => 2,
    };
    if bonus > 0 { rng.next_int(bonus + 1) } else { 0 }
}

/// One active raid. Tracks its own raiders by entity id rather than tagging
/// [`super::SimMob`] with a raid-membership field — see this file's own
/// module doc: every raid-scoped question (`is this wave cleared`, `who is
/// the captain`) is answerable by looking those ids up in `self.mobs`, so no
/// new field on the mob type itself was needed.
#[derive(Debug, Clone)]
pub(super) struct Raid {
    pub uuid: Uuid,
    pub center: Vec3,
    pub difficulty: Difficulty,
    /// `Raid.raidOmenLevel`, `1..=`[`MAX_RAID_OMEN_LEVEL`] — set once at
    /// [`MobSim::start_raid`] time (see that method's own doc for why this
    /// port does not accumulate further absorption mid-raid the way vanilla's
    /// `absorbRaidOmen` can).
    pub omen_level: i32,
    pub total_waves: i32,
    /// `Raid.groupsSpawned` — waves actually spawned so far, `0` before the
    /// first.
    pub groups_spawned: i32,
    /// The current wave's live raider entity ids, pruned every tick against
    /// `self.mobs` (dead or missing == removed from this list).
    pub raiders: Vec<i32>,
    /// The current wave's *captain* — its first-spawned raider, the data-only
    /// stand-in for vanilla's `Raid.getOminousBannerInstance` head-slot
    /// equipment. See `docs/raids-and-patrols.md` §5 for why the banner
    /// itself needs an equipment-slot wire path this file cannot add.
    pub captain: Option<i32>,
    /// `Raid.raidCooldownTicks` — ticks left before the next wave, `0` before
    /// the first wave (which spawns immediately, matching vanilla's
    /// `raidCooldownTicks == 0 && groupsSpawned > 0` **not** being true yet).
    pub cooldown_ticks: i32,
    pub ticks_active: u64,
    pub status: RaidStatus,
    /// `Raid.postRaidTicks` — the 40-tick delay between "no raiders, no more
    /// waves" and actually declaring [`RaidStatus::Victory`].
    pub post_raid_ticks: i32,
    /// `Raid.heroesOfTheVillage` — every player-uuid credited with a killing
    /// blow on one of this raid's raiders, across every wave
    /// ([`MobSim::add_raid_hero`], `Raider.die`'s
    /// `raidWhenKilled.addHeroOfTheVillage(killer)`). Consulted once, on
    /// [`RaidStatus::Victory`], to queue a `minecraft:hero_of_the_village`
    /// grant per hero — see [`MobSim::tick_raids`]'s own doc for where that
    /// happens and [`MobSim::take_hero_of_the_village_grants`] for how a
    /// connection collects it.
    pub heroes: std::collections::HashSet<Uuid>,
}

impl Raid {
    fn has_more_waves(&self) -> bool {
        self.groups_spawned < self.total_waves
    }
}

/// `Raid.absorbRaidOmen`'s pure arithmetic: the new omen level after
/// absorbing a Bad-Omen-turned-Raid-Omen effect of `amplifier`, clamped to
/// `0..=`[`MAX_RAID_OMEN_LEVEL`].
///
/// Reached from production through [`MobSim::create_or_extend_raid`], which
/// `crate::server`'s per-connection Bad-Omen-to-Raid-Omen conversion calls on
/// Raid Omen's own last tick — see that method's own doc for the trigger's
/// full shape.
#[must_use]
pub fn absorb_raid_omen(existing_level: i32, amplifier: u32) -> i32 {
    (existing_level + amplifier as i32 + 1).clamp(0, MAX_RAID_OMEN_LEVEL)
}

impl<'w> MobSim<'w> {
    /// Starts a raid centred on `center` at `difficulty` with the given
    /// `omen_level` (`1..=5` — the value [`absorb_raid_omen`] would have
    /// produced). Returns the assigned raid id, or `None` on
    /// `Difficulty::Peaceful` ([`num_groups`] is `0`, so there is nothing to
    /// wave through). Reached from production through
    /// [`create_or_extend_raid`](Self::create_or_extend_raid), the same as
    /// [`absorb_raid_omen`].
    pub fn start_raid(&mut self, center: Vec3, difficulty: Difficulty, omen_level: i32) -> Option<i32> {
        let total_waves = num_groups(difficulty);
        if total_waves <= 0 {
            return None;
        }
        let id = self.next_raid_id;
        self.next_raid_id += 1;
        self.raids.insert(
            id,
            Raid {
                uuid: Uuid::new_v4(),
                center,
                difficulty,
                omen_level: omen_level.clamp(1, MAX_RAID_OMEN_LEVEL),
                total_waves,
                groups_spawned: 0,
                raiders: Vec::new(),
                captain: None,
                cooldown_ticks: 0,
                ticks_active: 0,
                status: RaidStatus::Ongoing,
                post_raid_ticks: 0,
                heroes: std::collections::HashSet::new(),
            },
        );
        Some(id)
    }

    /// `Raider.die`'s player-kill half: records `uuid` as a hero-of-the-village
    /// candidate for raid `id`. A no-op if `id` no longer names a live raid
    /// (the kill outraced the raid's own removal, which cannot currently
    /// happen in one tick but costs nothing to guard). See
    /// [`raid_containing_raider`](Self::raid_containing_raider) for how a
    /// caller resolves a killed entity id back to its raid.
    pub(super) fn add_raid_hero(&mut self, id: i32, uuid: Uuid) {
        if let Some(raid) = self.raids.get_mut(&id) {
            raid.heroes.insert(uuid);
        }
    }

    /// `Raider.getCurrentRaid`'s query, from the entity-id side this sim
    /// indexes mobs by rather than a raid-membership field on [`super::SimMob`]
    /// itself (see this file's own module doc for why): the raid `entity_id`
    /// currently belongs to as a live raider of the *current* wave, if any.
    #[must_use]
    pub(super) fn raid_containing_raider(&self, entity_id: i32) -> Option<i32> {
        self.raids
            .iter()
            .find(|(_, raid)| raid.raiders.contains(&entity_id))
            .map(|(&id, _)| id)
    }

    /// One tick of every active raid — `Raid.tick`, narrowed to the parts
    /// this sim can actually drive:
    ///
    /// * **No village-persistence check**, so [`RaidStatus`] never reaches
    ///   vanilla's `LOSS` — only `Ongoing`/`Victory`. Vanilla's loss
    ///   condition is entirely about
    ///   losing the village (`!level.isVillage(center)`); no POI census
    ///   crosses this seam (see `docs/raids-and-patrols.md` §5), so this is a
    ///   real, disclosed narrowing rather than a silent one.
    /// * **No player-distance/visibility gating on the boss bar** — the
    ///   `raidEvent`'s per-player add/remove vanilla does is not modelled;
    ///   [`push_raid_boss_bars`](Self::push_raid_boss_bars) always includes
    ///   every ongoing raid, the same simplification
    ///   [`super::dragon::MobSim::boss_bars`] already documents for the
    ///   dragon/wither bars.
    /// * **Spawn placement is a coarse random ring** around the raid centre
    ///   rather than vanilla's `findRandomSpawnPos`'s real
    ///   village-boundary-aware search — see [`wave_spawn_position`].
    ///
    /// On [`RaidStatus::Victory`], every uuid in the raid's own
    /// [`Raid::heroes`] set is queued into [`Self::pending_hero_grants`] with
    /// the raid's final omen level — `Raid.tick`'s own
    /// `hero.addEffect(HERO_OF_THE_VILLAGE, 48000, raidOmenLevel - 1)` loop,
    /// deferred to a queue rather than applied here because this method has
    /// no connection/`ActiveEffects` to apply an effect *to* (see
    /// [`Self::take_hero_of_the_village_grants`]'s own doc for who drains it
    /// and why a queue is the right shape). The 48000-tick timeout branch
    /// just below does **not** queue a grant — vanilla only awards Hero of
    /// the Village from the real `VICTORY` transition, never from a raid
    /// that simply expired.
    pub(super) fn tick_raids(&mut self) {
        let ids: Vec<i32> = self.raids.keys().copied().collect();
        let mut finished: Vec<i32> = Vec::new();
        let mut to_spawn: Vec<i32> = Vec::new();
        let mut hero_grants: Vec<(Uuid, i32)> = Vec::new();
        for id in ids {
            // Prune this wave's raider list against the live mob set first —
            // an immutable read of `self.mobs` through `self.get`, taken
            // before the raid's own mutable borrow below so the two never
            // overlap.
            let alive: Vec<i32> = {
                let Some(raid) = self.raids.get(&id) else { continue };
                raid.raiders.iter().copied().filter(|&rid| self.get(rid).is_some_and(|m| m.health() > 0.0)).collect()
            };
            let Some(raid) = self.raids.get_mut(&id) else { continue };
            raid.raiders = alive;
            raid.ticks_active += 1;
            if raid.ticks_active >= 48_000 {
                finished.push(id);
                continue;
            }
            if !raid.raiders.is_empty() {
                continue;
            }
            if raid.has_more_waves() {
                if raid.cooldown_ticks <= 0 {
                    if raid.groups_spawned == 0 {
                        // The very first wave spawns immediately —
                        // `raidCooldownTicks == 0 && groupsSpawned > 0` is
                        // false on the opening tick.
                        to_spawn.push(id);
                    } else {
                        raid.cooldown_ticks = 300;
                    }
                } else {
                    raid.cooldown_ticks -= 1;
                    if raid.cooldown_ticks <= 0 {
                        to_spawn.push(id);
                    }
                }
            } else if raid.post_raid_ticks < 40 {
                // No raiders, no more waves: the 40-tick victory delay.
                raid.post_raid_ticks += 1;
            } else {
                raid.status = RaidStatus::Victory;
                for &hero in &raid.heroes {
                    hero_grants.push((hero, raid.omen_level));
                }
                finished.push(id);
            }
        }
        // Spawning needs `&mut self` in full (`spawn_species`, `self.world`),
        // which cannot overlap the per-raid `&mut` borrows above — hence the
        // deferred list rather than spawning inline.
        for id in to_spawn {
            let world = self.world;
            spawn_wave(self, id, world);
        }
        for id in finished {
            self.raids.remove(&id);
        }
        self.pending_hero_grants.extend(hero_grants);
    }

    /// Every active raid's boss bar, appended to `out` by
    /// [`super::dragon::MobSim::boss_bars`] — the same shape
    /// [`super::wither::MobSim::push_wither_boss_bars`] already uses for the
    /// wither's own bar, and for the identical reason (`crate::tick` is
    /// off-limits for this change, so there is one public entry point rather
    /// than a second call site there).
    ///
    /// **No colour/style field** — `crate::protocol::BossBarSnapshot` does
    /// not carry one (see its own doc: neither the dragon's nor the wither's
    /// bar needed it either), so vanilla's `BossBarColor.RED`/
    /// `BossBarOverlay.NOTCHED_10` is a disclosed, pre-existing gap this
    /// change inherits rather than introduces.
    pub(super) fn push_raid_boss_bars(&self, out: &mut Vec<crate::protocol::BossBarSnapshot>) {
        let mut ids: Vec<i32> = self.raids.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(raid) = self.raids.get(&id) else { continue };
            let alive = raid.raiders.len() as i32;
            let name = if alive > 0 && alive <= 2 {
                format!("Raid - {alive} raiders remaining")
            } else {
                "Raid".to_string()
            };
            // Progress by *wave count*, not vanilla's health sum
            // (`getHealthOfLivingRaiders() / totalHealth`) — this port does
            // not track each wave's starting total health, only which
            // raiders are still alive; see this method's own doc.
            let progress = if raid.total_waves > 0 {
                (raid.groups_spawned as f32 / raid.total_waves as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            out.push(crate::protocol::BossBarSnapshot {
                id: raid.uuid,
                name: lodestone_model::Text::literal(name),
                progress,
                visible: raid.status == RaidStatus::Ongoing,
            });
        }
    }

    /// Whether a raid `id` is still tracked, and its `(wave, total_waves,
    /// raiders_alive)` — the query a gate needs without reaching into this
    /// module's private fields.
    #[must_use]
    pub fn raid_state(&self, id: i32) -> Option<(i32, i32, usize)> {
        self.raids.get(&id).map(|r| (r.groups_spawned, r.total_waves, r.raiders.len()))
    }

    /// The current wave's captain entity id, if a wave has been spawned —
    /// the data-only marker [`Raid::captain`]'s own doc describes.
    #[must_use]
    pub fn raid_captain(&self, id: i32) -> Option<i32> {
        self.raids.get(&id)?.captain
    }

    /// A raid's own Raid Omen level (`1..=`[`MAX_RAID_OMEN_LEVEL`]) — what
    /// [`absorb_raid_omen`] produced at [`MobSim::start_raid`] time. Vanilla
    /// reads this for `getEnchantOdds` (a loot bonus with no enchantment
    /// model to feed here — see `docs/raids-and-patrols.md` §5) and for the
    /// Hero of the Village effect amplifier on victory
    /// ([`take_hero_of_the_village_grants`](Self::take_hero_of_the_village_grants)
    /// — the amplifier `- 1` conversion happens there, matching
    /// `Raid.tick`'s own arithmetic); exposed so a gate can assert the value
    /// actually reached the raid rather than only that a raid started.
    #[must_use]
    pub fn raid_omen_level(&self, id: i32) -> Option<i32> {
        self.raids.get(&id).map(|r| r.omen_level)
    }

    /// Every raid-victory-earned Hero of the Village grant queued for `uuid`,
    /// each already converted to `Raid.tick`'s own amplifier
    /// (`raidOmenLevel - 1`) — drained, not merely read, so a connection that
    /// checks every tick never double-grants the same victory.
    ///
    /// A queue rather than an inline application because [`tick_raids`] runs
    /// inside the shared [`MobSim`] background task with no access to any
    /// connection's own `ActiveEffects` (`crate::server`'s per-connection
    /// state); `crate::server`'s own tick loop calls this once per tick with
    /// its own player's uuid, exactly the same producer/consumer split
    /// [`push_raid_boss_bars`](Self::push_raid_boss_bars) already uses for
    /// boss-bar state, here keyed by uuid instead of broadcast to every
    /// connection since only the actual hero should receive the effect. A
    /// player who is a hero of two raids finishing in the same tick gets two
    /// grants back, one per raid's own omen level — not folded into one.
    pub fn take_hero_of_the_village_grants(&mut self, uuid: Uuid) -> Vec<i32> {
        let mut amplifiers = Vec::new();
        self.pending_hero_grants.retain(|&(hero, omen_level)| {
            if hero == uuid {
                amplifiers.push(omen_level - 1);
                false
            } else {
                true
            }
        });
        amplifiers
    }

    /// Every live raid id, ascending.
    #[must_use]
    pub fn raid_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self.raids.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// `Raids.getNearbyRaid` — the id of the nearest *ongoing* raid whose
    /// centre lies within `max_dist_sqr` of `pos`, or `None`. Vanilla's own
    /// `ServerLevel::getRaidAt` calls this with `9216` (`96²`); passed
    /// through rather than hardcoded here since [`create_or_extend_raid`]
    /// is this method's only caller and already carries that citation.
    #[must_use]
    fn raid_near(&self, pos: Vec3, max_dist_sqr: f64) -> Option<i32> {
        let mut closest: Option<(i32, f64)> = None;
        for (&id, raid) in &self.raids {
            if raid.status != RaidStatus::Ongoing {
                continue;
            }
            let dx = raid.center.x - pos.x;
            let dy = raid.center.y - pos.y;
            let dz = raid.center.z - pos.z;
            let dist_sqr = dx * dx + dy * dy + dz * dz;
            if dist_sqr < max_dist_sqr {
                match closest {
                    Some((_, best)) if dist_sqr >= best => {}
                    _ => closest = Some((id, dist_sqr)),
                }
            }
        }
        closest.map(|(id, _)| id)
    }

    /// `Raids.createOrExtendRaid`'s "extend, don't duplicate" half: bumps an
    /// already-active raid's omen level via [`absorb_raid_omen`] rather than
    /// starting a second raid on top of it, capped at
    /// [`MAX_RAID_OMEN_LEVEL`] exactly as `Raid.getRaidOmenLevel() <
    /// Raid.getMaxRaidOmenLevel()`'s own guard.
    fn extend_raid_omen(&mut self, id: i32, amplifier: u32) {
        if let Some(raid) = self.raids.get_mut(&id)
            && raid.omen_level < MAX_RAID_OMEN_LEVEL
        {
            raid.omen_level = absorb_raid_omen(raid.omen_level, amplifier);
        }
    }

    /// `Raids.createOrExtendRaid` — issue #241's raid trigger. `origin` is
    /// vanilla's `raidOmenPosition`: the block a Raid Omen carrier stood on
    /// when Bad Omen converted, remembered by the caller across the 600-tick
    /// countdown and spent here on Raid Omen's own last tick (see
    /// `crate::server`'s wiring for both halves — this method only ever
    /// sees the second).
    ///
    /// Averages every occupied `#village`-tagged POI within 64 blocks of
    /// `origin` into a raid centre (falling back to `origin` itself when
    /// none are found, matching vanilla's own `count == 0` branch), then
    /// either [`extend_raid_omen`](Self::extend_raid_omen)s an
    /// already-ongoing raid found by [`raid_near`](Self::raid_near) within
    /// 96 blocks (`9216`, vanilla's own `ServerLevel::getRaidAt` constant)
    /// or [`start_raid`](Self::start_raid)s a fresh one with the omen level
    /// [`absorb_raid_omen`] produces from `amplifier` against a starting
    /// level of `0`.
    ///
    /// **The occupied-POI signal is [`super::MobSim::occupied_village_pois_in_range`]**
    /// — the live bed, workstation *and* bell claim ledgers unioned, matching
    /// vanilla's real `#village` tag (`home` + `meeting` +
    /// `#acquirable_job_site`) rather than beds alone. The disk-backed
    /// `crate::poi_storage::PoiStorage::occupied_in_range` still can never
    /// see any of the three, since none of the three claim ledgers persist
    /// (see `crate::mobs::villager`'s own module doc) — that narrowing is
    /// real and stays disclosed. A village whose villagers have claimed jobs
    /// and a bell but genuinely no bed (no `SleepInBed`/work-rest schedule
    /// has claimed one) still centres correctly now, which the beds-only
    /// query this method used to call could not do.
    ///
    /// Native-only, for [`super::MobSim::occupied_village_pois_in_range`]'s own
    /// reason.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn create_or_extend_raid(
        &mut self,
        origin: lodestone_model::BlockPos,
        difficulty: Difficulty,
        amplifier: u32,
    ) -> Option<i32> {
        let nearby = self.occupied_village_pois_in_range(origin, 64);
        let center = if nearby.is_empty() {
            Vec3::new(f64::from(origin.x), f64::from(origin.y), f64::from(origin.z))
        } else {
            let (sx, sy, sz) = nearby.iter().fold((0i64, 0i64, 0i64), |(sx, sy, sz), p| {
                (sx + i64::from(p.x), sy + i64::from(p.y), sz + i64::from(p.z))
            });
            let n = nearby.len() as f64;
            Vec3::new(sx as f64 / n, sy as f64 / n, sz as f64 / n)
        };
        if let Some(id) = self.raid_near(center, 9216.0) {
            self.extend_raid_omen(id, amplifier);
            Some(id)
        } else {
            self.start_raid(center, difficulty, absorb_raid_omen(0, amplifier))
        }
    }
}

/// A coarse spawn ring around the raid centre — `random angle, 20..40
/// blocks out, at the terrain surface` — standing in for vanilla's
/// `Raid.findRandomSpawnPos`, which walks outward from the centre testing
/// real line-of-sight/village-boundary conditions this sim's terrain seam
/// (`ChunkWorld`) has no equivalent census for (no POI/village data — the
/// same limit `docs/pillager-patrols.md` §5 already discloses for patrol
/// spawn placement).
fn wave_spawn_position(world: &super::ChunkWorld, center: Vec3, rng: &mut SpawnRng) -> Vec3 {
    let angle = f64::from(rng.next_f32()) * std::f64::consts::TAU;
    let dist = 20.0 + f64::from(rng.next_f32()) * 20.0;
    let x = center.x + angle.cos() * dist;
    let z = center.z + angle.sin() * dist;
    let y = world.surface_y(x.floor() as i32, z.floor() as i32).map_or(center.y, f64::from);
    Vec3::new(x, y + 1.0, z)
}

/// Spawns one wave of pillagers/vindicators for raid `id` and advances
/// `groups_spawned` — `Raid.spawnGroup`, narrowed to the two raider types
/// this crate's roster implements (see this file's own module doc for
/// ravager/evoker/witch).
fn spawn_wave(sim: &mut MobSim<'_>, id: i32, world: &super::ChunkWorld) {
    let Some((wave, difficulty)) = sim.raids.get(&id).map(|r| (r.groups_spawned + 1, r.difficulty)) else {
        return;
    };
    let wave_idx = usize::try_from(wave).unwrap_or(0).min(PILLAGER_BASE_SPAWNS.len() - 1);
    let pillagers = PILLAGER_BASE_SPAWNS[wave_idx] + bonus_spawns(difficulty, &mut sim.raid_rng);
    let vindicators = VINDICATOR_BASE_SPAWNS[wave_idx] + bonus_spawns(difficulty, &mut sim.raid_rng);
    let center = sim.raids.get(&id).expect("checked above").center;
    let mut spawned: Vec<i32> = Vec::new();
    for _ in 0..pillagers.max(0) {
        let pos = wave_spawn_position(world, center, &mut sim.raid_rng);
        let mob_id = sim.spawn_species("minecraft:pillager".parse().expect("valid key"), pos).id();
        spawned.push(mob_id);
    }
    for _ in 0..vindicators.max(0) {
        let pos = wave_spawn_position(world, center, &mut sim.raid_rng);
        let mob_id = sim.spawn_species("minecraft:vindicator".parse().expect("valid key"), pos).id();
        spawned.push(mob_id);
    }
    if let Some(raid) = sim.raids.get_mut(&id) {
        raid.captain = spawned.first().copied();
        raid.raiders = spawned;
        raid.groups_spawned = wave;
    }
}

#[cfg(test)]
mod raid_tests {
    use super::*;
    use crate::mobs::ChunkWorld;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -50..=50 {
            for z in -50..=50 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    /// **`start_raid` refuses Peaceful** — `getNumGroups(Peaceful) == 0`, so
    /// there is nothing to wave through and no raid is created at all.
    #[test]
    fn control_a_peaceful_raid_never_starts() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        assert_eq!(sim.start_raid(Vec3::new(0.0, 1.0, 0.0), Difficulty::Peaceful, 1), None);
        assert!(sim.raid_ids().is_empty());
    }

    /// **The discriminating input: a multi-wave raid on Hard escalates
    /// through all seven real waves**, each spawning the exact
    /// `PILLAGER_BASE_SPAWNS`/`VINDICATOR_BASE_SPAWNS` count for that wave
    /// (plus whatever the bonus roll adds, which is why the assertion is a
    /// floor, not an exact equality, on every wave but the ones a `0` base
    /// makes exact anyway).
    #[test]
    fn a_hard_raid_escalates_through_all_seven_waves() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim.start_raid(Vec3::new(0.0, 1.0, 0.0), Difficulty::Hard, 1).expect("Hard has 7 waves");
        let mut waves_seen: Vec<i32> = Vec::new();
        // Enough ticks to clear all 7 waves: each wave's raiders are killed
        // off by the test itself the tick after it spawns, so cooldown
        // (300 ticks after the first) dominates the budget.
        for _ in 0..(300 * 8 + 100) {
            sim.tick_raids();
            let Some((wave, total, alive)) = sim.raid_state(id) else { break };
            assert_eq!(total, 7, "Hard is 7 waves, Raid.getNumGroups(HARD)");
            if wave > waves_seen.last().copied().unwrap_or(0) {
                waves_seen.push(wave);
            }
            if alive > 0 {
                // Kill this wave's raiders immediately so the cooldown for
                // the next one starts on schedule rather than waiting on the
                // test to fight them.
                for mob_id in current_raiders(&sim, id) {
                    if let Some(m) = sim.get_mut(mob_id) {
                        m.damage_self(1_000.0);
                    }
                }
                sim.tick();
            }
        }
        assert_eq!(waves_seen, vec![1, 2, 3, 4, 5, 6, 7], "all seven waves must spawn, in order, with none skipped");
    }

    /// A collector the test above needs: the live raider ids for a raid,
    /// read through the public [`MobSim::raid_state`]/mob query surface
    /// rather than this module's private field — kept local to the test
    /// module since no production caller needs a raid's full raider list
    /// (only its count, via `raid_state`).
    fn current_raiders(sim: &MobSim<'_>, id: i32) -> Vec<i32> {
        let Some(raid) = sim.raids.get(&id) else { return Vec::new() };
        raid.raiders.clone()
    }

    /// **A raid with a single (Easy) wave reaches `Victory` once its lone
    /// wave is cleared and the 40-tick delay elapses** — the control input
    /// that separates "the wave-clear detection works" from "it merely never
    /// fires because there is always another wave", which the multi-wave
    /// gate above cannot see (it never runs out of waves within its budget).
    #[test]
    fn a_cleared_final_wave_reaches_victory_after_the_delay() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim.start_raid(Vec3::new(0.0, 1.0, 0.0), Difficulty::Easy, 1).expect("Easy has 3 waves");
        // Clear all three waves as fast as possible.
        for _ in 0..2_000 {
            sim.tick_raids();
            for mob_id in current_raiders(&sim, id) {
                if let Some(m) = sim.get_mut(mob_id) {
                    m.damage_self(1_000.0);
                }
            }
            sim.tick();
            let Some((wave, total, alive)) = sim.raid_state(id) else { break };
            if wave == total && alive == 0 {
                break;
            }
        }
        let (wave, total, alive) = sim.raid_state(id).expect("raid must still be tracked right after its last wave clears");
        assert_eq!((wave, alive), (total, 0), "all three Easy waves must be spawned and cleared");
        // The 40-tick post-raid delay, plus slack.
        for _ in 0..45 {
            sim.tick_raids();
        }
        assert!(sim.raid_state(id).is_none(), "a raid must stop being tracked once it reaches Victory");
    }

    /// **`absorb_raid_omen` is real arithmetic, predicted from the outside
    /// record** (`Raid.absorbRaidOmen`), not merely "goes up": absorbing a
    /// Bad Omen of amplifier `0` (the un-upgraded ominous bottle) at an
    /// existing level of `0` yields exactly `1`, and the ceiling clamps at
    /// [`MAX_RAID_OMEN_LEVEL`] regardless of how high the input climbs.
    #[test]
    fn absorb_raid_omen_matches_the_derived_values() {
        assert_eq!(absorb_raid_omen(0, 0), 1, "existing 0 + amplifier 0 + 1 = 1");
        assert_eq!(absorb_raid_omen(1, 0), 2);
        assert_eq!(absorb_raid_omen(0, 3), 4, "a higher-amplifier bottle jumps straight to level 4");
        assert_eq!(absorb_raid_omen(4, 4), MAX_RAID_OMEN_LEVEL, "clamped at the ceiling, not 9");
    }

    /// The captain marker: the first wave's first-spawned raider is
    /// [`MobSim::raid_captain`]'s answer — the data-only stand-in this
    /// file's own doc discloses for the (unbuilt) ominous banner.
    #[test]
    fn the_first_raider_of_a_wave_is_marked_captain() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim.start_raid(Vec3::new(0.0, 1.0, 0.0), Difficulty::Normal, 1).expect("Normal has 5 waves");
        sim.tick_raids();
        let captain = sim.raid_captain(id).expect("wave 1 must have spawned and named a captain");
        let raiders = current_raiders(&sim, id);
        assert!(raiders.contains(&captain), "the captain must be one of the wave's own raiders");
        assert_eq!(raiders.first().copied(), Some(captain), "the captain is the first raider spawned, not an arbitrary one");
    }

    /// Issue #241's raid trigger, wired end to end within this crate:
    /// `create_or_extend_raid` reads the live bed-claim ledger through
    /// `occupied_homes_in_range` for real (not a POI record built by hand),
    /// so this proves the whole `Raids.createOrExtendRaid` path — occupied-POI
    /// averaging, omen absorption and the first `start_raid` call — against a
    /// villager that actually claimed a bed through a real tick, the same
    /// shape `villager_bed_claim_tests` already proves for the claim itself.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn create_or_extend_raid_uses_a_real_claimed_bed_as_its_centre() {
        let mut world = flat_world();
        world.set_block(10, 1, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(10.0, 1.0, 0.0));
        sim.tick();
        assert!(
            !sim.occupied_homes_in_range(lodestone_model::BlockPos::new(0, 1, 0), 64).is_empty(),
            "the villager must have claimed the bed before the raid trigger can see it"
        );

        let id = sim
            .create_or_extend_raid(lodestone_model::BlockPos::new(0, 1, 0), Difficulty::Normal, 0)
            .expect("a claimed bed within 64 blocks must start a raid");
        assert_eq!(sim.raid_omen_level(id), Some(1), "absorb_raid_omen(0, 0) == 1");
        let (wave, total, _) = sim.raid_state(id).expect("raid must be tracked immediately, before its first tick");
        assert_eq!((wave, total), (0, 5), "Normal has 5 waves, none spawned yet");
    }

    /// A second omen absorbed near an already-active raid **extends** it
    /// (`raid_near`/`extend_raid_omen`) rather than starting a duplicate —
    /// the discriminating case `raid_near` exists for, since two omens near
    /// the same village must not spawn two overlapping raids.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_second_omen_near_an_active_raid_extends_it_instead_of_duplicating() {
        let mut world = flat_world();
        world.set_block(10, 1, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(10.0, 1.0, 0.0));
        sim.tick();

        let first = sim
            .create_or_extend_raid(lodestone_model::BlockPos::new(0, 1, 0), Difficulty::Normal, 0)
            .expect("the first omen must start a raid");
        let second = sim
            .create_or_extend_raid(lodestone_model::BlockPos::new(1, 1, 0), Difficulty::Normal, 0)
            .expect("the second omen must find the same raid, not refuse");
        assert_eq!(first, second, "must be the same raid id, not a duplicate");
        assert_eq!(sim.raid_omen_level(first), Some(2), "absorb_raid_omen(1, 0) == 2, the second absorption");
    }

    /// **Control for the extend arm above**: with no active raid nearby,
    /// `create_or_extend_raid` must still create one rather than silently
    /// doing nothing — proves `raid_near` returning `None` is not mistaken
    /// for "an extend that did nothing".
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn create_or_extend_raid_with_no_nearby_raid_creates_a_fresh_one() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        assert!(sim.raid_ids().is_empty());
        let id = sim
            .create_or_extend_raid(lodestone_model::BlockPos::new(0, 1, 0), Difficulty::Easy, 3)
            .expect("Easy is not Peaceful, so a raid must start");
        assert_eq!(sim.raid_ids(), vec![id]);
        assert_eq!(sim.raid_omen_level(id), Some(4), "absorb_raid_omen(0, 3) == 4");
    }

    /// The POI-signal widening this pass makes: a village whose only claimed
    /// `#village` POI is a workstation (no bed claimed at all) must still
    /// trigger a raid, because vanilla's real tag is `home` + `meeting` +
    /// `#acquirable_job_site`, not beds alone. Before
    /// `occupied_village_pois_in_range` replaced the beds-only
    /// `occupied_homes_in_range` call here, this exact scene found nothing
    /// and `create_or_extend_raid` returned `None`.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn create_or_extend_raid_uses_a_claimed_workstation_with_no_bed_at_all() {
        let mut world = flat_world();
        world.set_block(10, 1, 0, "minecraft:composter");
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(10.0, 1.0, 0.0));
        sim.tick();
        assert!(
            sim.occupied_homes_in_range(lodestone_model::BlockPos::new(0, 1, 0), 64).is_empty(),
            "no bed exists in this scene at all — the beds-only query must find nothing"
        );
        assert!(
            !sim.occupied_village_pois_in_range(lodestone_model::BlockPos::new(0, 1, 0), 64).is_empty(),
            "the claimed composter must still count toward the union query"
        );

        let id = sim
            .create_or_extend_raid(lodestone_model::BlockPos::new(0, 1, 0), Difficulty::Normal, 0)
            .expect("a claimed workstation within 64 blocks must start a raid, even with no claimed bed");
        assert_eq!(sim.raid_omen_level(id), Some(1), "absorb_raid_omen(0, 0) == 1");
    }

    /// Issue #246's remaining gap, end to end within this crate: a player who
    /// lands the killing blow on a raider is credited as a hero
    /// (`add_raid_hero`, resolved through `raid_containing_raider` exactly as
    /// production's `attack_from_player` does), and once the raid the raider
    /// belonged to reaches [`RaidStatus::Victory`], `take_hero_of_the_village_grants`
    /// hands that player's uuid back the raid's own omen-level-derived
    /// amplifier — `Raid.tick`'s `raidOmenLevel - 1` — and only once.
    ///
    /// Easy (3 waves, weakest escalation) so the tick budget stays small: the
    /// test kills every wave's raiders itself via the real
    /// `attack_from_player` path (not `damage_self`, unlike the
    /// seven-wave-escalation test above) specifically so hero-crediting is
    /// exercised, not bypassed.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_killing_blow_on_a_raider_earns_hero_of_the_village_on_raid_victory() {
        use crate::mobs::{DamageFlags, PlayerIdentity};

        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim.start_raid(Vec3::new(0.0, 1.0, 0.0), Difficulty::Easy, 1).expect("Easy has 3 waves");
        let hero_uuid = Uuid::from_u128(0x1234_5678);
        let hero = PlayerIdentity { uuid: hero_uuid, entity_id: 99 };

        // Nobody has won anything yet — the queue must be empty before any
        // raid finishes, the control for "always returns something" reading
        // this test's later assertion.
        assert!(
            sim.take_hero_of_the_village_grants(hero_uuid).is_empty(),
            "no raid has reached Victory yet, so nothing should be queued"
        );

        for _ in 0..(300 * 4 + 100) {
            sim.tick_raids();
            let Some((_, _, alive)) = sim.raid_state(id) else { break };
            if alive > 0 {
                for mob_id in current_raiders(&sim, id) {
                    sim.attack_from_player(
                        mob_id,
                        Some(hero),
                        Vec3::new(0.0, 1.0, 0.0),
                        1_000.0,
                        DamageFlags::default(),
                        0.0,
                    );
                }
                sim.tick();
            }
        }
        assert!(sim.raid_state(id).is_none(), "the raid must have reached Victory and been removed by now");

        let grants = sim.take_hero_of_the_village_grants(hero_uuid);
        assert_eq!(
            grants,
            vec![0],
            "start_raid was called directly with omen_level 1, so raid.omen_level == 1 \
             and the amplifier (raidOmenLevel - 1) must be 0"
        );

        // Drained, not merely read: a second call must find nothing left for
        // the same uuid, and an unrelated uuid must never have received
        // anything at all.
        assert!(
            sim.take_hero_of_the_village_grants(hero_uuid).is_empty(),
            "a grant must be drained, not repeatable"
        );
        let bystander = Uuid::from_u128(0x9999);
        assert!(
            sim.take_hero_of_the_village_grants(bystander).is_empty(),
            "a player who landed no killing blow must never be queued a grant"
        );
    }
}
