//! The warden simulation consumes vibrations, updates anger, and resolves
//! sonic-boom or melee attacks through the shared damage pipeline.
//!
//! # What it is
//!
//! [`AngerLevel`] maps the `0..=150` anger score to `Calm`, `Agitated`, or
//! `Angry`. [`MobSim::resolve_warden_anger`] applies the per-tick decay and
//! vibration increase, counts the emerging and digging timers, and sends
//! attacks through [`SimMob::apply_damage`](super::SimMob::apply_damage),
//! including armour, invulnerability frames, and hurt sound handling.
//!
//! A warden stores one [`SimMob::warden_anger_target`](super::SimMob::warden_anger_target).
//! A vibration from another source replaces that target and resets anger to
//! zero before applying the new event. This is a deliberate single-suspect
//! model; it does not maintain a multi-target anger table.
//!
//! Movement comes from the entity brain module. It receives the resolved
//! target position once per tick and therefore follows the anger consumer by
//! one tick. The anger consumer remains the only attack producer.
//!
//! Every warden is invulnerable during [`EMERGE_DURATION_TICKS`] ticks of the
//! emerging pose. An angry warden in range prefers sonic boom when its
//! cooldown is clear and falls back to melee. Sonic boom uses the measured
//! 15-block horizontal and 20-block vertical range; melee covers the
//! close-range window. Digging starts when the 1,200-tick cooldown reaches
//! zero, runs for [`DIGGING_DURATION_TICKS`], and removes the mob afterward.
//! Roar targeting is not modeled, so an emerged calm warden with an expired
//! digging cooldown enters digging without a roar-target gate. Cooldown refresh
//! also follows the Angry state only; sub-Angry disturbances have no producer
//! in this simulation.
//!
//! The vibration producer currently supplies mob-death events, so anger and
//! attacks target another simulated mob rather than a player. Block, footstep,
//! and container events require producers in their respective tick and connection paths.
//!
//! # How to change it
//!
//! Movement speed and stop distance belong to the entity brain module. Digging
//! entry conditions belong here, while player knockback requires a player
//! velocity update in the server connection path. A second vibration-listener
//! species needs its own anger state and attack table.
//!
//! # Dependencies
//!
//! [`lodestone_entity::vibration`] supplies [`PostedVibration`]; the shared
//! mob simulation supplies targets, snapshots, and damage resolution.

use lodestone_entity::DamageFlags;
use lodestone_model::Vec3;

use super::MobSim;

/// `AngerManagement.MAX_ANGER`.
pub const MAX_ANGER: i32 = 150;

/// `AngerManagement.DEFAULT_ANGER_DECREASE`, applied once per tick to every
/// listener's [`SimMob::warden_anger`](super::SimMob::warden_anger) —
/// including a listener with no target, matching vanilla's own unconditional
/// per-suspect decay.
pub const ANGER_DECAY_PER_TICK: i32 = 1;

/// `Warden.increaseAngerAt(Entity)`'s own default amount
/// (`this.increaseAngerAt(entity, 35, true)`).
pub const ANGER_INCREASE: i32 = 35;

/// `AngerLevel.AGITATED.getMinimumAnger()`.
pub const AGITATED_THRESHOLD: i32 = 40;

/// `AngerLevel.ANGRY.getMinimumAnger()`.
pub const ANGRY_THRESHOLD: i32 = 80;

/// A warden's own melee reach, squared. **Not a transcribed vanilla
/// constant** — vanilla's warden melee behaviour has no `GoalSelector` reach
/// of its own to cite (see this module's own doc), so `3.0` blocks is a
/// disclosed, honest placeholder in the same family as
/// [`super::raid::MobSim`]'s own `wave_spawn_position` approximation, not a
/// jar-derived figure.
pub const MELEE_RANGE_SQR: f64 = 9.0;

/// `WardenAi.EMERGE_DURATION` — `Mth.ceil(133.59999F)`. How long a
/// freshly-spawned warden holds `Pose::EMERGING` — invulnerable
/// ([`SimMob::apply_damage`](super::SimMob::apply_damage)'s own warden arm)
/// and outside the `FIGHT` activity's reach (see [`resolve_warden_anger`]'s
/// own early-continue) — before returning to normal.
pub const EMERGE_DURATION_TICKS: i32 = 134;

