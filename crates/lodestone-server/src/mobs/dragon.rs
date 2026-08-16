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
//! * **`PhaseEffect::FireFireball` is computed *and* consumed.** A strafe
//!   that reaches its firing condition spawns a real
//!   `minecraft:dragon_fireball` through the same `spawn_projectile_from`
//!   funnel every other projectile in this crate uses. (An earlier version
//!   of this doc said the effect was computed but dropped; that was true
//!   when written and is not true now — re-verify a "not consumed"
//!   disclosure against the tree before repeating it.)

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

/// [`MobSim::init_end_dragon_fight`]'s return value — the entities it
/// really spawned, plus every block write a caller still needs to apply.
#[derive(Debug, Clone)]
pub struct EndDragonFightInit {
    /// The new dragon's network id — [`MobSim::spawn_dragon`]'s own return
    /// value, unchanged.
    pub dragon_id: i32,
    /// The ten new end crystals' network ids, in the same order
    /// [`lodestone_worldgen::end::end_spikes_for_seed`] returns their
    /// spikes.
    pub crystal_ids: Vec<i32>,
    /// Every obsidian/bedrock/iron-bars/podium block this fresh arena
    /// needs, in placement order (later entries overwrite earlier ones at
    /// the same position — see [`lodestone_worldgen::end::end_podium`]'s
    /// own doc for why that matters at the podium's own centre column).
    /// Not applied to any world by this call; see
    /// [`MobSim::init_end_dragon_fight`]'s own doc for why.
    pub block_writes: Vec<lodestone_worldgen::end::PodiumBlock>,
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

