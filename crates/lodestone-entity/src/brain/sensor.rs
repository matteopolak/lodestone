//! Sensors: the perception step that populates memory before behaviours run.
//!
//! Faithful to vanilla's `Sensor`. Each tick, before any behaviour starts, every
//! sensor writes what the mob currently perceives into memory
//! (`NEAREST_VISIBLE_PLAYER`, nearby entities, and so on). Behaviours then read
//! only memory, never the world directly — so the world coupling lives entirely
//! in sensors and the [`BrainMob`] seam.

use super::memory::{Memories, MemoryModuleType, MemoryValue};
use super::mob::BrainMob;
use lodestone_model::Vec3;

/// The perception units a brain ticks each frame.
///
/// `Send` is required for the same reason [`Goal`](crate::ai::Goal) requires it:
/// a [`Brain`](super::Brain) reaches production inside a
/// [`BrainGoal`](super::BrainGoal), which a `MobSim` stores as a
/// `Box<dyn Goal>` behind an `Arc<Mutex<…>>` and hands to the integrated
/// server's `tokio::spawn`ed connection task. Every sensor here is a plain state
/// machine over owned fields, so the bound costs nothing.
pub trait Sensor: Send {
    /// Updates memory from the mob's current perception.
    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob);

    /// Every memory this sensor writes, so the brain can register it.
    fn output_memories(&self) -> Vec<MemoryModuleType>;

    /// A short name for debugging.
    fn name(&self) -> &'static str;
}

/// Writes [`MemoryModuleType::NEAREST_VISIBLE_PLAYER`] from the mob's nearest
/// visible player, clearing it when there is none.
#[derive(Debug, Default)]
pub struct NearestPlayerSensor;

impl Sensor for NearestPlayerSensor {
    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) {
        match mob.nearest_visible_player() {
            Some(pos) => mem.set(
                MemoryModuleType::NEAREST_VISIBLE_PLAYER,
                MemoryValue::Pos(pos),
            ),
            None => mem.erase(MemoryModuleType::NEAREST_VISIBLE_PLAYER),
        }
    }

    fn output_memories(&self) -> Vec<MemoryModuleType> {
        vec![MemoryModuleType::NEAREST_VISIBLE_PLAYER]
    }

    fn name(&self) -> &'static str {
        "nearest_player"
    }
}

/// Writes [`MemoryModuleType::HURT_BY`] from [`BrainMob::last_hurt_by`],
/// clearing it once that expires. Vanilla's own `HurtBySensor` also records
/// the *damage source* (used by [`super::behaviors::Panic`]'s jar original to
/// filter which damage types cause a flee) — not modelled here, because
/// [`BrainMob::last_hurt_by`] carries only a position, the same cut
/// [`super::mob::BrainMob`]'s own doc discloses for every hurt-adjacent
/// reading on this seam. Every hurt is panic-causing here, broader than
/// vanilla's `DamageTypeTags.PANIC_CAUSES`.
#[derive(Debug, Default)]
pub struct HurtBySensor;

impl Sensor for HurtBySensor {
    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) {
        match mob.last_hurt_by() {
            Some(pos) => mem.set(MemoryModuleType::HURT_BY, MemoryValue::Pos(pos)),
            None => mem.erase(MemoryModuleType::HURT_BY),
        }
    }

    fn output_memories(&self) -> Vec<MemoryModuleType> {
        vec![MemoryModuleType::HURT_BY]
    }

    fn name(&self) -> &'static str {
        "hurt_by"
    }
}

/// Writes [`MemoryModuleType::NEAREST_HOSTILE`] from the nearest hostile
/// entity in [`BrainMob::nearby_entities`], within [`RANGE`](Self::RANGE)
/// blocks — vanilla's `NearestHostileSensor`, restricted to the one question
/// its own consumers ask (`VillagerPanicTrigger.hasHostile`): is there a
/// hostile nearby, and which one is closest.
///
/// Unmodelled next to the jar original: no line-of-sight test (this crate's
/// perception seam has no ray-cast, the same cut every other brain sensor
/// here discloses) and no `SENSOR_TAG` per-species exclusion list — every
/// entity [`BrainMob::nearby_entities`] marks `hostile` counts.
#[derive(Debug, Default)]
pub struct NearestHostileSensor;

