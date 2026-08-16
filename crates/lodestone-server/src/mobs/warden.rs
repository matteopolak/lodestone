//! The warden's anger consumer (issue #459's step 3): turns
//! [`SimMob::nearest_vibration`](super::SimMob::nearest_vibration) into real
//! anger, and real anger into a real melee hit.
//!
//! # What it is
//!
//! [`AngerLevel`] is vanilla's `AngerLevel` (`Warden.java`'s own nested
//! enum) — `Calm`/`Agitated`/`Angry`, bucketed from a `0..=150` anger score
//! by [`AngerLevel::from_anger`]. [`MobSim::resolve_warden_anger`] is the
//! per-tick consumer: it decays every warden's anger by
//! [`ANGER_DECAY_PER_TICK`] (`AngerManagement.tick`'s own
//! `DEFAULT_ANGER_DECREASE`), absorbs this tick's
//! [`nearest_vibration`](super::SimMob::nearest_vibration) answer by
//! [`ANGER_INCREASE`] (`Warden.increaseAngerAt`'s own default amount) at its
//! `source`, and — once a warden is [`AngerLevel::Angry`] and its target is
//! close enough — lands a real melee hit through the same
//! [`SimMob::apply_damage`](super::SimMob::apply_damage) pipeline every other
//! hit in this crate goes through (armour, i-frames, hurt sound all
//! included).
//!
//! # How it works — and what it deliberately narrows
//!
//! **Single-suspect, not vanilla's `AngerManagement` suspect list.** Vanilla
//! tracks several candidate suspects at once (`angerBySuspect`, an
//! `Object2IntMap<Entity>`) and picks the angriest via
//! `AngerManagement.Sorter` (angry-first, then player, then highest score).
//! This crate tracks exactly one:
//! [`SimMob::warden_anger_target`](super::SimMob::warden_anger_target). A
//! vibration from a **different** source than the current target replaces
//! it outright (reset to `0`, then absorb the new event) rather than being
//! tracked alongside it. That is a real behavioural narrowing — a warden
//! juggling two live threats here forgets the first the instant it hears the
//! second — not a silent approximation, and it is exactly the same shape
//! [`super::raid::MobSim::create_or_extend_raid`]'s own doc discloses for a
//! single-slot stand-in of a vanilla multi-item structure.
//!
//! **Pursuit now exists, on a separate seam from this module.** An angry
//! warden's own [`Brain`](lodestone_entity::brain::Brain) runs
//! `lodestone_entity::brain::roster::warden_brain`'s `FIGHT` activity, which
//! walks toward [`MobController::angry_target`](lodestone_entity::ai::MobController::angry_target)
//! — fed, once per tick, by [`MobSim::feed_perception`](super::MobSim::feed_perception)
//! resolving [`warden_anger_target`](super::SimMob::warden_anger_target) to a
//! live position and gated on [`AngerLevel::Angry`]. That behaviour only ever
//! walks; it never calls `BrainMob::attack`, so this module's own
//! [`resolve_warden_anger`] stays the single place a hit is actually
//! resolved — one production path, not two racing ones. One disclosed lag:
//! the position fed to the brain is *last* tick's [`resolve_warden_anger`]
//! answer (that method runs after the per-mob brain tick each tick, the same
//! ordering [`resolve_vibrations`](super::MobSim::resolve_vibrations)'s own
//! doc explains), so a freshly-angered warden starts walking one tick after
//! the anger that caused it, not the same tick. **No dig/emerge and no sonic
//! boom** still: both need machinery (a pose/animation state for dig/emerge,
//! a ranged burst-damage attack for sonic boom) this module does not build.
//!
//! # How to change it
//!
//! - **Pursuit speed/stop distance**: `lodestone_entity::brain::roster::warden_brain`'s
//!   own constants (`SCAFFOLD_STROLL_SPEED`, `WARDEN_PURSUIT_CLOSE_ENOUGH`),
//!   not this module — this module has no movement code of its own.
//! - **Sonic boom**: `Warden.java`'s own `TIME_TO_USE_MELEE_UNTIL_SONIC_BOOM`
//!   (200 ticks) and
//!   `net.minecraft.world.entity.ai.behavior.warden.SonicBoom` (10.0 true
//!   damage, a fixed knockback) are the transcription source.
//! - **A second listener species**: `lodestone_entity::vibration`'s own
//!   module doc already names this seam (`is_vibration_listener`); this
//!   module's anger/attack consumer is warden-specific and would need a
//!   second table if a calibrated sculk sensor (not a mob) ever needed
//!   anger of its own — it does not, sensors just shriek.
//!
//! # Dependencies
//!
//! [`lodestone_entity::vibration`] for [`PostedVibration`]; nothing else new.

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