/// `Pose.EMERGING.id()` and `Pose.DIGGING.id()` — the real jar ordinals
/// (vanilla's own pose enum), not guessed: `STANDING` through
/// `DYING` fill `0..=7`, then `CROAKING`/`USING_TONGUE` take `8`/`9` for the
/// frog before `SITTING` at `10` and `ROARING`/`SNIFFING` at `11`/`12`, so
/// the warden's own two poses land at `13`/`14`.
pub const POSE_EMERGING: u32 = 13;
/// See [`POSE_EMERGING`]. Produced by [`SimMob::snapshot`](super::SimMob::snapshot)
/// while [`SimMob::warden_digging_ticks`](super::SimMob::warden_digging_ticks)
/// is positive.
pub const POSE_DIGGING: u32 = 14;
/// `Pose.STANDING.id()` — vanilla's own default, and what
/// [`SimMob::snapshot`](super::SimMob::snapshot) sends once
/// [`EMERGE_DURATION_TICKS`] elapses so a client that already saw `13`
/// does not stay stuck showing it forever (`SET_ENTITY_DATA` is a sparse
/// update — see `MetadataField::Pose`'s own doc).
pub const POSE_STANDING: u32 = 0;

/// The measured digging duration is `100` ticks. How long
/// [`SimMob::warden_digging_ticks`](super::SimMob::warden_digging_ticks)
/// counts down before [`resolve_warden_anger`] discards the mob.
pub const DIGGING_DURATION_TICKS: i32 = 100;
/// The digging cooldown is `1200` ticks. The spawn seed and the angry-state
/// refresh use this same value; [`resolve_warden_anger`] uses one constant for
/// both.
pub const DIGGING_COOLDOWN_TICKS: i32 = 1200;

/// `SonicBoom.COOLDOWN` — ticks after a boom lands before the next one may
/// fire.
pub const SONIC_BOOM_COOLDOWN_TICKS: i32 = 40;

/// `SonicBoom.checkExtraStartConditions`'s `closerThan(target, 15.0, 20.0)`
/// horizontal leg — `Entity.closerThan`'s `xz` argument, squared for the
/// same reason [`MELEE_RANGE_SQR`] is.
pub const SONIC_BOOM_RANGE_XZ_SQR: f64 = 225.0;
/// The same call's vertical leg (`20.0`), squared.
pub const SONIC_BOOM_RANGE_Y_SQR: f64 = 400.0;

/// `SonicBoom.tick`'s hit: `10.0F` true damage through
/// `level.damageSources().sonicBoom(body)` — `minecraft:sonic_boom`,
/// `bypasses_armor bypasses_enchantments bypasses_shield` in the real
/// datapack table (`lodestone_data::damage_types`), so armour and
/// enchantments do nothing against it but Resistance still can, exactly as
/// vanilla's own `getDamageAfterMagicAbsorb` leaves that stage unbypassed.
pub const SONIC_BOOM_DAMAGE: f32 = 10.0;

/// `SonicBoom.KNOCKBACK_HORIZONTAL`/`KNOCKBACK_VERTICAL` — `target.push(...)`
/// scaled by `(1.0 - knockbackResistance)`. **Not yet applied to a player
/// target** — this crate has no mechanism to deliver a velocity impulse to a
/// player from the server at all yet (ordinary hostile melee against a
/// player has the identical gap — see [`super::PlayerHit`]'s own field
/// list, which carries no knockback vector either). Applied for real against
/// a mob target through the same [`SimMob::apply_knockback`](super::SimMob::apply_knockback)
/// every other mob-on-mob hit in this crate uses.
pub const SONIC_BOOM_KNOCKBACK_HORIZONTAL: f64 = 2.5;
/// See [`SONIC_BOOM_KNOCKBACK_HORIZONTAL`].
pub const SONIC_BOOM_KNOCKBACK_VERTICAL: f64 = 0.5;

/// `Warden.AngerLevel` — the three named buckets [`AngerLevel::from_anger`]
/// derives from a raw anger score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AngerLevel {
    Calm,
    Agitated,
    Angry,
}

impl AngerLevel {
    /// `AngerLevel.byAnger` — the highest bucket whose own minimum the score
    /// clears, falling back to [`AngerLevel::Calm`].
    #[must_use]
    pub fn from_anger(anger: i32) -> Self {
        if anger >= ANGRY_THRESHOLD {
            AngerLevel::Angry
        } else if anger >= AGITATED_THRESHOLD {
            AngerLevel::Agitated
        } else {
            AngerLevel::Calm
        }
    }

    /// `AngerLevel.isAngry()`.
    #[must_use]
    pub fn is_angry(self) -> bool {
        self == AngerLevel::Angry
    }
}

fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
    dx * dx + dy * dy + dz * dz
}

/// `Entity.closerThan(Entity, xz, y)`'s horizontal leg, squared —
/// [`SONIC_BOOM_RANGE_XZ_SQR`]'s own comparand.
fn distance_sqr_xz(a: Vec3, b: Vec3) -> f64 {
    let (dx, dz) = (a.x - b.x, a.z - b.z);
    dx * dx + dz * dz
}