impl NearestHostileSensor {
    /// `NearestHostileSensor.frequencyFilter` runs the search on a cadence,
    /// not the *entity* radius; the radius itself is vanilla's `8.0` (used
    /// throughout `Sensor`'s living-entity subclasses' default `getSensorTargets`
    /// cut). This crate's sensors have no cadence knob today, so the radius
    /// alone is what this type contributes; the host is expected to have
    /// already coarsely filtered [`BrainMob::nearby_entities`] to something
    /// reasonable (a chunk-local set, say), same as
    /// [`BrainMob::nearest_visible_player`]'s own undocumented range.
    pub const RANGE: f64 = 8.0;
}

impl Sensor for NearestHostileSensor {
    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) {
        let origin = mob.position();
        let range_sqr = Self::RANGE * Self::RANGE;
        let nearest = mob
            .nearby_entities()
            .into_iter()
            .filter(|e| e.hostile)
            .map(|e| (e.id, distance_sqr(origin, e.position)))
            .filter(|&(_, d)| d <= range_sqr)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match nearest {
            Some((id, _)) => mem.set(MemoryModuleType::NEAREST_HOSTILE, MemoryValue::Entity(id)),
            None => mem.erase(MemoryModuleType::NEAREST_HOSTILE),
        }
    }

    fn output_memories(&self) -> Vec<MemoryModuleType> {
        vec![MemoryModuleType::NEAREST_HOSTILE]
    }

    fn name(&self) -> &'static str {
        "nearest_hostile"
    }
}

/// Writes [`MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES`] from every
/// entity in [`BrainMob::nearby_entities`] within [`RANGE`](Self::RANGE)
/// blocks, nearest first — vanilla's `NearestLivingEntitiesSensor`, the feed
/// `PrepareRamNearestTarget` reduces to a single ram candidate via its own
/// `TargetingConditions`.
///
/// **No hostility, species or line-of-sight filter** — every entity
/// [`BrainMob::nearby_entities`] reports counts, unlike
/// [`NearestHostileSensor`], which keeps only the `hostile`-flagged ones.
/// That is deliberate: vanilla's own sensor is unfiltered too (the exclusion
/// — "not another goat", "respect the world border" — lives entirely in the
/// *consumer's* `TargetingConditions`), and this crate's
/// [`NearbyBrainEntity`](super::mob::NearbyBrainEntity) carries no species
/// tag a consumer could filter on even if this sensor tried, so a goat ram
/// candidate found through this feed is not excluded from being another
/// goat — a disclosed gap, not a silent one; see
/// [`super::behaviors::PrepareRam`]'s own doc for the rest of what its
/// consumer does not narrow.
#[derive(Debug, Default)]
pub struct NearestVisibleLivingEntitiesSensor;

impl NearestVisibleLivingEntitiesSensor {
    /// Wide enough to cover a goat's `RAM_MAX_DISTANCE` (7 blocks) with
    /// margin — this crate's sensors have no cadence knob, so (as
    /// [`NearestHostileSensor::RANGE`]'s own doc notes for its figure) the
    /// host is expected to have already coarsely filtered
    /// [`BrainMob::nearby_entities`] to something reasonable.
    pub const RANGE: f64 = 12.0;
}

impl Sensor for NearestVisibleLivingEntitiesSensor {
    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) {
        let origin = mob.position();
        let range_sqr = Self::RANGE * Self::RANGE;
        let mut nearby: Vec<(i32, f64)> = mob
            .nearby_entities()
            .into_iter()
            .map(|e| (e.id, distance_sqr(origin, e.position)))
            .filter(|&(_, d)| d <= range_sqr)
            .collect();
        nearby.sort_by(|a, b| a.1.total_cmp(&b.1));
        let ids: Vec<i32> = nearby.into_iter().map(|(id, _)| id).collect();
        // `Memories::set` already clears the slot for an empty collection
        // (`MemoryValue::is_empty_collection`), matching vanilla's own
        // `isEmptyCollection` coercion — no separate erase branch needed.
        mem.set(
            MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES,
            MemoryValue::Entities(ids),
        );
    }

    fn output_memories(&self) -> Vec<MemoryModuleType> {
        vec![MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES]
    }

    fn name(&self) -> &'static str {
        "nearest_visible_living_entities"
    }
}

