//! `MobSim`'s ender-dragon slice — spawn, per-tick flight/phase/heal, and
//! the query API. Follows the same split `tnt.rs`/`vehicles.rs` established:
//! [`super::TrackedDragon`] lives in `mobs/mod.rs`, the behaviour lives
//! here. Drives [`crate::dragon::phase::PhaseManager`] and
//! [`crate::dragon::crystal`] (this crate's pure, world-free port of
//! `EnderDragon`/`EnderDragonPhaseManager`) with real inputs pulled from this
//! sim's own state — the production wiring `docs/dragon-fight.md` names as
//! the missing piece.
//!
//! # What is a real port and what is a named simplification
//!
//! * **Health, max health (`200.0`, `EnderDragon.createMobAttributes`),
//!   phase transitions, and the crystal heal amount/interval are real
//!   ports** — driven through [`crate::dragon::phase::PhaseManager::tick`]
//!   and [`crate::dragon::crystal::crystal_heal_tick`] exactly as
//!   `docs/dragon-fight.md` describes.
//! * **Flight is a simplified circular orbit, not vanilla's 12-node path
//!   graph.** See `crate::dragon::phase`'s own module doc for why: this
//!   codebase's flying-mob AI has no aerial pathfinder. [`tick_dragons`]
//!   computes a fixed-radius circle around the fight origin and treats one
//!   full lap as a "leg" (`DragonInputs::leg_complete`) — a real, distinct
//!   signal (it is `false` on 3 out of 4 ticks of a short test lap, not
//!   hardcoded `true`), but not vanilla's arrival-at-a-real-waypoint
//!   condition.
//! * **Strafing/sitting-scan targeting use real nearby-player distance
//!   checks** (via [`MobSim::players`]) but not real line-of-sight — this
//!   sim has no raycast-against-terrain primitive wired for a flying entity,
//!   so `DragonInputs::strafe_in_range_and_los`/`strafe_aim_in_cone` are
//!   distance-only proxies (documented at their assignment below), which
//!   means a strafe can "fire" through a wall a real line-of-sight check
//!   would have blocked. This over-approximates vanilla rather than
//!   under-approximating it — the fight is harder to hide from here, not
//!   easier.
//! * **`PhaseEffect::FireFireball` is computed but not consumed.** No
//!   `minecraft:dragon_fireball` projectile producer exists in this sim, so
//!   a strafe that reaches its firing condition transitions phase (matching
//!   vanilla) but spawns no projectile. Disclosed rather than silently
//!   dropped — grep this file for `FireFireball` to find the exact point a
//!   projectile producer needs to hook in.

use lodestone_model::{ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::dragon::{crystal, fight, phase};
use crate::dragon::phase::DragonRng as _;
use crate::mob_spawn::SpawnRng;

use super::{MobSim, TrackedDragon};

/// `EnderDragon.createMobAttributes`'s `Attributes.MAX_HEALTH` value.
pub const MAX_HEALTH: f32 = 200.0;

/// The simplified orbit's radius, in blocks — not a vanilla constant (there
/// is no single "orbit radius" in the real node-graph flight); chosen to sit
/// comfortably inside `EnderDragonFight`'s own `192.0`-block player-tracking
/// range and `ARENA_SIZE_CHUNKS` (`8`, i.e. 128 blocks) footprint.
pub const ORBIT_RADIUS: f64 = 40.0;

/// The simplified orbit's height above the fight origin — chosen near
/// `EnderDragonFight.DRAGON_SPAWN_Y` (`128`) scaled down for a more visible
/// default arena; not itself a vanilla constant (see [`ORBIT_RADIUS`]).
pub const ORBIT_HEIGHT: f64 = 70.0;

/// Radians per tick the simplified orbit advances — a scope choice (see
/// [`ORBIT_RADIUS`]'s own note), tuned so one full lap is a few hundred
/// ticks, in the same rough order of magnitude as vanilla's own multi-leg
/// holding-pattern circuits.
const ORBIT_ANGULAR_SPEED: f64 = std::f64::consts::TAU / 300.0;

/// Seed for [`MobSim::dragon_rng`](super::MobSim) — its own stream so a
/// dragon's phase rolls never shift a mob spawn, a block drop, or any other
/// roll, matching every other per-behaviour seed in this crate
/// (`tnt::TNT_LAUNCH_SEED`, `orbs::ORB_BEHAVIOR_SEED`, ...).
pub(super) const DRAGON_PHASE_SEED: u64 = 0x4452_4147_4f4e_5048;

/// A player within this many blocks (squared) of the fight origin counts as
/// "near the egg"/"in scan range" for [`phase::DragonInputs`]'s targeting
/// fields — a single flat threshold standing in for vanilla's several
/// different named ranges (`20.0` scan, `150.0` charge, `4096.0` strafe),
/// since this sim's player perception has no per-purpose targeting
/// conditions the way `TargetingConditions` does. Documented here rather
/// than silently reusing one vanilla constant for all three purposes.
const NEARBY_PLAYER_RANGE_SQ: f64 = 64.0 * 64.0;

/// Adapts [`SpawnRng`] to [`phase::DragonRng`] — the seam
/// `crate::dragon::phase`'s tests use [`phase::AlwaysZeroRng`]/[`phase::NeverZeroRng`]
/// for, wired here to this sim's real seeded stream.
struct SpawnRngAdapter<'a>(&'a mut SpawnRng);