/// `Entity.closerThan(Entity, xz, y)`'s vertical leg, squared.
fn distance_sqr_y(a: Vec3, b: Vec3) -> f64 {
    let dy = a.y - b.y;
    dy * dy
}

impl<'w> MobSim<'w> {
    /// Decays every listener's anger, absorbs the tick's
    /// [`nearest_vibration`](super::SimMob::nearest_vibration) answer, and
    /// lands a hit on an in-range [`AngerLevel::Angry`] target. The resolved
    /// target is retained for the brain to pursue on the next tick.
    ///
    /// Runs after [`resolve_vibrations`](Self::resolve_vibrations) posts this
    /// tick's answer, so a death heard this tick can already raise anger this
    /// tick — the identical "same-tick, not one tick late" reasoning that
    /// method's own doc gives for its own placement.
    ///
    /// **A target that stops existing is not proactively pruned** — only
    /// natural decay-to-zero clears it (see [`warden_anger`](super::SimMob::warden_anger)'s
    /// own doc). `reap_dead` posts `EntityDie` for a mob after removing it from
    /// the roster, so pruning missing targets here would erase anger granted by
    /// that same-tick vibration. The target therefore remains until anger
    /// decays to zero.
    pub(super) fn resolve_warden_anger(&mut self) {
        let mut strikes: Vec<(i32, i32, Vec3)> = Vec::new();
        // The digging stop path also removes its pending anger record; the
        // digging block below performs both parts of that lifecycle.
        let mut discarded: Vec<i32> = Vec::new();
        for mob in &mut self.mobs {
            if !lodestone_entity::vibration::is_vibration_listener(mob.entity_type.path()) {
                continue;
            }
            // Vibration intake is independent of anger actions, so it continues
            // while the emerged pose is active. Only movement and attacks are
            // blocked during that countdown; the strike loop below enforces the
            // attack half by skipping a warden that is still emerging.
            if mob.warden_emerge_ticks > 0 {
                mob.warden_emerge_ticks -= 1;
            }
            if mob.warden_sonic_boom_cooldown > 0 {
                mob.warden_sonic_boom_cooldown -= 1;
            }
            // `Digging`'s own countdown, captured *before* decrementing so
            // the "just finished this tick" and "not digging at all" cases
            // stay distinguishable below — an in-place `== 0` check after
            // the decrement cannot tell those apart, and would let a warden
            // that just finished digging restart one in the same tick.
            let was_digging = mob.warden_digging_ticks > 0;
            if was_digging {
                mob.warden_digging_ticks -= 1;
                if mob.warden_digging_ticks == 0 {
                    // `Digging.stop`: `body.remove(RemovalReason.DISCARDED)`
                    // — a silent despawn, not a death (no loot, no death
                    // sound), the same `Entity.discard()` shape a creeper's
                    // own post-explosion removal already uses in this file.
                    discarded.push(mob.id);
                }
            }
            if mob.warden_anger > 0 {
                mob.warden_anger = (mob.warden_anger - ANGER_DECAY_PER_TICK).max(0);
                if mob.warden_anger == 0 {
                    mob.warden_anger_target = None;
                }
            }
            if let Some(vibration) = mob.nearest_vibration
                && let Some(source) = vibration.source
                && source != mob.id
            {
                if mob.warden_anger_target != Some(source) {
                    mob.warden_anger_target = Some(source);
                    mob.warden_anger = 0;
                }
                mob.warden_anger = (mob.warden_anger + ANGER_INCREASE).min(MAX_ANGER);
            }
            let angry = AngerLevel::from_anger(mob.warden_anger).is_angry();
            // An angry warden refreshes a positive digging cooldown to its full
            // value; a calm warden counts it down. This simulation does not
            // produce the separate sub-Angry disturbance and invalid-target
            // signals that also refresh that cooldown, so a disturbance that
            // stays below the Angry threshold follows the simpler countdown.
            if mob.warden_dig_cooldown > 0 {
                if angry {
                    mob.warden_dig_cooldown = DIGGING_COOLDOWN_TICKS;
                } else {
                    mob.warden_dig_cooldown -= 1;
                }
            } else if !was_digging && mob.warden_emerge_ticks == 0 && !angry {
                // No roar-target signal is modeled, so an emerged calm warden
                // with an expired cooldown starts digging unconditionally.
                // Emergence and digging checks run before attack handling.
                mob.warden_digging_ticks = DIGGING_DURATION_TICKS;
            }
            if mob.warden_emerge_ticks == 0
                && mob.warden_digging_ticks == 0
                && angry
                && let Some(target) = mob.warden_anger_target
            {
                strikes.push((mob.id, target, mob.position()));
            }
        }
        if !discarded.is_empty() {
            self.mobs.retain(|m| !discarded.contains(&m.id));
        }
        for (warden_id, target_id, warden_pos) in strikes {
            // The target may no longer exist (a corpse-derived suspect never
            // will) — anger persists regardless, per this method's own doc;
            // only the strike itself is skipped.
            let Some(target_pos) = self.mobs.iter().find(|m| m.id == target_id).map(super::SimMob::position) else {
                continue;
            };
            let Some(warden) = self.mobs.iter().find(|m| m.id == warden_id) else {
                continue;
            };
            let can_sonic_boom = warden.warden_sonic_boom_cooldown == 0
                && distance_sqr_xz(warden_pos, target_pos) <= SONIC_BOOM_RANGE_XZ_SQR
                && distance_sqr_y(warden_pos, target_pos) <= SONIC_BOOM_RANGE_Y_SQR;
            if can_sonic_boom {
                // `SonicBoom` runs *ahead* of `MeleeAttack` in
                // `WardenAi::initFightActivity`'s own behaviour list — a
                // warden in range and off cooldown always booms rather than
                // melees, exactly matching that ordering rather than the
                // (dead in 26.2 — see this module's own doc for the
                // verification) `TIME_TO_USE_MELEE_UNTIL_SONIC_BOOM` figure
                // this crate's earlier pass cited.
                let flags = DamageFlags::for_damage_type_name("sonic_boom")
                    .expect("sonic_boom is a real damage type");
                if let Some(target) = self.mobs.iter_mut().find(|m| m.id == target_id) {
                    let applied = target.apply_damage(SONIC_BOOM_DAMAGE, flags);
                    if applied > 0.0 {
                        let delta = Vec3::new(
                            target_pos.x - warden_pos.x,
                            target_pos.y - warden_pos.y,
                            target_pos.z - warden_pos.z,
                        );
                        let horizontal = (delta.x * delta.x + delta.z * delta.z).sqrt();
                        let (nx, nz) = if horizontal > 1e-6 {
                            (delta.x / horizontal, delta.z / horizontal)
                        } else {
                            (0.0, 0.0)
                        };
                        let resistance = target.knockback_resistance();
                        target.apply_knockback(Vec3::new(
                            nx * SONIC_BOOM_KNOCKBACK_HORIZONTAL * (1.0 - resistance),
                            SONIC_BOOM_KNOCKBACK_VERTICAL * (1.0 - resistance),
                            nz * SONIC_BOOM_KNOCKBACK_HORIZONTAL * (1.0 - resistance),
                        ));
                    }
                    target.mob.note_hurt(Some(warden_pos));
                    self.note_vocalisation(target_id, applied);
                }
                if let Some(warden) = self.mobs.iter_mut().find(|m| m.id == warden_id) {
                    warden.warden_sonic_boom_cooldown = SONIC_BOOM_COOLDOWN_TICKS;
                }
                continue;
            }
            if distance_sqr(warden_pos, target_pos) > MELEE_RANGE_SQR {
                continue;
            }
            let raw_damage = warden.attack_damage();
            if let Some(target) = self.mobs.iter_mut().find(|m| m.id == target_id) {
                let applied = target.apply_damage(raw_damage, DamageFlags::default());
                target.mob.note_hurt(Some(warden_pos));
                self.note_vocalisation(target_id, applied);
            }
        }
    }
}