impl<'w> MobSim<'w> {
    /// Issue #459's step 3: decays every listener's anger, absorbs this
    /// tick's [`nearest_vibration`](super::SimMob::nearest_vibration) answer,
    /// and lands a real hit on an in-range [`AngerLevel::Angry`] target — see
    /// this module's own doc for the single-suspect narrowing and the "no
    /// pursuit" gap.
    ///
    /// Runs after [`resolve_vibrations`](Self::resolve_vibrations) posts this
    /// tick's answer, so a death heard this tick can already raise anger this
    /// tick — the identical "same-tick, not one tick late" reasoning that
    /// method's own doc gives for its own placement.
    ///
    /// **A target that stops existing is not proactively pruned** — only
    /// natural decay-to-zero clears it (see [`warden_anger`](super::SimMob::warden_anger)'s
    /// own doc). Vanilla's `AngerManagement.tick` *does* drop an invalid
    /// suspect immediately, but doing that here would fight the very
    /// producer this substrate has: `reap_dead` posts `EntityDie` for a mob
    /// already removed from the roster, so a corpse is *always* "invalid" by
    /// the time this method sees it, and pruning on that basis would erase
    /// the anger the vibration just granted on the same tick it was granted.
    /// A real, disclosed narrowing rather than a silent one — see this
    /// module's own "no pursuit" gap for the same honesty standard.
    pub(super) fn resolve_warden_anger(&mut self) {
        let mut strikes: Vec<(i32, i32, Vec3)> = Vec::new();
        for mob in &mut self.mobs {
            if !lodestone_entity::vibration::is_vibration_listener(mob.entity_type.path()) {
                continue;
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
            if AngerLevel::from_anger(mob.warden_anger).is_angry()
                && let Some(target) = mob.warden_anger_target
            {
                strikes.push((mob.id, target, mob.position()));
            }
        }
        for (warden_id, target_id, warden_pos) in strikes {
            // The target may no longer exist (a corpse-derived suspect never
            // will) — anger persists regardless, per this method's own doc;
            // only the strike itself is skipped.
            let Some(target_pos) = self.mobs.iter().find(|m| m.id == target_id).map(super::SimMob::position) else {
                continue;
            };
            if distance_sqr(warden_pos, target_pos) > MELEE_RANGE_SQR {
                continue;
            }
            let Some(warden) = self.mobs.iter().find(|m| m.id == warden_id) else {
                continue;
            };
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
        }
        let health_before = sim.get(target).expect("spawned").health();

        sim.tick();

        assert_eq!(sim.get(target).expect("spawned").health(), health_before, "50 blocks away is far outside a 3-block melee reach");
    }

    /// The pursuit half of issue #459 step 3, driven through real ticks
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

    /// A target that no longer exists must never crash the strike
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
        }

        sim.tick();

        let w = sim.get(warden).expect("spawned");
        assert_eq!(w.warden_anger(), ANGRY_THRESHOLD + 4, "anger still decays normally with a stale target");
        assert_eq!(w.warden_anger_target(), Some(999_999), "a stale target is not proactively pruned");
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
}