/// Squared Euclidean distance — a plain helper since [`lodestone_model::Vec3`]
/// carries no `distance_squared` of its own.
fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod nearest_hostile_tests {
    use super::*;
    use crate::brain::mob::NearbyBrainEntity;

    /// A [`BrainMob`] double that reports a fixed set of nearby entities.
    struct FixedPerception {
        pos: Vec3,
        nearby: Vec<NearbyBrainEntity>,
    }

    impl BrainMob for FixedPerception {
        fn next_i32(&mut self, _bound: i32) -> i32 {
            0
        }
        fn next_f32(&mut self) -> f32 {
            0.0
        }
        fn game_time(&self) -> i64 {
            0
        }
        fn position(&self) -> Vec3 {
            self.pos
        }
        fn move_to(&mut self, _target: Vec3, _speed: f32) -> bool {
            true
        }
        fn navigation_done(&self) -> bool {
            true
        }
        fn stop_navigation(&mut self) {}
        fn look_at(&mut self, _target: Vec3) {}
        fn random_land_pos(&mut self, _max_xz: i32, _max_y: i32) -> Option<Vec3> {
            None
        }
        fn nearby_entities(&self) -> Vec<NearbyBrainEntity> {
            self.nearby.clone()
        }
    }

    /// The headline case: two hostiles and a non-hostile at varied distances,
    /// and the memory ends up holding the *nearest hostile's* id — not the
    /// nearest entity overall (which is the non-hostile) and not the farther
    /// hostile.
    #[test]
    fn writes_the_nearest_hostiles_id_not_the_nearest_entity_overall() {
        let mut mob = FixedPerception {
            pos: Vec3::default(),
            nearby: vec![
                NearbyBrainEntity {
                    id: 1,
                    position: Vec3::new(1.0, 0.0, 0.0),
                    hostile: false,
                },
                NearbyBrainEntity {
                    id: 2,
                    position: Vec3::new(5.0, 0.0, 0.0),
                    hostile: true,
                },
                NearbyBrainEntity {
                    id: 3,
                    position: Vec3::new(3.0, 0.0, 0.0),
                    hostile: true,
                },
            ],
        };
        let mut mem = Memories::new();
        mem.register(MemoryModuleType::NEAREST_HOSTILE);
        let mut sensor = NearestHostileSensor;
        sensor.tick(&mut mem, &mut mob);
        assert_eq!(
            mem.get(MemoryModuleType::NEAREST_HOSTILE),
            Some(&MemoryValue::Entity(3)),
            "must pick entity 3 (nearer hostile), not 2 (farther hostile) or 1 (nearest overall, but not hostile)"
        );
    }

    /// No hostile in range (one hostile just past 8.0, one non-hostile close)
    /// leaves the memory absent — and a previously-set value is cleared, not
    /// left stale.
    #[test]
    fn clears_the_memory_when_nothing_hostile_is_in_range() {
        let mut mob = FixedPerception {
            pos: Vec3::default(),
            nearby: vec![
                NearbyBrainEntity {
                    id: 1,
                    position: Vec3::new(2.0, 0.0, 0.0),
                    hostile: false,
                },
                NearbyBrainEntity {
                    id: 2,
                    position: Vec3::new(9.0, 0.0, 0.0),
                    hostile: true,
                },
            ],
        };
        let mut mem = Memories::new();
        mem.register(MemoryModuleType::NEAREST_HOSTILE);
        mem.set(MemoryModuleType::NEAREST_HOSTILE, MemoryValue::Entity(99));
        let mut sensor = NearestHostileSensor;
        sensor.tick(&mut mem, &mut mob);
        assert!(
            !mem.has_value(MemoryModuleType::NEAREST_HOSTILE),
            "a hostile beyond the 8.0 range must not be picked, and the stale value must be cleared"
        );
    }

    /// A hostile exactly at the range boundary is included (`<=`, not `<`) —
    /// the discriminating control against an off-by-one on the cut.
    #[test]
    fn a_hostile_exactly_at_the_range_boundary_is_included() {
        let mut mob = FixedPerception {
            pos: Vec3::default(),
            nearby: vec![NearbyBrainEntity {
                id: 7,
                position: Vec3::new(NearestHostileSensor::RANGE, 0.0, 0.0),
                hostile: true,
            }],
        };
        let mut mem = Memories::new();
        mem.register(MemoryModuleType::NEAREST_HOSTILE);
        let mut sensor = NearestHostileSensor;
        sensor.tick(&mut mem, &mut mob);
        assert_eq!(mem.get(MemoryModuleType::NEAREST_HOSTILE), Some(&MemoryValue::Entity(7)));
    }
}