#[cfg(test)]
mod warden_anger_tests {
    use lodestone_entity::ai::MobController;
    use lodestone_entity::vibration::{PostedVibration, VibrationEvent};

    use super::super::ChunkWorld;
    use super::*;

    fn flat_world() -> ChunkWorld {
        ChunkWorld::new(-64, 384)
    }

    fn spawn(sim: &mut MobSim<'_>, species: &str, pos: Vec3) -> i32 {
        sim.spawn_species(format!("minecraft:{species}").parse().expect("valid key"), pos)
            .id()
    }

    /// `AngerLevel::from_anger` matches `AngerLevel.byAnger`'s own three
    /// buckets exactly at their boundaries.
    #[test]
    fn anger_level_buckets_match_the_named_thresholds() {
        assert_eq!(AngerLevel::from_anger(0), AngerLevel::Calm);
        assert_eq!(AngerLevel::from_anger(39), AngerLevel::Calm);
        assert_eq!(AngerLevel::from_anger(40), AngerLevel::Agitated);
        assert_eq!(AngerLevel::from_anger(79), AngerLevel::Agitated);
        assert_eq!(AngerLevel::from_anger(80), AngerLevel::Angry);
        assert_eq!(AngerLevel::from_anger(150), AngerLevel::Angry);
        assert!(AngerLevel::Angry.is_angry());
        assert!(!AngerLevel::Agitated.is_angry());
    }

