//! `MobSim`'s thrown-eye-of-ender slice: spawn, the per-tick flight toward a
//! signalled target, and the end-of-life drop or shatter.
//!
//! # What it is
//!
//! An eye of ender thrown at a stronghold is a small tracked entity with no
//! collision and no gravity. It is told a target once, flies toward it under a
//! fixed steering rule, hovers, and after 80 ticks either lands as a dropped
//! `ender_eye` item (four throws in five) or shatters in a particle burst.
//!
//! # How it flies
//!
//! * [`signal_target`] clamps the target: beyond [`MAX_REACH`] blocks
//!   horizontally the eye aims at a point [`MAX_REACH`] blocks out along the
//!   same bearing and [`RISE`] blocks above its launch height; inside that
//!   range it aims at the real target.
//! * [`steer`] runs every tick. The horizontal speed eases a quarter of a
//!   percent of the way toward the remaining horizontal distance, both
//!   components are damped to 0.8x inside one block, and the vertical speed
//!   relaxes by 1.5% toward +1 or -1 depending on whether the eye is below or
//!   above the target height.
//! * The position advances by the velocity held at the start of the tick, then
//!   the velocity is steered from the new position. A freshly spawned eye
//!   therefore sits still on its first tick.
//!
//! # Where it ends
//!
//! Once its age exceeds [`LIFETIME`] the eye plays the break sound and is
//! removed. With 80% probability (decided at launch) it becomes an item entity
//! at its final position; otherwise a level event 2003 (the eye-shatter
//! particles) goes out at its block position. Both outcomes ride the existing
//! item stream and effect lane, so nothing here talks to a connection.
//!
//! # How to change it
//!
//! Constants below are the tuning surface. The target is chosen by the caller
//! (`crate::server`'s use-item arm asks the chunk source to locate the nearest
//! stronghold), never here. Eyes are transient: they are not persisted, and a
//! save and reload drops any that were in flight.

use lodestone_model::{BlockPos, ResourceKey, SoundCategory, Vec3};
use uuid::Uuid;

use super::{MobSim, TrackedEye};
use crate::effects::WorldEffect;
use crate::mob_spawn::SpawnRng;

/// Seed of the stream deciding drop-versus-shatter and the dropped item's
/// scatter.
pub(super) const EYE_SEED: u64 = 0x4559_455f_4f46_5f45;

/// Ticks after which the eye ends its flight (it ends on the tick its age
/// passes this value).
pub const LIFETIME: i32 = 80;

/// Horizontal distance beyond which the eye aims at a clamped point instead of
/// the true target.
pub const MAX_REACH: f64 = 12.0;

/// How far above its launch height a clamped target sits.
pub const RISE: f64 = 8.0;

/// Fraction of the remaining horizontal distance the horizontal speed eases
/// toward each tick.
const SPEED_EASE: f64 = 0.0025;

/// Per-tick relaxation of the vertical speed toward +-1.
const RISE_EASE: f64 = 0.015;

/// Both velocity components are scaled by this inside one block of the target.
const HOVER_DAMPING: f64 = 0.8;

/// The level event that bursts the shatter particles and sound.
pub const LEVEL_EVENT_SHATTER: i32 = 2003;

/// The entity-type key an eye streams as.
pub(super) fn eye_entity_type() -> ResourceKey {
    "minecraft:eye_of_ender"
        .parse()
        .expect("`minecraft:eye_of_ender` is a valid resource key")
}

fn ender_eye_item() -> ResourceKey {
    "minecraft:ender_eye"
        .parse()
        .expect("`minecraft:ender_eye` is a valid resource key")
}

/// The point an eye launched from `origin` steers toward for `target`.
#[must_use]
pub fn signal_target(origin: Vec3, target: Vec3) -> Vec3 {
    let dx = target.x - origin.x;
    let dz = target.z - origin.z;
    let horizontal = dx.hypot(dz);
    if horizontal > MAX_REACH {
        Vec3::new(
            origin.x + dx / horizontal * MAX_REACH,
            origin.y + RISE,
            origin.z + dz / horizontal * MAX_REACH,
        )
    } else {
        target
    }
}