    /// A fresh End dimension's initial furniture and combatant, bundled
    /// into one call — the ten spike/crystal ring plus the dragon itself.
    /// This is the piece `spawn_dragon` was missing a real caller for: it
    /// had no production entry point because nothing in this crate builds
    /// the arena a dragon needs to spawn *into*. See `docs/dragon-fight.md`
    /// for the exact join-path hunk a caller still needs to add (`server.rs`
    /// is off limits for this change) — this method is everything that hunk
    /// needs to call.
    ///
    /// `origin` generalises vanilla's own fixed `BlockPos.ZERO` fight
    /// origin (`ServerLevel.effectiveDragonFight`'s `dragonFight.init(this,
    /// seed, BlockPos.ZERO)`) into a caller-supplied offset, matching
    /// [`spawn_dragon`](Self::spawn_dragon)'s own existing parameter rather
    /// than hardcoding a world-absolute position into a sim that has no
    /// concept of "the world's own (0, 0, 0)" — passing [`Vec3::new`]`(0.0,
    /// 0.0, 0.0)` reproduces vanilla's fixed placement exactly. `min_y` is
    /// the dimension's own lowest generatable y
    /// ([`lodestone_worldgen::end::end_spike_blocks`]'s own parameter —
    /// this sim has no `ChunkSource` to read it from).
    ///
    /// Spawns the ten end crystals (`MobSim::spawn_end_crystal`) and the
    /// dragon (`spawn_dragon`) for real — both reach [`MobSim::snapshots`]
    /// on the very next tick through the same paths every other mob already
    /// uses. **Places zero blocks itself**: this `MobSim` only ever reads a
    /// world through a caller-supplied closure (the same "no block-write
    /// authority" contract [`MobSim::try_construct_wither`]'s own doc
    /// discloses), so [`EndDragonFightInit::block_writes`] carries every
    /// obsidian/bedrock/iron-bars/podium write as data and the caller (who
    /// holds the real `ChunkSource`) applies them. The podium is written
    /// **inactive** (`active: false`) — matching vanilla's own first-arrival
    /// state; a caller wires the *active* podium separately once the dragon
    /// dies, through [`lodestone_worldgen::end::end_podium`] directly.
    pub fn init_end_dragon_fight(&mut self, seed: i64, origin: Vec3, min_y: i32) -> EndDragonFightInit {
        let spikes = lodestone_worldgen::end::end_spikes_for_seed(seed);
        let mut block_writes = Vec::new();
        let mut crystal_ids = Vec::with_capacity(lodestone_worldgen::end::SPIKE_COUNT);
        for spike in &spikes {
            block_writes.extend(lodestone_worldgen::end::end_spike_blocks(&spike, min_y));
            let crystal_pos = Vec3::new(
                origin.x + f64::from(spike.center_x) + 0.5,
                origin.y + f64::from(spike.height + 1),
                origin.z + f64::from(spike.center_z) + 0.5,
            );
            crystal_ids.push(self.spawn_end_crystal(crystal_pos));
        }
        block_writes.extend(lodestone_worldgen::end::end_podium(
            origin.x.floor() as i32,
            origin.y.floor() as i32,
            origin.z.floor() as i32,
            false,
        ));
        let dragon_id = self.spawn_dragon(origin);
        EndDragonFightInit { dragon_id, crystal_ids, block_writes }
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
        let effect = dragon.phase.tick(&inputs, &mut adapter);

        // `DragonDeathPhase.doServerTick`'s health-drive clause — see
        // `phase::PhaseManager::dying_health_this_tick`'s own doc for why
        // this is a second call rather than folded into `tick` above: it
        // drives `health`, not the phase itself. **Previously never called
        // in production** — a real, tested, individually-correct function
        // with zero production callers, the island class this repo's own
        // rules name explicitly. Without it a killing blow redirected the
        // dragon into `Dying` (via `on_killing_blow`, below) and then never
        // actually finished it off: health stayed clamped at `1.0` forever.
        let mut just_died = false;
        if let Some(new_health) = dragon.phase.dying_health_this_tick(&inputs) {
            dragon.health = new_health;
            just_died = new_health <= 0.0;
        }
        let fire_origin = dragon.position;
        let dragon_yaw = dragon.yaw;

        // A dragon whose death-flight health reached zero leaves the sim
        // here — matching this module's own doc ("only the caller's own
        // removal logic, watching health hit 0.0, ends the fight"). The
        // egg/exit-portal/gateway spawn sequence (`crate::dragon::fight::
        // set_dragon_killed`) is **not** called from here: `MobSim` owns no
        // `FightState` (see `dragon_boss_bar`'s own doc for why that stays a
        // caller-supplied value), so a production wiring pass still needs to
        // watch for this removal and drive that controller — a narrower,
        // still-disclosed gap than "the dragon can never die at all".
        if just_died {
            self.dragons.remove(&id);
            return;
        }

        // `PhaseEffect::FireFireball` — previously computed and unconditionally
        // dropped (see this module's doc history); now spawns a real
        // `minecraft:dragon_fireball` through the same
        // `MobSim::spawn_projectile_from` funnel every other projectile
        // producer in this crate uses. Aimed at the strafe target's last-known
        // position (this sim's targeting is distance-only, matching every
        // other input above — see this module's doc for the disclosed LOS
        // simplification); a target that despawned between acquiring the
        // strafe lock and firing falls back to straight ahead along the
        // dragon's current heading rather than dropping the shot silently.
        if effect == Some(phase::PhaseEffect::FireFireball) {
            let target_pos = nearest_player
                .and_then(|(pid, _)| self.players.iter().find(|p| p.identity.is_some_and(|i| i.entity_id == pid)))
                .map(|p| p.perception.position);
            // No resolvable target position (the strafe lock's player
            // disconnected or moved out of the perception list this same
            // tick): fall back to straight ahead along the current heading
            // rather than dropping the shot silently.
            let heading = f64::from(dragon_yaw).to_radians();
            let aim = target_pos.unwrap_or(fire_origin + Vec3::new(heading.cos(), 0.0, heading.sin()));
            let delta = aim - fire_origin;
            let dir = if delta.length() > 1e-6 { delta.normalize() } else { Vec3::new(0.0, 0.0, 1.0) };
            let projectile = lodestone_entity::projectile::Projectile::throwable(fire_origin, dir.scale(1.0));
            self.spawn_projectile_from(
                "minecraft:dragon_fireball".parse().expect("valid key"),
                projectile,
                Some(id),
            );
        }
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
    ///
    /// A killing blow while **not** sitting redirects into `Dying` at
    /// `1.0` health, matching `handleKillingBlow` — the dragon is not
    /// removed here; [`tick_one_dragon`] finishes it off (and removes it)
    /// once the death-flight health-drive clause reaches `0.0`, see that
    /// method's own doc. A killing blow while **sitting** is `EnderDragon`'s
    /// one undisguised surprise — it dies outright, same tick, no redirect —
    /// so this method removes it immediately in that branch; there is no
    /// later tick that would otherwise do it.
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
        let health = dragon.health;
        if health <= 0.0 {
            self.dragons.remove(&id);
        }
        Some(health)
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
    ///   a caller-supplied parameter). This no longer means the bar lingers
    ///   forever, though: [`damage_dragon`](Self::damage_dragon) and
    ///   [`tick_one_dragon`] both remove a dragon whose health reaches `0.0`
    ///   (see their own docs), and a removed dragon's own uuid simply stops
    ///   appearing in this method's output — [`crate::server::EntityStreamer`]'s
    ///   own diff (`sync_boss_bars`) reads a missing id as "no longer
    ///   visible" and sends the `REMOVE` operation, exactly as it would for
    ///   `visible: false`. `dragon_killed`'s only remaining effect is the
    ///   `crate::dragon::fight::boss_bar_value`'s persisted-past-death "always
    ///   full" branch (`EnderDragonFight`'s post-victory display for a
    ///   re-entering player), which needs `FightState` regardless.
    #[must_use]
    pub fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        let mut ids: Vec<i32> = self.dragons.keys().copied().collect();
        ids.sort_unstable();
        let mut out: Vec<crate::protocol::BossBarSnapshot> = ids
            .into_iter()
            .filter_map(|id| {
                let d = self.dragons.get(&id)?;
                // Delegates to `dragon_boss_bar` (this method's own
                // single-dragon sibling, previously computed inline here a
                // second time with a duplicated `false` literal) rather than
                // calling `fight::boss_bar_value` directly a second time.
                let bar = self.dragon_boss_bar(id, false)?;
                Some(crate::protocol::BossBarSnapshot {
                    id: d.uuid,
                    name: lodestone_model::Text::translate("entity.minecraft.ender_dragon", Vec::new()),
                    progress: bar.progress,
                    visible: bar.visible,
                })
            })
            .collect();
        // The single public boss-bar entry point covers every boss/event bar
        // this crate produces — see `mobs::wither`'s own doc for why its bar
        // is appended here rather than requiring a second call site in
        // `crate::tick` (an off-limits, shared file for this change). Issue
        // #241's raid bar follows the identical shape.
        self.push_wither_boss_bars(&mut out);
        self.push_raid_boss_bars(&mut out);
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

    /// **The island this fix closes**: `phase::PhaseManager::dying_health_this_tick`
    /// existed, was individually tested in `phase`'s own test module, and had
    /// zero production callers — a killing blow redirected a flying dragon
    /// into `Dying` at `1.0` health and then nothing ever finished it off.
    /// This drives `tick_dragons` with no player nearby (`dying_flying_cleanly`
    /// resolves `false` — see `tick_one_dragon`'s own `inputs.dying_flying_cleanly`
    /// assignment, which needs a real nearby player to read `true`) so the
    /// health-drive clause takes its zero branch on the very next tick, and
    /// asserts the dragon actually leaves the sim — not just that its health
    /// field changed, which a gate reading only `dragon_health` could not
    /// distinguish from "still tracked at exactly 0.0 forever".
    #[test]
    fn a_killing_blow_while_flying_now_actually_finishes_the_dragon_off() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        let after_blow = sim.damage_dragon(id, 10_000.0);
        assert_eq!(after_blow, Some(1.0), "redirected into Dying at 1.0, matching handleKillingBlow");
        assert_eq!(sim.dragon_phase(id), Some(phase::Phase::Dying));

        sim.tick_dragons();

        assert!(sim.dragon_health(id).is_none(), "the dragon must leave the sim once the death-flight clause reaches 0.0");
        assert!(
            sim.snapshots().into_iter().all(|s| s.entity_type != ender_dragon_entity_type()),
            "a dead dragon must stop streaming, the same pixels-reaching-zero check `mobs::wither` uses"
        );
    }