#[cfg(test)]
mod nearest_visible_living_entities_tests {
    use super::*;
    use crate::brain::mob::NearbyBrainEntity;

    struct FixedPerception {
        pos: Vec3,
        nearby: Vec<NearbyBrainEntity>,
    }

    impl BrainMob for FixedPerception {
        fn next_i32(&mut self, _bound: i32) -> i32 {
            0
        }
        fn next_f32(&mut self) -> f32 {
            0.0
        }
        fn game_time(&self) -> i64 {
            0
        }
        fn position(&self) -> Vec3 {
            self.pos
        }
        fn move_to(&mut self, _target: Vec3, _speed: f32) -> bool {
            true
        }
        fn navigation_done(&self) -> bool {
            true
        }
        fn stop_navigation(&mut self) {}
        fn look_at(&mut self, _target: Vec3) {}
        fn random_land_pos(&mut self, _max_xz: i32, _max_y: i32) -> Option<Vec3> {
            None
        }
        fn nearby_entities(&self) -> Vec<NearbyBrainEntity> {
            self.nearby.clone()
        }
    }

    /// Three entities within range, one beyond — the in-range three must come
    /// out **nearest first**, and the out-of-range one must be absent, not
    /// merely sorted last.
    #[test]
    fn writes_in_range_entities_nearest_first() {
        let mut mob = FixedPerception {
            pos: Vec3::default(),
            nearby: vec![
                NearbyBrainEntity {
                    id: 1,
                    position: Vec3::new(9.0, 0.0, 0.0),
                    hostile: false,
                },
                NearbyBrainEntity {
                    id: 2,
                    position: Vec3::new(3.0, 0.0, 0.0),
                    hostile: true,
                },
                NearbyBrainEntity {
                    id: 3,
                    position: Vec3::new(1.0, 0.0, 0.0),
                    hostile: false,
                },
                NearbyBrainEntity {
                    id: 4,
                    position: Vec3::new(50.0, 0.0, 0.0),
                    hostile: false,
                },
            ],
        };
        let mut mem = Memories::new();
        mem.register(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES);
        let mut sensor = NearestVisibleLivingEntitiesSensor;
        sensor.tick(&mut mem, &mut mob);
        assert_eq!(
            mem.get(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES),
            Some(&MemoryValue::Entities(vec![3, 2, 1])),
            "must be sorted nearest-first and exclude id 4 (beyond RANGE)"
        );
    }

    /// Nothing nearby leaves the memory absent, clearing a stale value rather
    /// than leaving it — the same control [`NearestHostileSensor`]'s own
    /// clearing test makes.
    #[test]
    fn clears_the_memory_when_nothing_is_in_range() {
        let mut mob = FixedPerception {
            pos: Vec3::default(),
            nearby: Vec::new(),
        };
        let mut mem = Memories::new();
        mem.register(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES);
        mem.set(
            MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES,
            MemoryValue::Entities(vec![99]),
        );
        let mut sensor = NearestVisibleLivingEntitiesSensor;
        sensor.tick(&mut mem, &mut mob);
        assert!(
            !mem.has_value(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES),
            "an empty perception must clear a stale value, not leave it"
        );
    }
}