/// The velocity after one tick's steering, given the velocity the tick began
/// with, the eye's position after moving and its target.
#[must_use]
pub fn steer(velocity: Vec3, position: Vec3, target: Vec3) -> Vec3 {
    let hx = target.x - position.x;
    let hz = target.z - position.z;
    let horizontal = hx.hypot(hz);
    let current = velocity.x.hypot(velocity.z);
    let mut wanted_speed = current + SPEED_EASE * (horizontal - current);
    let mut vy = velocity.y;
    if horizontal < 1.0 {
        wanted_speed *= HOVER_DAMPING;
        vy *= HOVER_DAMPING;
    }
    let toward_y = if position.y - velocity.y < target.y { 1.0 } else { -1.0 };
    // Standing exactly over the target has no bearing to follow.
    let (sx, sz) = if horizontal > 0.0 {
        let scale = wanted_speed / horizontal;
        (hx * scale, hz * scale)
    } else {
        (0.0, 0.0)
    };
    Vec3::new(sx, vy + (toward_y - vy) * RISE_EASE, sz)
}

impl<'w> MobSim<'w> {
    /// Launches an eye at `position` toward `target` and returns its entity id.
    /// `target` is clamped by [`signal_target`].
    pub fn spawn_eye_of_ender(&mut self, position: Vec3, target: Vec3) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        let survives = self.eye_rng.next_int(5) > 0;
        self.eyes.insert(
            id,
            TrackedEye {
                uuid: Uuid::new_v4(),
                position,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                target: signal_target(position, target),
                life: 0,
                survives,
            },
        );
        id
    }

    /// How many eyes are in flight.
    #[must_use]
    pub fn eye_count(&self) -> usize {
        self.eyes.len()
    }

    /// The position and velocity of the eye `id`, if it is in flight.
    #[must_use]
    pub fn eye_motion(&self, id: i32) -> Option<(Vec3, Vec3)> {
        self.eyes.get(&id).map(|eye| (eye.position, eye.velocity))
    }

    /// Advances every eye one tick and resolves the ones whose flight ended.
    pub fn tick_eyes(&mut self) {
        if self.eyes.is_empty() {
            return;
        }
        let mut ids: Vec<i32> = self.eyes.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(eye) = self.eyes.get_mut(&id) else {
                continue;
            };
            let moved = Vec3::new(
                eye.position.x + eye.velocity.x,
                eye.position.y + eye.velocity.y,
                eye.position.z + eye.velocity.z,
            );
            eye.velocity = steer(eye.velocity, moved, eye.target);
            eye.position = moved;
            eye.life += 1;
            if eye.life <= LIFETIME {
                continue;
            }
            let ended = self.eyes.remove(&id).expect("eye was just read");
            self.finish_eye(&ended);
        }
    }

    fn finish_eye(&mut self, eye: &TrackedEye) {
        let seed = (self.eye_rng.next_f64() * i64::MAX as f64) as i64;
        self.pending_vocalisations.push(WorldEffect::Sound {
            sound: "minecraft:entity.ender_eye.death".to_owned(),
            category: SoundCategory::Neutral,
            pos: eye.position,
            volume: 1.0,
            pitch: 1.0,
            seed,
        });
        if eye.survives {
            let scatter = |rng: &mut SpawnRng| rng.next_f64() * 0.2 - 0.1;
            let velocity = Vec3::new(scatter(&mut self.eye_rng), 0.2, scatter(&mut self.eye_rng));
            self.spawn_item(
                ender_eye_item(),
                eye.position,
                velocity,
                lodestone_entity::item_entity::ItemLifecycle::newly_dropped(1, 64),
            );
        } else {
            self.pending_vocalisations.push(WorldEffect::LevelEvent {
                event: LEVEL_EVENT_SHATTER,
                pos: BlockPos::new(
                    eye.position.x.floor() as i32,
                    eye.position.y.floor() as i32,
                    eye.position.z.floor() as i32,
                ),
                data: 0,
                global: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }

    /// Far targets are clamped to 12 blocks out and 8 up; near ones are kept.
    #[test]
    fn signal_target_clamps_beyond_twelve_blocks() {
        let far = signal_target(v(10.0, 64.0, -5.0), v(10.0, 0.0, 395.0));
        close(far.x, 10.0);
        close(far.y, 72.0);
        close(far.z, 7.0);
        let diagonal = signal_target(v(0.0, 0.0, 0.0), v(30.0, 99.0, 40.0));
        close(diagonal.x, 7.2);
        close(diagonal.z, 9.6);
        let near = signal_target(v(0.0, 70.0, 0.0), v(6.0, 3.0, 8.0));
        assert_eq!(near, v(6.0, 3.0, 8.0));
    }

    /// Hand-worked from the steering rule for a launch at the origin aimed at
    /// (12, 8, 0): tick one speeds to 0.0025 * 12, tick two adds
    /// 0.0025 * (11.97 - 0.03) to that.
    #[test]
    fn steering_first_two_ticks_match_the_hand_computation() {
        let target = v(12.0, 8.0, 0.0);
        let first = steer(v(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), target);
        close(first.x, 0.03);
        close(first.y, 0.015);
        close(first.z, 0.0);
        let second = steer(first, v(0.03, 0.015, 0.0), target);
        close(second.x, 0.059_85);
        close(second.y, 0.015 + (1.0 - 0.015) * 0.015);
    }

    /// Inside a block of the target both components shrink by 0.8x, and a
    /// target below the eye pulls the vertical speed toward -1.
    #[test]
    fn hover_damps_and_a_lower_target_pulls_down() {
        let steered = steer(v(0.1, 0.5, 0.0), v(0.0, 10.0, 0.0), v(0.5, 0.0, 0.0));
        // current = 0.1; wanted = 0.1 + 0.0025 * (0.5 - 0.1) = 0.101, x0.8.
        close(steered.x, 0.5 * (0.101 * 0.8 / 0.5));
        // vy = 0.5 * 0.8 = 0.4, relaxing toward -1.
        close(steered.y, 0.4 + (-1.0 - 0.4) * 0.015);
    }

    #[test]
    fn an_eye_over_its_target_stays_finite() {
        let steered = steer(v(0.0, 0.0, 0.0), v(3.0, 4.0, 3.0), v(3.0, 4.0, 3.0));
        assert!(steered.x.is_finite() && steered.y.is_finite() && steered.z.is_finite());
    }

    fn sim_world() -> crate::ChunkWorld {
        crate::ChunkWorld::new(-64, 384)
    }

    /// The flight ends on the tick the age passes 80, and with survival forced
    /// on it leaves an item and no shatter event.
    #[test]
    fn a_surviving_eye_drops_an_item_after_eighty_ticks() {
        let world = sim_world();
        let mut sim = MobSim::new(&world);
        let id = sim.spawn_eye_of_ender(v(0.0, 64.0, 0.0), v(100.0, 0.0, 0.0));
        sim.eyes.get_mut(&id).expect("eye").survives = true;
        for _ in 0..LIFETIME {
            sim.tick_eyes();
        }
        assert_eq!(sim.eye_count(), 1, "alive through tick 80");
        assert_eq!(sim.item_count(), 0);
        sim.tick_eyes();
        assert_eq!(sim.eye_count(), 0);
        assert_eq!(sim.dropped_items(), vec![("minecraft:ender_eye".to_owned(), 1)]);
        let effects = sim.take_vocalisations();
        assert!(effects.iter().any(|e| matches!(e,
            WorldEffect::Sound { sound, .. } if sound == "minecraft:entity.ender_eye.death")));
        assert!(!effects.iter().any(|e| matches!(e, WorldEffect::LevelEvent { .. })));
    }

    #[test]
    fn a_shattering_eye_emits_the_break_event_and_no_item() {
        let world = sim_world();
        let mut sim = MobSim::new(&world);
        let id = sim.spawn_eye_of_ender(v(0.0, 64.0, 0.0), v(100.0, 0.0, 0.0));
        sim.eyes.get_mut(&id).expect("eye").survives = false;
        for _ in 0..=LIFETIME {
            sim.tick_eyes();
        }
        assert_eq!(sim.item_count(), 0);
        let events: Vec<_> = sim
            .take_vocalisations()
            .into_iter()
            .filter_map(|e| match e {
                WorldEffect::LevelEvent { event, .. } => Some(event),
                _ => None,
            })
            .collect();
        assert_eq!(events, vec![2003]);
    }

    /// Over many launches the drop share lands near four in five.
    #[test]
    fn about_four_in_five_launches_survive() {
        let world = sim_world();
        let mut sim = MobSim::new(&world);
        let mut survived = 0;
        for _ in 0..1000 {
            let id = sim.spawn_eye_of_ender(v(0.0, 64.0, 0.0), v(50.0, 0.0, 0.0));
            if sim.eyes[&id].survives {
                survived += 1;
            }
        }
        assert!((760..=840).contains(&survived), "{survived}");
    }

    #[test]
    fn an_in_flight_eye_streams_as_an_eye_carrying_an_ender_eye_stack() {
        let world = sim_world();
        let mut sim = MobSim::new(&world);
        let id = sim.spawn_eye_of_ender(v(1.0, 64.0, 1.0), v(100.0, 0.0, 1.0));
        let snap = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("eye snapshot");
        assert_eq!(snap.entity_type.to_string(), "minecraft:eye_of_ender");
        assert!(matches!(snap.metadata.as_slice(),
            [crate::protocol::MetadataField::Item { item, count: 1 }] if item.to_string() == "minecraft:ender_eye"));
    }
}