    /// The sitting-instant-kill path `damage_dragon`'s own doc names —
    /// `EnderDragon.handleKillingBlow`'s one place that does **not**
    /// distinguish sitting from standing — must also leave the sim, and on
    /// the same call (there is no later tick that would otherwise catch it,
    /// since `on_killing_blow` never redirects into `Dying` here).
    #[test]
    fn a_killing_blow_while_sitting_dies_outright_and_leaves_the_sim() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        {
            let dragon = sim.dragons.get_mut(&id).unwrap();
            dragon.phase = phase::PhaseManager::starting_in(phase::Phase::SittingScanning);
        }
        let after = sim.damage_dragon(id, 10_000.0);
        assert_eq!(after, Some(0.0), "a sitting dragon takes the killing blow outright, not redirected to 1.0");
        assert!(sim.dragon_health(id).is_none(), "and must leave the sim immediately — there is no Dying tick to catch it later");
    }

    /// **The second island this fix closes**: `PhaseEffect::FireFireball`
    /// was computed and unconditionally discarded (see this file's own
    /// module-doc history) — a strafing dragon transitioned phase correctly
    /// but no `minecraft:dragon_fireball` ever reached the wire. Forces the
    /// fire condition directly (five ticks of in-range-and-in-cone strafing,
    /// the exact sequence `phase::tests::strafe_fires_only_at_five_charge_and_in_cone`
    /// already proves fires on the fifth) and asserts a real tracked
    /// projectile now exists, not just that the phase changed.
    #[test]
    fn a_strafing_dragon_now_actually_fires_a_real_fireball_projectile() {
        let mut sim = sim();
        let id = sim.spawn_dragon(Vec3::new(0.0, 64.0, 0.0));
        sim.set_players(vec![crate::PerceivedPlayer {
            identity: Some(crate::PlayerIdentity {
                uuid: uuid::Uuid::new_v4(),
                entity_id: 1,
            }),
            perception: crate::PlayerPerception {
                // Within `NEARBY_PLAYER_RANGE_SQ` (`64.0` blocks) of the fight
                // *origin* `(0, 64, 0)` — `tick_one_dragon` measures every
                // targeting distance from `fight_origin`, not the dragon's own
                // (much higher, orbiting) position.
                position: Vec3::new(0.0, 64.0, 10.0),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }]);
        {
            let dragon = sim.dragons.get_mut(&id).unwrap();
            dragon.phase.set_phase_with_target(phase::Phase::StrafePlayer, phase::TargetSighting { id: 1 });
        }
        let before = sim.projectile_count();
        // Five ticks: `fireball_charge` must reach `5` with the target in
        // range/cone (the real player above sits well inside both
        // `NEARBY_PLAYER_RANGE_SQ` and the `100.0`-block "in range" gate
        // `tick_one_dragon` derives `strafe_in_range_and_los` from).
        for _ in 0..5 {
            sim.tick_dragons();
        }
        assert!(sim.projectile_count() > before, "a completed strafe charge must spawn a real fireball, not just transition phase");
        assert_eq!(sim.dragon_phase(id), Some(phase::Phase::HoldingPattern), "firing also returns to HoldingPattern, matching phase::PhaseManager::tick");
    }

    /// **The discriminating gate for the island `spawn_dragon` had no
    /// production caller for**: `init_end_dragon_fight` must actually spawn
    /// a live dragon and all ten crystals, not merely compute the geometry.
    /// `dragon_id`/`crystal_ids` are checked against `MobSim`'s own live
    /// query API (`dragon_health`, `end_crystal_position`) rather than
    /// merely asserting the returned ids are non-negative, so a version
    /// that computed the spike layout but never actually called
    /// `spawn_dragon`/`spawn_end_crystal` would fail this.
    #[test]
    fn init_end_dragon_fight_spawns_a_real_dragon_and_all_ten_crystals() {
        let mut sim = sim();
        let init = sim.init_end_dragon_fight(12345, Vec3::new(0.0, 64.0, 0.0), -64);

        assert!(sim.dragon_health(init.dragon_id).is_some(), "the returned dragon id must resolve to a live dragon");
        assert_eq!(sim.dragon_health(init.dragon_id), Some(MAX_HEALTH));

        assert_eq!(init.crystal_ids.len(), lodestone_worldgen::end::SPIKE_COUNT, "one crystal per spike");
        let mut mismatches = Vec::new();
        for &id in &init.crystal_ids {
            if sim.end_crystal_position(id).is_none() {
                mismatches.push(format!("crystal id {id} did not resolve to a live crystal"));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
        assert_eq!(sim.end_crystal_count(), lodestone_worldgen::end::SPIKE_COUNT, "no extra and no missing crystals");
    }

    /// A crystal's spawn position must be its own spike's centre (offset by
    /// the fight origin), at `height + 1` — not e.g. every crystal landing
    /// on the same spot, which the count check above could not catch.
    #[test]
    fn each_crystal_spawns_at_its_own_spikes_position() {
        let mut sim = sim();
        let origin = Vec3::new(0.0, 64.0, 0.0);
        let init = sim.init_end_dragon_fight(999, origin, -64);
        let spikes = lodestone_worldgen::end::end_spikes_for_seed(999);

        let mut mismatches = Vec::new();
        for (spike, &crystal_id) in spikes.iter().zip(init.crystal_ids.iter()) {
            let Some(pos) = sim.end_crystal_position(crystal_id) else {
                mismatches.push(format!("crystal {crystal_id} vanished"));
                continue;
            };
            let expected = Vec3::new(
                origin.x + f64::from(spike.center_x) + 0.5,
                origin.y + f64::from(spike.height + 1),
                origin.z + f64::from(spike.center_z) + 0.5,
            );
            if pos != expected {
                mismatches.push(format!("expected {expected:?}, got {pos:?}"));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// **Control**: the same seed and origin must always produce the same
    /// block-write count (the arena is deterministic, not randomly sized
    /// per call) — and that count must be well above what ten crystal
    /// support pairs alone could account for, proving the spike
    /// columns/cages and the podium really are all included rather than one
    /// silently dropped.
    #[test]
    fn block_writes_are_deterministic_and_include_every_piece() {
        let mut sim_a = sim();
        let mut sim_b = sim();
        let init_a = sim_a.init_end_dragon_fight(42, Vec3::new(0.0, 64.0, 0.0), -64);
        let init_b = sim_b.init_end_dragon_fight(42, Vec3::new(0.0, 64.0, 0.0), -64);
        assert_eq!(init_a.block_writes.len(), init_b.block_writes.len(), "same seed and origin must yield the same write count");
        // Ten crystal-support pairs alone is 20 writes; the real arena (ten
        // obsidian columns plus the podium) is far larger.
        assert!(init_a.block_writes.len() > 200, "got only {} writes — spike columns or the podium look dropped", init_a.block_writes.len());
        // Exactly two of the ten spikes are guarded (see
        // `end::spikes::tests::exactly_two_spikes_are_guarded_for_any_seed`),
        // so a real arena always carries at least one cage bar — a version
        // that silently dropped `end_spike_blocks`' guarded branch would
        // fail this while still passing the raw count check above.
        assert!(
            init_a.block_writes.iter().any(|w| w.state.starts_with("minecraft:iron_bars")),
            "expected at least one iron-bars cage write — got none"
        );
    }
}