impl phase::DragonRng for SpawnRngAdapter<'_> {
    fn next_below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        // `SpawnRng::next_int` takes an `i32` upper bound and returns a
        // value in `[0, bound)`, exactly `random.nextInt(bound)`'s contract.
        self.0.next_int(bound as i32).max(0) as u32
    }
}

/// The entity-type key every dragon streams as.
pub(super) fn ender_dragon_entity_type() -> ResourceKey {
    "minecraft:ender_dragon"
        .parse()
        .expect("`minecraft:ender_dragon` is a valid resource key")
}

impl<'w> MobSim<'w> {
    /// `EnderDragonFight.createNewDragon` — spawns a fresh dragon at
    /// `128` blocks above `origin` (`DRAGON_SPAWN_Y`), full health, starting
    /// in [`phase::Phase::HoldingPattern`]. Returns the new entity's network
    /// id.
    pub fn spawn_dragon(&mut self, origin: Vec3) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        let position = Vec3::new(origin.x, origin.y + f64::from(fight::DRAGON_SPAWN_Y), origin.z);
        self.dragons.insert(
            id,
            TrackedDragon {
                uuid: Uuid::new_v4(),
                position,
                yaw: 0.0,
                health: MAX_HEALTH,
                max_health: MAX_HEALTH,
                phase: phase::PhaseManager::new(),
                nearest_crystal: crystal::NearestCrystal::none(),
                fight_origin: origin,
                orbit_angle: 0.0,
            },
        );
        id
    }

    /// A live dragon's current health, if any.
    #[must_use]
    pub fn dragon_health(&self, id: i32) -> Option<f32> {
        self.dragons.get(&id).map(|d| d.health)
    }

    /// A live dragon's current phase, if any.
    #[must_use]
    pub fn dragon_phase(&self, id: i32) -> Option<phase::Phase> {
        self.dragons.get(&id).map(|d| d.phase.current())
    }

    /// A live dragon's current position, if any.
    #[must_use]
    pub fn dragon_position(&self, id: i32) -> Option<Vec3> {
        self.dragons.get(&id).map(|d| d.position)
    }

    /// The boss-bar value for a live dragon — see [`fight::boss_bar_value`].
    /// `dragon_killed` is the caller's [`fight::FightState::dragon_killed`]
    /// (this sim tracks no `FightState` of its own — see `docs/dragon-fight.md`
    /// for why the fight controller stays a separate, world-owned value
    /// rather than sim-owned state).
    #[must_use]
    pub fn dragon_boss_bar(&self, id: i32, dragon_killed: bool) -> Option<fight::BossBarValue> {
        self.dragons
            .get(&id)
            .map(|d| fight::boss_bar_value(dragon_killed, d.health, d.max_health))
    }

    /// One tick of every live dragon: the simplified orbit (see this
    /// module's doc), crystal-heal, and the phase-manager tick. `players`
    /// perception (`self.players`) is real; targeting beyond distance is
    /// not (see this module's doc for exactly which fields are
    /// approximated).
    pub fn tick_dragons(&mut self) {
        let ids: Vec<i32> = self.dragons.keys().copied().collect();
        for id in ids {
            self.tick_one_dragon(id);
        }
    }

    fn tick_one_dragon(&mut self, id: i32) {
        // Crystal rescan roll — `random.nextInt(10) == 0`, using this sim's
        // own seeded dragon stream so it never perturbs any other roll.
        let rescan_roll = {
            let mut adapter = SpawnRngAdapter(&mut self.dragon_rng);
            crystal::should_rescan_crystals(adapter.next_below(10))
        };
        let crystals = self.end_crystals();
        let alive_crystals = crystals.len() as i32;

        let Some(dragon) = self.dragons.get_mut(&id) else {
            return;
        };

        // `checkCrystals`: clear a removed nearest crystal, then (on the
        // roll) rescan for the real nearest by distance.
        let crystal_still_alive = |cid: i32| crystals.iter().any(|(id, _)| *id == cid);
        dragon.nearest_crystal.clear_if_removed(|cid| !crystal_still_alive(cid));
        if rescan_roll {
            let nearest = crystals
                .iter()
                .min_by(|(_, a), (_, b)| {
                    dist_sq(dragon.position, *a)
                        .partial_cmp(&dist_sq(dragon.position, *b))
                        .expect("distances are always finite")
                })
                .map(|(cid, _)| *cid);
            dragon.nearest_crystal.set_nearest(nearest);
        }

        // The heal proc — exact 1.0 HP / 10 ticks, gated on a live nearest
        // crystal and health below max.
        if let Some(new_health) = crystal::crystal_heal_tick(
            self.tick_count as i64,
            dragon.nearest_crystal.id().is_some(),
            dragon.health,
            dragon.max_health,
        ) {
            dragon.health = new_health;
        }

        // The simplified orbit. One full lap is a "leg"; `leg_complete`
        // fires on the tick the angle wraps past a full turn, then resets —
        // a real, infrequent signal rather than a constant `true`.
        dragon.orbit_angle += ORBIT_ANGULAR_SPEED;
        let leg_complete = dragon.orbit_angle >= std::f64::consts::TAU;
        if leg_complete {
            dragon.orbit_angle -= std::f64::consts::TAU;
        }
        dragon.position = Vec3::new(
            dragon.fight_origin.x + ORBIT_RADIUS * dragon.orbit_angle.cos(),
            dragon.fight_origin.y + ORBIT_HEIGHT,
            dragon.fight_origin.z + ORBIT_RADIUS * dragon.orbit_angle.sin(),
        );
        dragon.yaw = (dragon.orbit_angle.to_degrees() + 90.0) as f32;

        // Real nearest-player distance, no real line of sight (see module
        // doc). Used for every `phase::DragonInputs` targeting field —
        // vanilla's several different ranges collapse to one
        // `NEARBY_PLAYER_RANGE_SQ` here.
        let nearest_player = self
            .players
            .iter()
            .filter_map(|p| p.identity.map(|id| (id.entity_id, p.perception.position)))
            .map(|(pid, pos)| (pid, dist_sq(dragon.fight_origin, pos)))
            .filter(|(_, d)| *d <= NEARBY_PLAYER_RANGE_SQ)
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).expect("distances are always finite"));

        let mut inputs = phase::DragonInputs {
            alive_crystals,
            leg_complete,
            ..Default::default()
        };
        if let Some((pid, dist)) = nearest_player {
            let sighting = phase::TargetSighting { id: pid };
            inputs.player_near_egg = Some(sighting);
            // `egg.distToCenterSqr(...) / 512.0` — the real formula, fed the
            // real distance computed above.
            inputs.egg_distance_scaled = dist / 512.0;
            inputs.sitting_scan_target = Some(sighting);
            inputs.charge_target = Some(sighting);
            // Distance-only proxy for LOS/cone (see module doc).
            inputs.strafe_in_range_and_los = dist <= 4096.0;
            inputs.strafe_aim_in_cone = true;
            inputs.within_10_of_egg = dist <= 100.0;
            inputs.charge_arrived_or_collided = dist <= 100.0;
            inputs.dying_flying_cleanly = (100.0..=22500.0).contains(&dist);
        } else {
            inputs.egg_distance_scaled = 64.0; // vanilla's own no-player fallback
        }

        let mut adapter = SpawnRngAdapter(&mut self.dragon_rng);
        // `PhaseEffect::FireFireball` is intentionally dropped here — see
        // this module's doc comment for why (no projectile producer yet).
        let _effect = dragon.phase.tick(&inputs, &mut adapter);
    }

    /// Applies `damage` to a live dragon through
    /// [`phase::PhaseManager::on_sitting_damage`]/`on_killing_blow` — the
    /// `EnderDragon.hurt` clauses that are phase-state rather than plain
    /// health subtraction. Returns the resulting health, or `None` if `id`
    /// is not a live dragon.
    ///
    /// **Not yet wired to a real hit** (no `attack`/`explode` call site
    /// targets a dragon) — this method exists so a future hit-resolution
    /// pass has one call to make rather than reimplementing the
    /// sitting-damage/killing-blow clauses at the call site.
    pub fn damage_dragon(&mut self, id: i32, damage: f32) -> Option<f32> {
        let dragon = self.dragons.get_mut(&id)?;
        if damage >= dragon.health {
            // `handleKillingBlow`: redirect into Dying rather than actually
            // dying, unless already sitting.
            if dragon.phase.on_killing_blow() {
                dragon.health = 1.0;
                return Some(dragon.health);
            }
        }
        let before = dragon.health;
        dragon.health = (dragon.health - damage).max(0.0);
        let delta = before - dragon.health;
        dragon.phase.on_sitting_damage(delta, dragon.max_health);
        Some(dragon.health)
    }

    /// Appends every live dragon's [`crate::protocol::EntitySnapshot`] to
    /// `out` — the dragon half of [`MobSim::snapshots`]'s sidecar loops.
    /// See `end_crystal::push_end_crystal_snapshots`'s own doc for why this
    /// is its own method rather than inlined.
    ///
    /// Carries a real [`crate::protocol::MetadataField::DragonPhase`] now —
    /// `d.phase.current().id()`, the same [`phase::PhaseManager`] this file's
    /// own [`tick_one_dragon`] drives every tick. `EntityStreamer::sync`
    /// diffs it exactly like every other snapshot field, so a phase
    /// transition (holding pattern → strafing → sitting → …) reaches the
    /// wire on the very next streaming pass after it happens.
    pub(super) fn push_dragon_snapshots(&self, out: &mut Vec<crate::protocol::EntitySnapshot>) {
        let mut ids: Vec<i32> = self.dragons.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(d) = self.dragons.get(&id) else {
                continue;
            };
            out.push(crate::protocol::EntitySnapshot {
                id,
                uuid: d.uuid,
                entity_type: ender_dragon_entity_type(),
                position: d.position,
                rotation: Rotation::new(d.yaw, 0.0),
                head_yaw: d.yaw,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                metadata: vec![crate::protocol::MetadataField::DragonPhase(d.phase.current().id())],
                object_data: 0,
                leash_link: None,
            });
        }
    }

    /// Every live dragon's [`crate::protocol::BossBarSnapshot`] — the input
    /// [`crate::server::EntityStreamer`]'s boss-bar diff consumes to actually
    /// put the health bar on a client's screen (`crate::dragon::fight`'s own
    /// module doc names this crate's `BOSS_EVENT` encoder as the missing
    /// half; it now exists, and this is its producer).
    ///
    /// # Two named simplifications
    ///
    /// * **The bar's id is the dragon's own entity uuid**, not a separate
    ///   `Mth.createInsecureUUID` the way `EnderDragonFight.init` mints one.
    ///   This sim tracks one bar per dragon 1:1 and nothing needs the two
    ///   identities to differ — see [`crate::protocol::BossBarSnapshot::id`]'s
    ///   own doc.
    /// * **`dragon_killed` is hardcoded `false`.** `MobSim` tracks no
    ///   `crate::dragon::fight::FightState` of its own (see
    ///   [`dragon_boss_bar`](Self::dragon_boss_bar)'s own doc for why that is
    ///   a caller-supplied parameter) and nothing here removes a dragon whose
    ///   health reaches `0.0` — the same disclosed gap
    ///   [`damage_dragon`](Self::damage_dragon)'s own doc names. The bar
    ///   therefore empties out and stays visible rather than disappearing on
    ///   death, which is at least consistent with the entity itself staying
    ///   in the world.
    #[must_use]
    pub fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        let mut ids: Vec<i32> = self.dragons.keys().copied().collect();
        ids.sort_unstable();
        let mut out: Vec<crate::protocol::BossBarSnapshot> = ids
            .into_iter()
            .filter_map(|id| {
                let d = self.dragons.get(&id)?;
                let bar = fight::boss_bar_value(false, d.health, d.max_health);
                Some(crate::protocol::BossBarSnapshot {
                    id: d.uuid,
                    name: lodestone_model::Text::translate("entity.minecraft.ender_dragon", Vec::new()),
                    progress: bar.progress,
                    visible: bar.visible,
                })
            })
            .collect();
        // The single public boss-bar entry point covers both boss fights —
        // see `mobs::wither`'s own doc for why its bar is appended here
        // rather than requiring a second call site in `crate::tick` (an
        // off-limits, shared file for this change).
        self.push_wither_boss_bars(&mut out);
        out
    }
}