    /// The headline case, wired through a real tick: a warden that hears a
    /// nearby death gets angry at the dead mob's own id
    /// (`increaseAngerAt(sourceEntity)`'s no-projectile branch), by exactly
    /// [`ANGER_INCREASE`] — real arithmetic, not merely "went up".
    #[test]
    fn a_warden_gets_angry_at_a_heard_deaths_own_source() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let victim = spawn(&mut sim, "zombie", Vec3::new(10.0, 0.0, 0.0));
        sim.get_mut(victim).expect("spawned").mob.damage_self(1_000.0);

        sim.tick();

        let w = sim.get(warden).expect("spawned");
        assert_eq!(w.warden_anger_target(), Some(victim));
        assert_eq!(w.warden_anger(), ANGER_INCREASE, "one absorbed vibration is exactly increaseAngerAt's default 35");
        assert_eq!(w.warden_anger_level(), AngerLevel::Calm, "35 < 40, the Agitated floor");
    }

    /// Anger decays by exactly [`ANGER_DECAY_PER_TICK`] per tick once nothing
    /// new is heard, and the target is cleared once it reaches zero — the
    /// discriminating control against "anger only ever goes up".
    #[test]
    fn anger_decays_by_one_per_tick_and_clears_the_target_at_zero() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let victim = spawn(&mut sim, "zombie", Vec3::new(10.0, 0.0, 0.0));
        sim.get_mut(victim).expect("spawned").mob.damage_self(1_000.0);
        sim.tick();
        assert_eq!(sim.get(warden).expect("spawned").warden_anger(), ANGER_INCREASE);

        sim.tick();
        assert_eq!(
            sim.get(warden).expect("spawned").warden_anger(),
            ANGER_INCREASE - 1,
            "one further tick with nothing new heard must decay by exactly one"
        );

        for _ in 0..(ANGER_INCREASE - 1) {
            sim.tick();
        }
        let w = sim.get(warden).expect("spawned");
        assert_eq!(w.warden_anger(), 0);
        assert_eq!(w.warden_anger_target(), None, "the target must clear once anger reaches zero");
    }

    /// A different source replaces the tracked suspect outright rather than
    /// being tracked alongside it — this module's own disclosed
    /// single-suspect narrowing, made observable.
    #[test]
    fn a_second_source_replaces_the_tracked_suspect() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let first = spawn(&mut sim, "zombie", Vec3::new(5.0, 0.0, 0.0));
        let second = spawn(&mut sim, "zombie", Vec3::new(6.0, 0.0, 0.0));
        sim.get_mut(first).expect("spawned").mob.damage_self(1_000.0);
        sim.tick();
        assert_eq!(sim.get(warden).expect("spawned").warden_anger_target(), Some(first));

        sim.get_mut(second).expect("spawned").mob.damage_self(1_000.0);
        sim.tick();
        let w = sim.get(warden).expect("spawned");
        assert_eq!(w.warden_anger_target(), Some(second), "the newest source must replace the old one");
        assert_eq!(w.warden_anger(), ANGER_INCREASE, "the replacement resets anger before absorbing the new event");
    }

    /// The end-to-end consequence: an already-angry warden standing within
    /// melee range of its live target lands a real hit through the same
    /// `apply_damage` pipeline every other hit in this crate uses — reaching
    /// a real health change, not merely an internal anger counter.
    #[test]
    fn an_angry_warden_in_range_lands_a_real_hit() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        // Give the warden the anger directly — this test's subject is the
        // *consequence* of being angry, not the accumulation arithmetic
        // (covered above). One above the floor, so this tick's decay cannot
        // drop it out of `AngerLevel::Angry` before the strike is attempted.
        let target = spawn(&mut sim, "pig", Vec3::new(1.0, 0.0, 0.0));
        {
            let w = sim.get_mut(warden).expect("spawned");
            w.warden_anger = ANGRY_THRESHOLD + 1;
            w.warden_anger_target = Some(target);
            // Already past its emerge window — this test's subject is a
            // standing fight, not a fresh spawn (see
            // `an_emerging_warden_lands_no_hit_even_when_angry_and_in_range`
            // for that half).
            w.warden_emerge_ticks = 0;
        }
        let health_before = sim.get(target).expect("spawned").health();

        sim.tick();

        let health_after = sim.get(target).expect("spawned").health();
        assert!(health_after < health_before, "an angry, in-range warden must land a real hit: {health_before} -> {health_after}");
    }

    /// **Control**: the identical setup, but with the target well outside
    /// [`MELEE_RANGE_SQR`] — no hit must land, proving the range gate above
    /// is real rather than decorative (and, since anger is one above the
    /// floor here too, that this is genuinely the range branch rather than
    /// decay quietly making the warden not-angry before the attempt).
    #[test]
    fn an_angry_warden_out_of_range_lands_no_hit() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let target = spawn(&mut sim, "pig", Vec3::new(50.0, 0.0, 0.0));
        {
            let w = sim.get_mut(warden).expect("spawned");
            w.warden_anger = ANGRY_THRESHOLD + 1;
            w.warden_anger_target = Some(target);
            // Past its emerge window — this is the range control, not the
            // emerge one (see the dedicated emerge test below).
            w.warden_emerge_ticks = 0;
        }
        let health_before = sim.get(target).expect("spawned").health();

        sim.tick();

        assert_eq!(sim.get(target).expect("spawned").health(), health_before, "50 blocks away is far outside a 3-block melee reach and well past sonic boom's own 15/20 range");
    }

    /// The pursuit half of the vibration-driven warden behavior, driven through real ticks
    /// rather than by placing the target already in range. The
    /// `lodestone-entity`-side gates (`brain::roster`'s
    /// `a_warden_with_a_live_grudge_walks_toward_it_and_never_attacks_directly`)
    /// prove `WalkToPoi` is wired into `warden_brain`'s `FIGHT` activity in
    /// isolation, against a hermetic `BrainMob` double; this proves the
    /// **whole** production chain — a host-tracked anger target, through
    /// `feed_perception`'s per-tick resolution, through the real `Brain`,
    /// through `NavigatingMob`'s real A\*/kinematic follower, to an actual
    /// position change and the real hit `resolve_warden_anger` already
    /// lands once in range — the same "which production tick path reaches
    /// it" standard every other gate in this module already meets.
    #[test]
    fn an_angry_warden_starting_out_of_range_chases_its_target_down_and_lands_a_hit() {
        // A floor wide enough for both mobs plus the warden's whole chase
        // route, so nothing here depends on either mob standing in the void
        // (an idle mob with no ground under it falls instead of walking).
        let mut world = ChunkWorld::new(-64, 384);
        for x in -4..=20 {
            for z in -4..=4 {
                world.set_solid(x, -1, z, true);
            }
        }
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let target = spawn(&mut sim, "pig", Vec3::new(10.0, 0.0, 0.0));
        {
            let w = sim.get_mut(warden).expect("spawned");
            // `MAX_ANGER`, not merely `ANGRY_THRESHOLD + 1`: closing 10
            // blocks at the warden's own real ~0.2-block/tick ground speed
            // (`movement_speed` 0.3 through `ai_ground_speed`) takes several
            // dozen real ticks, and anger decays by `ANGER_DECAY_PER_TICK`
            // every tick regardless of pursuit progress — a smaller starting
            // value could decay all the way to `Calm` before the warden ever
            // arrives, which would test decay rather than pursuit.
            w.warden_anger = MAX_ANGER;
            w.warden_anger_target = Some(target);
            // Past its emerge window — this test's subject is the chase, not
            // the spawn animation, and 134 ticks of emerge decaying anger
            // with nothing acted on would exhaust the `MAX_ANGER` budget
            // this loop relies on before the warden ever took a step.
            w.warden_emerge_ticks = 0;
            // Forced past the loop's own tick budget: the target starts 10
            // blocks away, inside sonic boom's own 15-block range, so
            // without this a boom would land on tick 0 with no chase at all
            // — this test's whole subject is pursuit, not the ranged
            // attack (see `a_sonic_boom_lands_on_an_in_range_target` for
            // that one).
            w.warden_sonic_boom_cooldown = MAX_ANGER + 1;
        }
        let health_before = sim.get(target).expect("spawned").health();
        let target_pos = Vec3::new(10.0, 0.0, 0.0);

        let mut hit_tick = None;
        for t in 0..MAX_ANGER {
            sim.tick();
            // Pinned back every tick: a real `pig` carries its own
            // `RandomStrollGoal` and, left alone, wanders roughly as fast as
            // the warden closes — this test's subject is whether the warden
            // *chases*, not an unrelated race against a second mob's own
            // wander RNG. Re-teleporting isolates that one variable, the
            // same "pin everything but the one thing under test" shape a
            // hermetic gate elsewhere in this crate already uses.
            sim.get_mut(target).expect("spawned").teleport_to(target_pos);
            if sim.get(target).expect("spawned").health() < health_before {
                hit_tick = Some(t);
                break;
            }
        }

        assert!(
            hit_tick.is_some(),
            "an angry warden starting 10 blocks from its target must chase it down \
             (warden_brain's FIGHT activity) and land the real hit resolve_warden_anger \
             already resolves once in range, inside its own anger's decay window"
        );
    }

    /// An absent target must never crash the strike
    /// resolution, and — this module's own disclosed simplification — its
    /// anger is **not** proactively cleared; only natural decay does that.
    /// Set well above [`ANGRY_THRESHOLD`] so a tick's decay cannot drop it
    /// out of [`AngerLevel::Angry`] before the strike attempt runs, proving
    /// this is the "target missing" branch and not the "no longer angry"
    /// one.
    #[test]
    fn an_angry_warden_whose_target_no_longer_exists_lands_no_hit_and_keeps_its_anger() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        {
            let w = sim.get_mut(warden).expect("spawned");
            w.warden_anger = ANGRY_THRESHOLD + 5;
            w.warden_anger_target = Some(999_999);
            w.warden_emerge_ticks = 0;
        }

        sim.tick();

        let w = sim.get(warden).expect("spawned");
        assert_eq!(w.warden_anger(), ANGRY_THRESHOLD + 4, "anger still decays normally with a stale target");
        assert_eq!(w.warden_anger_target(), Some(999_999), "a stale target is not proactively pruned");
    }

    /// A freshly-spawned warden is angry and in melee range on tick one —
    /// and lands **no** hit, because `EMERGE` outranks `FIGHT`
    /// (`WardenAi::updateActivity`). The discriminating control against "the
    /// emerge gate does nothing": everything else about this setup is
    /// identical to `an_angry_warden_in_range_lands_a_real_hit`, which does
    /// land a hit once `warden_emerge_ticks` is zeroed.
    #[test]
    fn an_emerging_warden_lands_no_hit_even_when_angry_and_in_range() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let target = spawn(&mut sim, "pig", Vec3::new(1.0, 0.0, 0.0));
        {
            let w = sim.get_mut(warden).expect("spawned");
            w.warden_anger = ANGRY_THRESHOLD + 1;
            w.warden_anger_target = Some(target);
        }
        let health_before = sim.get(target).expect("spawned").health();

        sim.tick();

        assert_eq!(
            sim.get(target).expect("spawned").health(),
            health_before,
            "a warden still emerging must not strike even an adjacent, angry-target-eligible mob"
        );
    }

    /// The reciprocal control: an emerging warden is invulnerable to a real
    /// incoming hit — `Warden.isInvulnerableTo`'s own `isDiggingOrEmerging`
    /// gate, not merely "does not act".
    #[test]
    fn an_emerging_warden_takes_no_damage_from_a_real_hit() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let health_before = sim.get(warden).expect("spawned").health();

        let applied = sim.get_mut(warden).expect("spawned").apply_damage(1000.0, DamageFlags::default());

        assert_eq!(applied, 0.0, "a hit against an emerging warden must apply zero damage");
        assert_eq!(sim.get(warden).expect("spawned").health(), health_before);
    }

    /// A warden past its emerge window sends `Pose::STANDING` (`0`); one
    /// still emerging sends `Pose::EMERGING` (`13`, the real jar ordinal —
    /// see [`POSE_EMERGING`]'s own doc). Both directions asserted, since a
    /// snapshot test that only ever checks the non-default value cannot see
    /// a missing "reset to standing" arm — exactly the "the reset must reach
    /// the client too" hazard `MetadataField::Pose`'s own doc names.
    #[test]
    fn warden_pose_metadata_reports_emerging_then_standing() {
        use crate::protocol::MetadataField;

        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));

        let mid_emerge = sim.get(warden).expect("spawned").snapshot();
        assert!(
            mid_emerge.metadata.contains(&MetadataField::Pose(POSE_EMERGING)),
            "a freshly spawned warden must report the real Pose.EMERGING ordinal: {:?}",
            mid_emerge.metadata
        );

        for _ in 0..EMERGE_DURATION_TICKS {
            sim.tick();
        }

        let after_emerge = sim.get(warden).expect("spawned").snapshot();
        assert!(
            after_emerge.metadata.contains(&MetadataField::Pose(POSE_STANDING)),
            "the pose must revert to Standing once EMERGE_DURATION_TICKS elapses, not stay stuck at Emerging: {:?}",
            after_emerge.metadata
        );
    }

    /// The ranged attack, end to end: an angry warden past its emerge window
    /// with a target 10 blocks away (inside `SonicBoom`'s 15/20 range, well
    /// outside `MELEE_RANGE_SQR`'s 3 blocks) lands a real true-damage hit —
    /// proving the attack fires without requiring melee range — and starts
    /// its own cooldown so an immediate second tick lands **no** further hit
    /// even though the warden is still angry and still in range.
    #[test]
    fn a_sonic_boom_lands_on_a_mid_range_target_and_then_cools_down() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let target = spawn(&mut sim, "pig", Vec3::new(10.0, 0.0, 0.0));
        {
            let w = sim.get_mut(warden).expect("spawned");
            w.warden_anger = ANGRY_THRESHOLD + 1;
            w.warden_anger_target = Some(target);
            w.warden_emerge_ticks = 0;
        }
        {
            // A pig's real 10.0 max health would be exactly killed by one
            // 10.0-damage boom (and then reaped before the second tick's
            // assertion could even find it) — this test's subject is the
            // cooldown, not lethality, so give the target enough health to
            // survive the first hit and still be alive to assert against.
            let t = sim.get_mut(target).expect("spawned");
            t.health = 100.0;
            t.max_health = 100.0;
        }
        let health_before = sim.get(target).expect("spawned").health();

        sim.tick();

        let health_after_boom = sim.get(target).expect("spawned").health();
        assert!(
            health_after_boom < health_before,
            "a mid-range angry warden must land a real sonic-boom hit: {health_before} -> {health_after_boom}"
        );
        assert_eq!(
            sim.get(warden).expect("spawned").warden_sonic_boom_cooldown,
            SONIC_BOOM_COOLDOWN_TICKS,
            "the boom must set the full cooldown on the tick it lands"
        );

        sim.tick();

        assert_eq!(
            sim.get(target).expect("spawned").health(),
            health_after_boom,
            "a second boom must not land while the first one's cooldown is still counting down"
        );
    }

    /// Every `VibrationEvent` construction in this file must still compile
    /// against the real enum shape (`source` field present) — cheap
    /// insurance that this module's own imports stay honest if the
    /// substrate's shape changes again.
    #[test]
    fn posted_vibration_carries_a_source() {
        let v = PostedVibration {
            position: Vec3::new(0.0, 0.0, 0.0),
            event: VibrationEvent::EntityDie,
            source: Some(1),
        };
        assert_eq!(v.source, Some(1));
    }

    /// The resolved ambiguity, end to end through the real
    /// production `MobSim::tick` path: a warden left completely alone
    /// (never fought, never disturbed) past its emerge window and the full
    /// `DIGGING_COOLDOWN_TICKS` becomes digging-eligible, reports the real
    /// `Pose.DIGGING` ordinal, and despawns outright once
    /// `DIGGING_DURATION_TICKS` elapses — `Digging.stop`'s own
    /// `Entity.RemovalReason.DISCARDED`, not a state reset back to
    /// `IDLING`.
    #[test]
    fn an_undisturbed_warden_eventually_digs_and_despawns() {
        use crate::protocol::MetadataField;

        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));

        let mut saw_digging_pose = false;
        for _ in 0..(EMERGE_DURATION_TICKS + DIGGING_COOLDOWN_TICKS) {
            sim.tick();
            if sim
                .get(warden)
                .expect("still alive before the dig completes")
                .snapshot()
                .metadata
                .contains(&MetadataField::Pose(POSE_DIGGING))
            {
                saw_digging_pose = true;
                break;
            }
        }
        assert!(
            saw_digging_pose,
            "an undisturbed warden must eventually start digging and report the real Pose.DIGGING ordinal"
        );

        for _ in 0..DIGGING_DURATION_TICKS {
            sim.tick();
        }
        assert!(
            sim.get(warden).is_none(),
            "a warden that finishes digging must despawn outright, not merely reset its pose"
        );
    }

    /// **Control**: a warden kept angry throughout the whole cooldown window
    /// must never let `warden_dig_cooldown` reach zero —
    /// `WardenAi.DIG_COOLDOWN_SETTER`'s own "refresh only while present"
    /// shape, ported directly. Without this refresh, the positive test above
    /// would pass for the wrong reason (any warden digs eventually,
    /// regardless of how much fighting it did).
    #[test]
    fn a_continuously_angry_warden_never_lets_the_dig_cooldown_expire() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let victim = spawn(&mut sim, "zombie", Vec3::new(5.0, 0.0, 0.0));

        for _ in 0..(EMERGE_DURATION_TICKS + DIGGING_COOLDOWN_TICKS) {
            // Re-arms anger directly every tick, standing in for a
            // continuous stream of heard vibrations — the substrate itself
            // is already covered by `vibration_substrate_tests`; this
            // test's own job is the cooldown-refresh interaction, not
            // re-proving delivery.
            if let Some(m) = sim.get_mut(warden) {
                m.warden_anger = MAX_ANGER;
                m.warden_anger_target = Some(victim);
            }
            sim.tick();
            assert_ne!(
                sim.get(warden).expect("alive").warden_dig_cooldown,
                0,
                "a continuously angry warden must never let its dig cooldown reach zero"
            );
        }
    }
}