fn dist_sq(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ChunkWorld;

    fn sim() -> MobSim<'static> {
        let world: &'static ChunkWorld = Box::leak(Box::new(ChunkWorld::new(-64, 384)));
        MobSim::new(world)
    }

    #[test]
    fn a_spawned_dragon_starts_at_full_health_and_holding_pattern() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        assert_eq!(sim.dragon_health(id), Some(MAX_HEALTH));
        assert_eq!(sim.dragon_phase(id), Some(phase::Phase::HoldingPattern));
        assert_eq!(
            sim.dragon_position(id),
            Some(Vec3::new(0.0, 64.0 + f64::from(fight::DRAGON_SPAWN_Y), 0.0))
        );
    }

    #[test]
    fn a_ticked_dragon_actually_moves() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        let start = sim.dragon_position(id).unwrap();
        for _ in 0..10 {
            sim.tick_dragons();
        }
        let after = sim.dragon_position(id).unwrap();
        assert!(
            (start.x - after.x).abs() > 1e-6 || (start.z - after.z).abs() > 1e-6,
            "ten ticks of orbiting must actually change x/z, got {start:?} -> {after:?}"
        );
    }

    #[test]
    fn a_dragon_heals_from_a_nearby_crystal() {
        let mut sim = sim();
        let origin = Vec3::new(0.0, 64.0, 0.0);
        let id = sim.spawn_dragon(origin);
        sim.damage_dragon(id, 50.0);
        assert_eq!(sim.dragon_health(id), Some(MAX_HEALTH - 50.0));
        sim.spawn_end_crystal(Vec3::new(0.0, 64.0 + f64::from(fight::DRAGON_SPAWN_Y), 0.0));
        // Enough ticks to force a rescan roll (up to ~10 tries at 1/10 odds)
        // and at least one heal-interval boundary.
        for _ in 0..400 {
            sim.tick_dragons();
        }
        assert!(
            sim.dragon_health(id).unwrap() > MAX_HEALTH - 50.0,
            "a nearby live crystal must heal the dragon over 400 ticks, got {:?}",
            sim.dragon_health(id)
        );
    }

    #[test]
    fn a_dragon_with_no_crystal_never_heals() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        sim.damage_dragon(id, 50.0);
        for _ in 0..200 {
            sim.tick_dragons();
        }
        assert_eq!(sim.dragon_health(id), Some(MAX_HEALTH - 50.0), "no crystal, no heal");
    }

    #[test]
    fn a_dragon_is_streamed_and_visible() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        let snap = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("a live dragon must be streamed, or it reaches zero pixels");
        assert_eq!(snap.entity_type, ender_dragon_entity_type());
    }

    #[test]
    fn a_killing_blow_while_flying_redirects_to_dying_at_one_health() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        let after = sim.damage_dragon(id, 10_000.0);
        assert_eq!(after, Some(1.0), "a killing blow while not sitting clamps to 1.0, not 0.0");
        assert_eq!(sim.dragon_phase(id), Some(phase::Phase::Dying));
    }

    #[test]
    fn sustained_partial_damage_forces_takeoff_while_sitting() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        {
            let dragon = sim.dragons.get_mut(&id).unwrap();
            dragon.phase = phase::PhaseManager::starting_in(phase::Phase::SittingScanning);
        }
        sim.damage_dragon(id, 51.0); // > 0.25 * 200.0
        assert_eq!(sim.dragon_phase(id), Some(phase::Phase::Takeoff));
    }
}
